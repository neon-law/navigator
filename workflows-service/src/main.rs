//! Worker entry point. Opens the shared store connection,
//! builds the worker's `EmailService` (bare `SendGrid` in prod,
//! `CapturingEmail` otherwise), wires the `Notation` virtual-object
//! endpoint, and listens on the port the Restate broker discovers
//! via `restate-cli register`.
//!
//! `NAVIGATOR_WORKFLOW_ENGINE=dbos` runs the DBOS executor beside the Restate
//! endpoint (`workflows::dbos_engine`); unset, only the Restate endpoint runs.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use archives::workflow::ArchivesService;
use billing_workflows::canary::BillingCanaryService;
use billing_workflows::digest::BillingDigestService;
use billing_workflows::reconcile::ReconcileInvoicesService;
use restate_sdk::prelude::*;
use workflows::dbos_engine;
use workflows::{EmailService, SlackOpsDelivery};
use workflows_service::dri_digest::DriDigestService;
use workflows_service::email_summary::{EmailSummaryService, SummaryProviders};
use workflows_service::engine::Engine;
use workflows_service::finance_aggregate::FinanceAggregateService;
use workflows_service::general_nag::GeneralNagService;
use workflows_service::inbound_message_alert::InboundMessageAlertService;
use workflows_service::nightly_project_update::{
    membership::MembershipTagService,
    repository::{EnvFirmForges, RepositoryForge, RepositoryTagService},
    slack::SlackTagService,
    NightlyProjectUpdateService,
};
use workflows_service::notation_reset::NotationResetService;
use workflows_service::request_identity::{apply_identity_key, install_crypto_provider};
use workflows_service::social_publisher::{SocialPublishReconcilerService, SocialPublisherService};
use workflows_service::{
    email_from_env, notifier_from_env, project_slack::ProjectSlackService, slack_bot_from_env,
    NotationService,
};

macro_rules! bind_common_services {
    ($endpoint:expr, $surreal:expr, $email:expr, $storage:expr, $assets_storage:expr, $kms:expr, $email_summary:expr, $inbound_triage:expr, $notifier:expr, $ops_delivery:expr, $slack_bot:expr, $general_channel:expr, $finance_channel:expr, $messages_channel:expr, $staging_messages_channel:expr, $slack_bot_enabled:expr, $simulated_matters:expr, $forge:expr, $repository_forge:expr, $portal_origin:expr, $environment_label:expr) => {
        $endpoint
            .bind(NotationService::new(
                $surreal.clone(),
                $email.clone(),
                $storage.clone(),
            ))
            .bind(ProjectSlackService::new(
                $surreal.clone(),
                $slack_bot.clone(),
                $simulated_matters,
            ))
            .bind(SocialPublisherService::new(
                $surreal.clone(),
                $kms.clone(),
                $assets_storage.clone(),
            ))
            .bind(SocialPublishReconcilerService::new(
                $surreal.clone(),
                $kms,
                $assets_storage,
            ))
            .bind($email_summary)
            .bind(InboundMessageAlertService::new(
                $surreal.clone(),
                $slack_bot.clone(),
                $messages_channel.clone(),
                $staging_messages_channel.clone(),
                $simulated_matters,
                $slack_bot_enabled,
                $portal_origin.clone(),
            ))
            .bind(ArchivesService::new(
                $slack_bot.clone(),
                if $simulated_matters {
                    $staging_messages_channel.clone()
                } else {
                    $messages_channel.clone()
                },
                $slack_bot_enabled,
            ))
            .bind(NotationResetService::new(
                $surreal.clone(),
                $slack_bot.clone(),
                $messages_channel.clone(),
                $staging_messages_channel.clone(),
                $environment_label,
                $simulated_matters,
                $slack_bot_enabled,
            ))
            .bind(NightlyProjectUpdateService::new(
                $surreal.clone(),
                $slack_bot.clone(),
                $messages_channel,
                $staging_messages_channel,
                $simulated_matters,
                $slack_bot_enabled,
            ))
            .bind(MembershipTagService::new(
                $surreal.clone(),
                $slack_bot.clone(),
                $simulated_matters,
            ))
            .bind(SlackTagService::new(
                $surreal.clone(),
                $slack_bot.clone(),
                $portal_origin,
                $forge.clone(),
            ))
            .bind(RepositoryTagService::new(
                $surreal.clone(),
                $repository_forge,
                workflows_service::nightly_project_update::repository::RUNTIME_VERSION,
            ))
            .bind(BillingCanaryService::new($ops_delivery.clone()))
            .bind(BillingDigestService::new($ops_delivery))
            .bind(ReconcileInvoicesService::new(
                $surreal.clone(),
                $slack_bot.clone(),
                $finance_channel,
                $simulated_matters,
            ))
            .bind(DriDigestService::new(
                $surreal.clone(),
                $notifier,
                $simulated_matters,
            ))
            .bind(FinanceAggregateService::new($surreal.clone(), $storage))
            .bind(GeneralNagService::new(
                $surreal,
                $slack_bot,
                $general_channel,
                $simulated_matters,
            ))
    };
}

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

#[tokio::main]
#[allow(clippy::too_many_lines)] // Startup wiring for every bound service stays in one place.
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    // `.devx/env` overlay for local KIND iteration (port-forward URLs,
    // dev OAuth secrets). Loaded second so `.env` always wins.
    let _ = dotenvy::from_path(".devx/env");
    // One observability seam for every binary: stdout logs (JSON when an
    // OTLP endpoint is set) plus OTLP traces + metrics. Held to end of main
    // so the drop flushes any batched export before the process exits.
    let _telemetry = telemetry::init("navigator-workflows-service");
    // Zero-start the nightly run series so a missed run is visible after a
    // restart (the Dash0 missed-run check rule reads `increase()`).
    telemetry::register_nightly_project_update();

    let environment =
        store::DeploymentEnvironment::from_env().context("parse NAVIGATOR_ENVIRONMENT")?;
    // Drives `DriDigest` and `GeneralNag` staging disclosure and the
    // production-only open-matters follow-up — the same flag the site-wide
    // "simulated projects" banner reads.
    let simulated_matters =
        store::sample_matters(environment).context("resolve NAVIGATOR_SIMULATED_MATTERS")?;
    // Makes `jsonwebtoken`'s process-level `CryptoProvider` deterministic
    // before any request-identity signature is verified (ENG-550): Cargo
    // feature unification otherwise leaves both `rust_crypto` and
    // `aws_lc_rs` enabled on the shared `jsonwebtoken` instance, which
    // panics the first real Restate-signed request.
    install_crypto_provider();
    let lookup = |key: &str| std::env::var(key).ok();
    let dbos_hosting = if Engine::from_lookup(lookup).context("select the durable engine")?
        == Engine::Dbos
    {
        let settings = dbos_engine::Settings::from_lookup(lookup).context("configure DBOS")?;
        let general_nag_enabled = workflows::dbos_engine::general_nag::enabled(lookup)
            .context("read NAVIGATOR_DBOS_GENERAL_NAG_ENABLED")?;
        let dri_digest_enabled = workflows::dbos_engine::dri_digest::enabled(lookup)
            .context("read NAVIGATOR_DBOS_DRI_DIGEST_ENABLED")?;
        let notation_reset_enabled = workflows::dbos_engine::notation_reset::enabled(lookup)
            .context("read NAVIGATOR_DBOS_NOTATION_RESET_ENABLED")?;
        let archives_enabled = archives::workflow::dbos_enabled(lookup)
            .map_err(anyhow::Error::msg)
            .context("read NAVIGATOR_DBOS_ARCHIVES_ENABLED")?;
        let reconcile_enabled = workflows_service::dbos_reconcile::enabled(lookup)
            .map_err(anyhow::Error::msg)
            .context("read NAVIGATOR_DBOS_RECONCILE_INVOICES_ENABLED")?;
        let finance_aggregate_enabled = workflows_service::dbos_finance_aggregate::enabled(lookup)
            .map_err(anyhow::Error::msg)
            .context("read NAVIGATOR_DBOS_FINANCE_AGGREGATE_ENABLED")?;
        let nightly_project_update_enabled =
            workflows_service::dbos_nightly_project_update::enabled(lookup)
                .map_err(anyhow::Error::msg)
                .context("read NAVIGATOR_DBOS_NIGHTLY_PROJECT_UPDATE_ENABLED")?;
        let finance_claim_postgres = if finance_aggregate_enabled {
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
        {
            let token = lookup("NAVIGATOR_DBOS_INGRESS_TOKEN")
                .filter(|token| !token.trim().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "NAVIGATOR_DBOS_INGRESS_TOKEN is required when an authenticated DBOS ingress is enabled"
                    )
                })?;
            workflows_service::dbos_ingress::FinanceIngress::validate_token(&token)
                .map_err(anyhow::Error::msg)?;
            Some(token)
        } else {
            None
        };
        let surreal = if general_nag_enabled
            || dri_digest_enabled
            || notation_reset_enabled
            || reconcile_enabled
            || finance_aggregate_enabled
            || nightly_project_update_enabled
        {
            let surreal = store::surreal::connect_from_env()
                .await
                .context("connect to SurrealDB for DBOS daily workflows")?;
            store::schema::apply(&surreal)
                .await
                .context("apply SurrealDB schema for DBOS daily workflows")?;
            Some(surreal)
        } else {
            None
        };
        let general_nag = if general_nag_enabled {
            Some(
                workflows::dbos_engine::general_nag::Context::from_lookup(
                    surreal
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("GeneralNag database not configured"))?,
                    simulated_matters,
                    lookup,
                )
                .context("validate GeneralNag delivery configuration")?,
            )
        } else {
            None
        };
        let notation_reset = if notation_reset_enabled {
            Some(
                workflows::dbos_engine::notation_reset::Context::from_lookup(
                    surreal
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("NotationReset database not configured"))?,
                    workflows::notation_reset::environment_label(environment, simulated_matters),
                    simulated_matters,
                    lookup,
                )
                .context("validate NotationReset delivery configuration")?,
            )
        } else {
            None
        };
        let reconcile_context = if reconcile_enabled {
            Some(
                workflows_service::dbos_reconcile::Context::from_env(
                    surreal.clone().ok_or_else(|| {
                        anyhow::anyhow!("ReconcileInvoices database not configured")
                    })?,
                    slack_bot_from_env().map_err(anyhow::Error::msg)?,
                    simulated_matters,
                    lookup,
                )
                .map_err(anyhow::Error::msg)
                .context("configure DBOS ReconcileInvoices")?,
            )
        } else {
            None
        };
        let dri_digest = workflows_service::dri_digest::dbos_context(
            surreal.clone(),
            notifier_from_env(),
            simulated_matters,
            lookup,
        )
        .context("configure DBOS DriDigest")?;
        let billing_canary = if workflows::dbos_engine::billing_canary::enabled(lookup)
            .context("read NAVIGATOR_DBOS_BILLING_CANARY_ENABLED")?
        {
            Some(
                workflows::dbos_engine::billing_canary::Context::from_lookup(lookup)
                    .context("configure DBOS BillingCanary")?,
            )
        } else {
            None
        };
        let billing_digest = if workflows::dbos_engine::billing_digest::enabled(lookup)
            .context("read NAVIGATOR_DBOS_BILLING_DIGEST_ENABLED")?
        {
            Some(
                workflows::dbos_engine::billing_digest::Context::from_lookup(lookup)
                    .context("configure DBOS BillingDigest")?,
            )
        } else {
            None
        };
        let archives_context = if archives_enabled {
            let slack = slack_bot_from_env().map_err(anyhow::Error::msg)?;
            let channel_id = std::env::var(if simulated_matters {
                "SLACK_STAGING_MESSAGES_CHANNEL_ID"
            } else {
                "SLACK_MESSAGES_CHANNEL_ID"
            })
            .unwrap_or_default();
            let slack_enabled = workflows_service::notify_config::bot_enabled(lookup);
            anyhow::ensure!(
                slack_enabled && !channel_id.trim().is_empty(),
                "DBOS Archives requires SLACK_BOT_TOKEN and its messages channel"
            );
            Some((slack, channel_id, slack_enabled))
        } else {
            None
        };
        Some((
            settings,
            dbos_engine::Schedules {
                general_nag,
                dri_digest,
                billing_canary,
                billing_digest,
                notation_reset,
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
        ))
    } else {
        None
    };
    let endpoint_builder = apply_identity_key(Endpoint::builder(), environment, |key| {
        std::env::var(key).ok()
    })
    .map_err(anyhow::Error::msg)
    .context("configure Restate request identity")?;
    // Branding is Neon Law by default; NAVIGATOR_CUSTOM_BRANDING opts into a
    // mounted white-label bundle (fail closed when set but absent).
    let brand_bundle = views::brand_bundle::BrandBundle::from_env()
        .context("resolve brand bundle from NAVIGATOR_CUSTOM_BRANDING")?;
    let branding = brand_bundle
        .as_ref()
        .map_or(&views::brand::DEFAULT_BRANDING, |bundle| {
            views::brand::Branding::from_manifest(&bundle.manifest)
        });
    views::brand::install_process_branding(branding)
        .map_err(anyhow::Error::msg)
        .context("install process branding")?;
    tracing::info!(environment = environment.as_str(), "configured deployment");
    workflows_service::email_config::validate_for_deployment(environment, |key| {
        std::env::var(key).ok()
    })
    .context("validate deployment email configuration")?;
    // The email `@font-face` block renders here, not in `web`, so the asset
    // origin must be validated in this binary too — a malformed value would
    // otherwise reach the outbound-mail `<style>` block un-checked.
    workflows_service::asset_config::validate_for_deployment(|key| std::env::var(key).ok())
        .context("validate deployment asset base URL")?;

    let surreal = store::surreal::connect_from_env()
        .await
        .context("connect to SurrealDB")?;

    let email = email_from_env().context("build email service from env")?;
    tracing::info!(
        backend = if std::env::var("NAVIGATOR_EMAIL_BACKEND").as_deref() == Ok("sendgrid") {
            "SendGrid"
        } else {
            "Capturing"
        },
        "workflows-service email backend"
    );

    // Internal ops notifications deliver to the engineering channel through a
    // Slack incoming webhook. `BillingCanary` and `BillingDigest` route plain
    // text through `SlackOpsDelivery`, which fences the message in a code block. `Archives` posts mrkdwn directly to the
    // notifier so links and compact status formatting render correctly.
    // The client-facing Notation service uses the email backend, keeping
    // client content inside the client delivery channel.
    let notifier = notifier_from_env();
    let slack_bot = slack_bot_from_env()
        .map_err(anyhow::Error::msg)
        .context("configure Slack bot")?;
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
    let general_channel = std::env::var("SLACK_GENERAL_CHANNEL_ID").unwrap_or_default();
    // The `#finance` destination for the nightly Xero and IOLTA report. It
    // uses the same configured Slack App as `#general`; the empty-value rule
    // is the same too, so a run with nowhere to report is a terminal error.
    let finance_channel = std::env::var("SLACK_FINANCE_CHANNEL_ID").unwrap_or_default();
    let messages_channel = std::env::var("SLACK_MESSAGES_CHANNEL_ID").unwrap_or_default();
    let staging_messages_channel =
        std::env::var("SLACK_STAGING_MESSAGES_CHANNEL_ID").unwrap_or_default();
    let slack_bot_enabled =
        workflows_service::notify_config::bot_enabled(|k| std::env::var(k).ok());
    // How `NotationReset` names this deployment in its daily post.
    let environment_label =
        workflows_service::notation_reset::environment_label(environment, simulated_matters);
    // The GitHub forge for the daily Project sync; `None` when the deployment
    // configures no GitHub provider (KIND), a partial configuration is fatal.
    let forge: Option<Arc<dyn cloud::forge::ForgeService>> =
        store::project_surfaces::forge_from_env()
            .context("configure GitHub forge")?
            .map(|forge| Arc::new(forge) as Arc<dyn cloud::forge::ForgeService>);
    tracing::info!(
        backend = if workflows_service::notify_config::slack_enabled(|k| std::env::var(k).ok()) {
            "Slack"
        } else {
            "Capturing"
        },
        "workflows-service ops-notification backend"
    );
    let ops_delivery: Arc<dyn EmailService> = Arc::new(SlackOpsDelivery::new(notifier.clone()));

    // Object storage for `generate_pdf__*` step dispatch (the worker
    // renders the PDF and persists it here). Same `cloud::from_env`
    // backend selection as `web`: GCS in prod, FsStorage in dev.
    let storage = cloud::from_env()
        .await
        .context("configure object storage")?;
    let dbos_finance_storage = storage.clone();
    let assets_storage = cloud::assets_from_env()
        .await
        .context("configure public assets storage")?;
    let kms = social_runtime_kms_from_env().await;
    // The nightly `repository` tag: on Forgejo each Project's own Firm
    // client, which also governs the repository and proposes the version
    // update; otherwise the deployment's GitHub forge, detection only.
    let repository_forge = match cloud::ForgeBackend::from_lookup(|key| std::env::var(key).ok())
        .context("select the Project forge")?
    {
        cloud::ForgeBackend::Forgejo => {
            RepositoryForge::Forgejo(Arc::new(EnvFirmForges::new(kms.clone())))
        }
        cloud::ForgeBackend::GitHub => forge
            .clone()
            .map_or(RepositoryForge::None, RepositoryForge::GitHub),
    };
    let dbos_nightly_surreal = surreal.clone();
    let dbos_nightly_slack = slack_bot.clone();
    let dbos_nightly_repository_forge = repository_forge.clone();
    let dbos_nightly_github_forge = forge.clone();
    let dbos_nightly_portal_origin = portal_origin.clone();
    let dbos_nightly_messages_channel = messages_channel.clone();
    let dbos_nightly_staging_messages_channel = staging_messages_channel.clone();
    if dbos_hosting.as_ref().is_some_and(|hosting| hosting.8) {
        anyhow::ensure!(
            slack_bot_enabled,
            "DBOS NightlyProjectUpdate requires SLACK_BOT_TOKEN"
        );
        let channel = if simulated_matters {
            &dbos_nightly_staging_messages_channel
        } else {
            &dbos_nightly_messages_channel
        };
        anyhow::ensure!(
            !channel.trim().is_empty(),
            "DBOS NightlyProjectUpdate requires its messages channel"
        );
    }
    let summary_providers = SummaryProviders::from_env().context("configure summary providers")?;
    let email_summary = EmailSummaryService::new(
        surreal.clone(),
        storage.clone(),
        slack_bot.clone(),
        summary_providers,
    );
    // Rules-only when `NAVIGATOR_TRIAGE_ENABLED` is unset: the service stays
    // registered and every message still gets a triage row, from rules alone.
    let triage_providers = TriageProviders::from_env().context("configure triage providers")?;
    let inbound_triage = InboundTriageService::new(surreal.clone(), triage_providers);

    let listen: SocketAddr = std::env::var("WORKFLOWS_SERVICE_LISTEN")
        .unwrap_or_else(|_| "0.0.0.0:9080".into())
        .parse()
        .context("parse WORKFLOWS_SERVICE_LISTEN")?;

    tracing::info!(%listen, "workflows-service listening");

    // One endpoint hosts every workflow: the `Notation` virtual object,
    // `EmailSummary`, and the `Archives` nightly-export, `BillingCanary`,
    // `BillingDigest`
    // (daily GCP cost email), `ReconcileInvoices`, `DriDigest` (nightly
    // project-DRI notice), `GeneralNag` (daily `#general` jab), and
    // `NightlyProjectUpdate` (daily parent DAG over every Project, with one
    // child workflow per tag), and `NotationReset` (daily draft reset)
    // workflows, each with a thin CronJob trigger. All run against this one
    // worker — there is no per-workflow pod. The exact set of service names
    // bound here is mirrored in `workflows_service::registry`, which the
    // registry tests guard against drift (count + PascalCase naming).
    let server = HttpServer::new(
        bind_common_services!(
            endpoint_builder,
            surreal,
            email,
            storage,
            assets_storage,
            kms,
            email_summary,
            notifier,
            ops_delivery,
            slack_bot,
            general_channel,
            finance_channel,
            messages_channel,
            staging_messages_channel,
            slack_bot_enabled,
            simulated_matters,
            forge,
            repository_forge,
            portal_origin,
            environment_label
        )
        .build(),
    )
    .listen_and_serve(listen);

    let health_listener = bind_health_listener().await?;
    let dbos_ingress = Arc::new(std::sync::RwLock::new(
        workflows_service::dbos_ingress::WorkerIngress::default(),
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
    )) = dbos_hosting
    {
        let finance_aggregate_surreal_for_ingress = finance_aggregate_surreal.clone();
        let finance_aggregate_context = finance_aggregate_surreal.map(|surreal| {
            workflows_service::dbos_finance_aggregate::Context::new(surreal, dbos_finance_storage)
        });
        let dbos_ingress_for_registration = Arc::clone(&dbos_ingress);
        Some(
            dbos_engine::Executor::launch_hosting_with(
                &settings,
                notifier_from_env(),
                schedules,
                        move |dbos| {
                            let mut launchers: Vec<dbos_engine::ScheduleStarter> = Vec::new();
                            let mut registered_ingress =
                                workflows_service::dbos_ingress::WorkerIngress::default();
                            if nightly_project_update_enabled {
                                let outcome_channel = if simulated_matters {
                                    dbos_nightly_staging_messages_channel.clone()
                                } else {
                                    dbos_nightly_messages_channel.clone()
                                };
                                let token = dbos_ingress_token
                                    .clone()
                                    .expect("enabled DBOS ingress has a validated token");
                                let workflow = workflows_service::dbos_nightly_project_update::register(
                                    dbos,
                                    workflows_service::dbos_nightly_project_update::Context::new(
                                        dbos_nightly_surreal,
                                        dbos_nightly_slack,
                                        outcome_channel,
                                        simulated_matters,
                                        workflows_service::dbos_nightly_project_update::Providers {
                                            github_forge: dbos_nightly_github_forge.clone(),
                                            repository_forge: dbos_nightly_repository_forge,
                                            runtime_version: workflows_service::nightly_project_update::repository::RUNTIME_VERSION.into(),
                                            portal_origin: dbos_nightly_portal_origin,
                                        },
                                    ),
                                )?;
                                registered_ingress.nightly_project_update = Some(
                                    workflows_service::dbos_ingress::NightlyIngress::new(
                                        token,
                                        workflow,
                                    ),
                                );
                            }
                    if let Some((slack, channel_id, slack_enabled)) = archives_context {
                        let workflow = archives::workflow::register_dbos(
                            dbos,
                            slack,
                            channel_id,
                            slack_enabled,
                        )?;
                        launchers.push(Box::new(move |stop| {
                            tokio::spawn(archives::workflow::schedule_dbos(workflow, stop))
                        }));
                    }
                    if let Some(context) = reconcile_context {
                        let workflow = workflows_service::dbos_reconcile::register(dbos, context)?;
                        if let (Some(token), Some(_surreal)) =
                            (dbos_ingress_token.clone(), reconcile_surreal)
                        {
                            registered_ingress.reconcile =
                                Some(workflows_service::dbos_ingress::ReconcileIngress::new(
                                    token,
                                    workflow.clone(),
                                ));
                        }
                        launchers.push(Box::new(move |stop| {
                            tokio::spawn(workflows_service::dbos_reconcile::schedule(
                                workflow, stop,
                            ))
                        }));
                    }
                    if let Some(context) = finance_aggregate_context {
                        let workflow =
                            workflows_service::dbos_finance_aggregate::register(dbos, context)?;
                        if let (Some(token), Some(surreal), Some(postgres)) = (
                            dbos_ingress_token,
                            finance_aggregate_surreal_for_ingress,
                            finance_claim_postgres,
                        ) {
                            let ingress = workflows_service::dbos_ingress::FinanceIngress::new(
                                token,
                                workflow,
                                dbos.clone(),
                                surreal,
                                postgres,
                            );
                            registered_ingress.finance = Some(ingress);
                        }
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
    serve_health_listener(health_listener, dbos_ingress);

    let server_result = tokio::select! {
        () = server => Ok(()),
        result = shutdown_signal() => result,
    };
    if let Some(executor) = dbos_executor {
        executor.shutdown().await;
    }
    server_result?;

    Ok(())
}

/// Unsigned liveness/readiness probe (ENG-551): unlike the Restate endpoint,
/// it needs no Restate Cloud signature, so kubelet and the GCE LB can reach it
/// directly on every deployment. Its one route round-trips a throwaway
/// signature through the same `jsonwebtoken` verification path real traffic
/// uses, catching an ENG-550-style `CryptoProvider` regression instead of
/// shipping it invisibly again. Both engines serve it.
async fn bind_health_listener() -> anyhow::Result<tokio::net::TcpListener> {
    let health_addr = workflows_service::health::health_listen_addr(|key| std::env::var(key).ok())?;
    let health_listener = tokio::net::TcpListener::bind(health_addr)
        .await
        .with_context(|| format!("bind health probe listener on {health_addr}"))?;
    Ok(health_listener)
}

fn serve_health_listener(
    health_listener: tokio::net::TcpListener,
    dbos_ingress: Arc<std::sync::RwLock<workflows_service::dbos_ingress::WorkerIngress>>,
) {
    let health_addr = health_listener.local_addr().ok();
    tracing::info!(?health_addr, "health probe listener listening");
    tokio::spawn(async move {
        let ingress = dbos_ingress
            .read()
            .expect("DBOS ingress lock poisoned")
            .clone();
        if let Err(error) = axum::serve(
            health_listener,
            workflows_service::health::router_with_dbos_ingress(ingress),
        )
        .await
        {
            tracing::error!(%error, "health probe listener stopped");
        }
    });
}

async fn shutdown_signal() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .context("install SIGTERM handler")?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.context("waiting for Ctrl-C"),
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.context("waiting for Ctrl-C")
    }
}
