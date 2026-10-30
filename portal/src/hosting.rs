//! Shared host runtime assembly and run loop for Navigator's brand binaries.
//!
//! Every brand binary boots the same Navigator application: same database,
//! storage, providers, and `AppState`. What differs is the [`Site`] it
//! declares — its telemetry name and the public surface it publishes — so a
//! brand crate is a `Site` value and nothing else. [`build_from_env`]
//! assembles the runtime and [`run`] owns the boot: telemetry, that assembly,
//! composition through [`crate::bootstrap`], bind, serve, and drain.
//! [`run_with_dbos`] is the same boot hosting the DBOS executor beside the
//! server when `NAVIGATOR_DBOS_ENABLED=true` ([`with_dbos`]).
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
/// resolve config, connect + migrate + seed the database, configure object
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

    // The second store. Its schema is applied rather than migrated —
    // one idempotent `DEFINE` file converged on every boot, which is
    // what `store::schema::apply` documents itself for. There is no
    // fallback: `SurrealConfig::from_env` fails closed, so a missing
    // endpoint stops the boot here instead of surfacing later as a
    // password-reset link that cannot be minted.
    let surreal = store::surreal::connect_from_env()
        .await
        .context("connecting SurrealDB")?;
    store::schema::apply(&surreal)
        .await
        .context("applying the SurrealDB schema")?;
    tracing::info!(
        version = store::schema::SCHEMA_VERSION,
        "surreal schema applied"
    );

    // The Firm journal lives in PostgreSQL when a host is configured and in
    // SurrealDB's `journal_entry` table otherwise. With PostgreSQL, the
    // temporary import copies those SurrealDB rows on every boot before
    // accepting requests; journal requests then write only to PostgreSQL.
    let postgres_journal = store::firm_journal::JournalStore::postgres_configured();
    tracing::info!(
        backend = store::firm_journal::JournalStore::selected_backend(postgres_journal),
        "firm journal backend selected"
    );
    if postgres_journal {
        let postgres = store::postgres::PostgresDb::from_env()
            .await
            .context("connecting PostgreSQL journal")?;
        let report = store::journal_startup_migration::import(&surreal, &postgres)
            .await
            .context("importing legacy journal into PostgreSQL")?;
        tracing::info!(
            imported = report.imported,
            triage = report.triage.len(),
            "legacy journal import completed"
        );
    }

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
        store::seed::seed_environment(&surreal, &storage, cfg.environment, brand_seed)
            .await
            .context("seeding environment fixtures")?;
    tracing::info!(
        environment = cfg.environment.as_str(),
        brand = brand_seed.as_str(),
        summary = %seed_report.summary(),
        "seed applied"
    );

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

    // Real DocuSign provider when the env is configured; otherwise the
    // stub (KIND / local dev). The inbound completion webhook
    // (`crate::esignature_webhook`) closes the loop in both cases — the
    // stub's synthetic ids still persist + correlate.
    let signature_provider: std::sync::Arc<dyn crate::signature::SignatureProvider> =
        crate::signature::DocuSignSignatureProvider::from_env().map_or_else(
            || {
                tracing::info!("signature provider: StubSignatureProvider (DOCUSIGN_* unset)");
                std::sync::Arc::new(crate::signature::StubSignatureProvider::new())
                    as std::sync::Arc<dyn crate::signature::SignatureProvider>
            },
            |ds| {
                tracing::info!("signature provider: DocuSignSignatureProvider");
                std::sync::Arc::new(ds)
            },
        );

    // Firm-private provider resolution: the real resolver when both a
    // runtime KMS key and a parent Notion database are configured, else a
    // refusing one. Deliberately not a stub that answers with a fake page —
    // a Firm credential has no deployment-wide fallback, so "not configured"
    // is the honest answer rather than a provider that pretends.
    let integration_providers = crate::integrations::from_env().await;
    // The same fail-closed runtime KMS handle, exposed separately so the
    // Firm settings door can encrypt/decrypt directly through
    // `store::firm_secrets` without a Notion database id.
    let runtime_kms = crate::integrations::runtime_kms_from_env().await;

    // Real Xero provider when the env is configured; otherwise the stub
    // (KIND / local dev), so a fork boots and self-tests without a Xero
    // custom connection. Mirrors the signature-provider wiring above.
    let billing_provider: std::sync::Arc<dyn crate::billing::BillingProvider> =
        crate::billing::XeroBillingProvider::from_env().map_or_else(
            || {
                tracing::info!("billing provider: StubBillingProvider (XERO_* unset)");
                std::sync::Arc::new(crate::billing::StubBillingProvider::new())
                    as std::sync::Arc<dyn crate::billing::BillingProvider>
            },
            |xero| {
                tracing::info!("billing provider: XeroBillingProvider");
                std::sync::Arc::new(xero)
            },
        );

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

    let email = crate::email::from_env(surreal.clone()).context("loading email config")?;
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

    // Runtime selection: if `RESTATE_BROKER_URL` is set in the
    // environment, the `web` binary talks to the in-cluster
    // `workflows-service` worker through Restate. Otherwise we fall
    // back to the in-process `InMemoryRuntime` *wrapped in
    // `DispatchingRuntime`* — without that wrap the local dev binary
    // never fires the welcome email (the `email_send__*` step has no
    // worker to consume it).
    //
    // Both implement `StateMachineRuntime`; the workflow and
    // questionnaire timelines share a single runtime instance keyed
    // by `(MachineKind, notation_id)`.
    let (workflow_runtime, questionnaire_runtime): (
        std::sync::Arc<dyn workflows::StateMachineRuntime>,
        std::sync::Arc<dyn workflows::StateMachineRuntime>,
    ) = if std::env::var("RESTATE_BROKER_URL").is_ok() {
        let rt = std::sync::Arc::new(workflows::RestateRuntime::from_env());
        tracing::info!("runtime: RestateRuntime (RESTATE_BROKER_URL is set)");
        (rt.clone(), rt)
    } else {
        let inner: std::sync::Arc<dyn workflows::StateMachineRuntime> =
            std::sync::Arc::new(workflows::InMemoryRuntime::new());
        let workflow = std::sync::Arc::new(
            workflows::DispatchingRuntime::new(inner.clone(), email.clone(), storage.clone())
                .with_store(surreal.clone()),
        );
        tracing::info!(
            "runtime: DispatchingRuntime<InMemoryRuntime> (RESTATE_BROKER_URL unset; dispatches \
             email_send__* steps in-process through the EmailService)"
        );
        (workflow, inner)
    };

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
                None => store::firms::firm_id_for_brand_key(&surreal, "neon")
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

    let state = crate::AppState {
        brand_bundle,
        surreal,
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
    boot(brand, false).await
}

/// Boot one brand like [`run`], hosting the DBOS executor in this process when
/// `NAVIGATOR_DBOS_ENABLED=true`.
///
/// Unset or `false` is exactly [`run`]. With the flag on, the executor
/// launches after the listeners bind and before they serve, and shuts down
/// after every listener has drained on `SIGTERM` or Ctrl-C. See [`with_dbos`].
///
/// # Errors
///
/// Everything [`run`] propagates, plus an invalid flag, incomplete DBOS
/// settings, or a failed DBOS launch: with the flag on, the boot fails closed
/// rather than serving without the executor.
pub async fn run_with_dbos(brand: Site) -> anyhow::Result<()> {
    boot(brand, true).await
}

/// Run `serve`, hosting the DBOS executor beside it when `lookup` sets
/// `NAVIGATOR_DBOS_ENABLED=true`.
///
/// With the flag on this reads the DBOS login and release
/// (`workflows::dbos_engine::Settings`), registers every ported workflow,
/// launches DBOS (which re-enqueues this version's pending runs), and starts
/// each schedule before `serve` is polled. Once `serve` returns — the HTTP
/// server's graceful shutdown — it stops the schedules and shuts DBOS down;
/// work cut off there stays pending for the next launch of the same version.
/// Unset or `false` only awaits `serve`.
///
/// # Errors
///
/// An invalid flag, incomplete settings, or a failed launch, before `serve`
/// runs; otherwise whatever `serve` returns.
pub async fn with_dbos<T>(
    lookup: impl Fn(&str) -> Option<String>,
    notifier: Arc<dyn Notifier>,
    serve: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    with_dbos_and_general_nag(lookup, notifier, None, serve).await
}

/// Like [`with_dbos`], with an explicitly enabled GeneralNag context.
pub async fn with_dbos_and_general_nag<T>(
    lookup: impl Fn(&str) -> Option<String>,
    notifier: Arc<dyn Notifier>,
    general_nag: Option<workflows::dbos_engine::general_nag::Context>,
    serve: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    with_dbos_and_schedules(lookup, notifier, general_nag, None, serve).await
}

/// Like [`with_dbos`], with explicitly enabled daily operations schedules.
pub async fn with_dbos_and_schedules<T>(
    lookup: impl Fn(&str) -> Option<String>,
    notifier: Arc<dyn Notifier>,
    general_nag: Option<workflows::dbos_engine::general_nag::Context>,
    dri_digest: Option<workflows::dbos_engine::dri_digest::Context>,
    serve: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    with_dbos_hosting(
        lookup,
        notifier,
        dbos_engine::Schedules {
            general_nag,
            dri_digest,
            ..dbos_engine::Schedules::default()
        },
        serve,
    )
    .await
}

/// Like [`with_dbos`], with every optional schedule the host enabled.
pub async fn with_dbos_hosting<T>(
    lookup: impl Fn(&str) -> Option<String>,
    notifier: Arc<dyn Notifier>,
    schedules: dbos_engine::Schedules,
    serve: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    with_dbos_hosting_and_external(lookup, notifier, schedules, |_| Ok(Vec::new()), serve).await
}

/// Like [`with_dbos_hosting`], allowing the application host to register
/// workflows owned by service crates which depend on `workflows`.
///
/// The registration callback runs before DBOS launches, and its schedule
/// starters run only after a successful launch. When DBOS is disabled the
/// callback is not evaluated and `serve` retains the existing behavior.
pub async fn with_dbos_hosting_and_external<T, F>(
    lookup: impl Fn(&str) -> Option<String>,
    notifier: Arc<dyn Notifier>,
    schedules: dbos_engine::Schedules,
    register_external: F,
    serve: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T>
where
    F: FnOnce(&dbos::DBOS) -> dbos::Result<Vec<dbos_engine::ScheduleStarter>>,
{
    if !dbos_engine::enabled(&lookup).context("read NAVIGATOR_DBOS_ENABLED")? {
        return serve.await;
    }
    let settings = dbos_engine::Settings::from_lookup(&lookup).context("configure DBOS")?;
    let executor = dbos_engine::Executor::launch_hosting_with(
        &settings,
        notifier,
        schedules,
        register_external,
    )
    .await
    .context("launch the DBOS executor")?;
    let served = serve.await;
    executor.shutdown().await;
    served
}

async fn boot(brand: Site, host_dbos: bool) -> anyhow::Result<()> {
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
        &rt.state.surreal,
        rt.state.bootstrap_owner_email.as_deref(),
    )
    .await
    {
        tracing::error!(%error, "bootstrap Owner reconciliation failed");
    }

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
    let dbos_schedules = dbos_schedules(&rt, host_dbos, lookup).await?;
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
    // drains its own graceful-shutdown signal independently — Ctrl-C and
    // `SIGTERM` both support more than one listener — so every bound port
    // stops serving together.
    let serve = async move {
        let mut servers = tokio::task::JoinSet::new();
        for listener in listeners {
            let router = router.clone();
            servers.spawn(async move {
                axum::serve(listener, router)
                    .with_graceful_shutdown(shutdown_signal())
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
    let served = if host_dbos {
        let lookup = |key: &str| std::env::var(key).ok();
        with_dbos_hosting(
            lookup,
            workflows::notify::notifier_from_lookup(lookup),
            dbos_schedules,
            serve,
        )
        .await
    } else {
        serve.await
    };
    telemetry_guard.shutdown();
    served
}

/// Build every optional DBOS schedule this host enabled. Each schedule has its
/// own flag, resolved before its context so an unused schedule never requires
/// its credentials or storage.
async fn dbos_schedules(
    rt: &HostRuntime,
    host_dbos: bool,
    lookup: impl Fn(&str) -> Option<String> + Copy,
) -> anyhow::Result<dbos_engine::Schedules> {
    let general_nag = if should_host_general_nag(host_dbos, lookup)? {
        Some(general_nag_context(rt).context("configure DBOS GeneralNag")?)
    } else {
        None
    };
    let dri_digest = if should_host_dri_digest(host_dbos, lookup)? {
        Some(dri_digest_context(rt, lookup).context("configure DBOS DriDigest")?)
    } else {
        None
    };
    let project_recovery = if should_host_project_recovery(host_dbos, lookup)? {
        Some(
            project_recovery_context(rt, lookup)
                .await
                .context("configure DBOS ProjectRecovery")?,
        )
    } else {
        None
    };
    let source_imports = if should_host_source_imports(host_dbos, lookup)? {
        let pg = store::postgres::PostgresDb::from_env()
            .await
            .context("connect PostgreSQL for source imports")?;
        Some(workflows::dbos_engine::source_imports::Context::new(
            Arc::new(
                workflows::dbos_engine::source_imports::StoreImportSource::new(
                    rt.state.surreal.clone(),
                    pg,
                    cloud::runtime_kms_from_env().await,
                ),
            ),
        ))
    } else {
        None
    };
    let retention_purge = if should_host_retention_purge(host_dbos, lookup)? {
        Some(
            retention_purge_context(rt, lookup)
                .await
                .context("configure DBOS RetentionPurge")?,
        )
    } else {
        None
    };
    let deadline_mirror = if should_host_deadline_mirror(host_dbos, lookup)? {
        Some(deadline_mirror_context(rt).context("configure DBOS DeadlineMirror")?)
    } else {
        None
    };
    let purchase_activation = if should_host_purchase_activation(host_dbos, lookup)? {
        let pg = store::postgres::PostgresDb::from_env()
            .await
            .context("connect PostgreSQL for purchase activation")?;
        Some(workflows::dbos_engine::purchase_activation::Context::new(
            Arc::new(crate::purchases::StoreActivationSource::new(
                pg,
                rt.state.billing_provider.clone(),
            )),
        ))
    } else {
        None
    };
    let billing_canary = if should_host_billing_canary(host_dbos, lookup)? {
        Some(
            workflows::dbos_engine::billing_canary::Context::from_lookup(lookup)
                .context("configure DBOS BillingCanary")?,
        )
    } else {
        None
    };
    let billing_digest = if should_host_billing_digest(host_dbos, lookup)? {
        Some(
            workflows::dbos_engine::billing_digest::Context::from_lookup(lookup)
                .context("configure DBOS BillingDigest")?,
        )
    } else {
        None
    };
    let notation_reset = if should_host_notation_reset(host_dbos, lookup)? {
        Some(notation_reset_context(rt, lookup).context("configure DBOS NotationReset")?)
    } else {
        None
    };
    Ok(dbos_engine::Schedules {
        general_nag,
        dri_digest,
        project_recovery,
        source_imports,
        retention_purge,
        deadline_mirror,
        purchase_activation,
        billing_canary,
        billing_digest,
        notation_reset,
    })
}

/// Resolve the independent BillingCanary gate.
fn should_host_billing_canary(
    host_dbos: bool,
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<bool> {
    if !host_dbos || !dbos_engine::enabled(&lookup).context("read NAVIGATOR_DBOS_ENABLED")? {
        return Ok(false);
    }
    workflows::dbos_engine::billing_canary::enabled(lookup)
        .context("read NAVIGATOR_DBOS_BILLING_CANARY_ENABLED")
}

/// Resolve the independent BillingDigest gate before constructing its context.
fn should_host_billing_digest(
    host_dbos: bool,
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<bool> {
    if !host_dbos || !dbos_engine::enabled(&lookup).context("read NAVIGATOR_DBOS_ENABLED")? {
        return Ok(false);
    }
    workflows::dbos_engine::billing_digest::enabled(lookup)
        .context("read NAVIGATOR_DBOS_BILLING_DIGEST_ENABLED")
}

/// Resolve the independent NotationReset gate before constructing its
/// context, so an unused schedule never requires Slack credentials.
fn should_host_notation_reset(
    host_dbos: bool,
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<bool> {
    if !host_dbos || !dbos_engine::enabled(&lookup).context("read NAVIGATOR_DBOS_ENABLED")? {
        return Ok(false);
    }
    workflows::dbos_engine::notation_reset::enabled(lookup)
        .context("read NAVIGATOR_DBOS_NOTATION_RESET_ENABLED")
}

fn notation_reset_context(
    rt: &HostRuntime,
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<workflows::dbos_engine::notation_reset::Context> {
    let simulated = store::sample_matters(rt.config.environment)
        .context("resolve simulated-matters deployment")?;
    workflows::dbos_engine::notation_reset::Context::from_lookup(
        rt.state.surreal.clone(),
        workflows::notation_reset::environment_label(rt.config.environment, simulated),
        simulated,
        lookup,
    )
    .context("validate NotationReset delivery configuration")
}

/// Resolve the independent PurchaseActivation gate.
fn should_host_purchase_activation(
    host_dbos: bool,
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<bool> {
    if !host_dbos || !dbos_engine::enabled(&lookup).context("read NAVIGATOR_DBOS_ENABLED")? {
        return Ok(false);
    }
    workflows::dbos_engine::purchase_activation::enabled(lookup)
        .context("read NAVIGATOR_DBOS_PURCHASE_ACTIVATION_ENABLED")
}

/// Resolve the independent RetentionPurge gate.
fn should_host_retention_purge(
    host_dbos: bool,
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<bool> {
    if !host_dbos || !dbos_engine::enabled(&lookup).context("read NAVIGATOR_DBOS_ENABLED")? {
        return Ok(false);
    }
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
            surreal: rt.state.surreal.clone(),
            storage: Arc::clone(&rt.state.storage),
            pg: Some(pg),
            archive: Some(archive),
            authority,
        }),
    ))
}

/// Resolve the independent DeadlineMirror gate.
fn should_host_deadline_mirror(
    host_dbos: bool,
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<bool> {
    if !host_dbos || !dbos_engine::enabled(&lookup).context("read NAVIGATOR_DBOS_ENABLED")? {
        return Ok(false);
    }
    workflows::dbos_engine::deadline_mirror::enabled(lookup)
        .context("read NAVIGATOR_DBOS_DEADLINE_MIRROR_ENABLED")
}

/// The nightly mirror's source: the store, the deployment's Workspace
/// calendar provider, and the canonical host for matter-page links.
fn deadline_mirror_context(
    rt: &HostRuntime,
) -> anyhow::Result<workflows::dbos_engine::deadline_mirror::Context> {
    Ok(workflows::dbos_engine::deadline_mirror::Context::new(
        Arc::new(crate::calendar_mirror::ScheduledMirror {
            surreal: rt.state.surreal.clone(),
            providers: Arc::clone(&rt.state.integration_providers),
            base_url: rt.state.canonical_host.host().map(str::to_string),
        }),
    ))
}

/// Resolve the independent SourceImport gate.
fn should_host_source_imports(
    host_dbos: bool,
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<bool> {
    if !host_dbos || !dbos_engine::enabled(&lookup).context("read NAVIGATOR_DBOS_ENABLED")? {
        return Ok(false);
    }
    workflows::dbos_engine::source_imports::enabled(lookup)
        .context("read NAVIGATOR_DBOS_SOURCE_IMPORT_ENABLED")
}

/// Resolve the independent ProjectRecovery gate before opening any archive
/// lane, so an unused schedule never requires its storage.
fn should_host_project_recovery(
    host_dbos: bool,
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<bool> {
    if !host_dbos || !dbos_engine::enabled(&lookup).context("read NAVIGATOR_DBOS_ENABLED")? {
        return Ok(false);
    }
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
                rt.state.surreal.clone(),
                pg,
                Arc::clone(&rt.state.storage),
                archive,
                source_backups,
            ),
        ),
    ))
}

fn dri_digest_context(
    rt: &HostRuntime,
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<workflows::dbos_engine::dri_digest::Context> {
    let simulated = store::sample_matters(rt.config.environment)
        .context("resolve simulated-matters deployment")?;
    workflows::dbos_engine::dri_digest::Context::from_lookup(
        rt.state.surreal.clone(),
        workflows::notify::notifier_from_lookup(&lookup),
        simulated,
        lookup,
    )
    .context("validate DriDigest delivery configuration")
}

/// Resolve the independent DriDigest gate before constructing a delivery
/// context, so an unused schedule never requires Slack credentials.
fn should_host_dri_digest(
    host_dbos: bool,
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<bool> {
    if !host_dbos || !dbos_engine::enabled(&lookup).context("read NAVIGATOR_DBOS_ENABLED")? {
        return Ok(false);
    }
    workflows::dbos_engine::dri_digest::enabled(lookup)
        .context("read NAVIGATOR_DBOS_DRI_DIGEST_ENABLED")
}

fn general_nag_context(
    rt: &HostRuntime,
) -> anyhow::Result<workflows::dbos_engine::general_nag::Context> {
    let lookup = |key: &str| std::env::var(key).ok();
    let simulated = store::sample_matters(rt.config.environment)
        .context("resolve simulated-matters deployment")?;
    workflows::dbos_engine::general_nag::Context::from_lookup(
        rt.state.surreal.clone(),
        simulated,
        lookup,
    )
    .context("validate GeneralNag delivery configuration")
}

/// Resolve the independent DBOS and GeneralNag gates before constructing the
/// delivery context. A disabled DBOS host must not validate an unused workflow
/// flag or require Slack credentials.
fn should_host_general_nag(
    host_dbos: bool,
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<bool> {
    if !host_dbos || !dbos_engine::enabled(&lookup).context("read NAVIGATOR_DBOS_ENABLED")? {
        return Ok(false);
    }
    workflows::dbos_engine::general_nag::enabled(lookup)
        .context("read NAVIGATOR_DBOS_GENERAL_NAG_ENABLED")
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
    use super::{
        bind_ports, should_host_billing_canary, should_host_deadline_mirror,
        should_host_dri_digest, should_host_general_nag, should_host_notation_reset,
        should_host_project_recovery, should_host_retention_purge, should_host_source_imports,
    };

    type Gate = fn(bool, &dyn Fn(&str) -> Option<String>) -> anyhow::Result<bool>;

    /// The schedules gated only by DBOS and their own flag.
    fn recovery_gates() -> [(Gate, &'static str); 5] {
        [
            (
                |host, lookup| should_host_project_recovery(host, lookup),
                workflows::dbos_engine::project_recovery::ENABLED_ENV,
            ),
            (
                |host, lookup| should_host_source_imports(host, lookup),
                workflows::dbos_engine::source_imports::ENABLED_ENV,
            ),
            (
                |host, lookup| should_host_retention_purge(host, lookup),
                workflows::dbos_engine::retention_purge::ENABLED_ENV,
            ),
            (
                |host, lookup| should_host_deadline_mirror(host, lookup),
                workflows::dbos_engine::deadline_mirror::ENABLED_ENV,
            ),
            (
                |host, lookup| should_host_notation_reset(host, lookup),
                workflows::dbos_engine::notation_reset::ENABLED_ENV,
            ),
        ]
    }

    /// A recovery-lane schedule runs only when this host serves DBOS, DBOS is
    /// on, and the schedule's own flag is on.
    #[test]
    fn recovery_schedules_require_every_host_gate() {
        for (gate, flag) in recovery_gates() {
            let all_on = |key: &str| {
                (key == workflows::dbos_engine::ENABLED_ENV || key == flag).then(|| "true".into())
            };
            assert!(gate(true, &all_on).unwrap(), "{flag}");
            assert!(!gate(false, &all_on).unwrap(), "{flag}");
            assert!(!gate(true, &|_| None).unwrap(), "{flag}");
            let schedule_off =
                |key: &str| (key == workflows::dbos_engine::ENABLED_ENV).then(|| "true".into());
            assert!(!gate(true, &schedule_off).unwrap(), "{flag}");
        }
    }

    /// With DBOS off, a schedule's malformed flag is never read; with DBOS on,
    /// a malformed flag, the schedule's or DBOS's own, fails the boot.
    #[test]
    fn recovery_schedule_flags_fail_closed() {
        for (gate, flag) in recovery_gates() {
            let dbos_off = |key: &str| {
                if key == workflows::dbos_engine::ENABLED_ENV {
                    Some("false".into())
                } else {
                    Some("typo".into())
                }
            };
            assert!(!gate(true, &dbos_off).unwrap(), "{flag}");
            let typo = |key: &str| {
                if key == workflows::dbos_engine::ENABLED_ENV {
                    Some("true".into())
                } else {
                    (key == flag).then(|| "typo".into())
                }
            };
            assert!(gate(true, &typo).is_err(), "{flag}");
            let dbos_typo =
                |key: &str| (key == workflows::dbos_engine::ENABLED_ENV).then(|| "typo".into());
            assert!(gate(true, &dbos_typo).is_err(), "{flag}");
        }
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

    #[test]
    fn general_nag_requires_both_host_gates() {
        let both_enabled = |key: &str| match key {
            workflows::dbos_engine::ENABLED_ENV
            | workflows::dbos_engine::general_nag::ENABLED_ENV => Some("true".into()),
            _ => None,
        };
        assert!(!should_host_general_nag(false, both_enabled).unwrap());
        assert!(should_host_general_nag(true, both_enabled).unwrap());
        assert!(!should_host_general_nag(true, |_| None).unwrap());
    }

    #[test]
    fn dri_digest_requires_both_host_gates() {
        let both_enabled = |key: &str| match key {
            workflows::dbos_engine::ENABLED_ENV
            | workflows::dbos_engine::dri_digest::ENABLED_ENV => Some("true".into()),
            _ => None,
        };
        assert!(!should_host_dri_digest(false, both_enabled).unwrap());
        assert!(should_host_dri_digest(true, both_enabled).unwrap());
        assert!(!should_host_dri_digest(true, |_| None).unwrap());
    }

    #[test]
    fn billing_canary_requires_both_host_gates_and_defaults_off() {
        let both_enabled = |key: &str| match key {
            workflows::dbos_engine::ENABLED_ENV
            | workflows::dbos_engine::billing_canary::ENABLED_ENV => Some("true".into()),
            _ => None,
        };
        assert!(!should_host_billing_canary(false, both_enabled).unwrap());
        assert!(should_host_billing_canary(true, both_enabled).unwrap());
        assert!(!should_host_billing_canary(true, |_| None).unwrap());
    }

    #[test]
    fn general_nag_gate_is_not_read_when_dbos_host_is_off() {
        let invalid_nag_flag = |key: &str| match key {
            workflows::dbos_engine::ENABLED_ENV => Some("false".into()),
            workflows::dbos_engine::general_nag::ENABLED_ENV => Some("typo".into()),
            _ => None,
        };
        assert!(!should_host_general_nag(true, invalid_nag_flag).unwrap());
        assert!(!should_host_general_nag(false, |_| Some("typo".into())).unwrap());
    }

    #[test]
    fn invalid_enabled_host_flags_fail_closed() {
        let invalid_dbos =
            |key: &str| (key == workflows::dbos_engine::ENABLED_ENV).then(|| "typo".into());
        assert!(should_host_general_nag(true, invalid_dbos).is_err());
        let invalid_nag = |key: &str| match key {
            workflows::dbos_engine::ENABLED_ENV => Some("true".into()),
            workflows::dbos_engine::general_nag::ENABLED_ENV => Some("typo".into()),
            _ => None,
        };
        assert!(should_host_general_nag(true, invalid_nag).is_err());
        let invalid_digest = |key: &str| match key {
            workflows::dbos_engine::ENABLED_ENV => Some("true".into()),
            workflows::dbos_engine::dri_digest::ENABLED_ENV => Some("typo".into()),
            _ => None,
        };
        assert!(should_host_dri_digest(true, invalid_digest).is_err());
        let invalid_billing_canary = |key: &str| match key {
            workflows::dbos_engine::ENABLED_ENV => Some("true".into()),
            workflows::dbos_engine::billing_canary::ENABLED_ENV => Some("typo".into()),
            _ => None,
        };
        assert!(should_host_billing_canary(true, invalid_billing_canary).is_err());
    }
}
