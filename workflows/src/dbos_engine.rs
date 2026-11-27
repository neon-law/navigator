//! DBOS durable execution: the executor lifecycle, the configuration every
//! DBOS executor shares, and the effect-key rule that makes a step's side
//! effect safe to repeat.
//!
//! The `web` process hosts the executor when [`ENABLED_ENV`] is `true`
//! (`portal::hosting::run`); `workflows-service` runs the same
//! [`serve`] when its own engine switch selects DBOS. Both go through
//! [`Executor`], so there is one lifecycle wherever the executor runs.
//!
//! A DBOS step commits its checkpoint *after* its body returns, in a separate
//! transaction, so a crash between the two re-runs the body on recovery.
//! [`effect_key`] names the step's effect deterministically so a PostgreSQL
//! write or a provider call can deduplicate that re-run. The design, the
//! Restate mapping, and the port order are in `docs/dbos-durability.md`.

use std::future::Future;
use std::sync::Arc;

/// Bound DBOS system-schema lock waits so graceful shutdown can settle tracked starts.
/// This applies only to DBOS's own PostgreSQL connections.
const SYSTEM_DB_LOCK_TIMEOUT: &str = "5s";

use crate::Notifier;

pub mod billing_canary;
pub mod billing_digest;
pub mod ceo_digest;
pub mod daily;
/// The nightly Deadline-mirror reconcile schedule.
pub mod deadline_mirror;
pub mod dri_digest;
mod forge_parity;
pub mod gcp_cost_daily;
pub mod general_nag;
pub mod heartbeat;
pub mod linear_backlog;
pub mod notation_reset;
pub mod plan_retainers;
pub mod profit_digest;
pub mod project_calendar;
pub mod project_recovery;
pub mod purchase_activation;
pub mod release_maintenance;
pub mod retention_purge;
pub mod source_imports;

/// The variable carrying the deployed release, used as the DBOS application
/// version.
pub const RELEASE_ENV: &str = "NAVIGATOR_RELEASE_TAG";

/// The application name stamped on every DBOS row an executor writes.
pub const APP_NAME: &str = "navigator-workflows";

/// The PostgreSQL schema holding the DBOS system tables. The SDK owns it;
/// application tables never live here.
pub const SYSTEM_SCHEMA: &str = "dbos";

/// A DBOS switch or connection the environment does not complete.
///
/// Defined in [`crate::flags`] so callers without the `dbos` feature parse
/// the same switches; re-exported here for the established path.
pub use crate::flags::SettingsError;

pub use crate::flags::flag_enabled;

/// The DBOS login's connection variables. DBOS signs in with its own login,
/// never the application's, and reads only the `dbos` schema.
pub const HOST_ENV: &str = "NAVIGATOR_DBOS_HOST";
pub const PORT_ENV: &str = "NAVIGATOR_DBOS_PORT";
pub const USER_ENV: &str = "NAVIGATOR_DBOS_USER";
pub const PASSWORD_ENV: &str = "NAVIGATOR_DBOS_PASSWORD";
pub const DATABASE_ENV: &str = "NAVIGATOR_DBOS_DATABASE";
/// `verify-full` (the default), or `disable` for a loopback test server only.
pub const SSLMODE_ENV: &str = "NAVIGATOR_DBOS_SSLMODE";
/// Optional CA bundle for `verify-full`.
pub const ROOT_CERT_ENV: &str = "NAVIGATOR_DBOS_ROOT_CERT";
/// Stable for the lifetime of a pod and unique across concurrently running pods.
pub const EXECUTOR_ID_ENV: &str = "NAVIGATOR_DBOS_EXECUTOR_ID";

/// What a DBOS executor reads from its environment. No `Debug`: the connection
/// carries the login's password.
pub struct Settings {
    connection: String,
    app_version: String,
    executor_id: String,
}

impl Settings {
    /// Read the DBOS login's connection and the application version.
    ///
    /// # Errors
    ///
    /// Fails closed when a required variable is unset or empty, the port is
    /// not a port, TLS is disabled for a non-loopback host, or the release is
    /// `unknown`: a version-less executor could never recover its own runs.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, SettingsError> {
        let optional = |key: &str| {
            lookup(key)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let required = |key: &'static str| optional(key).ok_or(SettingsError::Missing(key));
        let host = required(HOST_ENV)?;
        let port: u16 = required(PORT_ENV)?
            .parse()
            .map_err(|_| SettingsError::Invalid(PORT_ENV))?;
        let sslmode = optional(SSLMODE_ENV).unwrap_or_else(|| "verify-full".to_owned());
        let loopback = matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1");
        if !(sslmode == "verify-full" || (sslmode == "disable" && loopback)) {
            return Err(SettingsError::Invalid(SSLMODE_ENV));
        }
        let mut url = url::Url::parse("postgres://localhost/")
            .map_err(|_| SettingsError::Invalid(HOST_ENV))?;
        url.set_host(Some(&host))
            .map_err(|_| SettingsError::Invalid(HOST_ENV))?;
        url.set_port(Some(port))
            .map_err(|()| SettingsError::Invalid(PORT_ENV))?;
        url.set_username(&required(USER_ENV)?)
            .map_err(|()| SettingsError::Invalid(USER_ENV))?;
        url.set_password(Some(&required(PASSWORD_ENV)?))
            .map_err(|()| SettingsError::Invalid(PASSWORD_ENV))?;
        url.set_path(&required(DATABASE_ENV)?);
        url.query_pairs_mut().append_pair("sslmode", &sslmode);
        if let Some(root_cert) = optional(ROOT_CERT_ENV) {
            url.query_pairs_mut().append_pair("sslrootcert", &root_cert);
        }
        let app_version = required(RELEASE_ENV)?;
        if app_version == "unknown" {
            return Err(SettingsError::Missing(RELEASE_ENV));
        }
        let executor_id = required(EXECUTOR_ID_ENV)?;
        if !valid_executor_id(&executor_id) {
            return Err(SettingsError::Invalid(EXECUTOR_ID_ENV));
        }
        Ok(Self {
            connection: url.into(),
            app_version,
            executor_id,
        })
    }

    /// The DBOS configuration these settings describe.
    #[must_use]
    pub fn config(&self) -> dbos::Config {
        config_with_executor_id(&self.connection, &self.app_version, &self.executor_id)
    }
}

/// The DBOS configuration for an executor: the shared application name, the
/// `dbos` system schema, and an explicit application version (DBOS computes
/// none, and recovers a workflow only on the version that started it). The
/// default `local` identity is for tests and local single-process use; hosted
/// entrypoints use [`Settings::config`] to supply their pod identity.
#[must_use]
pub fn config(database_url: &str, app_version: &str) -> dbos::Config {
    config_with_executor_id(database_url, app_version, "local")
}

/// The DBOS configuration for one executor identity.
///
/// DBOS system-database connections use a five-second PostgreSQL lock timeout;
/// application database connections keep their own settings.
#[must_use]
pub fn config_with_executor_id(
    database_url: &str,
    app_version: &str,
    executor_id: &str,
) -> dbos::Config {
    dbos::Config {
        schema: SYSTEM_SCHEMA.to_owned(),
        app_version: Some(app_version.to_owned()),
        executor_id: Some(executor_id.to_owned()),
        ..dbos::Config::new(APP_NAME, system_database_url(database_url))
    }
}

fn system_database_url(database_url: &str) -> String {
    let Ok(mut url) = url::Url::parse(database_url) else {
        return database_url.to_owned();
    };
    url.query_pairs_mut()
        .append_pair("options[lock_timeout]", SYSTEM_DB_LOCK_TIMEOUT);
    url.to_string()
}

fn valid_executor_id(value: &str) -> bool {
    (1..=256).contains(&value.len())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
}

/// The optional schedules a host enables beside the always-on Heartbeat.
#[derive(Default)]
pub struct Schedules {
    pub forge_parity: bool,
    pub daily_cost_export: Option<billing::gcp_cost_daily::ExportTable>,
    pub linear_backlog: Option<linear_backlog::Context>,
    pub general_nag: Option<general_nag::Context>,
    pub dri_digest: Option<dri_digest::Context>,
    pub project_recovery: Option<project_recovery::Context>,
    pub source_imports: Option<source_imports::Context>,
    pub retention_purge: Option<retention_purge::Context>,
    pub deadline_mirror: Option<deadline_mirror::Context>,
    pub plan_retainers: Option<Arc<dyn plan_retainers::Source>>,
    pub project_calendar: Option<Arc<dyn project_calendar::Source>>,
    pub purchase_activation: Option<purchase_activation::Context>,
    pub billing_canary: Option<billing_canary::Context>,
    pub billing_digest: Option<billing_digest::Context>,
    pub notation_reset: Option<notation_reset::Context>,
    pub profit_digest: Option<profit_digest::Context>,
    pub ceo_digest: Option<ceo_digest::Context>,
}

impl Schedules {
    /// Combine the worker's provider contexts with the application's own schedules.
    /// Explicit fields make a new schedule require an ownership decision.
    #[must_use]
    pub fn with_application_schedules(self, application: Self) -> Self {
        Self {
            daily_cost_export: application.daily_cost_export,
            forge_parity: self.forge_parity,
            general_nag: self.general_nag,
            dri_digest: self.dri_digest,
            billing_canary: self.billing_canary,
            billing_digest: self.billing_digest,
            notation_reset: self.notation_reset,
            profit_digest: self.profit_digest,
            linear_backlog: application.linear_backlog,
            project_recovery: application.project_recovery,
            source_imports: application.source_imports,
            retention_purge: application.retention_purge,
            deadline_mirror: application.deadline_mirror,
            plan_retainers: application.plan_retainers,
            project_calendar: application.project_calendar,
            purchase_activation: application.purchase_activation,
            ceo_digest: application.ceo_digest,
        }
    }
}

/// Register an optional schedule's workflows when its context is present.
fn register_optional<C, W>(
    dbos: &dbos::DBOS,
    context: Option<C>,
    register: fn(&dbos::DBOS, C) -> dbos::Result<W>,
) -> dbos::Result<Option<W>> {
    context.map(|context| register(dbos, context)).transpose()
}

/// Register the optional parity workflow before launch and install its schedule
/// only after the host can accept work.
async fn launch_with_parity(dbos: &dbos::DBOS, enabled: bool) -> dbos::Result<()> {
    let schedule = enabled.then(|| forge_parity::register(dbos)).transpose()?;
    dbos.launch().await?;
    if let Some(schedule) = schedule {
        if let Err(error) = dbos.apply_schedules(&[schedule]).await {
            dbos.shutdown().await;
            return Err(error);
        }
    }
    Ok(())
}

/// A schedule loop registered by a crate outside `workflows`.
///
/// Service crates that depend on `workflows` can register their DBOS
/// workflows through the shared executor without introducing a dependency
/// cycle back into this crate. Each returned starter is invoked only after
/// DBOS has launched successfully.
pub type ScheduleStarter =
    Box<dyn FnOnce(dbos::CancellationToken) -> tokio::task::JoinHandle<()> + Send>;

/// The schedules ported from Restate CronJobs that a host enabled: registered
/// before launch, their schedule loops started after it.
struct RestatePorts {
    billing_canary: Option<billing_canary::BillingCanaryWorkflow>,
    schedule_billing_canary: bool,
    notation_reset: Option<notation_reset::NotationResetWorkflow>,
}

impl RestatePorts {
    fn register(
        dbos: &dbos::DBOS,
        billing_canary: Option<billing_canary::Context>,
        notation_reset: Option<notation_reset::Context>,
    ) -> dbos::Result<Self> {
        let schedule_billing_canary = billing_canary
            .as_ref()
            .is_some_and(|context| context.schedule_enabled);
        Ok(Self {
            schedule_billing_canary,
            billing_canary: billing_canary
                .map(|context| billing_canary::register(dbos, context))
                .transpose()?,
            notation_reset: notation_reset
                .map(|context| notation_reset::register(dbos, context))
                .transpose()?,
        })
    }

    fn manual_starts(
        &self,
        billing_digest: Option<billing_digest::BillingDigestWorkflow>,
    ) -> ManualSchedules {
        ManualSchedules {
            billing_canary: self.billing_canary.clone(),
            billing_digest,
            notation_reset: self.notation_reset.clone(),
        }
    }

    fn spawn(self, stop: &dbos::CancellationToken, loops: &mut Vec<tokio::task::JoinHandle<()>>) {
        if let Some(workflow) = self.billing_canary.filter(|_| self.schedule_billing_canary) {
            loops.push(tokio::spawn(billing_canary::schedule(
                workflow,
                billing_canary::system_clock(),
                stop.clone(),
            )));
        }
        if let Some(workflow) = self.notation_reset {
            loops.push(tokio::spawn(notation_reset::schedule(
                workflow,
                notation_reset::system_clock(),
                stop.clone(),
            )));
        }
    }
}

/// A launched DBOS executor and its schedule loops.
///
/// Every ported workflow is registered before launch, launch migrates the
/// `dbos` schema and re-enqueues this version's pending runs, and each
/// schedule loop starts after launch. [`Executor::shutdown`] cancels scheduling,
/// shuts DBOS down, then joins the loops; work cut off there stays pending for the next launch
/// of the same version.
pub struct Executor {
    dbos: dbos::DBOS,
    stop: dbos::CancellationToken,
    schedules: Vec<tokio::task::JoinHandle<()>>,
    manual: ManualSchedules,
}
/// Typed handles for manual starts of the same workflows the schedules run.
#[derive(Clone, Default)]
pub struct ManualSchedules {
    billing_canary: Option<billing_canary::BillingCanaryWorkflow>,
    billing_digest: Option<billing_digest::BillingDigestWorkflow>,
    notation_reset: Option<notation_reset::NotationResetWorkflow>,
}

/// The selected workflow could not admit a manual start.
#[derive(Debug, thiserror::Error)]
#[error("the scheduled workflow could not be started")]
pub struct ManualStartError;

impl ManualSchedules {
    /// Start a registered schedule with an explicit identity. Unknown or disabled
    /// workflows return false; the caller never falls back to another engine.
    pub async fn start(&self, service: &str, workflow_id: &str) -> Result<bool, ManualStartError> {
        let options = dbos::StartOptions {
            workflow_id: Some(workflow_id),
            ..Default::default()
        };
        match service {
            "BillingCanary" => {
                if let Some(workflow) = &self.billing_canary {
                    workflow
                        .start_with((), options)
                        .await
                        .map_err(|_| ManualStartError)?;
                } else {
                    return Ok(false);
                }
            }
            "BillingDigest" => {
                if let Some(workflow) = &self.billing_digest {
                    workflow
                        .start_with((), options)
                        .await
                        .map_err(|_| ManualStartError)?;
                } else {
                    return Ok(false);
                }
            }
            "NotationReset" => {
                if let Some(workflow) = &self.notation_reset {
                    workflow
                        .start_with((), options)
                        .await
                        .map_err(|_| ManualStartError)?;
                } else {
                    return Ok(false);
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}

/// A readiness-probe handle for a launched executor: shares the stop token and
/// the system database URL without owning the executor, so the health route can
/// report DBOS state while `main` keeps exclusive shutdown rights.
#[derive(Clone)]
pub struct ExecutorProbe {
    stop: dbos::CancellationToken,
    system_database_url: Arc<String>,
}

impl ExecutorProbe {
    /// True while the executor is launched and no shutdown has been requested.
    #[must_use]
    pub fn is_running(&self) -> bool {
        !self.stop.is_cancelled()
    }

    /// The system database URL this executor launched against.
    ///
    /// `None` when the executor runs against no configured system database,
    /// which readiness reports as unhealthy rather than skipping the check.
    #[must_use]
    pub fn system_database_url(&self) -> Option<&str> {
        let url = self.system_database_url.as_str();
        (!url.is_empty()).then_some(url)
    }
}

impl Executor {
    /// Snapshot a readiness-probe handle for this executor.
    #[must_use]
    pub fn probe(&self) -> ExecutorProbe {
        ExecutorProbe {
            stop: self.stop.clone(),
            system_database_url: Arc::new(self.dbos.config().database_url.clone()),
        }
    }
}

impl Executor {
    /// Register every ported workflow, launch DBOS, and start each schedule.
    ///
    /// # Errors
    ///
    /// Fails when registration or launch fails.
    pub async fn launch(settings: &Settings, notifier: Arc<dyn Notifier>) -> dbos::Result<Self> {
        Self::launch_with_schedules(settings, notifier, None, None).await
    }

    /// Launch the shared executor, optionally registering the GeneralNag
    /// schedule. That schedule has its own opt-in flag so DBOS can run
    /// alongside the existing Restate CronJob during rollout.
    pub async fn launch_with_general_nag(
        settings: &Settings,
        notifier: Arc<dyn Notifier>,
        general_nag: Option<general_nag::Context>,
    ) -> dbos::Result<Self> {
        Self::launch_with_schedules(settings, notifier, general_nag, None).await
    }

    /// Launch the shared executor with the optional daily operations schedules.
    pub async fn launch_with_schedules(
        settings: &Settings,
        notifier: Arc<dyn Notifier>,
        general_nag: Option<general_nag::Context>,
        dri_digest: Option<dri_digest::Context>,
    ) -> dbos::Result<Self> {
        Self::launch_hosting(
            settings,
            notifier,
            Schedules {
                general_nag,
                dri_digest,
                ..Schedules::default()
            },
        )
        .await
    }

    /// Launch the shared executor with every optional schedule the host
    /// enabled.
    pub async fn launch_hosting(
        settings: &Settings,
        notifier: Arc<dyn Notifier>,
        schedules: Schedules,
    ) -> dbos::Result<Self> {
        Self::launch_hosting_with(settings, notifier, schedules, |_| Ok(Vec::new())).await
    }

    /// Launch this crate's workflows plus workflows registered by a dependent
    /// service crate. The callback runs before `DBOS::launch`; its schedule
    /// starters run after launch, alongside the built-in schedule loops.
    pub async fn launch_hosting_with<F>(
        settings: &Settings,
        notifier: Arc<dyn Notifier>,
        schedules: Schedules,
        register_external: F,
    ) -> dbos::Result<Self>
    where
        F: FnOnce(&dbos::DBOS) -> dbos::Result<Vec<ScheduleStarter>>,
    {
        let dbos = dbos::DBOS::new(settings.config());
        let clock = heartbeat::system_clock();
        let beat = heartbeat::register(&dbos, notifier, Arc::clone(&clock))?;
        let nag = register_optional(&dbos, schedules.general_nag, general_nag::register)?;
        let digest = register_optional(&dbos, schedules.dri_digest, dri_digest::register)?;
        let recovery = register_optional(
            &dbos,
            schedules.project_recovery,
            project_recovery::register,
        )?;
        let imports = register_optional(&dbos, schedules.source_imports, source_imports::register)?;
        let purge = register_optional(&dbos, schedules.retention_purge, retention_purge::register)?;
        let mirror =
            register_optional(&dbos, schedules.deadline_mirror, deadline_mirror::register)?;
        let retainers =
            register_optional(&dbos, schedules.plan_retainers, plan_retainers::register)?;
        let calendar = register_optional(
            &dbos,
            schedules.project_calendar,
            project_calendar::register_schedule,
        )?;
        let activation = register_optional(
            &dbos,
            schedules.purchase_activation,
            purchase_activation::register,
        )?;
        let restate_ports =
            RestatePorts::register(&dbos, schedules.billing_canary, schedules.notation_reset)?;
        let billing_digest =
            register_optional(&dbos, schedules.billing_digest, billing_digest::register)?;
        let manual = restate_ports.manual_starts(billing_digest.clone());
        let profit_digest =
            register_optional(&dbos, schedules.profit_digest, profit_digest::register)?;
        let ceo_digest = register_optional(&dbos, schedules.ceo_digest, ceo_digest::register)?;
        let linear_backlog =
            register_optional(&dbos, schedules.linear_backlog, linear_backlog::register)?;
        let starters =
            register_optional(&dbos, schedules.daily_cost_export, gcp_cost_daily::register)?
                .into_iter()
                .chain(retainers)
                .chain(calendar)
                .chain(register_external(&dbos)?);
        launch_with_parity(&dbos, schedules.forge_parity).await?;
        tracing::info!(app_name = APP_NAME, "DBOS launched");
        let stop = dbos::CancellationToken::new();
        let mut loops = vec![tokio::spawn(heartbeat::schedule(beat, clock, stop.clone()))];
        if let Some(linear_backlog) = linear_backlog {
            loops.push(tokio::spawn(linear_backlog::schedule(
                linear_backlog,
                stop.clone(),
            )));
        }
        if let Some(billing_digest) = billing_digest {
            loops.push(tokio::spawn(billing_digest::schedule(
                billing_digest,
                billing_digest::system_clock(),
                stop.clone(),
            )));
        }
        if let Some(profit_digest) = profit_digest {
            loops.push(tokio::spawn(profit_digest::schedule(
                profit_digest,
                profit_digest::system_clock(),
                stop.clone(),
            )));
        }
        if let Some(ceo_digest) = ceo_digest {
            loops.push(tokio::spawn(ceo_digest::schedule_weekly(
                ceo_digest.weekly,
                ceo_digest::system_clock(),
                stop.clone(),
            )));
            loops.push(tokio::spawn(ceo_digest::schedule_alert(
                ceo_digest.alert,
                ceo_digest::system_clock(),
                stop.clone(),
            )));
        }
        loops.extend(spawn_family_loops(
            &stop,
            FamilyStarters {
                nag,
                digest,
                recovery,
                purge,
                mirror,
                activation,
                imports,
            },
        ));
        restate_ports.spawn(&stop, &mut loops);
        loops.extend(starters.map(|start| start(stop.clone())));
        Ok(Self {
            dbos,
            stop,
            schedules: loops,
            manual,
        })
    }

    /// Clone manual-start handles without transferring executor shutdown ownership.
    #[must_use]
    pub fn manual_schedules(&self) -> ManualSchedules {
        self.manual.clone()
    }

    /// Stop scheduling, shut down the DBOS SDK, then join schedule loops.
    pub async fn shutdown(self) {
        self.stop.cancel();
        self.dbos.shutdown().await;
        for schedule in self.schedules {
            if let Err(error) = schedule.await {
                tracing::error!(%error, "DBOS schedule stopped abnormally");
            }
        }
        tracing::info!(app_name = APP_NAME, "DBOS shut down");
    }
}

/// The registered-but-unlaunched starters for the seven built-in family
/// schedules, in the engine's fixed family order.
struct FamilyStarters {
    nag: Option<general_nag::GeneralNagWorkflow>,
    digest: Option<dri_digest::DriDigestWorkflow>,
    recovery: Option<project_recovery::ProjectRecoveryWorkflow>,
    purge: Option<retention_purge::RetentionPurgeWorkflow>,
    mirror: Option<deadline_mirror::DeadlineMirrorWorkflow>,
    activation: Option<purchase_activation::PurchaseActivationWorkflow>,
    imports: Option<source_imports::SourceImportWorkflow>,
}

/// Spawn each registered family's schedule loop onto the runtime,
/// preserving the engine's fixed family order.
fn spawn_family_loops(
    stop: &dbos::CancellationToken,
    starters: FamilyStarters,
) -> Vec<tokio::task::JoinHandle<()>> {
    let FamilyStarters {
        nag,
        digest,
        recovery,
        purge,
        mirror,
        activation,
        imports,
    } = starters;
    let mut loops = Vec::new();
    if let Some(nag) = nag {
        loops.push(tokio::spawn(general_nag::schedule(
            nag,
            general_nag::system_clock(),
            stop.clone(),
        )));
    }
    if let Some(digest) = digest {
        loops.push(tokio::spawn(dri_digest::schedule(
            digest,
            dri_digest::system_clock(),
            stop.clone(),
        )));
    }
    if let Some(recovery) = recovery {
        loops.push(tokio::spawn(project_recovery::schedule(
            recovery,
            project_recovery::system_clock(),
            stop.clone(),
        )));
    }
    if let Some(purge) = purge {
        loops.push(tokio::spawn(retention_purge::schedule(
            purge,
            retention_purge::system_clock(),
            stop.clone(),
        )));
    }
    if let Some(mirror) = mirror {
        loops.push(tokio::spawn(deadline_mirror::schedule(
            mirror,
            deadline_mirror::system_clock(),
            stop.clone(),
        )));
    }
    if let Some(activation) = activation {
        loops.push(tokio::spawn(purchase_activation::schedule(
            activation,
            purchase_activation::system_clock(),
            stop.clone(),
        )));
    }
    if let Some(imports) = imports {
        loops.push(tokio::spawn(source_imports::schedule(
            imports,
            source_imports::system_clock(),
            stop.clone(),
        )));
    }
    loops
}

/// Run an [`Executor`] until `shutdown` resolves.
///
/// # Errors
///
/// Fails when registration or launch fails.
pub async fn serve(
    settings: &Settings,
    notifier: Arc<dyn Notifier>,
    shutdown: impl Future<Output = ()>,
) -> dbos::Result<()> {
    let executor = Executor::launch(settings, notifier).await?;
    shutdown.await;
    executor.shutdown().await;
    Ok(())
}

/// Run an executor with the optional GeneralNag host context.
///
/// # Errors
///
/// Fails when registration or launch fails.
pub async fn serve_with_general_nag(
    settings: &Settings,
    notifier: Arc<dyn Notifier>,
    general_nag: Option<general_nag::Context>,
    shutdown: impl Future<Output = ()>,
) -> dbos::Result<()> {
    let executor = Executor::launch_with_general_nag(settings, notifier, general_nag).await?;
    shutdown.await;
    executor.shutdown().await;
    Ok(())
}

/// Run an executor with the optional daily operations schedules.
pub async fn serve_with_schedules(
    settings: &Settings,
    notifier: Arc<dyn Notifier>,
    general_nag: Option<general_nag::Context>,
    dri_digest: Option<dri_digest::Context>,
    shutdown: impl Future<Output = ()>,
) -> dbos::Result<()> {
    let executor =
        Executor::launch_with_schedules(settings, notifier, general_nag, dri_digest).await?;
    shutdown.await;
    executor.shutdown().await;
    Ok(())
}

/// Run the shared executor with the typed schedule bundle prepared by a host.
pub async fn serve_with_hosting(
    settings: &Settings,
    notifier: Arc<dyn Notifier>,
    schedules: Schedules,
    shutdown: impl Future<Output = ()>,
) -> dbos::Result<()> {
    serve_with_hosting_and_external(settings, notifier, schedules, |_| Ok(Vec::new()), shutdown)
        .await
}

/// Run the shared executor while registering workflows owned by a dependent
/// service crate. External schedule loops share the executor's shutdown token.
pub async fn serve_with_hosting_and_external<F>(
    settings: &Settings,
    notifier: Arc<dyn Notifier>,
    schedules: Schedules,
    register_external: F,
    shutdown: impl Future<Output = ()>,
) -> dbos::Result<()>
where
    F: FnOnce(&dbos::DBOS) -> dbos::Result<Vec<ScheduleStarter>>,
{
    let executor =
        Executor::launch_hosting_with(settings, notifier, schedules, register_external).await?;
    shutdown.await;
    executor.shutdown().await;
    Ok(())
}

/// The deterministic key of the step this code is running inside:
/// `{workflow_id}/{step_id}`. `None` outside a DBOS step.
///
/// A recovery re-runs a step whose checkpoint never committed under the same
/// workflow id and step id, so the key is identical on every attempt. Store it
/// in a unique column in the same transaction as the write it guards, or pass
/// it as a provider's idempotency key.
#[must_use]
pub fn effect_key() -> Option<String> {
    let workflow_id = dbos::workflow_id()?;
    let step_id = dbos::step_id()?;
    Some(format!("{workflow_id}/{step_id}"))
}

/// The messages channel a deployment's own notices post to:
/// `SLACK_STAGING_MESSAGES_CHANNEL_ID` on a simulated-matters deployment,
/// `SLACK_MESSAGES_CHANNEL_ID` on any other.
///
/// # Errors
/// The selected channel is unset or blank.
pub fn messages_channel(
    lookup: impl Fn(&str) -> Option<String>,
    simulated: bool,
) -> Result<String, SettingsError> {
    required_channel(
        lookup,
        if simulated {
            "SLACK_STAGING_MESSAGES_CHANNEL_ID"
        } else {
            "SLACK_MESSAGES_CHANNEL_ID"
        },
    )
}

/// A required, non-blank Slack channel coordinate.
pub(crate) fn required_channel(
    lookup: impl Fn(&str) -> Option<String>,
    key: &'static str,
) -> Result<String, SettingsError> {
    lookup(key)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or(SettingsError::Missing(key))
}

/// Post `text` to `channel` at most once per [`effect_key`]: reuse a receipt
/// already in the channel's history since `oldest`, or post with the key as
/// Slack's correlation. `false` when the step has no key, Slack refuses, or
/// the receipt does not name this channel, key, and a timestamp.
pub(crate) async fn post_once(
    slack: &dyn crate::SlackBot,
    channel: &str,
    text: &str,
    oldest: chrono::DateTime<chrono::Utc>,
) -> bool {
    let Some(key) = effect_key() else {
        return false;
    };
    let receipt = match slack
        .message_receipt_by_correlation_since(channel, &key, oldest)
        .await
    {
        Ok(Some(receipt)) => receipt,
        Ok(None) => match slack
            .post_message_with_receipt(channel, text, Some(&key))
            .await
        {
            Ok(receipt) => receipt,
            Err(_) => return false,
        },
        Err(_) => return false,
    };
    receipt.channel_id == channel
        && receipt.correlation_id.as_deref() == Some(key.as_str())
        && !receipt.timestamp.is_empty()
}

#[cfg(test)]
mod tests {
    use super::{
        config, effect_key, Settings, SettingsError, APP_NAME, DATABASE_ENV, EXECUTOR_ID_ENV,
        HOST_ENV, PASSWORD_ENV, PORT_ENV, RELEASE_ENV, ROOT_CERT_ENV, SSLMODE_ENV, SYSTEM_SCHEMA,
        USER_ENV,
    };

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        move |key| {
            pairs
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        }
    }

    #[tokio::test]
    async fn billing_canary_keeps_manual_registration_when_its_timer_is_disabled() {
        struct UnusedProvider;
        #[async_trait::async_trait]
        impl crate::billing_canary::BillingCanaryProvider for UnusedProvider {
            async fn ensure_contact(
                &self,
                _: &billing::ContactRequest,
            ) -> Result<billing::ContactId, crate::billing_canary::CanaryProviderError>
            {
                panic!("an unlaunched schedule must never contact a provider")
            }
        }
        for scheduled in [None, Some(false), Some(true)] {
            let dbos = dbos::DBOS::new(config("postgres://example.invalid/navigator", "test"));
            let context = super::billing_canary::Context::new(
                std::sync::Arc::new(UnusedProvider),
                std::sync::Arc::new(crate::CapturingEmail::new()),
            );
            let context = if let Some(enabled) = scheduled {
                context.with_schedule_enabled(enabled)
            } else {
                context
            };
            let ports = super::RestatePorts::register(&dbos, Some(context), None).unwrap();
            assert!(
                ports.manual_starts(None).billing_canary.is_some(),
                "registration is independent of automatic starts"
            );
            let stop = dbos::CancellationToken::new();
            stop.cancel();
            let mut loops = Vec::new();
            ports.spawn(&stop, &mut loops);
            assert_eq!(loops.len(), usize::from(scheduled.unwrap_or(true)));
            for task in loops {
                tokio::time::timeout(std::time::Duration::from_secs(2), task)
                    .await
                    .unwrap()
                    .unwrap();
            }
        }
    }

    #[test]
    fn config_pins_the_system_schema_and_version() {
        let config = config("postgres://example.invalid/navigator", "26.10.13");
        assert_eq!(config.app_name, APP_NAME);
        assert_eq!(config.schema, SYSTEM_SCHEMA);
        let url = url::Url::parse(&config.database_url).expect("valid database URL");
        assert_eq!(
            url.query_pairs()
                .find(|(name, _)| name == "options[lock_timeout]")
                .map(|(_, value)| value.into_owned())
                .as_deref(),
            Some("5s")
        );
        let config = super::config_with_executor_id(
            "postgres://example.invalid/navigator?sslmode=verify-full&sslrootcert=%2Ftmp%2Fca.crt&options%5Bapplication_name%5D=existing",
            "26.10.13",
            "navigator-web-abc123",
        );
        let url = url::Url::parse(&config.database_url).expect("valid database URL");
        let options: Vec<(String, String)> = url
            .query_pairs()
            .map(|(name, value)| (name.into_owned(), value.into_owned()))
            .collect();
        assert!(options.contains(&("sslmode".to_owned(), "verify-full".to_owned())));
        assert!(options.contains(&("sslrootcert".to_owned(), "/tmp/ca.crt".to_owned())));
        assert!(options.contains(&(
            "options[application_name]".to_owned(),
            "existing".to_owned()
        )));
        assert!(options.contains(&(
            "options[lock_timeout]".to_owned(),
            super::SYSTEM_DB_LOCK_TIMEOUT.to_owned()
        )));
        assert_eq!(config.app_version.as_deref(), Some("26.10.13"));
    }

    #[test]
    fn effect_key_is_absent_outside_a_step() {
        assert_eq!(effect_key(), None);
    }

    #[test]
    fn workflow_settings_accept_only_strict_booleans() {
        const FLAG: &str = "NAVIGATOR_DBOS_FINANCE_AGGREGATE_ENABLED";
        assert_eq!(super::flag_enabled(FLAG, None), Ok(false));
        assert_eq!(super::flag_enabled(FLAG, Some("")), Ok(false));
        assert_eq!(super::flag_enabled(FLAG, Some("false")), Ok(false));
        assert_eq!(super::flag_enabled(FLAG, Some("true")), Ok(true));
        assert_eq!(
            super::flag_enabled(FLAG, Some("TRUE")),
            Err(SettingsError::Invalid(FLAG))
        );
        assert_eq!(
            super::flag_enabled(FLAG, Some("unset")),
            Err(SettingsError::Invalid(FLAG))
        );
    }

    const LOGIN: &[(&str, &str)] = &[
        (HOST_ENV, "navigator-postgres.example.invalid"),
        (PORT_ENV, "5432"),
        (USER_ENV, "synthetic_dbos"),
        (PASSWORD_ENV, "not/a secret"),
        (DATABASE_ENV, "navigator"),
        (RELEASE_ENV, "26.10.13"),
        (EXECUTOR_ID_ENV, "navigator-web-abc123"),
    ];

    fn without(key: &str) -> Vec<(&'static str, &'static str)> {
        LOGIN
            .iter()
            .copied()
            .filter(|(name, _)| *name != key)
            .collect()
    }

    fn with(key: &'static str, value: &'static str) -> Vec<(&'static str, &'static str)> {
        let mut pairs = without(key);
        pairs.push((key, value));
        pairs
    }

    fn query_pairs(url: &url::Url) -> Vec<(String, String)> {
        url.query_pairs()
            .map(|(name, value)| (name.into_owned(), value.into_owned()))
            .collect()
    }

    #[test]
    fn dbos_settings_build_a_verified_tls_connection() {
        let settings = Settings::from_lookup(lookup(LOGIN)).expect("complete settings");
        let config = settings.config();
        assert_eq!(config.app_version.as_deref(), Some("26.10.13"));
        assert_eq!(config.executor_id.as_deref(), Some("navigator-web-abc123"));
        assert_eq!(config.schema, SYSTEM_SCHEMA);
        let url = url::Url::parse(&config.database_url).expect("a connection URL");
        assert_eq!(url.host_str(), Some("navigator-postgres.example.invalid"));
        assert_eq!(url.port(), Some(5432));
        assert_eq!(url.username(), "synthetic_dbos");
        assert_eq!(url.password(), Some("not%2Fa%20secret"));
        assert_eq!(url.path(), "/navigator");
        assert_eq!(
            query_pairs(&url),
            [
                ("sslmode", "verify-full"),
                ("options[lock_timeout]", super::SYSTEM_DB_LOCK_TIMEOUT),
            ]
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
        );

        let with_ca = Settings::from_lookup(lookup(&with(ROOT_CERT_ENV, "/etc/ca.crt")))
            .expect("settings with a CA bundle");
        let url = url::Url::parse(&with_ca.config().database_url).expect("a connection URL");
        assert!(query_pairs(&url).contains(&("sslrootcert".to_owned(), "/etc/ca.crt".to_owned())));
    }

    #[test]
    fn dbos_settings_fail_closed() {
        for key in [
            HOST_ENV,
            PORT_ENV,
            USER_ENV,
            PASSWORD_ENV,
            DATABASE_ENV,
            RELEASE_ENV,
            EXECUTOR_ID_ENV,
        ] {
            assert_eq!(
                Settings::from_lookup(lookup(&without(key))).err(),
                Some(SettingsError::Missing(key)),
                "{key}"
            );
        }
        assert_eq!(
            Settings::from_lookup(lookup(&with(RELEASE_ENV, "unknown"))).err(),
            Some(SettingsError::Missing(RELEASE_ENV))
        );
        assert_eq!(
            Settings::from_lookup(lookup(&with(PORT_ENV, "postgres"))).err(),
            Some(SettingsError::Invalid(PORT_ENV))
        );
        for empty in ["", "   "] {
            assert_eq!(
                Settings::from_lookup(lookup(&with(EXECUTOR_ID_ENV, empty))).err(),
                Some(SettingsError::Missing(EXECUTOR_ID_ENV))
            );
        }
        for valid in ["a", "ab", "pod.name"] {
            assert!(
                Settings::from_lookup(lookup(&with(EXECUTOR_ID_ENV, valid))).is_ok(),
                "{valid}"
            );
        }
        for invalid in ["Pod-A", "pod/name"] {
            assert_eq!(
                Settings::from_lookup(lookup(&with(EXECUTOR_ID_ENV, invalid))).err(),
                Some(SettingsError::Invalid(EXECUTOR_ID_ENV)),
                "{invalid}"
            );
        }
        let too_long = "x".repeat(257);
        let login = lookup(LOGIN);
        assert_eq!(
            Settings::from_lookup(|key| {
                if key == EXECUTOR_ID_ENV {
                    Some(too_long.clone())
                } else {
                    login(key)
                }
            })
            .err(),
            Some(SettingsError::Invalid(EXECUTOR_ID_ENV))
        );
        assert_eq!(
            Settings::from_lookup(lookup(&with(SSLMODE_ENV, "disable"))).err(),
            Some(SettingsError::Invalid(SSLMODE_ENV)),
            "plaintext only on loopback"
        );
        let mut loopback = with(SSLMODE_ENV, "disable");
        loopback.retain(|(name, _)| *name != HOST_ENV);
        loopback.push((HOST_ENV, "127.0.0.1"));
        assert!(Settings::from_lookup(lookup(&loopback)).is_ok());
    }
}

#[cfg(test)]
mod schedule_ownership_tests {
    use super::*;

    #[test]
    fn combined_schedules_keep_worker_provider_and_timer_policy_and_application_only_contexts() {
        let provider: Arc<dyn crate::billing_canary::BillingCanaryProvider> =
            Arc::new(crate::billing_canary::BillingProviderAdapter(Arc::new(
                billing::StubBillingProvider::for_simulated_matters(),
            )));
        let mut worker =
            billing_canary::Context::new(provider.clone(), Arc::new(crate::CapturingEmail::new()))
                .with_schedule_enabled(false);
        worker.recipient = "worker@example.test".into();
        let mut application_canary = worker.clone().with_schedule_enabled(true);
        application_canary.recipient = "application@example.test".into();
        let linear = linear_backlog::Context::from_lookup(
            Arc::new(crate::CapturingSlackBot::new()),
            |key| match key {
                "SLACK_LINEAR_BACKLOG_CHANNEL_ID" => Some("C123".into()),
                "LINEAR_BACKLOG_READ_KEY" => Some("synthetic".into()),
                _ => None,
            },
        )
        .unwrap()
        .unwrap();
        let combined = Schedules {
            billing_canary: Some(worker),
            ..Default::default()
        }
        .with_application_schedules(Schedules {
            billing_canary: Some(application_canary.clone()),
            linear_backlog: Some(linear.clone()),
            ..Default::default()
        });
        let canary = combined.billing_canary.unwrap();
        assert!(Arc::ptr_eq(&canary.provider, &provider));
        assert_eq!(canary.recipient, "worker@example.test");
        assert!(!canary.schedule_enabled);
        assert!(combined.linear_backlog.is_some());
        let unregistered = Schedules::default().with_application_schedules(Schedules {
            billing_canary: Some(application_canary),
            linear_backlog: Some(linear),
            ..Default::default()
        });
        assert!(
            unregistered.billing_canary.is_none(),
            "the application cannot replace worker admission"
        );
        assert!(unregistered.linear_backlog.is_some());
    }
}
