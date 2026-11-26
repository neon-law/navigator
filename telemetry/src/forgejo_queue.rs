//! Bounded snapshots for the independent Forgejo queue observer.

use std::sync::{Arc, Mutex};

use opentelemetry::metrics::{Meter, ObservableGauge};
use opentelemetry::KeyValue;

/// Age in seconds of each observed waiting run.
pub const WAITING_SECONDS: &str = "navigator.forgejo.queue.waiting_seconds";
/// Presence of a recent failed main gate or CD run.
pub const RECENT_FAILURE: &str = "navigator.forgejo.queue.recent_failure";

/// Current run observations for one bound repository.
#[derive(Debug, Default, PartialEq)]
pub struct Snapshot {
    /// Run number and elapsed wait in seconds.
    pub waiting: Vec<(u64, f64)>,
    /// Run number and canonical workflow file name.
    pub failures: Vec<(u64, &'static str)>,
}

/// Observable gauges retain only the most recent poll's active series.
pub struct QueueMetrics {
    snapshot: Arc<Mutex<Snapshot>>,
    _waiting: ObservableGauge<f64>,
    _failures: ObservableGauge<u64>,
}

impl QueueMetrics {
    /// Register metrics for one repository.
    #[must_use]
    pub fn new(repository: &str) -> Self {
        let meter = opentelemetry::global::meter(super::FORGEJO_METER);
        Self::with_meter(&meter, repository)
    }

    fn with_meter(meter: &Meter, repository: &str) -> Self {
        let snapshot = Arc::new(Mutex::new(Snapshot::default()));
        let waiting = Arc::clone(&snapshot);
        let failures = Arc::clone(&snapshot);
        let waiting_repo = KeyValue::new("forgejo.repository", repository.to_owned());
        let failure_repo = waiting_repo.clone();
        let waiting = meter
            .f64_observable_gauge(WAITING_SECONDS)
            .with_unit("s")
            .with_callback(move |observer| {
                if let Ok(snapshot) = waiting.lock() {
                    for (run, age) in &snapshot.waiting {
                        observer.observe(
                            *age,
                            &[
                                waiting_repo.clone(),
                                KeyValue::new("forgejo.run_number", run.to_string()),
                            ],
                        );
                    }
                }
            })
            .build();
        let failures = meter
            .u64_observable_gauge(RECENT_FAILURE)
            .with_callback(move |observer| {
                if let Ok(snapshot) = failures.lock() {
                    for (run, workflow) in &snapshot.failures {
                        observer.observe(
                            1,
                            &[
                                failure_repo.clone(),
                                KeyValue::new("forgejo.run_number", run.to_string()),
                                KeyValue::new("forgejo.workflow", *workflow),
                            ],
                        );
                    }
                }
            })
            .build();
        Self {
            snapshot,
            _waiting: waiting,
            _failures: failures,
        }
    }

    /// Replace the current sample, dropping runs no longer in the snapshot.
    pub fn replace(&self, snapshot: Snapshot) {
        if let Ok(mut current) = self.snapshot.lock() {
            *current = snapshot;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::metrics::MeterProvider as _;
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, SdkMeterProvider};

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn queue_metrics_replace_the_run_series_instead_of_retaining_history() {
        let exporter = InMemoryMetricExporter::default();
        let provider = SdkMeterProvider::builder()
            .with_periodic_exporter(exporter.clone())
            .build();
        let metrics =
            QueueMetrics::with_meter(&provider.meter("forgejo-queue-test"), "example/test");
        metrics.replace(Snapshot {
            waiting: vec![(124, 430.0)],
            failures: vec![(125, "gate.yml")],
        });
        provider.force_flush().unwrap();
        let samples = exporter.get_finished_metrics().unwrap();
        let first = format!("{:?}", samples.last().unwrap());
        assert!(first.contains(WAITING_SECONDS) && first.contains("124"));
        assert!(first.contains(RECENT_FAILURE) && first.contains("125"));

        metrics.replace(Snapshot {
            waiting: vec![(126, 35.0)],
            failures: Vec::new(),
        });
        provider.force_flush().unwrap();
        let samples = exporter.get_finished_metrics().unwrap();
        let latest = format!("{:?}", samples.last().unwrap());
        assert!(latest.contains(WAITING_SECONDS) && latest.contains("126"));
        assert!(!latest.contains("\"124\""));
        assert!(!latest.contains(RECENT_FAILURE));
        provider.shutdown().unwrap();
    }
}
