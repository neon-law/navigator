// devx — developer-experience tool that brings up Neon Law Navigator
// dependency stack (SeaweedFS, Rauthy, Restate, SurrealDB, OpenObserve, and
// ClamAV) inside a KIND cluster while leaving the `web` binary on the host so
// it can be restarted in-process during a Rust edit-compile
// loop.
//
// The KIND cluster + manifests are also what `devx deploy` uses.
// Both flows drive Kustomize overlays under `k8s/overlays/`:
//   - `kind-deps` — base + deps + workflows-service (no `web`),
//                   used by `devx up` for host-side iteration
//   - `kind`      — full local stack including `web`,
//                   used by `devx deploy`
//   - `gke`       — production overlay; `ops ship` renders and
//                   reconciles this in GKE Autopilot

use std::env;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{bail, Context, Result};
use clap::Subcommand;

pub mod brand;
mod brand_readiness;
mod browser_e2e;
pub(crate) mod call_script;
// The scraper framework resolves the same pinned Chrome for Testing the
// e2e harnesses use, so the resolver is crate-reachable.
pub(crate) mod chrome;
mod deployment_config;
mod deployment_derivation;
pub mod deployments;
mod dns;
mod doctor;
mod e2e;
mod email_summary_delivery;
mod email_summary_redrive;
mod firm_certificate_roll;
mod flake_hunt;
mod forgejo;
mod gcp;
mod github_auto_merge;
pub(crate) mod github_setup;
mod host_setup;
mod lifecycle;
mod native_profile;
// `navigator jj setup` installs jj with the same Homebrew seam.
// `hudson same-day preview` serves the bundle this module stages, so
// its staging helper is crate-reachable.
mod observability;
mod orchestrate;
mod postgres_fixture;
mod postgres_volume;
pub(crate) mod project_slack_reconcile;
mod quickstart;
pub(crate) mod registry;
mod runtime;
pub(crate) mod same_day;
mod sample_project;
mod seaweedfs;
mod serve;
mod ship;
mod ship_log;
pub(crate) mod signed_commits;
mod social_key_changes;
mod staging;
mod webapp;
mod worktree_env;

pub use github_setup::RepositoryTarget;
pub use lifecycle::Action as StagingAction;
pub use worktree_env::WorktreeEnvCmd;

// Machine-bound orchestration lives in `orchestrate` (excluded from the
// coverage gate in `ci.yml` because every one of its functions drives
// KIND/Kubernetes/Docker or a host process). Re-export its `pub(super)`
// entry points so `dispatch` below and the sibling devx modules reach them
// as `super::<name>`, exactly as when they lived here.
use orchestrate::{
    down, down_in, kind_down_only, kind_up_only, kustomize_render, logs, print_env,
    rauthy_deployment_exists, require_auth, require_tools, run, status, undeploy, up,
    use_kind_context, wait_for_tcp, wait_rollout, workspace_root,
};

// KIND/local defaults. Each pairs with a `KindConfig` field and a
// `NAVIGATOR_*` env var (see `KindConfig::from_env`). The constants are
// the fallback an empty `.env` resolves to, so default behavior is
// byte-for-byte what the old inline `const`s gave.
const DEFAULT_CLUSTER_NAME: &str = "navigator";
const DEFAULT_NAMESPACE: &str = "navigator";
// Exact upstream KIND manifest from ingress-nginx controller-v1.11.2:
// deploy/static/provider/kind/deploy.yaml
// SHA-256: dc850e38ca4abcb08625f1601f0656c81b6b5e34cc23d5458c332060732c14e0
const INGRESS_MANIFEST: &str = "k8s/vendor/ingress-nginx-controller-v1.11.2.yaml";
#[cfg(test)]
const INGRESS_MANIFEST_SHA256: &str =
    "dc850e38ca4abcb08625f1601f0656c81b6b5e34cc23d5458c332060732c14e0";

// Restate Operator — same chart drives KIND and GKE. Release notes:
// https://github.com/restatedev/restate-operator/releases. Kept aligned
// with the server image in `k8s/staging/restate.yaml` and
// `RESTATE_CLI_VERSION` below (2.8.1 / 1.7.2), and with the chart
// version in `.github/workflows/deploy.yml`. NOTE: the local "restate
// won't provision" wedge was NOT a version skew — it was the operator's
// own `deny-all` NetworkPolicy being enforced by recent kindnet, which
// blocked the operator→node `:5122` provisioning dial. The fix lives in
// `restate.yaml` (`security.disableNetworkPolicies: true`), not here.

// The Operator's CRDs, applied explicitly by `install_restate_operator`.
//
// Chart 2.8.1 moved these into a native Helm `crds/` directory (gated by
// the chart's `installCrds`, default on). Helm installs a native CRD once
// and never touches it again on `helm upgrade` — a deliberately
// conservative lifecycle — so the chart alone would freeze a long-lived
// cluster's CRD schema at whatever version first created it while a fresh
// cluster got the current one. That silent split is exactly the drift the
// pinned constants exist to prevent, so apply the release's CRD artifacts
// on every `up` and let both converge.
// Exact v2.8.1 release artifact SHA-256 digests.
#[cfg(test)]
#[cfg(test)]
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    use std::fmt::Write as _;

    let mut output = String::with_capacity(64);
    for byte in sha2::Sha256::digest(bytes) {
        write!(output, "{byte:02x}").expect("write a SHA-256 digest to a string");
    }
    output
}

#[cfg(test)]
mod ingress_manifest_tests {
    use super::*;

    #[test]
    fn ingress_manifest_is_vendored_and_pinned() {
        assert!(
            !INGRESS_MANIFEST.contains("://"),
            "worktree bootstrap must not ask kubectl to download the ingress manifest"
        );

        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("repo root is cli/'s parent");
        let manifest = std::fs::read(root.join(INGRESS_MANIFEST))
            .expect("read the vendored ingress-nginx manifest");
        assert_eq!(
            sha256_hex(&manifest),
            INGRESS_MANIFEST_SHA256,
            "vendored manifest must match the recorded upstream digest"
        );
        assert!(
            std::str::from_utf8(&manifest)
                .expect("vendored ingress-nginx manifest is UTF-8")
                .contains("registry.k8s.io/ingress-nginx/controller:v1.11.2"),
            "vendored manifest must match the pinned ingress-nginx release"
        );
    }
}

// Restate CLI — pinned so operator-laptop and CI-runner versions
// don't drift. The CLI talks to Restate Cloud's admin API; a
// mismatch silently desyncs `deployments register`. 1.6.x renamed
// `deployment` to `deployments` and `--env` to `--environment`.
// Tracks the server line (see `RESTATE_OPERATOR_VERSION` above and
// the server image in `restate.yaml`) — keep all three on 1.7.x.
// Only `check_restate_cli_version` reads this: it warns when the CLI
// on PATH drifts. There is no CI env to keep in step.

// Last-resort public HTTPS endpoint Restate Cloud uses to reach the
// `workflows-service` worker. Backed by the ingress + managed cert
// under `examples/deploy/k8s/gke/ingress/workflows-*.yaml`. This is a
// placeholder of *last* resort: the URL is resolved by
// [`resolve_workflows_url`], which prefers an explicit `--url` /
// `NAVIGATOR_WORKFLOWS_URL`, then derives `https://workflows.<domain>/`
// from the resolved `brand.primary_domain` (Neon Law by default, or a custom
// bundle's), and only falls back to this constant when none of those are set.
// Hitting this constant in prod means the
// re-register silently no-ops (the 2026-06-10 ship symptom).

// Local `:dev` image tags KIND loads. CI publishes the real images to
// the private Artifact Registry (`YY.M.D`); `pull_retag_load` pulls one
// and retags it to the `:dev` name the manifests reference, so the
// overlays stay unchanged. Trigger images are pulled straight from the
// registry by their CronJobs in prod and are never loaded into the local
// cluster, so they need no local tag constant here.

// Kustomize overlay roots. `Up` applies the deps-only overlay (no
// in-cluster `web`). `Deploy` applies the full overlay including
// `web`. The `gke` overlay is reconciled by Config Sync in
// production — `dev kustomize gke` renders it locally for inspection.
const DEFAULT_KUSTOMIZE_KIND_DEPS: &str = "k8s/overlays/kind-deps";
const DEFAULT_KUSTOMIZE_KIND: &str = "k8s/overlays/kind";
// GKE overlay lives under `examples/deploy/k8s/gke/` (moved out of
// the canonical `k8s/` tree for the product release — the prod
// overlay is now an example users adapt, not a hard-coded part of
// the workspace surface). Config Sync in NeonLaw's prod cluster
// reconciles the same path, just under the new directory.
const DEFAULT_KUSTOMIZE_GKE: &str = "examples/deploy/k8s/gke";

// Host-side ports the locally-run `web` binary connects to.
// Restate's ingress port (8080 in-cluster) is remapped to host 9080
// because KIND already binds host 8080 to its nginx ingress.
const DEFAULT_INGRESS_HTTP_HOST_PORT: u16 = 8080;
const DEFAULT_INGRESS_HTTPS_HOST_PORT: u16 = 8443;
const DEFAULT_RESTATE_INGRESS_HOST_PORT: u16 = 9080;
const DEFAULT_RESTATE_ADMIN_HOST_PORT: u16 = 9070;
const DEFAULT_CLAMAV_HOST_PORT: u16 = 13310;
// Rauthy is exposed through KIND's fixed NodePort mapping. SeaweedFS S3
// uses a normal `kubectl port-forward`, so existing clusters pick up a
// changed host port without being recreated.
const DEFAULT_RAUTHY_HOST_PORT: u16 = 30080;
const LOCAL_RAUTHY_CLIENT_SECRET: &str =
    "navigatorwebsecretnavigatorwebsecretnavigatorwebsecretnavigatorw";
const DEFAULT_SEAWEEDFS_S3_HOST_PORT: u16 = 30900;
// SurrealDB (the `surreal` Service in `navigator`), the store the
// workspace is porting onto (#1093). Its own port 8000 is a crowded
// default on a developer's machine, and 8080 is already KIND's ingress,
// so the host mapping is 18000 — clear of every other port this module
// binds.
const DEFAULT_SURREAL_HOST_PORT: u16 = 18000;

// Local `web` defaults — matches `cargo run -p neon` defaults.
const DEFAULT_LOCAL_WEB_PORT: u16 = 3001;
// The shared `dev up` tier's fixed second port: DeleteYourData.com,
// locally unreachable by hostname (see ENG-437), needs its own bind port the
// same way `web` needs `DEFAULT_LOCAL_WEB_PORT`. Written under
// `views::brand::BrandKey::DeleteYourData.local_port_env_var()`, the exact
// var name `web` itself reads — devx and the running process can therefore
// never disagree on which port serves that brand.
const DEFAULT_LOCAL_DELETE_YOUR_DATA_WEB_PORT: u16 = 3011;

fn delete_your_data_port_env_var() -> &'static str {
    views::brand::BrandKey::DeleteYourData
        .local_port_env_var()
        .expect("DeleteYourData is a registered non-default key and always has a local port var")
}

// OpenObserve's UI (5080) and direct OTLP gRPC ingest port (5081) are
// port-forwarded to the host for the operator and host-side `web` process.
const DEFAULT_OPENOBSERVE_HOST_PORT: u16 = 5080;
const DEFAULT_OPENOBSERVE_OTLP_HOST_PORT: u16 = 5081;
// The opt-in KIND Forgejo's browser origin; `ROOT_URL` and the host
// port-forward `dev forgejo up` opens both use it.
const DEFAULT_FORGEJO_HOST_PORT: u16 = 3300;
// The retained PostgreSQL server `dev forgejo up` deploys, forwarded to the
// host so a local client can connect beside Forgejo.
const DEFAULT_POSTGRES_HOST_PORT: u16 = 15_432;
const KIND_OPENOBSERVE_USERNAME: &str = "root@example.com";
const KIND_OPENOBSERVE_PASSWORD: &str = "NavigatorKindOpenObserve1!";
const KIND_OPENOBSERVE_ORGANIZATION: &str = "default";
const KIND_OPENOBSERVE_STREAM: &str = "default";

/// Every KIND/local knob `devx` reads, resolved once in `main()` and
/// threaded into the subcommands. Each field falls back to a
/// `DEFAULT_*` constant, so an empty `.env` reproduces prior behavior
/// exactly. New local knobs are added here and in `from_env`, never as
/// a scattered `env::var` at a call site. See `docs/env-driven-devx.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct KindConfig {
    cluster: String,
    namespace: String,
    deps_overlay: String,
    full_overlay: String,
    gke_overlay: String,
    ingress_http_port: u16,
    ingress_https_port: u16,
    restate_ingress_port: u16,
    restate_admin_port: u16,
    clamav_port: u16,
    rauthy_port: u16,
    seaweedfs_s3_port: u16,
    surreal_port: u16,
    web_port: u16,
    delete_your_data_web_port: u16,
    openobserve_port: u16,
    openobserve_otlp_port: u16,
    forgejo_port: u16,
    postgres_port: u16,
}

impl KindConfig {
    /// The kubectl context for this cluster. Every cluster-mutating command
    /// passes it explicitly rather than relying on the current context: a
    /// stale GKE/EKS context is otherwise indistinguishable from a fresh KIND
    /// boot, and the command lands on the wrong cluster.
    fn kind_context(&self) -> String {
        format!("kind-{}", self.cluster)
    }

    /// Resolve the KIND/local config from the environment, falling back
    /// to the `DEFAULT_*` constants for any var that is unset or empty.
    fn from_env() -> Self {
        Self {
            cluster: env_string("NAVIGATOR_KIND_CLUSTER", DEFAULT_CLUSTER_NAME),
            namespace: env_string("NAVIGATOR_K8S_NAMESPACE", DEFAULT_NAMESPACE),
            deps_overlay: env_string("NAVIGATOR_KIND_DEPS_OVERLAY", DEFAULT_KUSTOMIZE_KIND_DEPS),
            full_overlay: env_string("NAVIGATOR_KIND_OVERLAY", DEFAULT_KUSTOMIZE_KIND),
            gke_overlay: env_string("NAVIGATOR_GKE_OVERLAY", DEFAULT_KUSTOMIZE_GKE),
            ingress_http_port: env_port(
                "NAVIGATOR_KIND_INGRESS_HTTP_PORT",
                DEFAULT_INGRESS_HTTP_HOST_PORT,
            ),
            ingress_https_port: env_port(
                "NAVIGATOR_KIND_INGRESS_HTTPS_PORT",
                DEFAULT_INGRESS_HTTPS_HOST_PORT,
            ),
            restate_ingress_port: env_port(
                "NAVIGATOR_KIND_RESTATE_INGRESS_PORT",
                DEFAULT_RESTATE_INGRESS_HOST_PORT,
            ),
            restate_admin_port: env_port(
                "NAVIGATOR_KIND_RESTATE_ADMIN_PORT",
                DEFAULT_RESTATE_ADMIN_HOST_PORT,
            ),
            clamav_port: env_port("NAVIGATOR_KIND_CLAMAV_PORT", DEFAULT_CLAMAV_HOST_PORT),
            rauthy_port: env_port("NAVIGATOR_KIND_RAUTHY_PORT", DEFAULT_RAUTHY_HOST_PORT),
            seaweedfs_s3_port: env_port(
                "NAVIGATOR_KIND_SEAWEEDFS_S3_PORT",
                DEFAULT_SEAWEEDFS_S3_HOST_PORT,
            ),
            surreal_port: env_port("NAVIGATOR_KIND_SURREAL_PORT", DEFAULT_SURREAL_HOST_PORT),
            web_port: env_port("NAVIGATOR_KIND_WEB_PORT", DEFAULT_LOCAL_WEB_PORT),
            delete_your_data_web_port: env_port(
                delete_your_data_port_env_var(),
                DEFAULT_LOCAL_DELETE_YOUR_DATA_WEB_PORT,
            ),
            openobserve_port: env_port(
                "NAVIGATOR_KIND_OPENOBSERVE_PORT",
                DEFAULT_OPENOBSERVE_HOST_PORT,
            ),
            openobserve_otlp_port: env_port(
                "NAVIGATOR_KIND_OPENOBSERVE_OTLP_PORT",
                DEFAULT_OPENOBSERVE_OTLP_HOST_PORT,
            ),
            forgejo_port: env_port("NAVIGATOR_KIND_FORGEJO_PORT", DEFAULT_FORGEJO_HOST_PORT),
            postgres_port: env_port("NAVIGATOR_KIND_POSTGRES_PORT", DEFAULT_POSTGRES_HOST_PORT),
        }
    }
}

/// The `kubectl` argv that switches the current context to a KIND cluster.
/// Pure so the pin — the guard that keeps a devx write off an ambient (possibly
/// prod) context — stays unit-tested in this coverage-counted module while
/// `orchestrate::use_kind_context` stays process plumbing.
fn use_context_args(context: &str) -> Vec<String> {
    ["config", "use-context", context]
        .iter()
        .map(|arg| (*arg).to_string())
        .collect()
}

/// The `kubectl` argv that deletes the KIND namespace boundary. Pure so the
/// `--context` pin keeps a unit test in this (coverage-counted) module while
/// `orchestrate::undeploy` stays process plumbing.
fn undeploy_args(context: &str, namespace: &str) -> Vec<String> {
    [
        "--context",
        context,
        "delete",
        "--ignore-not-found",
        "namespace",
        namespace,
    ]
    .iter()
    .map(|arg| (*arg).to_string())
    .collect()
}

/// Read a string env var, treating unset *and* empty as "use default".
/// Empty-as-default keeps a `FOO=` line in `.env` from blanking a path.
fn env_string(key: &str, default: &str) -> String {
    env::var(key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Read a `u16` port env var, falling back to `default` when unset,
/// empty, or unparseable. An invalid value falls back rather than
/// crashing the dev loop — the default is always a working port.
fn env_port(key: &str, default: u16) -> u16 {
    env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
        .unwrap_or(default)
}

/// The default bound on the shared Firm certificate's issuance wait.
pub const DEFAULT_CERTIFICATE_TIMEOUT_MINUTES: u32 =
    firm_certificate_roll::DEFAULT_ISSUANCE_TIMEOUT_MINUTES;

/// `hudson ops certificates`: shared-certificate maintenance.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum CertificatesCmd {
    /// Roll the shared Firm certificate onto this build's brand hosts ahead
    /// of the release that changes them: request the certificate named for
    /// the new domain set, attach it beside the one serving, wait until it is
    /// Active and every host serves trusted TLS, then detach and delete the
    /// one it replaces. `ops ship` does the same within the release; this
    /// moves the wait out of it. Run it with the navigator build of that
    /// release. A no-op when the certificate already serves; `--dry-run`
    /// reads the live certificate and prints the plan without writing.
    PreRoll {
        /// Deployment directory under `deployments/`, such as `example-deployment`.
        #[arg(long)]
        deployment: String,
        /// The directory CONTAINING `deployments/`. Defaults to
        /// `NAVIGATOR_DEPLOYMENTS_DIR`, then to the discovered workspace root.
        #[arg(long, value_name = "DIR")]
        deployments_dir: Option<std::path::PathBuf>,
        /// Read the deployment's configuration from this explicit GCP project
        /// instead of a local tree. Requires ADC.
        #[arg(long, conflicts_with = "deployments_dir")]
        project_id: Option<String>,
        /// The bound on the issuance wait, in minutes.
        #[arg(long, default_value_t = DEFAULT_CERTIFICATE_TIMEOUT_MINUTES, value_parser = clap::value_parser!(u32).range(1..))]
        timeout_minutes: u32,
        /// Read the live certificate and print the plan; write nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

/// `hudson ops host`: bring one registered brand host online.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum HostCmd {
    /// Point a registered brand host at a deployment and check it end to end:
    /// its DNS `A` record (through `DNSimple`; preview unless `--apply`), its
    /// TLS certificate (on the Firm `ManagedCertificate` `ops ship` renders
    /// for every registered host), and its sign-in (the per-host handoff
    /// through the deployment's one registered callback, so no identity
    /// provider needs a new redirect URI). Exits nonzero until all three are
    /// ready; safe to rerun.
    Setup {
        /// The host, as registered in `views::brand::BrandKey::hosts`, such as
        /// `iep.neonlaw.com`.
        #[arg(long)]
        host: String,
        /// Deployment directory under `deployments/`, such as `neon-law-prd`.
        #[arg(long)]
        deployment: String,
        /// The directory CONTAINING `deployments/`. Defaults to
        /// `NAVIGATOR_DEPLOYMENTS_DIR`, then to the discovered workspace root.
        #[arg(long, value_name = "DIR")]
        deployments_dir: Option<std::path::PathBuf>,
        /// Read the deployment's configuration from this explicit GCP project
        /// instead of a local tree. Requires ADC.
        #[arg(long, conflicts_with = "deployments_dir")]
        project_id: Option<String>,
        /// The gateway IPv4 the `A` record points at. Defaults to the address
        /// the deployment's own public host resolves to.
        #[arg(long, env = "NAVIGATOR_GATEWAY_IP")]
        gateway_ip: Option<String>,
        /// Write the DNS record. Without it the record is previewed.
        #[arg(long)]
        apply: bool,
        /// Per-check `curl` timeout, in seconds.
        #[arg(long, default_value_t = 15)]
        timeout_seconds: u32,
    },
}

#[derive(Subcommand)]
pub enum DnsCmd {
    /// Provision a domain's public-deploy DNS through `DNSimple`:
    /// reachability, the apex→www redirect, and both mail lanes (human
    /// mail on the apex, application mail on a `parse.` subdomain). Every
    /// group is opt-in via a flag and idempotent (matching record → no-op,
    /// single-valued drift → patched, missing → created; never deleted).
    /// Zone from `--domain` or `DNS_ZONE`; auth from `DNS_ACCT` +
    /// `DNS_SIMPLE` (`DNSIMPLE_API_TOKEN` is a legacy alias). The per-domain `SendGrid` / Google secrets are
    /// flags — never invented. Full recipe: `docs/dns.md`.
    Setup {
        /// Domain to configure (repeatable); defaults to `DNS_ZONE`.
        ///
        /// Repeat it to reconcile a whole family of domains from one
        /// reviewable run. Each zone is reconciled independently and in the
        /// order given, so a failure on the third names that zone and leaves
        /// the first two applied rather than rolling anything back — these
        /// are separate zones, not one transaction.
        #[arg(long = "domain")]
        domains: Vec<String>,
        /// Selected host `A` records → this gateway IP. Falls back to
        /// `NAVIGATOR_GATEWAY_IP` from the deployment config.
        #[arg(long, env = "NAVIGATOR_GATEWAY_IP")]
        gateway_ip: Option<String>,
        /// Extra `A` host label → gateway IP (repeatable; default `www`,
        /// `workflows`). Use `@` for the apex — an empty `--host ""` does not
        /// survive the command line.
        #[arg(long = "host")]
        hosts: Vec<String>,
        /// Apex `URL` record → `https://www.<domain>` (301 apex → www, via `DNSimple`'s redirector).
        #[arg(long)]
        redirect_apex_to_www: bool,
        /// Apex `MX` `smtp.google.com` + `_spf.google.com` in SPF (Google Workspace).
        #[arg(long)]
        google_workspace: bool,
        /// Apex `google-site-verification` token.
        #[arg(long)]
        google_site_verification: Option<String>,
        /// `parse.` subdomain `MX` `mx.sendgrid.net` + `sendgrid.net` in SPF.
        #[arg(long)]
        sendgrid: bool,
        /// `SendGrid` `DKIM` `CNAME` target (repeatable → `s1._domainkey`, `s2._domainkey`, … in order).
        #[arg(long = "dkim-target")]
        dkim_targets: Vec<String>,
        /// `SendGrid` link-branding `CNAME` as `label=target` (repeatable).
        #[arg(long = "sendgrid-link-brand", value_parser = parse_label_target)]
        sendgrid_link_brand: Vec<(String, String)>,
        /// Extra SPF `include:` mechanism (repeatable; e.g. `amazonses.com`).
        #[arg(long = "spf-include")]
        spf_includes: Vec<String>,
        /// `DMARC` policy; adds a `_dmarc` `TXT` record.
        #[arg(long, value_enum)]
        dmarc: Option<dns::DmarcPolicy>,
        /// `DMARC` report address (default `mailto:postmaster@<domain>`).
        #[arg(long)]
        dmarc_rua: Option<String>,
        /// Preview the planned record changes without applying them (the default).
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
        /// Apply the planned DNS record and certificate changes.
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
    },
}

/// One provider seam for DNS setup. `DNSimple` implements both record
/// reconciliation and its apex-certificate lifecycle; tests inject a fake
/// through the same dispatch path.
trait DnsSetupProvider: dns::DnsProvider + dns::CertificateProvider {}

impl<T> DnsSetupProvider for T where T: dns::DnsProvider + dns::CertificateProvider {}

/// clap value parser for `--sendgrid-link-brand label=target`. Both sides
/// must be non-empty — an empty label or target is an invalid CNAME and is
/// rejected at the CLI boundary rather than failing later as a provider error.
fn parse_label_target(raw: &str) -> std::result::Result<(String, String), String> {
    match raw.split_once('=') {
        Some((label, target)) => {
            let label = label.trim();
            let target = target.trim();
            if label.is_empty() || target.is_empty() {
                return Err(format!(
                    "expected `label=target` with a non-empty label and target, got `{raw}`"
                ));
            }
            Ok((label.to_string(), target.to_string()))
        }
        _ => Err(format!(
            "expected `label=target` with a non-empty label and target, got `{raw}`"
        )),
    }
}

// Clap materializes this only while parsing one operator command. Keeping the
// fields together makes `ops gcp setup --help` and the dispatcher auditable;
// heap indirection on individual options would obscure that contract.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
pub enum GcpCmd {
    /// Provision network, five GCS buckets, and a GKE Autopilot
    /// cluster in the given Google Cloud project. Safe to re-run:
    /// every step ignores "already exists" responses.
    Setup {
        /// Google Cloud project ID (e.g. `your-project-id`). Falls back to
        /// `NAVIGATOR_GCP_PROJECT_ID` so coordinates exported from a
        /// deployment's `config.toml` can carry the complete target.
        #[arg(long, env = "NAVIGATOR_GCP_PROJECT_ID")]
        project_id: String,
        /// Canonical HTTPS origin allowed to fetch public assets. Falls back
        /// to `NAV_BASE_URL` and must not include a path.
        #[arg(long, env = "NAV_BASE_URL")]
        public_base_url: String,
        /// Region for GKE Autopilot and bucket location.
        /// Falls back to `NAVIGATOR_GCP_LOCATION` then to the
        /// workspace default.
        #[arg(long, env = "NAVIGATOR_GCP_LOCATION")]
        region: Option<String>,
        /// GKE Autopilot cluster name. Falls back to
        /// `NAVIGATOR_GKE_CLUSTER_NAME`.
        #[arg(long, env = "NAVIGATOR_GKE_CLUSTER_NAME")]
        cluster_name: Option<String>,
        /// VPC network name. Falls back to `NAVIGATOR_VPC_NAME`.
        #[arg(long, env = "NAVIGATOR_VPC_NAME")]
        vpc_name: Option<String>,
        /// Regional GKE subnetwork name. Falls back to
        /// `NAVIGATOR_SUBNETWORK_NAME`.
        #[arg(long, env = "NAVIGATOR_SUBNETWORK_NAME")]
        subnetwork_name: Option<String>,
        /// Reserved global static-IP name. Falls back to
        /// `NAVIGATOR_GATEWAY_IP_NAME`.
        #[arg(long, env = "NAVIGATOR_GATEWAY_IP_NAME")]
        gateway_ip_name: Option<String>,
        /// HTTPS URL of the Git repo `Config Sync` should reconcile
        /// from. Falls back to `NAVIGATOR_CONFIG_SYNC_REPO`. Omit to
        /// skip the `RootSync` step entirely — sensible for forks not
        /// running `GitOps` yet.
        #[arg(long, env = "NAVIGATOR_CONFIG_SYNC_REPO")]
        config_sync_repo: Option<String>,
        /// Path inside the repo Config Sync should watch. Falls back
        /// to `NAVIGATOR_CONFIG_SYNC_DIR`.
        #[arg(long, env = "NAVIGATOR_CONFIG_SYNC_DIR")]
        config_sync_dir: Option<String>,
        /// Artifact Registry repository that holds every container image.
        /// Falls back to `NAVIGATOR_GAR_REPO` then the workspace default
        /// (`navigator`). A fork overrides it so its images and IAM
        /// bindings target its own repository.
        #[arg(long, env = "NAVIGATOR_GAR_REPO")]
        artifact_registry_repo: Option<String>,
        /// `owner/repo` slug the Workload Identity provider trusts for
        /// keyless CI pushes. Falls back to `NAVIGATOR_GITHUB_REPO` then
        /// the workspace default (`neon-law/navigator`). A fork
        /// MUST set this to its own slug, or its GitHub Actions workflow
        /// cannot impersonate the CI pusher service account.
        #[arg(long, env = "NAVIGATOR_GITHUB_REPO")]
        github_repo: Option<String>,
        /// Account id (local part of the SA email) of the CI pusher
        /// service account. Falls back to `NAVIGATOR_CI_PUSHER_ACCOUNT_ID`
        /// then the workspace default (`navigator-ci-pusher`).
        #[arg(long, env = "NAVIGATOR_CI_PUSHER_ACCOUNT_ID")]
        ci_pusher_account_id: Option<String>,
        /// Hub project holding the shared Artifact Registry (for this
        /// workspace, `ghcr`). When set, this environment gets no
        /// registry, CI pusher, or Workload Identity pool of its own — it
        /// only takes `roles/artifactregistry.reader` on the hub repository,
        /// which `ops gcp hub setup` provisioned. Falls back to
        /// `NAVIGATOR_IMAGES_PROJECT_ID`. Omit for the single-project shape.
        #[arg(long, env = "NAVIGATOR_IMAGES_PROJECT_ID")]
        images_project_id: Option<String>,
        /// Public assets bucket for this deployment. Falls back to
        /// `<project-id>-assets`.
        #[arg(long, env = "NAVIGATOR_ASSETS_BUCKET")]
        assets_bucket: Option<String>,
        /// Private documents bucket for this deployment. Falls back to
        /// `<project-id>-documents`.
        #[arg(long, env = "NAVIGATOR_DOCUMENTS_BUCKET")]
        documents_bucket: Option<String>,
        /// Nightly Parquet/Iceberg exports bucket for this deployment. Falls
        /// back to `<project-id>-exports`.
        #[arg(long, env = "NAVIGATOR_EXPORTS_BUCKET")]
        exports_bucket: Option<String>,
        /// Audit-log archive bucket for this deployment. Falls back to
        /// `<project-id>-logs`.
        #[arg(long, env = "NAVIGATOR_LOGS_BUCKET")]
        logs_bucket: Option<String>,
        /// Private applications bucket. It once held each Project's published
        /// Portal bundle; nothing publishes one any more. Falls back to
        /// `<project-id>-applications`.
        #[arg(long, env = "NAVIGATOR_APPLICATIONS_BUCKET")]
        applications_bucket: Option<String>,
        /// The GHE organization that owns this deployment's Project
        /// repositories — the one `repository_owner` the deployment's
        /// Workload Identity provider admits. Falls back to
        /// `NAVIGATOR_GITHUB_ORG`.
        #[arg(long, env = "NAVIGATOR_GITHUB_ORG")]
        applications_publisher_org: Option<String>,
        /// Long-term Iceberg archive of this deployment's Surreal store
        /// (`<archives-prefix><deployment>`). No lifecycle rule: this is where
        /// long-term storage lives. Omit to skip the lane entirely — there is
        /// no derived default, because the name is prefix-shaped rather than
        /// `<project-id>-`-shaped.
        #[arg(long, env = "NAVIGATOR_ARCHIVES_BUCKET")]
        archives_bucket: Option<String>,
        /// Private Project Recovery Archive bucket
        /// (`<deployment>-archived-projects`). Public access prevention is
        /// enforced and no lifecycle rule is set: retention is the governed
        /// purge's decision, never bucket age. Omit to skip the lane.
        #[arg(long, env = "NAVIGATOR_ARCHIVED_PROJECTS_BUCKET")]
        archived_projects_bucket: Option<String>,
        /// GCP project id that holds the offsite Iceberg archive bucket —
        /// a different project than the deployment's, so the live workload's
        /// blast radius cannot reach the archive. The bucket is created with a
        /// locked 25-year retention policy and public access prevention
        /// enforced; the deployment's workflows-service identity gets create
        /// and read roles on it and nothing else. Requires
        /// `--offsite-archives-bucket`. Falls back to
        /// `NAVIGATOR_OFFSITE_ARCHIVES_PROJECT_ID`.
        #[arg(
            long,
            env = "NAVIGATOR_OFFSITE_ARCHIVES_PROJECT_ID",
            requires_all = ["offsite_archives_bucket"]
        )]
        offsite_archives_project_id: Option<String>,
        /// Offsite Iceberg archive bucket name, created in the project
        /// `--offsite-archives-project-id` names with a locked 25-year
        /// retention policy and no lifecycle rule. No derived default: the
        /// name is an operator choice in another project. Requires
        /// `--offsite-archives-project-id`. Falls back to
        /// `NAVIGATOR_OFFSITE_ARCHIVES_BUCKET`.
        #[arg(
            long,
            env = "NAVIGATOR_OFFSITE_ARCHIVES_BUCKET",
            requires_all = ["offsite_archives_project_id"]
        )]
        offsite_archives_bucket: Option<String>,
        /// GCP project id that holds the locked Git bundle archive bucket —
        /// a different project than the deployment's, like the offsite
        /// archive. Requires `--forgejo-archives-bucket`. Falls back to
        /// `NAVIGATOR_FORGEJO_ARCHIVES_PROJECT_ID`.
        #[arg(
            long,
            env = "NAVIGATOR_FORGEJO_ARCHIVES_PROJECT_ID",
            requires_all = ["forgejo_archives_bucket"]
        )]
        forgejo_archives_project_id: Option<String>,
        /// Git bundle archive bucket name (`<deployment>-bundles`), created in
        /// the project `--forgejo-archives-project-id` names with a locked
        /// 7-year retention policy and no lifecycle rule; `hudson ops
        /// forgejo bundle archive` and the nightly repository backup write
        /// here, and the deployment identity gets create and read roles only.
        /// Requires `--forgejo-archives-project-id`. Falls back to
        /// `NAVIGATOR_FORGEJO_ARCHIVES_BUCKET`, the same variable the runtime
        /// reads the lane from.
        #[arg(
            long,
            env = "NAVIGATOR_FORGEJO_ARCHIVES_BUCKET",
            requires_all = ["forgejo_archives_project_id"]
        )]
        forgejo_archives_bucket: Option<String>,
        /// Telemetry landing zone (`<deployment>-telemetry`), where the `OTel`
        /// collector writes Parquet before the nightly lane promotes it into
        /// the archive. Provisioned with a flat 90-day object expiry. Omit to
        /// skip; no derived default, because the project may be named for the
        /// entity rather than the deployment.
        #[arg(long, env = "NAVIGATOR_TELEMETRY_BUCKET")]
        telemetry_bucket: Option<String>,
        /// Per-deployment Google service-account id used by GKE Workload
        /// Identity. Falls back to `navigator-web` for the single-project
        /// OSS shape.
        #[arg(long, env = "NAVIGATOR_GCP_SERVICE_ACCOUNT_ID")]
        google_service_account_id: Option<String>,
        /// Dedicated per-deployment Google service-account id used only for
        /// the delegated Workspace Drive credential. Falls back to
        /// `navigator-drive` for the single-project OSS shape.
        #[arg(long, env = "NAVIGATOR_DRIVE_GCP_SERVICE_ACCOUNT_ID")]
        drive_service_account_id: Option<String>,
        /// Kubernetes namespace containing the runtime service accounts.
        #[arg(long, env = "NAVIGATOR_K8S_NAMESPACE")]
        kubernetes_namespace: Option<String>,
        /// Deployment row the release-ship identity reads
        /// `navigator-deployment-config-<row>` for. With `--ship-repository`
        /// and `--ship-environment`, setup grants `navigator-release-ship`
        /// what `ops ship` uses and creates its GitHub Workload Identity
        /// provider. No environment fallback: the row is never inferred.
        #[arg(long, requires_all = ["ship_repository", "ship_environment"])]
        ship_deployment: Option<String>,
        /// `owner/repo` whose workflow runs the ship; the provider admits
        /// only this repository.
        #[arg(long, requires_all = ["ship_deployment", "ship_environment"])]
        ship_repository: Option<String>,
        /// GitHub Environment the ship job runs in (e.g. `staging-deploy`);
        /// the provider admits only this environment on `main`.
        #[arg(long, requires_all = ["ship_deployment", "ship_repository"])]
        ship_environment: Option<String>,
        /// Explicit operator IAM member (user:, serviceAccount:, or group:)
        /// granted SOPS encrypt/decrypt on the deployment KMS key alone.
        /// Omit to leave operator IAM unchanged; no identity is inferred.
        #[arg(long)]
        kms_operator: Option<String>,
        /// Preview the GCP API calls that would be made, without
        /// sending any traffic. `gcloud` has no universal
        /// equivalent of this flag, so we provide one ourselves.
        #[arg(long)]
        dry_run: bool,
    },
    /// Provision the shared image hub — the Artifact Registry every
    /// environment pulls from, the CI pusher service account, and the GitHub
    /// Workload Identity pool — and nothing else. The hub is not an
    /// environment: this command never creates buckets, GKE, or IAP
    /// resources, and it refuses a project ID recorded as an environment.
    #[command(subcommand)]
    Hub(GcpHubCmd),
    /// Converge the project's secret-access posture onto an `access.toml`:
    /// Data Access audit logs, the CMEK key Secret Manager encrypts with,
    /// and each Deployment Reader's read-only roles and per-secret access.
    ///
    /// The file lives beside the deployment row in the private deployments
    /// tree. `--check` writes nothing and exits nonzero on drift.
    Access {
        /// Google Cloud project ID. Falls back to `NAVIGATOR_GCP_PROJECT_ID`.
        #[arg(long, env = "NAVIGATOR_GCP_PROJECT_ID")]
        project_id: String,
        /// The `access.toml` to converge onto.
        #[arg(long)]
        file: std::path::PathBuf,
        /// Report drift without writing; exit nonzero when any exists.
        #[arg(long)]
        check: bool,
    },
    /// Report required APIs that are disabled on the project.
    ///
    /// Compares the APIs `ops gcp setup` enables with the APIs the project
    /// actually has enabled, `gmail.googleapis.com` among them.
    /// Read-only: it lists the project's enabled services and enables
    /// nothing. Exits nonzero when any required API is disabled.
    Services {
        /// Google Cloud project ID. Falls back to `NAVIGATOR_GCP_PROJECT_ID`.
        #[arg(long, env = "NAVIGATOR_GCP_PROJECT_ID")]
        project_id: String,
    },
    /// Identity-Aware Proxy operations for the `navigator-web`
    /// backend. Run after the GKE Ingress has provisioned the LB.
    #[command(subcommand)]
    Iap(IapCmd),
}

#[derive(Subcommand)]
pub enum GcpHubCmd {
    /// Create the Artifact Registry repository and its cleanup policy, the CI
    /// pusher service account and its repo-scoped writer binding, and the
    /// GitHub Workload Identity pool, provider, and impersonation binding.
    /// Safe to re-run: every step treats "already exists" as success.
    Setup {
        /// Google Cloud project that holds the shared registry. For this
        /// workspace, `ghcr`. Refused if it is one of the four
        /// environment projects — see `docs/environments.md`.
        #[arg(long)]
        project_id: String,
        /// Artifact Registry location. Falls back to
        /// `NAVIGATOR_GCP_LOCATION` then the workspace default.
        #[arg(long, env = "NAVIGATOR_GCP_LOCATION")]
        region: Option<String>,
        /// Repository name that holds every container image. Falls back to
        /// `NAVIGATOR_GAR_REPO` then the workspace default (`navigator`).
        #[arg(long, env = "NAVIGATOR_GAR_REPO")]
        artifact_registry_repo: Option<String>,
        /// `owner/repo` slug the Workload Identity provider trusts for
        /// keyless CI pushes. Falls back to `NAVIGATOR_GITHUB_REPO` then the
        /// workspace default. A fork MUST set this to its own slug.
        #[arg(long, env = "NAVIGATOR_GITHUB_REPO")]
        github_repo: Option<String>,
        /// Account id (local part of the SA email) of the CI pusher service
        /// account. Falls back to `NAVIGATOR_CI_PUSHER_ACCOUNT_ID`.
        #[arg(long, env = "NAVIGATOR_CI_PUSHER_ACCOUNT_ID")]
        ci_pusher_account_id: Option<String>,
        /// Print the exact hub plan without authenticating or contacting GCP.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub enum IapCmd {
    /// Print the IAP audience string `portal::iap::IapConfig` validates
    /// against. Format:
    /// `/projects/<PROJECT_NUMBER>/global/backendServices/<SERVICE_ID>`.
    /// Paste the value into `IAP_AUDIENCE` in your GKE overlay
    /// (the example overlay lives at
    /// `examples/deploy/k8s/gke/patches/web-env.yaml`).
    Audience {
        /// Google Cloud project ID (e.g. `your-project-id`).
        #[arg(long)]
        project_id: String,
        /// Compute backend-service name. Defaults to `navigator-web`,
        /// matching the example GKE overlay.
        #[arg(long, default_value = gcp::iap::DEFAULT_SERVICE_NAME)]
        service: String,
    },
    /// Add `--member` to `roles/iap.httpsResourceAccessor` on the
    /// IAP-protected backend service. Idempotent: a no-op when the
    /// principal is already bound. Accepted member formats:
    /// `user:libra@example.com`, `group:lawyer@example.com`,
    /// `serviceAccount:s@p.iam.gserviceaccount.com`, or a bare
    /// OAuth client ID like `12345-abc.apps.googleusercontent.com`.
    Grant {
        /// Google Cloud project ID (e.g. `your-project-id`).
        #[arg(long)]
        project_id: String,
        /// Principal to allow past IAP. See command docstring for
        /// supported formats.
        #[arg(long)]
        member: String,
        /// Compute backend-service name. Defaults to `navigator-web`.
        #[arg(long, default_value = gcp::iap::DEFAULT_SERVICE_NAME)]
        service: String,
    },
}

/// Dispatch the local `dev` and operator `ops` subsets of the `navigator` CLI.
/// `main` loads `.env` + `.devx/env` before parsing ordinary local commands,
/// so this only resolves the KIND config and routes. Deployment-scoped
/// commands read the `deployments/` tree through their explicit
/// `--deployment` flag instead. Non-orchestration commands never reach here.
///
/// One big match over every subcommand — readability comes from the flat
/// dispatch, not from splitting it across helpers.
#[allow(clippy::too_many_lines)]
pub fn dispatch(command: crate::Command) -> Result<()> {
    dispatch_with_dns_provider(command, None)
}

#[allow(clippy::too_many_lines)] // Keep the local CLI routes beside the production dispatch.
fn dispatch_with_dns_provider(
    command: crate::Command,
    dns_provider: Option<&dyn DnsSetupProvider>,
) -> Result<()> {
    if let crate::Command::Dev(crate::DevCmd::SendgridOpenapi {
        verify,
        regenerate,
        root,
    }) = &command
    {
        if *regenerate {
            return crate::sendgrid_openapi::regenerate(root);
        }
        let _ = verify;
        return crate::sendgrid_openapi::verify(root);
    }
    let cfg = KindConfig::from_env();
    match command {
        crate::Command::Dev(crate::DevCmd::Up { path }) => up(path.as_deref(), &cfg),
        crate::Command::Dev(crate::DevCmd::Down { path }) => down(path.as_deref(), &cfg),
        crate::Command::Dev(crate::DevCmd::Env) => print_env(&cfg),
        crate::Command::Dev(crate::DevCmd::Status) => {
            status(&cfg);
            Ok(())
        }
        crate::Command::Dev(crate::DevCmd::Serve) => serve::run(),
        crate::Command::Quickstart => quickstart::run(&cfg),
        crate::Command::Dev(crate::DevCmd::BuildWebapp { release }) => webapp::build(release),
        crate::Command::Dev(crate::DevCmd::BuildSameDay { release }) => same_day::build(release),
        crate::Command::Dev(crate::DevCmd::Staging(action)) => staging::dispatch_kind(action, &cfg),
        crate::Command::Dev(crate::DevCmd::Kind(crate::KindCmd::Up)) => kind_up_only(&cfg),
        crate::Command::Dev(crate::DevCmd::Kind(crate::KindCmd::Down)) => kind_down_only(&cfg),
        crate::Command::Dev(crate::DevCmd::Forgejo(crate::ForgejoCmd::Up)) => forgejo::up(&cfg),
        crate::Command::Dev(crate::DevCmd::Forgejo(crate::ForgejoCmd::Smoke {
            receiver_config,
        })) => forgejo::smoke(&cfg, receiver_config.as_deref()),
        crate::Command::Dev(crate::DevCmd::WorktreeEnv(cmd)) => worktree_env::dispatch(cmd, &cfg),
        crate::Command::Dev(crate::DevCmd::Deploy) => {
            worktree_env::up_demo(&workspace_root()?, None, &cfg)
        }
        crate::Command::Dev(crate::DevCmd::Undeploy) => {
            worktree_env::undeploy_demo(&workspace_root()?, &cfg)
        }
        crate::Command::Dev(crate::DevCmd::E2e) => e2e::run_e2e(&cfg),
        crate::Command::Dev(crate::DevCmd::SeaweedfsBootstrap) => seaweedfs::bootstrap(&cfg),
        crate::Command::Dev(crate::DevCmd::GrantLawyer) => e2e::grant_lawyer(&cfg),
        crate::Command::Dev(crate::DevCmd::SampleProject {
            project,
            repo,
            git_ref,
            keep,
        }) => sample_project::run(
            project.as_deref(),
            repo.as_deref(),
            git_ref.as_deref(),
            keep,
        ),
        crate::Command::Dev(crate::DevCmd::BrowserE2e { base_url }) => {
            browser_e2e::run_browser_e2e(base_url.as_deref())
        }
        crate::Command::Dev(crate::DevCmd::FlakeHunt {
            package,
            test_filter,
            runs,
        }) => flake_hunt::run(&package, test_filter.as_deref(), runs),
        crate::Command::Dev(crate::DevCmd::Logs) => logs(&cfg),
        crate::Command::Dev(crate::DevCmd::Kustomize(crate::KustomizeCmd::Kind)) => {
            kustomize_render(&cfg.full_overlay)
        }
        crate::Command::Dev(crate::DevCmd::Kustomize(crate::KustomizeCmd::Gke)) => {
            kustomize_render(&cfg.gke_overlay)
        }
        crate::Command::Ops(crate::OpsCmd::Doctor { namespace }) => {
            doctor::run(namespace.as_deref().unwrap_or(&cfg.namespace))
        }
        crate::Command::Ops(crate::OpsCmd::Forgejo(
            crate::ForgejoPolicyCmd::RenameOrganization(args),
        )) => forgejo::namespace_move::run(&args),
        crate::Command::Ops(crate::OpsCmd::Forgejo(crate::ForgejoPolicyCmd::SshKeys {
            action: crate::ForgejoSshKeysCmd::Sync { person, dry_run },
        })) => crate::ssh_keys::sweep(person.as_deref(), dry_run),
        crate::Command::Ops(crate::OpsCmd::Forgejo(crate::ForgejoPolicyCmd::RunnerOnce(
            options,
        ))) => forgejo::runner::run(&options),
        crate::Command::Ops(crate::OpsCmd::Forgejo(
            crate::ForgejoPolicyCmd::RunnerController {
                url,
                repository,
                runner_namespace,
                context,
                egress_host,
                egress_ip,
                dns_ip,
                interval,
                max_runners,
                once,
            },
        )) => forgejo::controller::run(&forgejo::controller::Options {
            url,
            repository,
            runner_namespace,
            context,
            egress_host,
            egress_ip,
            dns_ip,
            interval: std::time::Duration::from_secs(interval.max(5)),
            max_runners,
            once,
        }),
        crate::Command::Ops(crate::OpsCmd::Forgejo(crate::ForgejoPolicyCmd::QueueObserver {
            url,
            repository,
        })) => forgejo::queue_observer::run(&url, &repository),
        crate::Command::Ops(crate::OpsCmd::Forgejo(crate::ForgejoPolicyCmd::Logs {
            repository,
            run,
            url,
            context,
            namespace,
        })) => forgejo::logs::run(&repository, run, &url, &context, &namespace),
        crate::Command::Ops(crate::OpsCmd::Forgejo(crate::ForgejoPolicyCmd::Release {
            action: crate::ForgejoReleaseCmd::Mirror { url, version },
        })) => forgejo::release::run(&url, &version),
        crate::Command::Ops(crate::OpsCmd::Forgejo(crate::ForgejoPolicyCmd::Bundle {
            mirror,
            output,
        })) => forgejo::bundle::run(&mirror, &output),
        crate::Command::Ops(crate::OpsCmd::Forgejo(crate::ForgejoPolicyCmd::Archive {
            mirror,
            object,
            receipt,
        })) => forgejo::bundle::archive(&mirror, &object, &receipt),
        crate::Command::Ops(crate::OpsCmd::Forgejo(crate::ForgejoPolicyCmd::BackfillClosed {
            host,
            receipts,
        })) => forgejo::bundle::backfill_closed(&host, &receipts),
        crate::Command::Ops(crate::OpsCmd::Forgejo(crate::ForgejoPolicyCmd::Restore {
            receipt,
            output,
        })) => forgejo::bundle::restore(&receipt, &output),
        crate::Command::Ops(crate::OpsCmd::Forgejo(crate::ForgejoPolicyCmd::Setup {
            repository,
            url,
            check_context,
            dry_run,
        })) => forgejo::policy::run(&url, &repository, &check_context, dry_run, None),
        crate::Command::Ops(crate::OpsCmd::Forgejo(crate::ForgejoPolicyCmd::AutoMerge {
            repository,
            url,
            check_context,
            pull_request,
        })) => forgejo::policy::run(&url, &repository, &check_context, false, Some(pull_request)),
        crate::Command::Ops(crate::OpsCmd::Forgejo(crate::ForgejoPolicyCmd::ScheduleOpen {
            repository,
            url,
            check_context,
        })) => forgejo::policy::scheduler::run(&url, &repository, &check_context),
        crate::Command::Ops(crate::OpsCmd::Github(crate::GithubCmd::Setup {
            repository,
            dry_run,
            action_version,
            workflow_home,
        })) => github_setup::run(
            &RepositoryTarget::resolve(repository)?,
            dry_run,
            &action_version,
            &workflow_home,
        ),
        crate::Command::Ops(crate::OpsCmd::Github(crate::GithubCmd::AutoMerge {
            repository,
            pull_request,
            await_app,
        })) => github_auto_merge::run(
            &RepositoryTarget::resolve(repository)?,
            pull_request,
            await_app,
        ),
        crate::Command::Ops(crate::OpsCmd::Github(crate::GithubCmd::CheckSignatures {
            base,
            head,
            git_dir,
        })) => signed_commits::check_range(&git_dir, &base, &head),
        crate::Command::Ops(crate::OpsCmd::Gcp(GcpCmd::Setup {
            project_id,
            public_base_url,
            region,
            cluster_name,
            vpc_name,
            subnetwork_name,
            gateway_ip_name,
            config_sync_repo,
            config_sync_dir,
            artifact_registry_repo,
            github_repo,
            ci_pusher_account_id,
            images_project_id,
            assets_bucket,
            documents_bucket,
            exports_bucket,
            logs_bucket,
            applications_bucket,
            applications_publisher_org,
            archives_bucket,
            archived_projects_bucket,
            telemetry_bucket,
            offsite_archives_project_id,
            offsite_archives_bucket,
            forgejo_archives_project_id,
            forgejo_archives_bucket,
            google_service_account_id,
            drive_service_account_id,
            kubernetes_namespace,
            ship_deployment,
            ship_repository,
            ship_environment,
            kms_operator,
            dry_run,
        })) => {
            let release_ship = match (ship_deployment, ship_repository, ship_environment) {
                (Some(deployment), Some(repository), Some(environment)) => {
                    Some(gcp::release_ship::ReleaseShipConfig {
                        deployment,
                        repository,
                        environment,
                    })
                }
                _ => None,
            };
            let mut config = gcp::SetupConfig {
                kms_operator,
                release_ship,
                public_base_url: Some(public_base_url),
                images_project_id,
                assets_bucket,
                documents_bucket,
                exports_bucket,
                logs_bucket,
                applications_bucket,
                applications_publisher_org,
                archives_bucket,
                archived_projects_bucket,
                telemetry_bucket,
                offsite_archives_project_id,
                offsite_archives_bucket,
                forgejo_archives_project_id,
                forgejo_archives_bucket,
                ..gcp::SetupConfig::default()
            };
            if let Some(v) = google_service_account_id {
                config.google_service_account_id = v;
            }
            if let Some(v) = drive_service_account_id {
                config.drive_service_account_id = v;
            }
            if let Some(v) = kubernetes_namespace {
                config.kubernetes_namespace = v;
            }
            if let Some(v) = region {
                config.region = v;
            }
            if let Some(v) = cluster_name {
                config.cluster_name = v;
            }
            if let Some(v) = vpc_name {
                config.vpc_name = v;
            }
            if let Some(v) = subnetwork_name {
                config.subnetwork_name = v;
            }
            if let Some(v) = gateway_ip_name {
                config.gateway_ip_name = v;
            }
            if let Some(v) = config_sync_repo {
                config.config_sync_repo = Some(v);
            }
            if let Some(v) = config_sync_dir {
                config.config_sync_dir = v;
            }
            if let Some(v) = artifact_registry_repo {
                config.artifact_registry_repo = v;
            }
            if let Some(v) = github_repo {
                config.github_repo = v;
            }
            if let Some(v) = ci_pusher_account_id {
                config.ci_pusher_account_id = v;
            }
            gcp_setup(project_id, dry_run, config)
        }
        crate::Command::Ops(crate::OpsCmd::Gcp(GcpCmd::Hub(GcpHubCmd::Setup {
            project_id,
            region,
            artifact_registry_repo,
            github_repo,
            ci_pusher_account_id,
            dry_run,
        }))) => {
            let mut config = gcp::hub::HubSetupConfig::default();
            if let Some(v) = region {
                config.region = v;
            }
            if let Some(v) = artifact_registry_repo {
                config.artifact_registry_repo = v;
            }
            if let Some(v) = github_repo {
                config.github_repo = v;
            }
            if let Some(v) = ci_pusher_account_id {
                config.ci_pusher_account_id = v;
            }
            gcp_hub_setup(project_id, dry_run, config)
        }
        crate::Command::Ops(crate::OpsCmd::Gcp(GcpCmd::Services { project_id })) => {
            gcp_services(&project_id)
        }
        crate::Command::Ops(crate::OpsCmd::Gcp(GcpCmd::Access {
            project_id,
            file,
            check,
        })) => gcp_access(&project_id, &file, check),
        crate::Command::Ops(crate::OpsCmd::Gcp(GcpCmd::Iap(IapCmd::Audience {
            project_id,
            service,
        }))) => gcp_iap_audience(&project_id, &service),
        crate::Command::Ops(crate::OpsCmd::Gcp(GcpCmd::Iap(IapCmd::Grant {
            project_id,
            member,
            service,
        }))) => gcp_iap_grant(&project_id, &service, &member),
        crate::Command::Ops(crate::OpsCmd::Ship {
            deployment,
            deployments_dir,
            project_id,
            dry_run,
            restart_only,
            image_only,
            tag,
            image_digests,
            assert_signing_iam,
            quiet,
            log_sink,
            certificate_timeout_minutes,
        }) => {
            let opts = ship::ShipOpts {
                deployment,
                deployments_dir,
                project_id,
                dry_run,
                restart_only,
                image_only,
                tag,
                image_digests,
                assert_signing_iam,
                quiet,
                log_sink,
                certificate_timeout_minutes,
            };
            if opts.quiet {
                ship_log::run_quiet(&opts)
            } else {
                ship::run_ship(&opts)
            }
        }
        crate::Command::Ops(crate::OpsCmd::Logs {
            deployment,
            run,
            prefix,
            deployments_dir,
            project_id,
        }) => ship_log::run_logs(
            &deployment,
            &run,
            &prefix,
            deployments_dir.as_deref(),
            project_id.as_deref(),
        ),
        crate::Command::Ops(crate::OpsCmd::Deployments {
            action,
            deployments_dir,
            publish,
        }) => run_deployments(action, deployments_dir.as_deref(), publish),
        crate::Command::Ops(crate::OpsCmd::Secrets(crate::SecretsCmd::Set {
            deployment,
            project_id,
            key,
            service,
            raw,
        })) => {
            let set = key_set(service.as_deref());
            let mut value = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut value)
                .context("read the secret value from stdin")?;
            if !raw {
                strip_one_trailing_newline(&mut value);
            }
            deployment_config::secret_set(&project_id, &deployment, set, &key, &value)
        }
        crate::Command::Ops(crate::OpsCmd::Secrets(crate::SecretsCmd::Unset {
            deployment,
            project_id,
            key,
        })) => deployment_config::secret_unset(&project_id, &deployment, &key),
        crate::Command::Ops(crate::OpsCmd::Secrets(crate::SecretsCmd::ImportSocialChanges {
            deployment,
            deployments_dir,
            request,
        })) => social_key_changes::import(
            &deployments::root(deployments_dir.as_deref())?,
            &deployment,
            &request,
        ),
        crate::Command::Ops(crate::OpsCmd::Secrets(crate::SecretsCmd::Apply {
            deployment,
            service,
            deployments_dir,
            dry_run,
            check,
            project_id,
        })) => {
            let service = service.map(|service| match service.as_str() {
                "postgres" => deployments::ServiceSecrets::Postgres,
                "forgejo" => deployments::ServiceSecrets::Forgejo,
                _ => unreachable!("clap restricts service values"),
            });
            if let Some(project) = project_id {
                let loaded = deployment_config::load(&project, &deployment)?;
                return deployments::apply_from_record(&loaded, service, dry_run);
            }
            let root = deployments::root(deployments_dir.as_deref())?;
            if let Some(service) = service {
                deployments::apply_service(&root, &deployment, service, dry_run)
            } else if check {
                deployments::check_secrets(&root, &deployment)
            } else {
                deployments::apply(&root, &deployment, dry_run)
            }
        }
        crate::Command::Ops(crate::OpsCmd::ProjectSlack(operation)) => {
            project_slack_reconcile::run(operation)
        }
        crate::Command::Ops(crate::OpsCmd::EmailSummary(crate::EmailSummaryCmd::Redrive {
            receipt,
            after_failed_result,
            recovery_id,
            from_recovery,
        })) => email_summary_redrive::run(
            receipt,
            after_failed_result.as_deref(),
            recovery_id,
            from_recovery,
        ),
        crate::Command::Ops(crate::OpsCmd::EmailSummary(crate::EmailSummaryCmd::Health)) => {
            email_summary_delivery::health()
        }
        crate::Command::Ops(crate::OpsCmd::EmailSummary(crate::EmailSummaryCmd::Quarantined)) => {
            email_summary_delivery::quarantined()
        }
        crate::Command::Ops(crate::OpsCmd::EmailSummary(
            crate::EmailSummaryCmd::ReconcileConfirmed {
                receipt,
                actor,
                channel,
                timestamp,
            },
        )) => email_summary_delivery::reconcile_confirmed(receipt, actor, channel, timestamp),
        crate::Command::Ops(crate::OpsCmd::EmailSummary(
            crate::EmailSummaryCmd::AuthorizeResend { receipt, actor },
        )) => email_summary_delivery::authorize_resend(receipt, actor),
        crate::Command::Ops(crate::OpsCmd::Host(HostCmd::Setup {
            host,
            deployment,
            deployments_dir,
            project_id,
            gateway_ip,
            apply,
            timeout_seconds,
        })) => {
            let loaded = if let Some(project) = project_id {
                deployment_config::load(&project, &deployment)?
            } else {
                let root = deployments::root(deployments_dir.as_deref())?;
                deployments::Deployment::load(&root, &deployment)?
            };
            let cfg = ship::ShipConfig::from_deployment(&loaded)
                .with_context(|| format!("resolve ShipConfig for deployment {deployment}"))?;
            host_setup::run(&cfg, &host, gateway_ip, apply, timeout_seconds)
        }
        crate::Command::Ops(crate::OpsCmd::Dns(DnsCmd::Setup {
            domains,
            gateway_ip,
            hosts,
            redirect_apex_to_www,
            google_workspace,
            google_site_verification,
            sendgrid,
            dkim_targets,
            sendgrid_link_brand,
            spf_includes,
            dmarc,
            dmarc_rua,
            dry_run: _,
            apply,
        })) => dns_setup(
            domains,
            &dns::DnsSetupConfig {
                gateway_ip,
                hosts,
                redirect_apex_to_www,
                google_workspace,
                google_site_verification,
                sendgrid,
                dkim_targets,
                sendgrid_link_brand,
                spf_includes,
                dmarc,
                dmarc_rua,
            },
            apply,
            dns_provider,
        ),
        crate::Command::Ops(crate::OpsCmd::CallScript {
            number,
            greeting,
            extensions,
            fallback,
            project_id,
            apply,
        }) => {
            let script = call_script::CallScript {
                greeting,
                extensions,
                fallback,
            };
            let outcome = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("build tokio runtime")?
                .block_on(async {
                    let twilio = match project_id {
                        Some(project_id) => {
                            let client =
                                gcp::client::GcpClient::new(gcp::auth::adc_token_provider().await?);
                            call_script::TwilioStudio::for_deployment(&client, &project_id).await?
                        }
                        None => {
                            call_script::TwilioStudio::from_lookup(|name| std::env::var(name).ok())?
                        }
                    };
                    call_script::reconcile(&twilio, &number, &script, apply).await
                })?;
            println!("{}", outcome.line());
            Ok(())
        }
        crate::Command::Ops(crate::OpsCmd::BrandReadiness {
            deployment,
            deployments_dir,
            project_id,
            timeout_seconds,
        }) => brand_readiness::run_for_deployment(
            &deployment,
            deployments_dir.as_deref(),
            project_id.as_deref(),
            timeout_seconds,
        ),
        crate::Command::Ops(crate::OpsCmd::Certificates(CertificatesCmd::PreRoll {
            deployment,
            deployments_dir,
            project_id,
            timeout_minutes,
            dry_run,
        })) => firm_certificate_roll::pre_roll_for_deployment(
            &deployment,
            deployments_dir.as_deref(),
            project_id.as_deref(),
            timeout_minutes,
            dry_run,
        ),
        crate::Command::Ops(crate::OpsCmd::Rebrand(cmd)) => brand::run(cmd),
        crate::Command::Ops(crate::OpsCmd::Observability {
            deployment,
            deployments_dir,
            project_id,
            dry_run,
        }) => observability::run_observability(&observability::ObservabilityOpts {
            deployment,
            deployments_dir,
            project_id,
            dry_run,
        }),
        // `main` only routes the orchestration subset here; the notation /
        // live-site commands (validate, import, login, …) are handled there.
        _ => unreachable!("devx::dispatch received a non-orchestration command"),
    }
}

/// `ops deployments [init|set|unset|import]`: the published record is the only
/// row source; with no subcommand, check a local tree (or `--publish` it).
fn run_deployments(
    action: Option<crate::DeploymentsCmd>,
    deployments_dir: Option<&std::path::Path>,
    publish: Option<String>,
) -> Result<()> {
    use crate::DeploymentsCmd as Cmd;
    match action {
        None => {
            let root = deployments::root(deployments_dir)?;
            match publish {
                Some(name) => deployment_config::import(&root, &name),
                None => deployments::check(&root),
            }
        }
        Some(Cmd::Import {
            deployment,
            deployments_dir,
        }) => {
            deployment_config::import(&deployments::root(deployments_dir.as_deref())?, &deployment)
        }
        Some(Cmd::Init {
            deployment,
            project_id,
            location,
            github_automation_home,
            sms_test_recipients,
            set,
        }) => deployment_config::init(deployment_config::Init {
            name: deployment,
            project: project_id,
            location,
            github_automation_home,
            sms_test_recipients,
            set: assignments(set)?,
        }),
        Some(Cmd::Set {
            deployment,
            project_id,
            assignments: pairs,
            keys,
        }) => deployment_config::edit(
            &project_id,
            &deployment,
            deployment_config::Edit {
                set: assignments(pairs)?,
                add_keys: key_names(keys),
                ..deployment_config::Edit::default()
            },
        ),
        Some(Cmd::Unset {
            deployment,
            project_id,
            coordinates,
            keys,
        }) => deployment_config::edit(
            &project_id,
            &deployment,
            deployment_config::Edit {
                unset: coordinates.into_iter().collect(),
                remove_keys: key_names(keys),
                ..deployment_config::Edit::default()
            },
        ),
    }
}

/// Collect `KEY=VALUE` pairs, refusing a key named twice.
fn assignments(pairs: Vec<(String, String)>) -> Result<std::collections::BTreeMap<String, String>> {
    let mut out = std::collections::BTreeMap::new();
    for (key, value) in pairs {
        if out.insert(key.clone(), value).is_some() {
            bail!("{key} is assigned more than once");
        }
    }
    Ok(out)
}

fn key_names(keys: crate::SecretKeyNames) -> Vec<(deployment_config::KeySet, String)> {
    use deployment_config::KeySet;
    keys.web
        .into_iter()
        .map(|key| (KeySet::Web, key))
        .chain(keys.postgres.into_iter().map(|key| (KeySet::Postgres, key)))
        .chain(keys.forgejo.into_iter().map(|key| (KeySet::Forgejo, key)))
        .collect()
}

fn key_set(service: Option<&str>) -> deployment_config::KeySet {
    match service {
        Some("postgres") => deployment_config::KeySet::Postgres,
        Some("forgejo") => deployment_config::KeySet::Forgejo,
        _ => deployment_config::KeySet::Web,
    }
}

/// `echo value | hudson ops secrets set` appends a newline the operator
/// did not mean to store. Strip exactly one (`\n` or `\r\n`).
fn strip_one_trailing_newline(value: &mut Vec<u8>) {
    if value.last() == Some(&b'\n') {
        value.pop();
        if value.last() == Some(&b'\r') {
            value.pop();
        }
    }
}

/// `ops dns setup`: reconcile the desired DNS record set on the zone via
/// `DNSimple`. Zone from `--domain` / `DNS_ZONE`; auth from the provider's
/// `DNS_ACCT` + `DNS_SIMPLE` (or legacy `DNSIMPLE_API_TOKEN`). Builds a private Tokio runtime
/// because the DNS provider is async. Preview is the default; `--apply` is
/// the only mode that invokes provider mutation methods.
fn dns_setup(
    domains: Vec<String>,
    config: &dns::DnsSetupConfig,
    apply: bool,
    injected_provider: Option<&dyn DnsSetupProvider>,
) -> Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    let zones = dns_zones(domains)?;
    config.validate().map_err(|msg| anyhow::anyhow!(msg))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    runtime.block_on(async move {
        let default_provider = injected_provider
            .is_none()
            .then(dns::DnsimpleProvider::from_env)
            .transpose()?;
        let provider: &dyn DnsSetupProvider = match (injected_provider, default_provider.as_ref()) {
            (Some(provider), _) => provider,
            (None, Some(provider)) => provider,
            (None, None) => anyhow::bail!("DNS provider was not configured"),
        };
        for zone in &zones {
            let desired = dns::desired_records(zone, config);
            if desired.is_empty() {
                eprintln!(
                    "no record groups selected — pass flags such as --gateway-ip / \
                     --redirect-apex-to-www / --google-workspace / --sendgrid (see \
                     `ops dns setup --help`)"
                );
                return Ok(());
            }
            dns_setup_zone(provider, zone, config, &desired, apply).await?;
        }
        Ok::<(), anyhow::Error>(())
    })
}

async fn dns_setup_zone(
    provider: &dyn DnsSetupProvider,
    zone: &str,
    config: &dns::DnsSetupConfig,
    desired: &[dns::DesiredRecord],
    apply: bool,
) -> Result<()> {
    // Name the zone on every run so `(root)` records remain attributable.
    eprintln!("\n=== {zone} ===");
    let plan = dns::plan_setup(provider, zone, desired)
        .await
        .with_context(|| format!("plan {zone}"))?;
    let certificate_plan = if config.redirect_apex_to_www {
        Some(
            dns::plan_apex_certificate(provider, zone)
                .await
                .with_context(|| format!("plan {zone}'s apex certificate"))?,
        )
    } else {
        None
    };
    print_dns_plan(&plan);
    if config.redirect_apex_to_www {
        eprintln!("PLAN apex certificate reconciliation: enabled");
    }
    if apply {
        apply_dns_plan(provider, zone, &plan, certificate_plan).await?;
    } else {
        eprintln!("PREVIEW: no provider changes applied for {zone}");
    }
    Ok(())
}

fn print_dns_plan(plan: &[dns::PlannedRecord]) {
    for entry in plan {
        let operation = match entry.operation {
            dns::PlannedOperation::Create => "CREATE".to_string(),
            dns::PlannedOperation::Update { record_id } => format!("UPDATE id={record_id}"),
            dns::PlannedOperation::Unchanged => "UNCHANGED".to_string(),
        };
        eprintln!(
            "PLAN {operation:16} {:5} {:20} {} priority={:?} ttl={}",
            entry.desired.record_type.as_str(),
            if entry.desired.name.is_empty() {
                "(root)"
            } else {
                entry.desired.name.as_str()
            },
            entry.desired.content,
            entry.desired.priority,
            entry.desired.ttl,
        );
    }
}

async fn apply_dns_plan(
    provider: &dyn DnsSetupProvider,
    zone: &str,
    plan: &[dns::PlannedRecord],
    certificate_plan: Option<dns::ApexCertificatePlan>,
) -> Result<()> {
    let report = dns::apply_setup(provider, zone, plan)
        .await
        .with_context(|| format!("apply {zone}'s DNS records"))?;
    for entry in &report {
        eprintln!(
            "APPLIED {:5} {:20} {} : {:?}",
            entry.record_type.as_str(),
            if entry.name.is_empty() {
                "(root)"
            } else {
                entry.name.as_str()
            },
            entry.content,
            entry.outcome,
        );
    }
    if let Some(certificate_plan) = certificate_plan {
        let cert_report = dns::apply_apex_certificate(provider, zone, certificate_plan, true)
            .await
            .with_context(|| format!("apply {zone}'s apex certificate plan"))?;
        let result = match cert_report.action {
            dns::ApexCertificateAction::Ordered => "certificate ordered; issuance is pending",
            dns::ApexCertificateAction::IssueRequested => "issuance requested",
            dns::ApexCertificateAction::Unchanged => "certificate unchanged",
            dns::ApexCertificateAction::RemediationNeeded => {
                "certificate needs remediation; inspect DNSimple"
            }
        };
        eprintln!("APPLIED apex certificate operation: {result}");
    }
    Ok(())
}

/// The zones one `ops dns setup` run reconciles, in the order given.
///
/// `--domain` repeated wins; otherwise `DNS_ZONE` supplies the single zone,
/// which keeps the one-domain invocation and its env-var form working
/// unchanged. Blank entries are dropped rather than sent to the provider as
/// an empty zone, and a duplicate is rejected instead of being reconciled
/// twice — a repeated `--domain` in a four-domain command line is a typo, and
/// silently applying that zone twice would double every create.
fn dns_zones(domains: Vec<String>) -> Result<Vec<String>> {
    let mut zones: Vec<String> = domains
        .into_iter()
        .map(|zone| zone.trim().to_string())
        .filter(|zone| !zone.is_empty())
        .collect();
    if zones.is_empty() {
        let from_env = std::env::var("DNS_ZONE").unwrap_or_default();
        let from_env = from_env.trim();
        if !from_env.is_empty() {
            zones.push(from_env.to_string());
        }
    }
    anyhow::ensure!(
        !zones.is_empty(),
        "no domain: pass --domain (repeatable) or set DNS_ZONE"
    );
    for (index, zone) in zones.iter().enumerate() {
        anyhow::ensure!(
            !zones[..index].contains(zone),
            "--domain {zone} given more than once"
        );
    }
    Ok(zones)
}

/// `devx gcp iap audience`: print the IAP audience string for
/// `IAP_AUDIENCE`. Requires the LB to already exist (apply the gke
/// overlay first); a 404 surfaces as a clear error.
/// `ops gcp services`: report which APIs `ops gcp setup` enables are disabled
/// in `project_id`. Read-only; the fix it names is a setup re-run.
fn gcp_access(project_id: &str, file: &std::path::Path, check: bool) -> Result<()> {
    use gcp::access::{reconcile, AccessFile, Mode};
    use gcp::client::GcpClient;

    let text = std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    let declared = AccessFile::parse(&text)?;
    let mode = if check { Mode::Check } else { Mode::Apply };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    runtime.block_on(async move {
        let client = GcpClient::new(gcp::auth::adc_token_provider().await?);
        let drift = reconcile(&client, project_id, &declared, mode).await?;
        for line in &drift {
            println!("{line}");
        }
        match (mode, drift.is_empty()) {
            (_, true) => {
                println!("{project_id}: access converged");
                Ok(())
            }
            (Mode::Apply, false) => {
                println!("{project_id}: {} change(s) applied", drift.len());
                Ok(())
            }
            (Mode::Check, false) => Err(anyhow::anyhow!(
                "{project_id}: {} access change(s) pending",
                drift.len()
            )),
        }
    })
}

fn gcp_services(project_id: &str) -> Result<()> {
    use std::sync::Arc;

    use gcp::client::{GcpClient, TokenProvider};

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    runtime.block_on(async move {
        let token: Arc<dyn TokenProvider> = gcp::auth::adc_token_provider().await?;
        let client = GcpClient::new(token);
        let required = gcp::services::REQUIRED_SERVICES;
        let missing = gcp::services::missing(&client, project_id, required)
            .await
            .context("list the project's enabled APIs")?;
        print!(
            "{}",
            gcp::services::drift_report(project_id, required, &missing)
        );
        if missing.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "{} required API(s) disabled in {project_id}",
                missing.len()
            ))
        }
    })
}

fn gcp_iap_audience(project_id: &str, service: &str) -> Result<()> {
    use std::sync::Arc;

    use gcp::client::{GcpClient, TokenProvider};

    tracing_subscriber::fmt::try_init().ok();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    runtime.block_on(async move {
        let token: Arc<dyn TokenProvider> = gcp::auth::adc_token_provider().await?;
        let client = GcpClient::new(token);
        let project_number = gcp::iap::get_project_number(&client, project_id)
            .await
            .context("look up project number")?;
        let svc_id = gcp::iap::get_backend_service_id(&client, project_id, service)
            .await
            .with_context(|| {
                format!(
                    "look up backend service `{service}` (apply k8s/overlays/gke first \
and wait for the GKE Ingress controller to provision the LB)"
                )
            })?;
        let audience = gcp::iap::format_iap_audience(&project_number, &svc_id);
        // The audience string is the only thing the operator pastes
        // into web-env.yaml — print to stdout so it can be captured.
        println!("{audience}");
        eprintln!(
            "==> IAP_AUDIENCE for {service} in {project_id}: paste into \
k8s/overlays/gke/patches/web-env.yaml then kubectl apply -k k8s/overlays/gke"
        );
        Ok::<(), anyhow::Error>(())
    })
}

/// `devx gcp iap grant`: add a principal to `roles/iap.httpsResourceAccessor`
/// on the IAP-protected backend service. Safe to re-run — checks the
/// existing policy and skips setIamPolicy when the binding is already there.
fn gcp_iap_grant(project_id: &str, service: &str, member: &str) -> Result<()> {
    use std::sync::Arc;

    use gcp::client::{GcpClient, TokenProvider};
    use gcp::iap::BindingOutcome;

    tracing_subscriber::fmt::try_init().ok();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    runtime.block_on(async move {
        let token: Arc<dyn TokenProvider> = gcp::auth::adc_token_provider().await?;
        let client = GcpClient::new(token);
        let project_number = gcp::iap::get_project_number(&client, project_id)
            .await
            .context("look up project number")?;
        let outcome = gcp::iap::ensure_iap_iam_binding(&client, &project_number, service, member)
            .await
            .with_context(|| format!("bind {member} on {service}"))?;
        match outcome {
            BindingOutcome::Added => {
                eprintln!("==> added {member} to roles/iap.httpsResourceAccessor on {service}");
            }
            BindingOutcome::AlreadyPresent => {
                eprintln!("==> {member} already bound on {service} (no change)");
            }
        }
        Ok::<(), anyhow::Error>(())
    })
}

fn gcp_setup(project_id: String, dry_run: bool, config: gcp::SetupConfig) -> Result<()> {
    use std::sync::Arc;

    use gcp::client::{GcpClient, StaticToken, TokenProvider};

    tracing_subscriber::fmt::try_init().ok();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    runtime.block_on(async move {
        // Dry-run uses a stub token — we never authenticate.
        let token: Arc<dyn TokenProvider> = if dry_run {
            Arc::new(StaticToken("dry-run".into()))
        } else {
            gcp::auth::adc_token_provider().await?
        };
        let mut client = GcpClient::new(token);
        if dry_run {
            client = client.with_dry_run();
        }
        gcp::run(&client, &project_id, &config).await?;
        if dry_run {
            eprintln!(
                "--- dry run: {} call(s) would be made ---",
                client.recorded_calls().len()
            );
            for call in client.recorded_calls() {
                eprintln!("{} {}", call.method, call.url);
                if let Some(body) = call.body {
                    eprintln!("  {body}");
                }
            }
        } else {
            tracing::info!(project = %project_id, "setup complete");
        }
        Ok::<(), anyhow::Error>(())
    })
}

/// `ops gcp hub setup --project-id <ID> [--dry-run]`: provision the shared
/// image hub. Uses its own typed configuration and pipeline, so no flag on
/// this command can reach the environment resources — buckets, GKE, IAP —
/// that the hub must never hold.
fn gcp_hub_setup(
    project_id: String,
    dry_run: bool,
    config: gcp::hub::HubSetupConfig,
) -> Result<()> {
    use std::sync::Arc;

    use gcp::client::{GcpClient, StaticToken, TokenProvider};

    // Refuse an environment project before attempting ADC authentication or
    // any cloud operation. `gcp::hub::run` validates again for direct callers.
    gcp::tenants::validate_target(gcp::tenants::TenantRole::Hub, &project_id)?;
    tracing_subscriber::fmt::try_init().ok();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    runtime.block_on(async move {
        // Dry-run uses a stub token — we never authenticate.
        let token: Arc<dyn TokenProvider> = if dry_run {
            Arc::new(StaticToken("dry-run".into()))
        } else {
            gcp::auth::adc_token_provider().await?
        };
        let mut client = GcpClient::new(token);
        if dry_run {
            client = client.with_dry_run();
        }
        gcp::hub::run(&client, &project_id, &config).await?;
        if dry_run {
            eprintln!(
                "--- dry run: {} call(s) would be made ---",
                client.recorded_calls().len()
            );
            for call in client.recorded_calls() {
                eprintln!("{} {}", call.method, call.url);
                if let Some(body) = call.body {
                    eprintln!("  {body}");
                }
            }
        } else {
            tracing::info!(project = %project_id, "hub setup complete");
        }
        Ok::<(), anyhow::Error>(())
    })
}

/// Nested Docker (Cursor Cloud `DinD`) cannot load `xt_multiport` or
/// `xt_statistic`, so kube-proxy's default iptables mode never programs
/// `ClusterIP`/`NodePort` rules. KIND's `kubeProxyMode: nftables` uses the
/// in-node `nft` binary instead.
fn inject_kube_proxy_nftables(yaml: &str) -> String {
    if yaml
        .lines()
        .any(|line| line.trim_start().starts_with("kubeProxyMode:"))
    {
        return yaml.to_string();
    }
    let needle = "apiVersion: kind.x-k8s.io/v1alpha4\n";
    let Some(idx) = yaml.find(needle) else {
        return yaml.to_string();
    };
    let insert_at = idx + needle.len();
    let mut out = String::with_capacity(yaml.len() + 48);
    out.push_str(&yaml[..insert_at]);
    out.push_str("networking:\n  kubeProxyMode: nftables\n");
    out.push_str(&yaml[insert_at..]);
    out
}

/// Substitute configurable `hostPort:` values in a `kind-config.yaml` body.
/// The container ports stay fixed, so Service manifests remain in sync. At
/// default ports the output is byte-identical to the input.
fn render_kind_config(template: &str, cfg: &KindConfig) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(template.lines().count());
    // The `hostPort:` line follows its `containerPort:` line; track
    // which mapping we're inside so we rewrite the right host port.
    let mut pending: Option<u16> = None;
    for line in template.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("- containerPort:") {
            pending = if trimmed.contains("containerPort: 80") {
                Some(cfg.ingress_http_port)
            } else if trimmed.contains("containerPort: 443") {
                Some(cfg.ingress_https_port)
            } else if trimmed.contains("containerPort: 31532") {
                Some(cfg.postgres_port)
            } else if trimmed.contains(&format!("containerPort: {DEFAULT_RAUTHY_HOST_PORT}")) {
                Some(cfg.rauthy_port)
            } else {
                None
            };
            lines.push(line.to_string());
        } else if trimmed.starts_with("hostPort:") {
            match pending.take() {
                Some(port) => {
                    let indent = &line[..line.len() - trimmed.len()];
                    lines.push(format!("{indent}hostPort: {port}"));
                }
                None => lines.push(line.to_string()),
            }
        } else {
            lines.push(line.to_string());
        }
    }
    let mut out = lines.join("\n");
    if template.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// The browser-facing Rauthy origin for a local tier.
///
/// This is the one origin a browser can actually reach: KIND maps
/// `cfg.rauthy_port` straight through to the Rauthy container
/// (`render_kind_config` above rewrites that mapping per worktree), and
/// [`render_env`] derives `OAUTH_ISSUER_URL` from the same port. Deriving both
/// from `cfg` is what keeps the issuer host-side `web` is configured against
/// and the endpoint it redirects the browser to from drifting apart.
fn rauthy_origin(rauthy_port: u16) -> String {
    format!("http://localhost:{rauthy_port}")
}

fn rauthy_issuer(rauthy_port: u16) -> String {
    format!("{}/auth/v1/", rauthy_origin(rauthy_port))
}

/// Merge patch pointing Rauthy's public URL and `WebAuthn` origin at this tier's
/// browser-reachable port.
///
/// Rauthy intentionally has one canonical URL for discovery endpoints, token
/// issuers, and browser redirects. Host-run web reaches the mapped `NodePort`
/// directly; full in-cluster web uses the loopback proxy rendered below.
fn rauthy_public_url_patch(rauthy_port: u16) -> String {
    let authority = format!("localhost:{rauthy_port}");
    format!(
        r#"{{"stringData":{{"PUB_URL":"{authority}","RP_ORIGIN":"{origin}"}}}}"#,
        authority = authority,
        origin = rauthy_origin(rauthy_port),
    )
}

/// Nginx config for the full-stack pod's Rauthy loopback bridge.
///
/// The browser and the in-cluster web process must call the identical issuer.
/// A worktree changes the host-side `NodePort`, so the proxy listen port and Host
/// header follow that same derived value.
fn rauthy_loopback_config(rauthy_port: u16) -> String {
    format!(
        "worker_processes 1;\n\
         error_log /dev/stderr warn;\n\
         \n\
         events {{\n\
           worker_connections 128;\n\
         }}\n\
         \n\
         http {{\n\
           access_log /dev/stdout combined;\n\
           server {{\n\
             listen 127.0.0.1:{rauthy_port};\n\
             location / {{\n\
               proxy_http_version 1.1;\n\
               proxy_set_header Host localhost:{rauthy_port};\n\
               proxy_pass http://rauthy:8080;\n\
             }}\n\
           }}\n\
         }}\n"
    )
}

fn rauthy_loopback_config_patch(rauthy_port: u16) -> String {
    serde_json::json!({
        "data": {
            "nginx.conf": rauthy_loopback_config(rauthy_port),
        }
    })
    .to_string()
}

// ---------- helpers ----------

/// Single-quote a `.devx/env` value so both a POSIX shell `source` and a
/// `dotenvy` parse read it literally. Worktree roots can contain spaces (and
/// other shell-significant characters); an unquoted `KUBECONFIG=/my path/…`
/// truncates at the first space when the file is sourced, so a destructive
/// `hudson dev staging reset` would silently fall back to the ambient
/// kubeconfig instead of the task-owned cluster. Single quotes are literal in
/// both consumers (unlike double quotes, which still expand `$`); a directory
/// containing apostrophes remains literal in both shell and dotenv readers.
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn render_env(cfg: &KindConfig, root: &Path) -> String {
    render_env_for(cfg, "navigator", cfg.web_port, root)
}

/// Like [`render_env`] but parameterized by the store database name and
/// host `web` port. The ordinary loop and each worktree tier use `navigator`
/// within their own KIND cluster, threaded through the same renderer so they
/// cannot drift in how they wire `web` to the worker.
/// `root` is the workspace (or worktree) root; the per-Project git repos
/// land under its `.devx/repos/<db_name>`, keyed by database so parallel
/// worktrees never share a repo volume.
#[allow(clippy::too_many_lines)]
fn render_env_for(cfg: &KindConfig, db_name: &str, web_port: u16, root: &Path) -> String {
    // The canonical seed is embedded from `store/seeds/*.yaml` and is
    // applied unconditionally and idempotently by `web` at startup.
    //
    // Host-side `web` runs `enforce_deployment_invariants` for the dev
    // profile. SENDGRID_API_KEY /
    // NAVIGATOR_CI_HARNESS explicitly authorizes the fake integration
    // values emitted by this automated KIND fixture. A normal dev
    // deployment supplies real non-production credentials instead.
    // `NAVIGATOR_GIT_REPO_ROOT` is where `repos::RepoStore` keeps each
    // Project's bare repo. It is optional — `portal::matter_documents`
    // returns `Ok(None)` on `RepoError::RootUnset`, and no web boot
    // invariant requires it (`store::deployment::WEB_REQUIREMENTS`) — but
    // host-side `web` sets it so the repo-writing paths stay exercised
    // locally while `repos::RepoStore` lives (ENG-108).
    // `.devx/env` is the complete task-local application environment. Keep
    // the orchestration coordinates alongside the host application's values:
    // a command such as `hudson dev deploy` must target the same isolated
    // worktree cluster that `dev worktree-env up` prepared, rather than
    // silently falling back to the shared `navigator` cluster.
    let lines = [
        ("PORT", web_port.to_string()),
        ("NAVIGATOR_ENVIRONMENT", "dev".into()),
        ("NAVIGATOR_CI_HARNESS", "1".into()),
        // Signature provider selection has no default and refuses to boot
        // unset (`portal::signature::SignatureProviderKind::from_env`). The
        // local tier runs the fake loop; `fake` is refused in production.
        ("NAVIGATOR_SIGNATURE_PROVIDER", "fake".into()),
        (
            "KUBECONFIG",
            shell_single_quote(&root.join(".devx").join("kubeconfig").display().to_string()),
        ),
        ("NAVIGATOR_KIND_CLUSTER", cfg.cluster.clone()),
        ("NAVIGATOR_K8S_NAMESPACE", cfg.namespace.clone()),
        ("NAVIGATOR_KIND_DEPS_OVERLAY", cfg.deps_overlay.clone()),
        ("NAVIGATOR_KIND_OVERLAY", cfg.full_overlay.clone()),
        ("NAVIGATOR_GKE_OVERLAY", cfg.gke_overlay.clone()),
        (
            "NAVIGATOR_KIND_INGRESS_HTTP_PORT",
            cfg.ingress_http_port.to_string(),
        ),
        (
            "NAVIGATOR_KIND_INGRESS_HTTPS_PORT",
            cfg.ingress_https_port.to_string(),
        ),
        ("NAVIGATOR_KIND_CLAMAV_PORT", cfg.clamav_port.to_string()),
        ("NAVIGATOR_KIND_RAUTHY_PORT", cfg.rauthy_port.to_string()),
        (
            "NAVIGATOR_KIND_SEAWEEDFS_S3_PORT",
            cfg.seaweedfs_s3_port.to_string(),
        ),
        ("NAVIGATOR_KIND_WEB_PORT", cfg.web_port.to_string()),
        // The second bind port `portal::hosting::run` opens for
        // `BrandKey::DeleteYourData`, alongside `PORT` for the default
        // brand — see ENG-437 and `views::brand::BrandKey::local_port_env_var`.
        (
            delete_your_data_port_env_var(),
            cfg.delete_your_data_web_port.to_string(),
        ),
        (
            "NAVIGATOR_KIND_OPENOBSERVE_PORT",
            cfg.openobserve_port.to_string(),
        ),
        (
            "NAVIGATOR_KIND_OPENOBSERVE_OTLP_PORT",
            cfg.openobserve_otlp_port.to_string(),
        ),
        // The opt-in `dev forgejo up` reads these from the sourced tier, so a
        // worktree's Forgejo forward and PostgreSQL mapping move with its slot.
        ("NAVIGATOR_KIND_FORGEJO_PORT", cfg.forgejo_port.to_string()),
        (
            "NAVIGATOR_KIND_POSTGRES_PORT",
            cfg.postgres_port.to_string(),
        ),
        (
            "NAVIGATOR_GIT_REPO_ROOT",
            shell_single_quote(
                &root
                    .join(".devx")
                    .join("repos")
                    .join(db_name)
                    .display()
                    .to_string(),
            ),
        ),
        // One directory holding every staged matter's bundle, each under its
        // own Project code. Boot re-reads each `navigator.yaml` rather than
        // trusting the directory name, so a bundle staged under the wrong
        // code is refused instead of published on another matter's portal.
        (
            store::sample_project::STAGE_ENV,
            shell_single_quote(
                &root
                    .join(".devx")
                    .join("sample-projects")
                    .display()
                    .to_string(),
            ),
        ),
        ("NAVIGATOR_STORAGE_BACKEND", "s3".into()),
        (
            "NAVIGATOR_STORAGE_ENDPOINT",
            format!("http://localhost:{}", cfg.seaweedfs_s3_port),
        ),
        ("NAVIGATOR_STORAGE_BUCKET", "navigator-documents".into()),
        ("NAVIGATOR_ASSETS_BUCKET", "navigator-assets".into()),
        (
            "NAVIGATOR_APPLICATIONS_BUCKET",
            "navigator-applications".into(),
        ),
        ("NAVIGATOR_LFS_BUCKET", "navigator-lfs".into()),
        // The dedicated Iceberg-archive bucket — distinct from
        // NAVIGATOR_STORAGE_BUCKET above, with its own SeaweedFS key pair, so
        // the nightly `archives` promotion (ENG-206) can be exercised
        // locally without borrowing the documents bucket.
        ("NAVIGATOR_ARCHIVES_BUCKET", "navigator-archives".into()),
        ("NAVIGATOR_STORAGE_REGION", "us-east-1".into()),
        ("NAVIGATOR_TELEMETRY_BUCKET", "navigator-telemetry".into()),
        (
            "NAVIGATOR_ARCHIVED_PROJECTS_BUCKET",
            "navigator-archived-projects".into(),
        ),
        (
            "NAVIGATOR_EXPORTS_ACCESS_KEY",
            env_string("NAVIGATOR_SEAWEEDFS_EXPORTS_ACCESS_KEY", ""),
        ),
        (
            "NAVIGATOR_EXPORTS_SECRET_KEY",
            env_string("NAVIGATOR_SEAWEEDFS_EXPORTS_SECRET_KEY", ""),
        ),
        (
            "NAVIGATOR_TELEMETRY_ACCESS_KEY",
            env_string("NAVIGATOR_SEAWEEDFS_TELEMETRY_ACCESS_KEY", ""),
        ),
        (
            "NAVIGATOR_TELEMETRY_SECRET_KEY",
            env_string("NAVIGATOR_SEAWEEDFS_TELEMETRY_SECRET_KEY", ""),
        ),
        (
            "NAVIGATOR_ARCHIVED_PROJECTS_ACCESS_KEY",
            env_string("NAVIGATOR_SEAWEEDFS_ARCHIVED_PROJECTS_ACCESS_KEY", ""),
        ),
        (
            "NAVIGATOR_ARCHIVED_PROJECTS_SECRET_KEY",
            env_string("NAVIGATOR_SEAWEEDFS_ARCHIVED_PROJECTS_SECRET_KEY", ""),
        ),
        (
            "NAVIGATOR_STORAGE_ACCESS_KEY",
            env_string("NAVIGATOR_SEAWEEDFS_ACCESS_KEY", "navigator-kind"),
        ),
        (
            "NAVIGATOR_STORAGE_SECRET_KEY",
            env_string("NAVIGATOR_SEAWEEDFS_SECRET_KEY", "navigator-kind-secret"),
        ),
        (
            "NAVIGATOR_ASSETS_ACCESS_KEY",
            env_string("NAVIGATOR_SEAWEEDFS_ASSETS_ACCESS_KEY", "navigator-assets"),
        ),
        (
            "NAVIGATOR_ASSETS_SECRET_KEY",
            env_string(
                "NAVIGATOR_SEAWEEDFS_ASSETS_SECRET_KEY",
                "navigator-assets-secret",
            ),
        ),
        (
            "NAVIGATOR_APPLICATIONS_ACCESS_KEY",
            env_string(
                "NAVIGATOR_SEAWEEDFS_APPLICATIONS_ACCESS_KEY",
                "navigator-applications",
            ),
        ),
        (
            "NAVIGATOR_APPLICATIONS_SECRET_KEY",
            env_string(
                "NAVIGATOR_SEAWEEDFS_APPLICATIONS_SECRET_KEY",
                "navigator-applications-secret",
            ),
        ),
        (
            "NAVIGATOR_LFS_ACCESS_KEY",
            env_string("NAVIGATOR_SEAWEEDFS_LFS_ACCESS_KEY", "navigator-lfs"),
        ),
        (
            "NAVIGATOR_LFS_SECRET_KEY",
            env_string("NAVIGATOR_SEAWEEDFS_LFS_SECRET_KEY", "navigator-lfs-secret"),
        ),
        (
            "NAVIGATOR_ARCHIVES_ACCESS_KEY",
            env_string(
                "NAVIGATOR_SEAWEEDFS_ARCHIVES_ACCESS_KEY",
                "navigator-archives",
            ),
        ),
        (
            "NAVIGATOR_ARCHIVES_SECRET_KEY",
            env_string(
                "NAVIGATOR_SEAWEEDFS_ARCHIVES_SECRET_KEY",
                "navigator-archives-secret",
            ),
        ),
        ("OAUTH_ISSUER_URL", rauthy_issuer(cfg.rauthy_port)),
        ("OAUTH_CLIENT_ID", "navigator-web".into()),
        ("OAUTH_CLIENT_SECRET", LOCAL_RAUTHY_CLIENT_SECRET.into()),
        (
            "OAUTH_REDIRECT_URI",
            format!("http://localhost:{web_port}/auth/callback"),
        ),
        (
            "SESSION_SECRET",
            "dev-only-session-secret-change-in-prod".into(),
        ),
        (
            "NAVIGATOR_CLAMD_ADDR",
            format!("127.0.0.1:{}", cfg.clamav_port),
        ),
        ("SENDGRID_API_KEY", "SG.kind-stub".into()),
        ("SENDGRID_INBOUND_SECRET", "kind-stub".into()),
        // OpenObserve's direct OTLP sink is port-forwarded to the host. To
        // run `web` with plain stdout and no export, set an empty endpoint in
        // `.env`, which loads before this generated file.
        (
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            format!("http://localhost:{}", cfg.openobserve_otlp_port),
        ),
        (
            "NAVIGATOR_OPENOBSERVE_URL",
            format!("http://localhost:{}", cfg.openobserve_port),
        ),
        (
            "NAVIGATOR_OPENOBSERVE_USERNAME",
            KIND_OPENOBSERVE_USERNAME.into(),
        ),
        (
            "NAVIGATOR_OPENOBSERVE_PASSWORD",
            KIND_OPENOBSERVE_PASSWORD.into(),
        ),
        (
            "NAVIGATOR_OPENOBSERVE_ORGANIZATION",
            KIND_OPENOBSERVE_ORGANIZATION.into(),
        ),
        (
            "NAVIGATOR_OPENOBSERVE_STREAM",
            KIND_OPENOBSERVE_STREAM.into(),
        ),
    ];
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Generated by `devx up`. Do not edit by hand — your edits are\n\
         # overwritten on the next `devx up`. Persistent / hand-edited\n\
         # values belong in `.env` at the workspace root, which is\n\
         # auto-loaded BEFORE this file by every binary's `main()`, so\n\
         # `.env` always wins on collisions.\n",
    );
    for (k, v) in lines {
        let _ = writeln!(out, "{k}={v}");
    }
    out
}

/// Coordinates of the existing private native resource, verified without lifecycle mutation.
pub(crate) struct NativeAdmissionTarget {
    pub port: u16,
    pub database: String,
    pub root_cert: std::path::PathBuf,
}

pub(crate) fn verified_native_admission_target() -> Result<Option<NativeAdmissionTarget>> {
    if std::env::var("NAVIGATOR_KIND_DEPS_OVERLAY").as_deref() != Ok(native_profile::OVERLAY) {
        return Ok(None);
    }
    let root = workspace_root()?;
    let cfg = worktree_env::postgres_fixture_config(&root, &KindConfig::from_env())?;
    let database = orchestrate::verified_native_database(&root, &cfg)?;
    Ok(Some(NativeAdmissionTarget {
        port: cfg.postgres_port,
        database,
        root_cert: postgres_fixture::ca_path(&root),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn parse_label_target_requires_both_sides() {
        assert_eq!(
            parse_label_target("em7475=target.sendgrid.net").unwrap(),
            ("em7475".to_string(), "target.sendgrid.net".to_string())
        );
        assert!(parse_label_target("=target").is_err());
        assert!(parse_label_target("label=").is_err());
        assert!(parse_label_target("   =target").is_err());
        assert!(parse_label_target("label=   ").is_err());
        assert!(parse_label_target("no-separator").is_err());
    }

    // The deploy workflow's "stub public assets" step generates
    // the placeholder `/public/img/...` and `/public/fonts/...` bytes the KIND
    // `assets verify` gate requires and removes the matching `.dockerignore`
    // rules so the baked image actually carries them. That step MUST run for whichever
    // image the KIND web pod runs — and since ENG-142 that is the single
    // distroless `navigator-web` (`k8s/base/web/web.yaml`), which no KIND
    // overlay patch overrides any more. Gating the stub step on the wrong
    // family leaves the served image empty and 404s all 18 paths in the
    // "content images are served by KIND web" gate.
    // The parsing/matching logic is factored into pure helpers below so both
    // the pass and the fail path are exercised directly — the guard is only
    // worth having if the failing case actually reports the drift.

    /// The web image family a manifest pins, parsed from its `image:` line
    /// (`:tag` stripped).
    fn kind_web_image_family(manifest_yaml: &str) -> Option<String> {
        manifest_yaml
            .lines()
            .find_map(|l| l.trim().strip_prefix("image:"))
            .map(|v| v.trim().split(':').next().unwrap_or_default().to_string())
    }

    /// The `build`-matrix leg that actually COMPILES `image` — the leg whose
    /// own `image` it is, or the leg carrying it as an `alias`.
    ///
    /// `navigator-web` is a tag on the `neon-server` leg's image, not a leg of
    /// its own: both names come from the same Containerfile, and a second leg
    /// would be a second metered compile of bytes the run already has. Every
    /// guard that asks "is the image the KIND pod runs built correctly?" has to
    /// follow that indirection, or it reads a leg that does not exist and
    /// reports drift that is not there.
    fn build_leg_for_image(deploy_yml: &str, image: &str) -> Option<String> {
        let workflow: serde_yaml::Value = serde_yaml::from_str(deploy_yml).ok()?;
        let legs = workflow["jobs"]["build"]["strategy"]["matrix"]["include"].as_sequence()?;
        legs.iter()
            .find(|leg| {
                leg["image"].as_str() == Some(image) || leg["alias"].as_str() == Some(image)
            })
            .and_then(|leg| leg["image"].as_str())
            .map(str::to_string)
    }

    /// `Ok` iff the deploy workflow's public-asset stub step is gated to run
    /// for `kind_web_image`; `Err(explanation)` otherwise. This is the exact
    /// linkage that broke once: the step ran only for one family while the
    /// KIND web pod served `/public` from another.
    fn stub_step_covers_image(deploy_yml: &str, kind_web_image: &str) -> Result<(), String> {
        let step_pos = deploy_yml
            .find("name: stub public assets")
            .ok_or_else(|| "deploy.yml: stub step not found".to_string())?;
        let if_line = deploy_yml[step_pos..]
            .lines()
            .take(3)
            .find(|l| l.trim_start().starts_with("if:"))
            .ok_or_else(|| "deploy.yml: stub step has no `if:` condition".to_string())?;
        if if_line.contains(&format!("== '{kind_web_image}'")) {
            Ok(())
        } else {
            Err(format!(
                "the `stub public assets` step must run for the image the KIND web \
                 pod runs (`{kind_web_image}`), or the served image will 404 on \
                 `/public/img/...` and `/public/fonts/...`. Found `if:` = `{}`",
                if_line.trim()
            ))
        }
    }

    #[test]
    fn stub_step_covers_the_image_the_kind_web_pod_runs() {
        use std::path::Path;
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("repo root is cli/'s parent")
            .to_path_buf();

        let base_web = std::fs::read_to_string(root.join("k8s/base/web/web.yaml"))
            .expect("read k8s/base/web/web.yaml");
        let kind_web_image =
            kind_web_image_family(&base_web).expect("base web.yaml pins a web image");
        assert_eq!(
            kind_web_image, "navigator-web",
            "the KIND web pod runs the single distroless web image (ENG-142)"
        );

        let deploy = std::fs::read_to_string(root.join(".github/workflows/deploy.yml"))
            .expect("read deploy.yml");
        // Through the alias, not around it: the KIND pod runs `navigator-web`,
        // which is a tag on whichever leg builds it. The stubs have to be in
        // THAT leg's image.
        let builder = build_leg_for_image(&deploy, &kind_web_image).unwrap_or_else(|| {
            panic!(
                "deploy.yml's build matrix must produce `{kind_web_image}`, as a leg's own image \
                 or as its `alias`"
            )
        });
        stub_step_covers_image(&deploy, &builder).expect(
            "deploy.yml stub step must cover the leg that builds the image the KIND web pod runs",
        );
    }

    /// No KIND overlay may override the web-container `image:`. The base pin
    /// is the single source of truth for what the in-cluster pod runs, and an
    /// override diverges from it silently, because the stub-step guard above
    /// reads the base.
    #[test]
    fn no_kind_overlay_overrides_the_web_image() {
        use std::path::Path;
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("repo root is cli/'s parent")
            .to_path_buf();

        let mut offenders = Vec::new();
        let mut stack = vec![root.join("k8s/overlays")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read overlay dir") {
                let path = entry.expect("overlay dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                    continue;
                }
                let body = std::fs::read_to_string(&path).expect("read overlay manifest");
                // Only the navigator-web Deployment patch matters; the
                // dependency manifests (SeaweedFS, Rauthy, Surreal) legitimately
                // pin their own upstream images.
                if !body.contains("name: navigator-web") {
                    continue;
                }
                // Sidecars pin legitimate upstream images (the Rauthy loopback
                // proxy runs nginx), so only first-party `navigator-*` pins are
                // candidates for reintroducing a second web family.
                offenders.extend(
                    body.lines()
                        .filter_map(|l| l.trim().strip_prefix("image:"))
                        .map(|v| v.trim().split(':').next().unwrap_or_default())
                        .filter(|family| {
                            family.starts_with("navigator-") && *family != "navigator-web"
                        })
                        .map(|family| format!("{}: {family}", path.display())),
                );
            }
        }
        assert!(
            offenders.is_empty(),
            "a KIND overlay pins a non-`navigator-web` image for the web pod; \
             the base pin is the single source of truth: {offenders:?}"
        );
    }

    /// The release COMPILES the web application exactly once.
    ///
    /// Two ways that has been violated, both of them a second full-workspace
    /// build of the same `neon` binary. `navigator-git` was one, on a
    /// git-bearing base, costing 22m build + 13m publish per nightly run while
    /// nothing deployed consumed it (ENG-142). `navigator-web` as its own
    /// matrix leg was the other, measured at 263 min beside `neon-server`'s
    /// 272 over five days — half of `deploy.yml`'s whole runner bill for a
    /// byte-identical image.
    ///
    /// So the count that matters is legs compiling `Containerfile.neon`, not
    /// spellings of a name: the alias tag is free and must stay allowed, while
    /// a second compile must not.
    #[test]
    fn the_release_compiles_the_web_application_exactly_once() {
        use std::path::Path;
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("repo root is cli/'s parent")
            .to_path_buf();
        let deploy = std::fs::read_to_string(root.join(".github/workflows/deploy.yml"))
            .expect("read deploy.yml");
        let workflow: serde_yaml::Value =
            serde_yaml::from_str(&deploy).expect("deploy.yml parses as YAML");

        let compiles_web: Vec<&str> = workflow["jobs"]["build"]["strategy"]["matrix"]["include"]
            .as_sequence()
            .expect("the build job must declare an include matrix")
            .iter()
            .filter(|leg| leg["dockerfile"].as_str() == Some("images/Containerfile.neon"))
            .filter_map(|leg| leg["image"].as_str())
            .collect();
        assert_eq!(
            compiles_web,
            ["neon-server"],
            "deploy.yml must compile the web application exactly once"
        );

        // ...and the name the deployed manifests still pull has to keep
        // arriving, as a tag on that one compile.
        assert_eq!(
            build_leg_for_image(&deploy, "navigator-web").as_deref(),
            Some("neon-server"),
            "`k8s/base/web/web.yaml` still pulls navigator-web, so it must remain an alias of the \
             image that leg builds"
        );
        assert!(
            !deploy.contains("navigator-git") && !deploy.contains("Containerfile.git"),
            "deploy.yml must not reference a git-bearing image"
        );
    }

    /// The guard's fail path must actually fire on the regression it exists to
    /// catch (a stub step gated to a family the KIND web pod does not run) and
    /// stay quiet once the served image is covered.
    #[test]
    fn stub_step_guard_flags_a_gate_that_misses_the_served_image() {
        assert_eq!(
            kind_web_image_family("        image: navigator-web:dev\n").as_deref(),
            Some("navigator-web")
        );
        assert_eq!(kind_web_image_family("no image here\n"), None);

        let wrong_family = "      - name: stub public assets for the KIND web image\n        \
                        if: matrix.image == 'navigator-workflows-service'\n        run: |\n";
        let err = stub_step_covers_image(wrong_family, "navigator-web")
            .expect_err("a gate missing the served image must be flagged");
        assert!(
            err.contains("navigator-web") && err.contains("404"),
            "{err}"
        );

        let covered = "      - name: stub public assets for the KIND web image\n        \
                    if: matrix.image == 'navigator-web'\n";
        assert!(stub_step_covers_image(covered, "navigator-web").is_ok());

        let missing_step = "      - name: something else\n        if: always()\n";
        assert!(stub_step_covers_image(missing_step, "navigator-web")
            .unwrap_err()
            .contains("stub step not found"));

        let no_if = "      - name: stub public assets for the KIND web image\n        \
                     run: echo hi\n        env:\n";
        assert!(stub_step_covers_image(no_if, "navigator-web")
            .unwrap_err()
            .contains("no `if:` condition"));
    }

    /// Alias resolution is the indirection every guard above now walks, so it
    /// gets its own pass and fail paths: a name reached through `alias` must
    /// resolve to the leg that compiles it, a name that is a leg resolves to
    /// itself, and a name in neither resolves to nothing rather than to a
    /// confident wrong answer.
    #[test]
    fn build_leg_resolution_follows_an_alias_tag_to_the_leg_that_compiles_it() {
        let matrix = "jobs:\n  build:\n    strategy:\n      matrix:\n        include:\n\
                      \x20         - image: neon-server\n            dockerfile: images/Containerfile.neon\n\
                      \x20           alias: navigator-web\n";

        assert_eq!(
            build_leg_for_image(matrix, "navigator-web").as_deref(),
            Some("neon-server"),
            "an alias must resolve to the leg that compiles it"
        );
        assert_eq!(
            build_leg_for_image(matrix, "neon-server").as_deref(),
            Some("neon-server")
        );
        assert_eq!(build_leg_for_image(matrix, "navigator-git"), None);
        assert_eq!(
            build_leg_for_image("not: yaml: at: all", "navigator-web"),
            None
        );
    }

    // The `linux/<arch>` platform fed to `docker save` (in
    // `kind_load_image_into_cluster`) must use the OCI arch suffix, not the
    // Rust arch triple — `linux/aarch64` would never match an image's
    // `linux/arm64` manifest, defeating the multi-arch flatten.

    // Every env var `KindConfig::from_env` reads. Tests clear all of
    // them before asserting so a stray value from the developer's shell
    // (or a prior test) can't leak in.
    const KIND_ENV_VARS: &[&str] = &[
        "NAVIGATOR_KIND_CLUSTER",
        "NAVIGATOR_K8S_NAMESPACE",
        "NAVIGATOR_KIND_DEPS_OVERLAY",
        "NAVIGATOR_KIND_OVERLAY",
        "NAVIGATOR_GKE_OVERLAY",
        "NAVIGATOR_KIND_INGRESS_HTTP_PORT",
        "NAVIGATOR_KIND_INGRESS_HTTPS_PORT",
        "NAVIGATOR_KIND_RESTATE_INGRESS_PORT",
        "NAVIGATOR_KIND_RESTATE_ADMIN_PORT",
        "NAVIGATOR_KIND_CLAMAV_PORT",
        "NAVIGATOR_KIND_RAUTHY_PORT",
        "NAVIGATOR_KIND_SEAWEEDFS_S3_PORT",
        "NAVIGATOR_KIND_WEB_PORT",
        "NAVIGATOR_LOCAL_DELETE_YOUR_DATA_PORT",
        "NAVIGATOR_KIND_OPENOBSERVE_PORT",
        "NAVIGATOR_KIND_OPENOBSERVE_OTLP_PORT",
    ];

    // Process env is global; `from_env` reads all of it. Serialize the
    // env-mutating tests so they don't race each other.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn clear_kind_env() {
        for key in KIND_ENV_VARS {
            env::remove_var(key);
        }
    }

    /// A `KindConfig` at its defaults, built without touching the
    /// process environment — for the pure render tests.
    pub(super) fn default_cfg() -> KindConfig {
        KindConfig {
            cluster: DEFAULT_CLUSTER_NAME.into(),
            namespace: DEFAULT_NAMESPACE.into(),
            deps_overlay: DEFAULT_KUSTOMIZE_KIND_DEPS.into(),
            full_overlay: DEFAULT_KUSTOMIZE_KIND.into(),
            gke_overlay: DEFAULT_KUSTOMIZE_GKE.into(),
            ingress_http_port: DEFAULT_INGRESS_HTTP_HOST_PORT,
            ingress_https_port: DEFAULT_INGRESS_HTTPS_HOST_PORT,
            restate_ingress_port: DEFAULT_RESTATE_INGRESS_HOST_PORT,
            restate_admin_port: DEFAULT_RESTATE_ADMIN_HOST_PORT,
            clamav_port: DEFAULT_CLAMAV_HOST_PORT,
            rauthy_port: DEFAULT_RAUTHY_HOST_PORT,
            seaweedfs_s3_port: DEFAULT_SEAWEEDFS_S3_HOST_PORT,
            surreal_port: DEFAULT_SURREAL_HOST_PORT,
            web_port: DEFAULT_LOCAL_WEB_PORT,
            delete_your_data_web_port: DEFAULT_LOCAL_DELETE_YOUR_DATA_WEB_PORT,
            openobserve_port: DEFAULT_OPENOBSERVE_HOST_PORT,
            openobserve_otlp_port: DEFAULT_OPENOBSERVE_OTLP_HOST_PORT,
            forgejo_port: DEFAULT_FORGEJO_HOST_PORT,
            postgres_port: DEFAULT_POSTGRES_HOST_PORT,
        }
    }

    /// `undeploy` deletes a whole namespace. Unpinned it would delete it from
    /// whatever context is current — on an operator's machine as likely prod
    /// as KIND — so the pin is asserted, not assumed.
    #[test]
    fn undeploy_pins_the_kind_context() {
        assert_eq!(
            undeploy_args(&default_cfg().kind_context(), DEFAULT_NAMESPACE),
            [
                "--context",
                "kind-navigator",
                "delete",
                "--ignore-not-found",
                "namespace",
                "navigator",
            ]
        );
    }

    /// Local fixture commands pin the worktree’s KIND context.
    #[test]
    fn use_context_pins_the_isolated_kind_context() {
        // A worktree cluster carries a per-checkout suffix; the pin must target
        // that isolated cluster, not the shared `kind-navigator` or an ambient
        // GKE/EKS context.
        let mut worktree = default_cfg();
        worktree.cluster = "navigator-6e2e1eb5".to_string();
        let context = worktree.kind_context();
        assert_eq!(context, "kind-navigator-6e2e1eb5");
        assert_eq!(
            use_context_args(&context),
            ["config", "use-context", "kind-navigator-6e2e1eb5"]
        );
        assert!(
            !use_context_args(&context)
                .iter()
                .any(|arg| arg.starts_with("gke_")),
            "the pin must never resolve to a GKE context"
        );
    }

    /// Load-bearing safety net: an empty environment must reproduce the
    /// exact pre-refactor constants. If this fails, `devx up` against a
    /// clean `.env` would behave differently than it always has.
    #[test]
    fn from_env_with_no_vars_equals_defaults() {
        let _guard = lock();
        clear_kind_env();
        assert_eq!(KindConfig::from_env(), default_cfg());
    }

    #[test]
    fn from_env_reads_string_overrides() {
        let _guard = lock();
        clear_kind_env();
        env::set_var("NAVIGATOR_KIND_CLUSTER", "fork-cluster");
        env::set_var("NAVIGATOR_K8S_NAMESPACE", "fork-ns");
        env::set_var("NAVIGATOR_KIND_DEPS_OVERLAY", "my/deps");
        env::set_var("NAVIGATOR_KIND_OVERLAY", "my/full");
        env::set_var("NAVIGATOR_GKE_OVERLAY", "my/gke");
        let cfg = KindConfig::from_env();
        clear_kind_env();
        assert_eq!(cfg.cluster, "fork-cluster");
        assert_eq!(cfg.namespace, "fork-ns");
        assert_eq!(cfg.deps_overlay, "my/deps");
        assert_eq!(cfg.full_overlay, "my/full");
        assert_eq!(cfg.gke_overlay, "my/gke");
    }

    /// An overlay in this repository, built the way `dev deploy` applies it.
    ///
    /// Straight off disk, unlike the GKE pair in `ship.rs`: the KIND
    /// overlays carry no `${}` substitutions and are applied with
    /// `apply -k <path>`, so the on-disk tree already IS what gets applied.
    /// There is nothing for an embed-and-render step to add.
    fn kind_overlay(name: &str) -> String {
        let overlay = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../k8s/overlays")).join(name);
        ship::kustomize_build(&overlay)
            .unwrap_or_else(|err| panic!("k8s/overlays/{name} builds: {err:#}"))
    }

    #[test]
    fn kind_deps_use_direct_openobserve_without_lgtm() {
        let manifests = kind_overlay("kind-deps");

        assert!(
            !manifests.contains("grafana/otel-lgtm"),
            "the KIND dependency tier exports to OpenObserve directly, with no LGTM image"
        );
        assert!(
            !manifests.contains("name: lgtm"),
            "no LGTM Service or Deployment belongs in the KIND dependency tier"
        );

        let service = ship::manifest_doc(&manifests, "Service", "openobserve");
        assert_eq!(service["spec"]["ports"][0]["port"].as_u64(), Some(5080));
        assert_eq!(service["spec"]["ports"][1]["port"].as_u64(), Some(5081));

        let deployment = ship::manifest_doc(&manifests, "Deployment", "openobserve");
        let container = &deployment["spec"]["template"]["spec"]["containers"][0];
        assert!(
            container["image"]
                .as_str()
                .is_some_and(|image| image.starts_with("o2cr.ai/openobserve/openobserve@sha256:")),
            "OpenObserve must be pinned by immutable digest"
        );
        let env = container["env"].as_sequence().expect("OpenObserve has env");
        assert!(
            env.iter()
                .any(|entry| entry["name"].as_str() == Some("ZO_GRPC_PORT")
                    && entry["value"].as_str() == Some("5081")),
            "OpenObserve must expose direct OTLP gRPC on 5081"
        );
        assert!(
            manifests.contains("NAVIGATOR_OPENOBSERVE_PASSWORD"),
            "both Navigator binaries must receive the OpenObserve credential contract"
        );
        assert!(
            manifests.contains("openobserve_endpoint: http://openobserve:5081"),
            "the staging coordinates must point binaries directly at OpenObserve"
        );
    }

    #[test]
    fn from_env_reads_port_overrides() {
        let _guard = lock();
        clear_kind_env();
        env::set_var("NAVIGATOR_KIND_INGRESS_HTTP_PORT", "18080");
        env::set_var("NAVIGATOR_KIND_INGRESS_HTTPS_PORT", "18443");
        env::set_var("NAVIGATOR_KIND_RESTATE_INGRESS_PORT", "19080");
        env::set_var("NAVIGATOR_KIND_RESTATE_ADMIN_PORT", "19070");
        env::set_var("NAVIGATOR_KIND_CLAMAV_PORT", "23310");
        env::set_var("NAVIGATOR_KIND_RAUTHY_PORT", "31080");
        env::set_var("NAVIGATOR_KIND_SEAWEEDFS_S3_PORT", "31900");
        env::set_var("NAVIGATOR_KIND_WEB_PORT", "4001");
        env::set_var(delete_your_data_port_env_var(), "4011");
        let cfg = KindConfig::from_env();
        clear_kind_env();
        assert_eq!(cfg.ingress_http_port, 18080);
        assert_eq!(cfg.ingress_https_port, 18443);
        assert_eq!(cfg.restate_ingress_port, 19080);
        assert_eq!(cfg.restate_admin_port, 19070);
        assert_eq!(cfg.clamav_port, 23310);
        assert_eq!(cfg.rauthy_port, 31080);
        assert_eq!(cfg.seaweedfs_s3_port, 31900);
        assert_eq!(cfg.web_port, 4001);
        assert_eq!(cfg.delete_your_data_web_port, 4011);
    }

    #[test]
    fn empty_and_garbage_values_fall_back_to_defaults() {
        let _guard = lock();
        clear_kind_env();
        // Empty string → default (a `FOO=` line shouldn't blank a path).
        env::set_var("NAVIGATOR_KIND_CLUSTER", "");
        // Unparseable port → default rather than a crash.
        env::set_var("NAVIGATOR_KIND_WEB_PORT", "not-a-port");
        let cfg = KindConfig::from_env();
        clear_kind_env();
        assert_eq!(cfg.cluster, DEFAULT_CLUSTER_NAME);
        assert_eq!(cfg.web_port, DEFAULT_LOCAL_WEB_PORT);
        assert_eq!(
            cfg.delete_your_data_web_port,
            DEFAULT_LOCAL_DELETE_YOUR_DATA_WEB_PORT
        );
    }

    /// Credential-bearing variables a maintainer's shell may carry. The
    /// fresh-clone path in `docs/self-hosting.md` promises it needs none.
    const PRIVATE_CREDENTIAL_VARS: &[&str] = &[
        "GOOGLE_APPLICATION_CREDENTIALS",
        "GOOGLE_CLOUD_PROJECT",
        "CLOUDSDK_CORE_PROJECT",
        "CLOUDSDK_CONFIG",
        "NAVIGATOR_GCP_PROJECT_ID",
        "NAVIGATOR_IMAGE_REGISTRY",
        "SOPS_AGE_KEY",
        "SOPS_AGE_KEY_FILE",
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "NAVIGATOR_SEAWEEDFS_ACCESS_KEY",
        "NAVIGATOR_SEAWEEDFS_SECRET_KEY",
    ];

    /// Parse a rendered `.devx/env` the way `dotenvy` reads it: `KEY=value`
    /// lines, `#` comments skipped, one layer of single quotes removed.
    fn parse_rendered_env(rendered: &str) -> std::collections::BTreeMap<String, String> {
        rendered
            .lines()
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| {
                let value = value
                    .strip_prefix('\'')
                    .and_then(|v| v.strip_suffix('\''))
                    .unwrap_or(value);
                (key.to_owned(), value.to_owned())
            })
            .collect()
    }

    /// The documented fresh-clone loop is `dev up`, then `cargo run -p neon`
    /// with only `.devx/env` sourced. The completed file combines the service
    /// environment with the enrolled fixture's generated PostgreSQL credentials.
    /// It must pass the web invariants without private shell credentials.
    #[test]
    fn fresh_clone_env_boots_web_without_private_credentials() {
        let _guard = lock();
        clear_kind_env();
        for key in PRIVATE_CREDENTIAL_VARS {
            env::remove_var(key);
        }
        let root = Path::new("/fresh-clone");
        let cfg = KindConfig::from_env();
        let credentials = postgres_fixture::Credentials {
            app_password: "synthetic-app".into(),

            ca: "synthetic-ca".into(),
        };
        let mut environment = render_env(&cfg, root);
        environment.push_str(&postgres_fixture::render_environment(
            &credentials.environment_for(&cfg, root, "navigator"),
        ));
        let rendered = parse_rendered_env(&environment);
        let get = |key: &str| rendered.get(key).cloned();

        let config = portal::config::AppConfig::from_lookup(get)
            .expect("`.devx/env` resolves a web config on its own");
        assert_eq!(config.environment, store::DeploymentEnvironment::Dev);
        portal::config::enforce_deployment_invariants(store::DeploymentEnvironment::Dev, get)
            .expect("`.devx/env` satisfies every dev-profile web invariant without `.env`");

        for key in PRIVATE_CREDENTIAL_VARS {
            assert!(
                !rendered.contains_key(*key),
                "`.devx/env` must not need {key}"
            );
        }
        for (key, value) in &rendered {
            for private in ["pkg.dev", "googleapis.com", "gs://", "neon-law"] {
                assert!(
                    !value.contains(private),
                    "{key}={value} points the local loop at a private coordinate ({private})"
                );
            }
        }
    }

    /// Every container image the `dev up` dependency overlay pulls is public
    /// or built from the checkout (`:dev`), so a fresh clone needs no registry
    /// login. Walks the kustomization graph from the overlay root.
    #[test]
    fn fresh_clone_dependency_overlay_pulls_no_private_image() {
        use std::fs;
        use std::path::PathBuf;

        fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
            let manifest = dir.join("kustomization.yaml");
            let text = fs::read_to_string(&manifest)
                .unwrap_or_else(|err| panic!("read {}: {err}", manifest.display()));
            files.push(manifest.clone());
            let doc: serde_yaml::Value = serde_yaml::from_str(&text).expect("kustomization YAML");
            let entries = ["resources", "components"]
                .iter()
                .filter_map(|key| doc.get(*key).and_then(serde_yaml::Value::as_sequence))
                .flatten()
                .filter_map(serde_yaml::Value::as_str);
            for entry in entries {
                let path = dir.join(entry);
                if path.is_dir() {
                    collect(&path, files);
                } else {
                    files.push(path);
                }
            }
            if let Some(patches) = doc.get("patches").and_then(serde_yaml::Value::as_sequence) {
                for patch in patches {
                    if let Some(path) = patch.get("path").and_then(serde_yaml::Value::as_str) {
                        files.push(dir.join(path));
                    }
                }
            }
        }

        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("repo root is cli/'s parent");
        let mut files = Vec::new();
        collect(&root.join(DEFAULT_KUSTOMIZE_KIND_DEPS), &mut files);
        let mut images = Vec::new();
        for file in &files {
            let text = fs::read_to_string(file)
                .unwrap_or_else(|err| panic!("read {}: {err}", file.display()));
            for line in text.lines() {
                let trimmed = line.trim_start().trim_start_matches("- ");
                for key in ["image:", "newName:"] {
                    if let Some(image) = trimmed.strip_prefix(key) {
                        images.push((file.clone(), image.trim().to_owned()));
                    }
                }
            }
        }
        assert!(
            !images.is_empty(),
            "the dependency overlay names its images"
        );
        for (file, image) in images {
            assert!(
                !image.contains("pkg.dev") && !image.contains("gcr.io"),
                "{} pulls {image} from a private registry",
                file.display()
            );
        }
    }

    #[test]
    fn render_env_threads_the_ports() {
        let mut cfg = default_cfg();
        cfg.cluster = "navigator-task".into();
        cfg.web_port = 4001;
        cfg.delete_your_data_web_port = 4011;
        cfg.clamav_port = 23310;
        cfg.rauthy_port = 31080;
        cfg.seaweedfs_s3_port = 31900;
        cfg.openobserve_port = 15_080;
        cfg.openobserve_otlp_port = 15_081;
        cfg.forgejo_port = 21_437;
        cfg.postgres_port = 21_537;
        let root = Path::new("/ws");
        let env = render_env(&cfg, root);
        assert!(env.contains("PORT=4001"));
        assert!(env.contains("NAVIGATOR_KIND_FORGEJO_PORT=21437"));
        assert!(env.contains("NAVIGATOR_KIND_POSTGRES_PORT=21537"));
        assert!(env.contains(&format!(
            "KUBECONFIG='{}'",
            root.join(".devx").join("kubeconfig").display()
        )));
        assert!(env.contains("NAVIGATOR_KIND_CLUSTER=navigator-task"));
        assert!(env.contains("NAVIGATOR_KIND_WEB_PORT=4001"));
        assert!(env.contains(&format!("{}=4011", delete_your_data_port_env_var())));
        assert!(env.contains(&format!(
            "NAVIGATOR_GIT_REPO_ROOT='{}'",
            root.join(".devx").join("repos").join("navigator").display()
        )));
        assert!(env.contains("OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:15081"));
        assert!(env.contains("NAVIGATOR_OPENOBSERVE_URL=http://localhost:15080"));
        assert!(env.contains("NAVIGATOR_OPENOBSERVE_USERNAME=root@example.com"));
        assert!(env.contains("NAVIGATOR_OPENOBSERVE_PASSWORD=NavigatorKindOpenObserve1!"));
        assert!(env.contains("NAVIGATOR_OPENOBSERVE_ORGANIZATION=default"));
        assert!(env.contains("NAVIGATOR_CLAMD_ADDR=127.0.0.1:23310"));
        assert!(env.contains("OAUTH_ISSUER_URL=http://localhost:31080/auth/v1/"));
        assert!(env.contains("NAVIGATOR_STORAGE_ENDPOINT=http://localhost:31900"));
        assert!(env.contains("NAVIGATOR_ASSETS_BUCKET=navigator-assets"));
        assert!(env.contains("NAVIGATOR_ASSETS_ACCESS_KEY=navigator-assets"));
        assert!(env.contains("NAVIGATOR_APPLICATIONS_BUCKET=navigator-applications"));
        assert!(env.contains("NAVIGATOR_APPLICATIONS_ACCESS_KEY=navigator-applications"));
        assert!(env.contains("NAVIGATOR_LFS_BUCKET=navigator-lfs"));
        assert!(env.contains("NAVIGATOR_LFS_ACCESS_KEY=navigator-lfs"));
        assert!(env.contains("OAUTH_REDIRECT_URI=http://localhost:4001/auth/callback"));
    }

    /// `web` refuses to boot without an explicit signature provider, so
    /// the local tier names the fake loop or quickstart never comes up.
    #[test]
    fn render_env_selects_the_fake_signature_provider() {
        let env = render_env(&default_cfg(), Path::new("/ws"));
        assert!(env.contains("NAVIGATOR_SIGNATURE_PROVIDER=fake"), "{env}");
    }

    #[test]
    fn render_env_shell_quotes_worktree_paths() {
        // A worktree root with a space is the ordinary way an unquoted value
        // truncates when `.devx/env` is sourced, stranding destructive staging
        // commands on the ambient kubeconfig. The generated lines must survive
        // both a POSIX `source` and the `dotenvy` parse every binary runs.
        let cfg = default_cfg();
        let root = Path::new("/ws with space");
        let expected_kubeconfig = root.join(".devx").join("kubeconfig");
        let expected_repo_root = root.join(".devx").join("repos").join("navigator");
        let env = render_env(&cfg, root);
        assert!(
            env.contains(&format!("KUBECONFIG='{}'", expected_kubeconfig.display())),
            "{env}"
        );
        assert!(
            env.contains(&format!(
                "NAVIGATOR_GIT_REPO_ROOT='{}'",
                expected_repo_root.display()
            )),
            "{env}"
        );

        // `dotenvy` is the loader `main()` and the reset proof both use; parse
        // the rendered body back and confirm the quotes are stripped to the
        // exact path rather than being taken literally or split on the space.
        let parsed: std::collections::HashMap<String, String> =
            dotenvy::from_read_iter(env.as_bytes())
                .map(|item| item.expect("parse rendered .devx/env line"))
                .collect();
        assert_eq!(
            parsed.get("KUBECONFIG").map(String::as_str),
            Some(
                expected_kubeconfig
                    .to_str()
                    .expect("test path is valid UTF-8")
            )
        );
        assert_eq!(
            parsed.get("NAVIGATOR_GIT_REPO_ROOT").map(String::as_str),
            Some(
                expected_repo_root
                    .to_str()
                    .expect("test path is valid UTF-8")
            )
        );
    }

    // The committed cluster config; the render helper must reproduce it
    // byte-for-byte at default ports.
    const COMMITTED_KIND_CONFIG: &str = include_str!("../../../k8s/kind-config.yaml");

    /// Rauthy's issuer and `WebAuthn` origin must sit on this tier's own
    /// browser-reachable port.
    #[test]
    fn the_rauthy_public_url_patch_targets_this_tiers_own_port() {
        let patch = rauthy_public_url_patch(20_445);

        assert!(
            patch.contains(r#""PUB_URL":"localhost:20445""#),
            "patch must point PUB_URL at the tier's own port: {patch}"
        );
        assert!(
            patch.contains(r#""RP_ORIGIN":"http://localhost:20445""#),
            "patch must align RP_ORIGIN with the tier's own port: {patch}"
        );
        assert!(
            !patch.contains("localhost:8080"),
            "patch must not reintroduce the shared ingress origin: {patch}"
        );
        assert!(
            patch.contains(r#""stringData":"#),
            "the patch must preserve the Deployment's valueFrom contract: {patch}"
        );
    }

    /// The patch is a pure function of the port so repeated reconciliation
    /// cannot drift the issuer or `WebAuthn` origin.
    #[test]
    fn the_rauthy_public_url_patch_is_deterministic() {
        assert_eq!(
            rauthy_public_url_patch(20_445),
            rauthy_public_url_patch(20_445)
        );
        assert_ne!(
            rauthy_public_url_patch(20_445),
            rauthy_public_url_patch(30_080)
        );
    }

    /// `web` validates the issuer it was configured with against the one
    /// Rauthy advertises, so the public URL patch, host environment, and
    /// in-cluster loopback bridge are one fact.
    #[test]
    fn every_rauthy_channel_agrees_on_the_issuer_port() {
        let mut cfg = default_cfg();
        cfg.rauthy_port = 20_445;

        let origin = rauthy_origin(cfg.rauthy_port);
        let issuer = rauthy_issuer(cfg.rauthy_port);
        let env = render_env(&cfg, Path::new("/ws"));

        assert!(
            env.contains(&format!("OAUTH_ISSUER_URL={issuer}")),
            "`.devx/env` must carry the canonical issuer; env was:\n{env}"
        );
        assert!(
            rauthy_public_url_patch(cfg.rauthy_port).contains(&origin),
            "the Rauthy deployment patch must carry the same origin"
        );
        let proxy = rauthy_loopback_config(cfg.rauthy_port);
        assert!(proxy.contains("listen 127.0.0.1:20445"), "{proxy}");
        assert!(proxy.contains("Host localhost:20445"), "{proxy}");
        assert!(
            rauthy_loopback_config_patch(cfg.rauthy_port).contains("20445"),
            "the ConfigMap patch must carry the same port"
        );
    }

    #[test]
    fn render_kind_config_is_byte_identical_at_defaults() {
        let rendered = render_kind_config(COMMITTED_KIND_CONFIG, &default_cfg());
        assert_eq!(rendered, COMMITTED_KIND_CONFIG);
    }

    #[test]
    fn inject_kube_proxy_nftables_adds_networking_after_api_version() {
        let rendered = inject_kube_proxy_nftables(COMMITTED_KIND_CONFIG);
        assert!(rendered.contains("apiVersion: kind.x-k8s.io/v1alpha4\nnetworking:\n  kubeProxyMode: nftables\nname: navigator\n"));
        assert_eq!(
            inject_kube_proxy_nftables(&rendered),
            rendered,
            "a second pass must not duplicate the block"
        );
    }

    #[test]
    fn render_kind_config_substitutes_the_configurable_hostports() {
        let mut cfg = default_cfg();
        cfg.ingress_http_port = 18080;
        cfg.ingress_https_port = 18443;
        cfg.rauthy_port = 31080;
        cfg.postgres_port = 21537;
        let rendered = render_kind_config(COMMITTED_KIND_CONFIG, &cfg);
        assert!(rendered.contains("hostPort: 18080"));
        assert!(rendered.contains("hostPort: 18443"));
        assert!(rendered.contains("hostPort: 31080"));
        assert!(rendered.contains("hostPort: 21537"));
        assert!(rendered.contains("containerPort: 31532"));
        assert!(rendered.contains("listenAddress: \"127.0.0.1\""));
        assert!(!rendered.contains("hostPort: 15432"));
        assert!(!rendered.contains("hostPort: 8080"));
        assert!(!rendered.contains("hostPort: 8443"));
        assert!(!rendered.contains("hostPort: 30080"));
        assert!(rendered.contains("containerPort: 30080"));
        // Only host-port values change — line count is stable.
        assert_eq!(
            rendered.lines().count(),
            COMMITTED_KIND_CONFIG.lines().count()
        );
    }

    // --- ENG-743: one reviewable run over a family of zones ---------------

    /// Repeated `--domain` reconciles each zone, in the order given. Order is
    /// asserted because the run's output is the artifact a reviewer reads.
    #[test]
    fn repeated_domain_flags_reconcile_every_zone_in_order() {
        let zones = dns_zones(vec![
            "vestaestateplanning.com".to_string(),
            "misericordialaw.com".to_string(),
            "abhayaimmigration.com".to_string(),
            "deleteyourdebt.com".to_string(),
        ])
        .expect("four distinct zones");
        assert_eq!(
            zones,
            [
                "vestaestateplanning.com",
                "misericordialaw.com",
                "abhayaimmigration.com",
                "deleteyourdebt.com",
            ]
        );
    }

    /// A zone repeated on one command line is a typo, not an instruction to
    /// reconcile it twice. Left alone it would double every create in the
    /// run, which at an apex is exactly the duplicate this command exists to
    /// avoid.
    #[test]
    fn a_repeated_zone_is_rejected_rather_than_reconciled_twice() {
        let err = dns_zones(vec![
            "vestaestateplanning.com".to_string(),
            "misericordialaw.com".to_string(),
            "vestaestateplanning.com".to_string(),
        ])
        .expect_err("a duplicate zone is an error");
        assert!(
            err.to_string().contains("vestaestateplanning.com"),
            "the error names the offending zone: {err}"
        );
    }

    /// Blank entries never reach the provider as an empty zone.
    #[test]
    fn blank_domains_are_dropped() {
        let zones = dns_zones(vec![
            "  ".to_string(),
            " deleteyourdebt.com ".to_string(),
            String::new(),
        ])
        .expect("one real zone");
        assert_eq!(zones, ["deleteyourdebt.com"]);
    }

    /// No `--domain` and no `DNS_ZONE` fails with a message naming both ways
    /// in, rather than reconciling an empty zone.
    #[test]
    fn no_domain_anywhere_is_an_error_naming_both_inputs() {
        // `DNS_ZONE` is read inside, so only assert the message when the
        // ambient environment does not supply one — an operator shell often
        // does, and this test must not depend on that.
        if std::env::var("DNS_ZONE").is_ok_and(|zone| !zone.trim().is_empty()) {
            return;
        }
        let err = dns_zones(Vec::new()).expect_err("no zone anywhere");
        let message = err.to_string();
        assert!(message.contains("--domain"), "{message}");
        assert!(message.contains("DNS_ZONE"), "{message}");
    }

    /// Every brand apex asks for the same three records, and the apex `URL`
    /// record points at that zone's own `www` rather than a shared host.
    ///
    /// The copy-paste failure this guards is real: four near-identical
    /// invocations differing only in the domain are exactly where one apex
    /// ends up redirecting to another brand's site.
    #[test]
    fn each_brand_apex_redirects_to_its_own_www() {
        let config = dns::DnsSetupConfig {
            gateway_ip: Some("203.0.113.10".to_string()),
            hosts: vec!["www".to_string(), "staging".to_string()],
            redirect_apex_to_www: true,
            ..dns::DnsSetupConfig::default()
        };
        for zone in [
            "vestaestateplanning.com",
            "misericordialaw.com",
            "abhayaimmigration.com",
            "deleteyourdebt.com",
        ] {
            let desired = dns::desired_records(zone, &config);

            let apex = desired
                .iter()
                .find(|record| record.record_type == dns::RecordType::Url)
                .expect("the apex URL record");
            assert_eq!(apex.name, "", "the apex is the empty name, never `@`");
            assert_eq!(apex.content, format!("https://www.{zone}"));
            assert_eq!(apex.ttl, 300, "low TTL while iterating");

            for host in ["www", "staging"] {
                assert!(
                    desired.iter().any(|record| {
                        record.record_type == dns::RecordType::A
                            && record.name == host
                            && record.content == "203.0.113.10"
                    }),
                    "{zone} points {host} at the gateway: {desired:?}"
                );
            }
        }
    }
}

#[cfg(test)]
mod dns_setup_cli_tests {
    use super::*;
    use clap::Parser as _;
    use std::collections::HashMap;
    use std::sync::Mutex;

    fn parse_setup(args: &[&str]) -> DnsCmd {
        let mut command_line = vec!["navigator", "ops", "dns", "setup"];
        command_line.extend_from_slice(args);
        let cli = crate::Cli::try_parse_from(command_line).expect("valid DNS setup arguments");
        match cli.command.expect("operator command") {
            crate::Command::Ops(crate::OpsCmd::Dns(command)) => command,
            _ => panic!("expected DNS command"),
        }
    }

    #[test]
    fn dns_setup_defaults_to_preview_and_preserves_dry_run_alias() {
        let DnsCmd::Setup { apply, dry_run, .. } = parse_setup(&["--domain", "one.example"]);
        assert!(!apply, "apply must require --apply");
        assert!(!dry_run, "the compatibility flag is opt-in");

        let DnsCmd::Setup { apply, dry_run, .. } =
            parse_setup(&["--domain", "one.example", "--dry-run"]);
        assert!(!apply);
        assert!(dry_run);
    }

    #[test]
    fn dns_setup_apply_is_explicit_and_conflicts_with_dry_run() {
        let DnsCmd::Setup { apply, dry_run, .. } =
            parse_setup(&["--domain", "one.example", "--apply"]);
        assert!(apply);
        assert!(!dry_run);

        let err = crate::Cli::try_parse_from([
            "navigator",
            "ops",
            "dns",
            "setup",
            "--domain",
            "one.example",
            "--apply",
            "--dry-run",
        ])
        .err()
        .expect("contradictory DNS setup modes must be rejected");
        assert!(err.to_string().contains("cannot be used with"));
    }

    #[derive(Default)]
    struct FakeProvider {
        records: Mutex<HashMap<String, Vec<dns::ExistingRecord>>>,
        record_mutations: Mutex<Vec<String>>,
        certificates: Mutex<HashMap<String, Option<dns::ApexCertificate>>>,
        certificate_mutations: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl dns::DnsProvider for FakeProvider {
        async fn list_records(
            &self,
            zone: &str,
        ) -> std::result::Result<Vec<dns::ExistingRecord>, dns::DnsError> {
            Ok(self
                .records
                .lock()
                .expect("records lock")
                .get(zone)
                .cloned()
                .unwrap_or_default())
        }

        async fn create_record(
            &self,
            zone: &str,
            record: &dns::DesiredRecord,
        ) -> std::result::Result<u64, dns::DnsError> {
            self.record_mutations
                .lock()
                .expect("record mutations lock")
                .push(format!(
                    "create:{zone}:{}:{}",
                    record.record_type.as_str(),
                    record.name
                ));
            Ok(700)
        }

        async fn update_record(
            &self,
            zone: &str,
            record_id: u64,
            record: &dns::DesiredRecord,
        ) -> std::result::Result<(), dns::DnsError> {
            self.record_mutations
                .lock()
                .expect("record mutations lock")
                .push(format!(
                    "update:{zone}:{record_id}:{}:{}",
                    record.record_type.as_str(),
                    record.name
                ));
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl dns::CertificateProvider for FakeProvider {
        async fn latest_certificate(
            &self,
            zone: &str,
        ) -> std::result::Result<Option<dns::ApexCertificate>, dns::DnsError> {
            Ok(self
                .certificates
                .lock()
                .expect("certificates lock")
                .get(zone)
                .copied()
                .flatten())
        }

        async fn order_letsencrypt(
            &self,
            zone: &str,
            auto_renew: bool,
        ) -> std::result::Result<dns::ApexCertificate, dns::DnsError> {
            self.certificate_mutations
                .lock()
                .expect("certificate mutations lock")
                .push(format!("order:{zone}:auto-renew={auto_renew}"));
            Ok(dns::ApexCertificate {
                id: 901,
                state: dns::ApexCertificateState::Pending,
            })
        }

        async fn issue_letsencrypt(
            &self,
            zone: &str,
            certificate_id: u64,
        ) -> std::result::Result<dns::ApexCertificateState, dns::DnsError> {
            self.certificate_mutations
                .lock()
                .expect("certificate mutations lock")
                .push(format!("issue:{zone}:{certificate_id}"));
            Ok(dns::ApexCertificateState::Active)
        }
    }

    fn dispatch_dns(args: &[&str], provider: &FakeProvider) -> Result<()> {
        let mut command_line = vec!["navigator", "ops", "dns", "setup"];
        command_line.extend_from_slice(args);
        let cli = crate::Cli::try_parse_from(command_line)?;
        dispatch_with_dns_provider(cli.command.expect("operator command"), Some(provider))
    }

    fn active_certificate(id: u64) -> dns::ApexCertificate {
        dns::ApexCertificate {
            id,
            state: dns::ApexCertificateState::Active,
        }
    }

    #[test]
    fn default_and_dry_run_dispatch_do_not_mutate_records_or_certificates() {
        for extra_args in [vec![], vec!["--dry-run"]] {
            let provider = FakeProvider::default();
            dispatch_dns(
                &[
                    "--domain",
                    "one.example",
                    "--domain",
                    "two.example",
                    "--gateway-ip",
                    "203.0.113.10",
                    "--redirect-apex-to-www",
                ]
                .into_iter()
                .chain(extra_args)
                .collect::<Vec<_>>(),
                &provider,
            )
            .expect("preview dispatch");

            assert!(provider
                .record_mutations
                .lock()
                .expect("record mutations")
                .is_empty());
            assert!(provider
                .certificate_mutations
                .lock()
                .expect("certificate mutations")
                .is_empty());
        }
    }

    #[test]
    fn apply_dispatch_performs_only_the_printed_plan_across_zones() {
        let provider = FakeProvider::default();
        provider.records.lock().expect("records").insert(
            "one.example".into(),
            vec![
                dns::ExistingRecord {
                    id: 10,
                    record_type: dns::RecordType::A,
                    name: "www".into(),
                    content: "203.0.113.10".into(),
                    priority: None,
                },
                dns::ExistingRecord {
                    id: 13,
                    record_type: dns::RecordType::A,
                    name: "workflows".into(),
                    content: "192.0.2.99".into(),
                    priority: None,
                },
            ],
        );
        provider
            .certificates
            .lock()
            .expect("certificates")
            .insert("two.example".into(), Some(active_certificate(20)));

        dispatch_dns(
            &[
                "--domain",
                "one.example",
                "--domain",
                "two.example",
                "--gateway-ip",
                "203.0.113.10",
                "--redirect-apex-to-www",
                "--apply",
            ],
            &provider,
        )
        .expect("apply dispatch");

        assert_eq!(
            *provider.record_mutations.lock().expect("record mutations"),
            [
                "update:one.example:13:A:workflows",
                "create:one.example:URL:",
                "create:two.example:A:www",
                "create:two.example:A:workflows",
                "create:two.example:URL:",
            ]
        );
        assert_eq!(
            *provider
                .certificate_mutations
                .lock()
                .expect("certificate mutations"),
            ["order:one.example:auto-renew=true"]
        );
    }

    #[test]
    fn dispatch_keeps_unchanged_records_and_refuses_conflicts() {
        let unchanged = FakeProvider::default();
        unchanged.records.lock().expect("records").insert(
            "same.example".into(),
            vec![dns::ExistingRecord {
                id: 11,
                record_type: dns::RecordType::A,
                name: "www".into(),
                content: "203.0.113.10".into(),
                priority: None,
            }],
        );
        dispatch_dns(
            &[
                "--domain",
                "same.example",
                "--gateway-ip",
                "203.0.113.10",
                "--host",
                "www",
                "--apply",
            ],
            &unchanged,
        )
        .expect("unchanged apply");
        assert!(unchanged
            .record_mutations
            .lock()
            .expect("record mutations")
            .is_empty());

        let conflict = FakeProvider::default();
        conflict.records.lock().expect("records").insert(
            "conflict.example".into(),
            vec![dns::ExistingRecord {
                id: 12,
                record_type: dns::RecordType::Cname,
                name: "www".into(),
                content: "old.example.net".into(),
                priority: None,
            }],
        );
        let err = dispatch_dns(
            &[
                "--domain",
                "conflict.example",
                "--gateway-ip",
                "203.0.113.10",
                "--host",
                "www",
                "--apply",
            ],
            &conflict,
        )
        .expect_err("conflicting CNAME must stop apply");
        assert!(format!("{err:#}").contains("CNAME"));
        assert!(conflict
            .record_mutations
            .lock()
            .expect("record mutations")
            .is_empty());
    }
}

#[cfg(test)]
mod kms_operator_cli_tests {
    use clap::Parser as _;

    #[test]
    fn setup_parses_an_explicit_operator_member() {
        let cli = crate::Cli::try_parse_from([
            "navigator",
            "ops",
            "gcp",
            "setup",
            "--project-id",
            "example-test",
            "--public-base-url",
            "https://www.example.test",
            "--kms-operator",
            "user:operator@example.com",
            "--dry-run",
        ])
        .unwrap();
        let Some(crate::Command::Ops(crate::OpsCmd::Gcp(super::GcpCmd::Setup {
            kms_operator,
            dry_run,
            ..
        }))) = cli.command
        else {
            panic!("expected setup command");
        };
        assert_eq!(kms_operator.as_deref(), Some("user:operator@example.com"));
        assert!(dry_run);
    }
}

/// Tests for the pure KIND-tier render and argv helpers: the secret-set
/// newline strip, assignment folding, the Rauthy patch shapes, and the
/// kind-config port rewrite. Each test asserts evidence unique to the path.
#[cfg(test)]
mod devx_render_tests {
    use super::*;

    /// `ops secrets set` strips exactly one trailing newline (LF or CRLF)
    /// from the piped value and leaves every other byte alone.
    #[test]
    fn strip_one_trailing_newline_strips_lf_crlf_and_nothing_else() {
        let mut value = b"secret\n".to_vec();
        strip_one_trailing_newline(&mut value);
        assert_eq!(value, b"secret");

        let mut crlf = b"secret\r\n".to_vec();
        strip_one_trailing_newline(&mut crlf);
        assert_eq!(crlf, b"secret");

        // A blank line is not "no value": one LF is stripped, one byte stays.
        let mut blank = b"\n".to_vec();
        strip_one_trailing_newline(&mut blank);
        assert_eq!(blank, b"");

        // No trailing newline: nothing is stripped.
        let mut bare = b"secret".to_vec();
        strip_one_trailing_newline(&mut bare);
        assert_eq!(bare, b"secret");

        // Only ONE newline is stripped — a deliberate trailing blank line stays.
        let mut double = b"secret\n\n".to_vec();
        strip_one_trailing_newline(&mut double);
        assert_eq!(double, b"secret\n");

        let mut empty = Vec::new();
        strip_one_trailing_newline(&mut empty);
        assert!(empty.is_empty());
    }

    /// Assignments fold into a map and refuse a duplicate key — the caller
    /// gets an error naming the key instead of a silently-won overwrite.
    #[test]
    fn assignments_fold_pairs_and_reject_duplicates() {
        let folded = assignments(vec![("B".into(), "2".into()), ("A".into(), "1".into())]).unwrap();
        let keys: Vec<&str> = folded.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["A", "B"], "BTreeMap orders the output");
        assert_eq!(folded["A"], "1");

        let duplicate = assignments(vec![("A".into(), "1".into()), ("A".into(), "2".into())])
            .unwrap_err()
            .to_string();
        assert!(
            duplicate.contains("A is assigned more than once"),
            "{duplicate}"
        );
        // The first assignment wins nothing — the whole call fails.
        assert_eq!(assignments(vec![]).unwrap().len(), 0);
    }

    /// The Rauthy loopback patch is a `ConfigMap` patch whose nginx.conf
    /// proxies this tier's port to the in-cluster Rauthy.
    #[test]
    fn rauthy_loopback_patch_names_the_tiers_port() {
        let patch = rauthy_loopback_config_patch(31080);
        let parsed: serde_json::Value = serde_json::from_str(&patch).expect("valid JSON patch");
        let conf = parsed["data"]["nginx.conf"].as_str().expect("nginx.conf");
        assert!(conf.contains("listen 127.0.0.1:31080;"), "{conf}");
        assert!(
            conf.contains("proxy_set_header Host localhost:31080;"),
            "{conf}"
        );
        assert!(conf.contains("proxy_pass http://rauthy:8080;"), "{conf}");
    }

    /// The kind-config rewrite swaps only the three ingress host ports for
    /// this tier's values and is byte-identical at the defaults.
    #[test]
    fn render_kind_config_rewrites_this_tiers_ports_and_keeps_the_rest() {
        let template = "- containerPort: 80\n  hostPort: 80\n- containerPort: 443\n  hostPort: 443\n- containerPort: 30080\n  hostPort: 30080\n- containerPort: 9999\n  hostPort: 9999";
        let mut cfg = super::tests::default_cfg();
        cfg.ingress_http_port = 8000;
        cfg.ingress_https_port = 8443;
        cfg.rauthy_port = 31080;

        let rendered = render_kind_config(template, &cfg);
        // Every mapped host port is rewritten, the container ports and the
        // unmapped entry are left alone, and the line structure is exact.
        assert_eq!(
            rendered,
            "- containerPort: 80\n  hostPort: 8000\n- containerPort: 443\n  hostPort: 8443\n- containerPort: 30080\n  hostPort: 31080\n- containerPort: 9999\n  hostPort: 9999"
        );

        // At the true defaults the rewrite is a no-op, byte for byte.
        let defaults_template = "- containerPort: 80\n  hostPort: 8080\n- containerPort: 443\n  hostPort: 8443\n- containerPort: 30080\n  hostPort: 30080\n- containerPort: 9999\n  hostPort: 9999";
        let defaults = render_kind_config(defaults_template, &super::tests::default_cfg());
        assert_eq!(defaults, defaults_template);
    }

    /// Nested-Docker clusters get kube-proxy switched to nftables — but only
    /// when the config does not already choose a mode, and only when the body
    /// really is a KIND config.
    #[test]
    fn inject_kube_proxy_nftables_inserts_once_and_respects_an_existing_mode() {
        let without = "apiVersion: kind.x-k8s.io/v1alpha4\nnodes:\n- role: control-plane\n";
        let injected = inject_kube_proxy_nftables(without);
        assert_eq!(
            injected,
            "apiVersion: kind.x-k8s.io/v1alpha4\nnetworking:\n  kubeProxyMode: nftables\nnodes:\n- role: control-plane\n"
        );
        // An existing mode is never overridden.
        assert_eq!(
            inject_kube_proxy_nftables(
                "apiVersion: kind.x-k8s.io/v1alpha4\nnetworking:\n  kubeProxyMode: iptables\n"
            ),
            "apiVersion: kind.x-k8s.io/v1alpha4\nnetworking:\n  kubeProxyMode: iptables\n"
        );
        // Not a KIND config: untouched.
        assert_eq!(
            inject_kube_proxy_nftables("random: yaml\n"),
            "random: yaml\n"
        );
    }

    /// The use-context argv pins every kubectl call to this tier's context.
    #[test]
    fn use_context_args_target_the_named_context() {
        assert_eq!(
            use_context_args("wt-7"),
            vec!["config", "use-context", "wt-7"]
        );
        assert_eq!(
            undeploy_args("wt-7", "navigator"),
            vec![
                "--context",
                "wt-7",
                "delete",
                "--ignore-not-found",
                "namespace",
                "navigator"
            ]
        );
    }
}
