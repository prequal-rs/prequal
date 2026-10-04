//! InferencePool (`inference.networking.k8s.io/v1`) and Pod discovery: the pool's selector picks
//! Pods in its namespace, and every ready Pod contributes one `podIP:port` per target port.

use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use k8s_openapi::api::core::v1::Pod;
use kube::{
    Api, Client,
    api::{ApiResource, DynamicObject, GroupVersionKind, ListParams},
};
use serde_json::Value;

use crate::picker::Picker;

pub struct PoolRef {
    pub group: String,
    pub namespace: String,
    pub name: String,
}

/// Label selector (`k=v,...`) and target ports read from an InferencePool's `spec`.
pub fn pool_spec(pool: &Value) -> Option<(String, Vec<u16>)> {
    let spec = pool.get("spec")?;
    let labels = spec.get("selector")?.get("matchLabels")?.as_object()?;
    let selector = labels.iter().filter_map(|(k, v)| Some(format!("{k}={}", v.as_str()?))).collect::<Vec<_>>();
    let ports = spec
        .get("targetPorts")?
        .as_array()?
        .iter()
        .filter_map(|p| u16::try_from(p.get("number")?.as_u64()?).ok())
        .collect::<Vec<_>>();
    (!selector.is_empty() && !ports.is_empty()).then(|| (selector.join(","), ports))
}

/// Ready = not terminating and condition `Ready=True`, as in the reference EPP. Each endpoint carries llm-d's name
/// for it, `<pod>-rank-<target port index>`.
pub fn ready_endpoints(pods: &[Pod], ports: &[u16]) -> BTreeMap<SocketAddr, String> {
    let ready = |pod: &&Pod| {
        pod.metadata.deletion_timestamp.is_none()
            && pod
                .status
                .as_ref()
                .and_then(|s| s.conditions.as_ref())
                .is_some_and(|c| c.iter().any(|c| c.type_ == "Ready" && c.status == "True"))
    };
    pods.iter()
        .filter(ready)
        .filter_map(|pod| {
            let ip = pod.status.as_ref()?.pod_ip.as_deref()?.parse::<IpAddr>().ok()?;
            Some((ip, pod.metadata.name.as_deref().unwrap_or_default()))
        })
        .flat_map(|(ip, name)| {
            ports
                .iter()
                .enumerate()
                .map(move |(rank, &port)| (SocketAddr::new(ip, port), format!("{name}-rank-{rank}")))
        })
        .collect()
}

async fn refresh(client: &Client, pool: &PoolRef, picker: &Picker) -> kube::Result<Option<usize>> {
    let gvk = GroupVersionKind::gvk(&pool.group, "v1", "InferencePool");
    let resource = ApiResource::from_gvk_with_plural(&gvk, "inferencepools");
    let pools: Api<DynamicObject> = Api::namespaced_with(client.clone(), &pool.namespace, &resource);
    let object = pools.get(&pool.name).await?;
    let Some((selector, ports)) = pool_spec(&object.data) else { return Ok(None) };
    let pods: Api<Pod> = Api::namespaced(client.clone(), &pool.namespace);
    let listed = pods.list(&ListParams::default().labels(&selector)).await?;
    let endpoints = ready_endpoints(&listed.items, &ports);
    picker.sync(&endpoints);
    Ok(Some(endpoints.len()))
}

/// Re-reads the pool, its Pods and its InferenceObjectives every `period`, forever. Errors are logged and retried.
pub async fn watch(pool: PoolRef, picker: Arc<Picker>, period: Duration) {
    let client = loop {
        match Client::try_default().await {
            Ok(client) => break client,
            Err(e) => eprintln!("kubernetes client: {e}; retrying"),
        }
        tokio::time::sleep(period).await;
    };
    let mut last = None;
    let mut objectives_error = None;
    loop {
        match refresh(&client, &pool, &picker).await {
            Ok(count) if count != last => {
                eprintln!("pool {}/{}: {count:?} ready endpoints", pool.namespace, pool.name);
                last = count;
            }
            Ok(_) => {}
            Err(e) => eprintln!("pool {}/{}: {e}", pool.namespace, pool.name),
        }
        // Optional (RBAC or CRDs may be missing): logged when the outcome changes, not every period.
        let listed = picker.objectives().refresh(&client, &pool.namespace, &pool.name).await;
        let error = listed.err().map(|e| e.to_string());
        if error != objectives_error {
            match &error {
                Some(e) => eprintln!(
                    "pool {}/{}: InferenceObjectives: {e}; priorities default to 0",
                    pool.namespace, pool.name
                ),
                None => eprintln!("pool {}/{}: InferenceObjectives readable", pool.namespace, pool.name),
            }
            objectives_error = error;
        }
        tokio::time::sleep(period).await;
    }
}

#[cfg(test)]
mod tests {
    use k8s_openapi::api::core::v1::{PodCondition, PodStatus};
    use serde_json::json;

    use super::*;

    fn pod(ip: &str, ready: &str) -> Pod {
        Pod {
            metadata: kube::api::ObjectMeta { name: Some("vllm-0".into()), ..Default::default() },
            status: Some(PodStatus {
                pod_ip: Some(ip.into()),
                conditions: Some(vec![PodCondition {
                    type_: "Ready".into(),
                    status: ready.into(),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn reads_pool_spec_and_ready_endpoints() {
        let pool = json!({"spec": {"selector": {"matchLabels": {"app": "vllm"}}, "targetPorts": [{"number": 8000}, {"number": 8001}]}});
        let (selector, ports) = pool_spec(&pool).unwrap();
        assert_eq!((selector.as_str(), ports.as_slice()), ("app=vllm", [8000, 8001].as_slice()));
        assert_eq!(pool_spec(&json!({"spec": {"targetPorts": []}})), None);

        let pods = [pod("10.0.0.1", "True"), pod("10.0.0.2", "False")];
        let endpoints: Vec<(String, String)> =
            ready_endpoints(&pods, &ports).into_iter().map(|(addr, name)| (addr.to_string(), name)).collect();
        let expected = [("10.0.0.1:8000", "vllm-0-rank-0"), ("10.0.0.1:8001", "vllm-0-rank-1")];
        assert_eq!(endpoints, expected.map(|(a, n)| (a.to_owned(), n.to_owned())));
    }
}
