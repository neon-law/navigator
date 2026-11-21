//! Exercise the real hook HTTP boundary without provider credentials, process-env mutation or a source engine.
use super::start_inbound_hooks_from_lookup;
use uuid::Uuid;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use workflows::trigger::{
    DBOS_INGRESS_TOKEN_ENV, DBOS_INGRESS_URL_ENV, INBOUND_MESSAGE_ALERT_ENABLED_ENV,
    INBOUND_TRIAGE_DBOS_ENABLED_ENV,
};

fn message() -> Uuid {
    Uuid::from_u128(0x1234)
}

fn configured<'a>(
    native: &'a str,
    retained: &'a str,
) -> impl Fn(&str) -> Option<String> + Copy + 'a {
    move |key| match key {
        DBOS_INGRESS_URL_ENV => Some(native.into()),
        DBOS_INGRESS_TOKEN_ENV => Some("synthetic-native".into()),
        INBOUND_MESSAGE_ALERT_ENABLED_ENV | INBOUND_TRIAGE_DBOS_ENABLED_ENV => Some("false".into()),
        "RESTATE_BROKER_URL" => Some(retained.into()),
        "RESTATE_AUTH_TOKEN" => Some("synthetic-retained".into()),
        _ => None,
    }
}

async fn native_alert(server: &MockServer, status: u16, count: u64) {
    Mock::given(method("POST"))
        .and(path("/internal/dbos/inbound-message-alert"))
        .and(header("authorization", "Bearer synthetic-native"))
        .and(body_json(serde_json::json!({
            "workflow_id": format!("InboundMessageAlert:{}", message()),
            "request": { "message_id": message() }
        })))
        .respond_with(ResponseTemplate::new(status))
        .expect(count)
        .mount(server)
        .await;
}

async fn native_triage(server: &MockServer, status: u16, count: u64) {
    Mock::given(method("POST"))
        .and(path("/internal/dbos/inbound-triage"))
        .and(header("authorization", "Bearer synthetic-native"))
        .and(body_json(serde_json::json!({ "message_id": message() })))
        .respond_with(ResponseTemplate::new(status))
        .expect(count)
        .mount(server)
        .await;
}

#[tokio::test]
async fn native_hooks_preserve_authenticated_wire_and_repeat_the_same_message_identity() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let native = MockServer::start().await;
    let retained = MockServer::start().await;
    native_alert(&native, 202, 2).await;
    native_triage(&native, 202, 2).await;
    let native_url = native.uri();
    let retained_url = retained.uri();
    let retired_reads = AtomicUsize::new(0);
    let lookup = |key: &str| {
        if matches!(
            key,
            INBOUND_MESSAGE_ALERT_ENABLED_ENV
                | INBOUND_TRIAGE_DBOS_ENABLED_ENV
                | "RESTATE_BROKER_URL"
                | "RESTATE_AUTH_TOKEN"
        ) {
            retired_reads.fetch_add(1, Ordering::SeqCst);
        }
        configured(&native_url, &retained_url)(key)
    };
    for _ in 0..2 {
        start_inbound_hooks_from_lookup(message(), false, lookup)
            .await
            .unwrap();
    }
    assert_eq!(retired_reads.load(Ordering::SeqCst), 0);
    assert_eq!(native.received_requests().await.unwrap().len(), 4);
    assert!(retained.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn native_missing_coordinates_never_submit_to_the_configured_legacy_broker() {
    let native = MockServer::start().await;
    let retained = MockServer::start().await;
    let native_url = native.uri();
    let retained_url = retained.uri();
    for missing in [DBOS_INGRESS_URL_ENV, DBOS_INGRESS_TOKEN_ENV] {
        let lookup = |key: &str| {
            if key == missing {
                None
            } else {
                configured(&native_url, &retained_url)(key)
            }
        };
        assert!(start_inbound_hooks_from_lookup(message(), false, lookup)
            .await
            .is_err());
    }
    assert!(native.received_requests().await.unwrap().is_empty());
    assert!(retained.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn native_rejection_keeps_hook_order_and_never_falls_back() {
    let retained = MockServer::start().await;
    let retained_url = retained.uri();
    for (alert_status, triage_status, expected_calls) in [(403, 202, 1), (202, 503, 2)] {
        let native = MockServer::start().await;
        native_alert(&native, alert_status, 1).await;
        native_triage(&native, triage_status, u64::from(expected_calls == 2)).await;
        let native_url = native.uri();
        assert!(start_inbound_hooks_from_lookup(
            message(),
            false,
            configured(&native_url, &retained_url)
        )
        .await
        .is_err());
        assert_eq!(
            native.received_requests().await.unwrap().len(),
            expected_calls
        );
    }
    assert!(retained.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn native_transport_failure_never_falls_back() {
    let retained = MockServer::start().await;
    let retained_url = retained.uri();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let unavailable = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    assert!(start_inbound_hooks_from_lookup(
        message(),
        false,
        configured(&unavailable, &retained_url)
    )
    .await
    .is_err());
    assert!(retained.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn retained_hooks_keep_false_selector_legacy_wire_auth_and_message_keys() {
    let native = MockServer::start().await;
    let retained = MockServer::start().await;
    for service in ["InboundMessageAlert", "InboundTriage"] {
        Mock::given(method("POST"))
            .and(path(format!("/{service}/{}/run/send", message())))
            .and(header("authorization", "Bearer synthetic-retained"))
            .and(body_json(serde_json::json!({ "message_id": message() })))
            .respond_with(ResponseTemplate::new(202).set_body_json(serde_json::json!({
                "invocationId": "synthetic-retained-invocation"
            })))
            .expect(2)
            .mount(&retained)
            .await;
    }
    let native_url = native.uri();
    let retained_url = retained.uri();
    for _ in 0..2 {
        start_inbound_hooks_from_lookup(message(), true, configured(&native_url, &retained_url))
            .await
            .unwrap();
    }
    assert_eq!(retained.received_requests().await.unwrap().len(), 4);
    assert!(native.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn only_explicit_retained_hooks_keep_the_original_unconfigured_no_submission() {
    let lookup = |_: &str| None;
    start_inbound_hooks_from_lookup(message(), true, lookup)
        .await
        .unwrap();
    assert!(start_inbound_hooks_from_lookup(message(), false, lookup)
        .await
        .is_err());
}
