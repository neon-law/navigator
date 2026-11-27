//! Shared application and worker host for PostgreSQL-backed DBOS execution.
//! Opens the shared store connection and builds the worker's `EmailService`
//! (bare `SendGrid` in production, `CapturingEmail` otherwise).
//!
//! The host requires DBOS and complete native route ownership.

use std::sync::Arc;

mod local_providers;

use crate::email_summary::SummaryProviders;
use crate::engine::Engine;

use crate::firm_slack::{FirmSlackBots, ProjectSlackBots};

use crate::inbound_message_alert::InboundMessageAlertService;
use crate::inbound_triage::{InboundTriageService, TriageProviders};
use crate::nightly_project_update::repository::{EnvFirmForges, RepositoryForge};

use crate::social_publisher::SocialPublisherService;
use crate::youtube_comments::YoutubeCommentsPollerService;

use anyhow::Context;

use workflows::dbos_engine;

async fn social_runtime_kms_from_env() -> Arc<dyn cloud::RuntimeKms> {
    let Ok(config) = cloud::GoogleKmsConfig::from_env() else {
        tracing::info!("social publishing KMS: unconfigured");
        return Arc::new(cloud::NoRuntimeKms);
    };
    let Ok(tokens) = cloud::AdcTokenSource::new().await else {
        tracing::warn!("social publishing KMS: workload identity is unavailable");
        return Arc::new(cloud::NoRuntimeKms);
    };
    Arc::new(cloud::GoogleKms::new(config, Arc::new(tokens)))
}

/// Run the standalone DBOS worker.
pub async fn run(
    shutdown: impl std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
) -> anyhow::Result<()> {
    Box::pin(run_host(
        Box::pin(shutdown),
        None,
        Box::new(|_| Ok(Vec::new())),
        HostMode::Standalone,
        None,
    ))
    .await
}

/// Host every selected DBOS workflow alongside the application in one process.
/// The readiness sender fires after registration, launch and listener binding.
pub async fn run_embedded<F>(
    shutdown: impl std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    schedules: dbos_engine::Schedules,
    register_external: F,
    ready: tokio::sync::oneshot::Sender<()>,
) -> anyhow::Result<()>
where
    F: FnOnce(&dbos::DBOS) -> dbos::Result<Vec<dbos_engine::ScheduleStarter>> + Send + 'static,
{
    anyhow::ensure!(
        Engine::from_lookup(|key| std::env::var(key).ok())? == Engine::Dbos,
        "Embedded workflows require NAVIGATOR_WORKFLOW_ENGINE=dbos"
    );
    Box::pin(run_host(
        Box::pin(shutdown),
        Some(schedules),
        Box::new(register_external),
        HostMode::Embedded,
        Some(ready),
    ))
    .await
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HostMode {
    Standalone,
    Embedded,
}

fn require_native_routes(
    engine: Engine,
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<()> {
    anyhow::ensure!(engine == Engine::Dbos, "the workflow host requires DBOS");
    for workflow in store::workflow_engine::WORKFLOWS {
        anyhow::ensure!(
            dbos_engine::flag_enabled(workflow.flag, lookup(workflow.flag).as_deref())?,
            "{} requires {}=true in the native worker",
            workflow.name,
            workflow.flag
        );
    }
    Ok(())
}

// Erase host callbacks at the crate boundary so both roles share one compiled
// startup implementation rather than monomorphizing the registry in the app.
type RegisterExternal =
    Box<dyn FnOnce(&dbos::DBOS) -> dbos::Result<Vec<dbos_engine::ScheduleStarter>> + Send>;
type Shutdown = std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>;

#[allow(clippy::too_many_lines)] // Shared startup wiring for every bound service.
async fn run_host(
    shutdown: Shutdown,
    app_schedules: Option<dbos_engine::Schedules>,
    register_external: RegisterExternal,
    mode: HostMode,
    ready: Option<tokio::sync::oneshot::Sender<()>>,
) -> anyhow::Result<()> {
    let environment =
        store::DeploymentEnvironment::from_env().context("parse NAVIGATOR_ENVIRONMENT")?;
    let postgres = store::postgres::PostgresDb::application()
        .await
        .context("verify PostgreSQL worker authority")?;
    let object_selection =
        store::durable_object_handover::require_runtime_selection(&postgres, |key| {
            std::env::var(key).ok()
        })
        .await
        .context("verify durable object engine ownership")?;

    anyhow::ensure!(
        !object_selection.legacy_objects(),
        "the workflow host requires verified DBOS ownership"
    );
    // Drives `DriDigest` and `GeneralNag` staging disclosure and the
    // production-only open-matters follow-up — the same flag the site-wide
    // "simulated projects" banner reads.
    let simulated_matters =
        store::sample_matters(environment).context("resolve NAVIGATOR_SIMULATED_MATTERS")?;
    let lookup = |key: &str| std::env::var(key).ok();
    let enabled = |flag| dbos_engine::flag_enabled(flag, lookup(flag).as_deref());
    let triage_dbos_enabled = enabled(workflows::trigger::INBOUND_TRIAGE_DBOS_ENABLED_ENV)?;
    let inbox_attachment_dbos_enabled =
        enabled(workflows::trigger::INBOX_ATTACHMENT_FILING_DBOS_ENABLED_ENV)?;
    anyhow::ensure!(
        !inbox_attachment_dbos_enabled || triage_dbos_enabled,
        "Inbox attachment filing requires the DBOS Inbox triage workflow"
    );
    let youtube_dbos_enabled = enabled(workflows::trigger::YOUTUBE_COMMENTS_DBOS_ENABLED_ENV)?;
    let sandbox_credit_reset_enabled =
        enabled(workflows::trigger::SANDBOX_CREDIT_RESET_DBOS_ENABLED_ENV)?;
    let engine = Engine::from_lookup(lookup).context("select the durable engine")?;
    anyhow::ensure!(
        engine == Engine::Dbos
            || !(youtube_dbos_enabled || triage_dbos_enabled || sandbox_credit_reset_enabled),
        "The enabled DBOS workflow routes require the DBOS executor"
    );

    require_native_routes(engine, lookup)?;
    // Runtime KMS unseals Firm integration secrets, including the Slack Firm's
    // bot token every deployment notice posts with.
    let kms = social_runtime_kms_from_env().await;
    let (slack_bot, slack_bot_enabled) = workflows::firm_slack::deployment_bot_or_capture(
        store::surreal::SurrealDb::uninitialized(),
        kms.clone(),
        lookup,
    )
    .map_err(anyhow::Error::msg)
    .context("configure the deployment's Slack bot")?;
    let ops_notifier = workflows::notify::ops_notifier(slack_bot.clone(), lookup);
    let dbos_hosting = if engine == Engine::Dbos {
        let local_providers = local_providers::LocalProviders::from_lookup(
            mode,
            environment,
            simulated_matters,
            &lookup,
        )
        .context("select native local providers")?;
        let settings = dbos_engine::Settings::from_lookup(lookup).context("configure DBOS")?;
        let general_nag_enabled = workflows::dbos_engine::general_nag::enabled(lookup)
            .context("read NAVIGATOR_DBOS_GENERAL_NAG_ENABLED")?;
        let dri_digest_enabled = workflows::dbos_engine::dri_digest::enabled(lookup)
            .context("read NAVIGATOR_DBOS_DRI_DIGEST_ENABLED")?;
        let notation_reset_enabled = workflows::dbos_engine::notation_reset::enabled(lookup)
            .context("read NAVIGATOR_DBOS_NOTATION_RESET_ENABLED")?;
        let profit_digest_enabled =
            workflows::dbos_engine::profit_digest::configured(simulated_matters, lookup);
        let archives_enabled = archives::workflow::dbos_enabled(lookup)
            .map_err(anyhow::Error::msg)
            .context("read NAVIGATOR_DBOS_ARCHIVES_ENABLED")?;
        let reconcile_enabled = crate::dbos_reconcile::enabled(lookup)
            .map_err(anyhow::Error::msg)
            .context("read NAVIGATOR_DBOS_RECONCILE_INVOICES_ENABLED")?;
        let finance_aggregate_enabled = crate::dbos_finance_aggregate::enabled(lookup)
            .map_err(anyhow::Error::msg)
            .context("read NAVIGATOR_DBOS_FINANCE_AGGREGATE_ENABLED")?;
        let nightly_project_update_enabled = crate::dbos_nightly_project_update::enabled(lookup)
            .map_err(anyhow::Error::msg)
            .context("read NAVIGATOR_DBOS_NIGHTLY_PROJECT_UPDATE_ENABLED")?;
        let email_summary_enabled = crate::dbos_email_summary::enabled(lookup)
            .map_err(anyhow::Error::msg)
            .context("read NAVIGATOR_DBOS_EMAIL_SUMMARY_ENABLED")?;
        let inbound_message_alert_enabled = crate::dbos_inbound_message_alert::enabled(lookup)
            .map_err(anyhow::Error::msg)
            .context("read NAVIGATOR_DBOS_INBOUND_MESSAGE_ALERT_ENABLED")?;
        // The Project Slack lane posts with each Project's owning Firm's bot
        // (ENG-392) rather than the deployment's Slack Firm bot.
        let project_slack_enabled = crate::dbos_project_slack::enabled(lookup)
            .map_err(anyhow::Error::msg)
            .context("read NAVIGATOR_DBOS_PROJECT_SLACK_ENABLED")?;
        let project_slack_postgres = if project_slack_enabled {
            Some(
                store::postgres::PostgresDb::from_env()
                    .await
                    .context("connect PostgreSQL for the DBOS Project Slack lane")?,
            )
        } else {
            None
        };
        let notation_enabled = crate::dbos_notation::enabled(lookup)
            .map_err(anyhow::Error::msg)
            .context("read NAVIGATOR_DBOS_NOTATION_ENABLED")?;
        let (social_publisher_enabled, social_reconciler_enabled) =
            crate::dbos_social_publisher::enabled(lookup)
                .map_err(anyhow::Error::msg)
                .context("validate DBOS social registration flags")?;
        let finance_claim_postgres =
            if finance_aggregate_enabled || notation_enabled || email_summary_enabled {
                Some(
                    store::postgres::PostgresDb::from_env()
                        .await
                        .context("connect PostgreSQL for DBOS request identity claims")?,
                )
            } else {
                None
            };
        let dbos_ingress_token = if finance_aggregate_enabled
            || reconcile_enabled
            || nightly_project_update_enabled
            || email_summary_enabled
            || inbound_message_alert_enabled
            || project_slack_enabled
            || notation_enabled
            || social_publisher_enabled
            || social_reconciler_enabled
            || youtube_dbos_enabled
            || triage_dbos_enabled
            || sandbox_credit_reset_enabled
        {
            let token = lookup("NAVIGATOR_DBOS_INGRESS_TOKEN")
                .filter(|token| !token.trim().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "NAVIGATOR_DBOS_INGRESS_TOKEN is required when an authenticated DBOS ingress is enabled"
                    )
                })?;
            crate::dbos_ingress::FinanceIngress::validate_token(&token)
                .map_err(anyhow::Error::msg)?;
            Some(token)
        } else {
            None
        };
        let surreal = if general_nag_enabled
            || dri_digest_enabled
            || notation_reset_enabled
            || profit_digest_enabled
            || reconcile_enabled
            || finance_aggregate_enabled
            || nightly_project_update_enabled
            || email_summary_enabled
            || inbound_message_alert_enabled
            || project_slack_enabled
            || notation_enabled
            || social_publisher_enabled
            || social_reconciler_enabled
        {
            let surreal = store::surreal::SurrealDb::uninitialized();
            Some(surreal)
        } else {
            None
        };
        let general_nag = if general_nag_enabled {
            let source = surreal
                .clone()
                .context("GeneralNag database not configured")?;
            Some(if let Some(local) = &local_providers {
                local.general_nag(source, postgres.clone())
            } else {
                workflows::dbos_engine::general_nag::Context::from_lookup(
                    source,
                    slack_bot.clone(),
                    simulated_matters,
                    lookup,
                )
                .context("validate GeneralNag delivery configuration")?
            })
        } else {
            None
        };
        let notation_reset = if notation_reset_enabled {
            let source = surreal
                .clone()
                .context("NotationReset database not configured")?;
            let label =
                workflows::notation_reset::environment_label(environment, simulated_matters);
            Some(if let Some(local) = &local_providers {
                local.notation_reset(source, label)
            } else {
                workflows::dbos_engine::notation_reset::Context::from_lookup(
                    source,
                    slack_bot.clone(),
                    label,
                    simulated_matters,
                    lookup,
                )
                .context("validate NotationReset delivery configuration")?
            })
        } else {
            None
        };
        let profit_digest = if profit_digest_enabled {
            use workflows::dbos_engine::profit_digest;
            let surreal = surreal
                .clone()
                .ok_or_else(|| anyhow::anyhow!("ProfitDigest database not configured"))?;
            let pg = store::postgres::PostgresDb::from_env()
                .await
                .context("connect PostgreSQL for the profit digest")?;
            Some(
                profit_digest::Context::from_lookup(
                    Arc::new(profit_digest::StoreProfitSource::new(surreal, pg)),
                    slack_bot.clone(),
                    simulated_matters,
                    lookup,
                )
                .context("validate ProfitDigest delivery configuration")?,
            )
        } else {
            None
        };
        let reconcile_context = if reconcile_enabled {
            let source = surreal
                .clone()
                .context("ReconcileInvoices database not configured")?;
            Some(if let Some(local) = &local_providers {
                local.reconcile(source, postgres.clone(), &lookup)?
            } else {
                anyhow::ensure!(
                    slack_bot_enabled,
                    "DBOS ReconcileInvoices requires the deployment's Slack bot"
                );
                crate::dbos_reconcile::Context::from_env(
                    source,
                    slack_bot.clone(),
                    simulated_matters,
                    lookup,
                )
                .map_err(anyhow::Error::msg)
                .context("configure DBOS ReconcileInvoices")?
            })
        } else {
            None
        };
        let dri_digest = if let Some(local) = &local_providers {
            if dri_digest_enabled {
                Some(
                    local.dri_digest(
                        surreal
                            .clone()
                            .context("DriDigest database not configured")?,
                        postgres.clone(),
                    ),
                )
            } else {
                None
            }
        } else {
            crate::dri_digest::dbos_context(
                surreal.clone(),
                slack_bot.clone(),
                simulated_matters,
                lookup,
            )
            .context("configure DBOS DriDigest")?
        };
        let billing_canary = if workflows::dbos_engine::billing_canary::enabled(lookup)
            .context("read NAVIGATOR_DBOS_BILLING_CANARY_ENABLED")?
        {
            Some(if let Some(local) = &local_providers {
                local.canary(&lookup)?
            } else {
                workflows::dbos_engine::billing_canary::Context::from_lookup(
                    slack_bot.clone(),
                    lookup,
                )
                .context("configure DBOS BillingCanary")?
            })
        } else {
            None
        };
        let billing_digest = if workflows::dbos_engine::billing_digest::enabled(lookup)
            .context("read NAVIGATOR_DBOS_BILLING_DIGEST_ENABLED")?
        {
            Some(
                workflows::dbos_engine::billing_digest::Context::from_lookup(
                    slack_bot.clone(),
                    lookup,
                )
                .context("configure DBOS BillingDigest")?,
            )
        } else {
            None
        };
        let archives_context = if archives_enabled {
            Some(if let Some(local) = &local_providers {
                local.archives()
            } else {
                let channel_id = dbos_engine::messages_channel(lookup, simulated_matters)
                    .context("DBOS Archives requires its messages channel")?;
                anyhow::ensure!(
                    slack_bot_enabled,
                    "DBOS Archives requires the deployment's Slack bot"
                );
                (slack_bot.clone(), channel_id, slack_bot_enabled)
            })
        } else {
            None
        };
        Some((
            settings,
            dbos_engine::Schedules {
                forge_parity: true,
                general_nag,
                dri_digest,
                billing_canary,
                billing_digest,
                notation_reset,
                profit_digest,
                ..Default::default()
            },
            archives_context,
            reconcile_context,
            if finance_aggregate_enabled {
                surreal.clone()
            } else {
                None
            },
            if reconcile_enabled {
                surreal.clone()
            } else {
                None
            },
            dbos_ingress_token,
            finance_claim_postgres,
            nightly_project_update_enabled,
            email_summary_enabled,
            inbound_message_alert_enabled,
            if email_summary_enabled || inbound_message_alert_enabled {
                surreal.clone()
            } else {
                None
            },
            project_slack_enabled,
            project_slack_postgres,
            notation_enabled,
            if social_publisher_enabled {
                surreal
                    .clone()
                    .map(|surreal| (surreal, social_reconciler_enabled))
            } else {
                None
            },
        ))
    } else {
        None
    };

    // Branding is Neon Law by default; NAVIGATOR_CUSTOM_BRANDING opts into a
    // mounted white-label bundle (fail closed when set but absent).
    let brand_bundle = views::brand_bundle::BrandBundle::from_env()
        .context("resolve brand bundle from NAVIGATOR_CUSTOM_BRANDING")?;
    let branding = brand_bundle
        .as_ref()
        .map_or(&views::brand::DEFAULT_BRANDING, |bundle| {
            views::brand::Branding::from_manifest(&bundle.manifest)
        });
    if mode == HostMode::Standalone {
        views::brand::install_process_branding(branding)
            .map_err(anyhow::Error::msg)
            .context("install process branding")?;
    }
    tracing::info!(environment = environment.as_str(), "configured deployment");
    crate::email_config::validate_for_deployment(environment, |key| std::env::var(key).ok())
        .context("validate deployment email configuration")?;
    // The email `@font-face` block renders here, not in `web`, so the asset
    // origin must be validated in this binary too — a malformed value would
    // otherwise reach the outbound-mail `<style>` block un-checked.
    crate::asset_config::validate_for_deployment(|key| std::env::var(key).ok())
        .context("validate deployment asset base URL")?;

    let surreal = store::surreal::SurrealDb::uninitialized();

    // Every worker send — the workflow-driven template mail and the
    // Notation service's client mail — journals one `sent_email` row, the
    // same decorator `web` wraps its backend in.
    let (backend, sender) = crate::email_config::select_backend(|key| std::env::var(key).ok())
        .context("build email service from env")?;
    let email: Arc<dyn workflows::EmailService> =
        Arc::new(workflows::email::LoggingEmail::new(backend, sender));
    tracing::info!(
        backend = if std::env::var("NAVIGATOR_EMAIL_BACKEND").as_deref() == Ok("sendgrid") {
            "SendGrid"
        } else {
            "Capturing"
        },
        "workflows-service email backend"
    );

    // Operations notifications post with the deployment's Slack bot; client
    // mail uses the separately configured email backend.

    // The public origin the internal Project canvas links the Portal
    // under; unset, the canvas links the portal by its path.
    let portal_origin = std::env::var("NAVIGATOR_PUBLIC_HOST")
        .ok()
        .map(|host| host.trim().to_string())
        .filter(|host| !host.is_empty())
        .map(|host| {
            if host.starts_with("http://") || host.starts_with("https://") {
                host
            } else {
                format!("https://{host}")
            }
        });
    // `#general`'s channel ID — unset in KIND, where the CronJob is still
    // installed but has no live Slack destination. An empty value fails the
    // `GeneralNag` run as a terminal error rather than posting nowhere.

    // The `#finance` destination for the nightly Xero and IOLTA report. It
    // uses the same configured Slack App as `#general`; the empty-value rule
    // is the same too, so a run with nowhere to report is a terminal error.
    let messages_channel = std::env::var("SLACK_MESSAGES_CHANNEL_ID").unwrap_or_default();
    let staging_messages_channel =
        std::env::var("SLACK_STAGING_MESSAGES_CHANNEL_ID").unwrap_or_default();
    // How `NotationReset` names this deployment in its daily post.

    // The GitHub forge for the daily Project sync; `None` when the deployment
    // configures no GitHub provider (KIND), a partial configuration is fatal.
    let forge: Option<Arc<dyn cloud::forge::ForgeService>> =
        store::project_surfaces::forge_from_env()
            .context("configure GitHub forge")?
            .map(|forge| Arc::new(forge) as Arc<dyn cloud::forge::ForgeService>);
    tracing::info!(
        backend = if slack_bot_enabled {
            "Slack"
        } else {
            "Capturing"
        },
        "workflows-service ops-notification backend"
    );

    // Object storage for `generate_pdf__*` step dispatch (the worker
    // renders the PDF and persists it here). Same `cloud::from_env`
    // backend selection as `web`: GCS in prod, FsStorage in dev.
    let storage = cloud::from_env()
        .await
        .context("configure object storage")?;
    let dbos_finance_storage = storage.clone();
    let dbos_notation_email = Arc::clone(&email);
    let dbos_notation_storage = Arc::clone(&storage);
    let assets_storage = cloud::assets_from_env()
        .await
        .context("configure public assets storage")?;
    let dbos_social_kms = kms.clone();
    let youtube_dbos = youtube_dbos_enabled
        .then(|| YoutubeCommentsPollerService::new(surreal.clone(), kms.clone()));
    let dbos_social_assets_storage = assets_storage.clone();
    let repository_forge = RepositoryForge::ByProject {
        default: cloud::ForgeBackend::from_lookup(|key| std::env::var(key).ok())
            .context("select the default Project forge")?,
        github: forge,
        forgejo: Arc::new(EnvFirmForges::new(kms.clone())),
        forgejo_origin: std::env::var(cloud::forgejo_forge::NAVIGATOR_FORGEJO_ROOT_URL_ENV)
            .ok()
            .and_then(|root| url::Url::parse(root.trim()).ok())
            .map(|root| root.origin()),
    };
    let sandbox_credit_reset_stores = if sandbox_credit_reset_enabled {
        Some((
            store::postgres::PostgresDb::from_env()
                .await
                .context("connect PostgreSQL for DBOS SandboxCreditReset")?,
            surreal.clone(),
        ))
    } else {
        None
    };
    // Every Project channel is reached through its owning Firm's own Slack
    // bot, unsealed from the Firm's `slack_bot_token` credential (ENG-392).
    let project_slack_bots: Arc<dyn ProjectSlackBots> = Arc::new(FirmSlackBots::new(kms.clone()));
    let dbos_nightly_surreal = surreal.clone();
    let dbos_notation_surreal = surreal.clone();
    let dbos_nightly_slack = slack_bot.clone();
    let dbos_nightly_bots = project_slack_bots.clone();
    let dbos_project_slack_surreal = surreal.clone();
    let dbos_project_slack_bots = project_slack_bots.clone();
    let dbos_nightly_repository_forge = repository_forge.clone();
    let dbos_nightly_portal_origin = portal_origin.clone();
    let dbos_nightly_messages_channel = messages_channel.clone();
    let dbos_nightly_staging_messages_channel = staging_messages_channel.clone();
    // Nightly Folder convergence must remain available without Slack configuration.
    // The parent validates/posts Slack only after visiting the full Project roster.
    let summary_providers = SummaryProviders::from_env().context("configure summary providers")?;

    let dbos_email_storage = storage.clone();
    let dbos_email_slack = slack_bot.clone();
    let dbos_alert_slack = slack_bot.clone();
    let dbos_alert_messages_channel = messages_channel.clone();
    let dbos_alert_staging_messages_channel = staging_messages_channel.clone();
    let dbos_alert_portal_origin = portal_origin.clone();
    // Rules-only when `NAVIGATOR_TRIAGE_ENABLED` is unset: the service stays
    // registered and every message still gets a triage row, from rules alone.
    let triage_providers = TriageProviders::from_env().context("configure triage providers")?;
    let inbound_triage = InboundTriageService::new(surreal.clone(), triage_providers);
    let triage_dbos = triage_dbos_enabled.then(|| inbound_triage.clone());
    let inbox_attachment_dbos = if inbox_attachment_dbos_enabled {
        let address = lookup("NAVIGATOR_CLAMD_ADDR")
            .filter(|address| !address.trim().is_empty())
            .context("NAVIGATOR_CLAMD_ADDR is required for Inbox attachment filing")?;
        let scanner = Arc::new(cloud::attachment_scanner::ClamdAttachmentScanner::new(
            address,
        ));
        Some((surreal.clone(), storage.clone(), scanner))
    } else {
        None
    };

    // One endpoint hosts every workflow: the `Notation` virtual object,
    // `EmailSummary`, and the `Archives` nightly-export, `BillingCanary`,
    // `BillingDigest`
    // (daily GCP cost email), `ReconcileInvoices`, `DriDigest` (nightly
    // project-DRI notice), `GeneralNag` (daily `#general` jab), and
    // `NightlyProjectUpdate` (daily parent DAG over every Project, with one
    // child workflow per tag), and `NotationReset` (daily draft reset)
    // workflows, each with a thin CronJob trigger. All run against this one
    // worker — there is no per-workflow pod. The exact set of service names
    // bound here is mirrored in `crate::registry`, which the
    // registry tests guard against drift (count + PascalCase naming).

    let health_listener = bind_health_listener().await?;
    let manual_token = std::env::var(workflows::trigger::DBOS_INGRESS_TOKEN_ENV)
        .ok()
        .filter(|token| !token.trim().is_empty());
    if let Some(token) = &manual_token {
        crate::dbos_ingress::FinanceIngress::validate_token(token).map_err(anyhow::Error::msg)?;
    }
    let dbos_ingress = Arc::new(std::sync::RwLock::new(
        crate::dbos_ingress::WorkerIngress::default(),
    ));
    let dbos_executor = if let Some((
        settings,
        schedules,
        archives_context,
        reconcile_context,
        finance_aggregate_surreal,
        reconcile_surreal,
        dbos_ingress_token,
        finance_claim_postgres,
        nightly_project_update_enabled,
        email_summary_enabled,
        inbound_message_alert_enabled,
        event_surreal,
        project_slack_enabled,
        project_slack_postgres,
        notation_enabled,
        social_surreal,
    )) = dbos_hosting
    {
        let finance_aggregate_surreal_for_ingress = finance_aggregate_surreal.clone();
        let finance_aggregate_context = finance_aggregate_surreal.map(|surreal| {
            crate::dbos_finance_aggregate::Context::new(surreal, dbos_finance_storage)
        });
        let email_summary_context = if email_summary_enabled {
            let surreal = event_surreal
                .clone()
                .ok_or_else(|| anyhow::anyhow!("EmailSummary database not configured"))?;
            Some(crate::dbos_email_summary::Context::new(
                surreal,
                Arc::clone(&dbos_email_storage),
                Arc::clone(&dbos_email_slack),
                summary_providers.clone(),
            ))
        } else {
            None
        };
        let alert_context = if inbound_message_alert_enabled {
            let surreal = event_surreal
                .ok_or_else(|| anyhow::anyhow!("InboundMessageAlert database not configured"))?;
            Some(crate::dbos_inbound_message_alert::Context::new(
                InboundMessageAlertService::new(
                    surreal,
                    Arc::clone(&dbos_alert_slack),
                    dbos_alert_messages_channel,
                    dbos_alert_staging_messages_channel,
                    simulated_matters,
                    slack_bot_enabled,
                    dbos_alert_portal_origin,
                ),
                postgres.clone(),
            ))
        } else {
            None
        };
        let dbos_notation_postgres = finance_claim_postgres.clone();
        // Both stores, resolved before the executor launch so a misconfigured
        // notation flag fails boot with a clear message rather than inside
        // the registration closure (whose error type is the engine's).
        let dbos_notation_stores = if notation_enabled {
            Some((
                dbos_notation_surreal.clone(),
                dbos_notation_postgres
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("notation port requires PostgreSQL"))?,
            ))
        } else {
            None
        };
        let recovery_version = settings.config().app_version.ok_or_else(|| {
            anyhow::anyhow!("summary recovery requires the configured DBOS version")
        })?;
        let review_postgres = postgres.clone();
        let dbos_ingress_for_registration = Arc::clone(&dbos_ingress);
        Some(
            dbos_engine::Executor::launch_hosting_with(
                &settings,
                ops_notifier,
                match app_schedules {
                    Some(application) => schedules.with_application_schedules(application),
                    None => schedules,
                },
                move |dbos| {
                    let mut launchers = register_external(dbos)?;
                    let mut registered_ingress = crate::dbos_ingress::WorkerIngress {
                        manual_schedules: manual_token.map(|token| {
                            crate::dbos_ingress::ManualIngress {
                                token: token.into(),
                                builtin: dbos_engine::ManualSchedules::default(),
                                archives: None,
                            }
                        }),
                        ..Default::default()
                    };
                    if nightly_project_update_enabled {
                        let outcome_channel = if simulated_matters {
                            dbos_nightly_staging_messages_channel.clone()
                        } else {
                            dbos_nightly_messages_channel.clone()
                        };
                        let token = dbos_ingress_token
                            .clone()
                            .expect("enabled DBOS ingress has a validated token");
                        let workflow = crate::dbos_nightly_project_update::register(
                            dbos,
                            crate::dbos_nightly_project_update::Context::new(
                                dbos_nightly_surreal,
                                dbos_nightly_slack,
                                dbos_nightly_bots,
                                outcome_channel,
                                simulated_matters,
                                crate::dbos_nightly_project_update::Providers {
                                    repository_forge: dbos_nightly_repository_forge,
                                    runtime_version:
                                        crate::nightly_project_update::repository::RUNTIME_VERSION
                                            .into(),
                                    portal_origin: dbos_nightly_portal_origin,
                                },
                            ),
                        )?;
                        registered_ingress.nightly_project_update =
                            Some(crate::dbos_ingress::NightlyIngress::new(token, workflow));
                    }
                    if let Some((slack, channel_id, slack_enabled)) = archives_context {
                        let workflow = archives::workflow::register_dbos(
                            dbos,
                            slack,
                            channel_id,
                            slack_enabled,
                        )?;
                        if let Some(manual) = &mut registered_ingress.manual_schedules {
                            manual.archives = Some(workflow.clone());
                        }
                        launchers.push(Box::new(move |stop| {
                            tokio::spawn(archives::workflow::schedule_dbos(workflow, stop))
                        }));
                    }
                    if let Some(context) = reconcile_context {
                        let schedule_enabled = context.schedule_enabled();
                        let workflow = crate::dbos_reconcile::register(dbos, context)?;
                        if let (Some(token), Some(_surreal)) =
                            (dbos_ingress_token.clone(), reconcile_surreal)
                        {
                            registered_ingress.reconcile = Some(
                                crate::dbos_ingress::ReconcileIngress::new(token, workflow.clone()),
                            );
                        }
                        launchers.extend(crate::dbos_reconcile::schedule_starter(
                            workflow,
                            schedule_enabled,
                        ));
                    }
                    if let Some(context) = email_summary_context {
                        let postgres = finance_claim_postgres.clone().ok_or_else(|| {
                            dbos::Error::Config("summary recovery requires PostgreSQL".into())
                        })?;
                        let recovery = crate::dbos_email_summary::recovery::register(
                            dbos,
                            postgres.clone(),
                            recovery_version.clone(),
                        )?;
                        if let Some(token) = dbos_ingress_token.as_ref() {
                            registered_ingress.email_summary_recovery =
                                Some(crate::dbos_email_summary::recovery::RecoveryIngress::new(
                                    token.clone(),
                                    dbos.clone(),
                                    postgres,
                                    recovery,
                                    recovery_version.clone(),
                                ));
                        }
                        let workflow = crate::dbos_email_summary::register(dbos, context)?;
                        if let Some(token) = dbos_ingress_token.as_ref() {
                            registered_ingress.email_summary =
                                Some(crate::dbos_ingress::EmailSummaryIngress::new(
                                    token.clone(),
                                    workflow,
                                ));
                        }
                    }
                    if let Some(context) = alert_context {
                        let workflow = crate::dbos_inbound_message_alert::register(dbos, context)?;
                        if let Some(token) = dbos_ingress_token.as_ref() {
                            registered_ingress.inbound_message_alert = Some(
                                crate::dbos_ingress::AlertIngress::new(token.clone(), workflow),
                            );
                        }
                    }
                    if project_slack_enabled {
                        // Registered before Finance consumes the shared
                        // startup values, so the lane is live ahead of any
                        // sibling workflow that may want it later.
                        let workflow = crate::dbos_project_slack::register(
                            dbos,
                            crate::dbos_project_slack::Context::new(
                                dbos_project_slack_surreal.clone(),
                                dbos_project_slack_bots.clone(),
                                project_slack_postgres
                                    .clone()
                                    .expect("ProjectSlack lane has no PostgreSQL connection"),
                                simulated_matters,
                            ),
                        )?;
                        if let (Some(token), Some(postgres)) =
                            (dbos_ingress_token.clone(), project_slack_postgres.clone())
                        {
                            registered_ingress.project_slack =
                                Some(crate::dbos_ingress::ProjectSlackIngress::new(
                                    token,
                                    workflow,
                                    dbos.clone(),
                                    dbos_project_slack_surreal.clone(),
                                    postgres,
                                ));
                        }
                    }
                    if let Some(context) = finance_aggregate_context {
                        let workflow = crate::dbos_finance_aggregate::register(dbos, context)?;
                        if let (Some(token), Some(surreal), Some(postgres)) = (
                            dbos_ingress_token.clone(),
                            finance_aggregate_surreal_for_ingress,
                            finance_claim_postgres,
                        ) {
                            let ingress = crate::dbos_ingress::FinanceIngress::new(
                                token,
                                workflow,
                                dbos.clone(),
                                surreal,
                                postgres,
                            );
                            registered_ingress.finance = Some(ingress);
                        }
                    }
                    if let Some((surreal, postgres)) = dbos_notation_stores.clone() {
                        let context = crate::dbos_notation::Context::new(
                            surreal,
                            postgres.clone(),
                            Arc::clone(&dbos_notation_email),
                            Arc::clone(&dbos_notation_storage),
                        );
                        let workflow = crate::dbos_notation::register(dbos, context)?;
                        if let Some(token) = dbos_ingress_token.clone() {
                            registered_ingress.notation =
                                Some(crate::dbos_ingress::NotationIngress::new(
                                    token,
                                    dbos.clone(),
                                    postgres,
                                    workflow,
                                ));
                        }
                    }
                    if let Some((social_surreal, social_reconciler_enabled)) = social_surreal {
                        let social_publisher = SocialPublisherService::new(
                            social_surreal.clone(),
                            dbos_social_kms.clone(),
                            dbos_social_assets_storage.clone(),
                        )
                        .publisher();
                        let social_workflow = crate::dbos_social_publisher::register_publisher(
                            dbos,
                            crate::dbos_social_publisher::PublisherContext::new(
                                social_publisher.clone(),
                            ),
                        )?;
                        if let Some(token) = dbos_ingress_token.clone() {
                            registered_ingress.social_publisher =
                                Some(crate::dbos_ingress::SocialPublisherIngress::new(
                                    token,
                                    social_workflow.clone(),
                                ));
                        }
                        if social_reconciler_enabled {
                            let reconciler = crate::dbos_social_publisher::register_reconciler(
                                dbos,
                                crate::dbos_social_publisher::ReconcilerContext::from_lookup(
                                    social_surreal,
                                    social_publisher,
                                    social_workflow.clone(),
                                    lookup,
                                )
                                .map_err(|error| dbos::Error::Config(error.to_string()))?,
                            )?;
                            if let Some(token) = dbos_ingress_token.clone() {
                                registered_ingress.social_reconciler =
                                    Some(crate::dbos_ingress::SocialReconcilerIngress::new(
                                        token,
                                        reconciler.clone(),
                                    ));
                            }
                            launchers.push(Box::new(move |stop| {
                                let clock: crate::dbos_social_publisher::Clock =
                                    Arc::new(chrono::Utc::now);
                                tokio::spawn(async move {
                                    crate::dbos_social_publisher::schedule(
                                        &reconciler,
                                        clock,
                                        stop,
                                    )
                                    .await;
                                })
                            }));
                        }
                    }
                    if let (Some(service), Some(token)) =
                        (youtube_dbos.clone(), dbos_ingress_token.as_ref())
                    {
                        registered_ingress.youtube_comments =
                            Some(crate::dbos_ingress::YoutubeIngress {
                                token: token.as_str().into(),
                                workflow: crate::youtube_comments::register_dbos(dbos, service)?,
                            });
                    }
                    if let (Some(service), Some(token)) =
                        (triage_dbos.clone(), dbos_ingress_token.as_ref())
                    {
                        registered_ingress.inbound_triage =
                            Some(crate::dbos_ingress::TriageIngress {
                                token: token.as_str().into(),
                                workflow: crate::inbound_triage::register_dbos(
                                    dbos,
                                    service.clone(),
                                )?,
                                filing: crate::inbound_triage::register_filing_dbos(dbos, service)?,
                            });
                    }
                    let review =
                        crate::dbos_deadline_proposal::register(dbos, review_postgres.clone())?;
                    let review_engine = dbos.clone();
                    launchers.push(Box::new(move |stop| {
                        tokio::spawn(crate::dbos_deadline_proposal::schedule(
                            review_engine,
                            review_postgres,
                            review,
                            stop,
                        ))
                    }));
                    if let Some((db, storage, scanner)) = inbox_attachment_dbos {
                        let workflow = crate::inbox_attachment_filing::register_dbos(
                            dbos, db, storage, scanner,
                        )?;
                        launchers.push(Box::new(move |stop| {
                            tokio::spawn(crate::inbox_attachment_filing::schedule(workflow, stop))
                        }));
                    }
                    if let (Some((postgres, surreal)), Some(token)) = (
                        sandbox_credit_reset_stores.clone(),
                        dbos_ingress_token.as_ref(),
                    ) {
                        registered_ingress.sandbox_credit_reset =
                            Some(crate::dbos_ingress::SandboxResetIngress {
                                token: token.as_str().into(),
                                workflow: crate::dbos_sandbox_reset::register(
                                    dbos, postgres, surreal,
                                )?,
                            });
                    }
                    *dbos_ingress_for_registration
                        .write()
                        .expect("DBOS ingress lock poisoned") = registered_ingress;
                    Ok(launchers)
                },
            )
            .await
            .context("launch the DBOS worker")?,
        )
    } else {
        None
    };
    if let Some(executor) = &dbos_executor {
        if let Some(manual) = &mut dbos_ingress
            .write()
            .map_err(|_| anyhow::anyhow!("workflow ingress lock poisoned"))?
            .manual_schedules
        {
            manual.builtin = executor.manual_schedules();
        }
    }
    let dbos_probe = dbos_executor.as_ref().map(dbos_engine::Executor::probe);
    let stop = dbos::CancellationToken::new();
    let mut health = serve_health_listener(health_listener, dbos_ingress, dbos_probe, stop.clone());
    if let Some(ready) = ready {
        let _ = ready.send(());
    }
    let mut health_finished = false;
    let result = tokio::select! {
        result = shutdown => result,
        result = &mut health => {
            health_finished = true;
            match result {
                Ok(Ok(())) => Err(anyhow::anyhow!("workflow health listener stopped")),
                Ok(Err(error)) => Err(error.into()),
                Err(error) => Err(error.into()),
            }
        },
    };
    stop.cancel();
    let health_result = if health_finished {
        Ok(())
    } else {
        match health.await {
            Ok(result) => result.map_err(anyhow::Error::from),
            Err(error) => Err(error.into()),
        }
    };
    if let Some(executor) = dbos_executor {
        executor.shutdown().await;
    }
    result?;
    health_result
}

/// Bind the DBOS ingress and its liveness and readiness probes.
async fn bind_health_listener() -> anyhow::Result<tokio::net::TcpListener> {
    let health_addr = crate::health::health_listen_addr(|key| std::env::var(key).ok())?;
    let health_listener = tokio::net::TcpListener::bind(health_addr)
        .await
        .with_context(|| format!("bind health probe listener on {health_addr}"))?;
    Ok(health_listener)
}

fn serve_health_listener(
    health_listener: tokio::net::TcpListener,
    dbos_ingress: Arc<std::sync::RwLock<crate::dbos_ingress::WorkerIngress>>,
    dbos_probe: Option<dbos_engine::ExecutorProbe>,
    stop: dbos::CancellationToken,
) -> tokio::task::JoinHandle<std::io::Result<()>> {
    let health_addr = health_listener.local_addr().ok();
    tracing::info!(?health_addr, "health probe listener listening");
    tokio::spawn(async move {
        let ingress = dbos_ingress
            .read()
            .expect("DBOS ingress lock poisoned")
            .clone();
        let dbos = dbos_probe.map(|probe| {
            let system_database_url = probe.system_database_url().map(Arc::from);
            crate::health::DbosReadiness {
                is_running: Arc::new(move || probe.is_running()),
                system_database_url,
            }
        });
        axum::serve(
            health_listener,
            crate::health::router_with_dbos_ingress(ingress, dbos),
        )
        .with_graceful_shutdown(stop.cancelled_owned())
        .await
    })
}

#[cfg(test)]
mod native_host_tests {
    use super::*;
    #[test]
    fn native_host_refuses_missing_or_invalid_routes_before_launch() {
        assert!(require_native_routes(Engine::Dbos, |_| Some("true".into())).is_ok());
        assert!(require_native_routes(Engine::Dbos, |_| None).is_err());
        assert!(require_native_routes(Engine::Dbos, |_| Some("bad".into())).is_err());
        assert!(require_native_routes(Engine::Restate, |_| Some("true".into())).is_err());
    }
}
