//! Request priority from InferenceObjectives, as llm-d's EPP resolves it: the `x-llm-d-inference-objective` header
//! (alias `x-gateway-inference-objective`) names an objective whose `spec.priority` applies; unknown or unset is 0,
//! and a negative priority marks the request sheddable.

use std::{collections::HashMap, sync::RwLock};

use kube::{
    Api, Client,
    api::{ApiResource, DynamicObject, GroupVersionKind, ListParams},
};
use serde_json::Value;

pub const OBJECTIVE_HEADERS: [&str; 2] = ["x-llm-d-inference-objective", "x-gateway-inference-objective"];
/// Current group first: on a name clash it wins.
const GROUPS: [&str; 2] = ["llm-d.ai", "inference.networking.x-k8s.io"];

#[derive(Default)]
pub struct Objectives {
    priorities: RwLock<HashMap<String, i32>>,
}

impl Objectives {
    pub fn priority(&self, objective: Option<&str>) -> i32 {
        let Some(name) = objective else { return 0 };
        self.priorities.read().unwrap_or_else(|p| p.into_inner()).get(name).copied().unwrap_or(0)
    }

    pub fn replace(&self, priorities: HashMap<String, i32>) {
        *self.priorities.write().unwrap_or_else(|p| p.into_inner()) = priorities;
    }

    /// Re-lists the objectives that reference `pool` in `namespace`, under either API group. A group whose CRD is
    /// absent counts as empty; other errors keep the current set and are returned.
    pub async fn refresh(&self, client: &Client, namespace: &str, pool: &str) -> kube::Result<()> {
        let mut priorities = HashMap::new();
        for group in GROUPS.iter().rev() {
            let gvk = GroupVersionKind::gvk(group, "v1alpha2", "InferenceObjective");
            let resource = ApiResource::from_gvk_with_plural(&gvk, "inferenceobjectives");
            let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), namespace, &resource);
            match api.list(&ListParams::default()).await {
                Ok(list) => priorities.extend(list.items.iter().filter_map(|o| for_pool(o, pool))),
                Err(kube::Error::Api(e)) if e.code == 404 => {}
                Err(e) => return Err(e),
            }
        }
        self.replace(priorities);
        Ok(())
    }
}

/// `(name, priority)` of an objective whose `spec.poolRef.name` is `pool`.
fn for_pool(objective: &DynamicObject, pool: &str) -> Option<(String, i32)> {
    let spec = objective.data.get("spec")?;
    if spec.get("poolRef")?.get("name")?.as_str()? != pool {
        return None;
    }
    let priority = spec.get("priority").and_then(Value::as_i64).unwrap_or(0);
    Some((objective.metadata.name.clone()?, i32::try_from(priority).unwrap_or(0)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn objective(name: &str, spec: Value) -> DynamicObject {
        let mut object: DynamicObject = serde_json::from_value(json!({
            "apiVersion": "llm-d.ai/v1alpha2", "kind": "InferenceObjective", "metadata": {"name": name}
        }))
        .unwrap();
        object.data = json!({ "spec": spec });
        object
    }

    #[test]
    fn resolves_priority_by_objective_name_for_this_pool() {
        let batch = objective("batch", json!({"priority": -1, "poolRef": {"name": "pool"}}));
        let plain = objective("plain", json!({"poolRef": {"name": "pool"}}));
        let other = objective("elsewhere", json!({"priority": 5, "poolRef": {"name": "other-pool"}}));
        let objectives = Objectives::default();
        objectives.replace([batch, plain, other].iter().filter_map(|o| for_pool(o, "pool")).collect());
        assert_eq!(objectives.priority(Some("batch")), -1);
        assert_eq!(objectives.priority(Some("plain")), 0);
        assert_eq!(objectives.priority(Some("elsewhere")), 0, "another pool's objective");
        assert_eq!(objectives.priority(Some("unknown")), 0);
        assert_eq!(objectives.priority(None), 0);
    }
}
