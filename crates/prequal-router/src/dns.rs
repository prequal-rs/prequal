//! Replicas given as `host:port`, re-resolved through DNS.

use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
};

/// A replica argument: `host:port`, where host is an IP or a name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Backend(String);

impl std::str::FromStr for Backend {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.rsplit_once(':') {
            Some((host, port)) if !host.is_empty() && port.parse::<u16>().is_ok() => Ok(Self(s.to_owned())),
            _ => Err(format!("{s:?} is not host:port")),
        }
    }
}

/// Every backend as a socket address, or `None` if any is a name that needs resolving.
pub fn literal_addrs(backends: &[Backend]) -> Option<Vec<SocketAddr>> {
    backends.iter().map(|b| b.0.parse().ok()).collect()
}

/// Resolves each backend to all its addresses, so a name with several records (a headless Service, scaled compose
/// services) yields one replica per address.
pub struct DnsDiscovery {
    last: BTreeMap<Backend, BTreeSet<SocketAddr>>,
}

impl DnsDiscovery {
    pub fn new(backends: Vec<Backend>) -> Self {
        Self { last: backends.into_iter().map(|b| (b, BTreeSet::new())).collect() }
    }

    /// A name that fails to resolve keeps its previous addresses, so a DNS blip doesn't drop live replicas.
    pub async fn discover(&mut self) -> BTreeSet<SocketAddr> {
        for (backend, addrs) in &mut self.last {
            match tokio::net::lookup_host(&backend.0).await {
                Ok(resolved) => *addrs = resolved.collect(),
                Err(e) => eprintln!("prequal-router: resolving {}: {e}", backend.0),
            }
        }
        self.last.values().flatten().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backends(args: &[&str]) -> Vec<Backend> {
        args.iter().map(|a| a.parse().unwrap()).collect()
    }

    #[test]
    fn parses_host_port_and_rejects_the_rest() {
        assert!("vllm-0.vllm:8000".parse::<Backend>().is_ok());
        assert!("[fd00::1]:8000".parse::<Backend>().is_ok());
        for bad in ["vllm", "vllm:", ":8000", "vllm:http", "vllm:70000"] {
            assert!(bad.parse::<Backend>().is_err(), "{bad}");
        }
    }

    #[test]
    fn literals_skip_resolution() {
        assert_eq!(literal_addrs(&backends(&["10.0.0.1:8000", "[fd00::1]:80"])).unwrap().len(), 2);
        assert!(literal_addrs(&backends(&["10.0.0.1:8000", "localhost:8000"])).is_none());
    }

    #[tokio::test]
    async fn resolves_names_and_keeps_the_last_addresses_on_failure() {
        let mut dns = DnsDiscovery::new(backends(&["localhost:8000", "10.0.0.1:9000"]));
        let addrs = dns.discover().await;
        assert!(addrs.contains(&"127.0.0.1:8000".parse().unwrap()), "{addrs:?}");
        assert!(addrs.contains(&"10.0.0.1:9000".parse().unwrap()));

        let failing: Backend = "name.invalid:8000".parse().unwrap();
        dns.last.insert(failing.clone(), BTreeSet::from(["10.0.0.2:8000".parse().unwrap()]));
        assert!(dns.discover().await.contains(&"10.0.0.2:8000".parse().unwrap()));
    }
}
