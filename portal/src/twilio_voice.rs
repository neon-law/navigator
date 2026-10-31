//! Signed Twilio voice status callbacks for the shared Inbox.
//!
//! Each completed, missed, or failed call to a registered receiving number
//! enters the receiving Brand's Firm transcript as a `voice` conversation,
//! threaded on the caller and deduplicated on the `CallSid` exactly as a
//! text is on its `MessageSid`. Twilio's own timestamps are authoritative:
//! the stored row carries the call's `StartTime` (falling back to the
//! provider's `EndTime` and then the event `Timestamp` when the call never
//! started), never this process's clock.
//!
//! Auth: every request is verified by the [`crate::twilio_resolution`] spine
//! — the callee `To` is looked up in the Firm's PostgreSQL receiving-number
//! registry, `X-Twilio-Signature` is checked in constant time against that
//! number's own KMS-backed credential over the canonical URL rebuilt from
//! the deployment host, and the registered Brand resolves to its owning
//! Firm. There is no second voice auth path.
//!
//! Mid-call events (`queued`, `ringing`, `in-progress`) describe a call that
//! has not happened yet; they are acknowledged without ingesting so the
//! transcript records the call, not its ringing. A terminal event
//! (`completed`, `failed`, `busy`, `no-answer`, `canceled`) ingests once —
//! Twilio's redeliveries and out-of-order retries return the original row.
//! A missed or zero-duration call still lands in the transcript: the record
//! shows the client tried to reach the firm.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::twilio_resolution::Lane;
use crate::twilio_sms::SIGNATURE_HEADER;

/// Where the deployment's Twilio number sends a call's status events.
pub const VOICE_STATUS_PATH: &str = "/webhook/twilio/voice/status";

/// The call's progress, as Twilio reports it. Mid-call states never reach
/// the transcript; [`CallStatus::parse`] answers `None` for them and for
/// anything unrecognized, which the handler acknowledges without ingesting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallStatus {
    Completed,
    NoAnswer,
    Busy,
    Failed,
    Canceled,
}

impl CallStatus {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "completed" => Some(Self::Completed),
            "no-answer" => Some(Self::NoAnswer),
            "busy" => Some(Self::Busy),
            "failed" => Some(Self::Failed),
            "canceled" => Some(Self::Canceled),
            _ => None,
        }
    }

    /// How the transcript names the outcome.
    #[must_use]
    pub const fn outcome(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::NoAnswer => "no answer",
            Self::Busy => "busy",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }
}

/// One call's status event, reduced to the fields the transcript reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundCall {
    pub call_sid: String,
    pub from: String,
    pub to: String,
    pub status: CallStatus,
    /// Twilio's reported duration in seconds, on the events that carry it.
    pub duration_seconds: Option<i64>,
    /// The provider's own time for the call, RFC 3339.
    pub occurred_at: String,
}

impl InboundCall {
    /// Parse one terminal status event. `None` is a malformed event: a
    /// missing identifier or no provider timestamp to place it in time —
    /// the handler answers 400 rather than inventing a clock value.
    fn from_params(params: &[(String, String)], status: CallStatus) -> Option<Self> {
        let field = |name: &str| {
            params
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .filter(|value| !value.is_empty())
        };
        Some(Self {
            call_sid: field("CallSid")?,
            from: field("From")?,
            to: field("To")?,
            status,
            duration_seconds: field("CallDuration").and_then(|value| value.parse().ok()),
            occurred_at: provider_timestamp(params)?,
        })
    }
}

/// The provider's time for the call: the first parseable `StartTime`,
/// `EndTime`, or `Timestamp`, in that order — every one authored by
/// Twilio, never by this process.
fn provider_timestamp(params: &[(String, String)]) -> Option<String> {
    let field = |name: &str| {
        params
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .filter(|value| !value.is_empty())
    };
    for name in ["StartTime", "EndTime", "Timestamp"] {
        if let Some(value) = field(name) {
            if let Ok(at) = chrono::DateTime::parse_from_rfc2822(value) {
                return Some(at.to_rfc3339());
            }
        }
    }
    None
}

/// The transcript line for one call: deterministic in the event's fields,
/// with no clock value, so a redelivery digests identically.
#[must_use]
pub fn call_line(call: &InboundCall) -> String {
    let from = &call.from;
    match call.status {
        CallStatus::Completed => match call.duration_seconds {
            Some(seconds) if seconds > 0 => {
                format!("Call from {from} — completed ({seconds}s).")
            }
            _ => format!("Call from {from} — completed."),
        },
        other => format!("Missed call from {from} — {}.", other.outcome()),
    }
}

/// `POST /webhook/twilio/voice/status`
pub async fn status(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let params: Vec<(String, String)> = url::form_urlencoded::parse(&body).into_owned().collect();
    let provided = headers
        .get(SIGNATURE_HEADER)
        .and_then(|value| value.to_str().ok());
    // The callee keys the registry lookup.
    let Some(to) = params
        .iter()
        .find(|(key, _)| key == "To")
        .map(|(_, value)| value.as_str())
        .filter(|value| !value.is_empty())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let resolved =
        match crate::twilio_sms::resolve(&state, Lane::VoiceStatus, to, &params, provided).await {
            Ok(resolved) => resolved,
            Err(response) => return response,
        };
    let field = |name: &str| {
        params
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    // A mid-call event describes a call that has not happened yet: ack it
    // without ingesting. An unrecognized status is treated the same way —
    // never answered as an error that would loop Twilio's retries.
    let Some(call_status) = field("CallStatus").and_then(CallStatus::parse) else {
        return StatusCode::NO_CONTENT.into_response();
    };
    let Some(call) = InboundCall::from_params(&params, call_status) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Ok(saved) = ingest_call(&state.surreal, &resolved, &call).await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if crate::inbox::start_inbound_message_alert(saved.message_id)
        .await
        .is_err()
    {
        tracing::error!("Call notice submission failed; Message retained for reconciliation");
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    tracing::info!(
        call_sid = %call.call_sid,
        status = call_status.outcome(),
        "twilio voice status received"
    );
    StatusCode::OK.into_response()
}

async fn ingest_call(
    db: &store::surreal::SurrealDb,
    resolved: &crate::twilio_resolution::ResolvedTwilioWebhook,
    call: &InboundCall,
) -> Result<store::inbox::IngestedInbound, String> {
    store::inbox::ingest_inbound(
        db,
        &store::inbox::IngestInbound {
            firm_id: resolved.firm_id,
            brand_key: Some(&resolved.registration.brand_key),
            interaction_type: Some("message"),
            provider_post_id: None,
            parent_provider_message_id: None,
            visibility: Some("private"),
            channel: store::inbox::channel::VOICE,
            provider_account: &resolved.registration.number_e164,
            provider_message_id: &call.call_sid,
            provider_thread_ref: None,
            external_address: &call.from,
            external_name: None,
            subject: None,
            body: &call_line(call),
            source_page: Some(&resolved.registration.number_e164),
            source_channel: Some(store::inbox::channel::VOICE),
            matter_consent: false,
            promotional_opt_in: false,
            email_conversation_id: None,
            attachment_asset_id: None,
            occurred_at: &call.occurred_at,
        },
    )
    .await
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header;

    fn params(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn a_completed_call_names_its_length_and_a_missed_call_names_its_outcome() {
        let call = |status: CallStatus, seconds: Option<i64>| InboundCall {
            call_sid: "CA1".into(),
            from: "+15555550100".into(),
            to: "+15550100000".into(),
            status,
            duration_seconds: seconds,
            occurred_at: "2026-10-30T18:00:00+00:00".into(),
        };
        assert_eq!(
            call_line(&call(CallStatus::Completed, Some(42))),
            "Call from +15555550100 — completed (42s)."
        );
        assert_eq!(
            call_line(&call(CallStatus::Completed, Some(0))),
            "Call from +15555550100 — completed.",
            "a zero-duration call is recorded, not embellished"
        );
        assert_eq!(
            call_line(&call(CallStatus::Completed, None)),
            "Call from +15555550100 — completed."
        );
        assert_eq!(
            call_line(&call(CallStatus::NoAnswer, None)),
            "Missed call from +15555550100 — no answer."
        );
        assert_eq!(
            call_line(&call(CallStatus::Busy, None)),
            "Missed call from +15555550100 — busy."
        );
        assert_eq!(
            call_line(&call(CallStatus::Failed, None)),
            "Missed call from +15555550100 — failed."
        );
        assert_eq!(
            call_line(&call(CallStatus::Canceled, None)),
            "Missed call from +15555550100 — canceled."
        );
    }

    #[test]
    fn mid_call_and_unknown_statuses_never_reach_the_transcript() {
        assert_eq!(CallStatus::parse("queued"), None);
        assert_eq!(CallStatus::parse("ringing"), None);
        assert_eq!(CallStatus::parse("in-progress"), None);
        assert_eq!(CallStatus::parse("completed"), Some(CallStatus::Completed));
        assert_eq!(CallStatus::parse("no-answer"), Some(CallStatus::NoAnswer));
        assert_eq!(
            CallStatus::parse("COMPLETED"),
            None,
            "Twilio's values are lowercase"
        );
    }

    #[test]
    fn the_provider_timestamp_is_the_first_parseable_provider_field() {
        let fields = params(&[
            ("StartTime", "Fri, 30 Oct 2026 18:00:00 +0000"),
            ("EndTime", "Fri, 30 Oct 2026 18:00:42 +0000"),
            ("Timestamp", "Fri, 30 Oct 2026 18:00:43 +0000"),
        ]);
        assert_eq!(
            provider_timestamp(&fields).as_deref(),
            Some("2026-10-30T18:00:00+00:00"),
            "the call's start is authoritative"
        );
        let no_start = params(&[
            ("EndTime", "Fri, 30 Oct 2026 18:00:42 +0000"),
            ("Timestamp", "Fri, 30 Oct 2026 18:00:43 +0000"),
        ]);
        assert_eq!(
            provider_timestamp(&no_start).as_deref(),
            Some("2026-10-30T18:00:42+00:00"),
            "a call that never started carries its end"
        );
        let timestamp_only = params(&[("Timestamp", "Fri, 30 Oct 2026 18:00:43 +0000")]);
        assert_eq!(
            provider_timestamp(&timestamp_only).as_deref(),
            Some("2026-10-30T18:00:43+00:00")
        );
        assert_eq!(provider_timestamp(&params(&[])), None);
        assert_eq!(
            provider_timestamp(&params(&[("StartTime", "not a date")])),
            None,
            "an unparseable provider field never invents a clock value"
        );
    }

    #[test]
    fn a_missing_identifier_or_timestamp_is_malformed() {
        let status = CallStatus::Completed;
        let full = params(&[
            ("CallSid", "CA1"),
            ("From", "+15555550100"),
            ("To", "+15550100000"),
            ("CallStatus", "completed"),
            ("StartTime", "Fri, 30 Oct 2026 18:00:00 +0000"),
        ]);
        assert!(InboundCall::from_params(&full, status).is_some());
        let missing_sid = params(&[
            ("From", "+15555550100"),
            ("To", "+15550100000"),
            ("CallStatus", "completed"),
            ("StartTime", "Fri, 30 Oct 2026 18:00:00 +0000"),
        ]);
        assert!(InboundCall::from_params(&missing_sid, status).is_none());
        let missing_from = params(&[
            ("CallSid", "CA1"),
            ("To", "+15550100000"),
            ("CallStatus", "completed"),
            ("StartTime", "Fri, 30 Oct 2026 18:00:00 +0000"),
        ]);
        assert!(InboundCall::from_params(&missing_from, status).is_none());
        let missing_time = params(&[
            ("CallSid", "CA1"),
            ("From", "+15555550100"),
            ("To", "+15550100000"),
            ("CallStatus", "completed"),
        ]);
        assert!(
            InboundCall::from_params(&missing_time, status).is_none(),
            "no provider time, no ingest — never Utc::now()"
        );
    }

    /// The registry lane: a seeded Firm with `number` registered for its
    /// own token, and a state whose webhook lanes resolve through
    /// PostgreSQL with the host the spine should sign.
    async fn registry_state(
        db: &store::surreal::SurrealDb,
        pg: &store::postgres::PostgresDb,
        brand: &str,
        number: &str,
        token: &str,
        host: &str,
    ) -> (crate::AppState, uuid::Uuid) {
        let firm = store::test_support::seed_firm_with_brand(db, brand).await;
        crate::twilio_registry_harness::register_number(db, pg, firm, brand, number, token).await;
        let mut state = crate::test_support::app_state(db.clone()).await;
        state.canonical_host = crate::CanonicalHost::new(Some(host.to_string()));
        state.twilio_registry = Some(std::sync::Arc::new(pg.clone()));
        (state, firm)
    }

    fn signed_body(token: &str, host: &str, fields: &[(String, String)]) -> (HeaderMap, Bytes) {
        let signature = crate::twilio_sms::twilio_signature(
            token,
            &format!("https://{host}{VOICE_STATUS_PATH}"),
            fields,
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            header::HeaderName::from_static("x-twilio-signature"),
            signature.parse().unwrap(),
        );
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields)
            .finish();
        (headers, Bytes::from(body))
    }

    /// A completed call ingests once: the same signed body delivered twice
    /// returns 200 both times, but one voice conversation with one message —
    /// the same `message_id`, deduplicated on the `CallSid` like an SMS on
    /// its `MessageSid` — and one alert key, not a duplicate per delivery.
    #[tokio::test]
    async fn a_signed_call_lands_once_and_replays_to_the_same_row() {
        let Some(pg) = crate::twilio_registry_harness::postgres().await else {
            return;
        };
        let db = store::test_support::mem_surreal().await;
        let number = "+15551234444";
        let (state, firm) = registry_state(
            &db,
            &pg,
            "neon",
            number,
            "registry-token",
            "www.neonlaw.com",
        )
        .await;
        let owner = store::persons::create(
            &db,
            &store::persons::NewPerson::with_role(
                "Synthetic owner",
                format!("voice-{}@example.com", uuid::Uuid::now_v7()),
                store::persons::Role::Owner,
            ),
        )
        .await
        .unwrap()
        .id;
        let fields = params(&[
            ("CallSid", "CAsynthetic-completed"),
            ("From", "+15555550100"),
            ("To", number),
            ("CallStatus", "completed"),
            ("CallDuration", "42"),
            ("StartTime", "Fri, 30 Oct 2026 18:00:00 +0000"),
        ]);
        let (headers, body) = signed_body("registry-token", "www.neonlaw.com", &fields);

        let first = status(State(state.clone()), headers, body).await;
        assert_eq!(first.status(), StatusCode::OK);
        let (headers, body) = signed_body("registry-token", "www.neonlaw.com", &fields);
        let replay = status(State(state.clone()), headers, body).await;
        assert_eq!(replay.status(), StatusCode::OK);

        let rows = store::inbox::list_intake(&db, owner).await.unwrap();
        let voice: Vec<_> = rows
            .iter()
            .filter(|row| row.channel == store::inbox::channel::VOICE)
            .collect();
        assert_eq!(voice.len(), 1, "one call, one conversation");
        assert_eq!(voice[0].firm_id, firm);
        assert_eq!(voice[0].brand_key.as_deref(), Some("neon"));
        assert_eq!(voice[0].external_address, "+15555550100");

        let detail = store::inbox::intake_detail(&db, owner, voice[0].id)
            .await
            .unwrap();
        assert_eq!(
            detail.messages.len(),
            1,
            "a redelivered CallSid never writes a second message"
        );
        assert_eq!(
            detail.messages[0].occurred_at, "2026-10-30T18:00:00+00:00",
            "the provider's StartTime is authoritative, not the clock"
        );
        assert_eq!(
            store::inbox_alerts::awaiting_reconciliation(&db, 15)
                .await
                .unwrap()
                .len(),
            1,
            "repeated deliveries attach to the same alert"
        );
    }

    /// A late mid-call retry after the terminal event changes nothing: the
    /// transcript keeps the completed call, and a new CallSid on a mid-call
    /// event creates no conversation at all.
    #[tokio::test]
    async fn an_out_of_order_mid_call_event_is_absorbed_by_the_call_sid_idempotency() {
        let Some(pg) = crate::twilio_registry_harness::postgres().await else {
            return;
        };
        let db = store::test_support::mem_surreal().await;
        let number = "+15551234444";
        let (state, _) = registry_state(
            &db,
            &pg,
            "neon",
            number,
            "registry-token",
            "www.neonlaw.com",
        )
        .await;
        let owner = store::persons::create(
            &db,
            &store::persons::NewPerson::with_role(
                "Synthetic owner",
                format!("voice-{}@example.com", uuid::Uuid::now_v7()),
                store::persons::Role::Owner,
            ),
        )
        .await
        .unwrap()
        .id;
        let completed = params(&[
            ("CallSid", "CAsynthetic-order"),
            ("From", "+15555550100"),
            ("To", number),
            ("CallStatus", "completed"),
            ("CallDuration", "42"),
            ("StartTime", "Fri, 30 Oct 2026 18:00:00 +0000"),
        ]);
        let (headers, body) = signed_body("registry-token", "www.neonlaw.com", &completed);
        assert_eq!(
            status(State(state.clone()), headers, body).await.status(),
            StatusCode::OK
        );

        // The same call, redelivered out of order as its own ringing event.
        let ringing = params(&[
            ("CallSid", "CAsynthetic-order"),
            ("From", "+15555550100"),
            ("To", number),
            ("CallStatus", "ringing"),
        ]);
        let (headers, body) = signed_body("registry-token", "www.neonlaw.com", &ringing);
        assert_eq!(
            status(State(state.clone()), headers, body).await.status(),
            StatusCode::NO_CONTENT,
            "a mid-call event is acknowledged, not ingested"
        );

        // A different call that is still ringing has no transcript row yet.
        let fresh_ringing = params(&[
            ("CallSid", "CAsynthetic-fresh"),
            ("From", "+15555550101"),
            ("To", number),
            ("CallStatus", "in-progress"),
        ]);
        let (headers, body) = signed_body("registry-token", "www.neonlaw.com", &fresh_ringing);
        assert_eq!(
            status(State(state.clone()), headers, body).await.status(),
            StatusCode::NO_CONTENT
        );

        let rows = store::inbox::list_intake(&db, owner).await.unwrap();
        let voice: Vec<_> = rows
            .iter()
            .filter(|row| row.channel == store::inbox::channel::VOICE)
            .collect();
        assert_eq!(voice.len(), 1, "only the completed call is in the record");
        let detail = store::inbox::intake_detail(&db, owner, voice[0].id)
            .await
            .unwrap();
        assert_eq!(detail.messages.len(), 1);
        assert_eq!(
            detail.messages[0].body, "Call from +15555550100 — completed (42s).",
            "the late ringing retry did not rewrite the completed call"
        );
    }

    /// A call to a number outside the registry is refused before any Brand
    /// is considered, and nothing is ingested.
    #[tokio::test]
    async fn a_call_to_an_unregistered_number_is_refused_without_ingesting() {
        let Some(pg) = crate::twilio_registry_harness::postgres().await else {
            return;
        };
        let db = store::test_support::mem_surreal().await;
        let (state, _) = registry_state(
            &db,
            &pg,
            "neon",
            "+15551234444",
            "registry-token",
            "www.neonlaw.com",
        )
        .await;
        let owner = store::persons::create(
            &db,
            &store::persons::NewPerson::with_role(
                "Synthetic owner",
                format!("voice-{}@example.com", uuid::Uuid::now_v7()),
                store::persons::Role::Owner,
            ),
        )
        .await
        .unwrap()
        .id;
        let fields = params(&[
            ("CallSid", "CAsynthetic-foreign"),
            ("From", "+15555550100"),
            ("To", "+15559999999"),
            ("CallStatus", "completed"),
            ("CallDuration", "42"),
            ("StartTime", "Fri, 30 Oct 2026 18:00:00 +0000"),
        ]);
        let (headers, body) = signed_body("registry-token", "www.neonlaw.com", &fields);
        let response = status(State(state.clone()), headers, body).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            store::inbox::list_intake(&db, owner)
                .await
                .unwrap()
                .is_empty(),
            "an unregistered number ingests nothing"
        );
    }

    /// Without the registry configured the lane is off at 404 — the same
    /// answer an unknown number gets — and an unsigned callback is 401.
    #[tokio::test]
    async fn an_unconfigured_lane_is_off_and_an_unsigned_callback_is_denied() {
        let db = store::test_support::mem_surreal().await;
        let unconfigured = crate::test_support::app_state(db.clone()).await;
        let fields = params(&[
            ("CallSid", "CA1"),
            ("From", "+15555550100"),
            ("To", "+15551234444"),
            ("CallStatus", "completed"),
            ("StartTime", "Fri, 30 Oct 2026 18:00:00 +0000"),
        ]);
        let (headers, body) = signed_body("registry-token", "www.neonlaw.com", &fields);
        assert_eq!(
            status(State(unconfigured.clone()), headers, body)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );

        let Some(pg) = crate::twilio_registry_harness::postgres().await else {
            return;
        };
        let (state, _) = registry_state(
            &db,
            &pg,
            "neon",
            "+15551234444",
            "registry-token",
            "www.neonlaw.com",
        )
        .await;
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(&fields)
            .finish();
        assert_eq!(
            status(State(state), HeaderMap::new(), Bytes::from(body))
                .await
                .status(),
            StatusCode::UNAUTHORIZED,
            "a callback without the signature header never ingests"
        );
    }
}
