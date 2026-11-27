//! DBOS Notation caller over the authenticated shared HTTP transport.
//!
//! Native ingress configuration is explicit and fails closed when missing.
//! Start, signal, bodyless read and actor serialization use the neutral
//! Notation transport; approval uses the native revision-bound handlers.

use async_trait::async_trait;
use uuid::Uuid;

use crate::notation_http::{NotationHttpRuntime, NOTATION_SERVICE};
use crate::runtime::{SignalContext, StateMachineRuntime, WorkflowEvent, WorkflowRuntimeError};
use crate::spec::{MachineKind, StateName, WorkflowSpec};
use crate::trigger::{DBOS_INGRESS_TOKEN_ENV, DBOS_INGRESS_URL_ENV};

/// [`StateMachineRuntime`] over the worker's DBOS ingress.
#[derive(Clone)]
pub struct DbosNotationRuntime {
    inner: NotationHttpRuntime,
}

impl DbosNotationRuntime {
    /// Point the adapter at the worker's DBOS ingress with an explicit
    /// bearer token.
    #[must_use]
    pub fn new(ingress_url: impl Into<String>, token: Option<String>) -> Self {
        Self {
            inner: NotationHttpRuntime::new(ingress_url, NOTATION_SERVICE).with_auth_token(token),
        }
    }

    /// Configure the authenticated ingress client.
    ///
    /// # Errors
    ///
    /// [`RuntimeConfigError::MissingIngress`] when the ingress URL or bearer token is absent.
    pub fn from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, RuntimeConfigError> {
        let required = |name: &'static str| {
            lookup(name)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .ok_or(RuntimeConfigError::MissingIngress(name))
        };
        Ok(Self::new(
            required(DBOS_INGRESS_URL_ENV)?,
            Some(required(DBOS_INGRESS_TOKEN_ENV)?),
        ))
    }

    /// The worker ingress URL this runtime posts to.
    #[must_use]
    pub fn ingress_url(&self) -> String {
        self.inner.ingress_url()
    }
}

/// Why a boot could not build the DBOS notation runtime.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeConfigError {
    /// An ingress setting is absent.
    #[error("{0} is required for the DBOS Notation runtime")]
    MissingIngress(&'static str),
}

#[async_trait]
impl StateMachineRuntime for DbosNotationRuntime {
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

    async fn approve(
        &self,
        notation_id: Uuid,
        review_revision: i64,
        payload: &str,
        actor: Option<SignalContext>,
    ) -> Result<Option<StateName>, WorkflowRuntimeError> {
        let url = self
            .inner
            .handler_url(MachineKind::Workflow, notation_id, "approve");
        self.inner
            .post_json_no_response(
                &url,
                &serde_json::json!({
                    "review_revision": review_revision,
                    "value": payload,
                    "acting_person_id": actor.map(|actor| actor.acting_person_id),
                }),
            )
            .await?;
        Ok(None)
    }

    async fn approval_progress(
        &self,
        notation_id: Uuid,
        review_revision: i64,
    ) -> Result<Option<crate::ApprovalProgress>, WorkflowRuntimeError> {
        let url = self
            .inner
            .handler_url(MachineKind::Workflow, notation_id, "approval_progress");
        self.inner
            .post_json(
                &url,
                &serde_json::json!({ "review_revision": review_revision }),
            )
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
    use super::{DbosNotationRuntime, RuntimeConfigError};
    use crate::trigger::{DBOS_INGRESS_TOKEN_ENV, DBOS_INGRESS_URL_ENV};

    fn lookup<'a>(env: &'a [(&'static str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            env.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn boot_fails_closed_without_ingress_configuration() {
        assert!(matches!(
            DbosNotationRuntime::from_lookup(lookup(&[])),
            Err(RuntimeConfigError::MissingIngress(_))
        ));

        assert!(matches!(
            DbosNotationRuntime::from_lookup(lookup(&[(
                DBOS_INGRESS_URL_ENV,
                "http://worker:9083"
            ),])),
            Err(RuntimeConfigError::MissingIngress(_))
        ));
    }

    #[test]
    fn boot_points_the_shared_client_at_the_dbos_ingress() {
        let runtime = DbosNotationRuntime::from_lookup(lookup(&[
            (DBOS_INGRESS_URL_ENV, "http://worker:9083/"),
            (DBOS_INGRESS_TOKEN_ENV, "lane-token"),
        ]))
        .expect("complete configuration");
        assert_eq!(runtime.ingress_url(), "http://worker:9083");
    }
    #[tokio::test]
    async fn native_approval_uses_authenticated_revision_bound_handlers() {
        use crate::{ApprovalProgress, SignalContext, StateMachineRuntime};
        use wiremock::matchers::{body_partial_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let id = uuid::Uuid::from_u128(7);
        let actor = uuid::Uuid::from_u128(99);
        Mock::given(method("POST"))
            .and(path(format!("/notation/{id}/workflow_approve")))
            .and(header("authorization","Bearer native-token"))
            .and(body_partial_json(serde_json::json!({"review_revision":3,"value":"approved","acting_person_id":actor})))
            .respond_with(ResponseTemplate::new(200)).expect(1).mount(&server).await;
        Mock::given(method("POST"))
            .and(path(format!("/notation/{id}/workflow_approval_progress")))
            .and(header("authorization", "Bearer native-token"))
            .and(body_partial_json(serde_json::json!({"review_revision":3})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!("ready")))
            .expect(1)
            .mount(&server)
            .await;
        let runtime = DbosNotationRuntime::new(server.uri(), Some("native-token".into()));
        assert_eq!(
            runtime
                .approve(
                    id,
                    3,
                    "approved",
                    Some(SignalContext {
                        acting_person_id: actor
                    })
                )
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            runtime.approval_progress(id, 3).await.unwrap(),
            Some(ApprovalProgress::Ready)
        );
    }

    #[tokio::test]
    async fn native_approval_authentication_failure_remains_a_transport_error() {
        use crate::{StateMachineRuntime, WorkflowRuntimeError};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let id = uuid::Uuid::from_u128(7);
        Mock::given(method("POST"))
            .and(path(format!("/notation/{id}/workflow_approve")))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        let runtime = DbosNotationRuntime::new(server.uri(), Some("rejected-token".into()));
        assert!(matches!(
            runtime.approve(id, 3, "approved", None).await,
            Err(WorkflowRuntimeError::Transport(_))
        ));
    }
}
