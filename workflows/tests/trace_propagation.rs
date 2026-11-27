//! Authenticated DBOS starts carry W3C trace context across the HTTP boundary.
//!
//! This is its own test binary so it can install a tracing subscriber wired to
//! a real OpenTelemetry tracer without contending with other tests for the
//! process-global subscriber.

use opentelemetry::trace::TracerProvider as _;
use serde_json::json;
use tracing::Instrument;
use tracing_subscriber::layer::SubscriberExt;
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn dbos_start_injects_w3c_traceparent_when_a_span_is_active() {
    // W3C propagator + a real tracer (no exporter needed — the default
    // AlwaysOn sampler still mints valid, sampled span contexts).
    opentelemetry::global::set_text_map_propagator(
        opentelemetry_sdk::propagation::TraceContextPropagator::new(),
    );
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder().build();
    let tracer = provider.tracer("trace-propagation-test");
    let subscriber =
        tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
    // Thread-local default: `#[tokio::test]` runs on the current thread, so the
    // instrumented future below is polled under this subscriber.
    let _guard = tracing::subscriber::set_default(subscriber);

    let server = MockServer::start().await;
    // The mock only matches when `traceparent` is present — so a missing header
    // yields a 404, the DBOS start returns `Rejected`, and the `unwrap` below
    // fails the test. `expect(1)` is verified on server drop.
    Mock::given(method("POST"))
        .and(path("/internal/dbos/finance-aggregate"))
        .and(header_exists("traceparent"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&server)
        .await;

    async {
        workflows::trigger::start_dbos_finance_aggregate(
            &server.uri(),
            "synthetic-token",
            &json!({}),
        )
        .await
        .expect("trigger POST should be accepted (and carry traceparent)");
    }
    .instrument(tracing::info_span!("test.caller"))
    .await;
}
