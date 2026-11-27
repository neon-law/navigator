//! Shared host runtime assembly and run loop for Navigator's brand binaries.
//!
//! Every brand binary boots the same Navigator application: same database,
//! storage, providers, and `AppState`. What differs is the [`Site`] it
//! declares — its telemetry name and the public surface it publishes — so a
//! brand crate is a `Site` value and nothing else. [`build_from_env`]
//! assembles the runtime and [`run`] owns the boot: telemetry, that assembly,
//! composition through [`crate::bootstrap`], bind, serve, and drain.
//! The application hosts the DBOS executor beside the HTTP server.
//!
//! Both halves live here because they are one boot sequence. Splitting the
//! bind/serve tail into a separate crate would put a fourth dependency between
//! a brand crate and the application it mounts, and every brand main would
//! re-derive the ordering that `run` now states once.

use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::Router;

use workflows::dbos_engine;
use workflows::Notifier;

use crate::{AppConfig, AppState};

/// Re-exported for brand crates: composing a public surface means naming
/// `Router`, and the thin-crate rule keeps a brand from depending on `axum`
/// directly. A brand takes it from the application it mounts, like every other
/// type it needs.
pub use axum::Router as PublicRouter;

/// Re-exported so a brand crate composes its whole [`Site`] from `portal`
/// alone, without taking a `store` dependency for one enum.
pub use store::seed::BrandSeed;

/// The assembled shared host runtime.
///
/// Carries the configured [`AppState`], the resolved static-asset directory,
/// the deployment [`AppConfig`] (for the bind port), and the database handle
/// the binary closes at graceful shutdown.
pub struct HostRuntime {
    pub config: AppConfig,
    pub state: AppState,
    pub public_dir: PathBuf,
}

/// Build the shared Navigator host runtime from the process environment:
/// resolve config, validate PostgreSQL history and seed the database, configure object
/// storage and content, and assemble [`AppState`].
///
/// The caller owns telemetry init (a per-binary service name) and the
/// bind/serve loop; this function owns everything in between so brand
/// binaries cannot drift in how they assemble the application.
///
/// `brand_seed` names whose seeds this boot applies. It is [`Site::seed`] for
/// a real binary; a caller assembling a runtime directly picks the brand whose
/// data it means to carry.
#[allow(clippy::too_many_lines)]
pub async fn build_from_env(brand_seed: store::seed::BrandSeed) -> anyhow::Result<HostRuntime> {
    let cfg = crate::AppConfig::from_env().context("loading AppConfig")?;
    // Branding is Neon Law by default; NAVIGATOR_CUSTOM_BRANDING opts into a
    // mounted white-label bundle (fail closed when set but absent).
    let brand_bundle = views::brand_bundle::BrandBundle::from_env()
        .context("resolving brand bundle from NAVIGATOR_CUSTOM_BRANDING")?;
    let branding = brand_bundle
        .as_ref()
        .map_or(&views::brand::DEFAULT_BRANDING, |bundle| {
            views::brand::Branding::from_manifest(&bundle.manifest)
        });
    views::brand::install_process_branding(branding)
        .map_err(anyhow::Error::msg)
        .context("installing process branding")?;
    let voice_numbers = std::env::var(views::brand::NAVIGATOR_VOICE_NUMBERS).unwrap_or_default();
    views::brand::install_deployment_voice_numbers(&voice_numbers_by_brand(&voice_numbers)?)
        .map_err(anyhow::Error::msg)?;
    tracing::info!(custom = brand_bundle.is_some(), "brand bundle configured");
    tracing::info!(
        environment = cfg.environment.as_str(),
        "configured deployment"
    );

    // Fail loud if a production deploy lacks Restate or GCS —
    // each silently degrades into a dev-only fallback that would lose
    // durability, allow-all every request, or persist client files
    // on a node-local filesystem.
    crate::config::enforce_deployment_invariants(cfg.environment, |k| std::env::var(k).ok())
        .context("deployment environment invariants")?;

    // Application admission verifies the existing migration and activation history.
    // Legacy imports and schema changes belong to explicit operator commands.
    let postgres = store::postgres::PostgresDb::application()
        .await
        .context("verify PostgreSQL application authority")?;
    store::durable_object_handover::verify_runtime_ownership(&postgres)
        .await
        .context("verify durable object ownership")?;
    let application_store = store::application_store::ApplicationStore::from(postgres.clone());

    // Object storage is created before the seed because template bodies
    // are now seeded into blob storage (not an inline column).
    let storage = cloud::from_env()
        .await
        .context("configuring object storage")?;
    tracing::info!("object storage configured");

    // The canonical seed writes template bodies as blobs to object storage.
    // In KIND the web pod can start before SeaweedFS is reachable, so
    // wait for the store to answer a probe before seeding — otherwise the
    // first seed fails on a connection error and the pod crash-loops (with a
    // growing backoff) until the dependency is up. That crash-loop window is
    // the root of the KIND e2e flake. The `fs` backend answers instantly, so
    // this is a no-op for local/`fs` dev.
    cloud::wait_until_ready(&storage, Duration::from_mins(1))
        .await
        .context("waiting for object storage to become ready")?;
    tracing::info!("object storage ready");

    // Public-assets lane: blank government forms are pulled from here at
    // fill/download time and verified against their repo `.sha256` pins.
    // A distinct bucket in prod (`NAVIGATOR_ASSETS_BUCKET`); the same
    // root as `storage` for the fs backend and single-bucket KIND.
    let assets_storage = cloud::assets_from_env()
        .await
        .context("configuring public-assets object storage")?;
    tracing::info!("assets object storage configured");

    // Every boot applies the canonical seed and the booting brand's own
    // seeds — both reach production. A `dev` boot additionally applies the
    // sample-matter fixture, which never does.
    let seed_report =
        store::seed::seed_environment(&application_store, &storage, cfg.environment, brand_seed)
            .await
            .context("seeding environment fixtures")?;
    tracing::info!(
        environment = cfg.environment.as_str(),
        brand = brand_seed.as_str(),
        summary = %seed_report.summary(),
        "seed applied"
    );

    // The PostgreSQL-native Plan-panel tables seed here, not in the SurrealDB
    // loader: the six canonical stages and the anchor Firm's sample plans and
    // fee schedules (ENG-1269). A no-op when PostgreSQL is not configured;
    // idempotent on every later boot.
    store::plan_seeds::seed_quickstart(&application_store)
        .await
        .context("seeding plan panel fixtures")?;

    // Startup convergence has a finite budget; nightly reconciliation retries unfinished slots.
    match tokio::time::timeout(
        std::time::Duration::from_secs(20),
        store::project_folder_reconcile::backfill_from_env(&application_store),
    )
    .await
    {
        Ok(Ok(verified)) => {
            tracing::info!(verified, "Drive folder startup reconciliation finished");
        }
        Ok(Err(_)) => tracing::warn!("Drive folder startup reconciliation failed"),
        Err(_) => tracing::warn!("Drive folder startup reconciliation deferred to nightly retry"),
    }

    let public_dir = std::env::var("NAVIGATOR_PUBLIC_DIR")
        .map_or_else(|_| PathBuf::from(crate::DEFAULT_PUBLIC_DIR), PathBuf::from);
    tracing::info!(?public_dir, "serving static assets");

    // Point `dioxus-server` at the built client bundle (issue #641). The
    // `navigator dev build-webapp` subcommand and `images/Containerfile.web`
    // build it into `<public_dir>/dioxus`; an explicit `DIOXUS_PUBLIC_PATH`
    // (set by tests or a custom layout) always wins. When neither the env nor
    // the default directory carries an `index.html`, the Dioxus demo page stays
    // absent (see `crate::dioxus_app`).
    if std::env::var_os("DIOXUS_PUBLIC_PATH").is_none() {
        let dioxus_dir = public_dir.join("dioxus");
        if dioxus_dir.join("index.html").is_file() {
            // Safe: set once at startup before any router build or worker
            // thread reads it.
            std::env::set_var("DIOXUS_PUBLIC_PATH", &dioxus_dir);
            tracing::info!(?dioxus_dir, "serving Dioxus client bundle");
        }
    }

    let workshops_dir = std::env::var("NAVIGATOR_WORKSHOPS_DIR").map_or_else(
        |_| PathBuf::from(crate::DEFAULT_WORKSHOPS_DIR),
        PathBuf::from,
    );
    let workshop_materials = crate::workshops::loader::load_navigator(&workshops_dir)
        .context("loading workshop content")?;
    tracing::info!(
        count = workshop_materials.len(),
        ?workshops_dir,
        "loaded workshop materials"
    );
    let workshops = crate::WorkshopIndex::new(workshop_materials);

    let auth = crate::AuthConfig::from_env().await;
    tracing::info!(enforced = auth.is_enforced(), "auth configured");

    let google_oauth = crate::google_oauth::GoogleOauthConfig::from_env();
    tracing::info!(
        enforced = google_oauth.is_enforced(),
        "google_oauth configured"
    );

    let canonical_host = crate::CanonicalHost::from_env();
    tracing::info!(
        enforced = canonical_host.is_enforced(),
        "canonical host configured"
    );

    let portal_only = crate::PortalOnly::new(
        brand_bundle
            .as_ref()
            .is_some_and(|bundle| bundle.manifest.portal_only),
    );
    tracing::info!(
        enabled = portal_only.enabled(),
        "portal-only mode configured"
    );

    let sessions = crate::SessionStore::from_env()
        .unwrap_or_else(|| crate::SessionStore::new(crate::session::random_token_32()));
    let oauth = crate::OAuthConfig::from_env()
        .await
        .context("loading OAuth config")?;
    tracing::info!(
        enabled = oauth.is_some(),
        "oauth (Authorization Code + PKCE) configured"
    );
    // Second browser provider. Fails boot when half-configured (a client id
    // with no secret, or no tenant allowlist) rather than quietly rendering a
    // button that cannot complete a sign-in — the same posture as the primary
    // slot, which errors when `OAUTH_ISSUER_URL` is set without its siblings.
    let oauth_microsoft = crate::OAuthConfig::microsoft_from_env()
        .await
        .context("loading Microsoft Entra OAuth config")?;
    tracing::info!(
        enabled = oauth_microsoft.is_some(),
        "oauth microsoft (Authorization Code + PKCE) configured"
    );
    let oauth_apple = crate::OAuthConfig::apple_from_env()
        .await
        .context("loading Sign in with Apple OAuth config")?;
    tracing::info!(
        enabled = oauth_apple.is_some(),
        "oauth apple (Authorization Code + PKCE) configured"
    );

    let policy =
        crate::policy::PolicyClient::embedded().context("compiling embedded Rego policy")?;
    tracing::info!("policy client configured");

    let esignature_webhook_secret = std::env::var("DOCUSIGN_WEBHOOK_SECRET")
        .ok()
        .filter(|s| !s.is_empty());
    let esignature_hmac_key = std::env::var("DOCUSIGN_HMAC_KEY")
        .ok()
        .filter(|s| !s.is_empty());
    tracing::info!(
        path_secret = esignature_webhook_secret.is_some(),
        hmac_key = esignature_hmac_key.is_some(),
        "e-signature webhook auth configured"
    );

    // Explicit, fail-closed selection: `NAVIGATOR_SIGNATURE_PROVIDER`
    // must name `fake` or `docusign`. An unset or unknown value refuses
    // to boot — a "send for signature" button that quietly wrote fake
    // envelopes is the demo-shaped failure this exists to prevent — and
    // `fake` refuses a production environment inside the selection
    // itself. The inbound completion webhook (`crate::esignature_webhook`)
    // closes the loop in both cases — the fake's local ids still
    // persist + correlate.
    let signature_kind = crate::signature::SignatureProviderKind::from_env()
        .context("selecting the signature provider")?;
    // The same fail-closed runtime KMS handle, exposed separately so the
    // Firm settings door can encrypt/decrypt directly through
    // `store::firm_secrets` without a Notion database id.
    let runtime_kms = crate::integrations::runtime_kms_from_env().await;
    let (signature_provider, fake_signature_dir): (
        std::sync::Arc<dyn crate::signature::SignatureProvider>,
        Option<std::path::PathBuf>,
    ) = match signature_kind {
        crate::signature::SignatureProviderKind::Fake => {
            tracing::info!(
                "signature provider: FakeSignatureProvider (local signature loop; no DocuSign credentials)"
            );
            let mut fake = crate::signature::FakeSignatureProvider::from_dev_dir()
                .context("locating the fake provider's envelope directory")?;
            // The fake's sign pages are served by this portal itself: the
            // URL the signer is redirected to is one on this server,
            // backed by the pages the fake writes to disk.
            fake =
                fake.with_signing_base_url(format!("http://127.0.0.1:{}/fake-signature", cfg.port));
            let dir = fake.root().to_owned();
            // Loop the completion webhook back into this portal's own
            // receiver when the receiver can verify it: without
            // `DOCUSIGN_HMAC_KEY` the receiver refuses every delivery,
            // so an unverifiable loop is worse than none — completion
            // then reads straight from the provider instead.
            if let Some(key) = &esignature_hmac_key {
                let secret = esignature_webhook_secret
                    .clone()
                    .unwrap_or_else(|| "local".to_string());
                fake = fake.with_webhook(crate::signature::WebhookTarget {
                    url: format!("http://127.0.0.1:{}/webhook/esignature/{secret}", cfg.port),
                    hmac_key: key.clone().into_bytes(),
                });
                tracing::info!(
                    "fake provider completion webhooks loop back to this portal's receiver"
                );
            }
            (std::sync::Arc::new(fake), Some(dir))
        }
        crate::signature::SignatureProviderKind::DocuSign => {
            let demo = cfg.environment == store::DeploymentEnvironment::Dev
                || store::sample_matters(cfg.environment)
                    .context("resolve DocuSign environment")?;
            (
                std::sync::Arc::new(crate::firm_docusign::FirmDocuSign {
                    kms: runtime_kms.clone(),
                    demo,
                }),
                None,
            )
        }
    };

    // Firm-private provider resolution: the real resolver when both a
    // runtime KMS key and a parent Notion database are configured, else a
    // refusing one. Deliberately not a stub that answers with a fake page —
    // a Firm credential has no deployment-wide fallback, so "not configured"
    // is the honest answer rather than a provider that pretends.
    let integration_providers = crate::integrations::from_env().await;

    let simulated_billing =
        store::sample_matters(cfg.environment).context("resolve simulated billing environment")?;
    let billing_provider: std::sync::Arc<dyn crate::billing::BillingProvider> = if simulated_billing
    {
        std::sync::Arc::new(crate::billing::StubBillingProvider::for_simulated_matters())
    } else {
        std::sync::Arc::new(crate::billing::DisabledBillingProvider)
    };

    let esignature_webhook_secret = std::env::var("DOCUSIGN_WEBHOOK_SECRET")
        .ok()
        .filter(|s| !s.is_empty());
    let esignature_hmac_key = std::env::var("DOCUSIGN_HMAC_KEY")
        .ok()
        .filter(|s| !s.is_empty());
    tracing::info!(
        path_secret = esignature_webhook_secret.is_some(),
        hmac_key = esignature_hmac_key.is_some(),
        "e-signature webhook auth configured"
    );

    let email = crate::email::from_env().context("loading email config")?;
    tracing::info!("email backend configured");
    let clamd_addr = std::env::var("NAVIGATOR_CLAMD_ADDR")
        .ok()
        .filter(|value| !value.is_empty())
        .context("NAVIGATOR_CLAMD_ADDR must name the fail-closed attachment scanner")?;
    let attachment_scanner: std::sync::Arc<dyn crate::attachment_scanner::AttachmentScanner> =
        std::sync::Arc::new(crate::attachment_scanner::ClamdAttachmentScanner::new(
            clamd_addr,
        ));
    tracing::info!("attachment scanner configured");

    let workflow_runtime: std::sync::Arc<dyn workflows::StateMachineRuntime> = std::sync::Arc::new(
        workflows::DbosNotationRuntime::from_lookup(|key| std::env::var(key).ok())
            .context("configure the DBOS Notation runtime")?,
    );
    let questionnaire_runtime = workflow_runtime.clone();

    let inbound_email_secret = std::env::var("SENDGRID_INBOUND_SECRET")
        .ok()
        .filter(|s| !s.is_empty());
    tracing::info!(
        configured = inbound_email_secret.is_some(),
        "inbound email webhook secret configured"
    );

    let email_events_secret = std::env::var("SENDGRID_EVENTS_SECRET")
        .ok()
        .filter(|s| !s.is_empty());
    tracing::info!(
        configured = email_events_secret.is_some(),
        "email events webhook secret configured"
    );

    let sendgrid_events_public_key = std::env::var("SENDGRID_EVENTS_PUBLIC_KEY")
        .ok()
        .filter(|s| !s.is_empty());
    tracing::info!(
        configured = sendgrid_events_public_key.is_some(),
        "email events webhook signature verification configured"
    );

    let summary_envelope_recipients = crate::inbound_email::summary_envelope_recipients_from_env()
        .context("parse NAVIGATOR_SUMMARY_ENVELOPE_RECIPIENTS")?;
    tracing::info!(
        recipients = summary_envelope_recipients.len(),
        "summary envelope allowlist"
    );
    let summary_intake = match workflows::EmailSummaryConfig::from_env()
        .context("configure inbound email summary lane")?
    {
        None => None,
        Some(config) => {
            // The intake mailbox is Neon Law's, so without an explicit
            // override the receipt belongs to the Firm wearing the `neon`
            // brand — the same resolution support intake makes
            // (`inbox::ingest_support_email`). The seed above has already
            // attached the brand, so a miss here is a real gap, not a race.
            let firm_id = match config.firm_id {
                Some(id) => id,
                None => store::firms::firm_id_for_brand_key(&application_store, "neon")
                    .await
                    .context("resolve the summary lane's Firm from the neon brand")?
                    .context(
                        "the summary lane is enabled but no Firm wears the neon brand; \
                         set NAVIGATOR_SUMMARY_FIRM_ID or seed the practice Firm",
                    )?,
            };
            Some(crate::inbound_email::SummaryIntakeConfig {
                envelope_recipients: config.envelope_recipients,
                inbound_public_key: config.inbound_public_key,
                deployment: config.deployment,
                workflow_ingress: config.workflow_ingress,
                project_id: config.project_id,
                channel_id: config.channel_id,
                firm_id,
                matter_project_id: config.matter_project_id,
                provider_account: config.provider_account,
                gemini_model: config.gemini_model,
                gemini_location: config.gemini_location,
                max_input_chars: config.max_input_chars,
                max_output_tokens: config.max_output_tokens,
            })
        }
    };
    tracing::info!(enabled = summary_intake.is_some(), "email summary intake");
    // hCaptcha on the public lead forms. A sitekey without its secret stops
    // boot here rather than silently refusing every lead.
    let hcaptcha = crate::hcaptcha::HCaptcha::from_lookup(|key| std::env::var(key).ok())?
        .map(std::sync::Arc::new);
    tracing::info!(enabled = hcaptcha.is_some(), "hCaptcha on lead forms");
    let sms_intake =
        crate::twilio_sms::SmsIntakeConfig::from_env().context("configure Twilio SMS intake")?;

    tracing::info!(enabled = sms_intake.is_some(), "SMS inbox intake");

    // The Firm registry of Twilio receiving numbers, in PostgreSQL. Every
    // Twilio webhook resolves through it; without a registry the deployment
    // has no receiving numbers and the lanes answer 404.
    let twilio_registry = match store::postgres::PostgresDb::from_env().await {
        Ok(db) => Some(std::sync::Arc::new(db)),
        Err(error) => {
            tracing::warn!(
                %error,
                "Twilio receiving-number registry unavailable; webhook lanes off"
            );
            None
        }
    };

    // The Forgejo push receiver's two gates (`FORGEJO_WEBHOOK_SECRET`
    // coarse, `FORGEJO_WEBHOOK_HMAC_KEY` real) and its journal's
    // PostgreSQL lane — the same shared pool the Twilio registry uses.
    // Without the lane the receiver refuses verified deliveries (503)
    // rather than consume without journaling; Forgejo redelivers.
    let forgejo_webhook_secret = std::env::var("FORGEJO_WEBHOOK_SECRET")
        .ok()
        .filter(|s| !s.is_empty());
    let forgejo_webhook_hmac_key = std::env::var("FORGEJO_WEBHOOK_HMAC_KEY")
        .ok()
        .filter(|s| !s.is_empty());
    tracing::info!(
        path_secret = forgejo_webhook_secret.is_some(),
        hmac_key = forgejo_webhook_hmac_key.is_some(),
        "forgejo push webhook auth configured"
    );
    let forgejo_push_store = match store::postgres::PostgresDb::from_env().await {
        Ok(db) => Some(std::sync::Arc::new(db)),
        Err(error) => {
            tracing::warn!(
                %error,
                "Forgejo push journal unavailable; verified pushes are refused until PostgreSQL is configured"
            );
            None
        }
    };

    // The pipeline's tail: analysis through the Firm's Forgejo, then
    // Block Kit delivery to the Project's Slack channel. Computed before
    // the state literal, which consumes the store and the Surreal lane.
    // Without a journal store the receiver refuses verified pushes before
    // reaching the consumer, so the unwired stand-in never runs.
    let forgejo_push_consumer: std::sync::Arc<dyn crate::forgejo_webhook::PushConsumer> =
        match forgejo_push_store.as_ref() {
            Some(journal) => std::sync::Arc::new(
                crate::forgejo_push_pipeline::JjPushPipeline::for_deployment(
                    journal.clone(),
                    application_store.clone(),
                    runtime_kms.clone(),
                ),
            ),
            None => std::sync::Arc::new(crate::forgejo_webhook::UnwiredPushConsumer),
        };

    let state = crate::AppState {
        brand_bundle,
        application_store,
        workshops,
        auth,
        google_oauth,
        oauth_microsoft,
        oauth_apple,
        rate_limit: crate::rate_limit::RateLimit::from_env(),
        canonical_host,
        portal_only,
        sessions,
        github_oidc: crate::github_oidc::GitHubOidc::github_actions(),
        github_org: std::env::var(cloud::workspace::NAVIGATOR_GITHUB_ORG)
            .ok()
            .filter(|org| !org.trim().is_empty()),
        oauth,
        storage,
        assets_storage,
        forms_registry: std::sync::Arc::new(
            forms::registry().context("loading the vendored forms registry")?,
        ),
        policy,
        workflow_runtime,
        questionnaire_runtime,
        signature_provider,
        fake_signature_dir,
        billing_provider,
        integration_providers,
        runtime_kms,
        // Inbound-contract reviewer: Vertex Gemini when configured, else
        // the deterministic stub — selected here exactly like the A2A
        // router (chosen inside `bootstrap`). The
        // `analysis__contract_deviations` step is web-driven; the worker
        // has no LLM access.
        contract_reviewer: crate::contract_review::GeminiContractReviewer::from_env().map_or_else(
            || -> std::sync::Arc<dyn crate::contract_review::ContractReviewer> {
                std::sync::Arc::new(crate::contract_review::StubContractReviewer)
            },
            |r| std::sync::Arc::new(r),
        ),
        esignature_webhook_secret,
        esignature_hmac_key,
        email,
        attachment_scanner,
        inbound_email_secret,
        summary_intake,
        sms_intake,
        twilio_registry,
        forgejo_webhook_secret,
        forgejo_webhook_hmac_key,
        forgejo_push_store,
        forgejo_push_consumer,
        email_events_secret,
        sendgrid_events_public_key,
        bootstrap_owner_email: crate::oauth::bootstrap_owner_email_from_env(),
        on_call_lawyer_email: crate::oauth::on_call_lawyer_email_from_env(),
        // Opt-in email/password sign-in via GCP Identity Platform; `None`
        // unless `NAVIGATOR_IDENTITY_PLATFORM_API_KEY` is set.
        identity_password: crate::oauth::IdentityPasswordConfig::from_env(),
        // Opt-in admin door (password reset + email confirm); `None`
        // unless `NAVIGATOR_GCP_PROJECT_ID` is set.
        identity_admin: crate::idp_admin::IdentityAdminConfig::from_env(),
        // Production picks the router from env inside `bootstrap`
        // (Vertex when configured, else Null); no override here.
        a2a_router: None,
        plausible_conversions: crate::plausible::ConversionReporter::live(),
        hcaptcha,
    };

    Ok(HostRuntime {
        config: cfg,
        state,
        public_dir,
    })
}

/// Parse [`views::brand::NAVIGATOR_VOICE_NUMBERS`]: comma-separated
/// `brand_key=+E164` pairs. Blank is no override; an unknown Brand, a repeated
/// Brand, or a number that is not E.164 stops the boot rather than publishing
/// a line no caller can dial.
fn voice_numbers_by_brand(value: &str) -> anyhow::Result<Vec<(views::brand::BrandKey, String)>> {
    let var = views::brand::NAVIGATOR_VOICE_NUMBERS;
    let mut numbers: Vec<(views::brand::BrandKey, String)> = Vec::new();
    for pair in value
        .split(',')
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
    {
        let (key, number) = pair
            .split_once('=')
            .map(|(key, number)| (key.trim(), number.trim()))
            .with_context(|| format!("{var} entry `{pair}` is not brand_key=+E164"))?;
        let key = views::brand::BrandKey::parse(key)
            .with_context(|| format!("{var} names no Brand `{key}`"))?;
        anyhow::ensure!(
            store::brand_phone_numbers::is_e164(number),
            "{var} number for `{}` must be E.164, e.g. +15108002090",
            key.as_str()
        );
        anyhow::ensure!(
            numbers.iter().all(|(seen, _)| *seen != key),
            "{var} names `{}` twice",
            key.as_str()
        );
        numbers.push((key, number.to_string()));
    }
    Ok(numbers)
}

/// Resolve when the process receives `SIGINT` (Ctrl-C) or `SIGTERM`.
///
/// Kubernetes sends `SIGTERM` at pod termination; every host binary awaits this
/// so `axum::serve`'s graceful shutdown drains in-flight requests and flushes
/// batched telemetry before exit.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = term => {}
    }
}

/// Builds a brand's public Dioxus SSR routers once the runtime exists.
///
/// A closure rather than a plain value because the content-backed pages read
/// [`AppState`], which does not exist until [`build_from_env`] has run.
pub type PublicDioxusRouters = Box<dyn FnOnce(&AppState) -> Vec<Router> + Send>;

/// One public face, declared by the brand binary that serves it.
///
/// This is the whole of a brand crate: a name to report telemetry under and
/// the two halves of its public surface. Everything else — the database, the
/// authenticated application, the JSON API, the anonymous-access boundary —
/// comes from the application crate, identically for every brand, so a brand
/// cannot fork authorization by construction.
pub struct Site {
    /// The brand's short key (`neon`). Labels the boot log,
    /// so a deploy names the face it serves in its first lines.
    pub key: &'static str,
    /// Which brand's own seeds this deployment applies. Unlike the
    /// sample-matter fixture, this layer reaches production, so
    /// it carries the data one brand owns and the other must not — the
    /// Firm's postal identities against another entity's.
    pub seed: store::seed::BrandSeed,
    /// The telemetry `service.name` this deployment reports under. It names
    /// the deployment rather than the process, so a trace says which face it
    /// came from.
    pub service_name: &'static str,
    /// Whether this host publishes no public marketing surface at all — the
    /// white-label tenant shape, which lights [`crate::PortalOnly`].
    pub portal_only: bool,
    /// The brand's public route table, mounted outside the session boundary.
    pub public_routes: Router<AppState>,
    /// Every path the brand registers across its public and Dioxus
    /// routers. [`crate::bootstrap`] rejects any entry at or below a
    /// Navigator-owned prefix before Axum composes the routers.
    pub public_paths: &'static [&'static str],
    /// The brand's public Dioxus SSR routers. A brand that supplies none here
    /// 404s its own home page whenever its real pages are Dioxus ports.
    pub public_dioxus: PublicDioxusRouters,
}

/// Boot one brand: telemetry, the shared runtime, that brand's composition,
/// bind, serve, drain.
///
/// This is the entire body of a brand binary's `main`. The ordering it fixes
/// is load-bearing — `.env` before any environment read, telemetry before the
/// first `tracing` call, and the Dioxus routers built from `state` before
/// `state` moves into [`crate::bootstrap`] — and stating it once is what keeps
/// brand binaries from drifting the way the pre-#860 host pair did.
///
/// # Errors
///
/// Propagates a failure to assemble the runtime (configuration, database,
/// storage, content) or to bind and serve the configured port.
pub async fn run(brand: Site) -> anyhow::Result<()> {
    Box::pin(boot(brand)).await
}

/// Launch the DBOS executor before serving a mounted application.
pub async fn with_dbos<T>(
    lookup: impl Fn(&str) -> Option<String>,
    notifier: Arc<dyn Notifier>,
    serve: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    let settings = dbos_engine::Settings::from_lookup(&lookup).context("configure DBOS")?;
    let executor = dbos_engine::Executor::launch_hosting_with(
        &settings,
        notifier,
        dbos_engine::Schedules::default(),
        |_| Ok(Vec::new()),
    )
    .await
    .context("launch the DBOS executor")?;
    let served = serve.await;
    executor.shutdown().await;
    served
}

/// The boot-time journal replay's per-boot budget (ENG-787): how many
/// pending e-signature receipts one boot drains. A restart with a larger
/// backlog converges over subsequent boots instead of holding the boot
/// host.
const REPLAY_BUDGET: i64 = 100;

/// Journal replay (ENG-787): a verified e-signature receipt that
/// outlived its delivery — the worker died mid-consumption, the 5xx
/// outlived the provider's retry budget — is recovered at boot, from
/// the journal, through the same consumption function first delivery
/// ran. Non-fatal by design: a sweep that cannot run must not block
/// serving, and the backlog converges over subsequent boots.
fn spawn_journal_replay(state: &crate::AppState) {
    let replay_state = state.clone();
    tokio::spawn(async move {
        if let Err(error) =
            crate::esignature_webhook::replay_pending_receipts(&replay_state, REPLAY_BUDGET).await
        {
            tracing::error!(%error, "esignature webhook journal replay failed");
        }
        // The Forgejo push journal drains the same way: a verified
        // receipt whose consumption died mid-flight is recovered from
        // the journal at boot, never dropped.
        if let Err(error) =
            crate::forgejo_webhook::replay_pending_receipts(&replay_state, REPLAY_BUDGET).await
        {
            tracing::error!(%error, "forgejo push webhook journal replay failed");
        }
    });
}

mod cohost;

async fn boot(brand: Site) -> anyhow::Result<()> {
    // Load `.env` / `.devx/env` before any env read; in cluster the
    // environment is injected and both calls are no-ops.
    let _ = dotenvy::dotenv();
    let _ = dotenvy::from_path(".devx/env");

    // One observability seam per binary, named for the deployment.
    let telemetry_guard = telemetry::init(brand.service_name);

    let mut rt = build_from_env(brand.seed).await?;

    // The tenant shape lights the existing portal-only mode rather than adding
    // a second way to express "no public surface". A mounted brand manifest
    // can still enable it on its own, and a brand never turns it *off*, so a
    // portal-only bundle stays portal-only whatever binary serves it.
    if brand.portal_only {
        rt.state.portal_only = crate::PortalOnly::new(true);
    }

    preflight(&brand, &rt);

    // Reconcile the bootstrap-Owner invariant immediately, rather than
    // waiting for that identity's next sign-in: a changed
    // `NAVIGATOR_BOOTSTRAP_OWNER_EMAIL` should promote its new match and
    // demote any stale `Owner` row from an old value right away, not on
    // whatever schedule the two people involved happen to log back in.
    if let Err(error) = crate::oauth::reconcile_bootstrap_owner(
        &rt.state.application_store,
        rt.state.bootstrap_owner_email.as_deref(),
    )
    .await
    {
        tracing::error!(%error, "bootstrap Owner reconciliation failed");
    }

    // Journal replay (ENG-787): see `spawn_journal_replay`.
    spawn_journal_replay(&rt.state);
    crate::docusign_completion::spawn(&rt.state);

    // Every local port a registered brand's own env var claims (see
    // `views::brand::BrandKey::local_port_env_var`), read before `rt.state`
    // moves into `bootstrap`. Production and staging bind only the primary
    // port — every brand there is reached by its own real hostname, so this
    // is empty outside local dev.
    let extra_ports: Vec<u16> = rt.state.canonical_host.local_ports().collect();

    // Built before `rt.state` moves into `bootstrap`: the public pages are
    // Dioxus routers resolved from state, and a brand that passed none here
    // would 404 its own home page.
    let host_dioxus = (brand.public_dioxus)(&rt.state);
    let lookup = |key: &str| std::env::var(key).ok();
    let dbos_schedules = dbos_schedules(&rt, lookup).await?;
    let sent_email_export = sent_email_export_source(lookup).await?;
    let router = crate::bootstrap(
        rt.state,
        &rt.public_dir,
        brand.public_routes,
        brand.public_paths,
        host_dioxus,
    )?;

    #[cfg(debug_assertions)]
    let reload = crate::dev_reload::enabled();
    #[cfg(not(debug_assertions))]
    let reload = false;
    #[cfg(debug_assertions)]
    let router = if reload {
        router.layer(axum::middleware::from_fn(crate::dev_reload::refresh))
    } else {
        router
    };

    let mut listeners = Vec::new();
    for port in bind_ports(rt.config.port, extra_ports) {
        let ip = if reload { [127, 0, 0, 1] } else { [0, 0, 0, 0] };
        let addr = SocketAddr::from((ip, port));
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("binding {addr}"))?;
        tracing::info!(%addr, brand = brand.key, "server listening");
        listeners.push(listener);
    }

    // One `axum::serve` task per bound listener, all serving the same
    // router: a request's brand comes from its `Host:` header (`portal`'s
    // `resolve_brand_and_enforce_host`), not from which listener accepted
    // it, so every listener shares this one composed router. Each task
    // drains on the shared cancellation token, so every bound port stops
    // serving together when the signal or either host fails.
    let stop = dbos::CancellationToken::new();
    let server_stop = stop.clone();
    let serve = async move {
        let mut servers = tokio::task::JoinSet::new();
        for listener in listeners {
            let router = router.clone();
            let stop = server_stop.clone();
            servers.spawn(async move {
                axum::serve(listener, router)
                    .with_graceful_shutdown(stop.cancelled_owned())
                    .await
            });
        }
        while let Some(result) = servers.join_next().await {
            result
                .context("server task panicked")?
                .context("axum serve")?;
        }
        anyhow::Ok(())
    };
    let served = Box::pin(cohost::run(dbos_schedules, sent_email_export, stop, serve)).await;
    telemetry_guard.shutdown();
    served
}

/// Register the archive crate's DBOS schedules this host enabled, returning
/// a starter for each.
fn register_archive_schedules(
    dbos: &dbos::DBOS,
    sent_email_export: Option<Arc<dyn archives::sent_email_export::ExportSource>>,
) -> dbos::Result<Vec<dbos_engine::ScheduleStarter>> {
    let mut starters: Vec<dbos_engine::ScheduleStarter> = Vec::new();
    if let Some(source) = sent_email_export {
        let workflow = archives::sent_email_export::register(dbos, source)?;
        starters.push(Box::new(move |stop| {
            tokio::spawn(archives::sent_email_export::schedule(workflow, stop))
        }));
    }
    Ok(starters)
}

/// The Sent Email Export's source, when this host runs DBOS and the schedule's
/// own switch is on. The switch is read before PostgreSQL or the archive bucket
/// is opened, and an enabled schedule fails the boot without either: the
/// export is what lets the one-year purge run, so it never starts half-wired.
async fn sent_email_export_source(
    lookup: impl Fn(&str) -> Option<String> + Copy,
) -> anyhow::Result<Option<Arc<dyn archives::sent_email_export::ExportSource>>> {
    if !archives::sent_email_export::enabled(lookup)
        .map_err(anyhow::Error::msg)
        .context("read NAVIGATOR_DBOS_SENT_EMAIL_EXPORT_ENABLED")?
    {
        return Ok(None);
    }
    Ok(Some(Arc::new(
        archives::sent_email_export::StoreExportSource::from_env()
            .await
            .context("configure DBOS SentEmailExport")?,
    )))
}

/// The deployment's Slack bot every hosted schedule posts with, and whether
/// it posts rather than captures.
fn deployment_slack(
    rt: &HostRuntime,
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<(Arc<dyn workflows::SlackBot>, bool)> {
    workflows::firm_slack::deployment_bot_or_capture(
        rt.state.application_store.clone(),
        rt.state.runtime_kms.clone(),
        lookup,
    )
    .map_err(anyhow::Error::msg)
    .context("configure the deployment's Slack bot")
}

/// Build application-owned schedules, deferring worker contexts when cohosted.
/// Resolve each flag before context construction so unused schedules require
/// no credentials or storage.
#[allow(clippy::too_many_lines)] // One gate per hosted schedule.
async fn dbos_schedules(
    rt: &HostRuntime,
    lookup: impl Fn(&str) -> Option<String> + Copy,
) -> anyhow::Result<dbos_engine::Schedules> {
    let project_recovery = if should_host_project_recovery(lookup)? {
        Some(
            project_recovery_context(rt, lookup)
                .await
                .context("configure DBOS ProjectRecovery")?,
        )
    } else {
        None
    };
    let source_imports = source_imports_schedule(rt, lookup).await?;
    let retention_purge = if should_host_retention_purge(lookup)? {
        Some(
            retention_purge_context(rt, lookup)
                .await
                .context("configure DBOS RetentionPurge")?,
        )
    } else {
        None
    };
    let deadline_mirror = should_host_deadline_mirror(lookup)?.then(|| deadline_mirror_context(rt));
    let purchase_activation = if should_host_purchase_activation(lookup)? {
        let pg = store::postgres::PostgresDb::from_env()
            .await
            .context("connect PostgreSQL for purchase activation")?;
        Some(workflows::dbos_engine::purchase_activation::Context::new(
            Arc::new(crate::purchases::StoreActivationSource::new(
                pg,
                rt.state.billing_provider.clone(),
                rt.state.application_store.clone(),
            )),
        ))
    } else {
        None
    };
    let (slack, _) = deployment_slack(rt, lookup)?;
    let linear_backlog = dbos_engine::linear_backlog::Context::from_lookup(slack, lookup)?;
    let daily_cost_export = workflows::flags::daily_cost_export(lookup)?;

    Ok(dbos_engine::Schedules {
        forge_parity: true,
        daily_cost_export,
        linear_backlog,
        project_recovery,
        source_imports,
        retention_purge,
        deadline_mirror,
        plan_retainers: Some({
            Arc::new(crate::plan_retainers::PlanRetainers(rt.state.clone()))
                as Arc<dyn dbos_engine::plan_retainers::Source>
        }),
        project_calendar: Some({
            Arc::new(crate::calendar_mirror::MembershipSource {
                application_store: rt.state.application_store.clone(),
                providers: Arc::clone(&rt.state.integration_providers),
            }) as Arc<dyn dbos_engine::project_calendar::Source>
        }),
        purchase_activation,
        ceo_digest: ceo_digest_schedule(rt, lookup).await?,
        ..Default::default()
    })
}

/// Resolve the independent SourceImports gate and build its context, so an
/// unused schedule never requires PostgreSQL or KMS credentials.
async fn source_imports_schedule(
    rt: &HostRuntime,
    lookup: impl Fn(&str) -> Option<String> + Copy,
) -> anyhow::Result<Option<workflows::dbos_engine::source_imports::Context>> {
    if !should_host_source_imports(lookup)? {
        return Ok(None);
    }
    let pg = store::postgres::PostgresDb::from_env()
        .await
        .context("connect PostgreSQL for source imports")?;
    Ok(Some(workflows::dbos_engine::source_imports::Context::new(
        Arc::new(
            workflows::dbos_engine::source_imports::StoreImportSource::new(
                rt.state.application_store.clone(),
                pg,
                cloud::runtime_kms_from_env().await,
            ),
        ),
    )))
}

/// Resolve the independent CeoDigest gate and build its context, so an
/// unused schedule never requires Vertex, email, or PostgreSQL credentials.
async fn ceo_digest_schedule(
    rt: &HostRuntime,
    lookup: impl Fn(&str) -> Option<String> + Copy,
) -> anyhow::Result<Option<workflows::dbos_engine::ceo_digest::Context>> {
    if !should_host_ceo_digest(lookup)? {
        return Ok(None);
    }
    let context = ceo_digest_context(rt, lookup)
        .await
        .context("configure DBOS CeoDigest")?;
    Ok(Some(context))
}

/// Resolve the independent PurchaseActivation gate.
fn should_host_purchase_activation(
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<bool> {
    workflows::dbos_engine::purchase_activation::enabled(lookup)
        .context("read NAVIGATOR_DBOS_PURCHASE_ACTIVATION_ENABLED")
}

/// Resolve the independent RetentionPurge gate.
fn should_host_retention_purge(lookup: impl Fn(&str) -> Option<String>) -> anyhow::Result<bool> {
    workflows::dbos_engine::retention_purge::enabled(lookup)
        .context("read NAVIGATOR_DBOS_RETENTION_PURGE_ENABLED")
}

/// The retention schedule's purge. An enabled schedule fails the boot without
/// a well-formed authority map, PostgreSQL, or the archived-projects lane: an
/// unattended purge never runs on an ambient or invented authority.
async fn retention_purge_context(
    rt: &HostRuntime,
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<workflows::dbos_engine::retention_purge::Context> {
    let map = workflows::dbos_engine::retention_purge::authorities(&lookup)
        .context("read NAVIGATOR_RETENTION_PURGE_AUTHORITIES")?;
    let authority = crate::expunge::purge::PurgeAuthority::by_firm_from(&map)
        .map_err(|error| anyhow::anyhow!("NAVIGATOR_RETENTION_PURGE_AUTHORITIES: {error}"))?;
    let pg = store::postgres::PostgresDb::from_env()
        .await
        .context("connect PostgreSQL for the retention purge")?;
    let archive = cloud::archived_projects_from_lookup(&lookup)
        .await
        .context("open the archived-projects lane")?;
    Ok(workflows::dbos_engine::retention_purge::Context::new(
        Arc::new(crate::expunge::purge::ScheduledPurge {
            application_store: rt.state.application_store.clone(),
            storage: Arc::clone(&rt.state.storage),
            pg: Some(pg),
            archive: Some(archive),
            authority,
        }),
    ))
}

/// Resolve the independent DeadlineMirror gate.
fn should_host_deadline_mirror(lookup: impl Fn(&str) -> Option<String>) -> anyhow::Result<bool> {
    workflows::dbos_engine::deadline_mirror::enabled(lookup)
        .context("read NAVIGATOR_DBOS_DEADLINE_MIRROR_ENABLED")
}

/// The nightly mirror's source: the store, the deployment's Workspace
/// calendar provider, and the canonical host for matter-page links.
fn deadline_mirror_context(rt: &HostRuntime) -> workflows::dbos_engine::deadline_mirror::Context {
    workflows::dbos_engine::deadline_mirror::Context::new(Arc::new(
        crate::calendar_mirror::ScheduledMirror {
            application_store: rt.state.application_store.clone(),
            providers: Arc::clone(&rt.state.integration_providers),
            base_url: rt.state.canonical_host.host().map(str::to_string),
        },
    ))
}

/// Resolve the independent SourceImport gate.
fn should_host_source_imports(lookup: impl Fn(&str) -> Option<String>) -> anyhow::Result<bool> {
    workflows::dbos_engine::source_imports::enabled(lookup)
        .context("read NAVIGATOR_DBOS_SOURCE_IMPORT_ENABLED")
}

/// Resolve the independent ProjectRecovery gate before opening any archive
/// lane, so an unused schedule never requires its storage.
fn should_host_project_recovery(lookup: impl Fn(&str) -> Option<String>) -> anyhow::Result<bool> {
    workflows::dbos_engine::project_recovery::enabled(lookup)
        .context("read NAVIGATOR_DBOS_PROJECT_RECOVERY_ENABLED")
}

/// Open every lane the recovery schedule needs. An enabled schedule fails the
/// boot when PostgreSQL or the archived-projects lane is unavailable; the Git
/// bundle lane is optional, and a Project with a repository then fails its own
/// archive run rather than the boot.
async fn project_recovery_context(
    rt: &HostRuntime,
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<workflows::dbos_engine::project_recovery::Context> {
    let pg = store::postgres::PostgresDb::from_env()
        .await
        .context("connect PostgreSQL for recovery archives")?;
    let archive = cloud::archived_projects_from_lookup(&lookup)
        .await
        .context("open the archived-projects lane")?;
    let source_backups: Option<Arc<dyn cloud::StorageService>> =
        match lookup("NAVIGATOR_FORGEJO_ARCHIVES_BUCKET") {
            Some(bucket) if !bucket.trim().is_empty() => Some(Arc::new(
                cloud::GcsStorage::new_from_config(
                    cloud::GcsStorageConfig::forgejo_archives_from_lookup(&lookup)
                        .context("configure the Git bundle lane")?,
                )
                .await
                .context("open the Git bundle lane")?,
            )),
            _ => None,
        };
    Ok(workflows::dbos_engine::project_recovery::Context::new(
        Arc::new(
            workflows::dbos_engine::project_recovery::StoreRecoverySource::new(
                rt.state.application_store.clone(),
                pg,
                Arc::clone(&rt.state.storage),
                archive,
                source_backups,
            ),
        ),
    ))
}

async fn ceo_digest_context(
    rt: &HostRuntime,
    lookup: impl Fn(&str) -> Option<String> + Copy,
) -> anyhow::Result<workflows::dbos_engine::ceo_digest::Context> {
    let simulated = store::sample_matters(rt.config.environment)
        .context("resolve simulated-matters deployment")?;
    let prose = workflows::dbos_engine::ceo_digest::VertexProseProvider::from_lookup(lookup)
        .context("configure the CEO digest prose provider")?;
    let pg = store::postgres::PostgresDb::from_env()
        .await
        .context("connect PostgreSQL for the CEO digest")?;
    workflows::dbos_engine::ceo_digest::Context::from_lookup(
        Arc::new(workflows::dbos_engine::ceo_digest::StoreDigestSource::new(
            rt.state.application_store.clone(),
            pg,
        )),
        Arc::new(prose),
        crate::email::from_lookup(lookup).context("configure the CEO digest email backend")?,
        deployment_slack(rt, lookup)?.0,
        simulated,
        lookup,
    )
    .context("validate CEO digest delivery configuration")
}

/// Resolve the independent CEO digest gate before constructing a delivery
/// context, so an unused schedule never requires Slack credentials or a
/// Vertex project.
fn should_host_ceo_digest(lookup: impl Fn(&str) -> Option<String>) -> anyhow::Result<bool> {
    workflows::dbos_engine::ceo_digest::enabled(lookup)
        .context("read NAVIGATOR_DBOS_CEO_DIGEST_ENABLED")
}

/// Every port this boot binds: the primary configured port plus every extra
/// local-brand port, deduplicated and in a stable order. A pure helper so
/// the bind set is unit-testable without a real listener.
fn bind_ports(primary: u16, extra: impl IntoIterator<Item = u16>) -> Vec<u16> {
    let mut ports: Vec<u16> = std::iter::once(primary).chain(extra).collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// Report the resolved brand at boot, so a deploy serving the wrong face says
/// so in its first log lines rather than at the first request.
fn preflight(brand: &Site, rt: &HostRuntime) {
    tracing::info!(
        brand = brand.key,
        service_name = brand.service_name,
        portal_only = rt.state.portal_only.enabled(),
        environment = rt.config.environment.as_str(),
        "brand configured"
    );
}

#[cfg(test)]
mod tests {
    use super::{bind_ports, sent_email_export_source, voice_numbers_by_brand};

    /// Voice numbers are optional and per Brand, and every listed one must
    /// name a real Brand once with a dialable number.
    #[test]
    fn deployment_voice_numbers_are_per_brand_and_e164() {
        use views::brand::BrandKey;
        assert!(voice_numbers_by_brand(" ").unwrap().is_empty());
        assert_eq!(
            voice_numbers_by_brand(" summons = +15550100100 ,neon=+15550100101").unwrap(),
            vec![
                (BrandKey::Summons, "+15550100100".to_string()),
                (BrandKey::Neon, "+15550100101".to_string()),
            ]
        );
        for refused in [
            "summons",
            "nobody=+15550100100",
            "summons=555-010-0100",
            "summons=+15550100100,summons=+15550100101",
        ] {
            let error = voice_numbers_by_brand(refused).unwrap_err().to_string();
            assert!(error.contains("NAVIGATOR_VOICE_NUMBERS"), "{error}");
        }
    }

    #[tokio::test]
    async fn sent_email_export_requires_an_explicit_valid_schedule_setting() {
        let key = archives::sent_email_export::ENABLED_ENV;
        assert!(sent_email_export_source(|_| None).await.unwrap().is_none());
        assert!(
            sent_email_export_source(|name| (name == key).then(|| "yes".into()))
                .await
                .is_err()
        );
    }

    #[test]
    fn bind_ports_includes_the_primary_port_with_no_extras() {
        assert_eq!(bind_ports(3001, []), vec![3001]);
    }

    /// Every local-brand port joins the primary port, so `run` binds one
    /// listener per registered brand this boot can reach locally.
    #[test]
    fn bind_ports_adds_every_extra_port() {
        assert_eq!(bind_ports(3001, [3011]), vec![3001, 3011]);
    }

    /// A duplicate or out-of-order extra port collapses to one stable,
    /// sorted bind set rather than binding the same port twice.
    #[test]
    fn bind_ports_dedupes_and_sorts() {
        assert_eq!(bind_ports(3011, [3001, 3011, 3001]), vec![3001, 3011]);
    }
}
