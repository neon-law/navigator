//! Restate worker for the `notation` virtual object.
//!
//! Each Notation runs *two* state machines back-to-back: a
//! questionnaire walker followed by the post-intake workflow.
//! This crate hosts them as a single Restate virtual-object
//! service keyed by `notation_id`, so signals to either timeline
//! serialize on one logical journal and store-side projections
//! land in the same atomic-with-respect-to-replay envelope.
//!
//! The lib exports the service trait + impl for integration tests
//! and a `main.rs` binary that boots the HTTP server Restate
//! discovers and dispatches into.
//!
//! See `docs/glossary/` (Workflow Runtime, Restate, Durable
//! execution) for the architectural arc.

pub mod asset_config;
pub mod dbos_email_summary;
pub mod dbos_finance_aggregate;
pub mod dbos_inbound_message_alert;
pub mod dbos_ingress;
pub mod dbos_nightly_project_update;
pub mod dbos_project_slack;
pub mod dbos_reconcile;
pub mod dri_digest;
pub mod email_config;
pub mod email_summary;
pub mod engine;
pub mod finance_aggregate;
pub mod general_nag;
pub mod health;
pub mod inbound_message_alert;
pub mod journal;
pub mod nightly_project_update;
pub mod notation_reset;
pub mod notation_service;
pub mod notify_config;
pub mod project_slack;
pub mod registry;
pub mod request_identity;
pub mod social_publisher;
mod tiktok_business;

pub use email_config::{from_env as email_from_env, EmailConfigError};
pub use notation_service::{
    CurrentStateResponse, NotationService, QuestionnaireSignalBody, SignalResponse, StartBody,
    WorkflowSignalBody,
};
pub use notify_config::bot_from_env as slack_bot_from_env;
pub use notify_config::from_env as notifier_from_env;

/// Test support: record the fields a span is created with, so a test can assert
/// a root span carries `navigator.keep` without a collector or an exporter.
#[cfg(test)]
pub(crate) mod span_capture {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::Layer;

    /// Field name to a debug rendering of its value (a bool renders as `true`).
    pub(crate) type Fields = BTreeMap<String, String>;

    #[derive(Default)]
    struct Recorder(Fields);

    impl Visit for Recorder {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0.insert(field.name().to_owned(), format!("{value:?}"));
        }
    }

    struct Capture(Arc<Mutex<Vec<(String, Fields)>>>);

    impl<S: tracing::Subscriber> Layer<S> for Capture {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            _id: &tracing::Id,
            _ctx: Context<'_, S>,
        ) {
            let mut recorder = Recorder::default();
            attrs.record(&mut recorder);
            self.0
                .lock()
                .expect("capture lock")
                .push((attrs.metadata().name().to_owned(), recorder.0));
        }
    }

    /// Run `build`, returning the fields of the span it created.
    pub(crate) fn fields_of(build: impl FnOnce() -> tracing::Span) -> Fields {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(Capture(Arc::clone(&seen)));
        tracing::subscriber::with_default(subscriber, || drop(build()));
        let mut spans = seen.lock().expect("capture lock");
        assert_eq!(
            spans.len(),
            1,
            "the constructor must create exactly one span"
        );
        spans.remove(0).1
    }
}

#[cfg(test)]
mod machine_kind_token_tests {
    // The `notation_events.machine_kind` field stores string tokens
    // produced by two declarations: `workflows::MachineKind::as_str`
    // (the runtime enum) and `store::notation_events::MACHINE_*` (the
    // constants the Surreal query's filter uses). If they ever drift,
    // the projection silently misses rows because the filter no longer
    // matches what the worker wrote. This crate is the single place
    // that depends on both sides, so the equality check belongs here.
    use store::notation_events::{MACHINE_QUESTIONNAIRE, MACHINE_WORKFLOW};
    use workflows::MachineKind;

    #[test]
    fn workflows_enum_tokens_equal_store_constants() {
        assert_eq!(MachineKind::Questionnaire.as_str(), MACHINE_QUESTIONNAIRE);
        assert_eq!(MachineKind::Workflow.as_str(), MACHINE_WORKFLOW);
    }
}
