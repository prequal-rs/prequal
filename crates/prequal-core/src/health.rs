use crate::Config;

#[derive(Clone, Copy, Debug, Default)]
struct Health {
    consecutive_failures: u32,
    ejections: u32,
    ejected_until_us: u64,
}

/// Per-replica outlier ejection: `eject_after_failures` consecutive failures eject a replica for
/// `base_ejection_us`, doubling on repeat ejections up to `max_ejection_us`. The multiplier resets
/// once a replica stays un-ejected for `max_ejection_us`.
#[derive(Clone, Debug, Default)]
pub(crate) struct HealthTable {
    replicas: Vec<Health>,
}

impl HealthTable {
    pub(crate) fn push(&mut self) {
        self.replicas.push(Health::default());
    }

    pub(crate) fn swap_remove(&mut self, replica: usize) {
        self.replicas.swap_remove(replica);
    }

    pub(crate) fn is_ejected(&self, replica: usize, now_us: u64) -> bool {
        now_us < self.replicas[replica].ejected_until_us
    }

    pub(crate) fn ejected_count(&self, now_us: u64) -> usize {
        self.replicas.iter().filter(|h| now_us < h.ejected_until_us).count()
    }

    pub(crate) fn record_success(&mut self, replica: usize) {
        self.replicas[replica].consecutive_failures = 0;
    }

    /// Returns true if this failure ejected the replica. Ejection is refused when it would push
    /// the ejected share above `max_ejected_fraction`, so a fleet-wide outage can't empty the pool.
    pub(crate) fn record_failure(&mut self, replica: usize, now_us: u64, config: &Config) -> bool {
        if config.eject_after_failures == 0 {
            return false;
        }
        let allowed = (self.replicas.len() as f64 * config.max_ejected_fraction).floor() as usize;
        let room = self.ejected_count(now_us) < allowed;
        let h = &mut self.replicas[replica];
        h.consecutive_failures += 1;
        if h.consecutive_failures < config.eject_after_failures || now_us < h.ejected_until_us || !room {
            return false;
        }
        if now_us.saturating_sub(h.ejected_until_us) > config.max_ejection_us {
            h.ejections = 0;
        }
        let duration = config.base_ejection_us.saturating_mul(1 << h.ejections.min(20));
        h.ejected_until_us = now_us + duration.min(config.max_ejection_us);
        h.ejections += 1;
        h.consecutive_failures = 0;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(n: usize) -> HealthTable {
        let mut t = HealthTable::default();
        (0..n).for_each(|_| t.push());
        t
    }

    fn fail(t: &mut HealthTable, replica: usize, now: u64, times: u32) -> bool {
        (0..times).map(|_| t.record_failure(replica, now, &Config::default())).last().unwrap()
    }

    #[test]
    fn ejects_after_consecutive_failures_and_backs_off() {
        let c = Config::default();
        let mut t = table(4);
        assert!(!fail(&mut t, 0, 0, c.eject_after_failures - 1));
        t.record_success(0);
        assert!(!fail(&mut t, 0, 0, c.eject_after_failures - 1));
        assert!(t.record_failure(0, 0, &c));
        assert!(t.is_ejected(0, c.base_ejection_us - 1));
        assert!(!t.is_ejected(0, c.base_ejection_us));
        let back = c.base_ejection_us;
        assert!(fail(&mut t, 0, back, c.eject_after_failures));
        assert!(t.is_ejected(0, back + 2 * c.base_ejection_us - 1));
    }

    #[test]
    fn caps_ejected_fraction() {
        let c = Config::default();
        let mut t = table(4);
        assert!(fail(&mut t, 0, 0, c.eject_after_failures));
        assert!(fail(&mut t, 1, 0, c.eject_after_failures));
        assert!(!fail(&mut t, 2, 0, c.eject_after_failures));
        assert_eq!(t.ejected_count(0), 2);
    }
}
