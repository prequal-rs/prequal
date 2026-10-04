//! Envoy metadata and header helpers for the GIE endpoint-picker protocol
//! (gateway-api-inference-extension `docs/proposals/004-endpoint-picker-protocol`).

use std::{collections::HashMap, net::SocketAddr};

use envoy_types::pb::{
    envoy::config::core::v3::{HeaderMap, Metadata},
    google::protobuf::{Struct, Value, value::Kind},
};

pub const LB_NAMESPACE: &str = "envoy.lb";
pub const DESTINATION: &str = "x-gateway-destination-endpoint";
pub const SERVED: &str = "x-gateway-destination-endpoint-served";
pub const SUBSET_NAMESPACE: &str = "envoy.lb.subset_hint";
pub const SUBSET: &str = "x-gateway-destination-endpoint-subset";
/// Conformance-test hooks mirroring the reference lwepp, active only with `--conformance-test-hooks`.
pub const TEST_SELECTION_HEADER: &str = "test-epp-endpoint-selection";
pub const TEST_SERVED_HEADER: &str = "x-conformance-test-served-endpoint";
/// lwepp's value when the gateway did not report the served endpoint, so the conformance check fails.
pub const SERVED_MISSING: &str = "fail: missing destination endpoint served metadata";

fn string_value(s: String) -> Value {
    Value { kind: Some(Kind::StringValue(s)) }
}

/// `{"envoy.lb": {"x-gateway-destination-endpoint": "<ip:port>,<fallback>,..."}}`.
pub fn destination_metadata(value: &str) -> Struct {
    let inner = Struct { fields: HashMap::from([(DESTINATION.to_owned(), string_value(value.to_owned()))]) };
    Struct { fields: HashMap::from([(LB_NAMESPACE.to_owned(), Value { kind: Some(Kind::StructValue(inner)) })]) }
}

fn metadata_field<'a>(metadata: Option<&'a Metadata>, namespace: &str, key: &str) -> Option<&'a Kind> {
    metadata?.filter_metadata.get(namespace)?.fields.get(key)?.kind.as_ref()
}

/// Endpoint subset the gateway allows (string `"a,b"` or list form). `None` means no restriction.
pub fn subset_hint(metadata: Option<&Metadata>) -> Option<Vec<String>> {
    Some(match metadata_field(metadata, SUBSET_NAMESPACE, SUBSET)? {
        Kind::StringValue(s) => split_list(s),
        Kind::ListValue(list) => list
            .values
            .iter()
            .filter_map(|v| match &v.kind {
                Some(Kind::StringValue(s)) => Some(s.trim().to_owned()),
                _ => None,
            })
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    })
}

/// The gateway's report of the endpoint that actually served the request (it may be a fallback), verbatim.
pub fn served_value(metadata: Option<&Metadata>) -> Option<&str> {
    match metadata_field(metadata, LB_NAMESPACE, SERVED)? {
        Kind::StringValue(s) => Some(s.trim()),
        _ => None,
    }
}

pub fn served_endpoint(metadata: Option<&Metadata>) -> Option<SocketAddr> {
    served_value(metadata)?.parse().ok()
}

pub fn split_list(s: &str) -> Vec<String> {
    s.split(',').map(str::trim).filter(|e| !e.is_empty()).map(str::to_owned).collect()
}

/// Whether `addr` matches a subset entry: `ip:port`, or a bare `ip` meaning any port on that pod.
pub fn subset_allows(subset: &[String], addr: &SocketAddr) -> bool {
    subset.iter().any(|entry| match entry.parse::<SocketAddr>() {
        Ok(exact) => exact == *addr,
        Err(_) => entry.parse::<std::net::IpAddr>().is_ok_and(|ip| ip == addr.ip()),
    })
}

pub fn header(headers: Option<&HeaderMap>, name: &str) -> Option<String> {
    let h = headers?.headers.iter().find(|h| h.key.eq_ignore_ascii_case(name))?;
    Some(if h.raw_value.is_empty() { h.value.clone() } else { String::from_utf8_lossy(&h.raw_value).into_owned() })
}

/// The first of `names` present with a non-empty value (a header and its aliases).
pub fn header_any(headers: Option<&HeaderMap>, names: &[&str]) -> Option<String> {
    names.iter().filter_map(|name| header(headers, name)).find(|v| !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use envoy_types::pb::google::protobuf::ListValue;

    use super::*;

    fn metadata(namespace: &str, key: &str, kind: Kind) -> Metadata {
        let inner = Struct { fields: HashMap::from([(key.to_owned(), Value { kind: Some(kind) })]) };
        Metadata { filter_metadata: HashMap::from([(namespace.to_owned(), inner)]), ..Default::default() }
    }

    #[test]
    fn parses_subset_hint_in_both_forms() {
        let s = metadata(SUBSET_NAMESPACE, SUBSET, Kind::StringValue("10.0.0.1:8000, 10.0.0.2".into()));
        assert_eq!(subset_hint(Some(&s)).unwrap(), ["10.0.0.1:8000", "10.0.0.2"]);
        let list = ListValue { values: vec![string_value("10.0.0.3:8000".into())] };
        let l = metadata(SUBSET_NAMESPACE, SUBSET, Kind::ListValue(list));
        assert_eq!(subset_hint(Some(&l)).unwrap(), ["10.0.0.3:8000"]);
        assert_eq!(subset_hint(None), None);
    }

    #[test]
    fn subset_matching_and_served_endpoint() {
        let subset = split_list("10.0.0.1:8000,10.0.0.2");
        assert!(subset_allows(&subset, &"10.0.0.1:8000".parse().unwrap()));
        assert!(!subset_allows(&subset, &"10.0.0.1:9000".parse().unwrap()));
        assert!(subset_allows(&subset, &"10.0.0.2:9000".parse().unwrap()));
        let served = metadata(LB_NAMESPACE, SERVED, Kind::StringValue("10.0.0.9:8000".into()));
        assert_eq!(served_endpoint(Some(&served)), Some("10.0.0.9:8000".parse().unwrap()));
        let dest = destination_metadata("a,b");
        assert!(
            matches!(&dest.fields[LB_NAMESPACE].kind, Some(Kind::StructValue(s)) if s.fields.contains_key(DESTINATION))
        );
    }
}
