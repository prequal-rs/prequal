//! Shared caps on ext_proc work: concurrent streams and request bytes buffered awaiting a pick.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// A capacity shared across shards; [`Lease`]s take units from it and give them back on drop.
#[derive(Debug)]
pub struct Quota {
    used: AtomicUsize,
    max: usize,
}

impl Quota {
    /// `max` units; 0 means unlimited.
    pub fn new(max: usize) -> Arc<Self> {
        Arc::new(Self { used: AtomicUsize::new(0), max: if max == 0 { usize::MAX } else { max } })
    }

    pub fn used(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }

    pub fn lease(self: &Arc<Self>) -> Lease {
        Lease { quota: Arc::clone(self), held: 0 }
    }
}

/// Units taken from a [`Quota`], returned when released or dropped.
#[derive(Debug)]
pub struct Lease {
    quota: Arc<Quota>,
    held: usize,
}

impl Lease {
    /// Takes `n` more units, or none if that would exceed the quota.
    pub fn grow(&mut self, n: usize) -> bool {
        let quota = &self.quota;
        let mut used = quota.used.load(Ordering::Relaxed);
        loop {
            let Some(total) = used.checked_add(n).filter(|&total| total <= quota.max) else {
                return false;
            };
            match quota.used.compare_exchange_weak(used, total, Ordering::AcqRel, Ordering::Relaxed) {
                Ok(_) => break,
                Err(seen) => used = seen,
            }
        }
        self.held += n;
        true
    }

    pub fn release(&mut self) {
        self.quota.used.fetch_sub(std::mem::take(&mut self.held), Ordering::AcqRel);
    }
}

impl Default for Lease {
    fn default() -> Self {
        Quota::new(0).lease()
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leases_share_the_quota_and_return_it() {
        let quota = Quota::new(10);
        let (mut a, mut b) = (quota.lease(), quota.lease());
        assert!(a.grow(6));
        assert!(!b.grow(5), "would exceed");
        assert!(b.grow(4));
        assert_eq!(quota.used(), 10);
        drop(a);
        assert_eq!(quota.used(), 4);
        b.release();
        assert_eq!(quota.used(), 0);
        assert!(Quota::new(0).lease().grow(usize::MAX), "0 is unlimited");
    }
}
