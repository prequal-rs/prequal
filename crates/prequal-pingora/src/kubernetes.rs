//! Pingora service discovery from a Kubernetes Service's EndpointSlices (feature `kubernetes`).

use std::{
    collections::{BTreeSet, HashMap},
    net::{IpAddr, SocketAddr},
};

use async_trait::async_trait;
use k8s_openapi::api::discovery::v1::{Endpoint, EndpointSlice};
use kube::{Api, Client, api::ListParams};
use pingora_error::{Error, ErrorType, Result};
use pingora_load_balancing::{Backend, discovery::ServiceDiscovery};
use tokio::sync::OnceCell;

/// Which of the Service's ports to route to.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ServicePort {
    /// The port with this name.
    Name(String),
    /// The port with this number.
    Number(u16),
}

impl std::str::FromStr for ServicePort {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(s.parse().map_or_else(|_| Self::Name(s.to_owned()), Self::Number))
    }
}

/// Lists the EndpointSlices of one Service on every `discover()` (driven by the load balancer's
/// `update_frequency`) and returns its ready endpoints. Needs RBAC `list` on
/// `discovery.k8s.io/endpointslices` in the namespace.
#[derive(Debug)]
pub struct EndpointSliceDiscovery {
    namespace: String,
    selector: String,
    port: ServicePort,
    slices: OnceCell<Api<EndpointSlice>>,
}

impl EndpointSliceDiscovery {
    /// Connects on first `discover()` using the in-cluster service account (or local kubeconfig).
    /// Deferred because a kube client must live on the runtime that polls it, i.e. Pingora's.
    pub fn new(namespace: &str, service: &str, port: ServicePort) -> Self {
        Self {
            namespace: namespace.to_owned(),
            selector: format!("kubernetes.io/service-name={service}"),
            port,
            slices: OnceCell::new(),
        }
    }

    /// Like [`EndpointSliceDiscovery::new`], with an existing client.
    pub fn with_client(client: Client, namespace: &str, service: &str, port: ServicePort) -> Self {
        let discovery = Self::new(namespace, service, port);
        let _ = discovery.slices.set(Api::namespaced(client, namespace));
        discovery
    }

    async fn api(&self) -> kube::Result<&Api<EndpointSlice>> {
        self.slices
            .get_or_try_init(|| async { Ok(Api::namespaced(Client::try_default().await?, &self.namespace)) })
            .await
    }
}

#[async_trait]
impl ServiceDiscovery for EndpointSliceDiscovery {
    async fn discover(&self) -> Result<(BTreeSet<Backend>, HashMap<u64, bool>)> {
        let explain =
            |e: kube::Error| Error::explain(ErrorType::InternalError, format!("EndpointSlice discovery: {e}"));
        let slices = self.api().await.map_err(explain)?.list(&ListParams::default().labels(&self.selector)).await;
        Ok((ready_backends(&slices.map_err(explain)?.items, &self.port), HashMap::new()))
    }
}

/// k8s-openapi's version features disagree on whether `EndpointSlice.endpoints` is optional, and
/// the final binary picks the version, so accept both shapes.
trait EndpointList {
    fn as_endpoints(&self) -> &[Endpoint];
}

impl EndpointList for Vec<Endpoint> {
    fn as_endpoints(&self) -> &[Endpoint] {
        self
    }
}

impl EndpointList for Option<Vec<Endpoint>> {
    fn as_endpoints(&self) -> &[Endpoint] {
        self.as_deref().unwrap_or_default()
    }
}

/// Ready IP endpoints of `slices` on `port`. Per the EndpointSlice API, an unset `ready`
/// condition means ready; FQDN slices are skipped.
pub fn ready_backends(slices: &[EndpointSlice], port: &ServicePort) -> BTreeSet<Backend> {
    let mut backends = BTreeSet::new();
    for slice in slices.iter().filter(|s| s.address_type == "IPv4" || s.address_type == "IPv6") {
        let Some(number) = slice.ports.iter().flatten().find_map(|p| match port {
            ServicePort::Name(name) => (p.name.as_deref() == Some(name)).then_some(p.port?),
            ServicePort::Number(n) => (p.port == Some(i32::from(*n))).then_some(p.port?),
        }) else {
            continue;
        };
        let Ok(number) = u16::try_from(number) else { continue };
        for endpoint in slice.endpoints.as_endpoints() {
            if endpoint.conditions.as_ref().and_then(|c| c.ready) == Some(false) {
                continue;
            }
            for address in &endpoint.addresses {
                let Ok(ip) = address.parse::<IpAddr>() else { continue };
                if let Ok(backend) = Backend::new(&SocketAddr::new(ip, number).to_string()) {
                    backends.insert(backend);
                }
            }
        }
    }
    backends
}

#[cfg(test)]
mod tests {
    use k8s_openapi::api::discovery::v1::{EndpointConditions, EndpointPort};

    use super::*;

    fn endpoint(ip: &str, ready: Option<bool>) -> Endpoint {
        Endpoint {
            addresses: vec![ip.to_owned()],
            conditions: Some(EndpointConditions { ready, ..Default::default() }),
            ..Default::default()
        }
    }

    #[allow(clippy::useless_conversion, reason = "`endpoints` is Vec or Option<Vec> depending on k8s-openapi version")]
    fn slice(address_type: &str, endpoints: Vec<Endpoint>) -> EndpointSlice {
        EndpointSlice {
            address_type: address_type.to_owned(),
            endpoints: endpoints.into(),
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
        let addrs: Vec<String> = ready_backends(&slices, &"http".parse().unwrap())
            .iter()
            .map(|b| b.addr.as_inet().unwrap().to_string())
            .collect();
        assert_eq!(addrs, ["10.0.0.1:8000", "[fd00::3]:8000"]);
        assert_eq!(ready_backends(&slices, &ServicePort::Number(9090)).len(), 2);
        assert!(ready_backends(&slices, &"grpc".parse().unwrap()).is_empty());
    }
}
