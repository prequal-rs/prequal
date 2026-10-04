//! Replica discovery from a Kubernetes Service's EndpointSlices.

use std::{
    collections::BTreeSet,
    net::{IpAddr, SocketAddr},
};

use k8s_openapi::api::discovery::v1::EndpointSlice;
use kube::{Api, Client, api::ListParams};

/// Which of the Service's ports to route to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServicePort {
    Name(String),
    Number(u16),
}

impl std::str::FromStr for ServicePort {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(s.parse().map_or_else(|_| Self::Name(s.to_owned()), Self::Number))
    }
}

/// Lists one Service's EndpointSlices. Needs RBAC `list` on `discovery.k8s.io/endpointslices` in the namespace.
pub struct EndpointSliceDiscovery {
    slices: Api<EndpointSlice>,
    selector: String,
    port: ServicePort,
}

impl EndpointSliceDiscovery {
    /// Connects with the in-cluster service account (or local kubeconfig).
    pub async fn connect(namespace: &str, service: &str, port: ServicePort) -> kube::Result<Self> {
        Ok(Self {
            slices: Api::namespaced(Client::try_default().await?, namespace),
            selector: format!("kubernetes.io/service-name={service}"),
            port,
        })
    }

    pub async fn discover(&self) -> kube::Result<BTreeSet<SocketAddr>> {
        let slices = self.slices.list(&ListParams::default().labels(&self.selector)).await?;
        Ok(ready_addrs(&slices.items, &self.port))
    }
}

/// Ready IP endpoints of `slices` on `port`. Per the EndpointSlice API, an unset `ready` condition means ready;
/// FQDN slices are skipped.
fn ready_addrs(slices: &[EndpointSlice], port: &ServicePort) -> BTreeSet<SocketAddr> {
    let mut addrs = BTreeSet::new();
    for slice in slices.iter().filter(|s| s.address_type == "IPv4" || s.address_type == "IPv6") {
        let Some(number) = slice.ports.iter().flatten().find_map(|p| match port {
            ServicePort::Name(name) => (p.name.as_deref() == Some(name)).then_some(p.port?),
            ServicePort::Number(n) => (p.port == Some(i32::from(*n))).then_some(p.port?),
        }) else {
            continue;
        };
        let Ok(number) = u16::try_from(number) else { continue };
        for endpoint in &slice.endpoints {
            if endpoint.conditions.as_ref().and_then(|c| c.ready) == Some(false) {
                continue;
            }
            let ips = endpoint.addresses.iter().filter_map(|a| a.parse::<IpAddr>().ok());
            addrs.extend(ips.map(|ip| SocketAddr::new(ip, number)));
        }
    }
    addrs
}

#[cfg(test)]
mod tests {
    use k8s_openapi::api::discovery::v1::{Endpoint, EndpointConditions, EndpointPort};

    use super::*;

    fn endpoint(ip: &str, ready: Option<bool>) -> Endpoint {
        Endpoint {
            addresses: vec![ip.to_owned()],
            conditions: Some(EndpointConditions { ready, ..Default::default() }),
            ..Default::default()
        }
    }

    fn slice(address_type: &str, endpoints: Vec<Endpoint>) -> EndpointSlice {
        EndpointSlice {
            address_type: address_type.to_owned(),
            endpoints,
            ports: Some(vec![
                EndpointPort { name: Some("http".into()), port: Some(8000), ..Default::default() },
                EndpointPort { name: Some("metrics".into()), port: Some(9090), ..Default::default() },
            ]),
            ..Default::default()
        }
    }

    #[test]
    fn keeps_ready_ip_endpoints_on_the_selected_port() {
        let slices = [
            slice("IPv4", vec![endpoint("10.0.0.1", Some(true)), endpoint("10.0.0.2", Some(false))]),
            slice("IPv6", vec![endpoint("fd00::3", None)]),
            slice("FQDN", vec![endpoint("model.example.com", Some(true))]),
        ];
        let addrs: Vec<String> = ready_addrs(&slices, &"http".parse().unwrap()).iter().map(|a| a.to_string()).collect();
        assert_eq!(addrs, ["10.0.0.1:8000", "[fd00::3]:8000"]);
        assert_eq!(ready_addrs(&slices, &ServicePort::Number(9090)).len(), 2);
        assert!(ready_addrs(&slices, &"grpc".parse().unwrap()).is_empty());
    }
}
