//! Placement gossip: routers sharing a fleet tell each other where they sent each prompt, so each one's prefix index
//! covers all their placements instead of only its own (measured in `docs/peer-index.md`). Best effort over UDP: a
//! lost datagram costs one stale belief, and nothing waits on a peer.
//!
//! A datagram is `b"PQ"`, a version byte, the sender's id (u64), the replica (a family byte 4 or 6, the address, the
//! port) and then block hashes (u64 each), integers little-endian. A long prompt spans several datagrams, each
//! applied on its own. Nothing is authenticated: whoever reaches the port can steer routing, so expose it to peers
//! only.

use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Mutex,
    time::Duration,
};

use tokio::net::UdpSocket;

use crate::{
    Scheduler,
    prompt::{BLOCK_TOKENS, Prompt},
};

const MAGIC: [u8; 3] = *b"PQ\x01";
/// Blocks per datagram: with the header it stays under a 1,400-byte overlay-network MTU.
const MAX_BLOCKS: usize = 160;
/// Datagrams per placement, so one huge prompt can't flood the peers (64 cover 2.6 MB of prompt).
const MAX_DATAGRAMS: usize = 64;
const MAX_DATAGRAM_BYTES: usize = MAGIC.len() + 8 + 1 + 16 + 2 + MAX_BLOCKS * 8;

/// One router's end of the gossip: a UDP socket and the peers it announces to.
#[derive(Debug)]
pub struct Gossip {
    socket: UdpSocket,
    /// A plain non-blocking socket: tokio's `try_send_to` drops datagrams until its reactor has seen the socket
    /// writable.
    sender: std::net::UdpSocket,
    /// Random per process, so a router ignores its own announcements when the peer set includes itself.
    id: u64,
    peers: Mutex<Vec<SocketAddr>>,
}

impl Gossip {
    /// Listens for peers' announcements on `addr`.
    ///
    /// # Errors
    /// If the socket cannot be bound.
    pub async fn bind(addr: SocketAddr) -> io::Result<Self> {
        let sender = std::net::UdpSocket::bind(SocketAddr::new(addr.ip(), 0))?;
        sender.set_nonblocking(true)?;
        Ok(Self { socket: UdpSocket::bind(addr).await?, sender, id: rand::random(), peers: Mutex::default() })
    }

    /// The address peers reach this router's gossip at.
    ///
    /// # Errors
    /// If the socket has none.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Replaces the routers announced to. Including this router itself is harmless.
    pub fn set_peers(&self, peers: impl IntoIterator<Item = SocketAddr>) {
        *self.peers.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = peers.into_iter().collect();
    }

    /// Tells every peer that `prompt` went to `replica`. Never blocks: what the socket can't take now is dropped.
    pub fn announce(&self, replica: SocketAddr, prompt: &Prompt) {
        let peers = self.peers.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        if peers.is_empty() {
            return;
        }
        // Tail first: the head arrives last and stays the most recently used, as `BlockLru::touch` keeps it.
        for blocks in prompt.blocks.chunks(MAX_BLOCKS).take(MAX_DATAGRAMS).rev() {
            let datagram = encode(self.id, replica, blocks);
            for peer in &peers {
                let _ = self.sender.send_to(&datagram, *peer);
            }
        }
    }

    /// Applies peers' announcements to `scheduler`, forever.
    pub async fn receive(&self, scheduler: &Scheduler) {
        let mut datagram = [0; MAX_DATAGRAM_BYTES];
        loop {
            // Windows reports a peer's closed port as an error on the next receive.
            let Ok((len, _)) = self.socket.recv_from(&mut datagram).await else {
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            };
            match decode(&datagram[..len]) {
                Some((sender, replica, blocks)) if sender != self.id => {
                    let tokens = blocks.len() as u64 * BLOCK_TOKENS;
                    scheduler.observe_peer_route(replica, &Prompt { blocks, tokens });
                }
                _ => {}
            }
        }
    }

    /// Keeps the peer set at what `name` (`host:port`, e.g. a headless Service of the routers) resolves to, checked
    /// every `interval`. A failed lookup keeps the previous peers.
    pub async fn follow(&self, name: &str, interval: Duration) {
        loop {
            if let Ok(resolved) = tokio::net::lookup_host(name).await {
                self.set_peers(resolved);
            }
            tokio::time::sleep(interval).await;
        }
    }
}

fn encode(sender: u64, replica: SocketAddr, blocks: &[u64]) -> Vec<u8> {
    let mut datagram = Vec::with_capacity(MAX_DATAGRAM_BYTES);
    datagram.extend(MAGIC);
    datagram.extend(sender.to_le_bytes());
    match replica.ip() {
        IpAddr::V4(ip) => datagram.extend([4].into_iter().chain(ip.octets())),
        IpAddr::V6(ip) => datagram.extend([6].into_iter().chain(ip.octets())),
    }
    datagram.extend(replica.port().to_le_bytes());
    datagram.extend(blocks.iter().flat_map(|block| block.to_le_bytes()));
    datagram
}

/// `(sender, replica, blocks)`; `None` for anything that isn't a whole datagram of this version.
fn decode(datagram: &[u8]) -> Option<(u64, SocketAddr, Vec<u64>)> {
    let rest = datagram.strip_prefix(&MAGIC)?;
    let (sender, rest) = rest.split_first_chunk::<8>()?;
    let (family, rest) = rest.split_first()?;
    let (ip, rest) = match family {
        4 => rest.split_first_chunk::<4>().map(|(ip, rest)| (IpAddr::from(Ipv4Addr::from(*ip)), rest))?,
        6 => rest.split_first_chunk::<16>().map(|(ip, rest)| (IpAddr::from(Ipv6Addr::from(*ip)), rest))?,
        _ => return None,
    };
    let (port, rest) = rest.split_first_chunk::<2>()?;
    if rest.len() % 8 != 0 || rest.len() > MAX_BLOCKS * 8 {
        return None;
    }
    let blocks = rest.chunks_exact(8).map(|block| u64::from_le_bytes(block.try_into().expect("8 bytes"))).collect();
    Some((u64::from_le_bytes(*sender), SocketAddr::new(ip, u16::from_le_bytes(*port)), blocks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EngineStats, policy};

    #[test]
    fn datagrams_round_trip_and_malformed_ones_are_dropped() {
        for replica in ["10.0.0.7:8000", "[fd00::7]:8000"] {
            let replica: SocketAddr = replica.parse().unwrap();
            let datagram = encode(9, replica, &[1, u64::MAX, 3]);
            assert_eq!(decode(&datagram), Some((9, replica, vec![1, u64::MAX, 3])));
            assert_eq!(decode(&datagram[..datagram.len() - 1]), None, "a partial block");
            assert_eq!(decode(&datagram[..12]), None, "a truncated header");
        }
        assert_eq!(decode(b"PQ\x02"), None, "another version");
        assert!(encode(0, "[fd00::7]:1".parse().unwrap(), &[0; MAX_BLOCKS]).len() <= MAX_DATAGRAM_BYTES);
    }

    #[tokio::test]
    async fn a_peer_learns_where_a_long_prompt_went_and_ignores_itself() {
        let replicas: Vec<SocketAddr> = (1..=4).map(|host| SocketAddr::from(([10, 0, 0, host], 8000))).collect();
        let prompt = Prompt { blocks: (0..MAX_BLOCKS as u64 * 2 + 5).collect(), tokens: 0 };
        let scheduler = Scheduler::new(policy::by_name("prequal").unwrap());
        scheduler.sync(replicas.iter().copied());
        replicas.iter().for_each(|&addr| scheduler.observe(addr, EngineStats::default()));
        let any = SocketAddr::from(([127, 0, 0, 1], 0));
        let (sender, receiver) = (Gossip::bind(any).await.unwrap(), Gossip::bind(any).await.unwrap());
        sender.set_peers([sender.local_addr().unwrap(), receiver.local_addr().unwrap()]);
        sender.announce(replicas[2], &prompt);

        let heard = async {
            while scheduler.matched_blocks(replicas[2], &prompt) < prompt.blocks.len() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        tokio::select! {
            () = receiver.receive(&scheduler) => unreachable!(),
            heard = tokio::time::timeout(Duration::from_secs(5), heard) => heard.expect("all three datagrams arrive"),
        }
        assert_eq!(scheduler.route(&prompt, 10, |_| true).unwrap().addr(), replicas[2]);
    }
}
