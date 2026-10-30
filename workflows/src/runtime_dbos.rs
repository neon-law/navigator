//! DBOS-backed caller side for the `notation` state machine.
//!
//! The notation port (workflows-service `dbos_notation`) reproduces the
//! Restate virtual object's wire contract verbatim — same paths, same
//! request bodies, same bodyless reads — so the application's callers do
//! not change shape when the engine flips. This adapter is therefore a
//! thin named wrapper over [`crate::RestateRuntime`]: it points the same
//! HTTP client at the worker's authenticated DBOS ingress and exists so
//! the engine choice is a *type* at the boot site, not a URL string that
//! happens to differ.
//!
//! The header on [`crate::runtime_restate`] pins the wire shape; the
//! bodyless-read contract (`*_current_state` / `*_events` reject any body
//! or `Content-Type`) is inherited unchanged.
//!
//! Boot reads the engine from [`ENABLED_ENV`] — the same strict
//! `true`/`false` switch contract as every other `NAVIGATOR_DBOS_*` flag.
//! With the flag on, [`DbosNotationRuntime::from_lookup`] requires the
//! worker's DBOS ingress URL and bearer token; unlike the Restate
//! adapter's permissive localhost default, a missing ingress fails boot —
//! pointing notation traffic at nothing is never the safe default.

use async_trait::async_trait;
use uuid::Uuid;

use crate::dbos_engine::flag_enabled;
use crate::runtime::{SignalContext, StateMachineRuntime, WorkflowEvent, WorkflowRuntimeError};
use crate::runtime_restate::RestateRuntime;
use crate::spec::{MachineKind, StateName, WorkflowSpec};
use crate::trigger::{DBOS_INGRESS_TOKEN_ENV, DBOS_INGRESS_URL_ENV};

/// The workflow-specific switch that moves notation traffic to the DBOS
/// worker. Follows the strict `true` / `false` flag contract.
pub const ENABLED_ENV: &str = "NAVIGATOR_DBOS_NOTATION_ENABLED";

/// Whether [`ENABLED_ENV`] moves notation traffic to the DBOS worker.
///
/// # Errors
///
/// Refuses any value other than `true` or `false`, so a typo cannot
/// silently leave traffic on Restate.
pub fn enabled(lookup: impl Fn(&str) -> Option<String>) -> Result<bool, crate::dbos_engine::SettingsError> {
    let value = lookup(ENABLED_ENV);
    flag_enabled(ENABLED_ENV, value.as_deref())
}

/// [`StateMachineRuntime`] over the worker's DBOS ingress.
#[derive(Clone)]
pub struct DbosNotationRuntime {
    /// The worker deliberately speaks the Restate wire contract, so every
    /// method delegates to the shared HTTP client.
    inner: RestateRuntime,
}

impl DbosNotationRuntime {
    /// Point the adapter at the worker's DBOS ingress with an explicit
    /// bearer token.
    #[must_use]
    pub fn new(ingress_url: impl Into<String>, token: Option<String>) -> Self {
        Self {
            // The service path segment stays `notation` — the wire path the
            // port reproduces.
            inner: RestateRuntime::new(ingress_url, crate::runtime_restate::DEFAULT_SERVICE)
                .with_auth_token(token),
        }
    }

    /// Build from the environment the boot site already has in hand:
    /// [`ENABLED_ENV`] must be on (call [`enabled`] first), plus the
    /// worker's DBOS ingress URL and bearer token.
    ///
    /// # Errors
    ///
    /// [`RuntimeConfigError::MissingIngress`] when the flag is on but the
    /// worker's ingress URL or token is unset — fail the boot rather than
    /// silently routing notation traffic at nothing.
    pub fn from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, RuntimeConfigError> {
        let required = |name: &'static str| {
            lookup(name)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .ok_or(RuntimeConfigError::MissingIngress(name))
        };
        Ok(Self::new(required(DBOS_INGRESS_URL_ENV)?, Some(required(DBOS_INGRESS_TOKEN_ENV)?)))
    }

    /// The worker ingress URL this runtime posts to.
    #[must_use]
    pub fn ingress_url(&self) -> String {
        self.inner.broker_url()
    }
}

/// Why a boot could not build the DBOS notation runtime.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeConfigError {
    /// The flag is on but the worker's ingress is unconfigured.
    #[error("{0} must be set while NAVIGATOR_DBOS_NOTATION_ENABLED is on")]
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
        self.inner.signal(kind, notation_id, condition, payload).await
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
    use super::{DbosNotationRuntime, RuntimeConfigError, ENABLED_ENV, enabled};
    use crate::trigger::{DBOS_INGRESS_TOKEN_ENV, DBOS_INGRESS_URL_ENV};

    fn lookup<'a>(env: &'a [(&'static str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            env.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn flag_follows_the_strict_switch_contract() {
        assert!(!enabled(lookup(&[])).expect("unset is off"));
        assert!(!enabled(lookup(&[(ENABLED_ENV, "false")])).expect("false is off"));
        assert!(enabled(lookup(&[(ENABLED_ENV, "true")])).expect("true is on"));
        assert!(
            enabled(lookup(&[(ENABLED_ENV, "TRUE")])).is_err(),
            "non-lowercase truthy values fail closed, like every other DBOS flag"
        );
    }

    #[test]
    fn boot_fails_closed_without_ingress_configuration() {
        assert!(matches!(
            DbosNotationRuntime::from_lookup(lookup(&[(ENABLED_ENV, "true")])),
            Err(RuntimeConfigError::MissingIngress(_))
        ));

        assert!(matches!(
            DbosNotationRuntime::from_lookup(lookup(&[
                (ENABLED_ENV, "true"),
                (DBOS_INGRESS_URL_ENV, "http://worker:9083"),
            ])),
            Err(RuntimeConfigError::MissingIngress(_))
        ));
    }

    #[test]
    fn boot_points_the_shared_client_at_the_dbos_ingress() {
        let runtime = DbosNotationRuntime::from_lookup(lookup(&[
            (ENABLED_ENV, "true"),
            (DBOS_INGRESS_URL_ENV, "http://worker:9083/"),
            (DBOS_INGRESS_TOKEN_ENV, "lane-token"),
        ]))
        .expect("complete configuration");
        assert_eq!(runtime.ingress_url(), "http://worker:9083");
    }
}
