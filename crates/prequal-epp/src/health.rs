//! gRPC health: `liveness` always serves; the readiness names serve while endpoints are known and no drain started.

use std::{sync::Arc, time::Duration};

use tonic_health::{ServingStatus, server::HealthReporter};

use crate::{picker::Picker, shutdown::Draining};

/// Health service names gateways and charts probe for readiness.
pub const READINESS_SERVICES: [&str; 3] =
    ["readiness", "inference-extension", "envoy.service.ext_proc.v3.ExternalProcessor"];

/// Keeps the readiness services current until the drain starts, then reports NOT_SERVING for good.
pub async fn report_readiness(reporter: HealthReporter, picker: Arc<Picker>, draining: Draining) {
    reporter.set_service_status("liveness", ServingStatus::Serving).await;
    let mut last = None;
    loop {
        let status = match picker.is_synced() && !draining.started() {
            true => ServingStatus::Serving,
            false => ServingStatus::NotServing,
        };
        if last != Some(status) {
            for service in READINESS_SERVICES {
                reporter.set_service_status(service, status).await;
            }
            last = Some(status);
        }
        if draining.started() {
            return;
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(500)) => {}
            () = draining.clone().wait() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use prequal_llm::{Scheduler, policy};

    use super::*;
    use crate::shutdown;

    #[tokio::test]
    async fn readiness_follows_sync_then_drain() {
        let reporter = HealthReporter::new();
        let health = tonic_health::server::HealthService::from_health_reporter(reporter.clone());
        let scheduler = Scheduler::new(policy::by_name("prequal").unwrap());
        let picker = Arc::new(Picker::new(scheduler, 0, Duration::from_millis(50)));
        picker.sync(&[("10.0.0.1:8000".parse().unwrap(), "pod-rank-0".to_owned())].into());
        let (drain, draining) = shutdown::channel();
        let task = tokio::spawn(report_readiness(reporter, picker, draining));
        let status = async |service: &str| {
            use tonic_health::pb::{HealthCheckRequest, health_server::Health};
            let request = tonic::Request::new(HealthCheckRequest { service: service.into() });
            health.check(request).await.map(|r| r.into_inner().status).ok()
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            while status("readiness").await != Some(ServingStatus::Serving as i32) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drain.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task).await.unwrap().unwrap();
        for service in READINESS_SERVICES {
            assert_eq!(status(service).await, Some(ServingStatus::NotServing as i32), "{service}");
        }
        assert_eq!(status("liveness").await, Some(ServingStatus::Serving as i32));
    }
}
