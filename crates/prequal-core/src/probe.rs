/// Response header carrying a replica's requests in flight. Names match envoy-prequal so the two implementations can
/// probe each other's servers.
pub const HEADER_RIF: &str = "x-prequal-rif";
/// Response header carrying a replica's latency estimate in microseconds.
pub const HEADER_LATENCY_US: &str = "x-prequal-latency-us";

/// A replica's reported load: requests in flight and its latency estimate at that RIF.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProbeResponse {
    /// Requests in flight.
    pub rif: u32,
    /// Estimated latency at that RIF, in microseconds (0 when the server reports RIF only).
    pub latency_us: u64,
}

impl ProbeResponse {
    /// Parses the values of [`HEADER_RIF`] and [`HEADER_LATENCY_US`]; `None` if either is not a number.
    #[must_use]
    pub fn from_header_values(rif: &str, latency_us: &str) -> Option<Self> {
        Some(Self { rif: rif.trim().parse().ok()?, latency_us: latency_us.trim().parse().ok()? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_headers() {
        let p = ProbeResponse::from_header_values("7", " 1500 ").unwrap();
        assert_eq!(p, ProbeResponse { rif: 7, latency_us: 1500 });
        assert!(ProbeResponse::from_header_values("x", "1").is_none());
    }
}
