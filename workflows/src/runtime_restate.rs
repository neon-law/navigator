//! Compatibility caller for a retained Restate Notation endpoint.
//!
//! Explicit legacy callers retain environment selection and broker defaults.
//! Wire operations use the shared Notation HTTP transport.
use crate::notation_http::NotationHttpRuntime;
use crate::runtime::{SignalContext, StateMachineRuntime, WorkflowEvent, WorkflowRuntimeError};
use crate::spec::{MachineKind, StateName, WorkflowSpec};
use async_trait::async_trait;
use uuid::Uuid;

/// Default retained Notation service path.
pub const DEFAULT_SERVICE: &str = crate::notation_http::NOTATION_SERVICE;
/// Default ingress for explicitly selected legacy local endpoints.
pub const DEFAULT_BROKER_URL: &str = "http://localhost:8080";

#[derive(Clone)]
pub struct RestateRuntime {
    inner: NotationHttpRuntime,
}
impl RestateRuntime {
    /// Point an explicit legacy caller at its broker and service.
    #[must_use]
    pub fn new(broker_url: impl Into<String>, service: impl Into<String>) -> Self {
        Self {
            inner: NotationHttpRuntime::new(broker_url, service),
        }
    }
    /// Set bearer authentication; an empty token is absent.
    #[must_use]
    pub fn with_auth_token(mut self, token: Option<String>) -> Self {
        self.inner = self.inner.with_auth_token(token);
        self
    }
    /// The normalized broker URL used by requests.
    #[must_use]
    pub fn broker_url(&self) -> String {
        self.inner.ingress_url()
    }
    /// Select the retained broker using its existing environment contract.
    #[must_use]
    pub fn from_env() -> Self {
        let url =
            std::env::var("RESTATE_BROKER_URL").unwrap_or_else(|_| DEFAULT_BROKER_URL.to_string());
        let service =
            std::env::var("RESTATE_SERVICE").unwrap_or_else(|_| DEFAULT_SERVICE.to_string());
        let token = std::env::var("RESTATE_AUTH_TOKEN").ok();
        Self::new(url, service).with_auth_token(token)
    }
}

#[async_trait]
impl StateMachineRuntime for RestateRuntime {
    async fn start(
        &self,
        kind: MachineKind,
        notation_id: Uuid,
        spec: &WorkflowSpec,
    ) -> Result<(), WorkflowRuntimeError> {
        self.inner.start(kind, notation_id, spec).await
    }

    async fn start_ephemeral(
        &self,
        kind: MachineKind,
        notation_id: Uuid,
        spec: &WorkflowSpec,
    ) -> Result<(), WorkflowRuntimeError> {
        self.inner.start_ephemeral(kind, notation_id, spec).await
    }

    async fn signal(
        &self,
        kind: MachineKind,
        notation_id: Uuid,
        condition: &str,
        payload: Option<&str>,
    ) -> Result<StateName, WorkflowRuntimeError> {
        self.inner
            .signal(kind, notation_id, condition, payload)
            .await
    }

    async fn signal_with_context(
        &self,
        kind: MachineKind,
        notation_id: Uuid,
        condition: &str,
        payload: Option<&str>,
        context: SignalContext,
    ) -> Result<StateName, WorkflowRuntimeError> {
        self.inner
            .signal_with_context(kind, notation_id, condition, payload, context)
            .await
    }

    async fn signal_ephemeral(
        &self,
        kind: MachineKind,
        notation_id: Uuid,
        condition: &str,
        payload: Option<&str>,
    ) -> Result<StateName, WorkflowRuntimeError> {
        self.inner
            .signal_ephemeral(kind, notation_id, condition, payload)
            .await
    }

    async fn back(
        &self,
        kind: MachineKind,
        notation_id: Uuid,
        context: SignalContext,
    ) -> Result<StateName, WorkflowRuntimeError> {
        self.inner.back(kind, notation_id, context).await
    }

    async fn current_state(&self, kind: MachineKind, notation_id: Uuid) -> Option<StateName> {
        self.inner.current_state(kind, notation_id).await
    }

    async fn events(&self, kind: MachineKind, notation_id: Uuid) -> Vec<WorkflowEvent> {
        self.inner.events(kind, notation_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_BROKER_URL, DEFAULT_SERVICE};
    #[test]
    fn default_constants_match_compose_setup() {
        assert_eq!(DEFAULT_BROKER_URL, "http://localhost:8080");
        assert_eq!(DEFAULT_SERVICE, "notation");
    }
    #[tokio::test]
    async fn legacy_adapter_keeps_explicit_service_and_authentication() {
        use crate::{MachineKind, StateMachineRuntime};
        use wiremock::matchers::{body_bytes, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let id = uuid::Uuid::from_u128(7);
        Mock::given(method("POST"))
            .and(path(format!(
                "/retained-service/{id}/workflow_current_state"
            )))
            .and(header("authorization", "Bearer retained-token"))
            .and(body_bytes(Vec::new()))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"state":"BEGIN"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let runtime = super::RestateRuntime::new(server.uri(), "retained-service")
            .with_auth_token(Some("retained-token".into()));
        assert_eq!(runtime.broker_url(), server.uri());
        assert_eq!(
            runtime
                .current_state(MachineKind::Workflow, id)
                .await
                .unwrap()
                .as_str(),
            "BEGIN"
        );
    }
}
