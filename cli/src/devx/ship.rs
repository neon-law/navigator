//! `devx ship` — one-shot "roll one configured deployment onto a tagged image".
//!
//! This is the deterministic, in-binary form of the production rollout
//! path documented in `docs/cloud-operations.md`. That public doc keeps
//! the prose rationale ("why each step, in this order"); this module is
//! the executable that runs the steps so an operator types one command
//! instead of pasting several shell blocks.
//!
//! **CI builds and publishes; ship only rolls.** The daily
//! `deploy.yml` tag flow builds service images and publishes them to
//! Artifact Registry under an immutable release tag; this module never
//! builds or pushes an image — it pins the running cluster to an
//! already-published tag.
//!
//! Two flows, matching the documented rollout path:
//!
//! - **Roll** (default): take an immutable release tag to deploy (required
//!   `--tag`) → confirm every image is published at that tag → render the
//!   embedded GKE tree with the deployer's `NAVIGATOR_*` values **and the
//!   tag** → confirm the prod Secret satisfies the new binary's boot
//!   invariants against that rendered tree → `kubectl apply -k` it → wait
//!   out the rollouts → re-register the worker with Restate → verify the
//!   public asset origin. Every service deployment lands on the **same** tag —
//!   never a version skew.
//!
//!   The order is the safety property, not an implementation detail.
//!   Everything before the apply is local or read-only, so an unsatisfied
//!   boot invariant aborts a ship that has changed nothing. And the apply
//!   carries the real tag rather than a placeholder a later `set image`
//!   corrects: `workflows-service` (`maxSurge: 0`) deletes the running pod
//!   before the replacement is ready, so a first write on an unpullable tag
//!   takes that tier down for however long the gap lasts. One write, one
//!   `ReplicaSet`.
//! - **No-rebuild restart** (`--restart-only`): after a Secret value
//!   was rotated, `kubectl rollout restart` service deployments that
//!   `envFrom` the Secret, so the pods re-read it (pods cache `envFrom` at
//!   start and never reload).
//!
//! Everything that varies per deployment comes from the repository's
//! `deployments/<name>/config.toml`, selected by the required
//! `--deployment` flag — there is no literal project ID, region, domain,
//! or registry prefix in this file, and no coordinate is ever read from
//! the process environment. A stale shell can therefore never select the
//! wrong deployment: the flag is the whole safety property. See
//! [`ShipConfig::from_deployment`].
//!
//! ## What this does NOT do
//!
//! - It never builds or pushes images. CI owns that; ship rolls a
//!   tag CI already published. The public GitHub `YY.M.D` tag is the
//!   source restore point, so there is no git-bundle archive step.
//! - It never auto-patches a prod Secret. The invariant check *aborts*
//!   with the exact `kubectl patch` to run when a required key is
//!   missing — generating and writing a prod secret silently is a
//!   judgment call left to the operator.
//!
//! ## Testing
//!
//! The shell-out orchestration needs a real cluster. Its completion boundary
//! is kept pure and injected with a verifier so the ordering and failure path
//! are covered alongside the other pure pieces — env-driven config, registry
//! image-URL formulas, required-key parser, missing-key diff, and
//! embedded-manifest render — by the `tests` module below.

#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, ensure, Context, Result};
use include_dir::{include_dir, Dir};
use tempfile::TempDir;

use portal::inbound_email::NAVIGATOR_SUMMARY_ENVELOPE_RECIPIENTS;
use portal::twilio_sms::NAVIGATOR_SMS_NUMBER;
use store::NAVIGATOR_SIMULATED_MATTERS;
use views::brand::BrandKey;
use workflows::email_summary_config::{
    CHANNEL_ENV as SUMMARY_CHANNEL_ENV, DEPLOYMENT_ENV as SUMMARY_DEPLOYMENT_ENV,
    ENABLED_ENV as SUMMARY_ENABLED_ENV,
};

use super::registry;
use super::{require_auth, require_tools, run};

/// In-cluster Deployment + container names. These are workspace
/// conventions (the GKE overlay names them); they are not per-deploy
/// configuration, so they stay as constants rather than env vars.
const WEB_DEPLOYMENT: &str = "navigator-web";
/// The only publishable web images. A deployment chooses its immutable brand
/// image in its `config.toml`; no runtime flag can change the public face.
const BRAND_IMAGES: &[&str] = &["neon-server"];
const WORKFLOWS_DEPLOYMENT: &str = "workflows-service";

/// The GKE manifest tree, embedded at compile time. `ship` renders it —
/// substituting the deployer's `NAVIGATOR_*` values for the placeholder
/// tokens — into a throwaway temp dir at ship time, then `kubectl apply
/// -k`s it. Embedding the whole *directory* (not a per-file `include_str!`
/// list) is what keeps the manifests out of the operator's hands — only the
/// `deployments/` tree of the repository checkout is read at ship time — and
/// means a manifest added to the tree later is bundled automatically. The kustomize
/// root at `kustomization.yaml` references both `../exports` and
/// `../../../../k8s/base`, so their embedded trees are extracted alongside it
/// at the same relative offsets. This is the CLI-generated form of what the
/// deployer-private overlay used to hand-carry (see `docs/gke-prod.md`).
static GKE_MANIFESTS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../examples/deploy/k8s/gke");

/// Scheduled-job manifests referenced by the GKE overlay through `../exports`.
/// They must travel with the embedded GKE tree: `ops ship` renders into a
/// temporary directory and cannot fall back to the operator's checkout.
static EXPORTS_MANIFESTS: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../examples/deploy/k8s/exports");

/// The shared platform base (namespace and application workloads) the GKE
/// kustomization pulls in via `../../../../k8s/base`.
static K8S_BASE_MANIFESTS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../k8s/base");

/// PostgreSQL service manifests referenced by the embedded GKE PostgreSQL
/// overlays. Keep the release temp tree self-contained for `kubectl kustomize`.
static POSTGRES_MANIFESTS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../k8s/postgres");

/// Forgejo service manifests referenced by the embedded GKE Forgejo overlays.
static FORGEJO_MANIFESTS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../k8s/forgejo");

/// The kustomize root inside the rendered temp dir — the path `kubectl -k`
/// targets. Mirrors the manifests' own repo-relative layout so the base's
/// `../../../../k8s/base` reference resolves.
const GKE_KUSTOMIZE_SUBPATH: &str = "examples/deploy/k8s/gke";
const EXPORTS_KUSTOMIZE_SUBPATH: &str = "examples/deploy/k8s/exports";

/// Boot-required keys that ship as inline Deployment env / the
/// `navigator-otel-env` `ConfigMap` rather than the deployment Secret, so the
/// `SecretProviderClass` is not expected to declare them and no deployment's
/// `deployments/<name>/` tree needs to carry them. Everything else in
/// `store::deployment::WEB_REQUIREMENTS` rides the Secret rail.
pub(super) const INLINE_ENV_WEB_KEYS: &[&str] = &[
    "NAVIGATOR_CLAMD_ADDR",                // inline env → the ClamAV service
    "NAVIGATOR_STORAGE_BACKEND",           // inline env
    "NAVIGATOR_APPLICATIONS_BUCKET",       // inline env → the applications bucket coordinate
    "NAVIGATOR_EMAIL_BACKEND",             // inline env
    "GOOGLE_OAUTH_CLIENT_IDS",             // inline env allowlist
    "RESTATE_IDENTITY_KEY",                // public Restate Cloud request key
    "NAVIGATOR_SOCIAL_OAUTH_REDIRECT_URI", // inline env → the social OAuth callback
    store::deployment::GITHUB_AUTOMATION_HOME_FLAG,
    store::deployment::SMS_TEST_RECIPIENTS_FLAG,
];

/// The Secret Manager object keys listed by a `SecretProviderClass`.
///
/// `ops secrets apply` writes these values to both Secret Manager and the
/// Kubernetes Secret consumed by `web`. The class retains the single catalog
/// for the Secret Manager preflight; the pod contract is validated separately
/// against the `envFrom` reference in `web-env.yaml`.
pub(super) fn spc_projected_keys(spc: &str) -> BTreeSet<String> {
    let doc: serde_json::Value = serde_yaml::from_str(spc).expect("parse SecretProviderClass yaml");
    // `parameters.secrets` is a YAML block scalar (a string) listing each
    // Secret Manager object and the in-volume `path` it lands at.
    let secrets_block = doc
        .pointer("/spec/parameters/secrets")
        .and_then(serde_json::Value::as_str)
        .expect("SecretProviderClass has spec.parameters.secrets");
    serde_yaml::from_str::<serde_json::Value>(secrets_block)
        .expect("parse parameters.secrets block")
        .as_array()
        .expect("parameters.secrets is a list")
        .iter()
        .filter_map(|entry| entry.get("path").and_then(serde_json::Value::as_str))
        .map(ToString::to_string)
        .collect()
}

/// The Secret Manager object keys the shipped catalog lists. The object names
/// are deployment-independent; only the project prefix is substituted at
/// render.
pub(super) fn secret_provider_class_keys() -> BTreeSet<String> {
    let spc = GKE_MANIFESTS
        .get_file("secrets/secret-provider-class.yaml")
        .and_then(include_dir::File::contents_utf8)
        .expect("embedded SecretProviderClass manifest");
    spc_projected_keys(spc)
}

/// Where [`K8S_BASE_MANIFESTS`] is extracted, relative to the temp-dir root.
const K8S_BASE_SUBPATH: &str = "k8s/base";

/// One placeholder → real-value substitution the render applies to every
/// embedded manifest file. `token` is the literal string in the placeholder
/// base (`YOUR_PROJECT_ID`, `your-domain.example`, …); `value` is the
/// deployer's real value resolved from its `env` var.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Substitution {
    token: &'static str,
    env: &'static str,
    value: String,
}

const SLACK_CHANNEL_SUBSTITUTIONS: &[(&str, &str)] = &[
    (
        "YOUR_SLACK_MESSAGES_CHANNEL_ID",
        "SLACK_MESSAGES_CHANNEL_ID",
    ),
    (
        "YOUR_SLACK_STAGING_MESSAGES_CHANNEL_ID",
        "SLACK_STAGING_MESSAGES_CHANNEL_ID",
    ),
];

fn slack_channel_substitutions<F>(deployment: &str, get: &F) -> Result<Vec<Substitution>>
where
    F: Fn(&str) -> Option<String>,
{
    SLACK_CHANNEL_SUBSTITUTIONS
        .iter()
        .map(|&(token, env)| required_substitution(deployment, token, env, get))
        .collect()
}

fn forgejo_coordinate_substitutions<F>(deployment: &str, get: &F) -> Result<Vec<Substitution>>
where
    F: Fn(&str) -> Option<String>,
{
    [
        ("YOUR_FORGEJO_OIDC_NAME", "NAVIGATOR_FORGEJO_OIDC_NAME"),
        (
            "YOUR_FORGEJO_OIDC_DISCOVERY_URL",
            "NAVIGATOR_FORGEJO_OIDC_DISCOVERY_URL",
        ),
        (
            "YOUR_FORGEJO_ADMISSION_DOMAINS",
            "NAVIGATOR_FORGEJO_ADMISSION_DOMAINS",
        ),
    ]
    .iter()
    .map(|&(token, env)| required_substitution(deployment, token, env, get))
    .chain([forgejo_host_substitution(deployment, get)])
    .collect()
}

/// The placeholder for Forgejo's public host on the shared Ingress and its
/// `ManagedCertificate`.
const FORGEJO_HOST_TOKEN: &str = "NAVIGATOR_FORGEJO_HOST";

/// Render [`FORGEJO_HOST_TOKEN`] from `NAVIGATOR_FORGEJO_ROOT_URL` — the URL
/// Forgejo already advertises — rather than a second coordinate that could
/// disagree with it. Only a bare `https://<host>/` is accepted: the load
/// balancer terminates TLS on 443 and routes the whole host, so a port or a
/// sub-path would put Forgejo's links somewhere the Ingress does not serve.
fn forgejo_host_substitution<F>(deployment: &str, get: &F) -> Result<Substitution>
where
    F: Fn(&str) -> Option<String>,
{
    const ROOT_URL: &str = "NAVIGATOR_FORGEJO_ROOT_URL";
    let root_url = required_coordinate(deployment, ROOT_URL, get)?;
    let host = root_url
        .strip_prefix("https://")
        .and_then(|rest| rest.strip_suffix('/'))
        .filter(|host| valid_admission_domains(host) && !host.contains(','));
    let Some(host) = host else {
        bail!(
            "{ROOT_URL} in deployments/{deployment}/config.toml must be `https://<host>/` \
             with no port or path (got `{root_url}`) — its host becomes Forgejo's Ingress \
             rule and certificate"
        );
    };
    Ok(Substitution {
        token: FORGEJO_HOST_TOKEN,
        env: ROOT_URL,
        value: host.to_owned(),
    })
}

fn optional_release_substitutions<F>(deployment: &str, get: &F) -> Vec<Substitution>
where
    F: Fn(&str) -> Option<String>,
{
    let mut substitutions = vec![
        Substitution {
            token: "YOUR_STAGING",
            env: store::config::NAVIGATOR_STAGING,
            value: get(store::config::NAVIGATOR_STAGING).unwrap_or_else(|| "false".to_string()),
        },
        Substitution {
            token: "YOUR_SIMULATED_MATTERS",
            env: NAVIGATOR_SIMULATED_MATTERS,
            value: non_empty_env(NAVIGATOR_SIMULATED_MATTERS, get)
                .unwrap_or_else(|| "false".to_string()),
        },
        Substitution {
            token: "YOUR_SUMMARY_ENVELOPE_RECIPIENTS",
            env: NAVIGATOR_SUMMARY_ENVELOPE_RECIPIENTS,
            value: non_empty_env(NAVIGATOR_SUMMARY_ENVELOPE_RECIPIENTS, get).unwrap_or_default(),
        },
        Substitution {
            token: "YOUR_SMS_NUMBER",
            env: NAVIGATOR_SMS_NUMBER,
            value: non_empty_env(NAVIGATOR_SMS_NUMBER, get).unwrap_or_default(),
        },
        Substitution {
            token: "YOUR_VPC_NAME",
            env: "NAVIGATOR_VPC_NAME",
            value: non_empty_env("NAVIGATOR_VPC_NAME", get)
                .unwrap_or_else(|| super::gcp::network::DEFAULT_NETWORK_NAME.to_string()),
        },
    ];
    substitutions.extend(summary_substitutions(deployment, get));
    substitutions.extend(optional_provider_substitutions(get));
    substitutions
}

/// Manifest tokens and the required `config.toml` coordinates that fill them.
/// `ops ship` renders each as plaintext, so none may also be an encrypted key
/// in `deployments/sops-inventory.yaml`.
const PLAINTEXT_SUBSTITUTIONS: &[(&str, &str)] = &[
    ("YOUR_PROJECT_ID", "NAVIGATOR_GCP_PROJECT_ID"),
    ("YOUR_GITHUB_ORG", "NAVIGATOR_GITHUB_ORG"),
    // NOTE: the registry path in every `image:` line renders from
    // IMAGES_PROJECT_TOKEN below, NOT from this one. `YOUR_PROJECT_ID`
    // names the project that holds the buckets and the GSA — which is not
    // where the images live.
    ("NAVIGATOR_WEB_IMAGE", "NAVIGATOR_WEB_IMAGE"),
    ("NAVIGATOR_PUBLIC_HOST", "NAVIGATOR_PUBLIC_HOST"),
    ("NAVIGATOR_WORKFLOWS_HOST", "NAVIGATOR_WORKFLOWS_HOST"),
    ("YOUR_RESTATE_IDENTITY_KEY", "RESTATE_IDENTITY_KEY"),
    ("YOUR_DOCUMENTS_BUCKET", "NAVIGATOR_DOCUMENTS_BUCKET"),
    ("YOUR_APPLICATIONS_BUCKET", "NAVIGATOR_APPLICATIONS_BUCKET"),
    ("YOUR_ASSETS_BUCKET", "NAVIGATOR_ASSETS_BUCKET"),
    // The token is `YOUR_ASSET_BASE_URL`, not the key itself: the
    // manifest carries the key as an env *name* on the line above the
    // value, and substitution is a plain string replace, so a token
    // spelled `NAVIGATOR_ASSET_BASE_URL` would rewrite the name too.
    ("YOUR_ASSET_BASE_URL", "NAVIGATOR_ASSET_BASE_URL"),
    ("YOUR_EXPORTS_BUCKET", "NAVIGATOR_EXPORTS_BUCKET"),
    ("YOUR_ARCHIVES_BUCKET", "NAVIGATOR_ARCHIVES_BUCKET"),
    ("NAVIGATOR_GATEWAY_IP_NAME", "NAVIGATOR_GATEWAY_IP_NAME"),
    (
        "NAVIGATOR_GCP_SERVICE_ACCOUNT_ID",
        "NAVIGATOR_GCP_SERVICE_ACCOUNT_ID",
    ),
    ("navigator-web-secrets", "NAVIGATOR_WEB_SECRET_NAME"),
    ("YOUR_GOOGLE_OAUTH_REQUIRED_HD", "GOOGLE_OAUTH_REQUIRED_HD"),
    ("YOUR_SLACK_GENERAL_CHANNEL_ID", "SLACK_GENERAL_CHANNEL_ID"),
    ("YOUR_SLACK_FINANCE_CHANNEL_ID", "SLACK_FINANCE_CHANNEL_ID"),
    // The GCP region, for the resources that still live in GCP (GKE,
    // buckets, KMS). No longer part of an image reference: those come from
    // Artifact Registry, addressed by the one registry prefix.
    ("YOUR_GCP_REGION", "NAVIGATOR_GCP_LOCATION"),
    (
        "YOUR_OAUTH_CLIENT_ID_BROWSER",
        "NAVIGATOR_OAUTH_CLIENT_ID_BROWSER",
    ),
    (
        "YOUR_BOOTSTRAP_OWNER_EMAIL",
        "NAVIGATOR_BOOTSTRAP_OWNER_EMAIL",
    ),
];

/// The substitution table `ship` resolves from the selected deployment's
/// `deployments/<name>/config.toml` before rendering. Every entry is
/// REQUIRED — a missing or blank coordinate bails naming both the
/// placeholder and the key, so a half-substituted manifest never reaches
/// the cluster (mirrors [`require_asset_base_url`]). These are exactly the
/// values the deployer-private overlay used to substitute (`docs/gke-prod.md`
/// §"The private overlay"): the GCP project (buckets / GSA), the primary
/// domain (www / workflows / HD / redirect URI / public
/// base URL), and the required browser OAuth client ID.
///
/// Takes a getter closure over the deployment's coordinates, exactly like
/// `portal::config::enforce_deployment_invariants`, so the failure message
/// is unit-tested against an in-memory map.
fn resolve_substitutions_for_deployment<F>(
    deployment: &str,
    tag: &str,
    get: F,
) -> Result<Vec<Substitution>>
where
    F: Fn(&str) -> Option<String>,
{
    store::config::staging_from(&get).map_err(|_| {
        anyhow::anyhow!("NAVIGATOR_STAGING must be exactly true or false when configured")
    })?;
    let mut substitutions = base_substitutions(deployment, tag, &get)?;
    substitutions.extend(
        PLAINTEXT_SUBSTITUTIONS
            .iter()
            .map(|&(token, env)| required_substitution(deployment, token, env, &get))
            .collect::<Result<Vec<_>>>()?,
    );
    substitutions.extend(slack_channel_substitutions(deployment, &get)?);
    substitutions.extend(forgejo_coordinate_substitutions(deployment, &get)?);
    substitutions.extend(optional_release_substitutions(deployment, &get));
    substitutions.extend(capability_env_substitutions(deployment, &get)?);
    substitutions.push(web_patch_substitution(
        deployment,
        &get,
        &POSTGRES_WEB_PATCH,
    )?);
    substitutions.push(web_patch_substitution(deployment, &get, &DBOS_WEB_PATCH)?);
    substitutions.extend(dbos_schedule_substitutions(deployment, &get)?);
    substitutions.extend(replica_substitutions(deployment, &get)?);
    substitutions.extend(mirror_substitutions(&get)?);
    // Not in PLAINTEXT_SUBSTITUTIONS and not read from `get` at all: the additional brand
    // hosts this deployment's environment serves come from the compiled
    // `views::brand::BrandKey` registry, keyed off the `NAVIGATOR_PUBLIC_HOST`
    // value PLAINTEXT_SUBSTITUTIONS already resolved above — never a second hand-maintained
    // coordinate that could drift from the registry.
    let public_host = substitutions
        .iter()
        .find(|substitution| substitution.token == "NAVIGATOR_PUBLIC_HOST")
        .expect("NAVIGATOR_PUBLIC_HOST is a required PLAINTEXT_SUBSTITUTIONS entry")
        .value
        .clone();
    let namespace = substitutions
        .iter()
        .find(|substitution| substitution.token == "namespace: navigator")
        .and_then(|substitution| substitution.value.strip_prefix("namespace: "))
        .map(str::to_owned)
        .expect("namespace substitution is required");
    let brand_bindings = additional_brand_bindings(&public_host);
    let brand_hosts = additional_brand_hosts(&public_host);
    substitutions.push(Substitution {
        token: BRAND_MANAGED_CERTIFICATES_TOKEN,
        env: "views::brand::BrandKey",
        value: brand_managed_certificate_yaml(&namespace, &public_host, &brand_bindings),
    });
    substitutions.push(Substitution {
        token: BRAND_CERT_KUSTOMIZE_TOKEN,
        env: "views::brand::BrandKey",
        value: brand_cert_kustomize_resource(),
    });
    substitutions.push(Substitution {
        token: MANAGED_CERTIFICATES_ANNOTATION_TOKEN,
        env: "views::brand::BrandKey",
        value: managed_certificates_annotation(&brand_bindings),
    });
    substitutions.push(Substitution {
        token: BRAND_INGRESS_RULES_TOKEN,
        env: "views::brand::BrandKey",
        value: brand_ingress_rule_lines(&brand_hosts),
    });
    Ok(substitutions)
}

/// Render the validated deployment capabilities into the web container so
/// its boot requirements use the same scope as `ops ship` preflight. Both
/// flags are always explicit strings, including `false` for an unmarked row.
fn capability_env_substitutions<F>(deployment: &str, get: &F) -> Result<Vec<Substitution>>
where
    F: Fn(&str) -> Option<String>,
{
    let capabilities = store::deployment::DeploymentCapabilities::from_lookup(get)
        .with_context(|| format!("parse capability flags for {deployment}"))?;
    Ok([
        (
            store::deployment::GITHUB_AUTOMATION_HOME_FLAG,
            capabilities.github_automation_home,
        ),
        (
            store::deployment::SMS_TEST_RECIPIENTS_FLAG,
            capabilities.sms_test_recipients_home,
        ),
    ]
    .into_iter()
    .map(|(key, enabled)| Substitution {
        token: if key == store::deployment::GITHUB_AUTOMATION_HOME_FLAG {
            "            # NAVIGATOR_CAPABILITY_ENV NAVIGATOR_GITHUB_AUTOMATION_HOME\n"
        } else {
            "            # NAVIGATOR_CAPABILITY_ENV NAVIGATOR_SMS_TEST_RECIPIENTS_DEPLOYMENT\n"
        },
        env: key,
        value: format!("            - name: {key}\n              value: \"{enabled}\"\n"),
    })
    .collect())
}

/// The deployment coordinate that opts `web` into its PostgreSQL connection.
const NAVIGATOR_POSTGRES_ENABLED: &str = "NAVIGATOR_POSTGRES_ENABLED";

const MIRROR_RESOURCE_TOKEN: &str = "  # NAVIGATOR_POSTGRES_MIRROR_RESOURCE\n";
const MIRROR_ENV_TOKEN: &str = "            # NAVIGATOR_POSTGRES_MIRROR_ENV\n";

fn mirror_substitutions(get: &impl Fn(&str) -> Option<String>) -> Result<Vec<Substitution>> {
    let enabled = match non_empty_env(store::postgres_mirror::MIRROR_ENV, get).as_deref() {
        None | Some("0") => false,
        Some("1") => true,
        Some(_) => bail!("NAVIGATOR_POSTGRES_MIRROR must be \"1\" or \"0\""),
    };
    ensure!(
        !enabled || non_empty_env(NAVIGATOR_POSTGRES_ENABLED, get).as_deref() == Some("true"),
        "NAVIGATOR_POSTGRES_MIRROR=1 requires NAVIGATOR_POSTGRES_ENABLED=true"
    );
    Ok(vec![
        Substitution {
            token: MIRROR_RESOURCE_TOKEN,
            env: store::postgres_mirror::MIRROR_ENV,
            value: if enabled {
                "  - ../../../../k8s/postgres/mirror/recovery\n".into()
            } else {
                String::new()
            },
        },
        Substitution {
            token: MIRROR_ENV_TOKEN,
            env: store::postgres_mirror::MIRROR_ENV,
            value: if enabled {
                "            - name: NAVIGATOR_POSTGRES_MIRROR\n              value: \"1\"\n".into()
            } else {
                String::new()
            },
        },
    ])
}

/// The whole-line placeholder in `kustomization.yaml` that becomes the
/// PostgreSQL `web` patch for an opted-in deployment. It sits after the
/// `web-env.yaml` entry, whose `$patch: replace` would otherwise drop the
/// connection variables.
const POSTGRES_WEB_PATCH_TOKEN: &str = "  # NAVIGATOR_POSTGRES_WEB_PATCH\n";

/// The deployment coordinate that hosts the DBOS executor inside `web`.
const NAVIGATOR_DBOS_ENABLED: &str = "NAVIGATOR_DBOS_ENABLED";

/// The whole-line placeholder in `kustomization.yaml` that becomes the DBOS
/// `web` patch for an opted-in deployment, also after `web-env.yaml`.
const DBOS_WEB_PATCH_TOKEN: &str = "  # NAVIGATOR_DBOS_WEB_PATCH\n";

/// One opt-in `web` patch: the `config.toml` switch, the placeholder line it
/// replaces, and the patch file it renders.
struct WebPatch {
    env: &'static str,
    token: &'static str,
    path: &'static str,
}

const POSTGRES_WEB_PATCH: WebPatch = WebPatch {
    env: NAVIGATOR_POSTGRES_ENABLED,
    token: POSTGRES_WEB_PATCH_TOKEN,
    path: "postgres/web/web-postgres.yaml",
};

const DBOS_WEB_PATCH: WebPatch = WebPatch {
    env: NAVIGATOR_DBOS_ENABLED,
    token: DBOS_WEB_PATCH_TOKEN,
    path: "postgres/web/web-dbos.yaml",
};

/// A daily workflow the DBOS executor in `web` can host in place of its
/// Restate trigger `CronJob`. Its `config.toml` switch, the `CronJob` it
/// retires, that `CronJob`'s line in the exports `kustomization.yaml` (which
/// doubles as the placeholder `ops ship` drops when the switch is on), the
/// `config.toml` coordinates rendered into `web` beside the switch, and the
/// row secret `web` refuses to boot without.
struct DbosSchedule {
    env: &'static str,
    cronjob: &'static str,
    resource: &'static str,
    coordinates: &'static [&'static str],
    secret: Option<&'static str>,
}

const DBOS_SCHEDULES: [DbosSchedule; 4] = [
    DbosSchedule {
        env: "NAVIGATOR_DBOS_GENERAL_NAG_ENABLED",
        cronjob: "general-nag",
        resource: "  - cron-general-nag.yaml\n",
        coordinates: &["SLACK_GENERAL_CHANNEL_ID"],
        secret: None,
    },
    DbosSchedule {
        env: "NAVIGATOR_DBOS_DRI_DIGEST_ENABLED",
        cronjob: "dri-digest-trigger",
        resource: "  - cron-dri-digest-trigger.yaml\n",
        coordinates: &[],
        secret: Some(SLACK_WEBHOOK_URL),
    },
    DbosSchedule {
        env: "NAVIGATOR_DBOS_BILLING_DIGEST_ENABLED",
        cronjob: "billing-digest-trigger",
        resource: "  - cron-billing-digest-trigger.yaml\n",
        coordinates: &["BILLING_EXPORT_TABLE", "BIGQUERY_PROJECT"],
        secret: Some(SLACK_WEBHOOK_URL),
    },
    DbosSchedule {
        env: "NAVIGATOR_DBOS_NOTATION_RESET_ENABLED",
        cronjob: "notation-reset-trigger",
        resource: "  - cron-notation-reset-trigger.yaml\n",
        coordinates: &[
            "SLACK_MESSAGES_CHANNEL_ID",
            "SLACK_STAGING_MESSAGES_CHANNEL_ID",
        ],
        secret: None,
    },
];

/// The whole-line placeholder in `web-dbos.yaml` that becomes the schedule
/// switches and the coordinates switched-on schedules read.
const DBOS_SCHEDULE_ENV_TOKEN: &str = "            # NAVIGATOR_DBOS_SCHEDULE_ENV\n";

/// The incoming webhook `DriDigest` on DBOS refuses to boot without.
const SLACK_WEBHOOK_URL: &str = "SLACK_WEBHOOK_URL";

/// Each schedule's switch, read as `"true"` or `"false"` (absent is off), in
/// [`DBOS_SCHEDULES`] order. A schedule on DBOS needs the executor, so a
/// switch without `NAVIGATOR_DBOS_ENABLED = "true"` is refused rather than
/// left inert while its Restate `CronJob` is gone.
fn dbos_schedule_flags<F>(deployment: &str, get: &F) -> Result<Vec<bool>>
where
    F: Fn(&str) -> Option<String>,
{
    let mut flags = Vec::with_capacity(DBOS_SCHEDULES.len());
    for schedule in &DBOS_SCHEDULES {
        let flag = match non_empty_env(schedule.env, get).as_deref() {
            None | Some("false") => false,
            Some("true") => true,
            Some(_) => bail!(
                "{} in deployments/{deployment}/config.toml must be \"true\" or \"false\"",
                schedule.env
            ),
        };
        ensure!(
            !flag || non_empty_env(NAVIGATOR_DBOS_ENABLED, get).as_deref() == Some("true"),
            "{} requires {NAVIGATOR_DBOS_ENABLED} = \"true\" in deployments/{deployment}/config.toml",
            schedule.env
        );
        flags.push(flag);
    }
    Ok(flags)
}

/// Move each switched-on schedule from Restate to DBOS in one render: its
/// switch and coordinates reach `web`, and its trigger `CronJob` leaves the
/// exports tree.
fn dbos_schedule_substitutions<F>(deployment: &str, get: &F) -> Result<Vec<Substitution>>
where
    F: Fn(&str) -> Option<String>,
{
    use std::fmt::Write as _;
    let flags = dbos_schedule_flags(deployment, get)?;
    let mut env = String::new();
    let mut rendered: Vec<&str> = Vec::new();
    let mut substitutions = Vec::new();
    for (enabled, schedule) in flags.into_iter().zip(&DBOS_SCHEDULES) {
        let _ = write!(
            env,
            "            - name: {}\n              value: \"{enabled}\"\n",
            schedule.env
        );
        if !enabled {
            continue;
        }
        substitutions.push(Substitution {
            token: schedule.resource,
            env: schedule.env,
            value: String::new(),
        });
        for key in schedule.coordinates {
            let value = non_empty_env(key, get).with_context(|| {
                format!(
                    "{} requires {key} in deployments/{deployment}/config.toml",
                    schedule.env
                )
            })?;
            if !rendered.contains(key) {
                rendered.push(key);
                let _ = write!(
                    env,
                    "            - name: {key}\n              value: \"{value}\"\n"
                );
            }
        }
    }
    substitutions.push(Substitution {
        token: DBOS_SCHEDULE_ENV_TOKEN,
        env: NAVIGATOR_DBOS_ENABLED,
        value: env,
    });
    Ok(substitutions)
}

/// The secret each switched-on schedule needs before `web` can boot with it.
fn require_dbos_schedule_prerequisites(deployment: &super::deployments::Deployment) -> Result<()> {
    let flags = dbos_schedule_flags(&deployment.name, &|key: &str| {
        deployment.coordinates.get(key).cloned()
    })?;
    for (enabled, schedule) in flags.into_iter().zip(&DBOS_SCHEDULES) {
        if let (true, Some(secret)) = (enabled, schedule.secret) {
            ensure!(
                deployment.encrypted_keys.contains(secret),
                "{} requires {secret} in deployments/{}/secrets.enc.yaml: its DBOS workflow \
                 refuses to boot without it",
                schedule.env,
                deployment.name
            );
        }
    }
    Ok(())
}

/// Trigger `CronJob`s no render produces any more, whose workflow now runs only
/// on DBOS. Every roll deletes them, because `kubectl apply` never prunes a
/// `CronJob` an earlier release created.
const RETIRED_CRONJOBS: [&str; 1] = ["heartbeat-trigger"];

/// Every trigger `CronJob` this roll deletes: each switched-on DBOS schedule's
/// Restate trigger, then every [`RETIRED_CRONJOBS`] entry.
fn retired_cronjobs(deployment: &super::deployments::Deployment) -> Result<Vec<&'static str>> {
    let flags = dbos_schedule_flags(&deployment.name, &|key: &str| {
        deployment.coordinates.get(key).cloned()
    })?;
    Ok(flags
        .into_iter()
        .zip(&DBOS_SCHEDULES)
        .filter_map(|(enabled, schedule)| enabled.then_some(schedule.cronjob))
        .chain(RETIRED_CRONJOBS)
        .collect())
}

/// `kubectl apply` never prunes, so a trigger `CronJob` the render dropped
/// would keep starting its Restate workflow beside the DBOS schedule and post
/// twice. Delete each retired one after the new `web` is applied.
fn retire_dbos_hosted_cronjobs(
    cfg: &ShipConfig,
    dry_run: bool,
    deployment: &super::deployments::Deployment,
) -> Result<()> {
    for cronjob in retired_cronjobs(deployment)? {
        exec(
            dry_run,
            kubectl(cfg)
                .arg("delete")
                .arg("cronjob")
                .arg(cronjob)
                .arg("--ignore-not-found=true"),
        )?;
    }
    Ok(())
}

/// Render one [`WebPatch`]. Absent or `false` keeps the rendered tree
/// byte-identical to a deployment without it; any other value is refused so a
/// typo cannot silently leave `web` on the old path.
fn web_patch_substitution<F>(deployment: &str, get: &F, patch: &WebPatch) -> Result<Substitution>
where
    F: Fn(&str) -> Option<String>,
{
    let value = match non_empty_env(patch.env, get).as_deref() {
        None | Some("false") => String::new(),
        Some("true") => format!(
            "  - path: {}\n    target:\n      kind: Deployment\n      name: navigator-web\n",
            patch.path
        ),
        Some(_) => bail!(
            "{} in deployments/{deployment}/config.toml must be \"true\" or \"false\"",
            patch.env
        ),
    };
    Ok(Substitution {
        token: patch.token,
        env: patch.env,
        value,
    })
}

/// The whole-line placeholder in the GKE `kustomization.yaml` `patches:` list
/// that becomes the replica-count patches for a two-replica row.
const REPLICAS_PATCH_TOKEN: &str = "  # NAVIGATOR_REPLICAS_PATCH\n";

/// The whole-line placeholder in the GKE `kustomization.yaml` `resources:`
/// list that becomes the `PodDisruptionBudget`s for a two-replica row.
const DISRUPTION_BUDGETS_RESOURCE_TOKEN: &str = "  # NAVIGATOR_DISRUPTION_BUDGETS_RESOURCE\n";

/// The Deployments whose replica count follows the row's profile.
const REPLICATED_DEPLOYMENTS: [&str; 2] = [WEB_DEPLOYMENT, WORKFLOWS_DEPLOYMENT];

/// How many pods each [`REPLICATED_DEPLOYMENTS`] entry runs on this row.
///
/// The cluster setup is the same on every row; only the count differs. A row
/// whose matters are simulated (`NAVIGATOR_SIMULATED_MATTERS = "true"`, the
/// persistent staging row) runs one pod, and a row over real matters
/// (`"false"`) runs two. Unset or empty reads as `"false"`, the same default
/// the manifests' `NAVIGATOR_SIMULATED_MATTERS` value renders, so a row never
/// runs a single pod by omission. Any other value is refused before render.
fn replicas_for_row<F>(deployment: &str, get: &F) -> Result<u8>
where
    F: Fn(&str) -> Option<String>,
{
    match non_empty_env(NAVIGATOR_SIMULATED_MATTERS, get).as_deref() {
        Some("true") => Ok(1),
        None | Some("false") => Ok(2),
        Some(_) => bail!(
            "{NAVIGATOR_SIMULATED_MATTERS} in deployments/{deployment}/config.toml must be \
             \"true\" or \"false\""
        ),
    }
}

/// The replica-count patches and the `PodDisruptionBudget` resource for this
/// row. One replica leaves both placeholders empty, so the render is the
/// committed overlay's one pod per tier and no budget (a `minAvailable: 1`
/// budget over one pod would block every node drain). Two replicas patch both
/// Deployments to two and add `disruption-budgets.yaml`, which keeps one pod
/// of each tier up through a voluntary eviction.
fn replica_substitutions<F>(deployment: &str, get: &F) -> Result<Vec<Substitution>>
where
    F: Fn(&str) -> Option<String>,
{
    use std::fmt::Write as _;
    let replicas = replicas_for_row(deployment, get)?;
    let (patches, budgets) = if replicas == 1 {
        (String::new(), String::new())
    } else {
        let patches = REPLICATED_DEPLOYMENTS
            .iter()
            .fold(String::new(), |mut out, name| {
                let _ = write!(
                    out,
                    "  - target:\n      kind: Deployment\n      name: {name}\n    patch: |\n      \
                 - op: replace\n        path: /spec/replicas\n        value: {replicas}\n"
                );
                out
            });
        (patches, "  - disruption-budgets.yaml\n".to_string())
    };
    Ok(vec![
        Substitution {
            token: REPLICAS_PATCH_TOKEN,
            env: NAVIGATOR_SIMULATED_MATTERS,
            value: patches,
        },
        Substitution {
            token: DISRUPTION_BUDGETS_RESOURCE_TOKEN,
            env: NAVIGATOR_SIMULATED_MATTERS,
            value: budgets,
        },
    ])
}

fn summary_substitutions<F>(deployment: &str, get: &F) -> Vec<Substitution>
where
    F: Fn(&str) -> Option<String>,
{
    vec![
        Substitution {
            token: "YOUR_SUMMARY_ENABLED",
            env: SUMMARY_ENABLED_ENV,
            value: non_empty_env(SUMMARY_ENABLED_ENV, get).unwrap_or_else(|| "false".to_string()),
        },
        Substitution {
            token: "YOUR_DEPLOYMENT_ID",
            env: SUMMARY_DEPLOYMENT_ENV,
            value: deployment.to_string(),
        },
        Substitution {
            token: "YOUR_SUMMARY_CHANNEL_ID",
            env: SUMMARY_CHANNEL_ENV,
            value: non_empty_env(SUMMARY_CHANNEL_ENV, get).unwrap_or_default(),
        },
    ]
}

/// Render the optional provider switches. An omitted or blank client id keeps
/// the provider off, while its secret-rail values are enforced separately by
/// the deployment requirements once the switch is present.
fn optional_provider_substitutions<F>(get: &F) -> Vec<Substitution>
where
    F: Fn(&str) -> Option<String>,
{
    vec![
        // Microsoft is a second provider next to Google. Its tenant allowlist
        // is optional here because the provider's boot check owns that
        // dependency once the client id is non-empty.
        Substitution {
            token: "YOUR_OAUTH_MICROSOFT_CLIENT_ID",
            env: "OAUTH_MICROSOFT_CLIENT_ID",
            value: non_empty_env("OAUTH_MICROSOFT_CLIENT_ID", get).unwrap_or_default(),
        },
        Substitution {
            token: "YOUR_OAUTH_MICROSOFT_ALLOWED_TENANTS",
            env: "OAUTH_MICROSOFT_ALLOWED_TENANTS",
            value: non_empty_env("OAUTH_MICROSOFT_ALLOWED_TENANTS", get).unwrap_or_default(),
        },
        // Opt-in: omitted unless the deployment sets it, so the portal's
        // default (off) applies and an unconfigured row renders unchanged.
        optional_env_entry_substitution(
            "            # NAVIGATOR_OPTIONAL_ENV OAUTH_MICROSOFT_ALLOW_ANY_TENANT\n",
            "OAUTH_MICROSOFT_ALLOW_ANY_TENANT",
            get,
        ),
        // Apple stays off until a deployment owner enrolls the app; its three
        // signing values remain in the deployment Secret and are required
        // only when this public Services ID turns the provider on.
        optional_env_entry_substitution(
            "            # NAVIGATOR_OPTIONAL_ENV OAUTH_APPLE_CLIENT_ID\n",
            "OAUTH_APPLE_CLIENT_ID",
            get,
        ),
        // hCaptcha stays off until a deployment names its public sitekey; the
        // secret is required only once this coordinate is present.
        optional_env_entry_substitution(
            "            # NAVIGATOR_OPTIONAL_ENV HCAPTCHA_SITEKEY\n",
            "HCAPTCHA_SITEKEY",
            get,
        ),
    ]
}

/// Render one optional inline environment entry. The marker owns the entire
/// YAML entry, so an absent or blank coordinate removes both its name and
/// value instead of leaving a present-but-empty variable for consumers to
/// interpret. Future optional client IDs use another marker of this shape.
fn optional_env_entry_substitution<F>(
    token: &'static str,
    env: &'static str,
    get: &F,
) -> Substitution
where
    F: Fn(&str) -> Option<String>,
{
    Substitution {
        token,
        env,
        value: non_empty_env(env, get)
            .map(|value| {
                // Quoted so `true` / `1` stay strings: an env `value` that
                // parses as a YAML bool or int is rejected by Kubernetes.
                let quoted = serde_json::to_string(&value).unwrap_or(value);
                format!("            - name: {env}\n              value: {quoted}\n")
            })
            .unwrap_or_default(),
    }
}

/// One additional live brand this deployment's environment serves, with the
/// host that belongs on that environment (`www` or `staging.`).
struct AdditionalBrandBinding {
    key: BrandKey,
    host: &'static str,
}

/// Every host this deployment's environment serves for a brand other than
/// the default (`views::brand::BrandKey::default()`), derived from the
/// compiled registry rather than a hand-maintained coordinate — so a brand
/// added to `BrandKey` gets a cert and an Ingress rule with no second list to
/// keep in sync. [`BrandKey::hosts`] mixes production and staging hosts for
/// the same key (e.g. `www.deleteyourdata.com` and
/// `staging.deleteyourdata.com`); this keeps only the one matching
/// `public_host`'s own `staging.`-prefix convention, the same split
/// `NAVIGATOR_PUBLIC_HOST` already follows for the default brand.
fn additional_brand_hosts(public_host: &str) -> Vec<&'static str> {
    additional_brand_bindings(public_host)
        .into_iter()
        .map(|binding| binding.host)
        .collect()
}

fn additional_brand_bindings(public_host: &str) -> Vec<AdditionalBrandBinding> {
    let staging = public_host.starts_with("staging.");
    // Built from `views::brand::release_brand_hosts` — the full compiled
    // registry, live or not — rather than `views::brand::live_brand_hosts`
    // (the launch gate) or `BrandKey::ALL` filtered on `is_live()` here.
    //
    // Before ENG-808 this filtered on `is_live()`, so a held-out brand had no
    // certificate or Ingress rule until the same change that launched it. TLS
    // readiness and the launch decision are independent: a certificate is
    // release infrastructure that should already be valid, trusted, and
    // serving the right SAN before a launch is approved, not requested by the
    // approval itself. `views::brand::admitted_brand_key` — unaffected by
    // this — is still what gates the router, the crawler base, and the
    // footer's family row, so a held-out host now serves a real certificate
    // and then a `404`, never a TLS handshake failure. See the launch-gate
    // note in `views::brand` and `the_render_covers_the_full_registry_while_the_router_stays_live_gated`.
    //
    // `release_brand_hosts` mixes production and staging names for the same
    // key (e.g. `www.deleteyourdata.com` and `staging.deleteyourdata.com`);
    // this keeps only the one matching `public_host`'s own `staging.`-prefix
    // convention, the same split `NAVIGATOR_PUBLIC_HOST` already follows for
    // the default brand.
    views::brand::release_brand_hosts()
        .into_iter()
        .filter(|(key, _)| *key != BrandKey::default())
        .filter(|(_, host)| host.starts_with("staging.") == staging)
        .map(|(key, host)| AdditionalBrandBinding { key, host })
        .collect()
}

fn brand_certificate_name(key: BrandKey) -> String {
    format!("navigator-web-{}", key.as_str())
}

/// The most Compute Engine SSL certificates one target HTTPS proxy
/// references. GKE Ingress attaches one per `ManagedCertificate` it names,
/// and the limit is fixed, not a quota:
/// <https://docs.cloud.google.com/load-balancing/docs/ssl-certificates>.
#[cfg(test)]
const TARGET_PROXY_CERTIFICATE_LIMIT: usize = 15;

/// The one `ManagedCertificate` that carries every brand host of the
/// environment: the Firm wears every brand, so its storefronts share a
/// certificate instead of spending a target-proxy slot each.
const FIRM_CERTIFICATE_NAME: &str = "navigator-firm";

/// Brands served only by the Firm certificate. The rest keep their own
/// certificate until the Firm certificate is `Active` on every deployment;
/// GKE recreates a certificate whose domains change, so the safe order is
/// to attach the new one first and remove the old ones after:
/// <https://docs.cloud.google.com/kubernetes-engine/docs/how-to/managed-certs>.
const FIRM_CERTIFICATE_ONLY: &[BrandKey] = &[
    BrandKey::Abhaya,
    BrandKey::DeathAndDivorce,
    BrandKey::GuiebShook,
];

fn has_own_certificate(key: BrandKey) -> bool {
    !FIRM_CERTIFICATE_ONLY.contains(&key)
}

/// The own `ManagedCertificate` of every [`FIRM_CERTIFICATE_ONLY`] brand. No
/// render produces them, but `kubectl apply` never prunes one an earlier
/// release created, and GKE attaches every `ManagedCertificate` still in the
/// namespace whether or not the Ingress names it. Left in place they push the
/// target proxy past its 15 certificates, the Ingress stops syncing, and
/// `navigator-firm` is never attached, so it can never validate.
fn retired_brand_certificates() -> Vec<String> {
    FIRM_CERTIFICATE_ONLY
        .iter()
        .map(|key| brand_certificate_name(*key))
        .collect()
}

/// Delete each [`retired_brand_certificates`] entry after the apply.
fn retire_firm_only_brand_certificates(cfg: &ShipConfig, dry_run: bool) -> Result<()> {
    for name in retired_brand_certificates() {
        exec(
            dry_run,
            kubectl(cfg)
                .arg("delete")
                .arg("managedcertificate")
                .arg(&name)
                .arg("--ignore-not-found=true"),
        )?;
    }
    Ok(())
}

/// Every brand host of `public_host`'s environment: the default brand's
/// public host, then each other registered brand's.
fn firm_certificate_hosts(public_host: &str) -> Vec<String> {
    std::iter::once(public_host.to_string())
        .chain(
            additional_brand_hosts(public_host)
                .into_iter()
                .map(str::to_string),
        )
        .collect()
}

fn managed_certificate_document(namespace: &str, name: &str, domains: &[String]) -> String {
    use std::fmt::Write as _;
    let domains = domains.iter().fold(String::new(), |mut list, domain| {
        let _ = writeln!(list, "    - {domain}");
        list
    });
    format!(
        "apiVersion: networking.gke.io/v1\n\
         kind: ManagedCertificate\n\
         metadata:\n  name: {name}\n  namespace: {namespace}\n\
         spec:\n  domains:\n{domains}"
    )
}

/// The Firm certificate, then one `ManagedCertificate` per additional brand
/// that still has its own. Each per-brand document lists exactly that
/// family's environment host.
fn brand_managed_certificate_yaml(
    namespace: &str,
    public_host: &str,
    bindings: &[AdditionalBrandBinding],
) -> String {
    std::iter::once(managed_certificate_document(
        namespace,
        FIRM_CERTIFICATE_NAME,
        &firm_certificate_hosts(public_host),
    ))
    .chain(
        bindings
            .iter()
            .filter(|binding| has_own_certificate(binding.key))
            .map(|binding| {
                managed_certificate_document(
                    namespace,
                    &brand_certificate_name(binding.key),
                    &[binding.host.to_string()],
                )
            }),
    )
    .collect::<Vec<_>>()
    .join("---\n")
}

/// The Firm certificate always exists, so the overlay always lists the file.
fn brand_cert_kustomize_resource() -> String {
    "  - ingress/brand-managed-certificates.yaml\n".to_string()
}

fn managed_certificates_annotation(bindings: &[AdditionalBrandBinding]) -> String {
    let mut names = vec!["navigator-web".to_string()];
    names.extend(
        bindings
            .iter()
            .filter(|binding| has_own_certificate(binding.key))
            .map(|binding| brand_certificate_name(binding.key)),
    );
    names.push("navigator-workflows".to_string());
    names.push("navigator-forgejo".to_string());
    names.push(FIRM_CERTIFICATE_NAME.to_string());
    format!(
        "    networking.gke.io/managed-certificates: {}\n",
        names.join(",")
    )
}

/// Render [`BRAND_INGRESS_RULES_TOKEN`]'s replacement: one Ingress host rule
/// per host, routed straight to `navigator-web` on a catch-all `/*` — no
/// `/mcp` path, which stays off every brand host. Matches the indentation of
/// the sibling `- host: NAVIGATOR_PUBLIC_HOST` rule.
fn brand_ingress_rule_lines(hosts: &[&str]) -> String {
    hosts.iter().fold(String::new(), |mut rules, host| {
        rules.push_str("    - host: ");
        rules.push_str(host);
        rules.push_str(
            "\n      http:\n        paths:\n          - path: /*\n            \
             pathType: ImplementationSpecific\n            backend:\n              service:\n                \
             name: navigator-web\n                port:\n                  number: 80\n",
        );
        rules
    })
}

fn base_substitutions<F>(deployment: &str, tag: &str, get: &F) -> Result<Vec<Substitution>>
where
    F: Fn(&str) -> Option<String>,
{
    let namespace = required_coordinate(deployment, "NAVIGATOR_K8S_NAMESPACE", get)?;
    let image_registry = required_coordinate(deployment, "NAVIGATOR_IMAGE_REGISTRY", get)?;

    Ok(vec![
        Substitution {
            token: "kind: Namespace\nmetadata:\n  name: navigator",
            env: "NAVIGATOR_K8S_NAMESPACE",
            value: format!("kind: Namespace\nmetadata:\n  name: {namespace}"),
        },
        Substitution {
            token: "namespace: navigator",
            env: "NAVIGATOR_K8S_NAMESPACE",
            value: format!("namespace: {namespace}"),
        },
        Substitution {
            token: "navigator.svc.cluster.local",
            env: "NAVIGATOR_K8S_NAMESPACE",
            value: format!("{namespace}.svc.cluster.local"),
        },
        Substitution {
            token: IMAGE_REGISTRY_TOKEN,
            env: "NAVIGATOR_IMAGE_REGISTRY",
            value: image_registry,
        },
        // The release tag is a placeholder like any other. It renders here
        // — not via a later `kubectl set image` — so `apply -k` lands a
        // pullable image on the first write: `maxSurge: 0`
        // (workflows-service) deletes the running pod before the
        // replacement is ready, so an apply carrying an unpullable tag
        // takes that tier down for the whole gap until the tag is
        // corrected. `roll` validates the shape before we get here.
        Substitution {
            token: RELEASE_TAG_TOKEN,
            env: "--tag",
            value: tag.to_owned(),
        },
    ])
}

fn required_substitution<F>(
    deployment: &str,
    token: &'static str,
    env: &'static str,
    get: &F,
) -> Result<Substitution>
where
    F: Fn(&str) -> Option<String>,
{
    let value = non_empty_env(env, get).ok_or_else(|| {
        anyhow::anyhow!(
            "{env} must be set in deployments/{deployment}/config.toml for `navigator ops ship` \
             — it renders the `{token}` placeholder in the embedded GKE manifests. \
             Set it, then re-run \
             `navigator ops ship --deployment {deployment} --tag <YY.M.D>`."
        )
    })?;
    if token.starts_with("YOUR_OAUTH_CLIENT_ID") {
        validate_google_oauth_client_id(env, &value)?;
    }
    Ok(Substitution { token, env, value })
}

fn validate_google_oauth_client_id(env: &str, value: &str) -> Result<()> {
    if !value.ends_with(GOOGLE_OAUTH_CLIENT_ID_SUFFIX) {
        bail!(
            "{env} must be the FULL Google OAuth client id ending in \
             `{GOOGLE_OAUTH_CLIENT_ID_SUFFIX}` (got `{value}`) — a bare id renders an \
             `OAUTH_CLIENT_ID` Google will not match, breaking the login redirect."
        );
    }
    Ok(())
}

fn required_coordinate<F>(deployment: &str, env: &'static str, get: &F) -> Result<String>
where
    F: Fn(&str) -> Option<String>,
{
    non_empty_env(env, get)
        .ok_or_else(|| anyhow::anyhow!("{env} must be set in deployments/{deployment}/config.toml"))
}

fn non_empty_env<F>(env: &str, get: &F) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    get(env).filter(|value| !value.trim().is_empty())
}

/// The suffix every Google OAuth client id carries. The render refuses a
/// bare id for the `YOUR_OAUTH_CLIENT_ID_*` placeholders so a missing suffix
/// fails at ship time, not at a user's first login.
const GOOGLE_OAUTH_CLIENT_ID_SUFFIX: &str = ".apps.googleusercontent.com";

/// The placeholder every `<registry>/navigator-*:` image in the
/// embedded tree carries. It is a substitution token, NOT an example of the
/// tag convention — the render replaces it with the `--tag` being rolled.
const RELEASE_TAG_TOKEN: &str = "YY.M.D";

/// The whole-line placeholder comment in `brand-managed-certificates.yaml`.
/// `ops ship` replaces it with one `ManagedCertificate` document per
/// additional live brand, or with nothing when this environment serves none.
const BRAND_MANAGED_CERTIFICATES_TOKEN: &str = "# NAVIGATOR_BRAND_MANAGED_CERTIFICATES\n";

/// The whole-line placeholder comment in `kustomization.yaml` that becomes
/// a `resources:` entry once at least one additional brand certificate exists.
const BRAND_CERT_KUSTOMIZE_TOKEN: &str = "  # NAVIGATOR_BRAND_CERT_RESOURCES\n";

/// The Ingress annotation listing the default-brand, workflows, and Forgejo
/// certificates. `ops ship` rewrites the whole line so additional brand
/// certificates sit between the first two.
const MANAGED_CERTIFICATES_ANNOTATION_TOKEN: &str = "    networking.gke.io/managed-certificates: navigator-web,navigator-workflows,navigator-forgejo\n";

/// The whole-line placeholder comment in `ingress.yaml`. `ops ship` replaces
/// it with zero or more Ingress host rules — see [`additional_brand_hosts`].
const BRAND_INGRESS_RULES_TOKEN: &str = "    # NAVIGATOR_BRAND_HOST_INGRESS_RULES\n";

/// The placeholder standing in for the registry namespace that HOLDS the
/// images — the `<host>/<namespace>` half of every image line.
///
/// Distinct from `YOUR_PROJECT_ID`, which renders the environment project
/// (buckets, GSA). The images do not live in a GCP project at all now, which
/// is the point: one token where there were three (region, hub project, repo
/// name), and no way for two of them to disagree.
const IMAGE_REGISTRY_TOKEN: &str = "YOUR_IMAGE_REGISTRY";

/// Apply every substitution to one manifest file's text. Plain string
/// replacement — the tokens are distinct and non-overlapping, so order is
/// irrelevant.
fn apply_substitutions(content: &str, subs: &[Substitution]) -> String {
    subs.iter().fold(content.to_string(), |acc, sub| {
        acc.replace(sub.token, &sub.value)
    })
}

/// Recursively write an embedded manifest `Dir` under `dest`, substituting
/// placeholders in every UTF-8 file (all our manifests are text; a
/// non-UTF-8 blob would be copied verbatim). Directory structure is
/// preserved so kustomize's relative `resources:`/`patches:` paths resolve.
fn render_embedded_dir(dir: &Dir, dest: &Path, subs: &[Substitution]) -> Result<()> {
    for file in dir.files() {
        let out = dest.join(file.path());
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create rendered manifest dir {}", parent.display()))?;
        }
        match file.contents_utf8() {
            Some(text) => fs::write(&out, apply_substitutions(text, subs)),
            None => fs::write(&out, file.contents()),
        }
        .with_context(|| format!("write rendered manifest {}", out.display()))?;
    }
    for child in dir.dirs() {
        render_embedded_dir(child, dest, subs)?;
    }
    Ok(())
}

/// Render the embedded GKE, exports, base, PostgreSQL, and Forgejo manifest
/// trees into a fresh temp dir with `subs` applied, returning the owned [`TempDir`] — dropping it
/// removes the rendered files, so the caller holds it only for the span of
/// the `kubectl` calls. The relative paths reproduce the repository layout
/// the kustomization references.
fn render_manifests_with(subs: &[Substitution]) -> Result<TempDir> {
    let tmp = tempfile::Builder::new()
        .prefix("navigator-ship-manifests-")
        .tempdir()
        .context("create temp dir for rendered manifests")?;
    render_embedded_dir(
        &GKE_MANIFESTS,
        &tmp.path().join(GKE_KUSTOMIZE_SUBPATH),
        subs,
    )?;
    render_embedded_dir(
        &EXPORTS_MANIFESTS,
        &tmp.path().join(EXPORTS_KUSTOMIZE_SUBPATH),
        subs,
    )?;
    render_embedded_dir(
        &K8S_BASE_MANIFESTS,
        &tmp.path().join(K8S_BASE_SUBPATH),
        subs,
    )?;
    render_embedded_dir(&POSTGRES_MANIFESTS, &tmp.path().join("k8s/postgres"), subs)?;
    render_embedded_dir(&FORGEJO_MANIFESTS, &tmp.path().join("k8s/forgejo"), subs)?;
    Ok(tmp)
}

/// The complete release plan rendered from one deployment row. The Collector
/// is carried alongside the application tree so a release roll can surface
/// and reconcile observability drift in the same operator-visible plan.
struct RenderedReleasePlan {
    application: TempDir,
    collector: String,
    mirror_enabled: bool,
}

fn render_release_plan(
    subs: &[Substitution],
    cfg: &ShipConfig,
    deployment: &super::deployments::Deployment,
) -> Result<RenderedReleasePlan> {
    Ok(RenderedReleasePlan {
        application: render_manifests_with(subs)?,
        collector: super::observability::render_collector_for_ship(cfg, deployment)?,
        mirror_enabled: deployment
            .coordinates
            .get(store::postgres_mirror::MIRROR_ENV)
            .is_some_and(|value| value.trim() == "1"),
    })
}

/// Service overlays are applied individually so PostgreSQL readiness and
/// database convergence can gate Navigator migrations, Forgejo, and finally
/// the application. They remain outside the ordinary application kustomization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReleaseStep {
    Postgres,
    PostgresRoles,
    NavigatorMigrations,
    ForgejoDatabase,
    Forgejo,
    Application,
}

const ORDERED_RELEASE_STEPS: &[ReleaseStep] = &[
    ReleaseStep::Postgres,
    ReleaseStep::PostgresRoles,
    ReleaseStep::NavigatorMigrations,
    ReleaseStep::ForgejoDatabase,
    ReleaseStep::Forgejo,
    ReleaseStep::Application,
];

fn ordered_release_steps() -> &'static [ReleaseStep] {
    ORDERED_RELEASE_STEPS
}

/// Verify deployment-owned inputs before the first cluster mutation. Values
/// stay in their dedicated encrypted service files; this checks key names and
/// public coordinates only.
pub(super) fn require_ordered_service_prerequisites(
    deployment: &super::deployments::Deployment,
) -> Result<()> {
    let required = [
        (
            "postgres-secrets.enc.yaml",
            &deployment.postgres_encrypted_keys,
            &[
                "NAVIGATOR_POSTGRES_PASSWORD",
                "NAVIGATOR_POSTGRES_TLS_CERTIFICATE",
                "NAVIGATOR_POSTGRES_TLS_PRIVATE_KEY",
                "NAVIGATOR_POSTGRES_CA_CERT",
                "NAVIGATOR_POSTGRES_APP_PASSWORD",
                "NAVIGATOR_POSTGRES_MIGRATOR_PASSWORD",
                "NAVIGATOR_POSTGRES_BACKUP_PASSWORD",
            ][..],
        ),
        (
            "forgejo-secrets.enc.yaml",
            &deployment.forgejo_encrypted_keys,
            &[
                "NAVIGATOR_FORGEJO_DATABASE_PASSWORD",
                "NAVIGATOR_FORGEJO_SECRET_KEY",
                "NAVIGATOR_FORGEJO_INTERNAL_TOKEN",
                "NAVIGATOR_FORGEJO_OAUTH2_JWT_SECRET",
                "NAVIGATOR_FORGEJO_POSTGRES_CA_CERT",
                "NAVIGATOR_FORGEJO_OIDC_CLIENT_SECRET",
            ][..],
        ),
    ];
    let dbos_keys: &[&str] = if dbos_enabled(deployment) {
        &["NAVIGATOR_POSTGRES_DBOS_PASSWORD"]
    } else {
        &[]
    };
    let required = required.into_iter().chain([(
        "postgres-secrets.enc.yaml",
        &deployment.postgres_encrypted_keys,
        dbos_keys,
    )]);
    for (file, present, expected) in required {
        let missing: Vec<_> = expected
            .iter()
            .filter(|key| !present.contains(**key))
            .copied()
            .collect();
        ensure!(
            missing.is_empty(),
            "ordered release requires {file} in deployments/{} with keys: {}",
            deployment.name,
            missing.join(", ")
        );
    }
    for key in [
        "NAVIGATOR_FORGEJO_ROOT_URL",
        "NAVIGATOR_FORGEJO_OIDC_CLIENT_ID",
        "NAVIGATOR_FORGEJO_OIDC_NAME",
        "NAVIGATOR_FORGEJO_OIDC_DISCOVERY_URL",
        "NAVIGATOR_FORGEJO_ADMISSION_DOMAINS",
    ] {
        let Some(value) = deployment
            .coordinates
            .get(key)
            .filter(|value| !value.trim().is_empty())
        else {
            bail!(
                "ordered release requires {key} in deployments/{}/config.toml",
                deployment.name
            );
        };
        ensure!(
            !value.contains('\n') && !value.contains('\r'),
            "{key} in deployments/{}/config.toml must be a single line",
            deployment.name
        );
    }
    for key in [
        "NAVIGATOR_FORGEJO_ROOT_URL",
        "NAVIGATOR_FORGEJO_OIDC_DISCOVERY_URL",
    ] {
        ensure!(
            deployment.coordinates[key].starts_with("https://"),
            "{key} in deployments/{}/config.toml must use HTTPS",
            deployment.name
        );
    }
    ensure!(
        deployment.coordinates["NAVIGATOR_FORGEJO_ROOT_URL"].ends_with('/'),
        "NAVIGATOR_FORGEJO_ROOT_URL in deployments/{}/config.toml must end with `/`",
        deployment.name
    );
    ensure!(
        valid_admission_domains(&deployment.coordinates["NAVIGATOR_FORGEJO_ADMISSION_DOMAINS"]),
        "NAVIGATOR_FORGEJO_ADMISSION_DOMAINS in deployments/{}/config.toml must be a comma-separated list of DNS names",
        deployment.name
    );
    Ok(())
}

fn valid_admission_domains(domains: &str) -> bool {
    !domains.is_empty()
        && domains.split(',').all(|domain| {
            !domain.is_empty()
                && domain.split('.').count() >= 2
                && domain.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && label
                            .as_bytes()
                            .first()
                            .is_some_and(u8::is_ascii_alphanumeric)
                        && label
                            .as_bytes()
                            .last()
                            .is_some_and(u8::is_ascii_alphanumeric)
                        && label
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                })
        })
}

/// The pure ordering boundary used by the live release and synthetic tests.
/// A failed step prevents every later apply/readiness check from running.
fn run_ordered_release<F>(mut run_step: F) -> Result<()>
where
    F: FnMut(ReleaseStep) -> Result<()>,
{
    for step in ordered_release_steps() {
        run_step(*step).with_context(|| format!("ordered release stopped at {step:?}"))?;
    }
    Ok(())
}

/// The service overlay that targets this deployment's namespace. Each overlay
/// pins its own `namespace:`, so that is what selects it — not
/// `NAVIGATOR_ENVIRONMENT`, which staging sets to `production` for its
/// runtime behaviour and which would otherwise ship production's overlay into
/// a namespace the staging cluster does not have.
fn render_service_overlay(rendered: &TempDir, cfg: &ShipConfig, service: &str) -> Result<PathBuf> {
    let base = rendered.path().join(GKE_KUSTOMIZE_SUBPATH).join(service);
    let pinned = format!("namespace: {}", cfg.namespace);
    let environment = if cfg.public_host.starts_with("staging.") {
        "staging"
    } else {
        "production"
    };
    let path = base.join(environment);
    let source = fs::read_to_string(path.join("kustomization.yaml"))
        .with_context(|| format!("read the {environment} {service} overlay"))?;
    if source.lines().any(|line| line.trim_end() == pinned) {
        return Ok(path);
    }
    bail!("the {environment} {service} overlay does not target the selected deployment namespace")
}

fn ensure_required_manifest(path: &Path, kind: &str, name: &str) -> Result<()> {
    let rendered = kustomize_build(path)?;
    let documents = rendered
        .split("---")
        .filter_map(|document| serde_yaml::from_str::<serde_json::Value>(document).ok());
    ensure!(
        documents.into_iter().any(|document| {
            document.get("kind").and_then(serde_json::Value::as_str) == Some(kind)
                && document
                    .pointer("/metadata/name")
                    .and_then(serde_json::Value::as_str)
                    == Some(name)
        }),
        "required {kind}/{name} is missing from rendered deployment overlay {}",
        path.display()
    );
    Ok(())
}

/// Whether this deployment hosts the DBOS executor in `web`
/// (`NAVIGATOR_DBOS_ENABLED = "true"` in its `config.toml`).
fn dbos_enabled(deployment: &super::deployments::Deployment) -> bool {
    deployment
        .coordinates
        .get(NAVIGATOR_DBOS_ENABLED)
        .is_some_and(|value| value.trim() == "true")
}

/// The DBOS login's Secret, required before the first apply only when the
/// deployment hosts the executor.
const DBOS_SERVICE_OBJECT: (&str, &str) = ("secret", "navigator-postgres-dbos");

/// Objects the Deployment Operator provisions outside the release. The
/// `navigator-forgejo-oidc` and `navigator-forgejo-admission` `ConfigMap`s are
/// absent on purpose: the Forgejo overlay generates them from `config.toml`
/// coordinates, so before a deployment's first ordered release they cannot
/// exist yet.
const ORDERED_SERVICE_OBJECTS: &[(&str, &str)] = &[
    ("secret", "navigator-postgres-auth"),
    ("secret", "navigator-postgres-tls"),
    ("secret", "navigator-postgres-app"),
    ("secret", "navigator-postgres-migrator"),
    ("secret", "navigator-postgres-backup"),
    ("configmap", "navigator-postgres-backup-target"),
    ("secret", "navigator-forgejo-config"),
    ("secret", "navigator-forgejo-postgres-ca"),
    ("secret", "navigator-forgejo-database"),
    ("secret", "navigator-forgejo-oidc"),
];

/// Read object names only: `kubectl get secret -o name` never returns Secret
/// data. These cluster-owned inputs must exist before the first apply.
fn ensure_ordered_service_objects(cfg: &ShipConfig, dry_run: bool, dbos: bool) -> Result<()> {
    let required = ORDERED_SERVICE_OBJECTS
        .iter()
        .copied()
        .chain(dbos.then_some(DBOS_SERVICE_OBJECT));
    if dry_run {
        eprintln!("DRY-RUN: would verify service Secret/ConfigMap names before apply");
        return Ok(());
    }
    let mut missing = Vec::new();
    for (kind, name) in required {
        let result = kubectl(cfg)
            .arg("get")
            .arg(kind)
            .arg(name)
            .arg("-o")
            .arg("name")
            .output()
            .with_context(|| format!("check required {kind}/{name}"))?;
        if !result.status.success() {
            missing.push(format!("{kind}/{name}"));
        }
    }
    ensure!(
        missing.is_empty(),
        "ordered release prerequisites are missing in the target namespace: {}. Provision them through the deployment's approved secret/configuration path before retrying; no resources were applied",
        missing.join(", ")
    );
    Ok(())
}

fn pin_service_images(
    rendered: &TempDir,
    path: &Path,
    cfg: &ShipConfig,
    tag: &str,
    digest_file: Option<&Path>,
    dry_run: bool,
) -> Result<()> {
    service_images_with(
        rendered,
        path,
        cfg,
        tag,
        digest_file,
        dry_run,
        |root, manifest, digests| {
            ensure_manifest_images_published(cfg, dry_run, manifest, tag)?;
            if !dry_run {
                pin_and_verify_manifest_images(cfg, root, manifest, tag, digests)?;
            }
            Ok(())
        },
    )
}

fn service_images_with(
    rendered: &TempDir,
    path: &Path,
    cfg: &ShipConfig,
    tag: &str,
    digest_file: Option<&Path>,
    dry_run: bool,
    verify_and_pin: impl FnOnce(&Path, &str, &Path) -> Result<()>,
) -> Result<()> {
    // The migration manifest intentionally carries no default image. Render
    // the same release image as web, then subject it to the normal registry,
    // digest-manifest, and cosign checks before applying its Job.
    replace_in_tree(
        &rendered.path().join("k8s/postgres/migrate"),
        "image: navigator-migrate",
        &format!("image: {}", cfg.web_image(tag)),
    )?;
    replace_in_tree(
        &rendered.path().join("k8s/postgres/mirror"),
        "image: navigator-surreal-archive ",
        &format!("image: {}/navigator-surreal-archive:{tag} ", cfg.registry()),
    )?;
    let Some(digest_file) = digest_file else {
        if dry_run {
            return Ok(());
        }
        bail!("`--image-digests <FILE>` is required for an ordered release");
    };
    let manifest = kustomize_build(path)?;
    // The overlay references shared manifests outside its own directory.
    // Pin the verified image in that complete source tree before apply.
    verify_and_pin(rendered.path(), &manifest, digest_file)
}

/// Bring the live PostgreSQL data volume up to the rendered claim template
/// before the apply (see [`super::postgres_volume`]): expand each smaller
/// claim, then delete the `StatefulSet` with `--cascade=orphan` when its
/// immutable template must change. Returns whether the `StatefulSet` is (or,
/// under `--dry-run`, would be) recreated.
fn grow_postgres_volume(cfg: &ShipConfig, dry_run: bool, service: &Path) -> Result<bool> {
    use super::postgres_volume::{claim_storage, desired_claim, plan, template_storage};
    let desired = desired_claim(&kustomize_build(service)?, "navigator-postgres")?;
    let live = kubectl_json_if_present(cfg, "statefulset", &desired.statefulset)?;
    let mut claims = Vec::new();
    for name in desired.claim_names() {
        let size = kubectl_json_if_present(cfg, "pvc", &name)?
            .as_ref()
            .and_then(claim_storage)
            .map(str::to_owned);
        claims.push((name, size));
    }
    let live_template = live
        .as_ref()
        .map(|statefulset| {
            template_storage(statefulset).with_context(|| {
                format!(
                    "live statefulset/{} has no claim template size",
                    desired.statefulset
                )
            })
        })
        .transpose()?;
    let actions = plan(&desired, live_template, &claims)?;
    if actions.is_empty() {
        eprintln!(
            "==> PostgreSQL data volume: {} on every claim; nothing to grow",
            desired.storage
        );
        return Ok(false);
    }
    let mut recreating = false;
    for action in &actions {
        eprintln!("==> PostgreSQL data volume: {}", action.describe());
        recreating |= matches!(
            action,
            super::postgres_volume::VolumeAction::RecreateOrphaned { .. }
        );
        exec(dry_run, kubectl(cfg).args(action.kubectl_args()))
            .with_context(|| format!("PostgreSQL data volume: {}", action.describe()))?;
    }
    Ok(recreating)
}

/// `kubectl get <kind> <name> -o json`, or `None` when the object is absent.
fn kubectl_json_if_present(
    cfg: &ShipConfig,
    kind: &str,
    name: &str,
) -> Result<Option<serde_json::Value>> {
    let out = kubectl(cfg)
        .args(["get", kind, name, "--ignore-not-found", "-o", "json"])
        .output()
        .with_context(|| format!("run kubectl get {kind} {name}"))?;
    if !out.status.success() {
        bail!(
            "kubectl get {kind} {name} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    if out.stdout.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    serde_json::from_slice(&out.stdout)
        .map(Some)
        .with_context(|| format!("parse `kubectl get {kind} {name} -o json`"))
}

fn wait_statefulset(cfg: &ShipConfig, dry_run: bool, name: &str, timeout: &str) -> Result<()> {
    exec(
        dry_run,
        kubectl(cfg)
            .arg("rollout")
            .arg("status")
            .arg(format!("statefulset/{name}"))
            .arg(format!("--timeout={timeout}")),
    )
    .with_context(|| format!("statefulset/{name} did not become Ready within {timeout}"))
}

/// Wait for `job/{name}` to complete, and fail as soon as it fails.
///
/// `kubectl wait --for=condition=complete` alone never returns for a Job that
/// reached its backoff limit: it waits out the whole timeout. 26.10.27's staging
/// ship sat 25 minutes on a failed `navigator-postgres-mirror-reconcile` until
/// the workflow's 30-minute cap cancelled it, which also lost the transcript.
/// So a second `kubectl wait --for=condition=failed` races the first.
fn wait_job(cfg: &ShipConfig, dry_run: bool, name: &str, timeout: &str) -> Result<()> {
    let wait = |condition: &str| {
        let mut cmd = kubectl(cfg);
        cmd.arg("wait")
            .arg(format!("--for=condition={condition}"))
            .arg(format!("job/{name}"))
            .arg(format!("--timeout={timeout}"));
        cmd
    };
    let mut complete = wait("complete");
    if dry_run {
        return exec(true, &mut complete);
    }
    let mut complete = complete
        .spawn()
        .with_context(|| format!("wait for job/{name} to complete"))?;
    let mut failed = wait("failed")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("watch job/{name} for failure"))?;
    let mut failure_watch_open = true;
    loop {
        if let Some(status) = complete.try_wait()? {
            let _ = failed.kill();
            let _ = failed.wait();
            ensure!(
                status.success(),
                "job/{name} did not complete within {timeout}"
            );
            return Ok(());
        }
        if failure_watch_open {
            if let Some(status) = failed.try_wait()? {
                failure_watch_open = false;
                if status.success() {
                    let _ = complete.kill();
                    let _ = complete.wait();
                    bail!(
                        "job/{name} failed: it reached its backoff limit before completing; read \
                         `kubectl logs job/{name}` before its pod is cleaned up"
                    );
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

/// Rerun an idempotent one-shot service Job without touching retained data.
/// Only the named Job is deleted; Kustomize apply then recreates it and the
/// release blocks until it completes successfully.
fn run_job_overlay(
    cfg: &ShipConfig,
    dry_run: bool,
    path: &Path,
    name: &str,
    timeout: &str,
) -> Result<()> {
    ensure_required_manifest(path, "Job", name)?;
    if dry_run {
        eprintln!(
            "DRY-RUN: would delete only job/{name}, apply {}, and wait for completion",
            path.display()
        );
        return Ok(());
    }
    let deleted = kubectl(cfg)
        .arg("delete")
        .arg("job")
        .arg(name)
        .arg("--ignore-not-found=true")
        .status()
        .with_context(|| format!("delete completed job/{name} before rerun"))?;
    ensure!(
        deleted.success(),
        "kubectl delete job/{name} failed; refusing to continue"
    );
    reconcile_manifests(cfg, false, path)?;
    wait_job(cfg, false, name, timeout)
}

/// The `SecretProviderClass`, relative to the rendered GKE kustomize root.
const SECRET_PROVIDER_CLASS: &str = "secrets/secret-provider-class.yaml";

/// Rewrite the rendered `SecretProviderClass` so it references exactly the
/// objects this deployment writes.
///
/// The object list is one embedded manifest shared by every deployment. An
/// entry the shipping deployment will never write is therefore not a harmless
/// extra reference: the Secret Manager preflight aborts at
/// [`ensure_projected_objects_resolve`]. One shared list cannot express an
/// object that is required in one deployment and forbidden in another (the
/// engineering webhook trio) or one belonging to an integration a deployment
/// declines outright (`DocuSign`), so the list is rendered per deployment from the
/// same `skipped` set `ops secrets apply` reports.
///
/// In place on the rendered copy: the embedded tree is never touched, so the
/// omission lasts for the span of one
/// ship and is visible in the `kubectl diff` that precedes the apply.
fn omit_unwritten_objects(gke_root: &Path, skipped: &BTreeSet<String>) -> Result<()> {
    if skipped.is_empty() {
        return Ok(());
    }
    let path = gke_root.join(SECRET_PROVIDER_CLASS);
    let spc = fs::read_to_string(&path)
        .with_context(|| format!("read rendered SecretProviderClass {}", path.display()))?;
    let filtered = without_projected_objects(&spc, skipped)?;
    fs::write(&path, filtered)
        .with_context(|| format!("write rendered SecretProviderClass {}", path.display()))?;
    eprintln!(
        "==> SecretProviderClass renders {} object(s) this deployment does not write: {}",
        skipped.len(),
        skipped.iter().cloned().collect::<Vec<_>>().join(", ")
    );
    Ok(())
}

/// Drop every `parameters.secrets` entry naming an object in `omitted`.
///
/// A line filter rather than a YAML round-trip, for the reason
/// the embedded manifest carries: this manifest carries load-bearing comments
/// (which objects were trimmed and why, which keys are boot invariants) that a
/// serialize/deserialize cycle would silently drop. Every entry is a head line
/// plus its one continuation, so the shape is asserted rather than assumed — an
/// entry that ever grows a third line fails here instead of rendering a
/// half-removed reference. The result is then re-parsed and checked, so the
/// guarantee is structural and not "the text looked right".
fn without_projected_objects(spc: &str, omitted: &BTreeSet<String>) -> Result<String> {
    let mut kept: Vec<&str> = Vec::new();
    let mut dropping = false;
    for line in spc.lines() {
        let trimmed = line.trim_start();
        if let Some(object) = entry_object_name(trimmed) {
            dropping = omitted.contains(&object);
        } else if dropping && !trimmed.starts_with("path:") {
            // The entry ended without our having seen its continuation, which
            // means the manifest no longer has the shape this filter removes.
            dropping = false;
        }
        if !dropping {
            kept.push(line);
        }
    }
    let mut filtered = kept.join("\n");
    if spc.ends_with('\n') {
        filtered.push('\n');
    }
    let surviving = projected_object_names(&filtered)?;
    let leaked: Vec<&String> = omitted.iter().filter(|o| surviving.contains(*o)).collect();
    ensure!(
        leaked.is_empty(),
        "the rendered SecretProviderClass still references {leaked:?} after omitting them; \
         refusing to ship a catalog that names an object this deployment does not write"
    );
    Ok(filtered)
}

/// The object name a `parameters.secrets` entry head line names.
fn entry_object_name(trimmed: &str) -> Option<String> {
    if let Some(rest) = trimmed.strip_prefix("- resourceName:") {
        let reference = rest.trim().trim_matches('"');
        let (_, tail) = reference.split_once("/secrets/")?;
        let (object, _) = tail.split_once("/versions/")?;
        return Some(object.to_string());
    }
    None
}

/// Every object name a `SecretProviderClass` document references — the check
/// side of [`without_projected_objects`].
fn projected_object_names(spc: &str) -> Result<BTreeSet<String>> {
    let doc: serde_json::Value =
        serde_yaml::from_str(spc).context("parse the filtered SecretProviderClass")?;
    let secrets_block = doc
        .pointer("/spec/parameters/secrets")
        .and_then(serde_json::Value::as_str)
        .context("the filtered SecretProviderClass has spec.parameters.secrets")?;
    let names: BTreeSet<String> = serde_yaml::from_str::<serde_json::Value>(secrets_block)
        .context("parse the filtered parameters.secrets block")?
        .as_array()
        .context("parameters.secrets is a list")?
        .iter()
        .filter_map(|entry| entry.get("path").and_then(serde_json::Value::as_str))
        .map(ToString::to_string)
        .collect();
    Ok(names)
}

/// The ordered `kubectl -k` verbs the reconcile runs against the rendered
/// dir. A live run diffs (surface drift) then applies; `--dry-run` stops
/// after the diff and never applies — the machine-checkable form of "shows
/// the diff with no apply".
fn reconcile_verbs(dry_run: bool) -> &'static [&'static str] {
    if dry_run {
        &["diff"]
    } else {
        &["diff", "apply"]
    }
}

/// Every per-deployment value `ship` reads, resolved once from the selected
/// `deployments/<name>/config.toml`. Required values bail when unset (fail
/// fast — never substitute a project-internal default). Optional values fall
/// back to a documented workspace default or to `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShipConfig {
    /// The selected deployment's directory name under `deployments/`. Every
    /// error that tells the operator to fix a coordinate names this file.
    pub name: String,
    /// Typed deployment profile. `dev` is cloud staging; production is live.
    pub environment: store::DeploymentEnvironment,
    /// Explicit deployment roles used to scope singleton integrations.
    pub capabilities: store::deployment::DeploymentCapabilities,
    /// Target GCP project ID (`NAVIGATOR_GCP_PROJECT_ID`). Used to
    /// derive the `kubectl` context.
    pub project_id: String,
    /// Region the cluster lives in (`NAVIGATOR_GCP_LOCATION`). Used to
    /// derive the `kubectl` context.
    pub location: String,
    /// Cluster name (`NAVIGATOR_GKE_CLUSTER_NAME`). Used to derive the
    /// `kubectl` context.
    pub cluster: String,
    /// Registry namespace the images live under — `<host>/<namespace>`,
    /// from required `NAVIGATOR_IMAGE_REGISTRY` deployment configuration.
    pub registry: String,
    /// K8s namespace for the Deployments (`NAVIGATOR_K8S_NAMESPACE`,
    /// default `navigator`).
    pub namespace: String,
    /// Immutable brand image name (`NAVIGATOR_WEB_IMAGE`).
    pub web_image_name: String,
    /// Exact public hostname (`NAVIGATOR_PUBLIC_HOST`) used for smoke checks.
    pub public_host: String,
    /// Public asset origin (`NAVIGATOR_ASSET_BASE_URL`) checked after a live
    /// roll, normally the selected deployment's same-origin `/assets` route.
    pub asset_base_url: String,
    /// Google service-account id bound to the runtime Kubernetes service
    /// accounts (`NAVIGATOR_GCP_SERVICE_ACCOUNT_ID`).
    pub google_service_account_id: String,
    /// Public hostname for the post-rollout smoke check — the resolved
    /// `brand.primary_domain` (Neon Law by default, or a custom bundle's).
    pub primary_domain: String,
    /// Name of the K8s Secret service deployments `envFrom`
    /// (`NAVIGATOR_WEB_SECRET_NAME`).
    pub secret_name: String,
    /// Public worker URL Restate Cloud dials (`NAVIGATOR_WORKFLOWS_URL`).
    /// `None` → fall through to the `devx restate register` default.
    pub workflows_url: Option<String>,
    /// `kubectl` context to pin every prod call to. Override with
    /// `NAVIGATOR_GKE_CONTEXT`; otherwise the GKE convention
    /// `gke_<project>_<location>_<cluster>`.
    pub context: String,
}

impl ShipConfig {
    /// Resolve every value from a loaded `deployments/<name>/` tree — the
    /// only source; nothing here reads the process environment.
    pub fn from_deployment(deployment: &super::deployments::Deployment) -> Result<Self> {
        Self::from_lookup(&deployment.name, |key| {
            deployment.coordinates.get(key).cloned()
        })
    }

    /// The resolution behind [`ShipConfig::from_deployment`], split from the
    /// tree load so it is unit-tested against an in-memory coordinate map.
    /// Bails on the first missing required key with a message naming it and
    /// the `config.toml` it belongs in.
    fn from_lookup<F>(name: &str, get: F) -> Result<Self>
    where
        F: Fn(&str) -> Option<String>,
    {
        let environment = store::DeploymentEnvironment::from_lookup(&get)
            .context("parse NAVIGATOR_ENVIRONMENT")?;
        let capabilities = store::deployment::DeploymentCapabilities::from_lookup(&get)
            .context("parse deployment capability flags")?;
        let project_id = required_coordinate(name, "NAVIGATOR_GCP_PROJECT_ID", &get)?;
        let location = required_coordinate(name, "NAVIGATOR_GCP_LOCATION", &get)?;
        let cluster = required_coordinate(name, "NAVIGATOR_GKE_CLUSTER_NAME", &get)?;
        let registry = required_coordinate(name, "NAVIGATOR_IMAGE_REGISTRY", &get)?;
        let primary_domain = super::brand::primary_domain_with(&get)?;
        let namespace =
            non_empty_env("NAVIGATOR_K8S_NAMESPACE", &get).unwrap_or_else(|| "navigator".into());
        let web_image_name = required_coordinate(name, "NAVIGATOR_WEB_IMAGE", &get)?;
        if !BRAND_IMAGES.contains(&web_image_name.as_str()) {
            bail!(
                "NAVIGATOR_WEB_IMAGE must be one of {}; got `{web_image_name}`",
                BRAND_IMAGES.join(", ")
            );
        }
        let public_host = required_coordinate(name, "NAVIGATOR_PUBLIC_HOST", &get)?;
        let asset_base_url = get(ASSET_BASE_URL_KEY).unwrap_or_default();
        let google_service_account_id = non_empty_env("NAVIGATOR_GCP_SERVICE_ACCOUNT_ID", &get)
            .unwrap_or_else(|| "navigator-web".into());
        let secret_name = required_coordinate(name, "NAVIGATOR_WEB_SECRET_NAME", &get)?;
        let workflows_url = non_empty_env("NAVIGATOR_WORKFLOWS_URL", &get);
        let context = non_empty_env("NAVIGATOR_GKE_CONTEXT", &get)
            .unwrap_or_else(|| derived_context(&project_id, &location, &cluster));
        Ok(Self {
            name: name.to_owned(),
            environment,
            capabilities,
            project_id,
            location,
            cluster,
            registry,
            namespace,
            web_image_name,
            public_host,
            asset_base_url,
            google_service_account_id,
            primary_domain,
            secret_name,
            workflows_url,
            context,
        })
    }

    /// Registry namespace — `<host>/<namespace>`. CI publishes every
    /// image under this prefix; ship rolls the cluster onto them.
    #[must_use]
    pub fn registry(&self) -> String {
        self.registry.clone()
    }

    /// Published immutable brand image URL at the `YY.M.D` `tag`.
    #[must_use]
    pub fn web_image(&self, tag: &str) -> String {
        format!("{}/{}:{tag}", self.registry(), self.web_image_name)
    }

    /// Published `navigator-workflows-service` image URL at the
    /// `YY.M.D` `tag`.
    #[must_use]
    pub fn workflows_image(&self, tag: &str) -> String {
        format!("{}/navigator-workflows-service:{tag}", self.registry())
    }

    /// The public worker URL the 7d re-register targets, resolved the
    /// same way `devx restate register` resolves it: explicit
    /// `NAVIGATOR_WORKFLOWS_URL` first, otherwise derived from the resolved
    /// `brand.primary_domain` (`https://workflows.<domain>/`), never
    /// the bare placeholder when a domain is known. This is what the
    /// 2026-06-10 ship needed — it had a domain but no explicit URL and
    /// fell through to `workflows.example.com`, silently no-op'ing the
    /// register.
    #[must_use]
    pub fn workflows_url_resolved(&self) -> String {
        super::resolve_workflows_url(
            None,
            self.workflows_url.as_deref(),
            Some(&self.primary_domain),
        )
    }
}

/// The GKE context naming convention `gcloud container clusters
/// get-credentials` writes. Factored out so the formula is testable.
#[must_use]
fn derived_context(project_id: &str, location: &str, cluster: &str) -> String {
    format!("gke_{project_id}_{location}_{cluster}")
}

/// The registry prefix ship pulls from.
///
/// The images live in one private Artifact Registry repository, not in the project being
/// shipped into: CI pushes each image once and every deployment pulls the same
/// digest, which is what makes staging a proving ring rather than a different
/// build. Factored out so the fallback is testable without mutating process
/// environment.
#[must_use]
#[cfg(test)]
pub(super) fn images_registry(configured: Option<&str>) -> String {
    configured
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(String::new, str::to_string)
}

/// The public origin `web` serves blog/marketing images from — normally the
/// site's same-origin private-bucket proxy, e.g.
/// `https://www.example.test/assets`. `server/public/img/`
/// is gitignored and never baked into the image, so with this unset the
/// rolled binary resolves content images against an empty same-origin
/// `/public` and every hero 404s. A coordinate of the selected
/// `deployments/<name>/config.toml`.
const ASSET_BASE_URL_KEY: &str = "NAVIGATOR_ASSET_BASE_URL";
const ASSETS_BUCKET_KEY: &str = "NAVIGATOR_ASSETS_BUCKET";

/// Whether the asset origin is usably configured (present and not blank).
fn asset_base_url_present(value: Option<&str>) -> bool {
    matches!(value, Some(v) if !v.trim().is_empty())
}

/// Refuse to ship until the public asset origin is configured. A missing
/// value means the selected deployment's `config.toml` is missing the key —
/// the fix is to set it there, not to work around it, so name it explicitly
/// and stop before any rollout rather than deploy a site whose every image
/// 404s.
fn require_asset_base_url(deployment: &super::deployments::Deployment) -> Result<()> {
    require_asset_base_url_value(
        &deployment.name,
        deployment
            .coordinates
            .get(ASSET_BASE_URL_KEY)
            .map(String::as_str),
    )
}

/// The decision behind [`require_asset_base_url`], split from the tree
/// load so the failure message is unit-tested against literal inputs.
fn require_asset_base_url_value(deployment: &str, value: Option<&str>) -> Result<()> {
    if !asset_base_url_present(value) {
        bail!(
            "{ASSET_BASE_URL_KEY} must be set in deployments/{deployment}/config.toml \
             before shipping — it is the public origin `web` serves blog/marketing images from, \
             normally `<NAV_BASE_URL>/assets`, backed by the private NAVIGATOR_ASSETS_BUCKET \
             through the deployment's GKE workload identity. Without it \
             the rolled site resolves images against an empty `/public` and every hero 404s. Set \
             it there, then run \
             `navigator ops ship --deployment {deployment} --tag <YY.M.D>`."
        );
    }
    Ok(())
}

fn verify_assets_bucket_value_with<F>(
    deployment: &str,
    value: Option<&str>,
    verify: F,
) -> Result<()>
where
    F: FnOnce(&str) -> Result<()>,
{
    let Some(bucket) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        bail!(
            "{ASSETS_BUCKET_KEY} must be set in deployments/{deployment}/config.toml before \
             shipping — the asset preflight checks that deployment's bucket directly"
        );
    };
    verify(bucket).with_context(|| {
        format!("verify public assets for deployment `{deployment}` in bucket `gs://{bucket}`")
    })
}

fn verify_assets_bucket(root: &Path, deployment: &super::deployments::Deployment) -> Result<()> {
    verify_assets_bucket_value_with(
        &deployment.name,
        deployment
            .coordinates
            .get(ASSETS_BUCKET_KEY)
            .map(String::as_str),
        |bucket| crate::assets::verify_required_public_assets_bucket(root, bucket),
    )
}

/// A boot requirement shared with web's deployment-invariant validator.
///
/// `any_of` holds the alternatives of a disjunctive invariant
/// (`"A or B + C must be set"`): the requirement is satisfied when any one
/// alternative's keys are ALL present in the Secret ∪ Deployment env. A
/// plain `"KEY must be set"` invariant is the one-alternative, one-key
/// case.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SecretRequirement {
    /// Alternatives (split on `" or "`), each a conjunction of keys
    /// (split on `" + "`) that must all be present together.
    pub any_of: Vec<Vec<String>>,
    /// Env var that gates a conditional requirement (`"… when TRIGGER
    /// is …"`); the requirement applies only when the trigger is itself
    /// configured. `None` → always required.
    pub trigger: Option<String>,
}

impl SecretRequirement {
    /// True when any one alternative's keys are all in `satisfied`.
    #[must_use]
    pub fn is_satisfied_by(&self, satisfied: &BTreeSet<String>) -> bool {
        self.any_of
            .iter()
            .any(|alt| alt.iter().all(|key| satisfied.contains(key)))
    }

    /// The invariant's own phrasing — `"A or B + C"` — for error text.
    #[must_use]
    pub fn describe(&self) -> String {
        self.any_of
            .iter()
            .map(|alt| alt.join(" + "))
            .collect::<Vec<_>>()
            .join(" or ")
    }
}

fn shared_web_requirements(
    capabilities: store::deployment::DeploymentCapabilities,
) -> Vec<SecretRequirement> {
    store::deployment::WEB_REQUIREMENTS
        .iter()
        .filter(|requirement| requirement.scope.applies(capabilities))
        .map(|requirement| SecretRequirement {
            any_of: requirement
                .any_of
                .iter()
                .map(|alternative| alternative.iter().map(ToString::to_string).collect())
                .collect(),
            trigger: requirement.trigger.map(ToString::to_string),
        })
        .collect()
}

/// Test-only parser for invariant-shaped requirement literals.
/// enforces, by scraping the `"<KEYS> must be set` string literals
/// straight from the invariant source. Reading the source (rather than
/// maintaining a duplicate list) means this never drifts from the
/// binary's actual boot requirements.
///
/// The scraper is line-based: everything from the literal's opening `"`
/// through `" must be set"` must sit on one source line. Two shapes
/// parse:
///
/// - a single key (`"SENDGRID_API_KEY must be set (otherwise …"`) — the
///   one-alternative, one-key requirement;
/// - a disjunction (`"A or B + C must be set …"`) — alternatives split
///   on `" or "`, conjunctions on `" + "`; the pod boots when any one
///   alternative is fully configured.
///
/// Some invariants are conditional: the binary only enforces them when
/// another env var is itself set — e.g. `"OIDC_AUDIENCE must be set when
/// OIDC_JWKS_URL is …"`. The invariant message names its own trigger
/// ("… when `TRIGGER` is …"), so the trigger is read from the same
/// literal, staying faithful to the "scrape the source, never maintain a
/// parallel list" philosophy. A requirement that appears both
/// conditionally and unconditionally resolves to unconditional (it is
/// always required). Sorted + de-duplicated.
#[must_use]
#[cfg(test)]
pub fn secret_requirements(config_src: &str) -> Vec<SecretRequirement> {
    const MARKER: &str = " must be set";
    let mut reqs: BTreeMap<Vec<Vec<String>>, Option<String>> = BTreeMap::new();
    for line in config_src.lines() {
        let Some(end) = line.find(MARKER) else {
            continue;
        };
        let Some(any_of) = requirement_chain(&line[..end]) else {
            continue;
        };
        let trigger = trigger_after(&line[end + MARKER.len()..]);
        let unconditional = trigger.is_none();
        reqs.entry(any_of)
            .and_modify(|existing| {
                if unconditional {
                    *existing = None;
                }
            })
            .or_insert(trigger);
    }
    reqs.into_iter()
        .map(|(any_of, trigger)| SecretRequirement { any_of, trigger })
        .collect()
}

/// Walk left from `" must be set"` across identifier runs chained by
/// `" or "` / `" + "`, requiring the chain to open a string literal —
/// the char before the leftmost identifier is a double-quote. That
/// filters out prose that happens to contain "… must be set". Returns
/// the alternatives (`" or "`) of conjunctions (`" + "`) in source
/// order, or `None` when the text before the marker is not such a chain.
#[cfg(test)]
fn requirement_chain(prefix: &str) -> Option<Vec<Vec<String>>> {
    const OR: &str = " or ";
    const AND: &str = " + ";
    fn is_ident(b: u8) -> bool {
        b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'
    }
    let bytes = prefix.as_bytes();
    let mut alternatives: Vec<Vec<String>> = Vec::new();
    let mut conjunction: Vec<String> = Vec::new();
    let mut end = bytes.len();
    loop {
        let mut start = end;
        while start > 0 && is_ident(bytes[start - 1]) {
            start -= 1;
        }
        if start == end || start == 0 {
            // No identifier where the chain requires one, or the chain
            // runs to line start without ever opening a literal.
            return None;
        }
        conjunction.insert(0, prefix[start..end].to_string());
        let before = &prefix[..start];
        if bytes[start - 1] == b'"' {
            alternatives.insert(0, conjunction);
            return Some(alternatives);
        }
        if let Some(rest) = before.strip_suffix(AND) {
            end = rest.len();
        } else {
            let rest = before.strip_suffix(OR)?;
            alternatives.insert(0, std::mem::take(&mut conjunction));
            end = rest.len();
        }
    }
}

/// Read the trigger key out of the tail of an invariant message — the
/// `TRIGGER` in `" when TRIGGER is …"`. Returns `None` for an
/// unconditional invariant (whose tail starts with `" (otherwise …"`).
#[cfg(test)]
fn trigger_after(tail: &str) -> Option<String> {
    let rest = tail.trim_start().strip_prefix("when ")?.trim_start();
    let ident: String = rest
        .chars()
        .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
        .collect();
    (!ident.is_empty()).then_some(ident)
}

/// Of the parsed requirements, those actually applying to *this*
/// environment: every unconditional requirement, plus each conditional
/// one whose trigger is itself satisfied (present in the Secret or a
/// Deployment env). A conditional requirement whose trigger is absent
/// does not apply — the binary's runtime invariant skips it too.
#[must_use]
pub fn effective_requirements(
    parsed: &[SecretRequirement],
    satisfied: &BTreeSet<String>,
) -> Vec<SecretRequirement> {
    parsed
        .iter()
        .filter(|req| req.trigger.as_ref().is_none_or(|t| satisfied.contains(t)))
        .cloned()
        .collect()
}

/// Requirements no alternative of which is satisfied by anything the
/// running pod can see. `satisfied` is the union of the Secret's
/// non-empty data keys and the Deployments' usably-declared env-var
/// names — a key present in either is fine (a plain env var, a
/// `secretKeyRef`, or `envFrom` all count). What remains will
/// crash-loop the new pod at boot.
#[must_use]
pub fn unsatisfied_requirements(
    required: &[SecretRequirement],
    satisfied: &BTreeSet<String>,
) -> Vec<SecretRequirement> {
    required
        .iter()
        .filter(|req| !req.is_satisfied_by(satisfied))
        .cloned()
        .collect()
}

fn missing_requirements_by_deployment(
    parsed: &[SecretRequirement],
    secret_keys: &BTreeSet<String>,
    deployment_envs: &[(String, BTreeSet<String>)],
) -> Vec<(String, Vec<SecretRequirement>)> {
    deployment_envs
        .iter()
        .filter_map(|(deployment, env_names)| {
            let satisfied: BTreeSet<String> = secret_keys.union(env_names).cloned().collect();
            // Drop conditional invariants whose trigger isn't configured
            // in this deployment — the binary's own runtime check skips
            // them too, so requiring them would be a false positive.
            let required = effective_requirements(parsed, &satisfied);
            let missing = unsatisfied_requirements(&required, &satisfied);
            (!missing.is_empty()).then(|| (deployment.clone(), missing))
        })
        .collect()
}

// ---------- orchestration (shell-out; not unit-tested) ----------

/// Options parsed from the `devx ship` flags.
#[allow(clippy::struct_excessive_bools)] // One independent switch per CLI flag.
#[derive(Debug, Clone, Default)]
pub struct ShipOpts {
    /// Deployment directory under `deployments/` to roll. Required and
    /// explicit — there is no environment fallback, because a fallback is
    /// what lets a stale shell silently ship the wrong deployment.
    pub deployment: String,
    /// The directory holding the `deployments/` tree, when it is not the
    /// workspace — a checkout that carries the tree and nothing else.
    /// `None` falls back to `NAVIGATOR_DEPLOYMENTS_DIR`, then to the
    /// discovered workspace root. See [`super::deployments::root`].
    pub deployments_dir: Option<PathBuf>,
    /// Explicit project containing the versioned cloud configuration. Bypasses
    /// local deployment paths, including `NAVIGATOR_DEPLOYMENTS_DIR`.
    pub project_id: Option<String>,
    /// Print every command instead of running it.
    pub dry_run: bool,
    /// No-rebuild path: just `kubectl rollout restart` service
    /// deployments (Secret-value rotation), then exit.
    pub restart_only: bool,
    /// The NARROW lane, for automated deploys: move every navigator image
    /// to `--tag` and nothing else.
    ///
    /// It exists so CI can hold a credential that cannot do anything but
    /// bump a version. The full roll re-asserts IAM on the web GSA and
    /// applies the whole manifest tree, which needs `serviceAccountAdmin`
    /// and write on every manifest kind; this needs `patch`/`get` on
    /// `apps/deployments` and `batch/cronjobs` in one namespace.
    ///
    /// It REFUSES when `kubectl diff -k` reports drift, because the
    /// manifest changes it is skipping are exactly the ones it cannot
    /// apply — silently running the cheap lane against a changed tree is
    /// the failure this flag would otherwise introduce.
    pub image_only: bool,
    /// The immutable `YY.M.D` or `YY.M.D-hotfix.N` tag to
    /// roll onto. Required for a roll —
    /// `None` is rejected (we never guess the latest tag); only the
    /// `--restart-only` path, which changes no image, runs without it.
    pub tag: Option<String>,
    /// Release workflow's immutable image-repository to digest manifest.
    pub image_digests: Option<PathBuf>,
    /// Withdraw the roll's authority to write the web GSA's self-signing
    /// binding: verify it, and refuse when it is absent instead of granting
    /// it. For an operator under a no-IAM-changes rule — reading the policy
    /// needs `getIamPolicy`, writing it needs `setIamPolicy`, and this flag
    /// keeps the roll inside the first. It declines the write, never the
    /// check; see [`ensure_web_signing_iam`].
    pub assert_signing_iam: bool,
    /// Run the ship as a child process whose whole transcript goes to
    /// `log_sink`, leaving only a one-line verdict on the console. For a
    /// runner whose log is public. See [`super::ship_log`].
    pub quiet: bool,
    /// Where `--quiet` writes the transcript: `gs://BUCKET[/PREFIX]` or
    /// `file:///DIR`.
    pub log_sink: Option<String>,
}

fn restart_deployments() -> &'static [&'static str] {
    &[WEB_DEPLOYMENT, WORKFLOWS_DEPLOYMENT]
}

fn rollout_wait_deployments() -> &'static [&'static str] {
    restart_deployments()
}

/// The Deployments that run the `web` binary and so answer to its boot
/// invariants.
fn web_binary_deployments() -> &'static [&'static str] {
    &[WEB_DEPLOYMENT]
}

/// Load coordinates and secret key names from the explicit cloud project or
/// local tree. Neither path reads credential values or decrypts SOPS.
pub fn run_ship(opts: &ShipOpts) -> Result<()> {
    let cloud_root;
    let (root, deployment) = if let Some(project) = &opts.project_id {
        ensure!(
            opts.deployments_dir.is_none(),
            "--project-id conflicts with --deployments-dir"
        );
        if ship_lane(opts) != ShipLane::RestartOnly {
            let tag = opts.tag.as_deref().context("release roll requires --tag")?;
            registry::validate_release_tag(tag)?;
        }
        let deployment = super::deployment_config::load(project, &opts.deployment)?;
        // Asset checks use the CLI's embedded inventory when no source tree exists.
        cloud_root = tempfile::tempdir().context("create cloud ship scratch directory")?;
        (cloud_root.path().to_path_buf(), deployment)
    } else {
        let root = super::deployments::root(opts.deployments_dir.as_deref())?;
        let deployment = super::deployments::Deployment::load(&root, &opts.deployment)?;
        (root, deployment)
    };
    run_selected_ship(&root, &deployment, opts)
}

/// Both configuration sources use the same rollout validation and execution.
pub(super) fn run_selected_ship(
    root: &Path,
    deployment: &super::deployments::Deployment,
    opts: &ShipOpts,
) -> Result<()> {
    let cfg = ShipConfig::from_deployment(deployment)?;
    // Prod must know where public images are served from before any pod
    // rolls onto the new tag — enforced for both the roll and the
    // restart-only push, since both recreate pods that read it.
    require_asset_base_url(deployment)?;
    let lane = ship_lane(opts);
    if opts.dry_run {
        return offline_ship_plan(&cfg, deployment, opts, lane);
    }
    // Held until the ship returns: dropping it deletes the scratch
    // kubeconfig and registry login.
    let _credentials = ShipCredentials::cluster_and_registry(&cfg)?;
    if lane_requires_asset_preflight(lane) {
        // The applicable inventory — on-disk markdown when `server/content` is
        // a directory, otherwise the references embedded in this CLI — is
        // checked in the selected bucket before either rollout lane mutates
        // the cluster. A restart changes no image or content version.
        verify_assets_bucket(root, deployment)?;
    }
    match lane {
        ShipLane::ImageOnly => image_only(&cfg, deployment, opts, root),
        ShipLane::RestartOnly => restart_only(&cfg, opts.dry_run),
        ShipLane::Roll => roll(&cfg, deployment, opts, root),
    }
}

fn cloud_context_command(cfg: &ShipConfig) -> Command {
    let mut command = Command::new("gcloud");
    command.args([
        "container",
        "clusters",
        "get-credentials",
        &cfg.cluster,
        "--location",
        &cfg.location,
        "--project",
        &cfg.project_id,
    ]);
    command
}

/// The cluster and registry credentials a live cloud operation holds for itself.
///
/// An operator's shell is not a safe source for either. The CLI loads
/// `.devx/env`, whose `KUBECONFIG` names the local KIND cluster, and cosign
/// reads registry logins from Docker's config, which `gcloud` does not fill.
/// Both live in one scratch directory, deleted when this value drops.
pub(super) struct ShipCredentials {
    dir: tempfile::TempDir,
}

/// The scratch Docker config of the ship in progress. Only the `cosign`
/// children read it; every other child keeps the operator's own.
static COSIGN_DOCKER_CONFIG: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

impl ShipCredentials {
    /// Fetch the row's cluster credentials into a scratch kubeconfig that
    /// every later `kubectl` and `gcloud` child reads, whatever `KUBECONFIG`
    /// the shell or a dotenv file set.
    pub(super) fn cluster(cfg: &ShipConfig) -> Result<Self> {
        let dir = tempfile::tempdir().context("create ship credentials scratch directory")?;
        // The CLI runs on one thread here, and every child — including a
        // `--quiet` ship's — inherits the process environment.
        std::env::set_var("KUBECONFIG", dir.path().join("kubeconfig"));
        exec(false, &mut cloud_context_command(cfg))?;
        if let Some(mut rename) = context_rename_command(cfg) {
            exec(false, &mut rename)?;
        }
        Ok(Self { dir })
    }

    /// [`ShipCredentials::cluster`], plus a short-lived registry login for
    /// `cosign` when the row's images live in a Google registry.
    fn cluster_and_registry(cfg: &ShipConfig) -> Result<Self> {
        let credentials = Self::cluster(cfg)?;
        if let Some(host) = artifact_registry_host(&cfg.registry) {
            let docker_config = credentials.dir.path().join("docker");
            write_registry_login(&docker_config, host, &gcloud_access_token()?)?;
            *COSIGN_DOCKER_CONFIG
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(docker_config);
            eprintln!("==> cosign uses a scratch registry login for {host}");
        }
        Ok(credentials)
    }
}

impl Drop for ShipCredentials {
    fn drop(&mut self) {
        *COSIGN_DOCKER_CONFIG
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

/// `cosign verify` for one signed digest, reading `docker_config` for its
/// registry login when the ship holds one.
fn cosign_verify_command(signed_ref: &str, docker_config: Option<&Path>) -> Command {
    let mut command = Command::new("cosign");
    command.args([
        "verify",
        "--certificate-oidc-issuer=https://token.actions.githubusercontent.com",
        "--certificate-identity=https://github.com/neon-law/navigator/.github/workflows/deploy.yml@refs/heads/main",
        signed_ref,
    ]);
    if let Some(docker_config) = docker_config {
        command.env("DOCKER_CONFIG", docker_config);
    }
    command
}

/// `get-credentials` always names its context `gke_<project>_<location>_<cluster>`.
/// A row that pins another `NAVIGATOR_GKE_CONTEXT` gets it renamed to match.
fn context_rename_command(cfg: &ShipConfig) -> Option<Command> {
    let derived = derived_context(&cfg.project_id, &cfg.location, &cfg.cluster);
    if cfg.context == derived {
        return None;
    }
    let mut command = Command::new("kubectl");
    command.args(["config", "rename-context", &derived, &cfg.context]);
    Some(command)
}

/// The registry host when it is Artifact Registry or Container Registry, the
/// only hosts a `gcloud` access token logs in to.
fn artifact_registry_host(registry: &str) -> Option<&str> {
    let host = registry.split('/').next()?;
    (host.ends_with("-docker.pkg.dev") || host == "gcr.io" || host.ends_with(".gcr.io"))
        .then_some(host)
}

fn gcloud_access_token() -> Result<String> {
    let out = Command::new("gcloud")
        .args(["auth", "print-access-token"])
        .output()
        .context("run gcloud auth print-access-token")?;
    ensure!(
        out.status.success(),
        "gcloud auth print-access-token failed; run `gcloud auth login` and retry: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let token = String::from_utf8(out.stdout).context("gcloud access token is not UTF-8")?;
    let token = token.trim();
    ensure!(!token.is_empty(), "gcloud printed an empty access token");
    Ok(token.to_owned())
}

/// Docker's `config.json` holding one `oauth2accesstoken` login.
fn registry_login_config(host: &str, token: &str) -> String {
    use base64::Engine as _;
    let auth =
        base64::engine::general_purpose::STANDARD.encode(format!("oauth2accesstoken:{token}"));
    serde_json::json!({ "auths": { host: { "auth": auth } } }).to_string()
}

fn write_registry_login(docker_config: &Path, host: &str, token: &str) -> Result<()> {
    use std::io::Write as _;
    fs::create_dir_all(docker_config).context("create scratch DOCKER_CONFIG")?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(docker_config.join("config.json"))
        .context("create scratch registry login")?;
    file.write_all(registry_login_config(host, token).as_bytes())
        .context("write scratch registry login")
}

/// Build a useful local plan without credentials, network access, or a
/// cluster. It validates the selected row and renders the same coordinates
/// the live path will use, but explicitly leaves every external preflight to
/// the subsequent live run.
fn offline_ship_plan(
    cfg: &ShipConfig,
    deployment: &super::deployments::Deployment,
    opts: &ShipOpts,
    lane: ShipLane,
) -> Result<()> {
    if lane_requires_asset_preflight(lane) {
        require_assets_bucket_coordinate(deployment)?;
    }

    let mut local_validation = Vec::new();
    let mut operations = Vec::new();
    match lane {
        ShipLane::RestartOnly => {
            operations.extend(
                restart_deployments()
                    .iter()
                    .map(|name| format!("restart deployment/{name}")),
            );
        }
        ShipLane::ImageOnly => {
            let tag = image_only_tag(opts.tag.as_deref())?;
            local_validation.push("selected release tag is valid".to_owned());
            let substitutions = deployment_substitutions(deployment, tag)?;
            let manifests = render_manifests_with(&substitutions)?;
            let count = validate_rendered_tree(manifests.path())?;
            local_validation.push(format!("parsed {count} rendered YAML documents"));
            operations
                .push("set configured release image on web and workflow Deployments".to_owned());
            operations.push("re-pin Navigator CronJob images".to_owned());
            operations.push("wait for web and workflow rollouts".to_owned());
            operations.push("re-register the Restate worker".to_owned());
            operations.push("verify public assets and smoke-check the public host".to_owned());
        }
        ShipLane::Roll => {
            let tag = opts.tag.as_deref().ok_or_else(|| anyhow!(
                "`--tag <YY.M.D|YY.M.D-hotfix.N>` is required: name the published release to roll onto"
            ))?;
            registry::validate_release_tag(tag)?;
            local_validation.push("selected release tag is valid".to_owned());
            require_ordered_service_prerequisites(deployment)?;
            require_dbos_schedule_prerequisites(deployment)?;
            let substitutions = deployment_substitutions(deployment, tag)?;
            let rendered = render_release_plan(&substitutions, cfg, deployment)?;
            let count = validate_rendered_tree(rendered.application.path())?;
            validate_yaml_documents(&rendered.collector, "rendered collector")?;
            local_validation.push(format!(
                "parsed {count} rendered YAML documents and collector YAML"
            ));
            operations.extend(ordered_release_steps().iter().map(|step| match step {
                ReleaseStep::Postgres => "reconcile PostgreSQL StatefulSet".to_owned(),
                ReleaseStep::PostgresRoles => "run PostgreSQL roles Job".to_owned(),
                ReleaseStep::NavigatorMigrations => "run Navigator migrations Job".to_owned(),
                ReleaseStep::ForgejoDatabase => "run Forgejo database Job".to_owned(),
                ReleaseStep::Forgejo => "reconcile Forgejo StatefulSet".to_owned(),
                ReleaseStep::Application => {
                    if rendered.mirror_enabled {
                        "require clean mirror reconciliation; apply web, workflows, and recurring mirror recovery".to_owned()
                    } else {
                        "reconcile Navigator web and workflow Deployments".to_owned()
                    }
                }
            }));
            for cronjob in retired_cronjobs(deployment)? {
                operations.push(format!(
                    "delete the Restate CronJob {cronjob} now hosted by DBOS"
                ));
            }
            for name in retired_brand_certificates() {
                operations.push(format!(
                    "delete the ManagedCertificate {name} now carried by \
                     {FIRM_CERTIFICATE_NAME}"
                ));
            }
            operations.push("pin Navigator CronJob images to the selected tag".to_owned());
            operations.push("wait for rollouts and re-register the Restate worker".to_owned());
            operations.push("verify public assets and smoke-check the public host".to_owned());
        }
    }
    print_offline_ship_plan(
        lane,
        &local_validation,
        &operations,
        opts.quiet,
        opts.project_id.is_some(),
    );
    Ok(())
}

fn deployment_substitutions(
    deployment: &super::deployments::Deployment,
    tag: &str,
) -> Result<Vec<Substitution>> {
    resolve_substitutions_for_deployment(&deployment.name, tag, |key| {
        deployment.coordinates.get(key).cloned()
    })
}

fn require_assets_bucket_coordinate(deployment: &super::deployments::Deployment) -> Result<()> {
    ensure!(
        deployment
            .coordinates
            .get(ASSETS_BUCKET_KEY)
            .is_some_and(|value| !value.trim().is_empty()),
        "{ASSETS_BUCKET_KEY} must be set in deployments/{}/config.toml before shipping",
        deployment.name
    );
    Ok(())
}

fn print_offline_ship_plan(
    lane: ShipLane,
    local_validation: &[String],
    operations: &[String],
    quiet: bool,
    cloud_configuration: bool,
) {
    if cloud_configuration {
        eprintln!("configuration fetched from the explicit GCP project; rollout checks below remain offline");
    }
    if quiet {
        eprintln!("OFFLINE SHIP PLAN: local plan valid");
        eprintln!("  live authentication, context, IAM, drift, image, secret, and service checks were not performed");
        eprintln!("  no remote transcript was uploaded; no cloud or cluster commands were run");
        return;
    }
    eprintln!(
        "OFFLINE SHIP PLAN ({})",
        match lane {
            ShipLane::Roll => "roll",
            ShipLane::ImageOnly => "image-only",
            ShipLane::RestartOnly => "restart-only",
        }
    );
    for check in local_validation {
        eprintln!("  local validation: {check}");
    }
    for operation in operations {
        eprintln!("  would: {operation}");
    }
    eprintln!("  not checked: live authentication, cluster context, IAM, manifest drift, live Secret and service objects, asset availability, image availability, Restate registration, and public endpoints");
    eprintln!("  no cloud or cluster commands were run; execute the live ship command to perform those checks and apply changes");
}

fn validate_rendered_tree(root: &Path) -> Result<usize> {
    let mut stack = vec![root.to_path_buf()];
    let mut documents = 0;
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("read rendered manifest directory {}", directory.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("yaml" | "yml")
            ) {
                let text = fs::read_to_string(&path)
                    .with_context(|| format!("read rendered manifest {}", path.display()))?;
                documents += validate_yaml_documents(&text, &path.display().to_string())?;
            }
        }
    }
    ensure!(
        documents > 0,
        "rendered manifest tree contains no YAML documents"
    );
    Ok(documents)
}

fn validate_yaml_documents(text: &str, source: &str) -> Result<usize> {
    use serde::Deserialize;
    let mut count = 0;
    for document in serde_yaml::Deserializer::from_str(text) {
        let value = serde_json::Value::deserialize(document)
            .with_context(|| format!("parse YAML document in {source}"))?;
        if value.is_null() {
            continue;
        }
        ensure!(
            value.is_object(),
            "YAML document in {source} must be a mapping"
        );
        count += 1;
    }
    Ok(count)
}

/// Which of the three lanes an `ops ship` invocation selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShipLane {
    /// `--image-only`: move every navigator image to `--tag`, nothing else.
    ImageOnly,
    /// `--restart-only`: recreate the pods so they re-read a rotated Secret.
    RestartOnly,
    /// The full roll.
    Roll,
}

/// Resolve the lane from the flags, in one place rather than as a fall-through
/// chain inside [`run_ship`], so the precedence is provable without a cluster:
/// `--image-only` is the narrowest lane and so wins over `--restart-only`, and
/// a bare `ops ship` is the full roll.
fn ship_lane(opts: &ShipOpts) -> ShipLane {
    if opts.image_only {
        ShipLane::ImageOnly
    } else if opts.restart_only {
        ShipLane::RestartOnly
    } else {
        ShipLane::Roll
    }
}

fn lane_requires_asset_preflight(lane: ShipLane) -> bool {
    matches!(lane, ShipLane::ImageOnly | ShipLane::Roll)
}

fn lane_requires_post_roll_asset_verification(lane: ShipLane) -> bool {
    matches!(lane, ShipLane::ImageOnly | ShipLane::Roll)
}

/// Where the post-roll check reads the asset references it probes the public
/// origin for. Decided once from the selected root so the live lane and the
/// dry-run line cannot disagree about it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PostRollAssetRefs {
    /// A source checkout: walk `server/content` under the root, the complete
    /// set `navigator ops assets verify` reads.
    ContentTree(PathBuf),
    /// A deploy-only tree (`--deployments-dir` pointing at a checkout that
    /// carries `deployments/` and nothing else) has no `server/content`. The
    /// references embedded in this binary stand in: the workshop markdown,
    /// the gallery variants, and the licensed faces — everything but the
    /// blog's markdown, which exists only on disk.
    Embedded {
        /// The directory that was looked for and not found, named in the
        /// operator warning so a source checkout with a stray root is not
        /// mistaken for a deploy-only tree.
        missing: PathBuf,
    },
}

impl PostRollAssetRefs {
    /// The clause the dry-run line carries so an operator sees which set a
    /// live run from this root would probe.
    fn describe(&self) -> String {
        match self {
            Self::ContentTree(content_root) => {
                format!("references from `{}`", content_root.display())
            }
            Self::Embedded { missing } => format!(
                "`{}` is absent, so the references embedded in this CLI",
                missing.display()
            ),
        }
    }
}

/// `server/content` under `root` when it is a directory, else the embedded set.
/// A file at that path counts as absent: there is nothing to walk.
fn post_roll_asset_refs(root: &Path) -> PostRollAssetRefs {
    let content_root = root.join("server/content");
    if content_root.is_dir() {
        PostRollAssetRefs::ContentTree(content_root)
    } else {
        PostRollAssetRefs::Embedded {
            missing: content_root,
        }
    }
}

/// Finish a lane that changed the deployed image: Restate must see the new
/// handler list first, then the public origin must prove every asset the
/// rolled binary can request. Both verifiers are injected so this ordering,
/// the choice between them, and their failure boundary stay unit-testable
/// without a cluster or network: `verify` walks a source checkout's
/// `server/content`; `verify_embedded` probes the same origin for the
/// references this binary embeds when the root carries no such tree (the
/// documented deploy-only invocation, ENG-552). Neither is skipped — a
/// missing tree changes the reference source, never whether the origin is
/// checked — and either failing blocks completion.
fn finish_roll<R, F, E>(
    lane: ShipLane,
    cfg: &ShipConfig,
    root: &Path,
    dry_run: bool,
    reregister: R,
    verify: F,
    verify_embedded: E,
) -> Result<()>
where
    R: FnOnce() -> Result<()>,
    F: FnOnce(&Path, &str) -> Result<()>,
    E: FnOnce(&str) -> Result<()>,
{
    reregister()?;
    if lane_requires_post_roll_asset_verification(lane) {
        let refs = post_roll_asset_refs(root);
        if dry_run {
            eprintln!(
                "DRY-RUN: would verify public assets at {} after the roll ({})",
                cfg.asset_base_url,
                refs.describe()
            );
        } else {
            match refs {
                PostRollAssetRefs::ContentTree(content_root) => {
                    verify(&content_root, &cfg.asset_base_url)
                }
                PostRollAssetRefs::Embedded { missing } => {
                    eprintln!(
                        "WARN: `{}` is not a directory, so this is a deploy-only tree: asset \
                         checks use the inventory embedded in this CLI — the workshop images, \
                         the gallery variants, the brand media, and the licensed fonts — and \
                         the post-roll check probes {} for that inventory. References held \
                         only by the blog markdown are not in this inventory. Run \
                         `navigator ops assets verify` from a source checkout for the complete set.",
                        missing.display(),
                        cfg.asset_base_url
                    );
                    verify_embedded(&cfg.asset_base_url)
                }
            }
            .with_context(|| {
                format!(
                    "post-roll public asset verification failed for deployment `{}`",
                    cfg.name
                )
            })?;
        }
    }
    Ok(())
}

/// The no-rebuild push: restart service deployments so the pods re-read
/// a rotated Secret value. Pods cache `envFrom` at start and never
/// reload, so a rotation is invisible until the pod is recreated.
fn restart_only(cfg: &ShipConfig, dry_run: bool) -> Result<()> {
    require_tools(&["kubectl", "cosign"])?;
    require_auth(&["gcloud"])?;
    restart_only_steps(cfg, dry_run)
}

/// The restart lane past its preflight. Split from [`restart_only`] so a
/// `--dry-run` test drives the whole sequence: every step below prints instead
/// of shelling out under dry-run, so the two probes above — which need
/// `kubectl` on PATH and an authenticated `gcloud` — are the only part of the
/// lane a unit test cannot execute.
fn restart_only_steps(cfg: &ShipConfig, dry_run: bool) -> Result<()> {
    verify_context(cfg, dry_run)?;
    eprintln!("==> no-rebuild push: rollout restart service deployments (Secret rotation)");
    let mut cmd = kubectl(cfg);
    cmd.arg("rollout").arg("restart");
    for deployment in restart_deployments() {
        cmd.arg(format!("deployment/{deployment}"));
    }
    exec(dry_run, &mut cmd)?;
    wait_rollouts(cfg, dry_run, "120s", rollout_wait_deployments())?;
    eprintln!(
        "==> restart complete. VERIFY on the third-party side (the pod will 2xx against a \
         valid-but-wrong key) — compare upstream stats before/after."
    );
    Ok(())
}

/// Move every navigator image to `tag` and change nothing else.
///
/// FOUR STEPS, NOT ONE. "Set the image and wait" is the obvious reading and
/// it is wrong twice over:
///
///   * The seven scheduled-job `CronJob`s carry their own image. Skip them and
///     the web pods run the new version while every scheduled job keeps running
///     the old one — a skew nothing reports, because `CronJob`s do not roll.
///   * Restate resolves a worker's handler list at registration. A new
///     `workflows-service` image that Restate has not rediscovered leaves a
///     stale list, and `reregister`'s own error says webhook submissions then
///     fail SILENTLY. That is a live wrong-behaviour bug, not a cosmetic gap.
///
/// What it deliberately does NOT do is the expensive half of a full roll: the
/// web GSA's self-signing IAM re-assert, and `apply -k` over the whole
/// rendered tree. Both are idempotent no-ops on a routine version bump, and
/// both are what make the full roll's credential large.
fn image_only(
    cfg: &ShipConfig,
    deployment: &super::deployments::Deployment,
    opts: &ShipOpts,
    root: &Path,
) -> Result<()> {
    // Arguments before the machine. A missing or malformed `--tag` is knowable
    // without kubectl installed, gcloud authenticated, or a reachable cluster,
    // and the scheduled caller that gets it wrong should read that rather than
    // a toolchain complaint.
    let tag = image_only_tag(opts.tag.as_deref())?;

    require_tools(&["kubectl"])?;
    require_auth(&["gcloud"])?;
    image_only_steps(cfg, deployment, tag, opts.dry_run, root)
}

/// The narrow lane past its preflight, split from [`image_only`] for the same
/// reason [`restart_only_steps`] is: under `--dry-run` every step below prints
/// its command instead of running it, so a unit test walks the whole four-step
/// sequence — context cross-check, drift refusal, the image writes, the
/// `CronJob` re-pin, the rollout wait, the Restate re-register — with no
/// cluster, no `kubectl`, and no `gcloud`.
fn image_only_steps(
    cfg: &ShipConfig,
    deployment: &super::deployments::Deployment,
    tag: &str,
    dry_run: bool,
    root: &Path,
) -> Result<()> {
    verify_context(cfg, dry_run)?;

    refuse_on_manifest_drift(cfg, deployment, tag, dry_run)?;

    eprintln!("==> image-only roll: every navigator image → {tag}");
    for (deployment, image) in image_only_image_writes(cfg, tag) {
        let mut cmd = kubectl(cfg);
        cmd.arg("set")
            .arg("image")
            .arg(format!("deployment/{deployment}"))
            .arg(format!("{deployment}={image}"));
        exec(dry_run, &mut cmd)?;
    }

    pin_cronjob_images(cfg, tag, None, dry_run)?;
    wait_rollouts(cfg, dry_run, ROLLOUT_TIMEOUT, rollout_wait_deployments())?;
    finish_roll(
        ShipLane::ImageOnly,
        cfg,
        root,
        dry_run,
        || reregister(cfg, dry_run),
        crate::assets::verify_public_asset_origin,
        crate::assets::verify_public_asset_origin_embedded,
    )?;

    eprintln!(
        "==> image-only roll complete: {tag} live in {}",
        cfg.project_id
    );
    Ok(())
}

/// The `(Deployment, image)` pairs the narrow lane writes, in order.
///
/// One `set image` per Deployment rather than one combined call: the container
/// name inside each differs from the other, so a single invocation would need
/// `container=image` pairs that only hold for one of them. Two writes, two
/// `ReplicaSets`, both pinned to the same tag — which is what a test asserts
/// here, because a skew between them is the defect this lane must not
/// introduce.
fn image_only_image_writes(cfg: &ShipConfig, tag: &str) -> [(&'static str, String); 2] {
    [
        (WEB_DEPLOYMENT, cfg.web_image(tag)),
        (WORKFLOWS_DEPLOYMENT, cfg.workflows_image(tag)),
    ]
}

/// The release tag the narrow lane rolls onto, or the reason it cannot.
///
/// Two refusals, not one. The lane's only job is to move images to a named
/// release, so an absent `--tag` leaves it with nothing to do — and it never
/// guesses the latest published tag, exactly as the full roll never does.
/// A present tag still has to be a real `YY.M.D` or
/// `YY.M.D-hotfix.N` release name: the
/// scheduled caller derives the tag from a clock, and a derivation that
/// produces `2026-08-17` or an empty string must fail here rather than as a
/// pod pulling an image that does not exist.
fn image_only_tag(tag: Option<&str>) -> Result<&str> {
    let Some(tag) = tag else {
        bail!("--image-only requires --tag: the lane exists to move images to a named release");
    };
    registry::validate_release_tag(tag)?;
    Ok(tag)
}

/// Abort the narrow lane when the rendered tree differs from the cluster.
///
/// `kubectl diff` exits 0 for no drift, 1 for drift, and >1 for a real error
/// (unreachable context, auth failure, kustomize build error). The narrow lane
/// treats 1 as fatal — the opposite of the full roll, which treats it as the
/// normal signal that there is something to apply.
fn refuse_on_manifest_drift(
    cfg: &ShipConfig,
    deployment: &super::deployments::Deployment,
    tag: &str,
    dry_run: bool,
) -> Result<()> {
    if dry_run {
        eprintln!("DRY-RUN: would refuse the image-only roll if `kubectl diff -k` reported drift");
        return Ok(());
    }
    // Reuse the selected configuration so all lanes render the same target.
    let coordinate = |key: &str| deployment.coordinates.get(key).cloned();
    let subs = resolve_substitutions_for_deployment(&cfg.name, tag, coordinate)?;
    let rendered = render_manifests_with(&subs)?;
    let target = rendered.path().join(GKE_KUSTOMIZE_SUBPATH);
    let status = kubectl_ctx(cfg)
        .arg("diff")
        .arg("-k")
        .arg(&target)
        .status()
        .with_context(|| format!("spawn kubectl diff -k {}", target.display()))?;
    drift_verdict(status.code())
}

/// What one `kubectl diff -k` exit code means to the narrow lane.
///
/// Split out from the shell-out because the mapping is the whole safety
/// property and the shell-out is not testable: `kubectl diff` exits 0 for no
/// drift, 1 for drift, and >1 for a real error (unreachable context, auth
/// failure, kustomize build error). Only 0 may proceed.
///
/// 1 is fatal here and normal in the full roll — that inversion is the reason
/// this lane can hold a small credential, so it is asserted rather than left
/// to a reading of the code. `None` (killed by a signal, so no code at all) is
/// an error too: it is the absence of an answer, not a "no drift".
fn drift_verdict(code: Option<i32>) -> Result<()> {
    match code {
        Some(0) => Ok(()),
        Some(1) => bail!(
            "the rendered manifests differ from the cluster, so this is not a version bump. \
             `--image-only` applies no manifest change by design — running it here would move \
             the images and silently leave the rest of the diff unapplied. Roll with a full \
             `ops ship` instead."
        ),
        Some(other) => {
            bail!("kubectl diff -k failed with exit {other} — the cluster was not reached")
        }
        None => bail!("kubectl diff -k was killed by a signal — the cluster was not reached"),
    }
}

/// Roll the cluster onto an already-published immutable release tag.
/// CI built and published the images; this only updates the cluster:
/// resolve the tag → confirm the Secret satisfies the new binary's boot
/// invariants → pin service deployments AND every trigger `CronJob` to
/// that tag → wait → re-register the worker with Restate → smoke-check.
/// No build, no push, no skew — every navigator image in sync at one tag.
fn roll(
    cfg: &ShipConfig,
    deployment: &super::deployments::Deployment,
    opts: &ShipOpts,
    root: &Path,
) -> Result<()> {
    // Resolve every service input before checking credentials or contacting
    // the cluster. Missing publisher/service configuration is actionable from
    // the repository alone and must not become a late, expensive rollout.
    require_ordered_service_prerequisites(deployment)?;
    require_dbos_schedule_prerequisites(deployment)?;
    require_tools(&["kubectl"])?;
    // Authenticated, not just installed: gcloud carries the GKE
    // credentials the pinned kubectl context resolves against.
    require_auth(&["gcloud"])?;
    let dry_run = opts.dry_run;

    // 1. Pre-flight — confirm the prod context resolves before any call.
    verify_context(cfg, dry_run)?;

    // 1b. The immutable release tag to roll — always an explicit `--tag`, so
    //     the operator names the exact published release rather than letting
    //     the roll guess. A malformed tag is knowable without reaching GCP at
    //     all, so it is settled before the IAM check below: otherwise a typo
    //     reports an IAM failure. Present service deployments get the SAME tag.
    let Some(tag) = opts.tag.as_deref() else {
        bail!(
            "`--tag <YY.M.D|YY.M.D-hotfix.N>` is required: name the published release to roll onto. \
             We never guess the latest tag; pass the tag from the deploy hand-off \
             (or use `--restart-only` to re-read a rotated Secret without changing the image)."
        );
    };
    registry::validate_release_tag(tag)?;
    let tag = tag.to_string();

    // 1c. Assert the web GSA's self-signing IAM. Independent of the image tag,
    //     so it belongs before any rollout: document downloads issue GCS
    //     signed URLs, which the pod can only mint if its GSA holds
    //     serviceAccountTokenCreator on itself. Verify-then-assert, so the
    //     steady state is a read — see `ensure_web_signing_iam`.
    ensure_web_signing_iam(cfg, dry_run, signing_iam_authority(opts))?;

    // 2. Name the images this tag resolves to.
    let web_remote = cfg.web_image(&tag);
    let workflows_remote = cfg.workflows_image(&tag);
    eprintln!(
        "==> rolling {} ({}) onto {tag}\n      {web_remote}\n      {workflows_remote}",
        cfg.project_id, cfg.context
    );

    // 2b. The published-image check runs at step 4a over the BUILT manifest
    //     stream, which names the trigger CronJobs as well as the Deployments.
    //     Everything between here and step 4a is
    //     local work — render, prune, `kubectl kustomize` — so an unpublished
    //     tag still aborts before the first cluster read.
    let coordinate = |key: &str| deployment.coordinates.get(key).cloned();

    // 3. Render the embedded manifest tree with the deployment's NAVIGATOR_*
    //    coordinates and the tag being rolled. Pure local work — nothing
    //    reaches the cluster until step 5, which is what lets step 4 abort
    //    for free.
    let subs = resolve_substitutions_for_deployment(&deployment.name, &tag, coordinate)?;
    let rendered = render_release_plan(&subs, cfg, deployment)?;
    let target = rendered.application.path().join(GKE_KUSTOMIZE_SUBPATH);

    // 3b. Render the Secret Manager catalog for THIS deployment: drop every
    //     entry it does not write. A deployment that declines DocuSign cannot
    //     retain a catalog entry for it — and must not be given a placeholder
    //     credential to make
    //     the reference resolve, since `portal::signature` reaches its stub
    //     only through genuine absence. Same for an object scoped to another
    //     deployment, which this one is forbidden to hold at all.
    omit_unwritten_objects(
        &target,
        &super::deployments::skipped_projected_objects(deployment)?,
    )?;

    // 4. Confirm the prod Secret satisfies the new binary's invariants —
    //    BEFORE the reconcile mutates anything. An unsatisfied requirement
    //    aborts a ship that has touched nothing; running this after the
    //    apply strands the cluster on a half-rolled state (which is exactly
    //    how the 26.7.15 ship wedged the git tier).
    let mut manifests = kustomize_build(&target)?;

    // 4a. Confirm every image the render pinned is actually published at this
    //     tag, BEFORE the first cluster read below and long before the apply.
    //     Applying a manifest that names a missing tag wedges a Deployment in
    //     ImagePullBackOff; a trigger CronJob is worse, because it has no
    //     rollout to wait on and no replica to report — it simply stops firing.
    //     `publish-triggers` is a `fail-fast: false` matrix, so one failed leg
    //     leaves exactly that hole while every other image publishes.
    //
    //     The list comes from the BUILT stream rather than from a hard-coded
    //     set of image names. That is the whole point: the reconcile applies
    //     this stream, so checking it cannot drift from what is about to be
    //     written — a trigger added to `../exports` is covered with no edit
    //     here. Reading the built
    //     stream rather than the manifest files is also what picks up the
    //     `web-image` patch, which overrides the base's registry-less tag.
    ensure_manifest_images_published(cfg, dry_run, &manifests, &tag)?;
    if !dry_run {
        let digest_file = opts
            .image_digests
            .as_deref()
            .ok_or_else(|| anyhow!("`--image-digests <FILE>` is required for a release roll"))?;
        pin_and_verify_manifest_images(
            cfg,
            rendered.application.path(),
            &manifests,
            &tag,
            digest_file,
        )?;
        manifests = kustomize_build(&target)?;
    }

    ensure_web_secret_envfrom(&manifests, &cfg.secret_name)?;
    ensure_secret_invariants(cfg, dry_run, &manifests)?;

    // 4b. …and confirm every catalog entry has an enabled Secret Manager
    //     version. This is metadata-only and runs in `--dry-run` too, which
    //     keeps the rendered catalog from drifting from what `ops secrets
    //     apply` can write.
    ensure_projected_objects_resolve(&manifests)?;
    ensure_ordered_service_objects(cfg, dry_run, dbos_enabled(deployment))?;

    // 5. Reconcile services in dependency order, then apply the application.
    reconcile_ordered_services(cfg, dry_run, &rendered, opts, &tag, &target)?;
    retire_dbos_hosted_cronjobs(cfg, dry_run, deployment)?;
    retire_firm_only_brand_certificates(cfg, dry_run)?;

    // 6. Wait on the rollouts the reconcile started. The apply in step 5
    //    already pinned every image to `tag` — the render substitutes it —
    //    so there is no `kubectl set image` to follow up with. One write
    //    means one ReplicaSet: no intermediate generation on an unpullable
    //    placeholder for a `maxSurge: 0` workload to be deleted for.
    // 5b. Pin every trigger CronJob to the same tag so a roll is atomic
    //     across ALL navigator images, not just the two services. CronJobs
    //     don't "roll" — the new image takes effect on the next scheduled
    //     run — so there is nothing to wait on.
    pin_cronjob_images(cfg, &tag, opts.image_digests.as_deref(), dry_run)?;

    // 6. Re-register the workers with Restate, then verify every public asset
    // after the new pods are live. The DevX notification worker must register
    // successfully: unlike the established workflows service, it has no prior
    // deployment to preserve on its first ship. A failed asset check returns
    // before the completion line below.
    finish_roll(
        ShipLane::Roll,
        cfg,
        root,
        dry_run,
        || reregister(cfg, dry_run),
        crate::assets::verify_public_asset_origin,
        crate::assets::verify_public_asset_origin_embedded,
    )?;

    // 7. Smoke-check the public surface (best-effort).
    smoke_check(cfg, dry_run);

    eprintln!("==> ship complete: {tag} live in {}", cfg.project_id);
    Ok(())
}

fn reconcile_ordered_services(
    cfg: &ShipConfig,
    dry_run: bool,
    rendered: &RenderedReleasePlan,
    opts: &ShipOpts,
    tag: &str,
    target: &Path,
) -> Result<()> {
    // Reconcile the collector first, followed by PostgreSQL, its roles and
    // migrations, Forgejo's database and service, then the application.
    let collector_changed = reconcile_collector_manifest(
        cfg,
        dry_run,
        &rendered.collector,
        rendered.application.path(),
    )?;
    if collector_changed {
        wait_rollouts(cfg, dry_run, "120s", &["otel-collector"])?;
    }
    run_ordered_release(|step| match step {
        ReleaseStep::Postgres => {
            let service = render_service_overlay(&rendered.application, cfg, "postgres")?;
            ensure_required_manifest(&service, "StatefulSet", "navigator-postgres")?;
            let recreating = grow_postgres_volume(cfg, dry_run, &service)?;
            if dry_run && recreating {
                // The live StatefulSet still carries the old, immutable claim
                // template, so its server-side diff would refuse the change
                // the real release makes after the orphaned delete.
                eprintln!(
                    "DRY-RUN: skipping the PostgreSQL diff; the release recreates \
                     statefulset/navigator-postgres before it applies"
                );
                return Ok(());
            }
            reconcile_manifests(cfg, dry_run, &service)?;
            wait_statefulset(cfg, dry_run, "navigator-postgres", ROLLOUT_TIMEOUT)
        }
        ReleaseStep::PostgresRoles => {
            let service = render_service_overlay(&rendered.application, cfg, "postgres/access")?;
            run_job_overlay(cfg, dry_run, &service, "navigator-postgres-roles", "15m")
        }
        ReleaseStep::NavigatorMigrations => {
            let service = render_service_overlay(&rendered.application, cfg, "postgres/migrate")?;
            pin_service_images(
                &rendered.application,
                &service,
                cfg,
                tag,
                opts.image_digests.as_deref(),
                dry_run,
            )?;
            run_job_overlay(cfg, dry_run, &service, "navigator-migrate", "15m")
        }
        ReleaseStep::ForgejoDatabase => {
            let service = render_service_overlay(&rendered.application, cfg, "forgejo/database")?;
            run_job_overlay(cfg, dry_run, &service, "navigator-forgejo-database", "15m")
        }
        ReleaseStep::Forgejo => {
            let service = render_service_overlay(&rendered.application, cfg, "forgejo")?;
            ensure_required_manifest(&service, "StatefulSet", "navigator-forgejo")?;
            reconcile_manifests(cfg, dry_run, &service)?;
            wait_statefulset(cfg, dry_run, "navigator-forgejo", ROLLOUT_TIMEOUT)
        }
        ReleaseStep::Application => {
            if rendered.mirror_enabled {
                let service =
                    render_service_overlay(&rendered.application, cfg, "postgres/mirror")?;
                pin_service_images(
                    &rendered.application,
                    &service,
                    cfg,
                    tag,
                    opts.image_digests.as_deref(),
                    dry_run,
                )?;
                run_job_overlay(
                    cfg,
                    dry_run,
                    &service,
                    "navigator-postgres-mirror-reconcile",
                    "30m",
                )?;
            }
            reconcile_manifests(cfg, dry_run, target)?;
            wait_rollouts(cfg, dry_run, ROLLOUT_TIMEOUT, rollout_wait_deployments())
        }
    })
}

/// The repository part of an image reference: `name@sha256:<digest>` loses
/// its digest, and `name:<tag>` loses its tag only when that colon comes after
/// the last `/`, so a registry `host:port` is never mistaken for a tag.
fn image_repository(image: &str) -> &str {
    if let Some((repository, _)) = image.split_once('@') {
        return repository;
    }
    let name_start = image.rfind('/').map_or(0, |slash| slash + 1);
    match image[name_start..].rfind(':') {
        Some(colon) => &image[..name_start + colon],
        None => image,
    }
}

/// What a ship does to one trigger `CronJob` container image.
#[derive(Debug, PartialEq, Eq)]
enum CronJobPin {
    /// Already on the signed image map's digest; nothing to write.
    Current,
    /// Re-point at this signed `repository@sha256:<digest>` reference.
    Repoint(String),
    /// Re-point at `target` once `repository` is confirmed to publish the tag.
    CheckTag { repository: String, target: String },
}

/// The pure decision behind [`pin_cronjob_images`]: `None` for an image this
/// deployment's registry does not publish. With the release's image digest
/// map, a `CronJob` on the map's digest is current and any other reference is
/// re-pointed at that digest; without one (or for an image the map does not
/// carry) the tag-only path re-points at `<repository>:<tag>`.
fn cronjob_pin_action(
    image: &str,
    prefix: &str,
    tag: &str,
    digests: Option<&std::collections::BTreeMap<String, String>>,
) -> Option<CronJobPin> {
    if !image.starts_with(prefix) {
        return None;
    }
    let reference = image_repository(image);
    let repository = reference.strip_prefix(prefix).unwrap_or(reference);
    if let Some(digest) = digests.and_then(|d| digest_for_image(d, repository, reference).ok()) {
        let signed = format!("{reference}@{digest}");
        return Some(if image == signed {
            CronJobPin::Current
        } else {
            CronJobPin::Repoint(signed)
        });
    }
    Some(CronJobPin::CheckTag {
        repository: repository.to_string(),
        target: format!("{reference}:{tag}"),
    })
}

/// Re-pin every navigator trigger `CronJob` to `tag`. Discovers the
/// `CronJobs` from the live cluster and re-points any container whose image
/// is one of ours (`<region>-docker.pkg.dev/<project>/<repo>/navigator-*`) — no hard-coded list,
/// so a newly added trigger is covered automatically.
///
/// With the release's signed image digest map (`--image-digests`), a trigger
/// already on its mapped digest counts as current and any other is re-pointed
/// at `repository@sha256:<digest>`. Without one, each image is re-pinned to
/// `repository:<tag>` only after confirming `tag` is actually published for
/// it; a trigger whose image hasn't published `tag` yet is skipped with a
/// warning rather than wedged in `ImagePullBackOff`.
fn pin_cronjob_images(
    cfg: &ShipConfig,
    tag: &str,
    digest_file: Option<&Path>,
    dry_run: bool,
) -> Result<()> {
    let prefix = format!("{}/", cfg.registry());
    if dry_run {
        eprintln!("DRY-RUN: would re-pin every {prefix}navigator-* CronJob image to {tag}");
        return Ok(());
    }
    let digests = digest_file.map(read_digest_manifest).transpose()?;
    let list = kubectl_list_json(cfg, "cronjobs")?;
    let Some(items) = list.get("items").and_then(serde_json::Value::as_array) else {
        eprintln!("==> no CronJobs found; nothing to pin");
        return Ok(());
    };
    let (mut pinned, mut current) = (0u32, 0u32);
    for item in items {
        let Some(name) = item
            .pointer("/metadata/name")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let containers = item
            .pointer("/spec/jobTemplate/spec/template/spec/containers")
            .and_then(serde_json::Value::as_array);
        let Some(containers) = containers else {
            continue;
        };
        for c in containers {
            let (Some(cname), Some(image)) = (
                c.get("name").and_then(serde_json::Value::as_str),
                c.get("image").and_then(serde_json::Value::as_str),
            ) else {
                continue;
            };
            let target = match cronjob_pin_action(image, &prefix, tag, digests.as_ref()) {
                None => continue,
                Some(CronJobPin::Current) => {
                    current += 1;
                    continue;
                }
                Some(CronJobPin::Repoint(target)) => target,
                Some(CronJobPin::CheckTag { repository, target }) => {
                    match registry::tag_exists(&cfg.registry, &repository, tag) {
                        Ok(true) => target,
                        Ok(false) => {
                            eprintln!(
                                "WARN: {repository} has no {tag} tag in the registry — leaving CronJob/{name} on {image}"
                            );
                            continue;
                        }
                        Err(error) => {
                            eprintln!(
                                "WARN: could not check whether {repository}:{tag} is published ({error}) — \
                                 leaving CronJob/{name} on {image}"
                            );
                            continue;
                        }
                    }
                }
            };
            exec(
                dry_run,
                kubectl(cfg)
                    .arg("set")
                    .arg("image")
                    .arg(format!("cronjob/{name}"))
                    .arg(format!("{cname}={target}")),
            )?;
            pinned += 1;
        }
    }
    eprintln!(
        "==> pinned {pinned} trigger CronJob image(s) to {tag}; {current} already on the signed digest"
    );
    Ok(())
}

/// 3 — reconcile the full manifest tree. Render the embedded GKE + base
/// manifests with the deployer's `NAVIGATOR_*` values into a throwaway
/// temp dir, `kubectl diff -k` it (surface drift), then — unless
/// `--dry-run` — `kubectl apply -k` it. The apply is unconditional, so
/// any structural manifest change (a renamed container, a new sidecar or
/// volume, an env-list edit) that landed in `main` reaches the cluster
/// instead of silently rotting. There is no external overlay folder and no
/// image-only fall-through: the CLI generates from env what a deployer used
/// to hand-keep in a private overlay.
///
/// The rendered temp dir is dropped on return, so the substituted manifests
/// never linger on disk. A missing required substitution var bails by name
/// before anything is written (see [`resolve_substitutions`]).
fn reconcile_manifests(cfg: &ShipConfig, dry_run: bool, target: &Path) -> Result<()> {
    eprintln!(
        "==> reconciling manifests — rendered embedded tree → {}",
        target.display()
    );
    reconcile_kustomize(dry_run, |verb| {
        let status = kubectl_ctx(cfg)
            .arg(verb)
            .arg("-k")
            .arg(target)
            .status()
            .with_context(|| format!("spawn kubectl {verb} -k {}", target.display()))?;
        Ok(status.code().unwrap_or(-1))
    })
}

/// Reconcile the rendered Collector manifest separately from the application
/// kustomization. A changed `ConfigMap` is a named release finding and applies
/// the checksum-annotated Deployment, which restarts the Collector when its
/// pipeline changed. An unchanged manifest is left alone.
fn reconcile_collector_manifest(
    cfg: &ShipConfig,
    dry_run: bool,
    manifest: &str,
    rendered_root: &Path,
) -> Result<bool> {
    let path = rendered_root.join("otel-collector.yaml");
    fs::write(&path, manifest)
        .with_context(|| format!("write rendered collector manifest {}", path.display()))?;
    let changed = reconcile_manifest_file(dry_run, |verb| {
        let status = kubectl_ctx(cfg)
            .arg(verb)
            .arg("-f")
            .arg(&path)
            .status()
            .with_context(|| format!("spawn kubectl {verb} -f {}", path.display()))?;
        Ok(status.code().unwrap_or(-1))
    })?;
    if changed {
        eprintln!(
            "==> collector manifest drift: rendered collector differs from the live ConfigMap/Deployment"
        );
    } else {
        eprintln!("==> collector manifest is converged");
    }
    Ok(changed)
}

/// Diff one manifest file and apply it only when the live object differs.
/// `kubectl diff` exit 1 means drift; greater values mean the cluster was not
/// checked and must abort the release.
fn reconcile_manifest_file<R>(dry_run: bool, mut run_verb: R) -> Result<bool>
where
    R: FnMut(&str) -> Result<i32>,
{
    let changed = match run_verb("diff") {
        Ok(0) => false,
        Ok(1) => true,
        Ok(code) => {
            bail!("kubectl diff -f failed (exit {code}); the collector manifest was NOT checked")
        }
        Err(error) => {
            return Err(error)
                .context("kubectl diff -f could not run; the collector manifest was NOT checked")
        }
    };
    if changed && !dry_run {
        let code = run_verb("apply")?;
        if code != 0 {
            bail!("kubectl apply -f failed (exit {code}); the collector was NOT fully reconciled");
        }
    } else if dry_run {
        eprintln!(
            "DRY-RUN: collector manifest rendered and diffed above; skipping `kubectl apply`"
        );
    }
    Ok(changed)
}

/// How long `kubectl rollout status` waits per Deployment after the apply.
/// Autopilot adds node-provisioning time to a roll, so 300s tripped on
/// healthy ships (26.10.4 `workflows-service`); a truly wedged pod still fails.
const ROLLOUT_TIMEOUT: &str = "600s";

/// Tries `kubectl diff -k` gets before the reconcile gives up. Its API-server
/// dry-run fails intermittently while Autopilot's resource mutator is
/// rewriting a Deployment (26.10.5: `workflows-service` "image: Required
/// value" on a manifest whose image was verified published a second earlier).
const DIFF_ATTEMPTS: u32 = 3;

/// Pause between diff attempts; zero in tests so they stay instant.
const DIFF_RETRY_BACKOFF: std::time::Duration = if cfg!(test) {
    std::time::Duration::ZERO
} else {
    std::time::Duration::from_secs(15)
};

/// Run the `diff` verb, retrying an exit code above 1 (the diff itself broke,
/// not drift) up to [`DIFF_ATTEMPTS`] times. Exit 0/1 and spawn failures
/// return at once: drift is a result, and an unspawnable kubectl will not heal.
fn diff_with_retries<R>(run_verb: &mut R) -> Result<i32>
where
    R: FnMut(&str) -> Result<i32>,
{
    let mut code = run_verb("diff")?;
    for attempt in 2..=DIFF_ATTEMPTS {
        if code <= 1 {
            break;
        }
        eprintln!(
            "==> kubectl diff -k exited {code}; retrying (attempt {attempt}/{DIFF_ATTEMPTS}) \
             in {}s",
            DIFF_RETRY_BACKOFF.as_secs()
        );
        std::thread::sleep(DIFF_RETRY_BACKOFF);
        code = run_verb("diff")?;
    }
    Ok(code)
}

/// Drive the reconcile's `kubectl -k` verbs in order. `run_verb` returns the
/// process exit code (or `Err` if kubectl could not be spawned); the real
/// ship shells out to `kubectl <verb> -k <target>`, tests pass a stub.
///
/// `kubectl diff` has three outcomes: exit 0 (no drift), 1 (drift found —
/// the normal, benign signal), and >1 (a real error: unreachable context,
/// auth failure, a kustomize build error). Exit 0/1 proceed; a >1 exit or a
/// spawn failure **aborts** the reconcile — the diff is the operator's
/// mandatory drift-review gate, so a diff that never ran must not let a
/// `--dry-run` exit 0 (mistaking a missing check for "no changes") or a live
/// run fall through to `apply` having skipped review. `apply` must likewise
/// exit 0 or the ship aborts (a partial reconcile is worse than none). Under
/// `--dry-run` the verb list stops at `diff` ([`reconcile_verbs`]), so
/// nothing is applied.
fn reconcile_kustomize<R>(dry_run: bool, mut run_verb: R) -> Result<()>
where
    R: FnMut(&str) -> Result<i32>,
{
    for verb in reconcile_verbs(dry_run) {
        match *verb {
            "apply" => {
                let code = run_verb("apply")?;
                if code != 0 {
                    bail!(
                        "kubectl apply -k failed (exit {code}); the cluster was NOT fully \
                         reconciled — inspect the output above before retrying"
                    );
                }
            }
            // `diff`: 0/1 are the expected outcomes (no-drift / drift); a
            // higher code or a spawn failure means the diff itself broke, so
            // the mandatory drift-review gate never ran — abort rather than
            // fall through to `apply` (live) or exit 0 (`--dry-run`).
            "diff" => {
                match diff_with_retries(&mut run_verb) {
                    Ok(0 | 1) => {}
                    Ok(code) => bail!(
                        "kubectl diff -k failed (exit {code}) after {DIFF_ATTEMPTS} attempts; the \
                     rendered manifests were NOT checked against the cluster — inspect the \
                     output above before retrying"
                    ),
                    Err(err) => return Err(err).context(
                        "kubectl diff -k could not run; the rendered manifests were NOT checked \
                         against the cluster",
                    ),
                }
            }
            _ => {}
        }
    }
    if dry_run {
        eprintln!(
            "DRY-RUN: manifests rendered and diffed above; skipping `kubectl apply` \
             (rendered dir removed on exit)"
        );
    }
    Ok(())
}

/// 7b — abort the ship if any web-binary boot requirement is
/// unsatisfied by both the Secret and that Deployment's own env. An
/// unsatisfied requirement crash-loops the new pod at boot
/// (`enforce_deployment_invariants`), so catching it here beats a
/// silently-stalled rollout. We never auto-patch: print the exact
/// `kubectl patch` and stop.
fn ensure_secret_invariants(cfg: &ShipConfig, dry_run: bool, manifests: &str) -> Result<()> {
    if dry_run {
        eprintln!(
            "DRY-RUN: checking required keys vs Secret + each web-binary Deployment env (read-only)"
        );
    }
    let secret_keys = secret_data_keys(cfg)?;
    check_secret_invariants(cfg, manifests, &secret_keys)
}

/// Abort before reconciliation unless the rendered `web` container receives
/// the deployment Secret through `envFrom`.
///
/// `ops secrets apply` is responsible for writing that Secret. Checking the
/// built stream, rather than the source patch, proves the pod contract that
/// `kubectl apply -k` will actually receive.
fn ensure_web_secret_envfrom(manifests: &str, secret_name: &str) -> Result<()> {
    use serde::Deserialize;
    for document in serde_yaml::Deserializer::from_str(manifests) {
        let value = serde_json::Value::deserialize(document)
            .context("parse a document of the rendered manifest stream")?;
        let is_web_deployment = value.get("kind").and_then(serde_json::Value::as_str)
            == Some("Deployment")
            && value
                .pointer("/metadata/name")
                .and_then(serde_json::Value::as_str)
                == Some(WEB_DEPLOYMENT);
        if !is_web_deployment {
            continue;
        }
        let receives_secret = value
            .pointer("/spec/template/spec/containers")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|containers| {
                containers.iter().any(|container| {
                    container.get("name").and_then(serde_json::Value::as_str) == Some("web")
                        && container
                            .get("envFrom")
                            .and_then(serde_json::Value::as_array)
                            .is_some_and(|sources| {
                                sources.iter().any(|source| {
                                    source
                                        .pointer("/secretRef/name")
                                        .and_then(serde_json::Value::as_str)
                                        == Some(secret_name)
                                })
                            })
                })
            });
        ensure!(
            receives_secret,
            "the rendered {WEB_DEPLOYMENT} `web` container does not envFrom the `{secret_name}` \
             Secret; refusing to ship a pod that cannot receive values written by ops secrets apply"
        );
        return Ok(());
    }
    bail!(
        "the rendered manifests define no {WEB_DEPLOYMENT} Deployment; refusing to ship a tree \
         whose Secret contract cannot be checked"
    )
}

/// One Secret Manager object a rendered `SecretProviderClass` catalogs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ProjectedObject {
    project_id: String,
    secret_id: String,
    version: String,
}

impl ProjectedObject {
    /// Parse a `resourceName`. The whole coordinate is read rather than just
    /// the object name: the project is what makes the check ask about the
    /// deployment actually being shipped, and the version is what the catalog
    /// pins — assuming `latest` would check something else.
    fn parse(resource_name: &str) -> Result<Self> {
        let parts: Vec<&str> = resource_name.split('/').collect();
        match parts.as_slice() {
            ["projects", project_id, "secrets", secret_id, "versions", version]
                if !project_id.is_empty() && !secret_id.is_empty() && !version.is_empty() =>
            {
                Ok(Self {
                    project_id: (*project_id).to_owned(),
                    secret_id: (*secret_id).to_owned(),
                    version: (*version).to_owned(),
                })
            }
            _ => bail!(
                "`{resource_name}` is not a Secret Manager resource name \
                 (projects/<project>/secrets/<object>/versions/<version>). A malformed reference \
                 would be silently unresolvable, so the ship stops here rather than reconciling it."
            ),
        }
    }
}

/// Every Secret Manager object the rendered manifest stream's
/// `SecretProviderClass` objects reference.
///
/// Read from the *built* stream rather than the manifest file, so an object is
/// only checked when the kustomization actually applies the class. Before the
/// catalog is wired in there is nothing to resolve and this is empty; after it
/// is, this is exactly the set `ops secrets apply` must provide.
/// Pure, so the parse is unit-tested against a fixture rather than a cluster.
pub(super) fn referenced_secret_manager_objects(
    manifests: &str,
) -> Result<BTreeSet<ProjectedObject>> {
    use serde::Deserialize;
    let mut referenced = BTreeSet::new();
    for document in serde_yaml::Deserializer::from_str(manifests) {
        let value = serde_json::Value::deserialize(document)
            .context("parse a document of the rendered manifest stream")?;
        if value.get("kind").and_then(serde_json::Value::as_str) != Some("SecretProviderClass") {
            continue;
        }
        let block = value
            .pointer("/spec/parameters/secrets")
            .and_then(serde_json::Value::as_str)
            .context("a SecretProviderClass carries spec.parameters.secrets")?;
        let entries: serde_json::Value = serde_yaml::from_str(block)
            .context("parse a SecretProviderClass spec.parameters.secrets block")?;
        for entry in entries
            .as_array()
            .context("spec.parameters.secrets is a list")?
        {
            let resource_name = entry
                .get("resourceName")
                .and_then(serde_json::Value::as_str)
                .context("a spec.parameters.secrets entry carries a resourceName")?;
            referenced.insert(ProjectedObject::parse(resource_name)?);
        }
    }
    Ok(referenced)
}

/// 4c — abort the ship if any object the rendered `SecretProviderClass`
/// references does not resolve to an `ENABLED` version in that deployment's
/// project.
///
/// Resolving the names here turns a catalog mismatch into a named abort before
/// the reconcile has touched anything. `ops secrets apply` is the only command
/// that writes the corresponding Kubernetes Secret; `ops ship` stays
/// metadata-only.
///
/// The check is names-only: it reads each version's state and never accesses a
/// payload, so it needs `secretmanager.versions.get` and not
/// `secretmanager.versions.access`, and no projected credential enters this
/// process.
/// Every repository in the rendered manifest stream that this deployment's own
/// registry publishes, deduplicated and stripped of its tag.
///
/// The filter is [`pin_cronjob_images`]'s rule — `<registry>/…` — so a
/// third-party image (Envoy, `ClamAV`) and the KIND base's registry-less
/// `navigator-web:dev` are both left alone, and only images CI is responsible
/// for publishing are asserted on.
fn manifest_images_under_registry(manifests: &str, prefix: &str) -> BTreeSet<String> {
    let mut images = BTreeSet::new();
    for line in manifests.lines() {
        let Some(value) = line.trim().strip_prefix("image:") else {
            continue;
        };
        let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
        let Some(rest) = value.strip_prefix(prefix) else {
            continue;
        };
        // `<repository>:<tag>` or `<repository>@sha256:<digest>`.
        let repository = image_repository(rest);
        if !repository.is_empty() {
            images.insert(repository.to_string());
        }
    }
    images
}

/// The abort-or-proceed decision itself, split from the registry reads that
/// feed it for the same reason [`check_projected_objects_resolve`] is: the
/// thing that stops a bad roll is unit-tested against a fixture rather than
/// only against a live registry.
///
/// Every unpublished image is named, not just the first. A partial publish is
/// a *count* mismatch between what the release was supposed to produce and
/// what it did, and an operator re-running the roll once per abort learns that
/// count the slow way.
#[derive(Debug, PartialEq, Eq)]
enum ManifestImageStatus {
    Published,
    Unpublished,
    LookupFailed(String),
}

fn check_manifest_images_published(
    states: &[(String, ManifestImageStatus)],
    tag: &str,
) -> Result<()> {
    let missing: Vec<&str> = states
        .iter()
        .filter_map(|(image, status)| match status {
            ManifestImageStatus::Unpublished => Some(image.as_str()),
            ManifestImageStatus::Published | ManifestImageStatus::LookupFailed(_) => None,
        })
        .collect();
    let lookup_failures: Vec<String> = states
        .iter()
        .filter_map(|(image, status)| match status {
            ManifestImageStatus::LookupFailed(cause) => Some(format!("{image} ({cause})")),
            ManifestImageStatus::Published | ManifestImageStatus::Unpublished => None,
        })
        .collect();
    if missing.is_empty() && lookup_failures.is_empty() {
        eprintln!(
            "==> images OK ({} rendered image(s), all published at {tag})",
            states.len()
        );
        return Ok(());
    }
    let mut details = Vec::new();
    if !missing.is_empty() {
        details.push(format!(
            "{} image(s) are not published: {}",
            missing.len(),
            missing.join(", ")
        ));
    }
    if !lookup_failures.is_empty() {
        details.push(format!(
            "registry lookup failed for {} image(s): {}",
            lookup_failures.len(),
            lookup_failures.join(", ")
        ));
    }
    bail!(
        "the rendered manifests pin {} image(s) to {tag}, but {}.\n\
         The reconcile writes every one of these, so rolling now could apply an unsafe image \
         reference. Re-run the failed publish leg for an absent image, resolve the registry \
         problem for a lookup failure, or roll a tag that can be confirmed completely. Nothing \
         was applied.",
        states.len(),
        details.join("; ")
    )
}

/// Assert that every image the rendered stream pins is published at `tag`.
///
/// Skipped under `--dry-run`, which reaches no registry — the same contract
/// the reconcile's apply follows.
fn ensure_manifest_images_published(
    cfg: &ShipConfig,
    dry_run: bool,
    manifests: &str,
    tag: &str,
) -> Result<()> {
    if dry_run {
        eprintln!("DRY-RUN: would confirm every rendered image is published at {tag}");
        return Ok(());
    }
    let prefix = format!("{}/", cfg.registry());
    let states: Vec<(String, ManifestImageStatus)> =
        manifest_images_under_registry(manifests, &prefix)
            .into_iter()
            .map(|image| {
                let status = match registry::tag_exists(&cfg.registry, &image, tag) {
                    Ok(true) => ManifestImageStatus::Published,
                    Ok(false) => ManifestImageStatus::Unpublished,
                    Err(error) => ManifestImageStatus::LookupFailed(error.to_string()),
                };
                (image, status)
            })
            .collect();
    check_manifest_images_published(&states, tag)
}

/// Verify each workflow-published image's keyless signature and replace its
/// version tag with the signed immutable digest in the rendered manifests.
fn pin_and_verify_manifest_images(
    cfg: &ShipConfig,
    target: &Path,
    manifests: &str,
    tag: &str,
    digest_file: &Path,
) -> Result<()> {
    let digests = read_digest_manifest(digest_file)?;
    let prefix = format!("{}/", cfg.registry());
    let repositories = manifest_images_under_registry(manifests, &prefix);
    for repository in repositories {
        let reference = format!("{}/{}", cfg.registry(), repository);
        let digest = digest_for_image(&digests, &repository, &reference)?;
        let tagged_digest = registry::tag_digest(&cfg.registry, &repository, tag)
            .with_context(|| format!("resolve {reference}:{tag} to its registry digest"))?;
        ensure_tag_digest(&reference, tag, &digest, &tagged_digest)?;
        let signed_ref = format!("{reference}@{digest}");
        let docker_config = COSIGN_DOCKER_CONFIG
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let output = cosign_verify_command(&signed_ref, docker_config.as_deref())
            .output()
            .with_context(|| format!("verify cosign signature for {signed_ref}"))?;
        ensure!(
            output.status.success(),
            "cosign signature verification failed for `{signed_ref}`: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        pin_verified_image(target, &reference, tag, &digest, true)?;
    }
    Ok(())
}

/// Read the release's image repository → `sha256:<digest>` map.
fn read_digest_manifest(digest_file: &Path) -> Result<std::collections::BTreeMap<String, String>> {
    let bytes = fs::read(digest_file)
        .with_context(|| format!("read image digest manifest {}", digest_file.display()))?;
    serde_json::from_slice(&bytes).context("parse image digest manifest JSON")
}

fn ensure_tag_digest(reference: &str, tag: &str, expected: &str, actual: &str) -> Result<()> {
    ensure!(
        expected == actual,
        "digest manifest pins `{reference}` to {expected}, but `{reference}:{tag}` resolves to {actual}"
    );
    Ok(())
}

fn pin_verified_image(
    target: &Path,
    reference: &str,
    tag: &str,
    digest: &str,
    signature_verified: bool,
) -> Result<()> {
    ensure!(
        signature_verified,
        "refusing to pin `{reference}` because its cosign signature was not verified"
    );
    replace_in_tree(
        target,
        &format!("{reference}:{tag}"),
        &format!("{reference}@{digest}"),
    )
}

/// Look an image up by its registry-free name, falling back to the fully
/// qualified ref older release manifests were keyed by. The registry is a
/// secret, so current manifests never carry it: a job output or release asset
/// holding it is dropped by GitHub.
fn digest_for_image(
    digests: &std::collections::BTreeMap<String, String>,
    name: &str,
    reference: &str,
) -> Result<String> {
    if digests.contains_key(name) {
        return digest_for_ref(digests, name);
    }
    digest_for_ref(digests, reference)
}

fn digest_for_ref(
    digests: &std::collections::BTreeMap<String, String>,
    reference: &str,
) -> Result<String> {
    let digest = digests
        .get(reference)
        .ok_or_else(|| anyhow!("image digest manifest is missing `{reference}`"))?;
    ensure!(
        digest.len() == 71
            && digest.starts_with("sha256:")
            && digest[7..].bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid SHA-256 digest for `{reference}`"
    );
    Ok(digest.clone())
}

fn replace_in_tree(root: &Path, from: &str, to: &str) -> Result<()> {
    for entry in fs::read_dir(root).with_context(|| format!("read {}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            replace_in_tree(&path, from, to)?;
        } else if path.is_file() {
            let Ok(contents) = fs::read_to_string(&path) else {
                continue;
            };
            if contents.contains(from) {
                fs::write(&path, contents.replace(from, to))
                    .with_context(|| format!("pin image in {}", path.display()))?;
            }
        }
    }
    Ok(())
}

fn ensure_projected_objects_resolve(manifests: &str) -> Result<()> {
    let referenced = referenced_secret_manager_objects(manifests)?;
    if referenced.is_empty() {
        eprintln!(
            "==> no SecretProviderClass in the rendered tree — no Secret Manager objects to resolve"
        );
        return Ok(());
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    let states = runtime.block_on(async {
        let token = super::gcp::auth::adc_token_provider().await?;
        let client = super::gcp::client::GcpClient::new(token);
        let mut states = Vec::new();
        for object in referenced {
            let state = super::gcp::secret_manager::version_state(
                &client,
                &object.project_id,
                &object.secret_id,
                &object.version,
            )
            .await?;
            states.push((object, state));
        }
        Ok::<_, anyhow::Error>(states)
    })?;

    check_projected_objects_resolve(&states)
}

/// The abort-or-proceed decision itself, split from the Secret Manager reads
/// that feed it so the thing that stops a bad ship is unit-tested against a
/// fixture rather than only against live GCP.
///
/// Every unresolved object is reported, not just the first: the failure this
/// guards is a *count* mismatch between what the manifest references and what
/// the project holds, and an operator fixing them one abort at a time learns
/// the count the slow way.
fn check_projected_objects_resolve(states: &[(ProjectedObject, Option<String>)]) -> Result<()> {
    let unresolved: Vec<String> = states
        .iter()
        .filter_map(|(object, state)| match state.as_deref() {
            Some("ENABLED") => None,
            Some(state) => Some(format!("{} ({state})", object.secret_id)),
            None => Some(format!("{} (no such object)", object.secret_id)),
        })
        .collect();
    if unresolved.is_empty() {
        eprintln!(
            "==> Secret Manager objects OK ({} referenced, {} resolved to an ENABLED version)",
            states.len(),
            states.len()
        );
        return Ok(());
    }
    let project_ids: BTreeSet<&str> = states
        .iter()
        .map(|(object, _)| object.project_id.as_str())
        .collect();
    bail!(
        "the rendered SecretProviderClass references {} Secret Manager object(s) but only {} \
         resolve to an ENABLED version in {}: {}.\n\
         `ops secrets apply` cannot write the deployment Secret from a catalog entry that lacks \
         an enabled version. Write the missing values with \
         `navigator ops secrets apply --deployment <name>`, or trim the objects this deployment \
         does not carry from examples/deploy/k8s/gke/secrets/secret-provider-class.yaml. \
         Nothing was applied.",
        states.len(),
        states.len() - unresolved.len(),
        project_ids.into_iter().collect::<Vec<_>>().join(", "),
        unresolved.join(", ")
    )
}

/// The preflight decision itself, split from the two shell-outs that feed
/// it (the Secret read and `kubectl kustomize`) so the whole abort-or-
/// proceed choice — the thing that stops a bad ship — is unit-tested
/// against a manifest fixture rather than only against a live cluster.
fn check_secret_invariants(
    cfg: &ShipConfig,
    manifests: &str,
    secret_keys: &BTreeSet<String>,
) -> Result<()> {
    let parsed = shared_web_requirements(cfg.capabilities);
    let deployment_envs = parse_web_binary_envs(manifests, &cfg.secret_name, secret_keys)?;
    let missing_by_deployment =
        missing_requirements_by_deployment(&parsed, secret_keys, &deployment_envs);
    if missing_by_deployment.is_empty() {
        eprintln!(
            "==> Secret invariants OK ({} web-binary deployment(s) checked)",
            deployment_envs.len()
        );
        return Ok(());
    }
    bail!(unsatisfied_secret_error(cfg, &missing_by_deployment))
}

/// The operator-facing text for an unsatisfied boot requirement. Pure —
/// every input is already resolved — so the exact string the operator
/// reads is asserted by unit tests instead of only by a failed prod ship.
fn unsatisfied_secret_error(
    cfg: &ShipConfig,
    missing_by_deployment: &[(String, Vec<SecretRequirement>)],
) -> String {
    let described: Vec<String> = missing_by_deployment
        .iter()
        .map(|(deployment, missing)| {
            let missing = missing
                .iter()
                .map(SecretRequirement::describe)
                .collect::<Vec<_>>()
                .join(", ");
            format!("{deployment}: {missing}")
        })
        .collect();
    format!(
        "the new binary has boot requirements satisfied by neither the `{secret}` Secret \
         nor the target Deployment's env: {described:?}\n\
         (`A or B + C` boots when either the single key A or both B and C are present.)\n\
         An unsatisfied requirement crash-loops the new pod at boot. Add the missing keys \
         (values never transit logs) before re-running ship — one patch covers every \
         deployment listed above, which share the Secret:\n  \
         kubectl --context {ctx} -n {ns} patch secret {secret} --type=merge \\\n    \
         -p '{patch}'\n\
         (Keys provided as deployment env belong in the rendered manifests the reconcile applies, \
         not the Secret.)",
        secret = cfg.secret_name,
        ctx = cfg.context,
        ns = cfg.namespace,
        patch = secret_patch_stringdata(&suggested_patch_keys(missing_by_deployment)),
    )
}

/// The keys the suggested `kubectl patch` carries: for every unsatisfied
/// requirement, the FIRST alternative's keys — the canonical way to
/// satisfy it (`A or B + C` suggests `A`; the operator swaps in `B + C`
/// if that is the shape they hold). Deduped in source order, because the
/// web-binary deployments share one Secret and so surface the same
/// requirement once each.
fn suggested_patch_keys(missing_by_deployment: &[(String, Vec<SecretRequirement>)]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for key in missing_by_deployment
        .iter()
        .flat_map(|(_, missing)| missing.iter())
        .filter_map(|req| req.any_of.first())
        .flatten()
    {
        if !keys.contains(key) {
            keys.push(key.clone());
        }
    }
    // A requirement with no alternatives can't name a key; keep the
    // command shape valid rather than emitting an empty patch.
    if keys.is_empty() {
        keys.push("KEY".to_string());
    }
    keys
}

/// The `--type=merge` patch body for `keys` — every missing key mapped to
/// a `<value>` placeholder, so one paste-and-fill sets them all.
fn secret_patch_stringdata(keys: &[String]) -> String {
    let pairs = keys
        .iter()
        .map(|key| format!("\"{key}\":\"<value>\""))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{\"stringData\":{{{pairs}}}}}")
}

/// Build the rendered tree into a manifest stream — the state `apply -k`
/// is about to write.
///
/// `kubectl kustomize` (not a hand-parse of the tree) so the overlay's
/// patches — notably `patches/web-env.yaml`, which `$patch: replace`s the
/// whole env list — resolve exactly as the apply will.
///
/// `pub(super)` because the devx render tests use the same built-stream reader.
pub(super) fn kustomize_build(target: &Path) -> Result<String> {
    let out = Command::new("kubectl")
        .arg("kustomize")
        .arg(target)
        .output()
        .with_context(|| format!("run kubectl kustomize {}", target.display()))?;
    if !out.status.success() {
        bail!(
            "kubectl kustomize {} failed: {}",
            target.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    String::from_utf8(out.stdout).context("kubectl kustomize output is not UTF-8")
}

/// Pull one document out of a rendered manifest stream by kind + name.
///
/// Sits beside [`kustomize_build`] rather than inside a test module because
/// both the GKE tests here and the KIND overlay tests in `mod.rs` assert
/// against a built stream, and one shared reader keeps them asserting on
/// the same thing.
#[cfg(test)]
pub(super) fn manifest_doc(manifests: &str, kind: &str, name: &str) -> serde_yaml::Value {
    use serde::Deserialize;
    for document in serde_yaml::Deserializer::from_str(manifests) {
        let Ok(value) = serde_yaml::Value::deserialize(document) else {
            continue;
        };
        let matches_kind = value.get("kind").and_then(serde_yaml::Value::as_str) == Some(kind);
        let matches_name = value
            .get("metadata")
            .and_then(|m| m.get("name"))
            .and_then(serde_yaml::Value::as_str)
            == Some(name);
        if matches_kind && matches_name {
            return value;
        }
    }
    panic!("no {kind}/{name} in the rendered stream");
}

/// The web-binary Deployments in a rendered manifest stream, each with its
/// usable env names. Pure — split from the `kubectl kustomize` shell-out so
/// the parse is unit-tested against a fixture instead of a live cluster.
///
/// The env is read from the rendered manifests rather than the live cluster
/// because the manifests are the state the reconcile is about to apply.
/// That is what lets the preflight run BEFORE any mutation: the live
/// Deployment's env is both stale (the apply is about to overwrite it) and
/// absent entirely on a first-ever ship, whereas the rendered tree is
/// authoritative in both cases.
fn parse_web_binary_envs(
    manifests: &str,
    secret_name: &str,
    secret_keys: &BTreeSet<String>,
) -> Result<Vec<(String, BTreeSet<String>)>> {
    use serde::Deserialize;
    let mut envs = Vec::new();
    for document in serde_yaml::Deserializer::from_str(manifests) {
        let value = serde_json::Value::deserialize(document)
            .context("parse a document of the rendered manifest stream")?;
        if value.get("kind").and_then(serde_json::Value::as_str) != Some("Deployment") {
            continue;
        }
        let Some(name) = value
            .pointer("/metadata/name")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        // `workflows-service` runs the worker binary, not `web`, so it is
        // not subject to the web boot invariants.
        if !web_binary_deployments().contains(&name) {
            continue;
        }
        envs.push((
            name.to_string(),
            populated_env_names(&value, secret_name, secret_keys),
        ));
    }
    if envs.is_empty() {
        bail!(
            "the rendered manifests define none of {:?} — refusing to ship a tree whose web tier \
             cannot be boot-checked",
            web_binary_deployments()
        );
    }
    Ok(envs)
}

/// The data keys carrying a non-empty value in the prod Secret.
fn secret_data_keys(cfg: &ShipConfig) -> Result<BTreeSet<String>> {
    let json = kubectl_json(cfg, "secret", &cfg.secret_name)?;
    Ok(populated_secret_keys(&json))
}

/// The Secret's `data` keys whose value is non-empty. An empty value is
/// as fatal as a missing key — `enforce_deployment_invariants` treats an empty
/// env var as unset — so counting the bare key name here would pass the
/// preflight and still crash-loop the pod at boot. (No base64 decode
/// needed: the empty value encodes to the empty string.)
fn populated_secret_keys(secret: &serde_json::Value) -> BTreeSet<String> {
    secret
        .get("data")
        .and_then(serde_json::Value::as_object)
        .map(|m| {
            m.iter()
                .filter(|(_, value)| value.as_str().is_some_and(|v| !v.is_empty()))
                .map(|(key, _)| key.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// The container env names that can satisfy a boot requirement (see
/// [`env_var_is_usable`] for the per-declaration rule). A name-only or
/// empty-literal declaration is excluded — the pod sees an empty string,
/// which `enforce_deployment_invariants` treats as unset.
fn populated_env_names(
    deployment: &serde_json::Value,
    secret_name: &str,
    secret_keys: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    if let Some(containers) = deployment
        .pointer("/spec/template/spec/containers")
        .and_then(serde_json::Value::as_array)
    {
        for container in containers {
            if let Some(env) = container.get("env").and_then(serde_json::Value::as_array) {
                for var in env {
                    if !env_var_is_usable(var, secret_name, secret_keys) {
                        continue;
                    }
                    if let Some(name) = var.get("name").and_then(serde_json::Value::as_str) {
                        names.insert(name.to_string());
                    }
                }
            }
        }
    }
    names
}

/// Whether a single container env declaration resolves to a value the
/// boot invariant would accept:
///
/// - a non-empty literal `value` — usable;
/// - a `secretKeyRef` into the very Secret `ship` inspects — resolvable,
///   so usable only when the referenced key carries a non-empty value
///   (an empty one is as fatal as a missing key at boot, and counting it
///   here would let the preflight pass while the pod crash-loops);
/// - any other `valueFrom` (a different Secret, a `ConfigMap`, the
///   downward API) — the preflight cannot inspect the resolved value, so stay
///   optimistic and count it;
/// - a name-only or empty-literal declaration — not usable.
fn env_var_is_usable(
    var: &serde_json::Value,
    secret_name: &str,
    secret_keys: &BTreeSet<String>,
) -> bool {
    if let Some(value_from) = var.get("valueFrom") {
        if let Some(secret_ref) = value_from.get("secretKeyRef") {
            if secret_ref.get("name").and_then(serde_json::Value::as_str) == Some(secret_name) {
                return secret_ref
                    .get("key")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|key| secret_keys.contains(key));
            }
        }
        return true;
    }
    var.get("value")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|v| !v.is_empty())
}

struct RestateDeployment {
    name: &'static str,
    url: String,
}

fn reregister_targets(cfg: &ShipConfig) -> [RestateDeployment; 1] {
    // One worker, every service — there is no separate worker endpoint to
    // register.
    [RestateDeployment {
        name: WORKFLOWS_DEPLOYMENT,
        url: cfg.workflows_url_resolved(),
    }]
}

fn apply_registration_result(deployment: &RestateDeployment, result: Result<()>) -> Result<()> {
    result.with_context(|| {
        format!(
            "Restate re-register of {} failed; its handler list would be stale — missing any \
             service added since the last registration — so invocations to it would silently \
             fail. Refusing to complete the ship.",
            deployment.name
        )
    })
}

/// 7d — register the workflows worker with Restate so handlers added since the
/// last registration are reachable. REQUIRED: a failed re-register leaves
/// Restate on a stale handler list that would silently drop invocations to a
/// new service (a canary invocation tests only its own handler, not the full
/// service list), so the ship fails rather than complete with the new
/// services unreachable.
fn reregister(cfg: &ShipConfig, dry_run: bool) -> Result<()> {
    let deployments = reregister_targets(cfg);
    if dry_run {
        for deployment in deployments {
            eprintln!(
                "DRY-RUN: would re-register {} with Restate (devx restate register {})",
                deployment.name, deployment.url
            );
        }
        return Ok(());
    }
    // The admin REST API path (RESTATE_ADMIN_URL + RESTATE_ADMIN_TOKEN) needs
    // no `restate` CLI, so only require the CLI when those env vars are absent.
    // These are operator-session credentials like ADC or a `restate cloud
    // login` token — never deployment coordinates, which all come from the
    // `deployments/` tree. Ship reads no secret value from the repository.
    let has_admin_api = !std::env::var("RESTATE_ADMIN_URL")
        .unwrap_or_default()
        .trim()
        .is_empty()
        && !std::env::var("RESTATE_ADMIN_TOKEN")
            .unwrap_or_default()
            .trim()
            .is_empty();
    if !has_admin_api && !tool_present("restate") {
        bail!(
            "no RESTATE_ADMIN_URL/TOKEN and `restate` CLI not on PATH; cannot register {WORKFLOWS_DEPLOYMENT}"
        );
    }
    // Pass every resolved URL explicitly so ship targets the same endpoints
    // it printed, independent of the workflows-only environment resolver.
    for deployment in &deployments {
        apply_registration_result(deployment, super::restate_register(Some(&deployment.url)))?;
    }
    Ok(())
}

/// 8 — curl the public landing and grep a fixed phrase. Best-effort:
/// reports, never fails the ship.
fn smoke_check(cfg: &ShipConfig, dry_run: bool) {
    let url = format!("https://{}/", cfg.public_host);
    if dry_run {
        eprintln!("DRY-RUN: would smoke-check {url}");
        return;
    }
    if !tool_present("curl") {
        eprintln!("WARN: `curl` not on PATH; skipping smoke check of {url}");
        return;
    }
    // Confirm the public landing is non-empty by grepping a stable phrase.
    // `--max-time` bounds the whole request so a stalled or unreachable
    // host can't hang the ship after the rollout already succeeded.
    let phrase = "home";
    match Command::new("curl")
        .args(["-fsS", "--max-time", "20", &url])
        .output()
    {
        Ok(out) if out.status.success() => {
            let body = String::from_utf8_lossy(&out.stdout).to_ascii_lowercase();
            if body.contains(phrase) {
                eprintln!("==> smoke check OK ({url})");
            } else {
                eprintln!("WARN: {url} returned 200 but the expected phrase was absent — inspect the page");
            }
        }
        Ok(out) => eprintln!("WARN: smoke check non-2xx for {url}: {}", out.status),
        Err(err) => eprintln!("WARN: smoke check could not reach {url}: {err}"),
    }
    eprintln!("==> workflows-service has no public /; confirm it is ready:");
    eprintln!(
        "    kubectl --context {} -n {} get pods -l app={WORKFLOWS_DEPLOYMENT}",
        cfg.context, cfg.namespace
    );
}

// ---------- small shared helpers ----------

/// A `kubectl` invocation pinned to the prod context and namespace.
pub(crate) fn kubectl(cfg: &ShipConfig) -> Command {
    let mut cmd = kubectl_ctx(cfg);
    cmd.arg("-n").arg(&cfg.namespace);
    cmd
}

/// A `kubectl` invocation pinned to the prod context only (for the
/// kustomize `diff`/`apply`, which carry their own namespaces).
pub(crate) fn kubectl_ctx(cfg: &ShipConfig) -> Command {
    let mut cmd = Command::new("kubectl");
    cmd.arg("--context").arg(&cfg.context);
    cmd
}

/// `kubectl get <kind> <name> -o json`, parsed.
pub(crate) fn kubectl_json(cfg: &ShipConfig, kind: &str, name: &str) -> Result<serde_json::Value> {
    let out = kubectl(cfg)
        .arg("get")
        .arg(kind)
        .arg(name)
        .arg("-o")
        .arg("json")
        .output()
        .with_context(|| format!("run kubectl get {kind} {name}"))?;
    if !out.status.success() {
        bail!(
            "kubectl get {kind} {name} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    serde_json::from_slice(&out.stdout)
        .with_context(|| format!("parse `kubectl get {kind} {name} -o json`"))
}

/// `kubectl get <kind> -o json` for a whole collection (no name), parsed.
/// The result is a `List` whose `items` array the caller walks.
fn kubectl_list_json(cfg: &ShipConfig, kind: &str) -> Result<serde_json::Value> {
    let out = kubectl(cfg)
        .arg("get")
        .arg(kind)
        .arg("-o")
        .arg("json")
        .output()
        .with_context(|| format!("run kubectl get {kind}"))?;
    if !out.status.success() {
        bail!(
            "kubectl get {kind} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    serde_json::from_slice(&out.stdout)
        .with_context(|| format!("parse `kubectl get {kind} -o json`"))
}

/// Wait on the selected Deployments' rollouts at the given timeout. On a
/// rollout that fails or times out, dump pod/event/log diagnostics for the
/// namespace before surfacing the error — so a wedged roll shows WHY
/// (crash-loop, `ImagePullBackOff`, Pending on no capacity) on the spot,
/// instead of a bare "timed out waiting for the condition". This is the
/// operator-side mirror of `deploy.yml`'s "dump diagnostics on failure".
fn wait_rollouts(
    cfg: &ShipConfig,
    dry_run: bool,
    timeout: &str,
    deployments: &[&str],
) -> Result<()> {
    for deployment in deployments {
        let result = exec(
            dry_run,
            kubectl(cfg)
                .arg("rollout")
                .arg("status")
                .arg(format!("deployment/{deployment}"))
                .arg(format!("--timeout={timeout}")),
        );
        if let Err(err) = result {
            if !dry_run {
                dump_rollout_diagnostics(cfg, deployment);
            }
            return Err(err).with_context(|| {
                format!("deployment/{deployment} did not become Ready within {timeout}")
            });
        }
    }
    Ok(())
}

/// Print pod status, recent events, and recent container logs for the
/// namespace when a rollout wedges — the on-the-spot answer to "why is the
/// deploy hanging?". Best-effort: each call ignores its own failure so a
/// diagnostics gap never masks the real rollout error. Output streams
/// straight to the operator's terminal (inherited stdio).
fn dump_rollout_diagnostics(cfg: &ShipConfig, deployment: &str) {
    eprintln!(
        "\n==> deployment/{deployment} stalled — dumping diagnostics (namespace {})",
        cfg.namespace
    );
    eprintln!("--- kubectl get pods ---");
    let _ = kubectl(cfg)
        .arg("get")
        .arg("pods")
        .arg("-o")
        .arg("wide")
        .status();
    // kubectl has no `--tail` for events, so capture the time-sorted
    // stream and tail it in Rust — no `sh -c`, so cfg.context/namespace
    // are passed as argv (never interpolated into a shell string) and
    // stay safe even when a fork's NAVIGATOR_GKE_CONTEXT carries
    // whitespace or shell metacharacters.
    eprintln!("--- recent events (last 25) ---");
    if let Ok(out) = kubectl(cfg)
        .arg("get")
        .arg("events")
        .arg("--sort-by=.lastTimestamp")
        .output()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = text.lines().collect();
        for line in &lines[lines.len().saturating_sub(25)..] {
            eprintln!("{line}");
        }
    }
    // `kubectl logs deployment/<name>` picks an arbitrary pod — on
    // 26.10.10-rc.1 it chose the old, healthy 26.10.9 pod and the new pod's
    // crash never reached the log. Name the pods that matter instead.
    let pods = match (
        kubectl_list_json(cfg, "replicasets"),
        kubectl_list_json(cfg, "pods"),
    ) {
        (Ok(replicasets), Ok(pods)) => diagnostic_pods(&replicasets, &pods, deployment),
        _ => Vec::new(),
    };
    if pods.is_empty() {
        eprintln!("--- recent logs: deployment/{deployment} (last 80 lines) ---");
        let _ = kubectl(cfg)
            .arg("logs")
            .arg(format!("deployment/{deployment}"))
            .arg("--all-containers")
            .arg("--tail=80")
            .status();
    }
    for pod in &pods {
        // A crash-looped pod's fatal output is in the PREVIOUS container,
        // not the one currently restarting; a pod that never restarted has
        // none, so fall back to its current logs.
        eprintln!("--- logs: pod/{pod} (previous container, last 80 lines) ---");
        let previous = kubectl(cfg)
            .arg("logs")
            .arg(pod)
            .arg("--all-containers")
            .arg("--previous")
            .arg("--tail=80")
            .output();
        match previous {
            Ok(out) if out.status.success() => {
                eprint!("{}", String::from_utf8_lossy(&out.stdout));
            }
            _ => {
                eprintln!("--- no previous container; current logs: pod/{pod} (last 80 lines) ---");
                let _ = kubectl(cfg)
                    .arg("logs")
                    .arg(pod)
                    .arg("--all-containers")
                    .arg("--tail=80")
                    .status();
            }
        }
    }
    eprintln!(
        "==> dig deeper: kubectl --context {} -n {} describe pods -l app={deployment}\n",
        cfg.context, cfg.namespace
    );
}

/// Cap on pods whose logs a stalled rollout prints: 80 lines each keeps
/// the release log readable and bounded however wide the Deployment scales.
const MAX_DIAGNOSTIC_PODS: usize = 4;

/// The pods of `deployment` worth reading logs from when its rollout
/// stalls: every pod of the newest `ReplicaSet` (the revision being
/// rolled out), plus any older pod that is not Ready or has restarted.
/// Newest-revision pods come first. Takes parsed `kubectl get replicasets
/// -o json` and `kubectl get pods -o json` lists.
fn diagnostic_pods(
    replicasets: &serde_json::Value,
    pods: &serde_json::Value,
    deployment: &str,
) -> Vec<String> {
    let str_at = |v: &serde_json::Value, ptr: &str| {
        v.pointer(ptr)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let owner = |v: &serde_json::Value, kind: &str| {
        v.pointer("/metadata/ownerReferences")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .find(|r| r.get("kind").and_then(serde_json::Value::as_str) == Some(kind))
            .and_then(|r| str_at(r, "/name"))
    };
    let items = |v: &serde_json::Value| {
        v.get("items")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default()
    };

    // (revision, name) of every ReplicaSet the Deployment owns.
    let revisions: Vec<(u64, String)> = items(replicasets)
        .iter()
        .filter(|rs| owner(rs, "Deployment").as_deref() == Some(deployment))
        .filter_map(|rs| {
            let name = str_at(rs, "/metadata/name")?;
            let revision = str_at(
                rs,
                "/metadata/annotations/deployment.kubernetes.io~1revision",
            )
            .and_then(|r| r.parse().ok())
            .unwrap_or(0);
            Some((revision, name))
        })
        .collect();
    let newest = revisions
        .iter()
        .max_by_key(|(rev, _)| *rev)
        .map(|(_, n)| n.as_str());

    let mut fresh = Vec::new();
    let mut troubled = Vec::new();
    for pod in items(pods) {
        let Some(name) = str_at(&pod, "/metadata/name") else {
            continue;
        };
        let Some(rs) = owner(&pod, "ReplicaSet") else {
            continue;
        };
        if !revisions.iter().any(|(_, n)| *n == rs) {
            continue;
        }
        if Some(rs.as_str()) == newest {
            fresh.push(name);
            continue;
        }
        let statuses = pod
            .pointer("/status/containerStatuses")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let restarted = statuses.iter().any(|c| {
            c.get("restartCount")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
                > 0
        });
        let ready = !statuses.is_empty()
            && statuses
                .iter()
                .all(|c| c.get("ready").and_then(serde_json::Value::as_bool) == Some(true));
        if restarted || !ready {
            troubled.push(name);
        }
    }
    fresh.extend(troubled);
    fresh.truncate(MAX_DIAGNOSTIC_PODS);
    fresh
}

/// Confirm the resolved `kubectl` context exists before any prod call.
/// A deterministic ship must not silently land on whatever context
/// happens to be current.
/// Whether the pinned context names the coordinates this ship resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ContextCheck {
    /// A `gke_<project>_<location>_<cluster>` context naming exactly the
    /// resolved project, location, and cluster.
    Matches,
    /// A `gke_…` context naming a *different* cluster. The deployment
    /// coordinates and the cluster the images would land on disagree.
    Mismatch {
        context_project: String,
        context_location: String,
        context_cluster: String,
    },
    /// Not a `get-credentials` context name, so its text carries no
    /// coordinates to cross-check.
    Unverifiable,
}

/// `gcloud container clusters get-credentials <cluster> --region <location>
/// --project <project>` names the context `gke_<project>_<location>_<cluster>`.
/// Project IDs, regions, and cluster names may all contain `-` but never `_`,
/// so the four-way split is unambiguous.
fn check_context(context: &str, project: &str, location: &str, cluster: &str) -> ContextCheck {
    let parts: Vec<&str> = context.split('_').collect();
    let [prefix, context_project, context_location, context_cluster] = parts.as_slice() else {
        return ContextCheck::Unverifiable;
    };
    if *prefix != "gke" {
        return ContextCheck::Unverifiable;
    }
    if *context_project == project && *context_location == location && *context_cluster == cluster {
        return ContextCheck::Matches;
    }
    ContextCheck::Mismatch {
        context_project: (*context_project).to_owned(),
        context_location: (*context_location).to_owned(),
        context_cluster: (*context_cluster).to_owned(),
    }
}

/// The resolved coordinates, printed before anything acts on them. Every
/// deployment in the `deployments/` tree runs in its own project; an
/// operator has to be able to see which one `--deployment` selected.
fn resolved_coordinates(cfg: &ShipConfig) -> String {
    format!(
        "==> resolved deployment\n      name:      {}\n      project:   {}\n      location:  {}\n      \
         cluster:   {}\n      namespace: {}\n      context:   {}\n      images:    {}",
        cfg.name, cfg.project_id, cfg.location, cfg.cluster, cfg.namespace, cfg.context,
        cfg.registry
    )
}

fn context_mismatch_error(cfg: &ShipConfig, check: &ContextCheck) -> String {
    let ContextCheck::Mismatch {
        context_project,
        context_location,
        context_cluster,
    } = check
    else {
        unreachable!("only a mismatch produces an error");
    };
    format!(
        "kubectl context '{}' names cluster {context_cluster} in {context_project}/{context_location}, \
         but this deployment resolved cluster {} in {}/{}. Shipping would roll {}'s release onto another \
         deployment's cluster. Fix NAVIGATOR_GKE_CONTEXT in deployments/{name}/config.toml, or get \
         credentials for the right cluster:\n    gcloud container clusters get-credentials {} --region {} --project {}",
        cfg.context,
        cfg.cluster,
        cfg.project_id,
        cfg.location,
        cfg.project_id,
        cfg.cluster,
        cfg.location,
        cfg.project_id,
        name = cfg.name,
    )
}

fn verify_context(cfg: &ShipConfig, dry_run: bool) -> Result<()> {
    eprintln!("{}", resolved_coordinates(cfg));
    // The coordinate cross-check is pure text, so it runs in dry-run too —
    // that is the mode an operator uses to confirm the target before a real
    // ship, and it is where a copied context should surface.
    let check = check_context(&cfg.context, &cfg.project_id, &cfg.location, &cfg.cluster);
    match &check {
        ContextCheck::Matches => {}
        ContextCheck::Mismatch { .. } => bail!("{}", context_mismatch_error(cfg, &check)),
        ContextCheck::Unverifiable => eprintln!(
            "==> NOTE: context '{}' is not a `gke_<project>_<location>_<cluster>` name, so its \
             coordinates could not be cross-checked against the resolved deployment",
            cfg.context
        ),
    }
    if dry_run {
        eprintln!("DRY-RUN: would pin kubectl context → {}", cfg.context);
        return Ok(());
    }
    let out = Command::new("kubectl")
        .args(["config", "get-contexts", "-o", "name"])
        .output()
        .context("kubectl config get-contexts")?;
    if !out.status.success() {
        bail!(
            "kubectl config get-contexts failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let contexts = String::from_utf8_lossy(&out.stdout);
    if !contexts.lines().any(|c| c == cfg.context) {
        bail!(
            "kubectl context '{}' not found. Get prod credentials \
             (`gcloud container clusters get-credentials …`) or set NAVIGATOR_GKE_CONTEXT \
             to the right context name.",
            cfg.context
        );
    }
    eprintln!("==> pinning kubectl context → {}", cfg.context);
    Ok(())
}

/// True when `tool` is on PATH (same probe as `require_tools`, but
/// boolean — for best-effort steps that downgrade to a warning).
pub(crate) fn tool_present(tool: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {tool}"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The `navigator-web` Google service account — the Workload Identity
/// principal the web pod runs as (KSA-bound in the Deployment; see
/// `web_pod_binds_the_navigator_web_service_account`).
fn web_gsa_email(project_id: &str, account_id: &str) -> String {
    format!("{account_id}@{project_id}.iam.gserviceaccount.com")
}

/// The role that carries `iam.serviceAccounts.signBlob` on a service account.
const SELF_SIGNING_ROLE: &str = "roles/iam.serviceAccountTokenCreator";

/// The `gcloud` argv that reads the web GSA's own IAM policy. This is the
/// steady-state call: it needs only `iam.serviceAccounts.getIamPolicy`, where
/// the binding write below also needs `setIamPolicy`. Factored out pure so it
/// is unit-testable without shelling out.
fn web_signing_iam_read_args(project_id: &str, account_id: &str) -> Vec<String> {
    vec![
        "iam".into(),
        "service-accounts".into(),
        "get-iam-policy".into(),
        web_gsa_email(project_id, account_id),
        format!("--project={project_id}"),
        "--format=json".into(),
    ]
}

/// The `gcloud` argv that lets the web GSA sign GCS URLs for itself.
/// Factored out pure so it is unit-testable without shelling out.
fn web_signing_iam_binding_args(project_id: &str, account_id: &str) -> Vec<String> {
    let gsa = web_gsa_email(project_id, account_id);
    vec![
        "iam".into(),
        "service-accounts".into(),
        "add-iam-policy-binding".into(),
        gsa.clone(),
        format!("--project={project_id}"),
        "--role".into(),
        SELF_SIGNING_ROLE.into(),
        format!("--member=serviceAccount:{gsa}"),
    ]
}

/// True when `policy` — a parsed `get-iam-policy` document — already grants
/// `gsa` the token-creator role on itself.
///
/// A binding carrying a `condition` does not count. The pod signs on every
/// document download, at an hour no expression here can predict, so a grant
/// that only sometimes applies is not the invariant step 1c asserts; treating
/// it as absent makes the roll write the unconditional binding instead of
/// passing a check the runtime may fail.
fn policy_grants_self_signing(policy: &serde_json::Value, gsa: &str) -> bool {
    let member = format!("serviceAccount:{gsa}");
    policy["bindings"].as_array().is_some_and(|bindings| {
        bindings.iter().any(|binding| {
            binding["role"].as_str() == Some(SELF_SIGNING_ROLE)
                && binding.get("condition").is_none()
                && binding["members"]
                    .as_array()
                    .is_some_and(|members| members.iter().any(|m| m.as_str() == Some(&member)))
        })
    })
}

/// What this roll is permitted to do about an absent self-binding.
///
/// The verify half is not selectable — the binding is asserted in every mode,
/// because a pod that cannot sign 500s every document download. What varies is
/// whether the roll may *establish* it, which is a different Google permission
/// (`setIamPolicy`) from the one the verify needs (`getIamPolicy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SigningIamAuthority {
    /// Write the binding when it is absent. The default, and what `ops gcp
    /// setup` has usually already made a no-op.
    #[default]
    Write,
    /// `--assert-signing-iam`: verify only. An absent binding fails the roll
    /// with the command that would establish it, rather than attempting a
    /// write the operator is not permitted.
    AssertOnly,
}

/// Read the authority out of the parsed flags. One place, so the flag's
/// meaning cannot drift between lanes.
fn signing_iam_authority(opts: &ShipOpts) -> SigningIamAuthority {
    if opts.assert_signing_iam {
        SigningIamAuthority::AssertOnly
    } else {
        SigningIamAuthority::Write
    }
}

/// The preflight error for a policy read that did not return an answer. The
/// roll stops rather than guessing: an unverified binding is exactly the state
/// that 500s every download. Names the permission, a role carrying it, and the
/// resource, because the operator's next move is to request that grant.
fn signing_iam_read_failed(gsa: &str, detail: &str) -> anyhow::Error {
    anyhow!(
        "cannot verify the GCS signing binding on {gsa}: reading its IAM policy failed. \
         `ops ship` needs `iam.serviceAccounts.getIamPolicy` on that service account \
         (carried by roles/iam.serviceAccountAdmin; roles/container.developer carries no \
         iam.serviceAccounts.* permission at all). gcloud said: {detail}"
    )
}

/// The preflight error for a binding that is genuinely missing and could not
/// be written. Distinct from the read failure above: here we know the pod
/// cannot sign, so there is nothing to roll onto until the grant lands.
/// `args` is the write argv this roll just attempted, quoted verbatim so the
/// operator can hand the exact command to someone who holds the permission.
fn signing_iam_write_failed(gsa: &str, args: &[String], detail: &str) -> anyhow::Error {
    anyhow!(
        "{gsa} is missing {SELF_SIGNING_ROLE} on itself and the binding could not be written. \
         Every document download would 500 on iam.serviceAccounts.signBlob, so the roll stops \
         here. Writing it needs `iam.serviceAccounts.setIamPolicy` on that service account \
         (carried by roles/iam.serviceAccountAdmin), or have someone holding it run: \
         `gcloud {}`. gcloud said: {detail}",
        args.join(" "),
    )
}

/// The preflight refusal for `--assert-signing-iam`. Distinct from
/// [`signing_iam_write_failed`], which reports a write this roll attempted and
/// lost: here no write was attempted at all, because the flag withdrew the
/// roll's authority to try one. The operator asked to be told rather than
/// granted for, so the message is the grant they now have to arrange —
/// the permission, a role carrying it, and the exact command, quoted verbatim
/// for whoever does hold it.
fn signing_iam_assert_failed(gsa: &str, args: &[String]) -> anyhow::Error {
    anyhow!(
        "{gsa} is missing {SELF_SIGNING_ROLE} on itself, and --assert-signing-iam withdrew \
         this roll's authority to write it. Every document download would 500 on \
         iam.serviceAccounts.signBlob, so the roll stops here rather than rolling onto a row \
         that cannot sign. Establishing the binding needs `iam.serviceAccounts.setIamPolicy` \
         on that service account (carried by roles/iam.serviceAccountAdmin) — have someone \
         holding it run: `gcloud {}`, then re-run this roll. Dropping the flag lets the roll \
         attempt that write itself.",
        args.join(" "),
    )
}

/// Ensure the web GSA can mint V4 GCS signed URLs. Under Workload
/// Identity the pod holds no private key, so the storage SDK signs each
/// document-download URL by calling IAM Credentials `signBlob` on its
/// own service account — which requires `roles/iam.serviceAccountTokenCreator`
/// on itself. Without it every `/…/documents/:doc_id/download` 500s with
/// `iam.serviceAccounts.signBlob denied`.
///
/// Verify-then-assert, not assert. `ops gcp setup` already writes this exact
/// binding, so on a provisioned row the steady state is "already bound" — and
/// gcloud's `add-iam-policy-binding` sends `setIamPolicy` unconditionally even
/// then, which would make every roll require a write permission it otherwise
/// has no use for. Reading first drops the common case to `getIamPolicy`.
///
/// The assertion is never skippable, only cheaper to confirm — but *who*
/// establishes the binding is a choice, because reading the policy and writing
/// it are different permissions. By default an absent binding is written here.
/// Under `--assert-signing-iam` it is reported and the roll stops, which is
/// the whole lane for an operator holding the release tag but no IAM write:
/// they can prove the invariant without being able to grant it.
///
/// `--dry-run` performs the read and declines only the write. The read is the
/// half a dry-run can answer honestly, so it does.
fn ensure_web_signing_iam(
    cfg: &ShipConfig,
    dry_run: bool,
    authority: SigningIamAuthority,
) -> Result<()> {
    ensure_web_signing_iam_with(cfg, dry_run, authority, || read_web_signing_policy(cfg))
}

/// Read the web GSA's IAM policy. Takes no `dry_run`, deliberately: this read
/// happens in every mode. A dry-run exists to answer "would the real roll
/// work", and skipping the one check it can perform for free is how a dry-run
/// reached `==> ship complete` for a roll that then died at step 1c. It is
/// also the permission probe — an operator missing `getIamPolicy` fails here,
/// under `--dry-run`, exactly as the live roll would.
fn read_web_signing_policy(cfg: &ShipConfig) -> Result<serde_json::Value> {
    let gsa = web_gsa_email(&cfg.project_id, &cfg.google_service_account_id);
    let out = Command::new("gcloud")
        .args(web_signing_iam_read_args(
            &cfg.project_id,
            &cfg.google_service_account_id,
        ))
        .output()
        .with_context(|| format!("run gcloud get-iam-policy {gsa}"))?;
    if !out.status.success() {
        return Err(signing_iam_read_failed(
            &gsa,
            String::from_utf8_lossy(&out.stderr).trim(),
        ));
    }
    serde_json::from_slice(&out.stdout).with_context(|| format!("parse the IAM policy of {gsa}"))
}

/// The step past its read, with the read injected so a unit test can drive
/// every verdict — including the one that matters here, that `--dry-run` still
/// calls `read_policy` and still fails when that read is denied.
fn ensure_web_signing_iam_with(
    cfg: &ShipConfig,
    dry_run: bool,
    authority: SigningIamAuthority,
    read_policy: impl FnOnce() -> Result<serde_json::Value>,
) -> Result<()> {
    let gsa = web_gsa_email(&cfg.project_id, &cfg.google_service_account_id);
    eprintln!("==> verifying {gsa} can sign GCS URLs ({SELF_SIGNING_ROLE} on itself)");

    let policy = read_policy()?;
    if policy_grants_self_signing(&policy, &gsa) {
        eprintln!("==> already bound; no IAM write");
        return Ok(());
    }

    let write_args = web_signing_iam_binding_args(&cfg.project_id, &cfg.google_service_account_id);

    // Checked ahead of the dry-run branch, so the flag refuses under both
    // modes. A dry-run answers "would the real roll work", and under this flag
    // the real roll refuses — returning Ok here would be the same false green
    // that moving the read into every mode was meant to end.
    if authority == SigningIamAuthority::AssertOnly {
        return Err(signing_iam_assert_failed(&gsa, &write_args));
    }

    // The write is the only half a dry-run declines to perform, and it says so
    // loudly: an absent binding means the live roll needs `setIamPolicy`, which
    // nothing short of attempting the write can confirm the operator holds.
    if dry_run {
        eprintln!("DRY-RUN $ gcloud {}", write_args.join(" "));
        eprintln!(
            "DRY-RUN: the binding is ABSENT, so the real roll performs that write — it needs \
             iam.serviceAccounts.setIamPolicy on {gsa}, which a dry-run cannot confirm."
        );
        return Ok(());
    }

    eprintln!("==> binding absent — granting {SELF_SIGNING_ROLE}");
    let out = Command::new("gcloud")
        .args(&write_args)
        .output()
        .with_context(|| format!("run gcloud add-iam-policy-binding {gsa}"))?;
    if !out.status.success() {
        return Err(signing_iam_write_failed(
            &gsa,
            &write_args,
            String::from_utf8_lossy(&out.stderr).trim(),
        ));
    }
    Ok(())
}

/// Run a command, or — under `--dry-run` — print it instead.
fn exec(dry_run: bool, cmd: &mut Command) -> Result<()> {
    if dry_run {
        eprintln!("DRY-RUN $ {}", render_cmd(cmd));
        Ok(())
    } else {
        run(cmd)
    }
}

/// Render a `Command` as a copy-pasteable shell line for `--dry-run`.
fn render_cmd(cmd: &Command) -> String {
    let mut out = cmd.get_program().to_string_lossy().into_owned();
    for arg in cmd.get_args() {
        out.push(' ');
        out.push_str(&arg.to_string_lossy());
    }
    out
}

#[cfg(test)]
mod tests {
    const TEST_REGISTRY: &str = "ghcr.io/example/navigator";
    use super::*;
    use base64::Engine as _;

    #[test]
    fn cloud_context_uses_only_the_selected_installations_coordinates() {
        let cfg = sample_config();
        let command = cloud_context_command(&cfg);
        assert_eq!(command.get_program(), "gcloud");
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert_eq!(
            args,
            [
                "container",
                "clusters",
                "get-credentials",
                &cfg.cluster,
                "--location",
                &cfg.location,
                "--project",
                &cfg.project_id
            ]
        );
    }

    /// A key cannot be both: the cloud record refuses a row that carries a
    /// name in its encrypted file and its plaintext coordinates.
    #[test]
    fn no_plaintext_coordinate_is_an_inventoried_secret() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        for inventory in [
            manifest.join("../deployments/sops-inventory.yaml"),
            manifest.join("tests/fixtures/deployment-tree/deployments/sops-inventory.yaml"),
        ] {
            let body = fs::read_to_string(&inventory).unwrap();
            let secrets: BTreeSet<&str> = body
                .lines()
                .filter_map(|line| line.strip_prefix("  ")?.split(':').next())
                .collect();
            for (_, key) in PLAINTEXT_SUBSTITUTIONS {
                assert!(
                    !secrets.contains(key),
                    "{key} is a plaintext coordinate and an inventoried secret in {}",
                    inventory.display()
                );
            }
        }
    }

    #[test]
    fn a_pinned_context_is_renamed_from_the_get_credentials_name() {
        let mut cfg = sample_config();
        cfg.context = derived_context(&cfg.project_id, &cfg.location, &cfg.cluster);
        assert!(context_rename_command(&cfg).is_none());
        cfg.context = "operator-alias".into();
        let command = context_rename_command(&cfg).unwrap();
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        let derived = derived_context(&cfg.project_id, &cfg.location, &cfg.cluster);
        assert_eq!(
            args,
            [
                "config",
                "rename-context",
                derived.as_str(),
                "operator-alias"
            ]
        );
    }

    #[test]
    fn registry_login_covers_only_google_registries() {
        assert_eq!(
            artifact_registry_host("us-docker.pkg.dev/example-project/navigator"),
            Some("us-docker.pkg.dev")
        );
        assert_eq!(
            artifact_registry_host("gcr.io/example-project"),
            Some("gcr.io")
        );
        assert_eq!(artifact_registry_host(TEST_REGISTRY), None);
        assert_eq!(
            artifact_registry_host("evil-docker.pkg.dev.example.com/x"),
            None
        );
    }

    #[test]
    fn only_cosign_reads_the_scratch_registry_login() {
        let docker_config = Path::new("/scratch/docker");
        let command = cosign_verify_command("registry.example/image@sha256:0", Some(docker_config));
        let envs: Vec<_> = command.get_envs().collect();
        assert_eq!(
            envs,
            [(
                std::ffi::OsStr::new("DOCKER_CONFIG"),
                Some(docker_config.as_os_str())
            )]
        );
        assert_eq!(
            cosign_verify_command("registry.example/image@sha256:0", None)
                .get_envs()
                .count(),
            0
        );
    }

    #[test]
    fn dropping_the_credentials_removes_the_scratch_login() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let docker_config = path.join("docker");
        write_registry_login(&docker_config, "us-docker.pkg.dev", "synthetic-token").unwrap();
        let credentials = ShipCredentials { dir };
        drop(credentials);
        assert!(
            !path.exists(),
            "the scratch login must not outlive the ship"
        );
    }

    #[test]
    fn registry_login_is_a_private_oauth2accesstoken_docker_config() {
        let dir = tempfile::tempdir().unwrap();
        let docker_config = dir.path().join("docker");
        write_registry_login(&docker_config, "us-docker.pkg.dev", "synthetic-token").unwrap();
        let path = docker_config.join("config.json");
        let body: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let auth = body["auths"]["us-docker.pkg.dev"]["auth"].as_str().unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(auth)
            .unwrap();
        assert_eq!(decoded, b"oauth2accesstoken:synthetic-token");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // A second login never overwrites one already on disk.
        assert!(write_registry_login(&docker_config, "us-docker.pkg.dev", "other").is_err());
    }

    #[test]
    fn cloud_ship_rejects_invalid_release_input_before_authentication() {
        let mut opts = ShipOpts {
            deployment: "example-deployment".into(),
            project_id: Some("example-project".into()),
            ..Default::default()
        };
        assert!(run_ship(&opts).unwrap_err().to_string().contains("--tag"));
        opts.tag = Some("invalid-tag".into());
        assert!(run_ship(&opts).is_err());
        opts.deployments_dir = Some(PathBuf::from("/unused"));
        assert!(run_ship(&opts)
            .unwrap_err()
            .to_string()
            .contains("conflicts"));
    }

    fn ordered_deployment() -> super::super::deployments::Deployment {
        let coordinates: BTreeMap<String, String> = FULL_ENV
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        let postgres_encrypted_keys = [
            "NAVIGATOR_POSTGRES_PASSWORD",
            "NAVIGATOR_POSTGRES_TLS_CERTIFICATE",
            "NAVIGATOR_POSTGRES_TLS_PRIVATE_KEY",
            "NAVIGATOR_POSTGRES_CA_CERT",
            "NAVIGATOR_POSTGRES_APP_PASSWORD",
            "NAVIGATOR_POSTGRES_MIGRATOR_PASSWORD",
            "NAVIGATOR_POSTGRES_BACKUP_PASSWORD",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let forgejo_encrypted_keys = [
            "NAVIGATOR_FORGEJO_DATABASE_PASSWORD",
            "NAVIGATOR_FORGEJO_SECRET_KEY",
            "NAVIGATOR_FORGEJO_INTERNAL_TOKEN",
            "NAVIGATOR_FORGEJO_OAUTH2_JWT_SECRET",
            "NAVIGATOR_FORGEJO_POSTGRES_CA_CERT",
            "NAVIGATOR_FORGEJO_OIDC_CLIENT_SECRET",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        super::super::deployments::Deployment {
            name: "example-prod".into(),
            kms_key:
                "projects/example/locations/example-region/keyRings/sops/cryptoKeys/deployment"
                    .into(),
            provisioned: true,
            coordinates,
            encrypted_keys: BTreeSet::new(),
            postgres_encrypted_keys,
            forgejo_encrypted_keys,
        }
    }

    #[test]
    fn ordered_release_has_explicit_postgres_then_migrations_then_forgejo_then_app_order() {
        assert_eq!(
            ordered_release_steps(),
            &[
                ReleaseStep::Postgres,
                ReleaseStep::PostgresRoles,
                ReleaseStep::NavigatorMigrations,
                ReleaseStep::ForgejoDatabase,
                ReleaseStep::Forgejo,
                ReleaseStep::Application,
            ]
        );

        let mut observed = Vec::new();
        run_ordered_release(|step| {
            observed.push(step);
            Ok(())
        })
        .expect("all release barriers pass");
        assert_eq!(observed, ordered_release_steps());
    }

    #[test]
    fn ordered_release_stops_at_the_first_failed_readiness_barrier() {
        let mut observed = Vec::new();
        let error = run_ordered_release(|step| {
            observed.push(step);
            if step == ReleaseStep::ForgejoDatabase {
                bail!("synthetic database job failure");
            }
            Ok(())
        })
        .expect_err("a failed Forgejo database Job aborts the release");
        assert!(error.to_string().contains("ForgejoDatabase"));
        assert_eq!(
            observed,
            [
                ReleaseStep::Postgres,
                ReleaseStep::PostgresRoles,
                ReleaseStep::NavigatorMigrations,
                ReleaseStep::ForgejoDatabase,
            ]
        );
    }

    #[test]
    fn ordered_release_never_requires_objects_the_release_itself_renders() {
        let overlay = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../examples/deploy/k8s/gke/forgejo/staging/kustomization.yaml"),
        )
        .unwrap();
        for (kind, name) in ORDERED_SERVICE_OBJECTS {
            assert!(
                !(*kind == "configmap" && overlay.contains(&format!("name: {name}\n"))),
                "`{name}` is generated by the Forgejo overlay, so requiring it before the first \
                 apply blocks every fresh deployment"
            );
        }
    }

    #[test]
    fn ordered_release_refuses_missing_service_credentials_and_forgejo_coordinates() {
        let mut deployment = ordered_deployment();
        deployment.postgres_encrypted_keys.clear();
        let error = require_ordered_service_prerequisites(&deployment)
            .expect_err("the complete PostgreSQL service file is required");
        assert!(error.to_string().contains("NAVIGATOR_POSTGRES_PASSWORD"));

        let mut deployment = ordered_deployment();
        deployment
            .coordinates
            .remove("NAVIGATOR_FORGEJO_OIDC_DISCOVERY_URL");
        let error = require_ordered_service_prerequisites(&deployment)
            .expect_err("the Forgejo OIDC issuer must be configured");
        assert!(error
            .to_string()
            .contains("NAVIGATOR_FORGEJO_OIDC_DISCOVERY_URL"));

        let mut deployment = ordered_deployment();
        deployment.coordinates.insert(
            "NAVIGATOR_FORGEJO_ADMISSION_DOMAINS".into(),
            "example.com, bad domain".into(),
        );
        let error = require_ordered_service_prerequisites(&deployment)
            .expect_err("malformed admission domains must not reach a rollout");
        assert!(error
            .to_string()
            .contains("NAVIGATOR_FORGEJO_ADMISSION_DOMAINS"));

        require_ordered_service_prerequisites(&ordered_deployment())
            .expect("complete synthetic service prerequisites pass");

        // Hosting DBOS in `web` needs its login before the first apply.
        let mut deployment = ordered_deployment();
        deployment
            .coordinates
            .insert(NAVIGATOR_DBOS_ENABLED.into(), "true".into());
        let error = require_ordered_service_prerequisites(&deployment)
            .expect_err("DBOS on needs the DBOS login's password");
        assert!(error
            .to_string()
            .contains("NAVIGATOR_POSTGRES_DBOS_PASSWORD"));
        deployment
            .postgres_encrypted_keys
            .insert("NAVIGATOR_POSTGRES_DBOS_PASSWORD".into());
        require_ordered_service_prerequisites(&deployment).expect("DBOS on with its login passes");
    }

    #[test]
    fn service_overlays_follow_the_deployment_ring_and_selected_namespace() {
        let substitutions = resolve_substitutions_for_deployment(
            "example-staging",
            "26.10.15-rc.1",
            env_getter(HUB_ENV),
        )
        .expect("synthetic release coordinates resolve");
        let rendered = render_manifests_with(&substitutions).expect("manifest tree renders");
        // Staging's release ring still uses the production runtime profile.
        let mut cfg = sample_config();
        cfg.environment = store::DeploymentEnvironment::Production;
        cfg.public_host = "staging.navigator.example.invalid".into();
        cfg.namespace = "example-staging-project".into();
        for service in [
            "postgres",
            "postgres/access",
            "postgres/migrate",
            "forgejo/database",
            "forgejo",
        ] {
            let path = render_service_overlay(&rendered, &cfg, service)
                .unwrap_or_else(|error| panic!("render {service} overlay: {error:#}"));
            assert!(
                path.ends_with("staging"),
                "{service} chose {}",
                path.display()
            );
        }

        cfg.namespace = "example-elsewhere".into();
        let error = render_service_overlay(&rendered, &cfg, "postgres")
            .expect_err("an unknown namespace must not fall back to another environment's overlay");
        assert!(error.to_string().contains("selected deployment namespace"));
    }

    #[test]
    fn gke_service_overlays_render_separately_with_retained_namespaces_and_database_boundaries() {
        let deployment = ordered_deployment();
        let substitutions =
            resolve_substitutions_for_deployment(&deployment.name, "26.10.15-rc.1", |key| {
                deployment.coordinates.get(key).cloned()
            })
            .expect("synthetic release coordinates resolve");
        let rendered = render_manifests_with(&substitutions).expect("manifest tree renders");
        let mut cfg = sample_config();
        cfg.environment = store::DeploymentEnvironment::Production;
        cfg.namespace = deployment.coordinates["NAVIGATOR_K8S_NAMESPACE"].clone();

        for service in [
            "postgres",
            "postgres/access",
            "postgres/migrate",
            "forgejo/database",
            "forgejo",
        ] {
            let path = render_service_overlay(&rendered, &cfg, service)
                .unwrap_or_else(|error| panic!("render {service} overlay: {error:#}"));
            assert!(path.join("kustomization.yaml").is_file());
            let source = fs::read_to_string(path.join("kustomization.yaml")).unwrap();
            assert!(source.contains(&format!("namespace: {}", cfg.namespace)));
        }

        if Command::new("kubectl")
            .args(["version", "--client"])
            .output()
            .is_ok()
        {
            let postgres = render_service_overlay(&rendered, &cfg, "postgres").unwrap();
            let postgres_render = kustomize_build(&postgres).expect("PostgreSQL overlay renders");
            assert!(postgres_render
                .contains("kind: StatefulSet\nmetadata:\n  name: navigator-postgres"));
            assert!(postgres_render
                .contains("persistentVolumeClaimRetentionPolicy:\n    whenDeleted: Retain"));
            assert!(postgres_render.contains("reclaimPolicy: Retain"));

            let forgejo = render_service_overlay(&rendered, &cfg, "forgejo").unwrap();
            let forgejo_render = kustomize_build(&forgejo).expect("Forgejo overlay renders");
            assert!(forgejo_render.contains("name: forgejo"));
            assert!(forgejo_render.contains("name: navigator-forgejo"));
            assert!(forgejo_render.contains("email_domain_allowlist: example.com"));
            assert!(forgejo_render.contains(
                "discovery_url: https://identity.example/.well-known/openid-configuration"
            ));

            let migrate = render_service_overlay(&rendered, &cfg, "postgres/migrate").unwrap();
            let mut migrate_cfg = cfg.clone();
            migrate_cfg.registry = "registry.example/navigator".into();
            pin_service_images(
                &rendered,
                &migrate,
                &migrate_cfg,
                "26.10.15-rc.1",
                None,
                true,
            )
            .expect("dry-run renders the migration image");
            let migrate_render = kustomize_build(&migrate).expect("migration Job renders");
            assert!(migrate_render
                .contains("image: registry.example/navigator/neon-server:26.10.15-rc.1"));
            assert!(migrate_render.contains("name: navigator-migrate"));
        }
    }

    fn rs(name: &str, deployment: &str, revision: &str) -> serde_json::Value {
        serde_json::json!({"metadata": {
            "name": name,
            "annotations": {"deployment.kubernetes.io/revision": revision},
            "ownerReferences": [{"kind": "Deployment", "name": deployment}],
        }})
    }

    fn pod(name: &str, rs: &str, ready: bool, restarts: u64) -> serde_json::Value {
        serde_json::json!({
            "metadata": {"name": name, "ownerReferences": [{"kind": "ReplicaSet", "name": rs}]},
            "status": {"containerStatuses": [
                {"name": "web", "ready": ready, "restartCount": restarts},
            ]},
        })
    }

    #[test]
    fn diagnostic_pods_pick_newest_replicaset_and_troubled_old_pods() {
        // The 26.10.10-rc.1 stall: two healthy 26.10.9 pods and one
        // crash-looping new pod; `kubectl logs deployment/…` chose an old one.
        let replicasets = serde_json::json!({"items": [
            rs("navigator-web-5485b97dbd", "navigator-web", "41"),
            rs("navigator-web-7f9c6d8b44", "navigator-web", "42"),
            rs("workflows-service-66d4", "workflows-service", "99"),
        ]});
        let pods = serde_json::json!({"items": [
            pod("navigator-web-5485b97dbd-sxtcp", "navigator-web-5485b97dbd", true, 0),
            pod("navigator-web-5485b97dbd-flaky", "navigator-web-5485b97dbd", true, 2),
            pod("navigator-web-5485b97dbd-q2k9w", "navigator-web-5485b97dbd", false, 0),
            pod("navigator-web-7f9c6d8b44-zz8rt", "navigator-web-7f9c6d8b44", false, 5),
            pod("workflows-service-66d4-abcde", "workflows-service-66d4", false, 3),
        ]});

        assert_eq!(
            diagnostic_pods(&replicasets, &pods, "navigator-web"),
            [
                "navigator-web-7f9c6d8b44-zz8rt",
                "navigator-web-5485b97dbd-flaky",
                "navigator-web-5485b97dbd-q2k9w",
            ]
        );
        assert_eq!(
            diagnostic_pods(&replicasets, &pods, "workflows-service"),
            ["workflows-service-66d4-abcde"]
        );
        assert!(diagnostic_pods(&replicasets, &pods, "clamav").is_empty());

        let wide = serde_json::json!({"items": (0..10)
            .map(|i| pod(&format!("navigator-web-7f9c6d8b44-{i}"), "navigator-web-7f9c6d8b44", false, 1))
            .collect::<Vec<_>>()});
        assert_eq!(
            diagnostic_pods(&replicasets, &wide, "navigator-web").len(),
            MAX_DIAGNOSTIC_PODS
        );
    }

    fn sample_config() -> ShipConfig {
        ShipConfig {
            name: "example-prod".into(),
            environment: store::DeploymentEnvironment::Production,
            capabilities: store::deployment::DeploymentCapabilities::default(),
            project_id: "my-org-prod".into(),
            location: "example-region".into(),
            cluster: "navigator".into(),
            registry: "ghcr.io/example/navigator".into(),
            namespace: "navigator".into(),
            web_image_name: "neon-server".into(),
            public_host: "www.example.com".into(),
            asset_base_url: "https://www.example.com/assets".into(),
            google_service_account_id: "navigator-web".into(),
            primary_domain: "example.com".into(),
            secret_name: "navigator-web-secrets".into(),
            workflows_url: None,
            context: "gke_my-org-prod_example-region_navigator".into(),
        }
    }

    #[test]
    fn a_context_naming_the_resolved_cluster_matches() {
        let cfg = sample_config();
        assert_eq!(
            check_context(&cfg.context, &cfg.project_id, &cfg.location, &cfg.cluster),
            ContextCheck::Matches
        );
    }

    #[test]
    fn a_context_from_another_deployment_refuses_the_ship() {
        // The failure this guard exists for. Deployment configs get seeded
        // by cloning each other, so a copied `NAVIGATOR_GKE_CONTEXT` is a
        // one-line edit away from rolling one deployment's release onto
        // another's cluster — across a real-matter boundary, silently,
        // because both are valid clusters and both contexts exist.
        let cfg = ShipConfig {
            project_id: "neon-law".into(),
            cluster: "example-staging-project".into(),
            context: "gke_another-deployment_example-region_example-production".into(),
            ..sample_config()
        };
        let check = check_context(&cfg.context, &cfg.project_id, &cfg.location, &cfg.cluster);
        assert_eq!(
            check,
            ContextCheck::Mismatch {
                context_project: "another-deployment".into(),
                context_location: "example-region".into(),
                context_cluster: "example-production".into(),
            }
        );

        let message = context_mismatch_error(&cfg, &check);
        // Names BOTH sides, so the operator can see which one is wrong…
        assert!(message
            .contains("names cluster example-production in another-deployment/example-region"));
        assert!(
            message.contains("resolved cluster example-staging-project in neon-law/example-region")
        );
        // …and carries the exact command that fixes it.
        assert!(message.contains(
            "gcloud container clusters get-credentials example-staging-project --region example-region \
             --project neon-law"
        ));
    }

    #[test]
    fn a_matching_cluster_name_in_the_wrong_project_still_refuses() {
        // Two deployments can share one GCP project and differ only by resource
        // prefix, so cluster name alone is not identity.
        let cfg = ShipConfig {
            project_id: "example-staging-project".into(),
            cluster: "example-a".into(),
            context: "gke_neon-law_example-region_example-a".into(),
            ..sample_config()
        };
        assert!(matches!(
            check_context(&cfg.context, &cfg.project_id, &cfg.location, &cfg.cluster),
            ContextCheck::Mismatch { .. }
        ));
    }

    #[test]
    fn a_context_that_is_not_a_get_credentials_name_cannot_be_cross_checked() {
        // A fork may rename its context; the text then carries no coordinates.
        // Unverifiable is not the same as matching — `verify_context` says so
        // out loud rather than passing quietly.
        for context in [
            "prod",
            "gke_only_three",
            "arn:aws:eks:us-west-2:1:cluster/x",
        ] {
            assert_eq!(
                check_context(context, "my-org-prod", "example-region", "navigator"),
                ContextCheck::Unverifiable,
                "`{context}` carries no GKE coordinates"
            );
        }
    }

    #[test]
    fn the_resolved_coordinates_are_printed_before_anything_acts_on_them() {
        let printed = resolved_coordinates(&sample_config());
        for expected in [
            "project:   my-org-prod",
            "location:  example-region",
            "cluster:   navigator",
            "namespace: navigator",
            "context:   gke_my-org-prod_example-region_navigator",
            "images:    ghcr.io/example/navigator",
        ] {
            assert!(printed.contains(expected), "missing `{expected}`");
        }
    }

    #[test]
    fn a_copied_context_aborts_the_dry_run_too() {
        // Dry-run is the mode an operator uses to confirm the target before a
        // real ship, so the cross-check must fire there — it is pure text and
        // needs no cluster.
        let cfg = ShipConfig {
            project_id: "neon-law".into(),
            cluster: "acme-stg".into(),
            context: "gke_example-staging-project_example-region_example-staging-project".into(),
            ..sample_config()
        };
        let err = verify_context(&cfg, true).expect_err("a copied context must abort the dry run");
        assert!(err.to_string().contains("Shipping would roll"));
    }

    /// A full, valid substitution environment for the render tests — one
    /// value per required var, keyed by the var name `resolve_substitutions`
    /// reads.
    const FULL_ENV: &[(&str, &str)] = &[
        ("NAVIGATOR_GCP_PROJECT_ID", "example-production-project"),
        ("NAVIGATOR_GITHUB_ORG", "neon-law"),
        ("NAVIGATOR_IMAGES_PROJECT_ID", "ghcr"),
        ("NAVIGATOR_GAR_REPO", "navigator"),
        ("NAVIGATOR_WEB_IMAGE", "neon-server"),
        ("NAVIGATOR_FORGEJO_OIDC_NAME", "oidc"),
        (
            "NAVIGATOR_FORGEJO_OIDC_DISCOVERY_URL",
            "https://identity.example/.well-known/openid-configuration",
        ),
        ("NAVIGATOR_FORGEJO_ADMISSION_DOMAINS", "example.com"),
        ("NAVIGATOR_IMAGE_REGISTRY", "ghcr.io/example/navigator"),
        (
            "NAVIGATOR_FORGEJO_ROOT_URL",
            "https://forge.example.invalid/",
        ),
        (
            "NAVIGATOR_FORGEJO_OIDC_CLIENT_ID",
            "synthetic-forgejo-client",
        ),
        ("NAVIGATOR_PUBLIC_HOST", "navigator.example.invalid"),
        ("NAVIGATOR_WORKFLOWS_HOST", "workflows.example.invalid"),
        ("RESTATE_IDENTITY_KEY", "publickeyv1_test-identity-key"),
        ("NAVIGATOR_DOCUMENTS_BUCKET", "example-production-documents"),
        (
            "NAVIGATOR_APPLICATIONS_BUCKET",
            "example-production-applications",
        ),
        ("NAVIGATOR_ASSETS_BUCKET", "example-production-assets"),
        (
            "NAVIGATOR_ASSET_BASE_URL",
            "https://navigator.example.invalid/assets",
        ),
        ("NAVIGATOR_EXPORTS_BUCKET", "example-production-exports"),
        // Prefix-named, not suffix-named: `devx gcp` classifies a bucket by
        // prefix, and `<deployment>-archives` would be read as an assets
        // bucket and created without this lane's lifecycle rules.
        (
            "NAVIGATOR_ARCHIVES_BUCKET",
            "example-law-archives-example-production",
        ),
        ("NAVIGATOR_GATEWAY_IP_NAME", "example-production-gateway-ip"),
        ("NAVIGATOR_GCP_SERVICE_ACCOUNT_ID", "example-production-web"),
        (
            "NAVIGATOR_WEB_SECRET_NAME",
            "example-production-web-secrets",
        ),
        ("GOOGLE_OAUTH_REQUIRED_HD", "example.invalid"),
        ("SLACK_GENERAL_CHANNEL_ID", "C012GENERAL"),
        ("SLACK_FINANCE_CHANNEL_ID", "C012FINANCE"),
        ("SLACK_MESSAGES_CHANNEL_ID", "C012MESSAGES"),
        ("SLACK_STAGING_MESSAGES_CHANNEL_ID", "C012STAGING"),
        ("NAVIGATOR_K8S_NAMESPACE", "example-production"),
        ("NAVIGATOR_GCP_LOCATION", "example-region"),
        (
            "NAVIGATOR_OAUTH_CLIENT_ID_BROWSER",
            "111-browser.apps.googleusercontent.com",
        ),
        ("NAVIGATOR_BOOTSTRAP_OWNER_EMAIL", "owner@example.com"),
    ];

    /// The two-project shape every real environment now runs: images in the
    /// hub, everything else in the environment's own project. `FULL_ENV`
    /// above deliberately omits the override so the single-project fallback
    /// stays covered by every test that uses it.
    const HUB_ENV: &[(&str, &str)] = &[
        ("NAVIGATOR_GCP_PROJECT_ID", "example-staging-project"),
        ("NAVIGATOR_GITHUB_ORG", "example-staging-org"),
        ("NAVIGATOR_IMAGES_PROJECT_ID", "ghcr"),
        ("NAVIGATOR_GAR_REPO", "navigator"),
        ("NAVIGATOR_IMAGE_REGISTRY", "ghcr.io/example/navigator"),
        ("NAVIGATOR_WEB_IMAGE", "neon-server"),
        ("NAVIGATOR_FORGEJO_OIDC_NAME", "rauthy"),
        (
            "NAVIGATOR_FORGEJO_OIDC_DISCOVERY_URL",
            "https://identity.example/.well-known/openid-configuration",
        ),
        ("NAVIGATOR_FORGEJO_ADMISSION_DOMAINS", "example.com"),
        (
            "NAVIGATOR_FORGEJO_ROOT_URL",
            "https://forge-navigator.example.invalid/",
        ),
        ("NAVIGATOR_PUBLIC_HOST", "staging.navigator.example.invalid"),
        (
            "NAVIGATOR_WORKFLOWS_HOST",
            "workflows-navigator.example.invalid",
        ),
        ("RESTATE_IDENTITY_KEY", "publickeyv1_test-identity-key"),
        (
            "NAVIGATOR_DOCUMENTS_BUCKET",
            "example-staging-project-documents",
        ),
        (
            "NAVIGATOR_APPLICATIONS_BUCKET",
            "example-staging-project-applications",
        ),
        ("NAVIGATOR_ASSETS_BUCKET", "example-staging-project-assets"),
        (
            "NAVIGATOR_ASSET_BASE_URL",
            "https://staging.navigator.example.invalid/assets",
        ),
        (
            "NAVIGATOR_EXPORTS_BUCKET",
            "example-staging-project-exports",
        ),
        (
            "NAVIGATOR_ARCHIVES_BUCKET",
            "example-law-archives-example-staging-project",
        ),
        (
            "NAVIGATOR_GATEWAY_IP_NAME",
            "example-staging-project-gateway-ip",
        ),
        (
            "NAVIGATOR_GCP_SERVICE_ACCOUNT_ID",
            "example-staging-project-web",
        ),
        (
            "NAVIGATOR_WEB_SECRET_NAME",
            "example-staging-project-web-secrets",
        ),
        ("GOOGLE_OAUTH_REQUIRED_HD", "example.invalid"),
        ("SLACK_GENERAL_CHANNEL_ID", "C012GENERAL"),
        ("SLACK_FINANCE_CHANNEL_ID", "C012FINANCE"),
        ("SLACK_MESSAGES_CHANNEL_ID", "C012MESSAGES"),
        ("SLACK_STAGING_MESSAGES_CHANNEL_ID", "C012STAGING"),
        ("NAVIGATOR_K8S_NAMESPACE", "example-staging-project"),
        ("NAVIGATOR_GCP_LOCATION", "example-region"),
        (
            "NAVIGATOR_OAUTH_CLIENT_ID_BROWSER",
            "111-browser.apps.googleusercontent.com",
        ),
        ("NAVIGATOR_BOOTSTRAP_OWNER_EMAIL", "owner@example.com"),
    ];

    /// A getter over an in-memory `(key, value)` slice — the test analogue
    /// of `std::env::var`, so the substitution resolver is exercised without
    /// mutating the process environment.
    fn env_getter(
        pairs: &'static [(&'static str, &'static str)],
    ) -> impl Fn(&str) -> Option<String> {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn bootstrap_owner_email_is_required_for_a_ship() {
        let err = resolve_substitutions_for_deployment("example-deployment", "26.9.6", |key| {
            (key != "NAVIGATOR_BOOTSTRAP_OWNER_EMAIL")
                .then(|| env_getter(FULL_ENV)(key))
                .flatten()
        })
        .expect_err("a deployment without a bootstrap Owner must not render");
        assert!(err.to_string().contains("NAVIGATOR_BOOTSTRAP_OWNER_EMAIL"));
        assert!(err
            .to_string()
            .contains("deployments/example-deployment/config.toml"));
    }

    /// The placeholder tokens that must be gone from every rendered file.
    ///
    /// Enumerating them catches a substitution that stopped firing, but it
    /// cannot catch a placeholder nobody added here —
    /// [`surviving_your_tokens`] covers that half.
    const PLACEHOLDER_TOKENS: &[&str] = &[
        "YOUR_PROJECT_ID",
        "YOUR_GITHUB_ORG",
        IMAGE_REGISTRY_TOKEN,
        "YOUR_GCP_REGION",
        "NAVIGATOR_WEB_IMAGE",
        "NAVIGATOR_IMAGE_REGISTRY",
        "NAVIGATOR_PUBLIC_HOST",
        "NAVIGATOR_WORKFLOWS_HOST",
        "YOUR_RESTATE_IDENTITY_KEY",
        "YOUR_DOCUMENTS_BUCKET",
        "YOUR_APPLICATIONS_BUCKET",
        "YOUR_ASSETS_BUCKET",
        "YOUR_ASSET_BASE_URL",
        "YOUR_EXPORTS_BUCKET",
        "NAVIGATOR_GATEWAY_IP_NAME",
        "NAVIGATOR_GCP_SERVICE_ACCOUNT_ID",
        "navigator-web-secrets",
        "YOUR_GOOGLE_OAUTH_REQUIRED_HD",
        "YOUR_SLACK_MESSAGES_CHANNEL_ID",
        "YOUR_SLACK_STAGING_MESSAGES_CHANNEL_ID",
        "YOUR_OAUTH_CLIENT_ID_BROWSER",
        "YOUR_BOOTSTRAP_OWNER_EMAIL",
        "YOUR_SUMMARY_ENVELOPE_RECIPIENTS",
        "YOUR_SMS_NUMBER",
        "YOUR_OAUTH_MICROSOFT_CLIENT_ID",
        "YOUR_OAUTH_MICROSOFT_ALLOWED_TENANTS",
        RELEASE_TAG_TOKEN,
        BRAND_MANAGED_CERTIFICATES_TOKEN,
        BRAND_CERT_KUSTOMIZE_TOKEN,
        MANAGED_CERTIFICATES_ANNOTATION_TOKEN,
        BRAND_INGRESS_RULES_TOKEN,
        POSTGRES_WEB_PATCH_TOKEN,
        DBOS_WEB_PATCH_TOKEN,
        REPLICAS_PATCH_TOKEN,
        DISRUPTION_BUDGETS_RESOURCE_TOKEN,
        FORGEJO_HOST_TOKEN,
    ];

    /// Every `YOUR_*` placeholder still present in `text`.
    ///
    /// Read out of the rendered text rather than compared against
    /// [`PLACEHOLDER_TOKENS`], because that list is hand-maintained and so can
    /// only report a token somebody remembered to add to it.
    /// `YOUR_RESTATE_CLOUD_INGRESS` sat in shipped trigger `CronJob`s and
    /// on no substitution table at all: the enumerated check passed while every
    /// `ops ship` rendered a literal `https://YOUR_RESTATE_CLOUD_INGRESS` into
    /// the cluster. This finds the next one without being told its name.
    fn surviving_your_tokens(text: &str) -> Vec<String> {
        text.match_indices("YOUR_")
            .map(|(at, _)| {
                text[at..]
                    .chars()
                    .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
                    .collect()
            })
            .collect()
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn render_substitutes_every_placeholder_to_zero_remaining() {
        // TDD step 1: given a full NAVIGATOR_* env, the rendered tree has
        // ZERO `YOUR_*` / `your-domain.example` placeholders. A leftover
        // token would ship a broken bucket name or cert domain.
        let subs = resolve_substitutions_for_deployment(
            "example-production",
            "26.7.15",
            env_getter(FULL_ENV),
        )
        .expect("full env resolves");
        let rendered = render_manifests_with(&subs).expect("render succeeds");

        let gke = rendered.path().join(GKE_KUSTOMIZE_SUBPATH);
        assert!(
            gke.join("kustomization.yaml").is_file(),
            "the GKE kustomize root is rendered"
        );
        assert!(
            rendered
                .path()
                .join(K8S_BASE_SUBPATH)
                .join("kustomization.yaml")
                .is_file(),
            "the shared base is rendered alongside so `../../../../k8s/base` resolves"
        );
        assert!(
            rendered
                .path()
                .join(EXPORTS_KUSTOMIZE_SUBPATH)
                .join("kustomization.yaml")
                .is_file(),
            "the scheduled-job tree is rendered alongside so `../exports` resolves"
        );

        let mut files_seen = 0;
        for entry in walkdir::WalkDir::new(rendered.path()) {
            let entry = entry.expect("walk rendered tree");
            if !entry.file_type().is_file() {
                continue;
            }
            let text = fs::read_to_string(entry.path()).expect("read rendered file");
            for token in PLACEHOLDER_TOKENS {
                assert!(
                    !text.contains(token),
                    "{} still contains placeholder `{token}` after render",
                    entry.path().display()
                );
            }
            let surviving = surviving_your_tokens(&text);
            assert!(
                surviving.is_empty(),
                "{} still contains placeholder(s) {surviving:?} after render. A `YOUR_*` token \
                 that no substitution resolves reaches the cluster verbatim: either give it an \
                 entry in `resolve_substitutions_for_deployment`, or source the value from the \
                 deployment Secret instead of writing it inline",
                entry.path().display()
            );
            files_seen += 1;
        }
        assert!(files_seen > 0, "the render wrote at least one file");

        // The real values landed where the placeholders were.
        let web_env = fs::read_to_string(gke.join("patches/web-env.yaml")).unwrap();
        let workflows_env = fs::read_to_string(gke.join("patches/workflows-env.yaml")).unwrap();
        assert!(workflows_env.contains("name: NAVIGATOR_GCP_PROJECT_ID"));
        assert!(
            workflows_env.contains("example-production-project"),
            "GCP project substituted"
        );
        assert!(workflows_env.contains("name: NAVIGATOR_GITHUB_ORG"));
        assert!(!workflows_env.contains("YOUR_GITHUB_ORG"));
        assert!(
            workflows_env.contains("value: neon-law"),
            "GitHub org substituted"
        );
        assert!(workflows_env.contains("name: NAVIGATOR_APPLICATIONS_BUCKET"));
        assert!(workflows_env.contains("example-production-applications"));
        assert!(
            web_env.contains("example-production-assets"),
            "deployment-specific assets bucket substituted"
        );
        assert!(
            web_env.contains("example-production-documents"),
            "deployment-specific documents bucket substituted"
        );
        assert!(
            web_env.contains("name: NAVIGATOR_DOCUMENTS_BUCKET"),
            "documents bucket environment-variable name is preserved"
        );
        assert!(
            web_env.contains("name: NAVIGATOR_ASSETS_BUCKET"),
            "assets bucket environment-variable name is preserved"
        );
        assert!(
            web_env.contains("name: NAVIGATOR_APPLICATIONS_BUCKET"),
            "applications bucket environment-variable name is preserved"
        );
        assert!(
            web_env.contains("example-production-applications"),
            "deployment-specific applications bucket substituted"
        );
        // Every inline-env web key must appear as a `name:` here. `$patch:
        // replace` drops the KIND base env list, so any INLINE_ENV_WEB_KEYS
        // entry absent from this patch is a boot requirement the rolled pod
        // never receives — and `ops ship`'s preflight then aborts the roll.
        for key in INLINE_ENV_WEB_KEYS {
            assert!(
                web_env.contains(&format!("name: {key}")),
                "inline-env web key `{key}` is declared in the rendered GKE web env"
            );
        }
        // The public asset origin must reach the pod, not merely pass
        // `require_asset_base_url`'s config.toml check. `$patch: replace`
        // drops the base env list wholesale, so an entry missing here is an
        // entry the rolled binary never sees — it then resolves every
        // content image against the empty `/public` fallback and each hero
        // 404s while the bucket itself is perfectly healthy.
        assert!(
            web_env.contains("name: NAVIGATOR_ASSET_BASE_URL"),
            "asset base URL environment-variable name is preserved"
        );
        assert!(
            web_env.contains("https://navigator.example.invalid/assets"),
            "deployment-specific asset base URL substituted"
        );
        assert!(
            web_env
                .contains("name: CANONICAL_HOST\n              value: navigator.example.invalid"),
            "web receives the canonical host derived from its public host"
        );
        assert!(
            web_env.contains("name: GOOGLE_OAUTH_REQUIRED_HD"),
            "OAuth hosted-domain environment-variable name is preserved"
        );
        // The production render's `NAVIGATOR_PUBLIC_HOST` is `navigator.example.invalid`,
        // so every OTHER registered `BrandKey` — launched or not, per
        // ENG-808 — contributes its own production host here, derived from
        // the compiled registry rather than a second hand-maintained
        // coordinate. Each additional family is its own `ManagedCertificate`;
        // bundling them onto `navigator-web` would reissue the firm's
        // certificate whenever one of those names changed.
        let cert_manifest =
            fs::read_to_string(gke.join("ingress/managed-certificate.yaml")).unwrap();
        assert!(
            cert_manifest.contains("- navigator.example.invalid"),
            "the default brand keeps the navigator-web certificate"
        );
        let brand_certs =
            fs::read_to_string(gke.join("ingress/brand-managed-certificates.yaml")).unwrap();
        let kustomization = fs::read_to_string(gke.join("kustomization.yaml")).unwrap();
        assert!(
            kustomization.contains("- ingress/brand-managed-certificates.yaml"),
            "the GKE overlay includes the additional brand certificates"
        );
        let ingress = fs::read_to_string(gke.join("ingress/ingress.yaml")).unwrap();
        for key in views::brand::BrandKey::ALL {
            if *key == views::brand::BrandKey::default() {
                continue;
            }
            let production_host = key.canonical_host();
            assert!(
                !cert_manifest.contains(production_host),
                "additional brands must not share the default-brand certificate: {production_host}"
            );
            assert!(
                brand_certs.contains(&format!("- {production_host}")),
                "{}'s production host is on a brand certificate",
                key.as_str(),
            );
            assert_eq!(
                brand_certs.contains(&format!("name: {}", brand_certificate_name(*key))),
                has_own_certificate(*key),
                "{}'s own certificate exists only until the Firm certificate replaces it",
                key.as_str(),
            );
            assert!(
                ingress.contains(&format!("- host: {production_host}")),
                "{}'s production host gets an Ingress rule: {ingress}",
                key.as_str(),
            );
            let staging_host = format!("staging.{}", key.apex());
            assert!(
                !cert_manifest.contains(&staging_host)
                    && !brand_certs.contains(&staging_host)
                    && !ingress.contains(&staging_host),
                "a production render must not carry {}'s staging sibling",
                key.as_str(),
            );
        }
        assert!(
            ingress.contains(&format!(
                "networking.gke.io/managed-certificates: {}",
                expected_managed_certificates_annotation()
            )),
            "the Ingress attaches every family's certificate: {ingress}"
        );
        assert!(
            ingress.contains(
                "name: navigator-web\n                port:\n                  number: 80"
            ),
            "the additional brand host routes to the same navigator-web Service"
        );
        // An optional coordinate's drift is the reverse of the required keys':
        // the env *name* must survive even when the value renders empty, or a
        // deployment that later sets it writes a `config.toml` line the pod
        // never receives.
        assert!(
            web_env.contains(&format!("name: {NAVIGATOR_SUMMARY_ENVELOPE_RECIPIENTS}")),
            "summary envelope allowlist environment-variable name is preserved"
        );
        assert!(
            web_env.contains(&format!("name: {NAVIGATOR_SMS_NUMBER}")),
            "the Twilio SMS number reaches the pod"
        );
        // The break-glass Owner must reach the pod for the same reason the
        // asset origin must: `$patch: replace` drops the base env list, so an
        // entry missing here is one the rolled binary never sees — and a
        // deployment with no bootstrap Owner 403s every first sign-in with no
        // way to seed the first Person.
        assert!(
            web_env.contains("name: NAVIGATOR_BOOTSTRAP_OWNER_EMAIL"),
            "bootstrap Owner environment-variable name is preserved"
        );
        assert!(
            web_env.contains("value: owner@example.com"),
            "bootstrap Owner identity reaches the pod"
        );
        assert!(
            web_env.contains("111-browser.apps.googleusercontent.com"),
            "browser OAuth client id substituted (no doubled suffix)"
        );
        assert!(
            web_env.contains("111-browser.apps.googleusercontent.com"),
            "browser OAuth client id substituted"
        );
        assert!(
            web_env.contains("https://navigator.example.invalid/auth/callback"),
            "public host substituted into the redirect URI"
        );
        assert!(
            web_env.contains("value: example.invalid"),
            "OAuth hosted-domain restriction substituted"
        );
        assert!(
            web_env.contains("namespace: example-production"),
            "deployment-specific namespace substituted"
        );
        let workflows = fs::read_to_string(gke.join("workflows-service/deployment.yaml")).unwrap();
        assert!(
            workflows.contains("value: example-production-exports"),
            "worker receives the deployment-specific exports bucket"
        );
        assert!(
            !workflows.contains("example-production-project-exports"),
            "worker must not derive a shared exports bucket from the GCP project"
        );
        assert!(
            workflows.contains("value: C012GENERAL"),
            "worker receives #general's channel ID for GeneralNag"
        );
        assert!(
            workflows.contains("NAVIGATOR_SIMULATED_MATTERS"),
            "worker receives the simulated-matters selector DriDigest and GeneralNag read"
        );
        let namespace_manifest = fs::read_to_string(
            rendered
                .path()
                .join(K8S_BASE_SUBPATH)
                .join("namespace.yaml"),
        )
        .unwrap();
        assert!(
            namespace_manifest.contains("name: example-production"),
            "the Namespace object itself uses the deployment namespace"
        );
        assert!(!namespace_manifest.contains("name: navigator"));
    }

    #[test]
    fn explicit_staging_reaches_both_social_egress_processes() {
        for (setting, expected) in [
            (None, "false"),
            (Some("true"), "true"),
            (Some("false"), "false"),
        ] {
            let subs =
                resolve_substitutions_for_deployment("synthetic-deployment", "26.9.16", |key| {
                    if key == store::config::NAVIGATOR_STAGING {
                        setting.map(str::to_owned)
                    } else {
                        env_getter(FULL_ENV)(key)
                    }
                })
                .expect("synthetic configuration resolves");
            let rendered = render_manifests_with(&subs).expect("manifests render");
            for path in ["patches/web-env.yaml", "workflows-service/deployment.yaml"] {
                let text =
                    fs::read_to_string(rendered.path().join(GKE_KUSTOMIZE_SUBPATH).join(path))
                        .unwrap();
                assert!(
                    text.contains(&format!(
                        "name: NAVIGATOR_STAGING\n              value: \"{expected}\""
                    )),
                    "{path}: {setting:?}"
                );
            }
        }
    }

    #[test]
    fn invalid_explicit_staging_fails_before_manifest_render() {
        for value in ["", "TRUE", "invalid", "true\"\ninjected"] {
            let result =
                resolve_substitutions_for_deployment("synthetic-deployment", "26.9.16", |key| {
                    if key == store::config::NAVIGATOR_STAGING {
                        Some(value.into())
                    } else {
                        env_getter(FULL_ENV)(key)
                    }
                });
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("NAVIGATOR_STAGING"));
        }
    }

    fn with_simulated_matters(value: Option<&str>) -> impl Fn(&str) -> Option<String> + '_ {
        move |key| {
            if key == NAVIGATOR_SIMULATED_MATTERS {
                value.map(str::to_owned)
            } else {
                env_getter(FULL_ENV)(key)
            }
        }
    }

    #[test]
    fn replicas_follow_the_simulated_matters_profile() {
        for (value, expected) in [
            (Some("true"), 1),
            (Some("false"), 2),
            (None, 2),
            (Some(""), 2),
        ] {
            assert_eq!(
                replicas_for_row("synthetic-deployment", &with_simulated_matters(value))
                    .expect("a valid profile resolves"),
                expected,
                "{value:?}"
            );
        }
    }

    #[test]
    fn invalid_simulated_matters_fails_before_manifest_render() {
        for value in ["TRUE", "yes", "1", "true\"\ninjected"] {
            let error = resolve_substitutions_for_deployment(
                "synthetic-deployment",
                "26.9.16",
                with_simulated_matters(Some(value)),
            )
            .unwrap_err()
            .to_string();
            assert!(
                error.contains(NAVIGATOR_SIMULATED_MATTERS),
                "{value:?}: {error}"
            );
            assert!(error.contains("deployments/synthetic-deployment/config.toml"));
        }
    }

    #[test]
    fn one_replica_row_leaves_both_placeholders_empty() {
        let subs = replica_substitutions(
            "synthetic-deployment",
            &with_simulated_matters(Some("true")),
        )
        .expect("simulated row resolves");
        assert_eq!(subs.len(), 2);
        assert!(subs.iter().all(|sub| sub.value.is_empty()));
    }

    fn replicas_of(manifests: &str, name: &str) -> Option<u64> {
        manifest_doc(manifests, "Deployment", name)["spec"]["replicas"].as_u64()
    }

    fn disruption_budgets(manifests: &str) -> Vec<serde_yaml::Value> {
        use serde::Deserialize as _;
        serde_yaml::Deserializer::from_str(manifests)
            .map(|doc| serde_yaml::Value::deserialize(doc).expect("rendered YAML"))
            .filter(|doc| doc["kind"].as_str() == Some("PodDisruptionBudget"))
            .collect()
    }

    #[test]
    fn simulated_matters_row_renders_one_pod_per_tier_and_no_budget() {
        let manifests = rendered_manifests(with_simulated_matters(Some("true")));
        for name in REPLICATED_DEPLOYMENTS {
            assert_eq!(replicas_of(&manifests, name), Some(1), "{name}");
        }
        assert!(disruption_budgets(&manifests).is_empty());
    }

    #[test]
    fn real_matters_row_renders_two_pods_per_tier_behind_budgets() {
        for value in [Some("false"), None] {
            let manifests = rendered_manifests(with_simulated_matters(value));
            for name in REPLICATED_DEPLOYMENTS {
                assert_eq!(
                    replicas_of(&manifests, name),
                    Some(2),
                    "{name} for {value:?}"
                );
            }
            let budgets = disruption_budgets(&manifests);
            assert_eq!(budgets.len(), 2, "{value:?}");
            for name in REPLICATED_DEPLOYMENTS {
                let budget = budgets
                    .iter()
                    .find(|budget| budget["metadata"]["name"].as_str() == Some(name))
                    .unwrap_or_else(|| panic!("{name} budget"));
                assert_eq!(budget["spec"]["minAvailable"].as_u64(), Some(1));
                assert_eq!(
                    budget["spec"]["selector"]["matchLabels"]["app"].as_str(),
                    Some(name)
                );
                assert_eq!(
                    budget["metadata"]["namespace"].as_str(),
                    Some("example-production"),
                    "the budget lands in the row's namespace"
                );
                let deployment = manifest_doc(&manifests, "Deployment", name);
                assert_eq!(
                    deployment["spec"]["template"]["metadata"]["labels"]["app"].as_str(),
                    Some(name),
                    "the budget selects the Deployment's own pods"
                );
            }
        }
    }

    #[test]
    fn web_rolls_with_a_surge_pod_on_per_pod_repository_scratch() {
        for value in [Some("true"), Some("false")] {
            let deployment = rendered_web_deployment(with_simulated_matters(value));
            let strategy = &deployment["spec"]["strategy"];
            assert_eq!(strategy["type"].as_str(), Some("RollingUpdate"));
            assert_eq!(strategy["rollingUpdate"]["maxSurge"].as_u64(), Some(1));
            assert_eq!(
                strategy["rollingUpdate"]["maxUnavailable"].as_u64(),
                Some(0)
            );
            let repos = deployment["spec"]["template"]["spec"]["volumes"]
                .as_sequence()
                .expect("web volumes")
                .iter()
                .find(|volume| volume["name"].as_str() == Some("repos"))
                .expect("repository volume");
            assert!(repos.get("emptyDir").is_some(), "{repos:?}");
            assert!(repos.get("persistentVolumeClaim").is_none(), "{repos:?}");
        }
        let manifests = rendered_manifests(with_simulated_matters(Some("false")));
        assert!(
            !manifests
                .contains("kind: PersistentVolumeClaim\nmetadata:\n  name: navigator-web-repos"),
            "no single-writer repository claim is rendered"
        );
    }

    #[test]
    fn web_dbos_executor_id_is_the_pod_name_at_two_replicas() {
        let get = |key: &str| match key {
            NAVIGATOR_DBOS_ENABLED => Some("true".to_owned()),
            NAVIGATOR_SIMULATED_MATTERS => Some("false".to_owned()),
            _ => env_getter(FULL_ENV)(key),
        };
        let manifests = rendered_manifests(get);
        assert_eq!(replicas_of(&manifests, WEB_DEPLOYMENT), Some(2));
        for name in REPLICATED_DEPLOYMENTS {
            let deployment = manifest_doc(&manifests, "Deployment", name);
            let executor_id = deployment["spec"]["template"]["spec"]["containers"]
                .as_sequence()
                .expect("containers")
                .iter()
                .flat_map(|container| container["env"].as_sequence().into_iter().flatten())
                .find(|entry| entry["name"].as_str() == Some("NAVIGATOR_DBOS_EXECUTOR_ID"))
                .unwrap_or_else(|| panic!("{name} carries a DBOS executor id"));
            assert_eq!(
                executor_id["valueFrom"]["fieldRef"]["fieldPath"].as_str(),
                Some("metadata.name"),
                "{name}: each pod needs its own executor id"
            );
        }
    }

    fn rendered_web_deployment<F>(get: F) -> serde_yaml::Value
    where
        F: Fn(&str) -> Option<String>,
    {
        let subs = resolve_substitutions_for_deployment("synthetic-deployment", "26.9.16", get)
            .expect("synthetic environment resolves");
        let rendered = render_manifests_with(&subs).expect("render succeeds");
        let manifests = kustomize_build(&rendered.path().join(GKE_KUSTOMIZE_SUBPATH))
            .expect("rendered GKE manifests build");
        manifest_doc(&manifests, "Deployment", WEB_DEPLOYMENT)
    }

    fn rendered_web_env_entries<F>(get: F) -> Vec<serde_yaml::Value>
    where
        F: Fn(&str) -> Option<String>,
    {
        let deployment = rendered_web_deployment(get);
        let web = deployment
            .get("spec")
            .and_then(|spec| spec.get("template"))
            .and_then(|template| template.get("spec"))
            .and_then(|spec| spec.get("containers"))
            .and_then(serde_yaml::Value::as_sequence)
            .and_then(|containers| {
                containers.iter().find(|container| {
                    container.get("name").and_then(serde_yaml::Value::as_str) == Some("web")
                })
            })
            .expect("navigator-web has its web container");
        web.get("env")
            .and_then(serde_yaml::Value::as_sequence)
            .expect("web container has an env list")
            .clone()
    }

    fn with_postgres(value: Option<&'static str>) -> impl Fn(&str) -> Option<String> {
        move |key| {
            if key == NAVIGATOR_POSTGRES_ENABLED {
                value.map(str::to_owned)
            } else {
                env_getter(FULL_ENV)(key)
            }
        }
    }

    #[test]
    fn web_without_postgres_opt_in_renders_no_postgres_wiring() {
        for value in [None, Some("false")] {
            let deployment = rendered_web_deployment(with_postgres(value));
            let text = serde_yaml::to_string(&deployment).expect("deployment serializes");
            assert!(
                !text.contains("NAVIGATOR_POSTGRES_") && !text.contains("postgres"),
                "{value:?} must leave web untouched by PostgreSQL wiring"
            );
        }
        let disabled = resolve_substitutions_for_deployment(
            "synthetic-deployment",
            "26.9.16",
            with_postgres(None),
        )
        .expect("synthetic environment resolves");
        let rendered = render_manifests_with(&disabled).expect("render succeeds");
        let kustomization = fs::read_to_string(
            rendered
                .path()
                .join(GKE_KUSTOMIZE_SUBPATH)
                .join("kustomization.yaml"),
        )
        .expect("rendered kustomization");
        assert!(!kustomization.contains("web-postgres.yaml"));
    }

    #[test]
    fn mirror_opt_in_requires_postgres_and_installs_recovery_with_the_web_flag() {
        for postgres in [None, Some("false")] {
            let get = |key: &str| match key {
                store::postgres_mirror::MIRROR_ENV => Some("1".into()),
                NAVIGATOR_POSTGRES_ENABLED => postgres.map(str::to_owned),
                _ => env_getter(FULL_ENV)(key),
            };
            assert!(
                resolve_substitutions_for_deployment("synthetic", "26.9.16", get)
                    .unwrap_err()
                    .to_string()
                    .contains("requires")
            );
        }
        let get = |key: &str| match key {
            store::postgres_mirror::MIRROR_ENV => Some("1".into()),
            NAVIGATOR_POSTGRES_ENABLED => Some("true".into()),
            _ => env_getter(FULL_ENV)(key),
        };
        let substitutions =
            resolve_substitutions_for_deployment("synthetic", "26.9.16", get).unwrap();
        let rendered = render_manifests_with(&substitutions).unwrap();
        let manifest = kustomize_build(&rendered.path().join(GKE_KUSTOMIZE_SUBPATH)).unwrap();
        assert!(manifest.contains("name: NAVIGATOR_POSTGRES_MIRROR"));
        assert!(manifest.contains("name: navigator-postgres-mirror-recovery"));
        assert!(manifest.contains("concurrencyPolicy: Forbid"));
        assert!(manifest.contains("PostgresMirrorRecoveryIncomplete"));
        assert!(!manifest.contains("YOUR_IMAGE_REGISTRY"));
        assert!(!manifest.contains("YY.M.D"));
        assert!(
            !manifest.contains("kind: Job\n"),
            "initial scan is a separate release gate"
        );
    }

    #[test]
    fn mirror_flag_refuses_typos_and_preserves_disabled_render() {
        assert!(mirror_substitutions(&|_| Some("true".into())).is_err());
        for flag in [None, Some("0")] {
            let get = |key: &str| {
                (key == store::postgres_mirror::MIRROR_ENV)
                    .then(|| flag.map(str::to_owned))
                    .flatten()
            };
            assert!(mirror_substitutions(&get)
                .unwrap()
                .iter()
                .all(|s| s.value.is_empty()));
        }
    }

    #[test]
    fn mirror_recurring_recovery_uses_the_same_projection_and_least_privilege_as_one_shot() {
        let one_shot: serde_json::Value = serde_yaml::from_str(include_str!(
            "../../../k8s/postgres/mirror/mirror-reconcile-job.yaml"
        ))
        .unwrap();
        let recurring: serde_json::Value = serde_yaml::from_str(include_str!(
            "../../../k8s/postgres/mirror/recovery/cronjob.yaml"
        ))
        .unwrap();
        let mut source = one_shot["spec"]["template"].clone();
        let target = &recurring["spec"]["jobTemplate"]["spec"]["template"];
        source["spec"]["containers"][0]["image"] = target["spec"]["containers"][0]["image"].clone();
        assert_eq!(&source, target);
        assert_eq!(recurring["spec"]["concurrencyPolicy"], "Forbid");
        assert_eq!(recurring["spec"]["jobTemplate"]["spec"]["backoffLimit"], 1);
    }

    #[test]
    fn mirror_service_digest_pin_reaches_the_shared_job_outside_its_overlay() {
        let substitutions =
            resolve_substitutions_for_deployment("synthetic", "26.9.16", env_getter(FULL_ENV))
                .unwrap();
        let rendered = render_manifests_with(&substitutions).unwrap();
        let service = rendered
            .path()
            .join(GKE_KUSTOMIZE_SUBPATH)
            .join("postgres/mirror/production");
        let cfg = sample_config();
        let reference = format!("{}/navigator-surreal-archive", cfg.registry());
        let digest = format!("sha256:{}", "a".repeat(64));
        service_images_with(
            &rendered,
            &service,
            &cfg,
            "26.9.16",
            Some(Path::new("synthetic-digests.json")),
            false,
            |root, manifest, _| {
                assert!(manifest.contains(&format!("{reference}:26.9.16")));
                pin_verified_image(root, &reference, "26.9.16", &digest, true)
            },
        )
        .unwrap();
        let manifest = kustomize_build(&service).unwrap();
        assert!(manifest.contains(&format!("{reference}@{digest}")));
        assert!(!manifest.contains(&format!("{reference}:26.9.16")));
    }

    #[test]
    fn web_postgres_opt_in_adds_verify_full_app_login_from_its_own_secret() {
        let deployment = rendered_web_deployment(with_postgres(Some("true")));
        let namespace = deployment["metadata"]["namespace"]
            .as_str()
            .expect("namespaced web")
            .to_owned();
        let template = &deployment["spec"]["template"];
        assert_eq!(
            template["metadata"]["labels"]["navigator.neonlaw.org/postgres-client"].as_str(),
            Some("true"),
            "the PostgreSQL NetworkPolicy admits only labelled clients"
        );
        let web = template["spec"]["containers"]
            .as_sequence()
            .expect("containers")
            .iter()
            .find(|container| container["name"].as_str() == Some("web"))
            .expect("web container");
        let env = web["env"].as_sequence().expect("env list");
        let value = |name: &str| {
            env.iter()
                .find(|entry| entry["name"].as_str() == Some(name))
                .unwrap_or_else(|| panic!("{name} must be rendered"))
                .clone()
        };
        let host = format!("navigator-postgres.{namespace}.svc.cluster.local");
        for (name, expected) in [
            ("NAVIGATOR_POSTGRES_HOST", host.as_str()),
            ("NAVIGATOR_POSTGRES_PORT", "5432"),
            ("NAVIGATOR_POSTGRES_DATABASE", "navigator"),
            ("NAVIGATOR_POSTGRES_SSLMODE", "verify-full"),
            ("NAVIGATOR_POSTGRES_USER", "navigator_app"),
            (
                "NAVIGATOR_POSTGRES_ROOT_CERT",
                "/etc/navigator/postgres/ca.crt",
            ),
        ] {
            assert_eq!(value(name)["value"].as_str(), Some(expected), "{name}");
        }
        let password = value("NAVIGATOR_POSTGRES_PASSWORD");
        assert_eq!(
            password["valueFrom"]["secretKeyRef"]["name"].as_str(),
            Some("navigator-postgres-app")
        );
        assert_eq!(
            password["valueFrom"]["secretKeyRef"]["key"].as_str(),
            Some("password")
        );
        assert!(
            env.iter()
                .any(|entry| entry["name"].as_str() == Some("PORT")),
            "the web-env.yaml list survives the PostgreSQL patch"
        );
        let mount = web["volumeMounts"]
            .as_sequence()
            .expect("volume mounts")
            .iter()
            .find(|mount| mount["name"].as_str() == Some("postgres-ca"))
            .expect("CA bundle mount");
        assert_eq!(mount["mountPath"].as_str(), Some("/etc/navigator/postgres"));
        assert_eq!(mount["readOnly"].as_bool(), Some(true));
        let volume = template["spec"]["volumes"]
            .as_sequence()
            .expect("volumes")
            .iter()
            .find(|volume| volume["name"].as_str() == Some("postgres-ca"))
            .expect("CA bundle volume");
        assert_eq!(
            volume["secret"]["secretName"].as_str(),
            Some("navigator-postgres-app")
        );
        let items = volume["secret"]["items"].as_sequence().expect("items");
        assert_eq!(
            items.len(),
            1,
            "only the CA bundle is mounted, never the password"
        );
        assert_eq!(items[0]["key"].as_str(), Some("ca.crt"));
    }

    #[test]
    fn web_postgres_opt_in_refuses_an_ambiguous_value() {
        let error = resolve_substitutions_for_deployment(
            "synthetic-deployment",
            "26.9.16",
            with_postgres(Some("yes")),
        )
        .unwrap_err();
        assert!(error.to_string().contains(NAVIGATOR_POSTGRES_ENABLED));
    }

    fn with_dbos(value: Option<&'static str>) -> impl Fn(&str) -> Option<String> {
        move |key| {
            if key == NAVIGATOR_DBOS_ENABLED {
                value.map(str::to_owned)
            } else {
                env_getter(FULL_ENV)(key)
            }
        }
    }

    #[test]
    fn web_without_dbos_opt_in_renders_no_dbos_wiring() {
        for value in [None, Some("false")] {
            let deployment = rendered_web_deployment(with_dbos(value));
            let text = serde_yaml::to_string(&deployment).expect("deployment serializes");
            assert!(
                !text.contains("NAVIGATOR_DBOS_") && !text.contains("dbos"),
                "{value:?} must leave web untouched by DBOS wiring"
            );
        }
    }

    #[test]
    fn web_dbos_opt_in_adds_the_executor_login_from_its_own_secret() {
        let deployment = rendered_web_deployment(with_dbos(Some("true")));
        let namespace = deployment["metadata"]["namespace"]
            .as_str()
            .expect("namespaced web")
            .to_owned();
        let template = &deployment["spec"]["template"];
        assert_eq!(
            template["metadata"]["labels"]["navigator.neonlaw.org/postgres-client"].as_str(),
            Some("true"),
            "the PostgreSQL NetworkPolicy admits only labelled clients"
        );
        let web = template["spec"]["containers"]
            .as_sequence()
            .expect("containers")
            .iter()
            .find(|container| container["name"].as_str() == Some("web"))
            .expect("web container");
        let env = web["env"].as_sequence().expect("env list");
        let value = |name: &str| {
            env.iter()
                .find(|entry| entry["name"].as_str() == Some(name))
                .unwrap_or_else(|| panic!("{name} must be rendered"))
                .clone()
        };
        let host = format!("navigator-postgres.{namespace}.svc.cluster.local");
        for (name, expected) in [
            ("NAVIGATOR_DBOS_ENABLED", "true"),
            ("NAVIGATOR_DBOS_HOST", host.as_str()),
            ("NAVIGATOR_DBOS_PORT", "5432"),
            ("NAVIGATOR_DBOS_DATABASE", "navigator"),
            ("NAVIGATOR_DBOS_SSLMODE", "verify-full"),
            ("NAVIGATOR_DBOS_USER", "navigator_dbos"),
            (
                "NAVIGATOR_DBOS_ROOT_CERT",
                "/etc/navigator/postgres-dbos/ca.crt",
            ),
        ] {
            assert_eq!(value(name)["value"].as_str(), Some(expected), "{name}");
        }
        let password = value("NAVIGATOR_DBOS_PASSWORD");
        assert_eq!(
            password["valueFrom"]["secretKeyRef"]["name"].as_str(),
            Some("navigator-postgres-dbos")
        );
        assert!(
            env.iter()
                .any(|entry| entry["name"].as_str() == Some("PORT")),
            "the web-env.yaml list survives the DBOS patch"
        );
        let volume = template["spec"]["volumes"]
            .as_sequence()
            .expect("volumes")
            .iter()
            .find(|volume| volume["name"].as_str() == Some("postgres-dbos-ca"))
            .expect("CA bundle volume");
        assert_eq!(
            volume["secret"]["secretName"].as_str(),
            Some("navigator-postgres-dbos")
        );
        let items = volume["secret"]["items"].as_sequence().expect("items");
        assert_eq!(items.len(), 1, "only the CA bundle is mounted");
        assert_eq!(items[0]["key"].as_str(), Some("ca.crt"));
    }

    #[test]
    fn web_dbos_opt_in_refuses_an_ambiguous_value() {
        let error = resolve_substitutions_for_deployment(
            "synthetic-deployment",
            "26.9.16",
            with_dbos(Some("yes")),
        )
        .unwrap_err();
        assert!(error.to_string().contains(NAVIGATOR_DBOS_ENABLED));
    }

    fn with_schedules(
        dbos: Option<&'static str>,
        general_nag: Option<&'static str>,
        dri_digest: Option<&'static str>,
    ) -> impl Fn(&str) -> Option<String> {
        move |key| match key {
            NAVIGATOR_DBOS_ENABLED => dbos.map(str::to_owned),
            "NAVIGATOR_DBOS_GENERAL_NAG_ENABLED" => general_nag.map(str::to_owned),
            "NAVIGATOR_DBOS_DRI_DIGEST_ENABLED" => dri_digest.map(str::to_owned),
            _ => env_getter(FULL_ENV)(key),
        }
    }

    /// The rendered GKE tree, exports included, as one YAML stream.
    fn rendered_manifests<F>(get: F) -> String
    where
        F: Fn(&str) -> Option<String>,
    {
        let subs = resolve_substitutions_for_deployment("synthetic-deployment", "26.9.16", get)
            .expect("synthetic environment resolves");
        let rendered = render_manifests_with(&subs).expect("render succeeds");
        kustomize_build(&rendered.path().join(GKE_KUSTOMIZE_SUBPATH))
            .expect("rendered GKE manifests build")
    }

    fn web_env_value(deployment: &serde_yaml::Value, name: &str) -> Option<String> {
        deployment["spec"]["template"]["spec"]["containers"]
            .as_sequence()?
            .iter()
            .find(|container| container["name"].as_str() == Some("web"))?["env"]
            .as_sequence()?
            .iter()
            .find(|entry| entry["name"].as_str() == Some(name))?["value"]
            .as_str()
            .map(str::to_owned)
    }

    fn renders_cronjob(manifests: &str, name: &str) -> bool {
        manifests.split("\n---").any(|doc| {
            doc.contains("kind: CronJob") && doc.contains(&format!("\n  name: {name}\n"))
        })
    }

    #[test]
    fn dbos_schedules_stay_on_restate_until_switched_on() {
        for value in [None, Some("false")] {
            let get = with_schedules(Some("true"), value, value);
            let manifests = rendered_manifests(&get);
            let web = manifest_doc(&manifests, "Deployment", WEB_DEPLOYMENT);
            for schedule in &DBOS_SCHEDULES {
                assert_eq!(web_env_value(&web, schedule.env).as_deref(), Some("false"));
                assert!(
                    renders_cronjob(&manifests, schedule.cronjob),
                    "{value:?} keeps the {} Restate CronJob",
                    schedule.cronjob
                );
            }
            assert_eq!(web_env_value(&web, "SLACK_GENERAL_CHANNEL_ID"), None);
        }
    }

    #[test]
    fn a_switched_on_schedule_moves_to_web_and_leaves_its_cronjob_out() {
        let manifests = rendered_manifests(with_schedules(Some("true"), Some("true"), None));
        let web = manifest_doc(&manifests, "Deployment", WEB_DEPLOYMENT);
        assert_eq!(
            web_env_value(&web, "NAVIGATOR_DBOS_GENERAL_NAG_ENABLED").as_deref(),
            Some("true")
        );
        assert_eq!(
            web_env_value(&web, "SLACK_GENERAL_CHANNEL_ID").as_deref(),
            Some("C012GENERAL"),
            "GeneralNag on DBOS posts to the general channel from web"
        );
        assert!(!renders_cronjob(&manifests, "general-nag"));
        assert!(renders_cronjob(&manifests, "dri-digest-trigger"));

        let manifests = rendered_manifests(with_schedules(Some("true"), None, Some("true")));
        assert!(renders_cronjob(&manifests, "general-nag"));
        assert!(!renders_cronjob(&manifests, "dri-digest-trigger"));
    }

    #[test]
    fn billing_digest_moves_to_web_and_retires_its_cronjob() {
        let switched = |key: &str| match key {
            NAVIGATOR_DBOS_ENABLED | "NAVIGATOR_DBOS_BILLING_DIGEST_ENABLED" => {
                Some("true".to_owned())
            }
            "BILLING_EXPORT_TABLE" => Some("synthetic-project.billing.export".to_owned()),
            "BIGQUERY_PROJECT" => Some("synthetic-project".to_owned()),
            _ => env_getter(FULL_ENV)(key),
        };
        let manifests = rendered_manifests(switched);
        let web = manifest_doc(&manifests, "Deployment", WEB_DEPLOYMENT);
        assert_eq!(
            web_env_value(&web, "NAVIGATOR_DBOS_BILLING_DIGEST_ENABLED").as_deref(),
            Some("true")
        );
        assert!(!renders_cronjob(&manifests, "billing-digest-trigger"));

        let mut deployment =
            super::super::deployments::Deployment::load(&fixture_tree(), "example-deployment")
                .expect("fixture row loads");
        deployment
            .coordinates
            .insert(NAVIGATOR_DBOS_ENABLED.into(), "true".into());
        deployment.coordinates.insert(
            "NAVIGATOR_DBOS_BILLING_DIGEST_ENABLED".into(),
            "true".into(),
        );
        deployment.encrypted_keys.remove(SLACK_WEBHOOK_URL);
        let error = require_dbos_schedule_prerequisites(&deployment).unwrap_err();
        assert!(error.to_string().contains(SLACK_WEBHOOK_URL), "{error}");

        let missing_executor = |key: &str| match key {
            "NAVIGATOR_DBOS_BILLING_DIGEST_ENABLED" => Some("true".to_owned()),
            _ => env_getter(FULL_ENV)(key),
        };
        let error = resolve_substitutions_for_deployment(
            "synthetic-deployment",
            "26.9.16",
            missing_executor,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains(NAVIGATOR_DBOS_ENABLED),
            "{error}"
        );

        let missing_export = |key: &str| match key {
            NAVIGATOR_DBOS_ENABLED | "NAVIGATOR_DBOS_BILLING_DIGEST_ENABLED" => {
                Some("true".to_owned())
            }
            "BIGQUERY_PROJECT" => Some("synthetic-project".to_owned()),
            _ => env_getter(FULL_ENV)(key),
        };
        let error =
            resolve_substitutions_for_deployment("synthetic-deployment", "26.9.16", missing_export)
                .unwrap_err();
        assert!(
            error.to_string().contains("BILLING_EXPORT_TABLE"),
            "{error}"
        );
    }

    #[test]
    fn notation_reset_moves_to_web_with_both_messages_channels() {
        let switched = |key: &str| match key {
            NAVIGATOR_DBOS_ENABLED | "NAVIGATOR_DBOS_NOTATION_RESET_ENABLED" => {
                Some("true".to_owned())
            }
            _ => env_getter(FULL_ENV)(key),
        };
        let manifests = rendered_manifests(switched);
        let web = manifest_doc(&manifests, "Deployment", WEB_DEPLOYMENT);
        assert_eq!(
            web_env_value(&web, "NAVIGATOR_DBOS_NOTATION_RESET_ENABLED").as_deref(),
            Some("true")
        );
        assert_eq!(
            web_env_value(&web, "SLACK_MESSAGES_CHANNEL_ID").as_deref(),
            Some("C012MESSAGES")
        );
        assert_eq!(
            web_env_value(&web, "SLACK_STAGING_MESSAGES_CHANNEL_ID").as_deref(),
            Some("C012STAGING")
        );
        assert!(!renders_cronjob(&manifests, "notation-reset-trigger"));
        assert!(renders_cronjob(&manifests, "general-nag"));

        let without_channel = |key: &str| match key {
            "SLACK_STAGING_MESSAGES_CHANNEL_ID" => None,
            _ => switched(key),
        };
        let error = resolve_substitutions_for_deployment(
            "synthetic-deployment",
            "26.9.16",
            without_channel,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("SLACK_STAGING_MESSAGES_CHANNEL_ID"),
            "{error}"
        );
    }

    #[test]
    fn a_schedule_switch_refuses_typos_and_a_missing_executor() {
        for (dbos, general_nag, dri_digest, named) in [
            (
                Some("true"),
                Some("yes"),
                None,
                "NAVIGATOR_DBOS_GENERAL_NAG_ENABLED",
            ),
            (
                Some("true"),
                None,
                Some("1"),
                "NAVIGATOR_DBOS_DRI_DIGEST_ENABLED",
            ),
            (
                None,
                Some("true"),
                None,
                "NAVIGATOR_DBOS_GENERAL_NAG_ENABLED",
            ),
            (
                Some("false"),
                None,
                Some("true"),
                "NAVIGATOR_DBOS_DRI_DIGEST_ENABLED",
            ),
        ] {
            let error = resolve_substitutions_for_deployment(
                "synthetic-deployment",
                "26.9.16",
                with_schedules(dbos, general_nag, dri_digest),
            )
            .unwrap_err();
            assert!(error.to_string().contains(named), "{error}");
        }
    }

    #[test]
    fn every_roll_deletes_the_retired_heartbeat_trigger() {
        for dbos in [None, Some("true")] {
            let manifests = rendered_manifests(with_schedules(dbos, None, None));
            for retired in RETIRED_CRONJOBS {
                assert!(
                    !renders_cronjob(&manifests, retired),
                    "{retired} is never rendered"
                );
            }
        }
        let mut deployment =
            super::super::deployments::Deployment::load(&fixture_tree(), "example-deployment")
                .expect("fixture row loads");
        assert_eq!(
            retired_cronjobs(&deployment).unwrap(),
            ["heartbeat-trigger"]
        );
        deployment
            .coordinates
            .insert(NAVIGATOR_DBOS_ENABLED.into(), "true".into());
        deployment
            .coordinates
            .insert("NAVIGATOR_DBOS_GENERAL_NAG_ENABLED".into(), "true".into());
        assert_eq!(
            retired_cronjobs(&deployment).unwrap(),
            ["general-nag", "heartbeat-trigger"]
        );
        deployment.coordinates.insert(
            "NAVIGATOR_DBOS_BILLING_DIGEST_ENABLED".into(),
            "true".into(),
        );
        deployment.encrypted_keys.insert(SLACK_WEBHOOK_URL.into());
        assert_eq!(
            retired_cronjobs(&deployment).unwrap(),
            ["general-nag", "billing-digest-trigger", "heartbeat-trigger"]
        );
    }

    /// Staging's Ingress asked for 17 certificates for 21 hours: the three
    /// brands moved onto `navigator-firm` kept their own `ManagedCertificate`
    /// because nothing deleted it, so `navigator-firm` never attached.
    #[test]
    fn every_roll_deletes_the_certificates_the_firm_certificate_replaced() {
        let manifests = rendered_manifests(with_schedules(None, None, None));
        let retired = retired_brand_certificates();
        assert_eq!(
            retired,
            [
                "navigator-web-abhaya",
                "navigator-web-death-and-divorce",
                "navigator-web-guieb-shook"
            ]
        );
        for name in &retired {
            assert!(
                !manifests.split("\n---").any(|doc| {
                    doc.contains("kind: ManagedCertificate")
                        && doc.contains(&format!("\n  name: {name}\n"))
                }),
                "{name} is never rendered"
            );
        }
    }

    #[test]
    fn dri_digest_on_dbos_requires_its_webhook_secret() {
        let mut deployment =
            super::super::deployments::Deployment::load(&fixture_tree(), "example-deployment")
                .expect("fixture row loads");
        deployment
            .coordinates
            .insert(NAVIGATOR_DBOS_ENABLED.into(), "true".into());
        deployment
            .coordinates
            .insert("NAVIGATOR_DBOS_DRI_DIGEST_ENABLED".into(), "true".into());
        deployment.encrypted_keys.remove(SLACK_WEBHOOK_URL);
        let error = require_dbos_schedule_prerequisites(&deployment).unwrap_err();
        assert!(error.to_string().contains(SLACK_WEBHOOK_URL), "{error}");
        deployment.encrypted_keys.insert(SLACK_WEBHOOK_URL.into());
        require_dbos_schedule_prerequisites(&deployment).unwrap();
    }

    #[test]
    fn public_example_rows_do_not_enable_dbos_implicitly() {
        let root = fixture_tree();
        for row in ["example-deployment", "example-automation-home"] {
            let path = root.join("deployments").join(row).join("config.toml");
            let config: toml::Table = fs::read_to_string(&path)
                .expect("read the deployment config")
                .parse()
                .expect("parse the deployment config");
            assert_eq!(
                config
                    .get("env")
                    .and_then(|env| env.get(NAVIGATOR_DBOS_ENABLED))
                    .and_then(toml::Value::as_str),
                None,
                "{row} must opt into DBOS explicitly; private deployment profiles are not inferred"
            );
        }
    }

    #[test]
    fn optional_client_id_entries_are_absent_when_unset_or_blank() {
        for (case, client_id, expected) in [
            ("missing", None, None),
            ("blank", Some("   "), None),
            (
                "configured",
                Some("com.example.navigator.synthetic"),
                Some("com.example.navigator.synthetic"),
            ),
        ] {
            let entries = rendered_web_env_entries(|key| {
                if key == "OAUTH_APPLE_CLIENT_ID" {
                    client_id.map(str::to_owned)
                } else {
                    env_getter(FULL_ENV)(key)
                }
            });
            let apple = entries.iter().find(|entry| {
                entry.get("name").and_then(serde_yaml::Value::as_str)
                    == Some("OAUTH_APPLE_CLIENT_ID")
            });

            match expected {
                Some(expected) => assert_eq!(
                    apple
                        .and_then(|entry| entry.get("value"))
                        .and_then(serde_yaml::Value::as_str),
                    Some(expected),
                    "a configured optional client ID must retain its value ({case})"
                ),
                None => assert!(
                    apple.is_none(),
                    "an {case} optional client ID must omit its whole env entry, not render it blank: {entries:?}"
                ),
            }
        }
    }

    #[test]
    fn deployment_capabilities_reach_the_web_pod_for_marked_and_unmarked_rows() {
        for (case, github, sms, expected_github, expected_sms) in [
            ("marked", Some("true"), Some("true"), "true", "true"),
            ("unmarked", Some("false"), Some("false"), "false", "false"),
            ("legacy absent", None, None, "false", "false"),
        ] {
            let entries = rendered_web_env_entries(|key| match key {
                store::deployment::GITHUB_AUTOMATION_HOME_FLAG => github.map(str::to_owned),
                store::deployment::SMS_TEST_RECIPIENTS_FLAG => sms.map(str::to_owned),
                _ => env_getter(FULL_ENV)(key),
            });
            for (key, expected) in [
                (
                    store::deployment::GITHUB_AUTOMATION_HOME_FLAG,
                    expected_github,
                ),
                (store::deployment::SMS_TEST_RECIPIENTS_FLAG, expected_sms),
            ] {
                let value = entries
                    .iter()
                    .find(|entry| entry["name"].as_str() == Some(key))
                    .and_then(|entry| entry["value"].as_str());
                assert_eq!(value, Some(expected), "{key} for {case} row");
            }
        }
    }

    #[test]
    fn invalid_deployment_capability_flags_fail_before_manifest_render() {
        for key in [
            store::deployment::GITHUB_AUTOMATION_HOME_FLAG,
            store::deployment::SMS_TEST_RECIPIENTS_FLAG,
        ] {
            let error = resolve_substitutions_for_deployment(
                "synthetic-deployment",
                "26.9.16",
                |candidate| {
                    if candidate == key {
                        Some("enabled".to_string())
                    } else {
                        env_getter(FULL_ENV)(candidate)
                    }
                },
            )
            .expect_err("capability flags accept only true or false");
            assert!(format!("{error:#}").contains(key), "{error:#}");
        }
    }

    #[test]
    fn microsoft_allow_any_tenant_reaches_the_pod_only_when_configured() {
        for (case, flag, expected) in [
            ("missing", None, None),
            ("blank", Some("   "), None),
            ("configured", Some("true"), Some("true")),
        ] {
            let entries = rendered_web_env_entries(|key| {
                if key == "OAUTH_MICROSOFT_ALLOW_ANY_TENANT" {
                    flag.map(str::to_owned)
                } else {
                    env_getter(FULL_ENV)(key)
                }
            });
            let entry = entries.iter().find(|entry| {
                entry.get("name").and_then(serde_yaml::Value::as_str)
                    == Some("OAUTH_MICROSOFT_ALLOW_ANY_TENANT")
            });

            match expected {
                Some(expected) => assert_eq!(
                    entry
                        .and_then(|entry| entry.get("value"))
                        .and_then(serde_yaml::Value::as_str),
                    Some(expected),
                    "a configured flag must reach the web container ({case})"
                ),
                None => assert!(
                    entry.is_none(),
                    "an {case} flag must omit the env entry so the portal default (off) applies: {entries:?}"
                ),
            }
        }
    }

    #[test]
    fn additional_brand_hosts_excludes_the_default_and_matches_the_environment() {
        assert_eq!(
            additional_brand_hosts("navigator.example.invalid"),
            vec![
                "www.deleteyourdata.com",
                "www.deleteyourdebt.com",
                "www.vestaestateplanning.com",
                "www.misericordialaw.com",
                "www.abhayaimmigration.com",
                "www.lawyershook.com",
                "www.summonsdefense.nyc",
                "www.daybridgedivorce.com",
                "www.deathanddivorcelaw.com",
                "www.cyberinjurylaw.com",
                "www.aegiscriminaldefense.com",
                "www.groundworkrealestatelaw.com",
                "www.guiebandshook.com",
            ],
            "a production public host pulls in every other registered brand's production host — \
             ENG-808: the full registry, not only the launched ones"
        );
        assert_eq!(
            additional_brand_hosts("staging.navigator.example.invalid"),
            vec![
                "staging.deleteyourdata.com",
                "staging.deleteyourdebt.com",
                "staging.vestaestateplanning.com",
                "staging.misericordialaw.com",
                "staging.abhayaimmigration.com",
                "staging.lawyershook.com",
                "staging.summonsdefense.nyc",
                "staging.daybridgedivorce.com",
                "staging.deathanddivorcelaw.com",
                "staging.cyberinjurylaw.com",
                "staging.aegiscriminaldefense.com",
                "staging.groundworkrealestatelaw.com",
                "staging.guiebandshook.com",
            ],
            "a staging public host pulls in only the other brands' staging hosts"
        );
    }

    /// The apex stays off the certificate and the Ingress, for every
    /// registered key — live or held out.
    ///
    /// DNS already 301s each naked domain to its own `www` from a URL record
    /// that does not resolve to the load balancer. Listing those apexes on a
    /// `ManagedCertificate` puts that certificate in `Provisioning` until
    /// every name validates, which they never will while the URL record
    /// stands. HTTPS at the apex is the DNS provider's redirect certificate,
    /// not GKE's.
    #[test]
    fn the_apex_stays_off_the_certificate_and_ingress() {
        let production = additional_brand_hosts("navigator.example.invalid");
        for key in views::brand::BrandKey::ALL {
            if *key == views::brand::BrandKey::default() {
                continue;
            }
            assert!(
                !production.contains(&key.apex()),
                "{} apex {} must not reach the load balancer: {production:?}",
                key.as_str(),
                key.apex(),
            );
            assert!(
                views::brand::registered_brand_key(key.apex()).is_none(),
                "{}'s apex must redirect rather than serve",
                key.as_str(),
            );
        }
        assert!(
            !additional_brand_hosts("staging.navigator.example.invalid")
                .iter()
                .any(|host| !host.starts_with("staging.")),
            "staging carries no apex"
        );
    }

    /// Every registered brand reaches its own certificate and Ingress rule,
    /// launched or not.
    ///
    /// Before ENG-808 a brand without an approved launch had no TLS at all
    /// until the same change that launched it. Certificate issuance is
    /// release infrastructure, independent of the launch decision: each
    /// family gets its own `ManagedCertificate` (ENG-768), so an unpointed
    /// or held-out name cannot hold another family's certificate in
    /// `Provisioning`, and it still must not appear in the footer or answer
    /// as that brand at the router — see
    /// `the_render_covers_the_full_registry_while_the_router_stays_live_gated`.
    #[test]
    fn every_registered_brand_reaches_its_own_certificate_regardless_of_launch_status() {
        for key in views::brand::BrandKey::ALL {
            if *key == views::brand::BrandKey::default() {
                continue;
            }
            for host in key.hosts() {
                let public_host = if host.starts_with("staging.") {
                    "staging.navigator.example.invalid"
                } else {
                    "navigator.example.invalid"
                };
                let rendered = additional_brand_hosts(public_host);
                assert!(
                    rendered.contains(host),
                    "{host} ({}, live={}) must reach its own certificate and Ingress rule",
                    key.as_str(),
                    key.is_live(),
                );
            }
        }
    }

    /// The deploy render covers the full registry; the request router stays
    /// gated on the approved launch set.
    ///
    /// Before ENG-808 these were the same list by construction: the render
    /// filtered `views::brand::live_brand_hosts` while `portal::canonical_host`
    /// filtered nothing, so the deployment published three brands while the
    /// process would wear any of eight. ENG-808 deliberately widens the
    /// render to [`views::brand::release_brand_hosts`] (every registered key)
    /// because a certificate is release infrastructure that should be ready
    /// before a launch is approved. The router's admission decision
    /// (`views::brand::admitted_brand_key`) is intentionally unchanged, so the
    /// two lists now diverge by exactly the held-out brands — this pins both
    /// halves of that divergence rather than re-deriving either filter.
    ///
    /// Both environments, because the split between `www.` and `staging.` is
    /// per-render and a gate that held for only one of them would leave the
    /// other open.
    #[test]
    fn the_render_covers_the_full_registry_while_the_router_stays_live_gated() {
        for public_host in ["navigator.example.invalid", "navigator.example.invalid"] {
            let rendered = additional_brand_hosts(public_host);
            let staging = public_host.starts_with("staging.");

            // Every host the router would admit on this environment, for a
            // brand other than the default, has a certificate and an
            // Ingress rule. A host the process answers for but the
            // deployment never routes would be a live-brand regression.
            for (key, host) in views::brand::live_brand_hosts() {
                if key == views::brand::BrandKey::default()
                    || host.starts_with("staging.") != staging
                {
                    continue;
                }
                assert!(
                    rendered.contains(&host),
                    "{public_host}: the router admits {host}, which the render omits: {rendered:?}",
                );
            }

            // And the release-inventory surplus over the launch set is
            // exactly the held-out brands: rendered but refused at the
            // router, never a registered host the router would admit.
            for host in &rendered {
                let key = views::brand::registered_brand_key(host)
                    .expect("every rendered host is registered");
                assert_eq!(
                    views::brand::admitted_brand_key(host).is_some(),
                    key.is_live(),
                    "{public_host}: {host} ({}) must be admitted iff it is live",
                    key.as_str(),
                );
            }
        }
    }

    /// The footer's family row reads the same gate.
    ///
    /// "Our Family" is a set of links; listing a brand whose host answers
    /// `404` advertises a practice a reader cannot reach, and for the NYC
    /// summons practice that is worse than a dead link.
    #[test]
    fn the_family_row_lists_only_admitted_brands() {
        let listed = webapp::firm_footer::compiled_family_brands(views::brand::BrandKey::default());
        for key in views::brand::BrandKey::ALL {
            let href = key.public_home_href();
            let present = listed.iter().any(|brand| brand.href == href);
            assert_eq!(
                present,
                key.is_live(),
                "{} is {}live and {}listed in the family row",
                key.as_str(),
                if key.is_live() { "" } else { "not " },
                if present { "" } else { "not " },
            );
        }
    }

    #[test]
    fn a_staging_render_carries_the_staging_sibling_not_the_production_one() {
        // Mirrors `render_substitutes_every_placeholder_to_zero_remaining`'s
        // production assertion, but for the `example-staging-project` shape (`HUB_ENV`),
        // whose `NAVIGATOR_PUBLIC_HOST` is `navigator.example.invalid` — each
        // registered brand's cert/Ingress entry must use its own `staging.`
        // host, and ENG-808 means every registered brand, not only the
        // launched ones.
        let subs = resolve_substitutions_for_deployment(
            "example-staging-project",
            "26.7.15",
            env_getter(HUB_ENV),
        )
        .expect("hub env resolves");
        let rendered = render_manifests_with(&subs).expect("render succeeds");
        let gke = rendered.path().join(GKE_KUSTOMIZE_SUBPATH);
        let cert_manifest =
            fs::read_to_string(gke.join("ingress/managed-certificate.yaml")).unwrap();
        for key in views::brand::BrandKey::ALL {
            if *key != views::brand::BrandKey::default() {
                assert!(
                    !cert_manifest.contains(&format!("staging.{}", key.apex())),
                    "{}'s staging host must not share the default-brand certificate",
                    key.as_str(),
                );
            }
        }
        let brand_certs =
            fs::read_to_string(gke.join("ingress/brand-managed-certificates.yaml")).unwrap();
        let ingress = fs::read_to_string(gke.join("ingress/ingress.yaml")).unwrap();
        for key in views::brand::BrandKey::ALL {
            if *key == views::brand::BrandKey::default() {
                continue;
            }
            let staging_host = format!("staging.{}", key.apex());
            assert!(
                brand_certs.contains(&format!("- {staging_host}")),
                "{}'s staging host gets its own certificate",
                key.as_str(),
            );
            assert!(
                ingress.contains(&format!("- host: {staging_host}")),
                "{}'s staging host gets an Ingress rule: {ingress}",
                key.as_str(),
            );
        }
        assert!(
            !brand_certs.contains("www.deleteyourdata.com"),
            "a staging render must not carry the production sibling"
        );
        assert!(
            ingress.contains(&format!(
                "networking.gke.io/managed-certificates: {}",
                expected_managed_certificates_annotation()
            )),
            "staging attaches every family's certificate: {ingress}"
        );
    }

    /// The full `networking.gke.io/managed-certificates` annotation ENG-808
    /// expects: the default brand, every other registered key that still has
    /// its own certificate in registry order, the workflows and Forgejo
    /// services, then the Firm certificate — derived from the same
    /// [`views::brand::BrandKey::ALL`] the render reads, so this drifts with
    /// the registry rather than needing a hand-maintained string.
    fn expected_managed_certificates_annotation() -> String {
        let mut names = vec!["navigator-web".to_string()];
        names.extend(
            views::brand::BrandKey::ALL
                .iter()
                .copied()
                .filter(|key| *key != views::brand::BrandKey::default())
                .filter(|key| has_own_certificate(*key))
                .map(brand_certificate_name),
        );
        names.push("navigator-workflows".to_string());
        names.push("navigator-forgejo".to_string());
        names.push(FIRM_CERTIFICATE_NAME.to_string());
        names.join(",")
    }

    /// A target HTTPS proxy references at most 15 Compute Engine SSL
    /// certificates, and GKE Ingress attaches one per `ManagedCertificate`
    /// it names. Past that the extra certificate is never attached, so
    /// Google cannot validate it and it sits at `FAILED_NOT_VISIBLE`.
    #[test]
    fn the_ingress_attaches_no_more_certificates_than_a_target_proxy_accepts() {
        for public_host in [
            "navigator.example.invalid",
            "staging.navigator.example.invalid",
        ] {
            let bindings = additional_brand_bindings(public_host);
            let annotation = managed_certificates_annotation(&bindings);
            let names = annotation
                .trim()
                .trim_start_matches("networking.gke.io/managed-certificates: ")
                .split(',')
                .count();
            assert!(
                names <= TARGET_PROXY_CERTIFICATE_LIMIT,
                "{public_host} attaches {names} certificates"
            );
        }
    }

    /// The Firm certificate lists every brand host the environment serves,
    /// the default brand's included, and none of the other environment's.
    #[test]
    fn the_firm_certificate_carries_every_brand_host_for_its_environment() {
        let production = firm_certificate_hosts("www.neonlaw.com");
        let staging = firm_certificate_hosts("staging.neonlaw.com");
        for key in views::brand::BrandKey::ALL {
            let www = key.canonical_host();
            let staging_host = format!("staging.{}", key.apex());
            assert!(
                production.iter().any(|host| host == www),
                "{www} is on the production Firm certificate"
            );
            assert!(
                staging.contains(&staging_host),
                "{staging_host} is on the staging Firm certificate"
            );
            assert!(!production.contains(&staging_host));
            assert!(!staging.iter().any(|host| host == www));
        }
        assert!(
            production.len() <= 100,
            "a managed certificate holds at most 100 names"
        );
    }

    /// The Ingress host rule for `host`, as `(path, service, port)` triples.
    fn ingress_routes(ingress: &serde_yaml::Value, host: &str) -> Vec<(String, String, u64)> {
        ingress
            .get("spec")
            .and_then(|spec| spec.get("rules"))
            .and_then(serde_yaml::Value::as_sequence)
            .expect("Ingress has host rules")
            .iter()
            .filter(|rule| rule.get("host").and_then(serde_yaml::Value::as_str) == Some(host))
            .flat_map(|rule| {
                rule["http"]["paths"]
                    .as_sequence()
                    .expect("host rule has paths")
                    .iter()
                    .map(|path| {
                        let service = &path["backend"]["service"];
                        (
                            path["path"].as_str().unwrap_or_default().to_string(),
                            service["name"].as_str().unwrap_or_default().to_string(),
                            service["port"]["number"].as_u64().unwrap_or_default(),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Forgejo is public on the deployment's one load balancer: its host,
    /// read from `NAVIGATOR_FORGEJO_ROOT_URL` rather than a second coordinate,
    /// routes to the Forgejo Service behind its own certificate, and that
    /// Service is a NEG backend whose health check is Forgejo's own probe.
    #[test]
    fn the_forgejo_host_routes_to_forgejo_behind_its_own_certificate() {
        for (deployment, env, host, namespace) in [
            (
                "example-production",
                FULL_ENV,
                "forge.example.invalid",
                "example-production",
            ),
            (
                "example-staging-project",
                HUB_ENV,
                "forge-navigator.example.invalid",
                "example-staging-project",
            ),
        ] {
            let subs =
                resolve_substitutions_for_deployment(deployment, "26.10.15", env_getter(env))
                    .expect("env resolves");
            let rendered = render_manifests_with(&subs).expect("render succeeds");
            let gke = rendered.path().join(GKE_KUSTOMIZE_SUBPATH);
            let manifests = kustomize_build(&gke).expect("rendered GKE manifests build");

            let ingress = manifest_doc(&manifests, "Ingress", "navigator-web-gke");
            assert_eq!(
                ingress_routes(&ingress, host),
                vec![("/*".to_string(), "navigator-forgejo".to_string(), 3000)],
                "{deployment}: the Forgejo host routes only to Forgejo"
            );
            let certificate = manifest_doc(&manifests, "ManagedCertificate", "navigator-forgejo");
            assert_eq!(
                certificate_domains(&certificate),
                vec![host],
                "{deployment}"
            );
            assert_eq!(
                certificate["metadata"]["namespace"].as_str(),
                Some(namespace),
                "{deployment}: the certificate sits beside the Ingress"
            );
            let attached = ingress["metadata"]["annotations"]
                ["networking.gke.io/managed-certificates"]
                .as_str()
                .expect("Ingress attaches certificates");
            assert!(
                attached.split(',').any(|name| name == "navigator-forgejo"),
                "{deployment}: the Ingress attaches the Forgejo certificate: {attached}"
            );
            let backend = manifest_doc(&manifests, "BackendConfig", "navigator-forgejo");
            assert_eq!(backend["metadata"]["namespace"].as_str(), Some(namespace));
            assert_eq!(
                backend["spec"]["healthCheck"]["requestPath"].as_str(),
                Some("/api/healthz")
            );
            assert_eq!(backend["spec"]["healthCheck"]["port"].as_u64(), Some(3000));

            for environment in ["staging", "production"] {
                let forgejo = kustomize_build(&gke.join("forgejo").join(environment))
                    .expect("rendered Forgejo overlay builds");
                let service = manifest_doc(&forgejo, "Service", "navigator-forgejo");
                let annotations = &service["metadata"]["annotations"];
                assert_eq!(
                    annotations["cloud.google.com/neg"].as_str(),
                    Some(r#"{"ingress": true}"#),
                    "{environment}: the Forgejo Service is a container-native backend"
                );
                assert_eq!(
                    annotations["cloud.google.com/backend-config"].as_str(),
                    Some(r#"{"default": "navigator-forgejo"}"#),
                    "{environment}: the Forgejo Service uses its BackendConfig"
                );
            }
        }
    }

    #[test]
    fn the_forgejo_host_is_the_bare_host_of_its_root_url() {
        let resolve = |root_url: Option<&'static str>| {
            resolve_substitutions_for_deployment("example-deployment", "26.10.15", move |key| {
                if key == "NAVIGATOR_FORGEJO_ROOT_URL" {
                    root_url.map(str::to_owned)
                } else {
                    env_getter(FULL_ENV)(key)
                }
            })
        };
        let host = resolve(Some("https://forge.example.com/"))
            .expect("a bare HTTPS root URL resolves")
            .into_iter()
            .find(|substitution| substitution.token == FORGEJO_HOST_TOKEN)
            .expect("the Forgejo host renders")
            .value;
        assert_eq!(host, "forge.example.com");
        for refused in [
            None,
            Some("http://forge.example.com/"),
            Some("https://forge.example.com/git/"),
            Some("https://forge.example.com:8443/"),
            Some("https:///"),
        ] {
            let err =
                resolve(refused).expect_err("only a bare HTTPS host may become an Ingress host");
            assert!(
                err.to_string().contains("NAVIGATOR_FORGEJO_ROOT_URL")
                    && err
                        .to_string()
                        .contains("deployments/example-deployment/config.toml"),
                "{refused:?}: {err}"
            );
        }
    }

    fn certificate_domains(doc: &serde_yaml::Value) -> Vec<&str> {
        doc.get("spec")
            .and_then(|spec| spec.get("domains"))
            .and_then(serde_yaml::Value::as_sequence)
            .expect("ManagedCertificate lists domains")
            .iter()
            .map(|domain| domain.as_str().expect("domain is a string"))
            .collect()
    }

    /// Each registered brand family is its own `ManagedCertificate` on the
    /// shared Ingress, launched or not (ENG-808). A Google-managed
    /// certificate reissues as a whole when `spec.domains` changes, so
    /// sharing one object across families is how a DNS miss on a marketing
    /// host takes the firm's own site offline.
    #[test]
    fn each_registered_brand_family_has_its_own_managed_certificate() {
        let subs = resolve_substitutions_for_deployment(
            "example-production",
            "26.7.15",
            env_getter(FULL_ENV),
        )
        .expect("full env resolves");
        let rendered = render_manifests_with(&subs).expect("render succeeds");
        let manifests = kustomize_build(&rendered.path().join(GKE_KUSTOMIZE_SUBPATH))
            .expect("rendered GKE manifests build");

        assert_eq!(
            certificate_domains(&manifest_doc(
                &manifests,
                "ManagedCertificate",
                "navigator-web"
            )),
            vec!["navigator.example.invalid"]
        );
        let firm_certificate =
            manifest_doc(&manifests, "ManagedCertificate", FIRM_CERTIFICATE_NAME);
        let firm_domains = certificate_domains(&firm_certificate);
        assert!(firm_domains.contains(&"navigator.example.invalid"));
        let mut certificate_names = vec![
            "navigator-web".to_string(),
            FIRM_CERTIFICATE_NAME.to_string(),
        ];
        for key in views::brand::BrandKey::ALL {
            if *key == views::brand::BrandKey::default() {
                continue;
            }
            assert!(
                firm_domains.contains(&key.canonical_host()),
                "{}'s www host is on the Firm certificate",
                key.as_str(),
            );
            if !has_own_certificate(*key) {
                continue;
            }
            let name = brand_certificate_name(*key);
            assert_eq!(
                certificate_domains(&manifest_doc(&manifests, "ManagedCertificate", &name)),
                vec![key.canonical_host()],
                "{}'s production certificate must list only its own www host",
                key.as_str(),
            );
            certificate_names.push(name);
        }
        certificate_names.push("navigator-workflows".to_string());
        for name in &certificate_names {
            for domain in certificate_domains(&manifest_doc(&manifests, "ManagedCertificate", name))
            {
                assert!(
                    domain.contains('.'),
                    "each certificate domain must be a hostname"
                );
                assert!(
                    domain.starts_with("www.")
                        || domain.starts_with("navigator.")
                        || domain.starts_with("workflows.")
                        || domain.starts_with("forge.")
                        || domain.starts_with("staging."),
                    "managed certificates must not include an apex"
                );
            }
        }

        let ingress = manifest_doc(&manifests, "Ingress", "navigator-web-gke");
        assert_eq!(
            ingress
                .get("metadata")
                .and_then(|metadata| metadata.get("annotations"))
                .and_then(|annotations| annotations.get("networking.gke.io/managed-certificates"))
                .and_then(serde_yaml::Value::as_str),
            Some(expected_managed_certificates_annotation().as_str())
        );
        let hosts: Vec<&str> = ingress
            .get("spec")
            .and_then(|spec| spec.get("rules"))
            .and_then(serde_yaml::Value::as_sequence)
            .expect("Ingress has host rules")
            .iter()
            .filter_map(|rule| rule.get("host").and_then(serde_yaml::Value::as_str))
            .collect();
        for key in views::brand::BrandKey::ALL {
            if *key == views::brand::BrandKey::default() {
                continue;
            }
            assert!(
                hosts.contains(&key.canonical_host()),
                "{}'s production host gets an Ingress rule: {hosts:?}",
                key.as_str(),
            );
            assert!(
                !hosts.contains(&key.apex()),
                "{}'s apex must not carry an Ingress rule",
                key.as_str(),
            );
        }
    }

    /// Collect every `env:` entry declared anywhere under `node`, as
    /// `(name, entry)`. Recursive rather than pointer-indexed because the
    /// scheduled jobs nest their pod spec differently from a Deployment
    /// (`spec.jobTemplate.spec.template.spec.containers`), and a check that
    /// hard-codes one shape silently stops looking when the other appears.
    fn env_entries(node: &serde_json::Value, found: &mut Vec<(String, serde_json::Value)>) {
        match node {
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::Array(env)) = map.get("env") {
                    for entry in env {
                        if let Some(name) = entry.get("name").and_then(serde_json::Value::as_str) {
                            found.push((name.to_string(), entry.clone()));
                        }
                    }
                }
                for value in map.values() {
                    env_entries(value, found);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    env_entries(item, found);
                }
            }
            _ => {}
        }
    }

    /// A key the `SecretProviderClass` projects must reach a scheduled trigger
    /// FROM that Secret, never as an inline literal.
    ///
    /// `ops ship` applies these `CronJob`s over the live objects, and a
    /// container's `env` list merges by entry name. An inline `value` here
    /// therefore merges with the cluster's `valueFrom` into a single entry
    /// carrying both, which the API server rejects outright — "may not be
    /// specified when `value` is not empty". That aborted `kubectl diff -k`
    /// before it compared anything, so for two releases no manifest change of
    /// any kind could reach either cluster and version rolls were stuck on
    /// `--image-only`. The projected set is read from the class itself, so the
    /// next trigger that inlines a projected key fails here without this test
    /// being updated.
    ///
    /// Deliberately scoped to the exports tree. `k8s/base/web/web.yaml` sets
    /// several projected keys inline as KIND development values, and the GKE
    /// overlay's `patches/web-env.yaml` `$patch: replace`s that whole list
    /// away. No patch rewrites these `CronJob`s, so what the file says is what
    /// ships.
    #[test]
    fn triggers_source_every_projected_key_from_the_deployment_secret() {
        let subs = resolve_substitutions_for_deployment(
            "example-production",
            "26.7.15",
            env_getter(FULL_ENV),
        )
        .expect("full env resolves");
        let rendered = render_manifests_with(&subs).expect("render succeeds");
        let projected = secret_provider_class_keys();
        // The render substitutes the Secret's name per deployment, so the
        // reference has to follow it — a trigger pinned to the literal
        // `navigator-web-secrets` would read another deployment's Secret, or
        // more likely none at all.
        let secret_name = FULL_ENV
            .iter()
            .find(|(key, _)| *key == "NAVIGATOR_WEB_SECRET_NAME")
            .map(|(_, value)| *value)
            .expect("the render env names the deployment Secret");

        let mut sourced_from_secret = 0;
        for entry in walkdir::WalkDir::new(rendered.path().join(EXPORTS_KUSTOMIZE_SUBPATH)) {
            let entry = entry.expect("walk the rendered exports tree");
            if !entry.file_type().is_file() {
                continue;
            }
            let text = fs::read_to_string(entry.path()).expect("read rendered manifest");
            let manifest: serde_json::Value =
                serde_yaml::from_str(&text).expect("rendered scheduled-job manifest parses");
            let mut env = Vec::new();
            env_entries(&manifest, &mut env);
            let file = entry.path().display();
            for (name, declared) in env {
                if !projected.contains(&name) {
                    continue;
                }
                assert!(
                    declared.get("value").is_none(),
                    "{file} sets `{name}` — a key the SecretProviderClass projects — as an inline \
                     literal. Applying that over the live CronJob merges `value` and `valueFrom` \
                     into one env entry and the API server rejects the whole object, taking every \
                     manifest change down with it. Use \
                     `valueFrom.secretKeyRef` into the deployment Secret."
                );
                let secret_ref = declared
                    .pointer("/valueFrom/secretKeyRef")
                    .unwrap_or_else(|| panic!("{file} sources `{name}` from a Secret"));
                assert_eq!(
                    secret_ref.get("name").and_then(serde_json::Value::as_str),
                    Some(secret_name),
                    "{file} must read `{name}` from this deployment's own Secret"
                );
                assert_eq!(
                    secret_ref.get("key").and_then(serde_json::Value::as_str),
                    Some(name.as_str()),
                    "{file} must read `{name}` from the Secret key of the same name"
                );
                // `optional: true` is load-bearing, not decoration.
                // `omit_unwritten_objects` drops an object the shipping
                // deployment does not write from the rendered class, so the
                // deployment Secret of an ordinary row carries no
                // RESTATE_INGRESS_URL at all — the assertion below proves that
                // from the fixture tree. A required reference would then leave
                // every trigger pod in CreateContainerConfigError, never
                // started and never logging why.
                assert_eq!(
                    secret_ref
                        .get("optional")
                        .and_then(serde_json::Value::as_bool),
                    Some(true),
                    "{file} must mark its `{name}` reference optional, or a deployment that does \
                     not write the key gets pods that never start instead of a trigger that \
                     exits naming the missing value"
                );
                sourced_from_secret += 1;
            }
        }

        // Each Restate trigger CronJob carries RESTATE_INGRESS_URL and
        // RESTATE_AUTH_TOKEN. A floor rather than an equality so a new trigger
        // does not fail this, while a file that quietly drops the references —
        // the state that would make the loop above vacuous — does.
        assert!(
            sourced_from_secret >= 8,
            "expected the Restate triggers' two keys to be sourced from the deployment Secret"
        );

        // The premise behind `optional: true`, proven rather than asserted:
        // an ordinary deployment legitimately does not project this key.
        let ordinary = super::super::deployments::Deployment::load(&fixture_tree(), ORDINARY_ROW)
            .expect("the ordinary row loads");
        assert!(
            ordinary.provisioned,
            "{ORDINARY_ROW} must be provisioned or the omission below is computed from no \
             secrets file at all and this assertion proves nothing"
        );
        assert!(
            super::super::deployments::skipped_projected_objects(&ordinary)
                .expect("the tree is complete")
                .contains("RESTATE_INGRESS_URL"),
            "RESTATE_INGRESS_URL is scoped to the automation home, so an ordinary deployment's \
             Secret does not carry it — which is why the triggers' reference must be optional"
        );
    }

    #[test]
    fn web_pod_binds_the_navigator_web_service_account() {
        // Regression guard. The `navigator-web` Deployment MUST render with
        // `serviceAccountName: navigator-web` so the pod runs under Workload
        // Identity as the KSA-bound GSA (`navigator-web@…`, which holds the
        // object-storage and Secret Manager grants). The base
        // (`k8s/base/web/web.yaml`) omits it
        // because KIND has no such KSA, so the GKE overlay owns it. When it
        // went missing, `kubectl apply` pruned the field from the live
        // Deployment and the pod fell back to the `default` KSA, whose GSA
        // holds none of those grants. Assert the overlay patch pins it so a
        // future edit can't silently drop it again.
        let subs = resolve_substitutions_for_deployment(
            "example-production",
            "26.7.15",
            env_getter(FULL_ENV),
        )
        .expect("full env resolves");
        let rendered = render_manifests_with(&subs).expect("render succeeds");
        let gke = rendered.path().join(GKE_KUSTOMIZE_SUBPATH);

        // (1) The patch file pins the KSA on the pod spec.
        let sa_patch = fs::read_to_string(gke.join("patches/web-service-account.yaml")).unwrap();
        assert!(
            sa_patch.contains("serviceAccountName: navigator-web"),
            "the web overlay must bind the navigator-web KSA, or the pod \
             falls back to `default` and loses every Workload Identity grant"
        );

        // (2) …and the kustomization actually APPLIES that patch to the
        // navigator-web Deployment. Checking only (1) would still pass if the
        // patch were dropped from `kustomization.yaml`'s `patches:` list or
        // retargeted — the file would keep the line while `kubectl apply -k`
        // rendered a pod on the `default` KSA. Assert
        // the exact wiring block so removal or retarget fails the test too.
        let kustomization = fs::read_to_string(gke.join("kustomization.yaml")).unwrap();
        assert!(
            kustomization.contains(
                "path: patches/web-service-account.yaml\n    \
                 target:\n      kind: Deployment\n      name: navigator-web"
            ),
            "kustomization must apply web-service-account.yaml to the \
             navigator-web Deployment, or the pinned KSA never reaches the pod"
        );

        // (3) The real thing: build the overlay with `kubectl kustomize` — the
        // same kustomize `ops ship` runs via `kubectl -k` — and assert the
        // MERGED navigator-web Deployment carries the KSA. This proves the
        // strategic-merge patch actually lands, not just that the text is
        // present (Greptile P1/security on #395). The `cargo test (workspace)`
        // CI job ships no kubectl (see .github/workflows/ci.yml), so this is
        // skipped there — (1)+(2) still guard the realistic removal/retarget
        // edits; where kubectl exists (dev, the deploy job) it is authoritative.
        match kustomize_deployment_service_account(gke.as_path(), "navigator-web") {
            KustomizeSa::ServiceAccount(sa) => assert_eq!(
                sa.as_deref(),
                Some("navigator-web"),
                "kustomize-rendered navigator-web Deployment must set \
                 serviceAccountName: navigator-web"
            ),
            KustomizeSa::KubectlUnavailable => eprintln!(
                "kubectl not available — skipping the kustomize-render assertion; \
                 wiring + patch-text assertions still ran"
            ),
        }
    }
    /// Outcome of reading a Deployment's KSA from a `kubectl kustomize` build.
    enum KustomizeSa {
        /// kubectl is absent or the build failed — the caller skips the render
        /// assertion (the `cargo test (workspace)` CI job has no kubectl; the
        /// text + wiring assertions still guard there).
        KubectlUnavailable,
        /// The named Deployment's `serviceAccountName` (`None` = unset).
        ServiceAccount(Option<String>),
    }

    /// Build the rendered GKE overlay with `kubectl kustomize` (the kustomize
    /// engine `ops ship` drives through `kubectl -k`) and return the named
    /// Deployment's `.spec.template.spec.serviceAccountName`.
    fn kustomize_deployment_service_account(gke_dir: &Path, deployment: &str) -> KustomizeSa {
        use serde::Deserialize;
        let Ok(out) = Command::new("kubectl")
            .arg("kustomize")
            .arg(gke_dir)
            .output()
        else {
            return KustomizeSa::KubectlUnavailable;
        };
        if !out.status.success() {
            return KustomizeSa::KubectlUnavailable;
        }
        let Ok(stdout) = String::from_utf8(out.stdout) else {
            return KustomizeSa::KubectlUnavailable;
        };
        for document in serde_yaml::Deserializer::from_str(&stdout) {
            let Ok(value) = serde_yaml::Value::deserialize(document) else {
                continue;
            };
            let is_deployment =
                value.get("kind").and_then(serde_yaml::Value::as_str) == Some("Deployment");
            let name = value
                .get("metadata")
                .and_then(|m| m.get("name"))
                .and_then(serde_yaml::Value::as_str);
            if is_deployment && name == Some(deployment) {
                let sa = value
                    .get("spec")
                    .and_then(|s| s.get("template"))
                    .and_then(|t| t.get("spec"))
                    .and_then(|s| s.get("serviceAccountName"))
                    .and_then(serde_yaml::Value::as_str)
                    .map(str::to_string);
                return KustomizeSa::ServiceAccount(sa);
            }
        }
        KustomizeSa::ServiceAccount(None)
    }

    #[test]
    fn resolve_substitutions_fails_by_name_when_a_var_is_missing() {
        // TDD step 2: a missing (or blank) required substitution var → a
        // named, actionable error, before anything is written (no partial
        // apply). Assert each var is the one that trips the error.
        for missing in [
            "NAVIGATOR_GCP_PROJECT_ID",
            "NAVIGATOR_GCP_LOCATION",
            "NAVIGATOR_OAUTH_CLIENT_ID_BROWSER",
            "NAVIGATOR_FORGEJO_OIDC_NAME",
            "NAVIGATOR_FORGEJO_OIDC_DISCOVERY_URL",
            "NAVIGATOR_FORGEJO_ADMISSION_DOMAINS",
        ] {
            // Keep every other setting valid so only the missing var trips the error.
            let getter = |key: &str| {
                if key == missing {
                    None
                } else {
                    env_getter(FULL_ENV)(key)
                }
            };
            let err = resolve_substitutions_for_deployment("example-production", "26.7.15", getter)
                .expect_err("a missing var must fail the resolve")
                .to_string();
            assert!(
                err.contains(missing),
                "error must name the missing var `{missing}`, got: {err}"
            );
            assert!(
                err.contains("deployments/example-production/config.toml"),
                "error must send the operator to the deployment's own config file, \
                 never a retired Doppler config: {err}"
            );
        }
    }

    #[test]
    fn forgejo_oidc_coordinates_render_from_the_deployment() {
        let substitutions = resolve_substitutions_for_deployment(
            "example-production",
            "26.10.15",
            env_getter(FULL_ENV),
        )
        .expect("the synthetic deployment supplies every Forgejo coordinate");

        for (token, expected) in [
            ("YOUR_FORGEJO_OIDC_NAME", "oidc"),
            (
                "YOUR_FORGEJO_OIDC_DISCOVERY_URL",
                "https://identity.example/.well-known/openid-configuration",
            ),
            ("YOUR_FORGEJO_ADMISSION_DOMAINS", "example.com"),
        ] {
            let rendered = substitutions
                .iter()
                .find(|substitution| substitution.token == token)
                .unwrap_or_else(|| panic!("missing required substitution for {token}"));
            assert_eq!(rendered.value, expected, "{token}");
        }
    }

    #[test]
    fn resolve_substitutions_rejects_a_bare_oauth_client_id() {
        // A bare OAuth id (no `.apps.googleusercontent.com`) renders an
        // `OAUTH_CLIENT_ID` Google won't match, breaking login. The resolve
        // must refuse it by name rather than ship a broken redirect.
        let bare = "NAVIGATOR_OAUTH_CLIENT_ID_BROWSER";
        let getter = |key: &str| {
            if key == bare {
                Some("1234567890-bareid".to_string()) // no suffix
            } else {
                env_getter(FULL_ENV)(key)
            }
        };
        let err = resolve_substitutions_for_deployment("example-production", "26.7.15", getter)
            .expect_err("a bare OAuth id must fail the resolve")
            .to_string();
        assert!(
            err.contains(bare),
            "error names the offending var `{bare}`: {err}"
        );
        assert!(
            err.contains(GOOGLE_OAUTH_CLIENT_ID_SUFFIX),
            "error names the required suffix: {err}"
        );

        // A value with the suffix somewhere in the MIDDLE (trailing garbage
        // after it) is also malformed — Google matches the client id
        // verbatim, so `123.apps.googleusercontent.com.extra` renders an
        // `OAUTH_CLIENT_ID` login rejects. The suffix must be at the END.
        let malformed = "NAVIGATOR_OAUTH_CLIENT_ID_BROWSER";
        let getter = |key: &str| {
            if key == malformed {
                Some("123.apps.googleusercontent.com.extra".to_string()) // suffix not at end
            } else {
                env_getter(FULL_ENV)(key)
            }
        };
        let err = resolve_substitutions_for_deployment("example-production", "26.7.15", getter)
            .expect_err("a client id with the suffix not at the end must fail")
            .to_string();
        assert!(
            err.contains(malformed),
            "error names the offending var `{malformed}`: {err}"
        );

        // The full-id form (both OAuth vars carry the suffix) resolves fine.
        assert!(resolve_substitutions_for_deployment(
            "example-production",
            "26.7.15",
            env_getter(FULL_ENV)
        )
        .is_ok());
    }

    #[test]
    fn render_temp_dir_is_removed_when_dropped() {
        // TDD step 3: the rendered manifests live only for the span of the
        // `kubectl` calls — dropping the TempDir removes them (no leak).
        let subs = resolve_substitutions_for_deployment(
            "example-production",
            "26.7.15",
            env_getter(FULL_ENV),
        )
        .unwrap();
        let rendered = render_manifests_with(&subs).unwrap();
        let path = rendered.path().to_path_buf();
        assert!(path.join(GKE_KUSTOMIZE_SUBPATH).exists());
        drop(rendered);
        assert!(
            !path.exists(),
            "the rendered manifest temp dir must be gone after drop"
        );
    }

    #[test]
    fn rendered_release_plan_includes_collector_when_config_changes() {
        let coordinates: BTreeMap<String, String> = FULL_ENV
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        let deployment = super::super::deployments::Deployment {
            name: "example-prod".into(),
            kms_key:
                "projects/my-org-prod/locations/example-region/keyRings/sops/cryptoKeys/navigator"
                    .into(),
            provisioned: true,
            coordinates,
            encrypted_keys: BTreeSet::new(),
            postgres_encrypted_keys: BTreeSet::new(),
            forgejo_encrypted_keys: BTreeSet::new(),
        };
        let subs = resolve_substitutions_for_deployment(&deployment.name, "26.9.24", |key| {
            deployment.coordinates.get(key).cloned()
        })
        .expect("release substitutions resolve");
        let first = render_release_plan(&subs, &sample_config(), &deployment)
            .expect("release plan renders");
        assert!(first.collector.contains("name: otel-collector-config"));
        assert!(first.collector.contains("checksum/otel-collector-config: "));
        assert!(!first.collector.contains("YOUR_CONFIG_CHECKSUM"));

        let mut changed = deployment.clone();
        changed
            .coordinates
            .insert("DASH0_ENDPOINT".into(), "https://dash0.example".into());
        changed
            .coordinates
            .insert("DASH0_DATASET".into(), "staging".into());
        changed.encrypted_keys.insert("DASH0_TOKEN".into());
        let changed_subs = resolve_substitutions_for_deployment(&changed.name, "26.9.24", |key| {
            changed.coordinates.get(key).cloned()
        })
        .expect("changed release substitutions resolve");
        let second = render_release_plan(&changed_subs, &sample_config(), &changed)
            .expect("changed release plan renders");
        let checksum = |manifest: &str| {
            manifest
                .lines()
                .find_map(|line| line.trim().strip_prefix("checksum/otel-collector-config: "))
                .map(str::to_owned)
                .expect("collector checksum annotation is rendered")
        };
        assert_ne!(
            checksum(&first.collector),
            checksum(&second.collector),
            "a collector config change must change the release plan checksum"
        );
    }

    #[test]
    fn collector_manifest_drift_applies_only_after_a_successful_diff() {
        let mut commands = Vec::new();
        let changed = reconcile_manifest_file(false, |verb| {
            commands.push(verb.to_owned());
            Ok(i32::from(verb == "diff"))
        })
        .expect("collector drift applies");
        assert!(changed);
        assert_eq!(commands, ["diff".to_owned(), "apply".to_owned()]);

        let mut dry_run_commands = Vec::new();
        let unchanged = reconcile_manifest_file(true, |verb| {
            dry_run_commands.push(verb.to_owned());
            Ok(0)
        })
        .expect("unchanged collector diff succeeds");
        assert!(!unchanged);
        assert_eq!(dry_run_commands, ["diff".to_owned()]);
    }

    #[test]
    fn dry_run_reconcile_diffs_but_never_applies() {
        // TDD step 5: `--dry-run` renders + diffs, never applies.
        assert_eq!(reconcile_verbs(true), &["diff"]);
        assert!(
            !reconcile_verbs(true).contains(&"apply"),
            "dry-run must not apply"
        );
        // A live run diffs THEN applies.
        assert_eq!(reconcile_verbs(false), &["diff", "apply"]);

        // Drive the executor with a recorder (no cluster): dry-run issues
        // `diff` only; a live run issues `diff` then `apply`.
        let mut dry = Vec::new();
        reconcile_kustomize(true, |verb| {
            dry.push(verb.to_string());
            Ok(0)
        })
        .unwrap();
        assert_eq!(dry, vec!["diff"], "dry-run must not apply");

        let mut live = Vec::new();
        reconcile_kustomize(false, |verb| {
            live.push(verb.to_string());
            Ok(0)
        })
        .unwrap();
        assert_eq!(live, vec!["diff", "apply"]);
    }

    #[test]
    fn reconcile_diff_drift_is_benign_but_diff_errors_abort() {
        // exit 0 = no drift, exit 1 = drift (the normal signal) → both proceed.
        assert!(reconcile_kustomize(true, |_| Ok(0)).is_ok());
        assert!(reconcile_kustomize(true, |_| Ok(1)).is_ok());
        // exit >1 = a real diff error (bad context / auth / kustomize build) →
        // the mandatory drift-review gate never ran, so it aborts rather than
        // let a `--dry-run` exit 0 or a live run fall through to `apply`.
        let errored = reconcile_kustomize(true, |_| Ok(2));
        assert!(errored.is_err(), "a >1 diff exit must abort");
        assert!(
            errored.unwrap_err().to_string().contains("NOT checked"),
            "the abort names that the manifests were not checked"
        );
        // …as does a kubectl that cannot be spawned.
        let unspawnable = reconcile_kustomize(true, |_| -> Result<i32> { bail!("no kubectl") });
        assert!(unspawnable.is_err(), "an unspawnable diff must abort");
    }

    #[test]
    fn reconcile_diff_retries_a_transient_error_then_proceeds() {
        let mut calls = 0_u32;
        let result = reconcile_kustomize(true, |_| {
            calls += 1;
            Ok(if calls < DIFF_ATTEMPTS { 2 } else { 0 })
        });
        assert!(result.is_ok(), "a diff that recovers must not abort");
        assert_eq!(calls, DIFF_ATTEMPTS);

        let mut persistent = 0_u32;
        let failed = reconcile_kustomize(true, |_| {
            persistent += 1;
            Ok(2)
        });
        assert!(failed.is_err());
        assert_eq!(persistent, DIFF_ATTEMPTS, "stops after the cap");
    }

    #[test]
    fn reconcile_apply_failure_aborts() {
        // A non-zero apply exit aborts the reconcile — a partial apply is
        // worse than none.
        let nonzero =
            reconcile_kustomize(false, |verb| if verb == "apply" { Ok(1) } else { Ok(0) });
        assert!(nonzero.is_err(), "a non-zero apply exit must abort");
        // As does an apply whose kubectl cannot be spawned.
        let unspawnable = reconcile_kustomize(false, |verb| -> Result<i32> {
            if verb == "apply" {
                bail!("boom")
            }
            Ok(0)
        });
        assert!(unspawnable.is_err(), "an unspawnable apply must abort");
    }

    #[test]
    fn reconcile_is_self_contained_needs_no_overlay_dir() {
        // TDD step 6: the reconcile is env-driven from the four substitution
        // vars only — NAVIGATOR_GKE_OVERLAY_DIR is retired and reading it is
        // gone. The render succeeds with only those vars set; there is no
        // overlay path in the ShipConfig or the substitution table.
        let subs = resolve_substitutions_for_deployment(
            "example-production",
            "26.7.15",
            env_getter(FULL_ENV),
        )
        .expect("no overlay dir required");
        assert!(
            subs.iter().all(|s| s.env != "NAVIGATOR_GKE_OVERLAY_DIR"),
            "the retired overlay-dir var is not part of the substitution table"
        );
        render_manifests_with(&subs).expect("render needs no overlay folder on disk");
    }

    #[test]
    fn workflows_patch_names_only_containers_the_base_deployment_has() {
        use serde::Deserialize;
        // A strategic-merge patch keys containers by name. #978's env patch
        // said `workflows-service` while the base container is `worker`, so
        // the render grew a second, image-less container and every
        // `kubectl diff -k` died with "image: Required value" (26.10.5).
        let containers = |path: &str| -> Vec<String> {
            let text = GKE_MANIFESTS
                .get_file(path)
                .unwrap_or_else(|| panic!("{path} is embedded"))
                .contents_utf8()
                .unwrap();
            serde_yaml::Deserializer::from_str(text)
                .map(|doc| serde_json::Value::deserialize(doc).unwrap())
                .filter(|v| {
                    v.get("kind").and_then(serde_json::Value::as_str) == Some("Deployment")
                        && v.pointer("/metadata/name")
                            .and_then(serde_json::Value::as_str)
                            == Some(WORKFLOWS_DEPLOYMENT)
                })
                .flat_map(|v| {
                    v.pointer("/spec/template/spec/containers")
                        .and_then(serde_json::Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                })
                .filter_map(|c| c.get("name")?.as_str().map(str::to_string))
                .collect()
        };
        let base = containers("workflows-service/deployment.yaml");
        let patched = containers("patches/workflows-env.yaml");
        assert!(!patched.is_empty(), "the env patch names its container");
        for name in &patched {
            assert!(
                base.contains(name),
                "patch container `{name}` is not in the base Deployment {base:?}"
            );
        }
    }

    #[test]
    fn apply_substitutions_replaces_all_tokens_in_one_pass() {
        let subs = vec![
            Substitution {
                token: "YOUR_PROJECT_ID",
                env: "NAVIGATOR_GCP_PROJECT_ID",
                value: "proj-1".into(),
            },
            Substitution {
                token: "your-domain.example",
                env: "brand.primary_domain",
                value: "example.org".into(),
            },
        ];
        let out = apply_substitutions(
            "bucket: YOUR_PROJECT_ID-assets host: www.your-domain.example YOUR_PROJECT_ID",
            &subs,
        );
        assert_eq!(out, "bucket: proj-1-assets host: www.example.org proj-1");
    }

    #[test]
    fn asset_base_url_present_requires_a_non_blank_origin() {
        // The ship gate treats unset and blank identically: both mean the
        // rolled site would resolve images against an empty `/public`.
        assert!(!asset_base_url_present(None));
        assert!(!asset_base_url_present(Some("")));
        assert!(!asset_base_url_present(Some("   ")));
        assert!(asset_base_url_present(Some(
            "https://storage.googleapis.com/my-org-prod-assets"
        )));
    }

    #[test]
    fn require_asset_base_url_value_names_the_key_and_the_config_file_when_absent() {
        assert!(require_asset_base_url_value(
            "example-prod",
            Some("https://storage.googleapis.com/my-org-prod-assets")
        )
        .is_ok());
        let err = require_asset_base_url_value("example-prod", None)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(ASSET_BASE_URL_KEY),
            "names the key, got: {err}"
        );
        assert!(
            err.contains("deployments/example-prod/config.toml"),
            "points at the deployment's own config file, got: {err}"
        );
        assert!(
            err.contains("ops ship --deployment example-prod"),
            "the re-run line carries the explicit deployment flag, got: {err}"
        );
    }

    #[test]
    fn asset_bucket_preflight_uses_the_selected_deployments_coordinate() {
        let mut checked = None;
        verify_assets_bucket_value_with(
            "example-prod",
            Some("  example-prod-assets  "),
            |bucket| {
                checked = Some(bucket.to_string());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(checked.as_deref(), Some("example-prod-assets"));

        let error = verify_assets_bucket_value_with("example-prod", None, |_| Ok(()))
            .unwrap_err()
            .to_string();
        assert!(error.contains(ASSETS_BUCKET_KEY), "got: {error}");
        assert!(
            error.contains("deployments/example-prod/config.toml"),
            "got: {error}"
        );
    }

    /// The preflight reads the bucket from the selected deployment's own
    /// coordinates, so a row that never declared one stops the ship before any
    /// storage client is opened.
    #[test]
    fn asset_bucket_preflight_stops_a_deployment_with_no_bucket_coordinate() {
        let deployment = super::super::deployments::Deployment {
            name: "example-prod".to_string(),
            kms_key:
                "projects/my-org-prod/locations/example-region/keyRings/sops/cryptoKeys/navigator"
                    .to_string(),
            provisioned: true,
            coordinates: std::collections::BTreeMap::new(),
            encrypted_keys: std::collections::BTreeSet::new(),
            postgres_encrypted_keys: std::collections::BTreeSet::new(),
            forgejo_encrypted_keys: std::collections::BTreeSet::new(),
        };

        let error = verify_assets_bucket(Path::new("."), &deployment)
            .expect_err("a deployment with no assets bucket cannot be preflighted")
            .to_string();

        assert!(error.contains(ASSETS_BUCKET_KEY), "got: {error}");
        assert!(
            error.contains("deployments/example-prod/config.toml"),
            "got: {error}"
        );
    }

    /// The keystone of the no-fallback design: every deployment in the tree
    /// resolves to a complete, self-consistent `ShipConfig` with no process
    /// environment at all. A deployment this test passes for is one
    /// `ops ship --deployment <name>` can at least *begin* to roll — dropping
    /// a new `deployments/<name>/` pair into the tree is the whole activation.
    ///
    /// The tree it reads is the synthetic one; the real rows are held to the
    /// same assertion by `navigator ops deployments`.
    #[test]
    fn every_checked_in_deployment_resolves_a_complete_ship_config() {
        let root = fixture_tree();
        for name in super::super::deployments::names(&root).expect("the tree is readable") {
            let deployment = super::super::deployments::Deployment::load(&root, &name)
                .expect("the deployment loads");
            assert!(
                deployment.provisioned,
                "{name} must be provisioned or this assertion is dormant"
            );
            let cfg = ShipConfig::from_deployment(&deployment)
                .unwrap_or_else(|error| panic!("{name} must resolve a ShipConfig: {error}"));
            assert_eq!(cfg.name, name);
            assert_eq!(
                check_context(&cfg.context, &cfg.project_id, &cfg.location, &cfg.cluster),
                ContextCheck::Matches,
                "{name}'s NAVIGATOR_GKE_CONTEXT must name its own cluster"
            );
            require_asset_base_url(&deployment)
                .unwrap_or_else(|error| panic!("{name} must carry its asset origin: {error}"));
            resolve_substitutions_for_deployment(&name, "26.1.1", |key| {
                deployment.coordinates.get(key).cloned()
            })
            .unwrap_or_else(|error| {
                panic!("{name} must resolve every manifest substitution: {error}")
            });
        }
    }

    #[test]
    fn a_missing_coordinate_names_the_deployments_config_file() {
        let err = ShipConfig::from_lookup("example-prod", |key| {
            (key == "NAVIGATOR_ENVIRONMENT").then(|| "production".to_string())
        })
        .expect_err("an empty coordinate map cannot resolve a ShipConfig")
        .to_string();
        assert!(
            err.contains("deployments/example-prod/config.toml"),
            "the first missing coordinate must name the file to fix: {err}"
        );
    }

    #[test]
    fn derived_names_target_the_published_registry() {
        let cfg = sample_config();
        assert_eq!(cfg.registry(), "ghcr.io/example/navigator");
        assert_eq!(
            cfg.web_image("26.6.23"),
            "ghcr.io/example/navigator/neon-server:26.6.23"
        );
        assert_eq!(
            cfg.workflows_image("26.6.23"),
            "ghcr.io/example/navigator/navigator-workflows-service:26.6.23"
        );
    }

    #[test]
    fn digest_manifest_requires_each_fully_qualified_registry_ref() {
        let valid = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let manifest = std::collections::BTreeMap::from([(
            "ghcr.io/example/navigator/neon-server".to_string(),
            valid.to_string(),
        )]);
        assert_eq!(
            digest_for_ref(&manifest, "ghcr.io/example/navigator/neon-server").unwrap(),
            valid
        );
        let missing = digest_for_ref(&manifest, "ghcr.io/example/navigator/other")
            .unwrap_err()
            .to_string();
        assert!(missing.contains("image digest manifest is missing"));
    }

    const CRON_PREFIX: &str = "registry.example/navigator/";
    const CRON_DIGEST: &str =
        "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const OTHER_DIGEST: &str =
        "sha256:fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    #[test]
    fn image_repository_strips_a_digest_or_a_tag_but_never_a_registry_port() {
        let digest_pinned =
            format!("registry.example/navigator/navigator-archives-trigger@{CRON_DIGEST}");
        assert_eq!(
            image_repository(&digest_pinned),
            "registry.example/navigator/navigator-archives-trigger"
        );
        assert_eq!(
            image_repository("registry.example/navigator/navigator-archives-trigger:26.10.26"),
            "registry.example/navigator/navigator-archives-trigger"
        );
        assert_eq!(
            image_repository("localhost:5000/navigator/navigator-archives-trigger"),
            "localhost:5000/navigator/navigator-archives-trigger"
        );
        assert_eq!(
            image_repository("localhost:5000/navigator/navigator-archives-trigger:26.10.26"),
            "localhost:5000/navigator/navigator-archives-trigger"
        );
    }

    #[test]
    fn cronjob_on_the_digest_maps_digest_is_already_current() {
        let digests = std::collections::BTreeMap::from([(
            "navigator-archives-trigger".to_string(),
            CRON_DIGEST.to_string(),
        )]);
        let image = format!("{CRON_PREFIX}navigator-archives-trigger@{CRON_DIGEST}");
        assert_eq!(
            cronjob_pin_action(&image, CRON_PREFIX, "26.10.26", Some(&digests)),
            Some(CronJobPin::Current)
        );
    }

    #[test]
    fn cronjob_on_another_digest_is_repointed_at_the_maps_digest() {
        let digests = std::collections::BTreeMap::from([(
            "navigator-archives-trigger".to_string(),
            CRON_DIGEST.to_string(),
        )]);
        for image in [
            format!("{CRON_PREFIX}navigator-archives-trigger@{OTHER_DIGEST}"),
            format!("{CRON_PREFIX}navigator-archives-trigger:26.10.19"),
        ] {
            assert_eq!(
                cronjob_pin_action(&image, CRON_PREFIX, "26.10.26", Some(&digests)),
                Some(CronJobPin::Repoint(format!(
                    "{CRON_PREFIX}navigator-archives-trigger@{CRON_DIGEST}"
                )))
            );
        }
    }

    #[test]
    fn cronjob_without_a_digest_map_checks_the_tag_on_the_bare_repository() {
        let expected = Some(CronJobPin::CheckTag {
            repository: "navigator-archives-trigger".to_string(),
            target: format!("{CRON_PREFIX}navigator-archives-trigger:26.10.26"),
        });
        assert_eq!(
            cronjob_pin_action(
                &format!("{CRON_PREFIX}navigator-archives-trigger:26.10.19"),
                CRON_PREFIX,
                "26.10.26",
                None,
            ),
            expected
        );
        // A digest-pinned image no longer leaks `@sha256` into the tag lookup.
        assert_eq!(
            cronjob_pin_action(
                &format!("{CRON_PREFIX}navigator-archives-trigger@{OTHER_DIGEST}"),
                CRON_PREFIX,
                "26.10.26",
                None,
            ),
            expected
        );
    }

    #[test]
    fn cronjob_behind_a_registry_port_keeps_its_port() {
        let prefix = "localhost:5000/navigator/";
        assert_eq!(
            cronjob_pin_action(
                "localhost:5000/navigator/navigator-archives-trigger",
                prefix,
                "26.10.26",
                None,
            ),
            Some(CronJobPin::CheckTag {
                repository: "navigator-archives-trigger".to_string(),
                target: "localhost:5000/navigator/navigator-archives-trigger:26.10.26".to_string(),
            })
        );
    }

    #[test]
    fn cronjob_outside_our_registry_is_left_alone() {
        assert_eq!(
            cronjob_pin_action(
                "docker.io/envoyproxy/envoy:v1",
                CRON_PREFIX,
                "26.10.26",
                None
            ),
            None
        );
    }

    #[test]
    fn cronjob_missing_from_the_digest_map_falls_back_to_the_tag_check() {
        let digests = std::collections::BTreeMap::new();
        assert_eq!(
            cronjob_pin_action(
                &format!("{CRON_PREFIX}navigator-new-trigger:26.10.19"),
                CRON_PREFIX,
                "26.10.26",
                Some(&digests),
            ),
            Some(CronJobPin::CheckTag {
                repository: "navigator-new-trigger".to_string(),
                target: format!("{CRON_PREFIX}navigator-new-trigger:26.10.26"),
            })
        );
    }

    #[test]
    fn digest_manifest_resolves_registry_free_names_before_full_refs() {
        let current = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let legacy = "sha256:fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
        let full = "ghcr.io/example/navigator/neon-server";
        let manifest = std::collections::BTreeMap::from([
            ("neon-server".to_string(), current.to_string()),
            (full.to_string(), legacy.to_string()),
            ("older".to_string(), legacy.to_string()),
        ]);
        assert_eq!(
            digest_for_image(&manifest, "neon-server", full).unwrap(),
            current
        );
        let legacy_only =
            std::collections::BTreeMap::from([(full.to_string(), legacy.to_string())]);
        assert_eq!(
            digest_for_image(&legacy_only, "neon-server", full).unwrap(),
            legacy
        );
        let error = digest_for_image(&legacy_only, "absent", "r/absent")
            .unwrap_err()
            .to_string();
        assert!(error.contains("missing `r/absent`"));
    }

    #[test]
    fn digest_manifest_rejects_mutable_or_malformed_refs() {
        let manifest = std::collections::BTreeMap::from([(
            "registry.example/navigator/web".to_string(),
            "latest".to_string(),
        )]);
        let error = digest_for_ref(&manifest, "registry.example/navigator/web")
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid SHA-256 digest"));
    }

    #[test]
    fn digest_must_match_the_registry_tag() {
        ensure_tag_digest(
            "registry.example/navigator/web",
            "26.10.12",
            "sha256:same",
            "sha256:same",
        )
        .unwrap();
        let error = ensure_tag_digest(
            "registry.example/navigator/web",
            "26.10.12",
            "sha256:expected",
            "sha256:actual",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("26.10.12"));
        assert!(error.contains("sha256:actual"));
    }

    #[test]
    fn unsigned_image_is_refused_before_rendered_manifest_changes() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = directory.path().join("deployment.yaml");
        fs::write(
            &manifest,
            "image: registry.example/navigator/web:26.10.12\n",
        )
        .unwrap();
        let error = pin_verified_image(
            directory.path(),
            "registry.example/navigator/web",
            "26.10.12",
            "sha256:deadbeef",
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("cosign signature was not verified"));
        assert_eq!(
            fs::read_to_string(manifest).unwrap(),
            "image: registry.example/navigator/web:26.10.12\n"
        );
    }

    #[test]
    fn verified_image_pin_rewrites_nested_manifests_and_leaves_other_files_alone() {
        let directory = tempfile::tempdir().unwrap();
        let nested = directory.path().join("base");
        fs::create_dir(&nested).unwrap();
        let image = "registry.example/navigator/web:26.10.12";
        let root_manifest = directory.path().join("deployment.yaml");
        let nested_manifest = nested.join("deployment.yaml");
        let unrelated = nested.join("notes.txt");
        let binary = nested.join("opaque.bin");
        fs::write(&root_manifest, format!("image: {image}\n")).unwrap();
        fs::write(&nested_manifest, format!("image: {image}\n")).unwrap();
        fs::write(&unrelated, "image: registry.example/other:26.10.12\n").unwrap();
        fs::write(&binary, [0xff, 0xfe, 0xfd]).unwrap();

        pin_verified_image(
            directory.path(),
            "registry.example/navigator/web",
            "26.10.12",
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            true,
        )
        .unwrap();

        let pinned = "image: registry.example/navigator/web@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n";
        assert_eq!(fs::read_to_string(root_manifest).unwrap(), pinned);
        assert_eq!(fs::read_to_string(nested_manifest).unwrap(), pinned);
        assert_eq!(
            fs::read_to_string(unrelated).unwrap(),
            "image: registry.example/other:26.10.12\n"
        );
        assert_eq!(fs::read(binary).unwrap(), [0xff, 0xfe, 0xfd]);
    }

    #[test]
    fn every_deployment_pulls_the_same_published_images() {
        // Both rows resolve to one namespace, which is what makes staging a
        // proving ring rather than a different build: production rolls the
        // exact image staging has been serving. The GAR shape needed a region,
        // a hub project, and a repository name to agree before that held; this
        // one cannot disagree with itself.
        assert_eq!(images_registry(None), "");
        assert_eq!(images_registry(Some(TEST_REGISTRY)), TEST_REGISTRY);
    }

    #[test]
    fn a_fork_publishes_and_pulls_from_its_own_namespace() {
        // A fork sets one variable and every image reference follows it.
        assert_eq!(images_registry(Some("ghcr.io/acme")), "ghcr.io/acme");
    }

    #[test]
    fn ship_rolls_exactly_the_two_service_deployments() {
        // The single-writer git tier retired with the topology (#295): a ship
        // restarts and waits on `web` and the worker, and nothing conditional
        // on a third Deployment being installed.
        assert_eq!(
            restart_deployments(),
            ["navigator-web", "workflows-service"]
        );
        assert_eq!(
            rollout_wait_deployments(),
            ["navigator-web", "workflows-service"]
        );
        assert_eq!(web_binary_deployments(), ["navigator-web"]);
    }

    #[test]
    fn workflows_worker_registration_is_required() {
        // Every durable workflow registers with `workflows-service`, so a
        // failed re-register must fail the ship — a stale handler list would
        // silently drop invocations to a new service until a later successful
        // registration.
        let targets = reregister_targets(&sample_config());
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].name, WORKFLOWS_DEPLOYMENT);
        let err = apply_registration_result(
            &targets[0],
            Err(anyhow::anyhow!("registration unavailable")),
        )
        .expect_err("a registration failure must abort the ship");
        assert!(err.to_string().contains(WORKFLOWS_DEPLOYMENT));
    }

    /// A realistic slice of a built stream: our two Deployments, two trigger
    /// `CronJobs`, a third-party sidecar, and the KIND base's registry-less tag.
    const BUILT_STREAM: &str = "\
spec:
  containers:
    - name: web
      image: ghcr.io/example/navigator/neon-server:26.8.31
    - name: clamav
      image: clamav/clamav:1.4.5_base
    - name: workflows-service
      image: \"ghcr.io/example/navigator/navigator-workflows-service:26.8.31\"
    - name: archives-trigger
      image: ghcr.io/example/navigator/navigator-archives-trigger:26.8.31
    - name: general-nag
      image: ghcr.io/example/navigator/navigator-general-nag:26.8.31
    - name: kind-base
      image: navigator-web:dev
";

    #[test]
    fn the_image_preflight_reads_our_images_and_leaves_everyone_elses_alone() {
        // The derived list must be exactly what CI is responsible for
        // publishing. A third-party sidecar is not ours to assert on, and the
        // KIND base's registry-less tag never reaches a cloud registry at all
        // — asserting on either turns the preflight into a false abort.
        let images = manifest_images_under_registry(BUILT_STREAM, "ghcr.io/example/navigator/");
        assert_eq!(
            images.iter().map(String::as_str).collect::<Vec<_>>(),
            [
                "navigator-archives-trigger",
                "navigator-general-nag",
                "navigator-workflows-service",
                "neon-server",
            ],
            "only images under our own registry, tags stripped, deduplicated"
        );
    }

    #[test]
    fn an_unpublished_trigger_image_stops_the_roll() {
        // ENG-215. `publish-triggers` is a `fail-fast: false` matrix, so one
        // failed leg publishes every other image and leaves a hole. The
        // reconcile writes the trigger CronJob's image unconditionally — the
        // render bakes the tag in — so without this abort the roll applies a
        // tag that does not exist and the CronJob stops firing. A CronJob has
        // no rollout to wait on and no replica to report, which is what makes
        // this the failure nobody sees.
        let states = vec![
            ("neon-server".to_string(), ManifestImageStatus::Published),
            (
                "navigator-workflows-service".to_string(),
                ManifestImageStatus::Published,
            ),
            (
                "navigator-archives-trigger".to_string(),
                ManifestImageStatus::Unpublished,
            ),
        ];
        let err = check_manifest_images_published(&states, "26.8.31")
            .expect_err("an unpublished trigger image must abort the roll");
        let message = err.to_string();
        assert!(
            message.contains("navigator-archives-trigger"),
            "the abort must name the image an operator has to re-publish: {message}"
        );
        assert!(
            message.contains("Nothing was applied"),
            "the abort must say the cluster is untouched: {message}"
        );
        assert!(
            !message.contains("neon-server"),
            "a published image must not be reported as missing: {message}"
        );
    }

    #[test]
    fn a_registry_lookup_failure_is_not_reported_as_unpublished() {
        let states = vec![(
            "navigator-archives-trigger".to_string(),
            ManifestImageStatus::LookupFailed("network unavailable".to_string()),
        )];
        let message = check_manifest_images_published(&states, "26.8.31")
            .expect_err("a registry lookup failure must abort the roll")
            .to_string();
        assert!(message.contains("registry lookup failed"), "{message}");
        assert!(message.contains("network unavailable"), "{message}");
        assert!(
            !message.contains("not published"),
            "a lookup failure must not be reported as an absent tag: {message}"
        );
        assert!(message.contains("Nothing was applied"), "{message}");
    }

    #[test]
    fn every_unpublished_image_is_named_at_once() {
        // One abort per re-run teaches the count the slow way; a partial
        // publish is exactly a count mismatch, so report all of them.
        let states = vec![
            (
                "navigator-archives-trigger".to_string(),
                ManifestImageStatus::Unpublished,
            ),
            (
                "navigator-general-nag".to_string(),
                ManifestImageStatus::Unpublished,
            ),
            ("neon-server".to_string(), ManifestImageStatus::Published),
        ];
        let message = check_manifest_images_published(&states, "26.8.31")
            .expect_err("two unpublished images must abort")
            .to_string();
        assert!(message.contains("navigator-archives-trigger"), "{message}");
        assert!(message.contains("navigator-general-nag"), "{message}");
    }

    #[test]
    fn a_fully_published_tag_rolls_unchanged() {
        let states = vec![
            ("neon-server".to_string(), ManifestImageStatus::Published),
            (
                "navigator-archives-trigger".to_string(),
                ManifestImageStatus::Published,
            ),
        ];
        check_manifest_images_published(&states, "26.8.31")
            .expect("a tag published for every rendered image must roll");
    }

    #[test]
    fn the_image_preflight_covers_the_trigger_cronjobs_the_reconcile_writes() {
        // The guard against this check going vacuous. Deriving the list from
        // the rendered tree is only worth anything if the trigger CronJobs are
        // actually in it — `../exports` is the first resource of the GKE
        // kustomization, and a trigger dropped from that list would silently
        // narrow the preflight back to the services.
        let subs = resolve_substitutions_for_deployment(
            "example-staging-project",
            "26.7.28",
            env_getter(HUB_ENV),
        )
        .expect("hub env resolves");
        let rendered = render_manifests_with(&subs).expect("render succeeds");

        let mut stream = String::new();
        for entry in walkdir::WalkDir::new(rendered.path()) {
            let entry = entry.expect("walk rendered tree");
            if entry.file_type().is_file() {
                if let Ok(text) = fs::read_to_string(entry.path()) {
                    stream.push_str(&text);
                    stream.push('\n');
                }
            }
        }
        let images = manifest_images_under_registry(&stream, &format!("{TEST_REGISTRY}/"));
        for required in [
            "navigator-archives-trigger",
            "navigator-billing-digest-trigger",
            "navigator-surreal-archive",
            "navigator-workflows-service",
        ] {
            assert!(
                images.contains(required),
                "`{required}` is applied by the reconcile, so the preflight must check it: \
                 {images:?}"
            );
        }
    }

    #[test]
    fn render_pins_every_navigator_image_to_the_rolled_tag() {
        // The regression that wedged the 26.7.15 ship: the reconcile applied
        // `:YY.M.D` — never a published tag — and left a later `set image` to
        // fix it up. `maxSurge: 0` (workflows-service) deletes the running
        // pod on that first write, so the gap is a real outage on EVERY ship,
        // not just a failed one. The apply must land the real tag itself.
        let subs = resolve_substitutions_for_deployment(
            "example-production",
            "26.7.15",
            env_getter(FULL_ENV),
        )
        .expect("full env resolves");
        let rendered = render_manifests_with(&subs).expect("render succeeds");
        let gke = rendered.path().join(GKE_KUSTOMIZE_SUBPATH);

        let mut images = Vec::new();
        for entry in walkdir::WalkDir::new(rendered.path()) {
            let entry = entry.expect("walk rendered tree");
            if !entry.file_type().is_file() {
                continue;
            }
            let text = fs::read_to_string(entry.path()).expect("read rendered file");
            for line in text.lines() {
                let line = line.trim();
                if let Some(image) = line.strip_prefix("image: ") {
                    if image.contains("/navigator-") || image.contains("/neon-server:") {
                        images.push(image.to_string());
                    }
                }
            }
        }
        assert!(
            images
                .iter()
                .any(|image| image.contains("navigator-surreal-archive")),
            "the operational archive CronJob must be rendered: {images:?}"
        );
        for image in &images {
            assert!(
                image.ends_with(":26.7.15"),
                "`{image}` must be pinned to the rolled tag by the render itself"
            );
        }
        // Spot-check the file the incident traced to.
        let web_image = fs::read_to_string(gke.join("patches/web-image.yaml")).unwrap();
        assert!(web_image.contains("neon-server:26.7.15"));
        assert!(!web_image.contains(RELEASE_TAG_TOKEN));
    }

    #[test]
    fn embedded_workflows_manifest_retains_container_resources_and_rollout_contract() {
        let subs = resolve_substitutions_for_deployment(
            "example-production",
            "26.7.15",
            env_getter(FULL_ENV),
        )
        .expect("full env resolves");
        let rendered = render_manifests_with(&subs).expect("render succeeds");
        let manifests = kustomize_build(&rendered.path().join(GKE_KUSTOMIZE_SUBPATH))
            .expect("rendered GKE manifests build");
        let deployment = manifest_doc(&manifests, "Deployment", WORKFLOWS_DEPLOYMENT);
        let containers = deployment["spec"]["template"]["spec"]["containers"]
            .as_sequence()
            .expect("workflows-service containers");

        for (name, cpu, memory) in [("worker", "100m", "256Mi"), ("envoy", "50m", "128Mi")] {
            let container = containers
                .iter()
                .find(|container| container["name"].as_str() == Some(name))
                .unwrap_or_else(|| panic!("{name} container must be rendered"));
            assert_eq!(
                container["resources"]["requests"]["cpu"].as_str(),
                Some(cpu),
                "{name} CPU request survives embedded render"
            );
            assert_eq!(
                container["resources"]["requests"]["memory"].as_str(),
                Some(memory),
                "{name} memory request survives embedded render"
            );
        }
    }

    #[test]
    fn embedded_workloads_receive_pod_identity_for_telemetry() {
        let subs = resolve_substitutions_for_deployment(
            "example-production",
            "26.9.16",
            env_getter(FULL_ENV),
        )
        .expect("full env resolves");
        let rendered = render_manifests_with(&subs).expect("render succeeds");
        let manifests = kustomize_build(&rendered.path().join(GKE_KUSTOMIZE_SUBPATH))
            .expect("rendered GKE manifests build");

        for (deployment_name, container_name) in
            [(WEB_DEPLOYMENT, "web"), (WORKFLOWS_DEPLOYMENT, "worker")]
        {
            let deployment = manifest_doc(&manifests, "Deployment", deployment_name);
            let container = deployment["spec"]["template"]["spec"]["containers"]
                .as_sequence()
                .expect("deployment containers")
                .iter()
                .find(|container| container["name"].as_str() == Some(container_name))
                .unwrap_or_else(|| panic!("{deployment_name}/{container_name} container"));
            assert_eq!(
                container["env"]
                    .as_sequence()
                    .expect("container env")
                    .iter()
                    .find(|entry| entry["name"].as_str() == Some("POD_NAME"))
                    .and_then(|entry| entry["valueFrom"]["fieldRef"]["fieldPath"].as_str()),
                Some("metadata.name"),
                "{deployment_name} must use the downward API for pod identity"
            );
            assert_eq!(
                container["env"]
                    .as_sequence()
                    .expect("container env")
                    .iter()
                    .find(|entry| entry["name"].as_str() == Some("OTEL_RESOURCE_ATTRIBUTES"))
                    .and_then(|entry| entry["value"].as_str()),
                Some("k8s.pod.name=$(POD_NAME),service.instance.id=$(POD_NAME)"),
                "{deployment_name} must carry pod identity on the shared resource"
            );
        }
    }

    #[test]
    fn images_render_from_the_registry_while_everything_else_renders_the_environment() {
        // The two projects are different and both must land: every image line
        // points at the hub CI publishes to, while the buckets and the GSA
        // stay in the project being shipped into.
        // Rendering images from the environment project applies a manifest
        // whose tags do not exist — an outage on the tiers that delete the
        // running pod before the replacement is ready.
        let subs = resolve_substitutions_for_deployment(
            "example-staging-project",
            "26.7.28",
            env_getter(HUB_ENV),
        )
        .expect("hub env resolves");
        let rendered = render_manifests_with(&subs).expect("render succeeds");

        let mut images = Vec::new();
        let mut saw_environment_project = false;
        for entry in walkdir::WalkDir::new(rendered.path()) {
            let entry = entry.expect("walk rendered tree");
            if !entry.file_type().is_file() {
                continue;
            }
            let text = fs::read_to_string(entry.path()).expect("read rendered file");
            if text.contains("example-staging-project") {
                saw_environment_project = true;
            }
            for line in text.lines() {
                if let Some(image) = line.trim().strip_prefix("image: ") {
                    // Only our own images. A rendered tree also carries
                    // third-party ones (Restate, ClamAV), which no substitution
                    // touches and which must not be held to our registry.
                    // Only our own images, and only the ones a substitution
                    // actually resolved. A rendered tree also carries
                    // third-party images (Restate, ClamAV) and the KIND base's
                    // registry-less `navigator-web:dev`, neither of which the
                    // registry rule governs.
                    if image.contains('/')
                        && (image.contains("navigator-") || image.contains("neon-server"))
                    {
                        images.push(image.to_string());
                    }
                }
            }
        }

        assert!(
            images
                .iter()
                .any(|image| image.contains("navigator-surreal-archive")),
            "the operational archive CronJob must render from the registry: {images:?}"
        );
        for image in &images {
            assert!(
                image.starts_with(&format!("{TEST_REGISTRY}/")),
                "`{image}` must pull from the published registry"
            );
        }
        assert!(
            saw_environment_project,
            "the environment project must still render into buckets / the GSA"
        );
    }

    #[test]
    fn workflows_url_derives_from_primary_domain_when_unset() {
        // The 2026-06-10 ship symptom: a domain is configured but the
        // explicit URL is not. The resolved URL must be the real
        // ingress derived from the domain, never the
        // `workflows.example.com` placeholder.
        let cfg = ShipConfig {
            primary_domain: "example.invalid".into(),
            workflows_url: None,
            ..sample_config()
        };
        assert_eq!(
            cfg.workflows_url_resolved(),
            "https://workflows.example.invalid/"
        );
    }

    #[test]
    fn workflows_url_prefers_explicit_override() {
        let cfg = ShipConfig {
            workflows_url: Some("https://workflows.example.invalid/".into()),
            ..sample_config()
        };
        assert_eq!(
            cfg.workflows_url_resolved(),
            "https://workflows.example.invalid/"
        );
    }

    #[test]
    fn derived_context_matches_gke_get_credentials_naming() {
        assert_eq!(
            derived_context("my-org-prod", "example-region", "navigator"),
            "gke_my-org-prod_example-region_navigator"
        );
    }

    /// Shorthand for the expected parse: alternatives of conjunctions,
    /// no trigger.
    fn req(any_of: &[&[&str]]) -> SecretRequirement {
        SecretRequirement {
            any_of: any_of
                .iter()
                .map(|alt| alt.iter().map(ToString::to_string).collect())
                .collect(),
            trigger: None,
        }
    }

    #[test]
    fn secret_requirements_scrape_the_invariant_literals() {
        // Mirrors the real shape of web/src/config.rs invariant lines,
        // including a multi-line string and a prose false-positive.
        let src = r#"
            bail!(
                "RESTATE_BROKER_URL must be set (otherwise the in-memory \
                 broker would silently swallow jobs)"
            );
            ensure!(cfg.has("DOCUSIGN_HMAC_KEY"), "DOCUSIGN_HMAC_KEY must be set (otherwise forgeable)");
            // a comment explaining that something must be set should NOT match
            "SENDGRID_API_KEY must be set";
        "#;
        assert_eq!(
            secret_requirements(src),
            vec![
                req(&[&["DOCUSIGN_HMAC_KEY"]]),
                req(&[&["RESTATE_BROKER_URL"]]),
                req(&[&["SENDGRID_API_KEY"]]),
            ]
        );
    }

    #[test]
    fn disjunctive_invariants_parse_into_alternatives_of_conjunctions() {
        // The live DocuSign shape: `A or B + C + D must be set` — boots on
        // the short-lived access token alone, or on the full JWT triple
        // together. The chain must open the string literal; the same words in
        // prose (no quote before the first key) must not parse.
        let src = r#"
            "DOCUSIGN_ACCESS_TOKEN or DOCUSIGN_INTEGRATION_KEY + DOCUSIGN_USER_ID + DOCUSIGN_PRIVATE_KEY must be set \
             (prefer the JWT grant over the short-lived token)";
            // prose: DOCUSIGN_ACCESS_TOKEN or DOCUSIGN_INTEGRATION_KEY must be set somewhere
        "#;
        let parsed = secret_requirements(src);
        assert_eq!(
            parsed,
            vec![req(&[
                &["DOCUSIGN_ACCESS_TOKEN"],
                &[
                    "DOCUSIGN_INTEGRATION_KEY",
                    "DOCUSIGN_USER_ID",
                    "DOCUSIGN_PRIVATE_KEY"
                ],
            ])]
        );

        // Satisfied by the access-token alternative alone…
        let token: BTreeSet<String> = ["DOCUSIGN_ACCESS_TOKEN"]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(parsed[0].is_satisfied_by(&token));
        // …or by the full JWT triple…
        let jwt: BTreeSet<String> = [
            "DOCUSIGN_INTEGRATION_KEY",
            "DOCUSIGN_USER_ID",
            "DOCUSIGN_PRIVATE_KEY",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert!(parsed[0].is_satisfied_by(&jwt));
        // …but NOT by a partial JWT triple, which is the failure mode a
        // key-set preflight exists to catch.
        let partial: BTreeSet<String> = ["DOCUSIGN_INTEGRATION_KEY", "DOCUSIGN_USER_ID"]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(!parsed[0].is_satisfied_by(&partial));
        assert_eq!(
            unsatisfied_requirements(&parsed, &partial),
            parsed,
            "an incomplete alternative must leave the requirement unsatisfied"
        );
    }

    #[test]
    fn conditional_invariants_carry_their_trigger_and_gate_on_it() {
        // Mirrors the real shape: two conditional OIDC invariants nested
        // under an `if get("OIDC_JWKS_URL")` block, plus one unconditional.
        let src = r#"
            if get("OIDC_JWKS_URL").is_some_and(|s| !s.is_empty()) {
                violations.push(
                    "OIDC_AUDIENCE must be set when OIDC_JWKS_URL is (otherwise \
                     bearer tokens are accepted without audience pinning)"
                        .into(),
                );
                violations.push(
                    "OIDC_ISSUER must be set when OIDC_JWKS_URL is (otherwise the \
                     bearer token's issuer is unverified)"
                        .into(),
                );
            }
            "SENDGRID_API_KEY must be set (otherwise outbound email is dropped)";
        "#;
        let parsed = secret_requirements(src);
        assert_eq!(
            parsed,
            vec![
                SecretRequirement {
                    trigger: Some("OIDC_JWKS_URL".to_string()),
                    ..req(&[&["OIDC_AUDIENCE"]])
                },
                SecretRequirement {
                    trigger: Some("OIDC_JWKS_URL".to_string()),
                    ..req(&[&["OIDC_ISSUER"]])
                },
                req(&[&["SENDGRID_API_KEY"]]),
            ]
        );

        // OIDC_JWKS_URL not configured → the two conditional requirements
        // do NOT apply; only the unconditional one does. This is the prod
        // case where the optional JWKS bearer path is off.
        let without_jwks: BTreeSet<String> =
            ["SENDGRID_API_KEY"].into_iter().map(String::from).collect();
        assert_eq!(
            effective_requirements(&parsed, &without_jwks),
            vec![req(&[&["SENDGRID_API_KEY"]])]
        );
        assert!(unsatisfied_requirements(
            &effective_requirements(&parsed, &without_jwks),
            &without_jwks
        )
        .is_empty());

        // OIDC_JWKS_URL configured but its companions absent → both
        // conditional requirements now apply and are reported unsatisfied.
        let with_jwks: BTreeSet<String> = ["SENDGRID_API_KEY", "OIDC_JWKS_URL"]
            .into_iter()
            .map(String::from)
            .collect();
        let unsatisfied: Vec<String> =
            unsatisfied_requirements(&effective_requirements(&parsed, &with_jwks), &with_jwks)
                .iter()
                .map(SecretRequirement::describe)
                .collect();
        assert_eq!(
            unsatisfied,
            vec!["OIDC_AUDIENCE".to_string(), "OIDC_ISSUER".to_string()]
        );
    }

    #[test]
    fn ship_consumes_the_shared_web_invariant_keys() {
        let parsed = shared_web_requirements(store::deployment::DeploymentCapabilities::default());
        for expected in [
            "RESTATE_BROKER_URL",
            "SENDGRID_API_KEY",
            "DOCUSIGN_HMAC_KEY",
            "SESSION_SECRET",
        ] {
            assert!(
                parsed.iter().any(|requirement| requirement
                    .any_of
                    .iter()
                    .any(|alternative| alternative.iter().any(|key| key == expected))),
                "embedded config invariants missing {expected}; parsed: {parsed:?}"
            );
        }

        // Key presence alone is not enough: `ship` has to carry each
        // requirement's trigger across the parse too. If it dropped one, the
        // preflight would demand DocuSign credentials from a deployment that
        // declares none — the exact failure this gate exists to prevent, and
        // invisible to a check that only looks for the key.
        let docusign = parsed
            .iter()
            .find(|requirement| requirement.any_of == vec![vec!["DOCUSIGN_HMAC_KEY".to_string()]])
            .expect("DOCUSIGN_HMAC_KEY is a parsed requirement");
        assert_eq!(
            docusign.trigger.as_deref(),
            Some("DOCUSIGN_BASE_URL"),
            "the DocuSign trigger must survive the parse: {docusign:?}"
        );
    }

    #[test]
    fn ship_requires_restate_ingress_credentials_only_for_the_automation_home() {
        let ingress = req(&[&["RESTATE_INGRESS_URL", "RESTATE_AUTH_TOKEN"]]);
        assert!(
            !shared_web_requirements(store::deployment::DeploymentCapabilities::default())
                .contains(&ingress)
        );
        assert!(
            shared_web_requirements(store::deployment::DeploymentCapabilities {
                github_automation_home: true,
                ..Default::default()
            })
            .contains(&ingress)
        );
    }

    #[test]
    fn secret_requirements_dedupe() {
        let src = r#""SENDGRID_API_KEY must be set"; "SENDGRID_API_KEY must be set again";"#;
        assert_eq!(
            secret_requirements(src),
            vec![req(&[&["SENDGRID_API_KEY"]])]
        );
    }

    #[test]
    fn secret_requirements_collapse_conditional_and_unconditional_to_unconditional() {
        // A key required both conditionally (`… when X is`) and
        // unconditionally is ALWAYS required: the unconditional occurrence
        // wins and the merged requirement carries no trigger — regardless of
        // which occurrence the scraper meets first. Guards the `and_modify`
        // collapse in `secret_requirements`; a regression would gate an
        // always-required secret behind a trigger and let a missing key
        // slip past the preflight into a crash-loop.
        let want = vec![req(&[&["SENDGRID_API_KEY"]])];

        // Conditional first, then unconditional → trigger cleared.
        let cond_then_uncond = r#"
            "SENDGRID_API_KEY must be set when OIDC_JWKS_URL is (otherwise dropped)";
            "SENDGRID_API_KEY must be set (otherwise outbound email is dropped)";
        "#;
        assert_eq!(
            secret_requirements(cond_then_uncond),
            want,
            "unconditional occurrence must clear the earlier trigger"
        );

        // Unconditional first, then conditional → still unconditional.
        let uncond_then_cond = r#"
            "SENDGRID_API_KEY must be set (otherwise outbound email is dropped)";
            "SENDGRID_API_KEY must be set when OIDC_JWKS_URL is (otherwise dropped)";
        "#;
        assert_eq!(
            secret_requirements(uncond_then_cond),
            want,
            "a later conditional occurrence must not re-introduce a trigger"
        );
    }

    #[test]
    fn unsatisfied_requirements_reports_only_the_unsatisfied() {
        let required = vec![
            req(&[&["DOCUSIGN_HMAC_KEY"]]),
            req(&[&["SENDGRID_API_KEY"]]),
        ];
        let satisfied: BTreeSet<String> =
            ["SENDGRID_API_KEY"].into_iter().map(String::from).collect();
        assert_eq!(
            unsatisfied_requirements(&required, &satisfied),
            vec![req(&[&["DOCUSIGN_HMAC_KEY"]])]
        );
    }

    #[test]
    fn unsatisfied_requirements_empty_when_all_satisfied() {
        let required = vec![req(&[&["A"]]), req(&[&["B"]])];
        let satisfied: BTreeSet<String> = ["A", "B", "C"].into_iter().map(String::from).collect();
        assert!(unsatisfied_requirements(&required, &satisfied).is_empty());
    }

    #[test]
    fn describe_restates_the_invariant_phrasing() {
        let disjunction = req(&[&["A"], &["B", "C"]]);
        assert_eq!(disjunction.describe(), "A or B + C");
        assert_eq!(req(&[&["A"]]).describe(), "A");
    }

    /// A rendered manifest stream shaped like `kubectl kustomize` output:
    /// multiple documents, the web tier plus a non-web-binary workload.
    const RENDERED_MANIFESTS: &str = r"
apiVersion: v1
kind: Service
metadata:
  name: navigator-web
  namespace: navigator
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: navigator-web
  namespace: navigator
spec:
  template:
    spec:
      containers:
        - name: web
          env:
            - name: NAVIGATOR_BLANK
              value: ''
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: workflows-service
  namespace: navigator
spec:
  template:
    spec:
      containers:
        - name: worker
          env:
            - name: NAVIGATOR_WORKER_ONLY
              value: yes-please
";

    #[test]
    fn rendered_envs_read_the_web_binary_deployments_from_the_manifests() {
        let envs = parse_web_binary_envs(
            RENDERED_MANIFESTS,
            "navigator-web-secrets",
            &BTreeSet::new(),
        )
        .expect("parse rendered manifests");
        let names: Vec<&str> = envs.iter().map(|(name, _)| name.as_str()).collect();
        // `workflows-service` runs the worker binary, so it is not subject
        // to the web boot invariants and must not be checked against them.
        assert_eq!(names, vec!["navigator-web"]);

        let web = &envs[0].1;
        // An empty-valued env var is as unset as a missing one, exactly as
        // the pod's own `enforce_deployment_invariants` sees it.
        assert!(!web.contains("NAVIGATOR_BLANK"));
    }

    #[test]
    fn web_secret_envfrom_guard_rejects_a_web_pod_without_the_deployment_secret() {
        let error = ensure_web_secret_envfrom(RENDERED_MANIFESTS, "navigator-web-secrets")
            .expect_err("the fixture web pod does not envFrom the deployment Secret");
        assert!(
            error.to_string().contains("navigator-web-secrets"),
            "{error}"
        );
    }

    /// The Secret keys prod actually carries for the requirements the
    /// manifest fixture does not supply as env. Enough to satisfy every
    /// `WEB_REQUIREMENTS` entry that isn't manifest-provided.
    fn satisfying_secret_keys() -> BTreeSet<String> {
        store::deployment::WEB_REQUIREMENTS
            .iter()
            .filter_map(|r| r.any_of.first())
            .flat_map(|alt| alt.iter().map(ToString::to_string))
            .collect()
    }

    #[test]
    fn the_rendered_secret_catalog_and_web_envfrom_are_scoped_to_one_deployment() {
        // Every deployment shares this one embedded catalog, so each rendered
        // Secret Manager coordinate and the Secret read by `web` must come
        // from the selected deployment's own configuration.
        let subs = resolve_substitutions_for_deployment(
            "example-production",
            "26.7.29",
            env_getter(FULL_ENV),
        )
        .expect("full env resolves");
        let rendered = render_manifests_with(&subs).expect("render succeeds");
        let spc = fs::read_to_string(
            rendered
                .path()
                .join(GKE_KUSTOMIZE_SUBPATH)
                .join("secrets/secret-provider-class.yaml"),
        )
        .expect("read the rendered SecretProviderClass");

        // Every Secret Manager reference resolves in the deployment's project.
        assert!(
            spc.contains(
                "projects/example-production-project/secrets/SESSION_SECRET/versions/latest"
            ),
            "resourceName must name the deployment's own project"
        );
        assert!(
            !spc.contains("YOUR_PROJECT_ID"),
            "no unsubstituted project placeholder may survive the render"
        );

        assert!(
            !spc.contains("secretObjects"),
            "the SecretProviderClass is a Secret Manager catalog, not a Kubernetes Secret writer"
        );
        // The catalog still lands in the deployment's namespace, not the
        // shared default.
        assert!(spc.contains("namespace: example-production"));

        let manifests = kustomize_build(&rendered.path().join(GKE_KUSTOMIZE_SUBPATH))
            .expect("rendered GKE manifests build");
        let secret_name = FULL_ENV
            .iter()
            .find(|(key, _)| *key == "NAVIGATOR_WEB_SECRET_NAME")
            .map(|(_, value)| *value)
            .expect("full environment names the deployment Secret");
        ensure_web_secret_envfrom(&manifests, secret_name)
            .expect("the web container receives its deployment Secret through envFrom");
    }

    /// The container in `deployment` named `container`, from a rendered stream.
    fn rendered_container(manifests: &str, deployment: &str, container: &str) -> serde_json::Value {
        use serde::Deserialize;
        serde_yaml::Deserializer::from_str(manifests)
            .filter_map(|document| serde_json::Value::deserialize(document).ok())
            .filter(|value| {
                value.get("kind").and_then(serde_json::Value::as_str) == Some("Deployment")
                    && value
                        .pointer("/metadata/name")
                        .and_then(serde_json::Value::as_str)
                        == Some(deployment)
            })
            .find_map(|value| {
                value
                    .pointer("/spec/template/spec/containers")
                    .and_then(serde_json::Value::as_array)
                    .and_then(|containers| {
                        containers
                            .iter()
                            .find(|c| {
                                c.get("name").and_then(serde_json::Value::as_str) == Some(container)
                            })
                            .cloned()
                    })
            })
            .unwrap_or_else(|| panic!("the render carries {deployment}/{container}"))
    }

    #[test]
    fn web_and_worker_receive_the_deployment_secret_and_the_social_callback() {
        // Social account tokens are sealed by `web` and opened by the
        // `workflows-service` publisher, so both read the runtime KMS key and
        // the social app values from the same deployment Secret.
        let subs = resolve_substitutions_for_deployment(
            "example-production",
            "26.7.29",
            env_getter(FULL_ENV),
        )
        .expect("full env resolves");
        let rendered = render_manifests_with(&subs).expect("render succeeds");
        let manifests = kustomize_build(&rendered.path().join(GKE_KUSTOMIZE_SUBPATH))
            .expect("rendered GKE manifests build");
        for (deployment, container) in [(WEB_DEPLOYMENT, "web"), (WORKFLOWS_DEPLOYMENT, "worker")] {
            let container_spec = rendered_container(&manifests, deployment, container);
            let secrets: Vec<&str> = container_spec
                .get("envFrom")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|source| source.pointer("/secretRef/name"))
                .filter_map(serde_json::Value::as_str)
                .collect();
            assert!(
                secrets.contains(&"example-production-web-secrets"),
                "{deployment}/{container} must envFrom the deployment Secret: {secrets:?}"
            );
        }
        let web = rendered_container(&manifests, WEB_DEPLOYMENT, "web");
        let callback = web
            .get("env")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .find(|entry| {
                entry.get("name").and_then(serde_json::Value::as_str)
                    == Some("NAVIGATOR_SOCIAL_OAUTH_REDIRECT_URI")
            })
            .and_then(|entry| entry.get("value"))
            .and_then(serde_json::Value::as_str);
        assert_eq!(
            callback,
            Some("https://navigator.example.invalid/social/oauth/callback"),
            "the social callback derives from the deployment's own public host"
        );
    }

    const SOCIAL_OBJECTS: &[&str] = &[
        "NAVIGATOR_META_APP_ID",
        "NAVIGATOR_META_APP_SECRET",
        "NAVIGATOR_META_WEBHOOK_APP_SECRET",
        "NAVIGATOR_META_WEBHOOK_VERIFY_TOKEN",
        "NAVIGATOR_THREADS_APP_ID",
        "NAVIGATOR_THREADS_APP_SECRET",
        "NAVIGATOR_LINKEDIN_CLIENT_ID",
        "NAVIGATOR_LINKEDIN_CLIENT_SECRET",
        "NAVIGATOR_YOUTUBE_CLIENT_ID",
        "NAVIGATOR_YOUTUBE_CLIENT_SECRET",
        "NAVIGATOR_X_CLIENT_ID",
        "NAVIGATOR_X_CLIENT_SECRET",
        "NAVIGATOR_TIKTOK_BUSINESS_APP_ID",
        "NAVIGATOR_TIKTOK_BUSINESS_AUTHORIZATION_URL",
        "NAVIGATOR_TIKTOK_BUSINESS_APP_SECRET",
    ];

    #[test]
    fn two_deployments_render_disjoint_secret_manager_coordinates() {
        // Same manifest, two configs: nothing may be shared between them.
        let render_for = |env| {
            let subs = resolve_substitutions_for_deployment(
                "example-production",
                "26.7.29",
                env_getter(env),
            )
            .expect("env resolves");
            let rendered = render_manifests_with(&subs).expect("render succeeds");
            fs::read_to_string(
                rendered
                    .path()
                    .join(GKE_KUSTOMIZE_SUBPATH)
                    .join("secrets/secret-provider-class.yaml"),
            )
            .expect("read the rendered SecretProviderClass")
        };

        let neon = render_for(FULL_ENV);
        let staging = render_for(HUB_ENV);
        assert!(
            !staging.contains("example-production-project"),
            "one deployment's render must not carry another's project"
        );
        assert!(
            !neon.contains("example-staging-project-web-secrets"),
            "one deployment's render must not carry another's Secret name"
        );
    }

    #[test]
    fn spc_projected_keys_reads_the_secret_manager_catalog() {
        let drifted = r#"
apiVersion: secrets-store.csi.x-k8s.io/v1
kind: SecretProviderClass
spec:
  parameters:
    secrets: |
      - resourceName: "projects/x/secrets/SESSION_SECRET/versions/latest"
        path: "SESSION_SECRET"
"#;
        let projected = spc_projected_keys(drifted);
        assert_eq!(projected, BTreeSet::from(["SESSION_SECRET".to_string()]));
    }

    /// Every key the `SecretProviderClass` lists for Secret Manager.
    ///
    /// Stated as a closed set rather than a floor, so adding or removing a
    /// Secret Manager object stays a visible edit. Sorted, so that edit lands
    /// where a reader looks for it rather than wherever the last one stopped.
    const SECRET_MANAGER_CONTRACT: &[&str] = &[
        "DASH0_TOKEN",
        "DOCUSIGN_ACCOUNT_ID",
        "DOCUSIGN_BASE_URL",
        "DOCUSIGN_HMAC_KEY",
        "DOCUSIGN_INTEGRATION_KEY",
        "DOCUSIGN_OAUTH_BASE",
        "DOCUSIGN_PRIVATE_KEY",
        "DOCUSIGN_SIGNER_EMAIL",
        "DOCUSIGN_USER_ID",
        "DOCUSIGN_WEBHOOK_SECRET",
        "HCAPTCHA_SECRET",
        "NAVIGATOR_CREDENTIAL_ENVIRONMENT",
        "NAVIGATOR_ENVIRONMENT",
        "NAVIGATOR_FORGE_BACKEND",
        "NAVIGATOR_GITHUB_ORG",
        "NAVIGATOR_IDENTITY_PLATFORM_API_KEY",
        "NAVIGATOR_LINKEDIN_CLIENT_ID",
        "NAVIGATOR_LINKEDIN_CLIENT_SECRET",
        "NAVIGATOR_META_APP_ID",
        "NAVIGATOR_META_APP_SECRET",
        "NAVIGATOR_META_WEBHOOK_APP_SECRET",
        "NAVIGATOR_META_WEBHOOK_VERIFY_TOKEN",
        "NAVIGATOR_NOTION_DATABASE_ID",
        "NAVIGATOR_RUNTIME_KMS_KEY",
        "NAVIGATOR_SMS_TEST_RECIPIENTS",
        "NAVIGATOR_SURREAL_ARCHIVES_BUCKET",
        "NAVIGATOR_SURREAL_DATABASE",
        "NAVIGATOR_SURREAL_ENDPOINT",
        "NAVIGATOR_SURREAL_NAMESPACE",
        "NAVIGATOR_SURREAL_PASSWORD",
        "NAVIGATOR_SURREAL_USER",
        "NAVIGATOR_THREADS_APP_ID",
        "NAVIGATOR_THREADS_APP_SECRET",
        "NAVIGATOR_TIKTOK_BUSINESS_APP_ID",
        "NAVIGATOR_TIKTOK_BUSINESS_APP_SECRET",
        "NAVIGATOR_TIKTOK_BUSINESS_AUTHORIZATION_URL",
        "NAVIGATOR_X_CLIENT_ID",
        "NAVIGATOR_X_CLIENT_SECRET",
        "NAVIGATOR_YOUTUBE_CLIENT_ID",
        "NAVIGATOR_YOUTUBE_CLIENT_SECRET",
        "OAUTH_APPLE_KEY_ID",
        "OAUTH_APPLE_PRIVATE_KEY",
        "OAUTH_APPLE_TEAM_ID",
        "OAUTH_CLIENT_SECRET",
        "OAUTH_MICROSOFT_CLIENT_SECRET",
        "RESTATE_AUTH_TOKEN",
        "RESTATE_BROKER_URL",
        "RESTATE_INGRESS_URL",
        "SENDGRID_API_KEY",
        "SENDGRID_EVENTS_PUBLIC_KEY",
        "SENDGRID_EVENTS_SECRET",
        "SENDGRID_FROM_EMAIL",
        "SENDGRID_INBOUND_PUBLIC_KEY",
        "SENDGRID_INBOUND_SECRET",
        "SESSION_SECRET",
        "SLACK_BOT_TOKEN",
        "SLACK_WEBHOOK_URL",
        "TWILIO_ACCOUNT_SID",
        "TWILIO_AUTH_TOKEN",
    ];

    /// The class lists the Secret Manager contract exactly.
    ///
    /// The store reaches a pod through the six `NAVIGATOR_SURREAL_*` keys
    /// listed here, and a credential for any other engine would have to join
    /// them to be cataloged at all — which this equality reports as a
    /// difference rather than letting it ride along unnoticed.
    #[test]
    fn the_secret_provider_class_lists_exactly_the_secret_manager_contract() {
        let expected: BTreeSet<String> = SECRET_MANAGER_CONTRACT
            .iter()
            .map(ToString::to_string)
            .collect();
        let projected = secret_provider_class_keys();

        let unexpected: Vec<&String> = projected.difference(&expected).collect();
        let absent: Vec<&String> = expected.difference(&projected).collect();
        assert!(
            unexpected.is_empty() && absent.is_empty(),
            "the SecretProviderClass no longer lists the Secret Manager contract. Listed but not \
             contracted: {unexpected:?}. Contracted but not listed: {absent:?}. Every key here is \
             created in Secret Manager and handed to `web` through its Kubernetes Secret, so reconcile \
             examples/deploy/k8s/gke/secrets/secret-provider-class.yaml and SECRET_MANAGER_CONTRACT \
             together.",
        );
    }

    /// The contract stays sorted.
    ///
    /// The equality above compares `BTreeSet`s, so it reports a missing or
    /// extra key and says nothing about where a new one was written. Without
    /// this, a key appended next to an unrelated neighbour reads as noise in
    /// every later diff of a list whose whole purpose is to make an edit
    /// visible.
    #[test]
    fn the_secret_manager_contract_is_sorted() {
        let mut sorted = SECRET_MANAGER_CONTRACT.to_vec();
        sorted.sort_unstable();
        assert_eq!(
            SECRET_MANAGER_CONTRACT,
            &sorted[..],
            "SECRET_MANAGER_CONTRACT is out of order; insert the key where it belongs",
        );
    }

    /// Every boot-required Secret key must be written by `ops secrets apply`.
    #[test]
    fn secret_manager_catalog_lists_every_boot_required_secret_key() {
        let satisfied: BTreeSet<String> = secret_provider_class_keys()
            .into_iter()
            .chain(INLINE_ENV_WEB_KEYS.iter().map(ToString::to_string))
            .collect();
        let required = shared_web_requirements(store::deployment::DeploymentCapabilities {
            github_automation_home: true,
            ..Default::default()
        });
        let effective = effective_requirements(&required, &satisfied);
        let missing = unsatisfied_requirements(&effective, &satisfied);
        assert!(
            missing.is_empty(),
            "Secret Manager catalog is missing boot-required keys: {}. \
             Declare them in examples/deploy/k8s/gke/secrets/secret-provider-class.yaml.",
            missing
                .iter()
                .map(SecretRequirement::describe)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    /// A rendered stream carrying one `SecretProviderClass` that references
    /// two objects, plus an unrelated document the scan must ignore.
    const RENDERED_SPC_STREAM: &str = r#"
apiVersion: v1
kind: Namespace
metadata:
  name: navigator
---
apiVersion: secrets-store.csi.x-k8s.io/v1
kind: SecretProviderClass
metadata:
  name: navigator-web
spec:
  provider: gke
  parameters:
    secrets: |
      - resourceName: "projects/example-project/secrets/SESSION_SECRET/versions/latest"
        path: "SESSION_SECRET"
      - resourceName: "projects/example-project/secrets/RESTATE_AUTH_TOKEN/versions/latest"
        path: "RESTATE_AUTH_TOKEN"
"#;

    fn object(secret_id: &str) -> ProjectedObject {
        ProjectedObject {
            project_id: "example-project".into(),
            secret_id: secret_id.into(),
            version: "latest".into(),
        }
    }

    #[test]
    fn referenced_objects_come_from_the_built_stream_with_their_full_coordinate() {
        let referenced = referenced_secret_manager_objects(RENDERED_SPC_STREAM)
            .expect("the rendered stream parses");
        assert_eq!(
            referenced.into_iter().collect::<Vec<_>>(),
            vec![object("RESTATE_AUTH_TOKEN"), object("SESSION_SECRET")]
        );
    }

    #[test]
    fn a_stream_with_no_secret_provider_class_references_nothing() {
        // Before the catalog is wired into the kustomization the class is
        // never applied, so there is nothing to resolve and the preflight must
        // not invent work — or a pre-activation ship would demand objects the
        // deployment has no reason to hold yet.
        let referenced = referenced_secret_manager_objects(RENDERED_MANIFESTS)
            .expect("the rendered stream parses");
        assert!(referenced.is_empty());
    }

    #[test]
    fn a_malformed_resource_name_aborts_rather_than_checking_nothing() {
        // A half-substituted or hand-edited reference would resolve to no
        // object at all. Skipping it silently is the one outcome that turns
        // this preflight into decoration.
        let broken = RENDERED_SPC_STREAM.replace(
            "projects/example-project/secrets/RESTATE_AUTH_TOKEN/versions/latest",
            "secrets/RESTATE_AUTH_TOKEN",
        );
        let error = referenced_secret_manager_objects(&broken)
            .expect_err("a malformed resourceName must abort");
        assert!(
            error.to_string().contains("secrets/RESTATE_AUTH_TOKEN"),
            "{error}"
        );
    }

    #[test]
    fn every_referenced_object_resolving_lets_the_ship_proceed() {
        let states = vec![
            (object("SESSION_SECRET"), Some("ENABLED".to_string())),
            (object("RESTATE_AUTH_TOKEN"), Some("ENABLED".to_string())),
        ];
        check_projected_objects_resolve(&states)
            .expect("a project holding every referenced object must let the ship proceed");
    }

    #[test]
    fn a_missing_object_aborts_the_ship_naming_it_and_the_count() {
        // The ENG-62 blocker as a unit test: the manifest referenced 29
        // objects while the project held 28, and the gap only surfaced as a
        // crash-looping pod. Both counts belong in the message, because the
        // mismatch is the finding.
        let states = vec![
            (object("SESSION_SECRET"), Some("ENABLED".to_string())),
            (object("DOCUSIGN_ACCESS_TOKEN"), None),
        ];
        let error = check_projected_objects_resolve(&states)
            .expect_err("a missing object must abort the ship");
        let message = error.to_string();
        assert!(message.contains("DOCUSIGN_ACCESS_TOKEN"), "{message}");
        assert!(message.contains("no such object"), "{message}");
        assert!(message.contains("references 2"), "{message}");
        assert!(message.contains("only 1"), "{message}");
        assert!(message.contains("example-project"), "{message}");
        assert!(message.contains("Nothing was applied"), "{message}");
    }

    #[test]
    fn a_disabled_version_aborts_as_firmly_as_a_missing_one() {
        // A disabled version answers the metadata read, so a check that only
        // looked for existence would pass while the catalog contains a version
        // `ops secrets apply` cannot use.
        let states = vec![(object("SESSION_SECRET"), Some("DISABLED".to_string()))];
        let error = check_projected_objects_resolve(&states)
            .expect_err("a disabled version cannot satisfy the catalog");
        assert!(error.to_string().contains("SESSION_SECRET (DISABLED)"));
    }

    #[test]
    fn the_built_overlay_wires_web_to_its_deployment_secret() {
        // The Secret Manager catalog and `web`'s Secret source are both
        // properties of the built stream, not isolated source files.
        let subs = resolve_substitutions_for_deployment(
            "example-staging-project",
            "26.7.19.20",
            env_getter(HUB_ENV),
        )
        .expect("full environment resolves");
        let rendered = render_manifests_with(&subs).expect("render succeeds");
        let manifests = kustomize_build(&rendered.path().join(GKE_KUSTOMIZE_SUBPATH))
            .expect("rendered GKE manifests build");

        let referenced =
            referenced_secret_manager_objects(&manifests).expect("the built stream parses");
        assert!(
            !referenced.is_empty(),
            "the SecretProviderClass must be in the built overlay"
        );
        for object in &referenced {
            assert_eq!(object.project_id, "example-staging-project");
            assert_eq!(object.version, "latest");
        }

        // Every referenced object is in the catalog that `ops secrets apply`
        // writes to both Secret Manager and the Kubernetes Secret.
        let referenced_ids: BTreeSet<String> = referenced
            .iter()
            .map(|object| object.secret_id.clone())
            .collect();
        assert_eq!(referenced_ids, secret_provider_class_keys());

        let web = manifest_doc(&manifests, "Deployment", "navigator-web");
        let containers = web
            .get("spec")
            .and_then(|spec| spec.get("template"))
            .and_then(|template| template.get("spec"))
            .and_then(|spec| spec.get("containers"))
            .and_then(serde_yaml::Value::as_sequence)
            .expect("the web Deployment declares containers");
        assert!(
            containers.iter().any(|container| {
                container.get("name").and_then(serde_yaml::Value::as_str) == Some("web")
                    && container
                        .get("envFrom")
                        .and_then(serde_yaml::Value::as_sequence)
                        .is_some_and(|sources| {
                            sources.iter().any(|source| {
                                source
                                    .get("secretRef")
                                    .and_then(|secret| secret.get("name"))
                                    .and_then(serde_yaml::Value::as_str)
                                    == HUB_ENV
                                        .iter()
                                        .find(|(key, _)| *key == "NAVIGATOR_WEB_SECRET_NAME")
                                        .map(|(_, value)| *value)
                            })
                        })
            }),
            "the web pod must receive the deployment Secret through envFrom"
        );
    }

    #[test]
    fn check_secret_invariants_passes_when_the_secret_satisfies_every_requirement() {
        // The abort-or-proceed decision itself, against a manifest stream —
        // no cluster. This is the `==> Secret invariants OK` path that lets
        // a ship reach the apply.
        check_secret_invariants(
            &sample_config(),
            RENDERED_MANIFESTS,
            &satisfying_secret_keys(),
        )
        .expect("a Secret carrying every requirement must let the ship proceed");
    }

    #[test]
    fn check_secret_invariants_aborts_naming_the_missing_keys() {
        // The 26.7.15 failure, reproduced as a unit test: a Secret missing
        // the Project-repo org must abort the ship BEFORE the reconcile, and
        // say which keys and which deployments.
        let mut keys = satisfying_secret_keys();
        keys.remove("NAVIGATOR_GITHUB_ORG");
        let err = check_secret_invariants(&sample_config(), RENDERED_MANIFESTS, &keys)
            .expect_err("a Secret missing a boot requirement must abort the ship");
        let message = err.to_string();
        assert!(message.contains("navigator-web"), "{message}");
        // And the remedy is one paste-able patch, not one key.
        assert!(
            message.contains(r#""NAVIGATOR_GITHUB_ORG":"<value>""#),
            "{message}"
        );
    }

    #[test]
    fn check_secret_invariants_fails_closed_on_unparseable_manifests() {
        // If the manifest stream can't be parsed we must NOT sail past the
        // preflight having checked nothing — finding zero deployments would
        // otherwise read as "no missing keys" and wave a bad ship through.
        let err = check_secret_invariants(
            &sample_config(),
            "kind: Deployment\n  bad: [indent",
            &satisfying_secret_keys(),
        )
        .expect_err("unparseable manifests must abort the ship, not pass it");
        assert!(
            err.to_string().contains("rendered manifest stream"),
            "{err}"
        );
    }

    #[test]
    fn check_secret_invariants_ignores_a_requirement_whose_trigger_is_absent() {
        // `OIDC_AUDIENCE` is required only when `OIDC_JWKS_URL` is
        // configured. With no JWKS url anywhere, demanding the audience
        // would be a false positive that stops a ship for nothing — the
        // pod's own invariant skips it too.
        let mut keys = satisfying_secret_keys();
        keys.remove("OIDC_JWKS_URL");
        keys.remove("OIDC_AUDIENCE");
        keys.remove("OIDC_ISSUER");
        check_secret_invariants(&sample_config(), RENDERED_MANIFESTS, &keys)
            .expect("an untriggered conditional requirement must not block the ship");
    }

    #[test]
    fn rendered_gke_web_deployment_supplies_the_clamd_address() {
        let subs = resolve_substitutions_for_deployment(
            "example-production",
            "26.7.19.20",
            env_getter(FULL_ENV),
        )
        .expect("full environment resolves");
        let rendered = render_manifests_with(&subs).expect("render succeeds");
        let manifests = kustomize_build(&rendered.path().join(GKE_KUSTOMIZE_SUBPATH))
            .expect("rendered GKE manifests build");
        let deployment_envs = parse_web_binary_envs(
            &manifests,
            "example-production-web-secrets",
            &BTreeSet::new(),
        )
        .expect("rendered manifest stream parses");
        let missing = missing_requirements_by_deployment(
            &[req(&[&["NAVIGATOR_CLAMD_ADDR"]])],
            &BTreeSet::new(),
            &deployment_envs,
        );

        assert!(
            missing.is_empty(),
            "every web-binary deployment needs the ClamAV address: {missing:?}"
        );
    }

    #[test]
    fn render_pins_a_same_day_h_tag() {
        // `YY.M.D` is the substitution TOKEN, not a shape constraint: a
        // legacy ad-hoc same-day release carries a fourth `.H` group. The render must pin it
        // verbatim rather than assume three components.
        let subs = resolve_substitutions_for_deployment(
            "example-production",
            "26.7.15.4",
            env_getter(FULL_ENV),
        )
        .expect("full env resolves");
        let rendered = render_manifests_with(&subs).expect("render succeeds");
        let web_image = fs::read_to_string(
            rendered
                .path()
                .join(GKE_KUSTOMIZE_SUBPATH)
                .join("patches/web-image.yaml"),
        )
        .unwrap();
        assert!(web_image.contains("neon-server:26.7.15.4"), "{web_image}");
        assert!(!web_image.contains(RELEASE_TAG_TOKEN));
    }

    #[test]
    fn render_pins_a_hotfix_tag_verbatim() {
        let subs = resolve_substitutions_for_deployment(
            "example-production",
            "26.8.19-hotfix.14",
            env_getter(FULL_ENV),
        )
        .expect("full env resolves");
        let rendered = render_manifests_with(&subs).expect("render succeeds");
        let web_image = fs::read_to_string(
            rendered
                .path()
                .join(GKE_KUSTOMIZE_SUBPATH)
                .join("patches/web-image.yaml"),
        )
        .unwrap();
        assert!(
            web_image.contains("neon-server:26.8.19-hotfix.14"),
            "{web_image}"
        );
        assert!(!web_image.contains(RELEASE_TAG_TOKEN));
    }

    #[test]
    fn substitutions_do_not_corrupt_one_another() {
        // `apply_substitutions` is a fold of plain string replacements, so
        // its correctness rests on the tokens being non-overlapping and no
        // substituted VALUE containing another token. Guard that here
        // rather than in the comment that currently asserts it.
        let subs = resolve_substitutions_for_deployment(
            "example-production",
            "26.7.15",
            env_getter(FULL_ENV),
        )
        .expect("full env resolves");
        for sub in &subs {
            for other in &subs {
                assert!(
                    !sub.value.contains(other.token),
                    "value of `{}` contains token `{}` — the fold order would matter",
                    sub.token,
                    other.token
                );
            }
        }
        let tokens: Vec<&str> = subs.iter().map(|s| s.token).collect();
        for (i, token) in tokens.iter().enumerate() {
            for (j, other) in tokens.iter().enumerate() {
                assert!(
                    i == j || !token.contains(other),
                    "token `{token}` contains `{other}` — replacement order would matter"
                );
            }
        }
    }

    #[test]
    fn suggested_patch_keys_degrade_to_a_valid_command() {
        // A requirement with no alternatives names no key. The command
        // shape must stay valid rather than emit an empty patch object.
        let by_deployment = vec![("navigator-web".to_string(), vec![req(&[])])];
        assert_eq!(suggested_patch_keys(&by_deployment), vec!["KEY"]);
        assert_eq!(
            secret_patch_stringdata(&suggested_patch_keys(&by_deployment)),
            r#"{"stringData":{"KEY":"<value>"}}"#
        );
    }

    #[test]
    fn rendered_envs_reject_unparseable_manifests() {
        // `kubectl kustomize` emitting something we can't parse must fail
        // the ship, not silently yield zero deployments and "pass" the
        // preflight by finding nothing to check.
        let err = parse_web_binary_envs("kind: Deployment\n  bad: [indent", "s", &BTreeSet::new())
            .expect_err("unparseable manifests must not pass the preflight");
        assert!(
            err.to_string().contains("rendered manifest stream"),
            "{err}"
        );
    }

    #[test]
    fn rendered_envs_skip_a_deployment_with_no_name() {
        // A nameless Deployment can't be matched to the web tier; skip it
        // rather than panic, and still fail closed on finding no web tier.
        let err = parse_web_binary_envs(
            "apiVersion: apps/v1\nkind: Deployment\nspec: {}\n",
            "s",
            &BTreeSet::new(),
        )
        .expect_err("a nameless Deployment is not a web tier");
        assert!(err.to_string().contains("cannot be boot-checked"), "{err}");
    }

    #[test]
    fn rendered_envs_reject_a_tree_with_no_web_tier() {
        // A tree that renders no web-binary Deployment cannot be
        // boot-checked. Failing closed beats shipping an unchecked web tier.
        let err = parse_web_binary_envs(
            "apiVersion: v1\nkind: Service\nmetadata:\n  name: navigator-web\n",
            "navigator-web-secrets",
            &BTreeSet::new(),
        )
        .expect_err("a tree with no web-binary Deployment must not pass the preflight");
        assert!(err.to_string().contains("cannot be boot-checked"), "{err}");
    }

    #[test]
    fn suggested_patch_keys_cover_every_missing_requirement_once() {
        // A deployment missing three requirements out of the one shared
        // Secret, one of them a multi-key bundle. The operator must get
        // every key in a single patch — a one-key example sends them round
        // the ship loop once per key.
        let missing = vec![
            req(&[&["SENDGRID_FROM_EMAIL"]]),
            req(&[&[
                "NAVIGATOR_GITHUB_ORG",
                "NAVIGATOR_GITHUB_APP_ID",
                "NAVIGATOR_GITHUB_APP_PRIVATE_KEY",
            ]]),
            req(&[&["NAVIGATOR_CREDENTIAL_ENVIRONMENT"]]),
        ];
        let by_deployment = vec![("navigator-web".to_string(), missing)];
        assert_eq!(
            suggested_patch_keys(&by_deployment),
            vec![
                "SENDGRID_FROM_EMAIL",
                "NAVIGATOR_GITHUB_ORG",
                "NAVIGATOR_GITHUB_APP_ID",
                "NAVIGATOR_GITHUB_APP_PRIVATE_KEY",
                "NAVIGATOR_CREDENTIAL_ENVIRONMENT",
            ]
        );
    }

    #[test]
    fn suggested_patch_keys_take_the_first_alternative() {
        // `A or B + C` — suggest the single-key `A`, not a mix of both
        // alternatives, which would over-ask for keys the deployer may
        // not hold.
        let by_deployment = vec![(
            "navigator-web".to_string(),
            vec![req(&[&["DOCUSIGN_ACCESS_TOKEN"], &["DOCUSIGN_USER_ID"]])],
        )];
        assert_eq!(
            suggested_patch_keys(&by_deployment),
            vec!["DOCUSIGN_ACCESS_TOKEN"]
        );
    }

    #[test]
    fn unsatisfied_secret_error_names_every_missing_key_in_one_patch() {
        // A deployment missing three requirements. The operator must be
        // able to copy ONE command and satisfy all of them.
        let missing = vec![
            req(&[&["SENDGRID_FROM_EMAIL"]]),
            req(&[&[
                "NAVIGATOR_GITHUB_ORG",
                "NAVIGATOR_GITHUB_APP_ID",
                "NAVIGATOR_GITHUB_APP_PRIVATE_KEY",
            ]]),
            req(&[&["NAVIGATOR_CREDENTIAL_ENVIRONMENT"]]),
        ];
        let message =
            unsatisfied_secret_error(&sample_config(), &[("navigator-web".to_string(), missing)]);
        // The diagnosis still names the deployment and the `A + B`
        // phrasing the invariant uses.
        assert!(message.contains("navigator-web: SENDGRID_FROM_EMAIL"));
        assert!(message.contains(
            "NAVIGATOR_GITHUB_ORG + NAVIGATOR_GITHUB_APP_ID + NAVIGATOR_GITHUB_APP_PRIVATE_KEY"
        ));
        // The remedy carries all five keys, not just the first.
        assert!(message.contains(
            r#"-p '{"stringData":{"SENDGRID_FROM_EMAIL":"<value>","NAVIGATOR_GITHUB_ORG":"<value>","NAVIGATOR_GITHUB_APP_ID":"<value>","NAVIGATOR_GITHUB_APP_PRIVATE_KEY":"<value>","NAVIGATOR_CREDENTIAL_ENVIRONMENT":"<value>"}}'"#
        ), "patch must carry every missing key; got:\n{message}");
        assert!(message.contains(
            "kubectl --context gke_my-org-prod_example-region_navigator -n navigator \
             patch secret navigator-web-secrets --type=merge"
        ));
    }

    #[test]
    fn secret_patch_stringdata_is_a_valid_merge_patch() {
        let body = secret_patch_stringdata(&["A".to_string(), "B".to_string()]);
        assert_eq!(body, r#"{"stringData":{"A":"<value>","B":"<value>"}}"#);
        // Paste-ability is the whole point: it has to parse as the JSON
        // `kubectl patch --type=merge` will read.
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(parsed["stringData"]["B"], "<value>");
    }

    #[test]
    fn empty_secret_values_do_not_satisfy() {
        // An empty-valued key crash-loops the pod exactly like a missing
        // one (`enforce_deployment_invariants` treats empty as unset), so it
        // must not enter the satisfied set: the integration key present and
        // the private key empty must leave the DocuSign JWT requirement
        // unsatisfied.
        let integration_key = base64::engine::general_purpose::STANDARD.encode("integration-key");
        let secret = serde_json::json!({
            "data": {
                "DOCUSIGN_INTEGRATION_KEY": integration_key,
                "DOCUSIGN_PRIVATE_KEY": "",
            }
        });
        let satisfied = populated_secret_keys(&secret);
        assert!(satisfied.contains("DOCUSIGN_INTEGRATION_KEY"));
        assert!(!satisfied.contains("DOCUSIGN_PRIVATE_KEY"));

        let jwt_grant = req(&[
            &["DOCUSIGN_ACCESS_TOKEN"],
            &["DOCUSIGN_INTEGRATION_KEY", "DOCUSIGN_PRIVATE_KEY"],
        ]);
        assert!(!jwt_grant.is_satisfied_by(&satisfied));
    }

    #[test]
    fn env_names_count_only_usable_declarations() {
        // A non-empty literal and a valueFrom reference the preflight
        // can't inspect (a different Secret "s") satisfy; a name-only or
        // empty-literal declaration resolves to an empty string in the
        // pod, which the boot invariant treats as unset.
        let deployment = serde_json::json!({
            "spec": { "template": { "spec": { "containers": [{
                "env": [
                    { "name": "EMPTY_LITERAL", "value": "" },
                    { "name": "NAME_ONLY" },
                    { "name": "FROM_SECRET",
                      "valueFrom": { "secretKeyRef": { "name": "s", "key": "k" } } },
                ]
            }] } } }
        });
        // The shipped Secret is a different one, so the secretKeyRef to
        // "s" stays optimistic and counts.
        let names = populated_env_names(&deployment, "navigator-web-secrets", &BTreeSet::new());
        let expected: BTreeSet<String> = ["FROM_SECRET"].into_iter().map(String::from).collect();
        assert_eq!(names, expected);
    }

    #[test]
    fn secret_ref_into_the_shipped_secret_resolves_against_its_values() {
        // A `secretKeyRef` into the very Secret `ship` inspects is
        // resolvable, so an empty referenced value must NOT count as
        // usable — otherwise this env path re-adds a key that
        // `populated_secret_keys` correctly dropped, passing the
        // preflight while the pod crash-loops. A ref to a *different*
        // Secret can't be inspected and stays optimistic.
        let deployment = serde_json::json!({
            "spec": { "template": { "spec": { "containers": [{
                "env": [
                    // Populated key in the shipped Secret → usable.
                    { "name": "DOCUSIGN_INTEGRATION_KEY",
                      "valueFrom": { "secretKeyRef": {
                          "name": "navigator-web-secrets", "key": "DOCUSIGN_INTEGRATION_KEY" } } },
                    // Empty key in the shipped Secret → NOT usable.
                    { "name": "DOCUSIGN_PRIVATE_KEY",
                      "valueFrom": { "secretKeyRef": {
                          "name": "navigator-web-secrets", "key": "DOCUSIGN_PRIVATE_KEY" } } },
                    // Ref into a different Secret → uninspectable, optimistic.
                    { "name": "FROM_OTHER",
                      "valueFrom": { "secretKeyRef": { "name": "other", "key": "k" } } },
                ]
            }] } } }
        });
        // The shipped Secret has the integration key populated but the
        // private key empty (only the populated key is in the set
        // `populated_secret_keys` returns).
        let secret_keys: BTreeSet<String> = ["DOCUSIGN_INTEGRATION_KEY"]
            .into_iter()
            .map(String::from)
            .collect();
        let names = populated_env_names(&deployment, "navigator-web-secrets", &secret_keys);
        let expected: BTreeSet<String> = ["DOCUSIGN_INTEGRATION_KEY", "FROM_OTHER"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(
            names, expected,
            "empty secretKeyRef into the shipped Secret must not be counted usable"
        );

        // End-to-end: the JWT requirement stays unsatisfied when the
        // private key is wired via an empty secretKeyRef into the shipped
        // Secret — a preflight-passing crash-loop otherwise.
        let jwt_grant = req(&[
            &["DOCUSIGN_ACCESS_TOKEN"],
            &["DOCUSIGN_INTEGRATION_KEY", "DOCUSIGN_PRIVATE_KEY"],
        ]);
        let satisfied: BTreeSet<String> = secret_keys.union(&names).cloned().collect();
        assert!(!jwt_grant.is_satisfied_by(&satisfied));
    }

    #[test]
    fn web_signing_iam_binding_grants_token_creator_on_the_web_gsa_itself() {
        // Regression guard for the document-download 500. The web pod signs
        // GCS URLs via IAM `signBlob`, which needs the web GSA to be a
        // serviceAccountTokenCreator on ITSELF — the binding must target the
        // GSA's own resource and name the same GSA as the member.
        let gsa = "navigator-web@my-org-prod.iam.gserviceaccount.com";
        let args = web_signing_iam_binding_args("my-org-prod", "navigator-web");

        assert_eq!(
            args[..3],
            ["iam", "service-accounts", "add-iam-policy-binding"],
            "must bind on the service-account resource, not the project policy"
        );
        assert!(
            args.contains(&gsa.to_string()),
            "the binding must target the web GSA's own resource: {args:?}"
        );
        assert!(
            args.contains(&"roles/iam.serviceAccountTokenCreator".to_string()),
            "signBlob requires the token-creator role: {args:?}"
        );
        assert!(
            args.contains(&format!("--member=serviceAccount:{gsa}")),
            "the GSA must be a token-creator on itself: {args:?}"
        );
        assert!(
            args.contains(&"--project=my-org-prod".to_string()),
            "the binding must be scoped to the deploy project: {args:?}"
        );
    }

    #[test]
    fn web_gsa_email_is_the_kind_bound_workload_identity_principal() {
        assert_eq!(
            web_gsa_email("my-org-prod", "navigator-web"),
            "navigator-web@my-org-prod.iam.gserviceaccount.com"
        );
    }

    #[test]
    fn dry_run_still_reads_the_policy() {
        // The regression this step exists to prevent: a dry-run that skipped
        // the read reported `ship complete` for a roll that could not clear
        // step 1c. The read is not conditioned on the mode, so a dry-run must
        // call it — and must fail when it is denied, exactly as a live roll
        // would, rather than printing a line and returning Ok.
        let mut read_calls = 0;
        let bound = policy_with(
            "roles/iam.serviceAccountTokenCreator",
            &format!(
                "serviceAccount:{}",
                web_gsa_email("my-org-prod", "navigator-web")
            ),
        );
        ensure_web_signing_iam_with(&sample_config(), true, SigningIamAuthority::Write, || {
            read_calls += 1;
            Ok(bound)
        })
        .expect("a bound policy clears the step");
        assert_eq!(read_calls, 1, "the dry-run must perform the read");
    }

    #[test]
    fn dry_run_fails_when_the_policy_cannot_be_read() {
        // The other half of the same property: a denied read is the answer to
        // "would the real roll work", so it must sink the dry-run too.
        let err =
            ensure_web_signing_iam_with(&sample_config(), true, SigningIamAuthority::Write, || {
                Err(signing_iam_read_failed(
                    "gsa@example.com",
                    "PERMISSION_DENIED",
                ))
            })
            .expect_err("a denied read must fail the dry-run");

        assert!(err.to_string().contains("cannot verify"), "{err}");
    }

    #[test]
    fn dry_run_prints_the_write_instead_of_performing_it() {
        // The write is the only half a dry-run declines: with the binding
        // absent it reports what the live roll would do and returns Ok rather
        // than shelling out to gcloud.
        let empty = serde_json::json!({ "etag": "BwYb0000000=" });
        ensure_web_signing_iam_with(&sample_config(), true, SigningIamAuthority::Write, || {
            Ok(empty)
        })
        .expect("dry-run must not attempt the IAM write");
    }

    /// A `get-iam-policy` document in the shape gcloud returns, granting
    /// `role` to `member`.
    fn policy_with(role: &str, member: &str) -> serde_json::Value {
        serde_json::json!({
            "bindings": [
                { "role": "roles/iam.workloadIdentityUser",
                  "members": ["serviceAccount:example-staging-project.svc.id.goog[navigator/navigator-web]"] },
                { "role": role, "members": [member] },
            ],
            "etag": "BwYb0000000=",
            "version": 1,
        })
    }

    #[test]
    fn web_signing_iam_read_needs_only_get_iam_policy() {
        // The steady-state call. It must be a READ on the GSA's own resource:
        // that is what drops the operator credential from setIamPolicy to
        // getIamPolicy on a row that is already bound.
        let args =
            web_signing_iam_read_args("example-staging-project", "example-staging-project-web");

        assert_eq!(
            args[..3],
            ["iam", "service-accounts", "get-iam-policy"],
            "the verify half must read, not write: {args:?}"
        );
        assert!(
            args.contains(&web_gsa_email(
                "example-staging-project",
                "example-staging-project-web"
            )),
            "the read must target the web GSA's own resource: {args:?}"
        );
        assert!(
            args.contains(&"--project=example-staging-project".to_string()),
            "the read must be scoped to the deploy project: {args:?}"
        );
        assert!(
            args.contains(&"--format=json".to_string()),
            "the verdict is decided by parsing the policy, so it must be JSON: {args:?}"
        );
    }

    #[test]
    fn a_present_self_binding_needs_no_write() {
        // The steady state on a provisioned row: `ops gcp setup` already wrote
        // this binding, so the roll must recognise it and skip the setIamPolicy.
        let gsa = "service-account@example.invalid";
        let policy = policy_with(
            "roles/iam.serviceAccountTokenCreator",
            &format!("serviceAccount:{gsa}"),
        );

        assert!(
            policy_grants_self_signing(&policy, gsa),
            "an unconditional self-binding is the no-write case"
        );
    }

    #[test]
    fn a_missing_self_binding_is_written() {
        // Three ways the grant can be absent, each of which must fall through
        // to the binding write rather than pass the verify.
        let gsa = "service-account@example.invalid";

        let empty = serde_json::json!({ "etag": "BwYb0000000=" });
        assert!(
            !policy_grants_self_signing(&empty, gsa),
            "a policy with no bindings at all grants nothing"
        );

        let other_member = policy_with(
            "roles/iam.serviceAccountTokenCreator",
            "serviceAccount:other@example.invalid",
        );
        assert!(
            !policy_grants_self_signing(&other_member, gsa),
            "token-creator held by another principal does not let the pod sign for itself"
        );

        let other_role = policy_with(
            "roles/iam.serviceAccountUser",
            &format!("serviceAccount:{gsa}"),
        );
        assert!(
            !policy_grants_self_signing(&other_role, gsa),
            "only serviceAccountTokenCreator carries signBlob"
        );
    }

    #[test]
    fn a_conditional_self_binding_does_not_satisfy_the_invariant() {
        // The pod signs on every download, at an hour no condition here can be
        // read to cover, so a conditional grant is treated as absent.
        let gsa = "service-account@example.invalid";
        let policy = serde_json::json!({
            "bindings": [{
                "role": "roles/iam.serviceAccountTokenCreator",
                "members": [format!("serviceAccount:{gsa}")],
                "condition": {
                    "title": "expires",
                    "expression": "request.time < timestamp(\"2030-01-01T00:00:00Z\")",
                },
            }],
        });

        assert!(
            !policy_grants_self_signing(&policy, gsa),
            "a conditional grant is not the unconditional invariant the roll asserts"
        );
    }

    #[test]
    fn assert_only_refuses_an_absent_binding_instead_of_writing() {
        // ENG-311. The residual this flag exists for: a LIVE roll (dry_run
        // false) whose binding is absent must stop at the verify rather than
        // reach for setIamPolicy. Nothing here is mocked past the read, so a
        // regression that took the write branch would shell out to gcloud —
        // the refusal, and its wording, is what proves it did not.
        let empty = serde_json::json!({ "etag": "BwYb0000000=" });
        let err = ensure_web_signing_iam_with(
            &sample_config(),
            false,
            SigningIamAuthority::AssertOnly,
            || Ok(empty),
        )
        .expect_err("an absent binding must fail the roll under --assert-signing-iam");
        let text = err.to_string();

        assert!(text.contains("--assert-signing-iam"), "{text}");
        assert!(
            text.contains("is missing"),
            "the operator is told the binding is absent, not that it was unreadable: {text}"
        );
        assert!(
            !text.contains("could not be written"),
            "no write was attempted, so this is not the denied-write message: {text}"
        );
    }

    #[test]
    fn assert_only_refuses_under_dry_run_too() {
        // A dry-run answers "would the real roll work". Under this flag the
        // real roll refuses, so a dry-run that returned Ok would be the same
        // false green that moving the read into every mode was meant to end.
        let empty = serde_json::json!({ "etag": "BwYb0000000=" });
        let err = ensure_web_signing_iam_with(
            &sample_config(),
            true,
            SigningIamAuthority::AssertOnly,
            || Ok(empty),
        )
        .expect_err("the dry-run must refuse exactly as the live roll would");

        assert!(err.to_string().contains("--assert-signing-iam"), "{err}");
    }

    #[test]
    fn assert_only_leaves_a_present_binding_untouched() {
        // The flag narrows one branch and must not disturb the steady state:
        // on a provisioned row — every row `ops gcp setup` has touched — the
        // roll still clears the step on the read alone, live and dry alike.
        let bound = policy_with(
            "roles/iam.serviceAccountTokenCreator",
            &format!(
                "serviceAccount:{}",
                web_gsa_email("my-org-prod", "navigator-web")
            ),
        );
        let mut read_calls = 0;

        ensure_web_signing_iam_with(
            &sample_config(),
            false,
            SigningIamAuthority::AssertOnly,
            || {
                read_calls += 1;
                Ok(bound)
            },
        )
        .expect("a bound row clears the step whether or not the roll may write");

        assert_eq!(read_calls, 1, "the verify is still performed: {read_calls}");
    }

    #[test]
    fn the_assert_refusal_names_the_write_permission_and_quotes_the_hand_off() {
        // The operator ran with the flag precisely because they cannot make
        // this grant, so the refusal has to be a hand-off: the permission, a
        // role carrying it, and the verbatim command for whoever holds it.
        let gsa = "service-account@example.invalid";
        let text = signing_iam_assert_failed(
            gsa,
            &web_signing_iam_binding_args("example-staging-project", "example-staging-project-web"),
        )
        .to_string();

        assert!(text.contains(gsa), "{text}");
        assert!(text.contains("iam.serviceAccounts.setIamPolicy"), "{text}");
        assert!(text.contains("roles/iam.serviceAccountAdmin"), "{text}");
        assert!(
            text.contains("gcloud iam service-accounts add-iam-policy-binding"),
            "the hand-off must be copy-pasteable: {text}"
        );
        assert!(
            text.contains("roles/iam.serviceAccountTokenCreator"),
            "the quoted command must name the role it grants: {text}"
        );
    }

    #[test]
    fn the_roll_may_write_the_binding_unless_the_flag_withdraws_it() {
        // The wiring. Defaulting this to AssertOnly would silently break every
        // provisioning roll on an unbound row, so the default is asserted here
        // rather than left to `#[derive(Default)]` going unread.
        let mut opts = ShipOpts {
            deployment: "example-staging-project".into(),
            tag: Some("26.8.22".into()),
            ..ShipOpts::default()
        };
        assert_eq!(
            signing_iam_authority(&opts),
            SigningIamAuthority::Write,
            "an operator who passed no flag keeps the historical behaviour"
        );

        opts.assert_signing_iam = true;
        assert_eq!(
            signing_iam_authority(&opts),
            SigningIamAuthority::AssertOnly,
            "--assert-signing-iam is what withdraws the write"
        );
    }

    #[test]
    fn a_denied_read_names_the_permission_not_a_raw_gcloud_error() {
        // The operator's next move is a grant request, so the message has to
        // carry the permission, a role that holds it, and the resource — and
        // must say we could not VERIFY, not that the binding is missing.
        let gsa = "service-account@example.invalid";
        let text = signing_iam_read_failed(
            gsa,
            "ERROR: (gcloud.iam.service-accounts.get-iam-policy) PERMISSION_DENIED",
        )
        .to_string();

        assert!(text.contains("cannot verify"), "{text}");
        assert!(text.contains("iam.serviceAccounts.getIamPolicy"), "{text}");
        assert!(text.contains("roles/iam.serviceAccountAdmin"), "{text}");
        assert!(text.contains(gsa), "{text}");
        assert!(
            !text.contains("is missing"),
            "an unreadable policy is not a known-absent binding: {text}"
        );
    }

    #[test]
    fn a_denied_write_says_the_binding_is_missing_and_quotes_the_hand_off() {
        // The other half of the distinction: here we know the pod cannot sign.
        let project = "example-staging-project";
        let account = "example-staging-project-web";
        let gsa = web_gsa_email(project, account);
        let text = signing_iam_write_failed(
            &gsa,
            &web_signing_iam_binding_args(project, account),
            "ERROR: (gcloud.iam.service-accounts.add-iam-policy-binding) PERMISSION_DENIED",
        )
        .to_string();

        assert!(text.contains("is missing"), "{text}");
        assert!(text.contains("iam.serviceAccounts.setIamPolicy"), "{text}");
        assert!(text.contains("roles/iam.serviceAccountAdmin"), "{text}");
        assert!(text.contains("signBlob"), "{text}");
        assert!(
            text.contains(&format!("add-iam-policy-binding {gsa}")),
            "the message must quote the command to hand off: {text}"
        );
    }

    /// A `SecretProviderClass` catalog with comments between entries.
    const TWO_OBJECT_SPC: &str = r#"apiVersion: secrets-store.csi.x-k8s.io/v1
kind: SecretProviderClass
metadata:
  name: navigator-web
spec:
  parameters:
    secrets: |
      - resourceName: "projects/p/secrets/RESTATE_AUTH_TOKEN/versions/latest"
        path: "RESTATE_AUTH_TOKEN"
      # A load-bearing comment the filter must not disturb.
      - resourceName: "projects/p/secrets/DOCUSIGN_HMAC_KEY/versions/latest"
        path: "DOCUSIGN_HMAC_KEY"
"#;

    #[test]
    fn omitting_an_object_removes_it_from_the_catalog() {
        let declined = BTreeSet::from(["DOCUSIGN_HMAC_KEY".to_string()]);
        let filtered =
            without_projected_objects(TWO_OBJECT_SPC, &declined).expect("the filter succeeds");

        assert!(!filtered.contains("DOCUSIGN_HMAC_KEY"));
        assert_eq!(
            projected_object_names(&filtered).expect("the filtered class parses"),
            BTreeSet::from(["RESTATE_AUTH_TOKEN".to_string()])
        );
        assert!(
            filtered.contains("# A load-bearing comment the filter must not disturb."),
            "a line filter is used precisely so the manifest's comments survive"
        );
    }

    #[test]
    fn omitting_nothing_leaves_the_class_byte_identical() {
        // Every deployment that supplies the whole object list — `example-staging-project`
        // today — must render exactly what it renders now.
        let filtered = without_projected_objects(TWO_OBJECT_SPC, &BTreeSet::new())
            .expect("an empty omission succeeds");
        assert_eq!(filtered, TWO_OBJECT_SPC);
    }

    #[test]
    fn an_entry_that_survives_the_filter_fails_the_render() {
        // The structural backstop. The line filter knows one entry shape; if
        // the manifest ever grows another — here a flow mapping, still valid
        // YAML and still a real reference — the ship must abort rather than
        // reconcile a class whose catalog retains the reference it left behind.
        let unremovable = TWO_OBJECT_SPC.replace(
            "      - resourceName: \"projects/p/secrets/DOCUSIGN_HMAC_KEY/versions/latest\"\n        path: \"DOCUSIGN_HMAC_KEY\"\n",
            "      - {resourceName: \"projects/p/secrets/DOCUSIGN_HMAC_KEY/versions/latest\", path: \"DOCUSIGN_HMAC_KEY\"}\n",
        );
        let declined = BTreeSet::from(["DOCUSIGN_HMAC_KEY".to_string()]);
        let error = without_projected_objects(&unremovable, &declined)
            .expect_err("a surviving reference must abort the render");
        assert!(
            error.to_string().contains("DOCUSIGN_HMAC_KEY"),
            "the abort must name the object it could not remove: {error}"
        );
    }

    /// Render the GKE tree for `deployment` and return the object names its
    /// `SecretProviderClass` references after the per-deployment omission —
    /// the exact list `ops secrets apply` must provide.
    fn rendered_object_names(deployment: &str, env: &'static [(&str, &str)]) -> BTreeSet<String> {
        let subs = resolve_substitutions_for_deployment(deployment, "26.8.9", env_getter(env))
            .expect("env resolves");
        let rendered = render_manifests_with(&subs).expect("render succeeds");
        let gke = rendered.path().join(GKE_KUSTOMIZE_SUBPATH);
        let root = super::super::deployments::Deployment::load(&fixture_tree(), deployment)
            .expect("the deployment loads");
        let skipped =
            super::super::deployments::skipped_projected_objects(&root).unwrap_or_default();
        omit_unwritten_objects(&gke, &skipped).expect("the omission succeeds");
        let spc = fs::read_to_string(gke.join(SECRET_PROVIDER_CLASS))
            .expect("read the rendered SecretProviderClass");
        projected_object_names(&spc).expect("the rendered class parses")
    }

    /// The synthetic deployment tree these render tests read.
    ///
    /// The real rows moved to a private repository with the credential that
    /// rolls them. They were also both
    /// `provisioned = false`, so every assertion below was dormant — the
    /// omission is computed from a row's `secrets.enc.yaml` and an
    /// unprovisioned row has none. The fixture declares two provisioned rows,
    /// one per side of the two forks this seam turns on, and arms them. See
    /// `cli/tests/fixtures/deployment-tree/README.md`.
    fn fixture_tree() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("deployment-tree")
    }

    /// The row that declines `DocuSign` and does not own the webhook receiver.
    const ORDINARY_ROW: &str = "example-deployment";
    /// The explicitly marked automation-home row, declaring `DocuSign` and
    /// owning the receiver, so it skips nothing.
    const AUTOMATION_HOME_ROW: &str = "example-automation-home";

    #[test]
    fn the_rendered_class_references_exactly_what_the_deployment_writes() {
        // The invariant the whole seam exists for, asserted for every real
        // deployment: the catalog may reference an object only if
        // `ops secrets apply` writes it. Referencing fewer would leave a boot
        // requirement absent from the deployment Secret; referencing more is a
        // value `ops secrets apply` cannot write.
        // Both fixture rows, because they skip different things: the ordinary
        // row omits DocuSign and the receiver trio, the automation home omits
        // nothing. One row could only ever prove one of those.
        for (deployment, env) in [(ORDINARY_ROW, HUB_ENV), (AUTOMATION_HOME_ROW, HUB_ENV)] {
            let root = super::super::deployments::Deployment::load(&fixture_tree(), deployment)
                .expect("the deployment loads");
            assert!(
                root.provisioned,
                "{deployment} must be provisioned or this assertion is dormant"
            );
            let skipped = super::super::deployments::skipped_projected_objects(&root)
                .expect("the tree is complete");
            let expected: BTreeSet<String> = super::super::deployments::projected_objects()
                .into_iter()
                .filter(|object| !skipped.contains(object))
                .collect();
            assert_eq!(
                rendered_object_names(deployment, env),
                expected,
                "{deployment}'s rendered class must reference exactly the objects it writes"
            );
        }
    }

    #[test]
    fn a_deployment_that_declines_docusign_renders_no_docusign_reference() {
        // A row that declares no `DOCUSIGN_BASE_URL` runs
        // `StubSignatureProvider`, holds no DocuSign object, and its catalog must
        // not ask for one. This is the ship half of the failure: without it the
        // resolve preflight aborts naming all nine, and the only way past would
        // be a placeholder credential — which boots the real provider instead
        // of the stub.
        let referenced = rendered_object_names(ORDINARY_ROW, HUB_ENV);
        assert!(
            !referenced
                .iter()
                .any(|object| object.starts_with("DOCUSIGN_")),
            "a declined integration must leave no reference behind: {referenced:?}"
        );
        // …while everything it does write is still projected.
        assert!(referenced.contains("NAVIGATOR_SURREAL_ENDPOINT"));
    }

    #[test]
    fn a_deployment_that_declines_dash0_renders_no_dash0_reference() {
        let referenced = rendered_object_names(ORDINARY_ROW, HUB_ENV);
        assert!(
            !referenced.contains("DASH0_TOKEN"),
            "a declined integration must leave no Dash0 Secret reference behind: {referenced:?}"
        );
    }

    #[test]
    fn the_automation_home_keeps_every_reference_but_production_social_apps() {
        // The other half: the automation home declares DocuSign and owns the
        // receiver, so it keeps every reference except the Brand social
        // objects, whose secrets are production-only and which it declines.
        let referenced = rendered_object_names(AUTOMATION_HOME_ROW, HUB_ENV);
        let expected: BTreeSet<String> = super::super::deployments::projected_objects()
            .into_iter()
            .filter(|object| !SOCIAL_OBJECTS.contains(&object.as_str()))
            .collect();
        assert_eq!(referenced, expected);
    }

    #[test]
    fn a_production_row_that_declares_social_apps_renders_every_social_reference() {
        let referenced = rendered_object_names(ORDINARY_ROW, FULL_ENV);
        for object in SOCIAL_OBJECTS.iter().chain(&["NAVIGATOR_RUNTIME_KMS_KEY"]) {
            assert!(referenced.contains(*object), "{object}: {referenced:?}");
        }
        assert!(!referenced.contains("NAVIGATOR_NOTION_DATABASE_ID"));
    }

    // ---------- the narrow `--image-only` lane ----------

    #[test]
    fn only_a_clean_diff_lets_the_image_only_lane_proceed() {
        // Exit 0 is the ONLY code that may continue. This is the property the
        // whole lane rests on: it applies no manifest change, so it is safe
        // exactly when there is no manifest change to apply.
        assert!(super::drift_verdict(Some(0)).is_ok());
    }

    #[test]
    fn manifest_drift_refuses_the_image_only_lane() {
        // Exit 1 means `kubectl diff` found a difference. The full roll treats
        // that as the normal signal to apply; this lane must treat it as
        // fatal, because it would move the images and silently leave the rest
        // of the diff unapplied. The inversion is the point.
        let error = super::drift_verdict(Some(1)).expect_err("drift is fatal here");
        let message = error.to_string();
        assert!(
            message.contains("not a version bump"),
            "the refusal must say why the lane is the wrong tool: {message}"
        );
        assert!(
            message.contains("ops ship"),
            "the refusal must name the roll that CAN apply the diff: {message}"
        );
    }

    #[test]
    fn an_unreachable_cluster_is_not_read_as_a_clean_diff() {
        // >1 is `kubectl diff` failing outright — unreachable context, auth
        // failure, kustomize build error. Nothing was compared, so nothing was
        // proven, and proceeding would ship on the strength of a check that
        // never ran. `None` is a signal kill: the same absence of an answer.
        for code in [Some(2), Some(127), None] {
            let error =
                super::drift_verdict(code).expect_err("a failed diff proves nothing about drift");
            assert!(
                error.to_string().contains("the cluster was not reached"),
                "exit {code:?} must read as an unreached cluster: {error}"
            );
        }
    }

    #[test]
    fn the_image_only_lane_never_guesses_a_tag() {
        // No `--tag` is a refusal, not a lookup of the latest published
        // release. The scheduled caller names the release it means.
        let error = super::image_only_tag(None).expect_err("--tag is required");
        assert!(
            error.to_string().contains("--image-only requires --tag"),
            "{error}"
        );
    }

    #[test]
    fn the_image_only_lane_rejects_a_tag_that_is_not_a_release() {
        // The scheduled caller derives its tag from a clock, so a derivation
        // that produces the wrong shape must fail here rather than as a pod
        // pulling an image nobody published.
        assert!(super::image_only_tag(Some("26.8.17")).is_ok());
        assert!(super::image_only_tag(Some("26.8.19-hotfix.14")).is_ok());
        for wrong in ["", "2026-08-17", "latest", "v26.8.17"] {
            assert!(
                super::image_only_tag(Some(wrong)).is_err(),
                "`{wrong}` is not a release tag"
            );
        }
    }

    #[test]
    fn the_narrowest_flag_picks_the_lane() {
        // `--image-only` is the lane an automated deploy runs under a
        // credential that can do nothing but bump a version, so it wins over a
        // stray `--restart-only` instead of silently widening into a restart.
        let roll = ShipOpts {
            deployment: "example-prod".into(),
            ..ShipOpts::default()
        };
        assert_eq!(ship_lane(&roll), ShipLane::Roll);
        assert_eq!(
            ship_lane(&ShipOpts {
                restart_only: true,
                ..roll.clone()
            }),
            ShipLane::RestartOnly
        );
        assert_eq!(
            ship_lane(&ShipOpts {
                image_only: true,
                ..roll.clone()
            }),
            ShipLane::ImageOnly
        );
        assert_eq!(
            ship_lane(&ShipOpts {
                image_only: true,
                restart_only: true,
                ..roll
            }),
            ShipLane::ImageOnly
        );
        assert!(lane_requires_asset_preflight(ShipLane::ImageOnly));
        assert!(lane_requires_asset_preflight(ShipLane::Roll));
        assert!(!lane_requires_asset_preflight(ShipLane::RestartOnly));
    }

    #[test]
    fn an_image_only_dry_run_walks_every_step_without_a_cluster() {
        // The whole lane under `--dry-run`: the context cross-check, the drift
        // refusal, both image writes, the `CronJob` re-pin, the rollout wait,
        // and the Restate re-register. Each one prints its command instead of
        // running it, so this proves the sequence is complete and
        // side-effect-free — a step that reached for kubectl, gcloud, the
        // deployment tree, or the network would fail right here.
        image_only_steps(
            &sample_config(),
            &ordered_deployment(),
            "26.8.17",
            true,
            Path::new("/nonexistent"),
        )
        .expect("a dry-run image-only roll needs no cluster");
    }

    #[test]
    fn a_copied_context_aborts_the_image_only_lane_too() {
        // The narrow lane skips the manifest apply, not the guard that keeps a
        // stale shell from rolling one deployment onto another's cluster.
        let cfg = ShipConfig {
            project_id: "neon-law".into(),
            cluster: "acme-stg".into(),
            context: "gke_example-staging-project_example-region_example-staging-project".into(),
            ..sample_config()
        };
        let err = image_only_steps(
            &cfg,
            &ordered_deployment(),
            "26.8.17",
            true,
            Path::new("/nonexistent"),
        )
        .expect_err("a copied context must abort the narrow lane");
        assert!(err.to_string().contains("Shipping would roll"), "{err}");
    }

    #[test]
    fn image_only_pins_both_deployments_to_one_published_tag() {
        let writes = image_only_image_writes(&sample_config(), "26.8.17");
        assert_eq!(
            writes.iter().map(|(d, _)| *d).collect::<Vec<_>>(),
            vec![WEB_DEPLOYMENT, WORKFLOWS_DEPLOYMENT],
            "both service deployments move, never one"
        );
        for (deployment, image) in &writes {
            assert!(
                image.starts_with("ghcr.io/example/navigator/"),
                "{deployment} pulls the published image: {image}"
            );
            assert!(
                image.ends_with(":26.8.17"),
                "{deployment} lands on the named release with no skew: {image}"
            );
        }
    }

    #[test]
    fn a_dry_run_reads_no_cluster_to_decide_drift() {
        // Dry-run renders nothing and reads nothing, so the diff never runs —
        // which is why a root that does not exist is still fine here. Reading
        // the lane back before running it must not itself render the tree or
        // touch the cluster.
        refuse_on_manifest_drift(&sample_config(), &ordered_deployment(), "26.8.17", true)
            .expect("a dry-run drift check needs no cluster");
    }

    #[test]
    fn a_restart_only_dry_run_walks_its_steps_without_a_cluster() {
        // The sibling lane behind the same seam: rollout restart, then wait.
        restart_only_steps(&sample_config(), true).expect("a dry-run restart needs no cluster");
    }

    /// A root shaped like a source checkout: `server/content` exists to walk.
    fn source_checkout() -> TempDir {
        let root = tempfile::tempdir().expect("create a test workspace");
        fs::create_dir_all(root.path().join("server/content")).expect("create content tree");
        root
    }

    #[test]
    fn a_completed_roll_verifies_public_assets_after_re_registering() {
        let root = source_checkout();
        let events = std::cell::RefCell::new(Vec::new());

        finish_roll(
            ShipLane::Roll,
            &sample_config(),
            root.path(),
            false,
            || {
                events.borrow_mut().push("restate");
                Ok(())
            },
            |content_root, base_url| {
                events.borrow_mut().push("assets");
                assert_eq!(content_root, root.path().join("server/content"));
                assert_eq!(base_url, "https://www.example.com/assets");
                Ok(())
            },
            |_| unreachable!("a source checkout walks its own content tree"),
        )
        .expect("a live completed roll should verify its public origin");

        assert_eq!(*events.borrow(), ["restate", "assets"]);
    }

    #[test]
    fn a_failed_post_roll_asset_check_blocks_ship_completion() {
        let root = source_checkout();
        let events = std::cell::RefCell::new(Vec::new());

        let error = finish_roll(
            ShipLane::ImageOnly,
            &sample_config(),
            root.path(),
            false,
            || {
                events.borrow_mut().push("restate");
                Ok(())
            },
            |_, _| {
                events.borrow_mut().push("assets");
                Err(anyhow!("public origin is unreachable"))
            },
            |_| unreachable!("a source checkout walks its own content tree"),
        )
        .expect_err("an unreachable public origin must block completion");

        assert_eq!(*events.borrow(), ["restate", "assets"]);
        assert!(error
            .to_string()
            .contains("post-roll public asset verification"));
        assert!(format!("{error:#}").contains("public origin is unreachable"));
    }

    #[test]
    fn a_deploy_only_tree_probes_the_embedded_asset_set_after_re_registering() {
        // ENG-552. The documented `--deployments-dir .` invocation from a tree
        // that carries `deployments/` and no application source used to fail
        // here with "content directory `./server/content` does not exist"
        // after the roll had already landed. The check still runs, still
        // after Restate, against the references this binary embeds.
        let root = tempfile::tempdir().expect("create a deploy-only tree");
        let events = std::cell::RefCell::new(Vec::new());

        finish_roll(
            ShipLane::Roll,
            &sample_config(),
            root.path(),
            false,
            || {
                events.borrow_mut().push("restate");
                Ok(())
            },
            |_, _| unreachable!("a deploy-only tree has no content tree to walk"),
            |base_url| {
                events.borrow_mut().push("embedded");
                assert_eq!(base_url, "https://www.example.com/assets");
                Ok(())
            },
        )
        .expect("a deploy-only roll verifies the embedded set instead of failing");

        assert_eq!(*events.borrow(), ["restate", "embedded"]);
    }

    #[test]
    fn a_failed_embedded_asset_check_blocks_ship_completion() {
        // The fallback is a different reference source, not a softer gate.
        let root = tempfile::tempdir().expect("create a deploy-only tree");
        let events = std::cell::RefCell::new(Vec::new());

        let error = finish_roll(
            ShipLane::ImageOnly,
            &sample_config(),
            root.path(),
            false,
            || {
                events.borrow_mut().push("restate");
                Ok(())
            },
            |_, _| unreachable!("a deploy-only tree has no content tree to walk"),
            |_| {
                events.borrow_mut().push("embedded");
                Err(anyhow!("workshop hero is missing"))
            },
        )
        .expect_err("a missing embedded asset must block completion");

        assert_eq!(*events.borrow(), ["restate", "embedded"]);
        assert!(error
            .to_string()
            .contains("post-roll public asset verification failed for deployment `example-prod`"));
        assert!(format!("{error:#}").contains("workshop hero is missing"));
    }

    #[test]
    fn a_dry_run_names_its_reference_source_and_probes_nothing() {
        // Under `--dry-run` neither verifier runs from either root shape; the
        // printed line carries which set a live run would probe.
        for root in [
            source_checkout(),
            tempfile::tempdir().expect("deploy-only tree"),
        ] {
            finish_roll(
                ShipLane::Roll,
                &sample_config(),
                root.path(),
                true,
                || Ok(()),
                |_, _| unreachable!("dry-run probes no origin"),
                |_| unreachable!("dry-run probes no origin"),
            )
            .expect("a dry-run finish needs no origin");
        }

        let checkout = source_checkout();
        let content_root = checkout.path().join("server/content");
        assert_eq!(
            post_roll_asset_refs(checkout.path()).describe(),
            format!("references from `{}`", content_root.display())
        );

        let deploy_only = tempfile::tempdir().expect("deploy-only tree");
        let missing = deploy_only.path().join("server/content");
        assert_eq!(
            post_roll_asset_refs(deploy_only.path()).describe(),
            format!(
                "`{}` is absent, so the references embedded in this CLI",
                missing.display()
            )
        );
    }

    #[test]
    fn preflight_and_post_roll_read_the_same_asset_inventory() {
        let deploy_only = tempfile::tempdir().expect("deploy-only tree");
        assert!(matches!(
            post_roll_asset_refs(deploy_only.path()),
            PostRollAssetRefs::Embedded { .. }
        ));
        assert_eq!(
            crate::assets::applicable_asset_refs(deploy_only.path()).expect("embedded inventory"),
            crate::assets::embedded_asset_refs()
        );

        let checkout = source_checkout();
        fs::write(
            checkout.path().join("server/content/note.md"),
            "![hero](img/demo/hero.png)\n",
        )
        .expect("write a content reference");
        match post_roll_asset_refs(checkout.path()) {
            PostRollAssetRefs::ContentTree(dir) => {
                assert_eq!(
                    crate::assets::applicable_asset_refs(checkout.path())
                        .expect("content inventory"),
                    crate::assets::published_asset_refs(&dir).expect("content refs")
                );
            }
            PostRollAssetRefs::Embedded { .. } => {
                panic!("a source checkout walks its content tree");
            }
        }

        let odd = tempfile::tempdir().expect("odd tree");
        fs::create_dir_all(odd.path().join("server")).expect("create server dir");
        fs::write(odd.path().join("server/content"), b"").expect("write a stray file");
        assert!(matches!(
            post_roll_asset_refs(odd.path()),
            PostRollAssetRefs::Embedded { .. }
        ));
        assert_eq!(
            crate::assets::applicable_asset_refs(odd.path()).expect("embedded inventory"),
            crate::assets::embedded_asset_refs()
        );
    }

    #[test]
    fn the_reference_source_follows_whether_the_content_tree_is_a_directory() {
        let checkout = source_checkout();
        assert_eq!(
            post_roll_asset_refs(checkout.path()),
            PostRollAssetRefs::ContentTree(checkout.path().join("server/content"))
        );

        let deploy_only = tempfile::tempdir().expect("deploy-only tree");
        assert_eq!(
            post_roll_asset_refs(deploy_only.path()),
            PostRollAssetRefs::Embedded {
                missing: deploy_only.path().join("server/content"),
            }
        );

        // A file where the directory should be is nothing to walk either.
        let odd = tempfile::tempdir().expect("odd tree");
        fs::create_dir_all(odd.path().join("server")).expect("create server dir");
        fs::write(odd.path().join("server/content"), b"").expect("write a stray file");
        assert_eq!(
            post_roll_asset_refs(odd.path()),
            PostRollAssetRefs::Embedded {
                missing: odd.path().join("server/content"),
            }
        );
    }

    #[test]
    fn restart_only_never_runs_the_post_roll_asset_check() {
        let root = tempfile::tempdir().expect("create a test workspace");
        let events = std::cell::RefCell::new(Vec::new());
        finish_roll(
            ShipLane::RestartOnly,
            &sample_config(),
            root.path(),
            false,
            || {
                events.borrow_mut().push("restate");
                Ok(())
            },
            |_, _| {
                events.borrow_mut().push("assets");
                Ok(())
            },
            |_| {
                events.borrow_mut().push("embedded");
                Ok(())
            },
        )
        .expect("restart-only completion does not need asset verification");
        assert_eq!(*events.borrow(), ["restate"]);
    }
}
