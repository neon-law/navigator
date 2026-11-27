//! Capture transports for the explicitly simulated, embedded development harness.
use super::HostMode;
use std::sync::Arc;
use store::application_store::ApplicationStore;
use store::postgres::PostgresDb;
use store::DeploymentEnvironment;
use workflows::{CapturingEmail, CapturingNotifier, CapturingSlackBot};

const CHANNEL: &str = "local-capture";
const EXPLICIT_DELIVERY: &[&str] = &[
    "SLACK_FIRM_ID",
    "SLACK_OPS_CHANNEL_ID",
    "SLACK_API_BASE_URL",
    "SLACK_GENERAL_CHANNEL_ID",
    "SLACK_FINANCE_CHANNEL_ID",
    "SLACK_MESSAGES_CHANNEL_ID",
    "SLACK_STAGING_MESSAGES_CHANNEL_ID",
    "XERO_CLIENT_ID",
    "XERO_CLIENT_SECRET",
    "XERO_TOKEN_URL",
    "XERO_SCOPE",
    "XERO_TENANT_ID",
    "XERO_BASE_URL",
    "XERO_ACCESS_TOKEN",
    "BILLING_CANARY_NOTIFY_EMAIL",
];

pub(super) struct LocalProviders {
    slack: Arc<CapturingSlackBot>,
    notifier: Arc<CapturingNotifier>,
    email: Arc<CapturingEmail>,
    billing: Arc<billing::StubBillingProvider>,
}

impl LocalProviders {
    pub(super) fn from_lookup(
        mode: HostMode,
        environment: DeploymentEnvironment,
        simulated: bool,
        get: &impl Fn(&str) -> Option<String>,
    ) -> anyhow::Result<Option<Self>> {
        if mode != HostMode::Embedded
            || !simulated
            || !store::deployment::harness_relaxations_apply(environment, get)
        {
            return Ok(None);
        }
        anyhow::ensure!(
            !store::config::staging_from(get)
                .map_err(|_| anyhow::anyhow!("invalid local staging identity"))?,
            "local capture providers are unavailable for staging"
        );
        anyhow::ensure!(
            !EXPLICIT_DELIVERY
                .iter()
                .any(|key| get(key).is_some_and(|value| !value.is_empty())),
            "local capture providers conflict with explicit delivery configuration"
        );
        Ok(Some(Self {
            slack: Arc::new(CapturingSlackBot::new()),
            notifier: Arc::new(CapturingNotifier::new()),
            email: Arc::new(CapturingEmail::new()),
            billing: Arc::new(billing::StubBillingProvider::for_simulated_matters()),
        }))
    }

    pub(super) fn general_nag(
        &self,
        application_store: ApplicationStore,
        pg: PostgresDb,
    ) -> workflows::dbos_engine::general_nag::Context {
        workflows::dbos_engine::general_nag::Context::with_store(
            application_store,
            pg,
            self.slack.clone(),
            CHANNEL.into(),
            true,
        )
    }

    pub(super) fn dri_digest(
        &self,
        application_store: ApplicationStore,
        pg: PostgresDb,
    ) -> workflows::dbos_engine::dri_digest::Context {
        workflows::dbos_engine::dri_digest::Context::with_store(
            application_store,
            pg,
            self.notifier.clone(),
            true,
        )
    }

    pub(super) fn notation_reset(
        &self,
        application_store: ApplicationStore,
        environment: &'static str,
    ) -> workflows::dbos_engine::notation_reset::Context {
        workflows::dbos_engine::notation_reset::Context {
            application_store,
            slack: self.slack.clone(),
            channel_id: CHANNEL.into(),
            environment,
        }
    }

    pub(super) fn canary(
        &self,
        get: &impl Fn(&str) -> Option<String>,
    ) -> Result<workflows::dbos_engine::billing_canary::Context, workflows::flags::SettingsError>
    {
        let enabled = workflows::flags::schedule_enabled(
            workflows::flags::BILLING_CANARY_SCHEDULE_ENABLED_ENV,
            get,
        )?;
        let mut context = workflows::dbos_engine::billing_canary::Context::new(
            Arc::new(workflows::billing_canary::BillingProviderAdapter(
                self.billing.clone(),
            )),
            self.email.clone(),
        )
        .with_schedule_enabled(enabled);
        context.recipient = "billing-canary@example.test".into();
        Ok(context)
    }

    pub(super) fn reconcile(
        &self,
        application_store: ApplicationStore,
        pg: PostgresDb,
        get: &impl Fn(&str) -> Option<String>,
    ) -> anyhow::Result<crate::dbos_reconcile::Context> {
        let enabled = workflows::flags::schedule_enabled(
            workflows::flags::RECONCILE_INVOICES_SCHEDULE_ENABLED_ENV,
            get,
        )?;
        Ok(crate::dbos_reconcile::Context::with_provider(
            application_store,
            pg,
            self.billing.clone(),
            self.slack.clone(),
            CHANNEL.into(),
            true,
        )
        .with_schedule_enabled(enabled))
    }
}

#[cfg(test)]
mod tests;
