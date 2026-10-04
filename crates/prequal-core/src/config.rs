/// Client-side Prequal parameters. Defaults follow the paper except `q_rif`, which uses 0.6:
/// the paper's 0.84 herded onto few replicas at high load in our simulation.
///
/// Start from [`Config::default`] and set fields (new fields may be added in minor releases):
///
/// ```
/// let mut config = prequal_core::Config::default();
/// config.probes_per_query = 1.0;
/// ```
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Config {
    /// Probes issued per query; fractional rates are spread deterministically across queries.
    pub probes_per_query: f64,
    /// Load reports kept in the probe pool.
    pub pool_capacity: usize,
    /// Pooled reports older than this are dropped.
    pub max_age_us: u64,
    /// Pool entries evicted per query, alternating oldest and worst. Keep it below the pool's
    /// inflow (probes plus piggybacked reports per query) or the pool never fills; ~1/3 works.
    pub removes_per_query: f64,
    /// The paper's δ in the reuse budget `1 + δ / ((1 - m/n)·r_probe - r_remove)`.
    pub reuse_delta: f64,
    /// Upper bound on how many queries one pooled report may route.
    pub max_reuse: u32,
    /// A probe is hot when its RIF exceeds this quantile of recently observed RIFs.
    pub q_rif: f64,
    /// Recent probe RIFs the hot/cold quantile is taken over.
    pub rif_history: usize,
    /// Consecutive failures that eject a replica; 0 disables ejection.
    pub eject_after_failures: u32,
    /// First ejection's length; repeat ejections double it.
    pub base_ejection_us: u64,
    /// Longest ejection, and how long a replica must stay un-ejected for the doubling to reset.
    pub max_ejection_us: u64,
    /// Upper bound on the share of replicas ejected at once.
    pub max_ejected_fraction: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            probes_per_query: 3.0,
            pool_capacity: 16,
            max_age_us: 1_000_000,
            removes_per_query: 1.0,
            reuse_delta: 1.0,
            max_reuse: 16,
            q_rif: 0.6,
            rif_history: 64,
            eject_after_failures: 5,
            base_ejection_us: 1_000_000,
            max_ejection_us: 30_000_000,
            max_ejected_fraction: 0.5,
        }
    }
}
