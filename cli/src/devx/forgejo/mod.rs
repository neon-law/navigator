//! `navigator dev forgejo` — the opt-in Forgejo in the local KIND tier.
//!
//! `up` adds Forgejo to a KIND tier: the retained PostgreSQL server with its
//! least-privilege roles, Forgejo's own login and database, Forgejo itself,
//! and one Actions runner, all from `k8s/overlays/kind/forgejo`. It signs in
//! through the tier's Rauthy. `smoke` proves that deployment end to end with
//! synthetic users and repositories only. `.github/workflows/forgejo-postgres.yml`
//! runs the same two commands against a fresh KIND cluster.
//!
//! This file holds the pure decisions and renderings, unit-tested here. The
//! cluster and HTTP orchestration lives in `cluster.rs` and `smoke.rs`, which
//! shell out to `kubectl`/`git` and are proven by the CI KIND job instead.
//! See `docs/forgejo-kind.md`.

pub(in crate::devx) mod bundle;
mod cluster;
mod oidc;
pub(in crate::devx) mod policy;
pub(in crate::devx) mod runner;
mod smoke;

use std::collections::BTreeMap;

use anyhow::{bail, ensure, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};

pub(super) use cluster::up;
pub(super) use smoke::smoke;

/// The opt-in overlay `up` applies.
const OVERLAY: &str = "k8s/overlays/kind/forgejo";
/// Rauthy's KIND fixture and its reusable Deployment, applied when the
/// cluster has no Rauthy yet (a bare CI cluster).
const RAUTHY_FIXTURE: &str = "k8s/overlays/kind/rauthy";
const RAUTHY_DEPLOYMENT: &str = "k8s/staging/rauthy.yaml";
/// The issuer authority the committed KIND manifests carry; `up` rewrites it
/// to the tier's own Rauthy port.
const FIXTURE_RAUTHY_AUTHORITY: &str = "localhost:30080";
const FIXTURE_LOOPBACK_LISTEN: &str = "127.0.0.1:30080";

const STATEFULSET: &str = "navigator-forgejo";
const POD: &str = "navigator-forgejo-0";
const DATA_CLAIM: &str = "data-navigator-forgejo-0";
const POSTGRES_POD: &str = "navigator-postgres-0";
const RUNNER_DEPLOYMENT: &str = "navigator-forgejo-runner";
const RUNNER_NAME: &str = "kind-runner";
/// The runner's one label. Jobs that ask for `runs-on: self-hosted` run in
/// the runner container itself (the `host` executor).
const RUNNER_LABEL: &str = "self-hosted";

/// The Secret holding the synthetic material `up` generates once per tier.
/// Every other KIND Secret is derived from it, so re-running `up` converges
/// instead of rotating.
const SEED_SECRET: &str = "navigator-forgejo-kind";
const ADMISSION_CONFIG_MAP: &str = "navigator-forgejo-admission";
/// The KIND instance signing key: an SSH key pair `up` generates once and
/// mounts read-only, so Forgejo can sign the merges a branch requiring signed
/// commits accepts.
const SIGNING_SECRET: &str = "navigator-forgejo-signing";
const SIGNING_PUBLIC_KEY: &str = "/etc/forgejo-signing/signing_key.pub";
/// The committer identity on Forgejo-signed commits in the KIND tier.
const SIGNING_NAME: &str = "Navigator Forgejo KIND";
const SIGNING_EMAIL: &str = "forgejo-signing@example.test";
const ALLOWLIST_KEY: &str = "email_domain_allowlist";

/// Synthetic credentials for one KIND tier. Never deployment material.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seed {
    postgres_password: String,
    app_password: String,
    migrator_password: String,
    backup_password: String,
    database_password: String,
    secret_key: String,
    internal_token: String,
    jwt_secret: String,
    runner_secret: String,
}

impl Seed {
    const KEYS: [&'static str; 9] = [
        "postgres_password",
        "app_password",
        "migrator_password",
        "backup_password",
        "database_password",
        "secret_key",
        "internal_token",
        "jwt_secret",
        "runner_secret",
    ];

    fn generate() -> Self {
        Self {
            postgres_password: random_hex(),
            app_password: random_hex(),
            migrator_password: random_hex(),
            backup_password: random_hex(),
            database_password: random_hex(),
            secret_key: random_hex(),
            internal_token: random_hex(),
            jwt_secret: URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>()),
            runner_secret: hex(&rand::random::<[u8; 20]>()),
        }
    }

    fn to_map(&self) -> BTreeMap<String, String> {
        Self::KEYS
            .iter()
            .zip([
                &self.postgres_password,
                &self.app_password,
                &self.migrator_password,
                &self.backup_password,
                &self.database_password,
                &self.secret_key,
                &self.internal_token,
                &self.jwt_secret,
                &self.runner_secret,
            ])
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect()
    }

    fn from_map(map: &BTreeMap<String, String>) -> Result<Self> {
        let get = |key: &str| {
            map.get(key)
                .filter(|value| !value.is_empty())
                .cloned()
                .with_context(|| format!("{SEED_SECRET} is missing `{key}`"))
        };
        let seed = Self {
            postgres_password: get("postgres_password")?,
            app_password: get("app_password")?,
            migrator_password: get("migrator_password")?,
            backup_password: get("backup_password")?,
            database_password: get("database_password")?,
            secret_key: get("secret_key")?,
            internal_token: get("internal_token")?,
            jwt_secret: get("jwt_secret")?,
            runner_secret: get("runner_secret")?,
        };
        ensure!(
            is_runner_secret(&seed.runner_secret),
            "{SEED_SECRET} `runner_secret` must be 40 lowercase hexadecimal characters"
        );
        Ok(seed)
    }
}

fn random_hex() -> String {
    hex(&rand::random::<[u8; 24]>())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn is_runner_secret(secret: &str) -> bool {
    secret.len() == 40
        && secret
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The browser origin Forgejo answers on in this tier.
fn root_url(forgejo_port: u16) -> String {
    format!("http://localhost:{forgejo_port}/")
}

/// Render the tier's base `app.ini` from the deployment template.
///
/// The template and its renderer are the ones `ops secrets apply --service
/// forgejo` uses, so KIND runs the production configuration. The only KIND
/// differences are the plain-HTTP loopback `ROOT_URL` and Actions, which a
/// deployment keeps off until its runner isolation is proven.
fn kind_app_ini(seed: &Seed, namespace: &str, forgejo_port: u16) -> Result<String> {
    let payloads = BTreeMap::from([
        (
            "NAVIGATOR_FORGEJO_DATABASE_PASSWORD".to_owned(),
            seed.database_password.clone(),
        ),
        (
            "NAVIGATOR_FORGEJO_SECRET_KEY".to_owned(),
            seed.secret_key.clone(),
        ),
        (
            "NAVIGATOR_FORGEJO_INTERNAL_TOKEN".to_owned(),
            seed.internal_token.clone(),
        ),
        (
            "NAVIGATOR_FORGEJO_OAUTH2_JWT_SECRET".to_owned(),
            seed.jwt_secret.clone(),
        ),
    ]);
    let rendered =
        super::deployments::render_forgejo_app_ini(&root_url(forgejo_port), namespace, &payloads)?;
    let disabled = "[actions]\nENABLED = false\n";
    ensure!(
        rendered.matches(disabled).count() == 1,
        "the Forgejo app.ini template must disable Actions exactly once"
    );
    // Forgejo signs authorized operations with its instance key; people sign
    // in through OIDC without enrolling a separate local factor or key.
    Ok(format!(
        "{}[repository.signing]\nFORMAT = ssh\nSIGNING_KEY = {SIGNING_PUBLIC_KEY}\n\
         SIGNING_NAME = {SIGNING_NAME}\nSIGNING_EMAIL = {SIGNING_EMAIL}\n\
         INITIAL_COMMIT = always\nCRUD_ACTIONS = always\nMERGES = always\n",
        rendered.replacen(disabled, "[actions]\nENABLED = true\n", 1)
    ))
}

/// The runner UUID Forgejo derives from a shared registration secret: the
/// ASCII bytes of its first 16 characters.
fn runner_uuid(secret: &str) -> Result<String> {
    ensure!(
        is_runner_secret(secret),
        "a runner secret is 40 lowercase hexadecimal characters"
    );
    let bytes: [u8; 16] = secret.as_bytes()[..16].try_into().expect("checked length");
    Ok(uuid::Uuid::from_bytes(bytes).hyphenated().to_string())
}

/// The runner's `config.yml`: one connection to the in-cluster Service.
fn runner_config(seed: &Seed) -> Result<String> {
    let uuid = runner_uuid(&seed.runner_secret)?;
    Ok(format!(
        "log:\n  level: info\nrunner:\n  capacity: 1\n  labels:\n    - \"{RUNNER_LABEL}:host\"\n\
         cache:\n  enabled: false\nserver:\n  connections:\n    navigator:\n      \
         url: http://navigator-forgejo:3000/\n      uuid: {uuid}\n      token: {token}\n",
        token = seed.runner_secret,
    ))
}

/// The synthetic workflows the smoke's merge proof commits: the required
/// gate, the publication that runs on `main`, and the bot push that must fail.
const GATE_WORKFLOW: &str = include_str!("fixtures/gate.yaml");
const PUBLISH_WORKFLOW: &str = include_str!("fixtures/publish.yaml");
const BOT_PUSH_WORKFLOW: &str = include_str!("fixtures/bot-push.yaml");

/// The commit-status context Forgejo Actions posts for a one-job workflow on
/// `event`: `<workflow name> / <job name> (<event>)`, composed as in Forgejo
/// 15.0.9's `services/actions/commit_status.go`. A job without `name` is
/// named by its key.
fn actions_context(workflow: &str, event: &str) -> Result<String> {
    let parsed: serde_yaml::Value = serde_yaml::from_str(workflow).context("parse the workflow")?;
    let name = parsed["name"]
        .as_str()
        .filter(|name| !name.trim().is_empty())
        .context("the workflow names itself")?;
    let jobs = parsed["jobs"]
        .as_mapping()
        .context("the workflow has jobs")?;
    let mut jobs = jobs.iter();
    let (Some((key, job)), None) = (jobs.next(), jobs.next()) else {
        bail!("the workflow has exactly one job");
    };
    let job = job["name"]
        .as_str()
        .or_else(|| key.as_str())
        .context("the job has a name")?;
    Ok(format!("{name} / {job} ({event})"))
}

/// Point rendered KIND manifests at this tier's Rauthy port: the issuer
/// authority and the pod-local loopback bridge that serves it.
fn align_issuer(rendered: &str, rauthy_port: u16) -> String {
    rendered
        .replace(
            FIXTURE_RAUTHY_AUTHORITY,
            &format!("localhost:{rauthy_port}"),
        )
        .replace(FIXTURE_LOOPBACK_LISTEN, &format!("127.0.0.1:{rauthy_port}"))
}

/// Solve a Rauthy proof-of-work challenge (`spow` version 1): the shortest
/// decimal counter whose SHA-256 over `challenge || counter` begins with the
/// challenge's difficulty in zero bits. The solution is `challenge || counter`.
fn solve_pow(challenge: &str) -> Result<String> {
    let mut fields = challenge.split(':');
    ensure!(
        fields.next() == Some("1"),
        "unsupported Rauthy proof-of-work version in {challenge:?}"
    );
    let difficulty: u32 = fields
        .next()
        .and_then(|field| field.parse().ok())
        .filter(|difficulty| (10..99).contains(difficulty))
        .with_context(|| format!("unreadable proof-of-work difficulty in {challenge:?}"))?;
    for counter in 0u64.. {
        let candidate = format!("{challenge}{counter}");
        if leading_zero_bits(&Sha256::digest(candidate.as_bytes())) >= difficulty {
            return Ok(candidate);
        }
    }
    bail!("proof-of-work search exhausted")
}

fn leading_zero_bits(digest: &[u8]) -> u32 {
    let mut bits = 0;
    for byte in digest {
        if *byte == 0 {
            bits += 8;
        } else {
            return bits + byte.leading_zeros();
        }
    }
    bits
}

/// A minimal cookie jar: name → value, fed from `Set-Cookie` headers.
#[derive(Debug, Default, Clone)]
struct CookieJar(BTreeMap<String, String>);

impl CookieJar {
    fn store<'a>(&mut self, set_cookie: impl IntoIterator<Item = &'a str>) {
        for header in set_cookie {
            let pair = header.split(';').next().unwrap_or_default();
            if let Some((name, value)) = pair.split_once('=') {
                let name = name.trim();
                if value.is_empty() {
                    self.0.remove(name);
                } else {
                    self.0.insert(name.to_owned(), value.trim().to_owned());
                }
            }
        }
    }

    fn header(&self) -> Option<String> {
        (!self.0.is_empty()).then(|| {
            self.0
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join("; ")
        })
    }
}

/// A Rauthy login page's server-rendered `<template id="tpl_…">` value.
fn rauthy_template(html: &str, id: &str) -> Option<String> {
    let start = html.find(&format!("<template id=\"{id}\">"))? + id.len() + 16;
    let end = html[start..].find("</template>")? + start;
    Some(html[start..end].to_owned())
}

/// A Forgejo form's hidden `_csrf` value.
fn forgejo_csrf(html: &str) -> Option<String> {
    let at = html.find("name=\"_csrf\"")?;
    let rest = &html[at..];
    let value = rest.find("value=\"")? + 7;
    let end = rest[value..].find('"')? + value;
    Some(rest[value..end].to_owned())
}

/// The local port in a `kubectl port-forward` banner line.
fn forwarded_port(line: &str) -> Option<u16> {
    let rest = line.strip_prefix("Forwarding from 127.0.0.1:")?;
    rest.split_whitespace().next()?.parse().ok()
}

/// The path and query of an absolute URL, for replaying a redirect against a
/// port-forward rather than the host it names.
fn path_and_query(location: &str) -> Result<String> {
    let url = url::Url::parse(location).with_context(|| format!("parse URL {location}"))?;
    Ok(match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_owned(),
    })
}

/// The synthetic email-domain policy the smoke opens, and the identities it
/// signs in with. Each pairs with a user in the Rauthy KIND fixture.
const SMOKE_ALLOWLIST: &str = "example.test";
const ALLOWED_IDENTITY: &str = "forge-allowed@example.test";
const DENIED_IDENTITIES: [(&str, &str); 3] = [
    ("forge-denied@example.org", "a domain off the allowlist"),
    (
        "forge-lookalike@notexample.test",
        "a suffix lookalike of an allowed domain",
    ),
    ("forge-unverified@example.test", "an unverified address"),
];
const FIXTURE_PASSWORD: &str = "password";

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// Synthetic material is assembled at runtime so no credential-shaped
    /// literal sits in the tree (see `.gitleaks.toml`).
    const SYNTHETIC_HEX: &str = "0123456789abcdef0123456789abcdef";

    fn synthetic_runner_secret() -> String {
        format!("{SYNTHETIC_HEX}{}", &SYNTHETIC_HEX[..8])
    }

    fn seed() -> Seed {
        Seed {
            postgres_password: "synthetic-postgres".into(),
            app_password: "synthetic-app".into(),
            migrator_password: "synthetic-migrator".into(),
            backup_password: "synthetic-backup".into(),
            database_password: "synthetic-forgejo".into(),
            secret_key: "synthetic-secret-key".into(),
            internal_token: "synthetic-internal-token".into(),
            jwt_secret: URL_SAFE_NO_PAD.encode(SYNTHETIC_HEX.as_bytes()),
            runner_secret: synthetic_runner_secret(),
        }
    }

    #[test]
    fn a_generated_seed_round_trips_through_its_secret() {
        let generated = Seed::generate();
        assert_eq!(Seed::from_map(&generated.to_map()).unwrap(), generated);
        assert!(is_runner_secret(&generated.runner_secret));
        assert_eq!(
            URL_SAFE_NO_PAD.decode(&generated.jwt_secret).unwrap().len(),
            32
        );
        assert_ne!(Seed::generate(), generated, "each tier gets its own seed");
    }

    #[test]
    fn an_incomplete_seed_is_refused_by_name() {
        let mut map = seed().to_map();
        map.remove("runner_secret");
        let error = Seed::from_map(&map).unwrap_err().to_string();
        assert!(error.contains("runner_secret"), "{error}");
        let mut map = seed().to_map();
        map.insert("runner_secret".into(), "short".into());
        assert!(Seed::from_map(&map).is_err());
    }

    #[test]
    fn the_kind_app_ini_is_the_deployment_template_with_actions_on() {
        let ini = kind_app_ini(&seed(), "navigator", 3300).unwrap();
        assert!(ini.contains("ROOT_URL = http://localhost:3300/"));
        assert!(ini.contains("HOST = navigator-postgres.navigator.svc.cluster.local:5432"));
        assert!(ini.contains("PASSWD = synthetic-forgejo"));
        assert!(ini.contains("SSL_MODE = verify-full"));
        assert!(ini.contains("[actions]\nENABLED = true\n"));
        assert!(!ini.contains("ENABLED = false\n[log]"));
        assert!(ini.ends_with(
            "[repository.signing]\nFORMAT = ssh\n\
             SIGNING_KEY = /etc/forgejo-signing/signing_key.pub\n\
             SIGNING_NAME = Navigator Forgejo KIND\n\
             SIGNING_EMAIL = forgejo-signing@example.test\n\
             INITIAL_COMMIT = always\nCRUD_ACTIONS = always\nMERGES = always\n"
        ));
        // Registration stays exactly as closed as a deployment's.
        assert!(ini.contains("DISABLE_REGISTRATION = true"));
        assert!(ini.contains("ENABLE_INTERNAL_SIGNIN = false"));
        assert!(ini.contains("ENABLE_AUTO_REGISTRATION = false"));
        assert!(ini.contains("ACCOUNT_LINKING = disabled"));
        assert!(!ini.contains("__NAVIGATOR_"));
    }

    #[test]
    fn the_merge_fixtures_name_their_contexts_and_triggers() {
        assert_eq!(
            actions_context(GATE_WORKFLOW, "pull_request").unwrap(),
            "gate / gate (pull_request)"
        );
        assert_eq!(
            actions_context(PUBLISH_WORKFLOW, "push").unwrap(),
            "publish / publish (push)"
        );
        let on = |workflow: &str| {
            serde_yaml::from_str::<serde_yaml::Value>(workflow).unwrap()["on"].clone()
        };
        assert_eq!(
            on(GATE_WORKFLOW),
            serde_yaml::from_str::<serde_yaml::Value>("[pull_request]").unwrap()
        );
        assert_eq!(
            on(PUBLISH_WORKFLOW),
            serde_yaml::from_str::<serde_yaml::Value>("{push: {branches: [main]}}").unwrap()
        );
        assert_eq!(
            on(BOT_PUSH_WORKFLOW),
            serde_yaml::from_str::<serde_yaml::Value>("[workflow_dispatch]").unwrap()
        );
        // Each runs on the KIND runner's only label.
        for workflow in [GATE_WORKFLOW, PUBLISH_WORKFLOW, BOT_PUSH_WORKFLOW] {
            assert!(workflow.contains(&format!("runs-on: {RUNNER_LABEL}\n")));
        }
    }

    /// The gate a Forgejo-backed Project repository is scaffolded with posts
    /// the context `main`'s branch protection requires.
    #[test]
    fn the_scaffolded_project_gate_posts_the_required_context() {
        use crate::projects::repository::{forgejo_gate_workflow, FORGEJO_REQUIRED_CHECK};
        let workflow = forgejo_gate_workflow();
        assert_eq!(
            actions_context(&workflow, "pull_request").unwrap(),
            FORGEJO_REQUIRED_CHECK
        );
        assert!(workflow.contains(&format!("runs-on: {RUNNER_LABEL}\n")));
    }

    #[test]
    fn an_actions_context_needs_a_named_workflow_with_one_job() {
        let named_job = "name: ci\njobs:\n  build:\n    name: Build\n    runs-on: x\n";
        assert_eq!(
            actions_context(named_job, "push").unwrap(),
            "ci / Build (push)"
        );
        for workflow in [
            "jobs:\n  build:\n    runs-on: x\n",
            "name: ci\n",
            "name: ci\njobs:\n  a:\n    runs-on: x\n  b:\n    runs-on: x\n",
            "name: \"\"\njobs:\n  a:\n    runs-on: x\n",
        ] {
            assert!(actions_context(workflow, "push").is_err(), "{workflow}");
        }
    }

    #[test]
    fn the_runner_uuid_matches_forgejos_shared_secret_derivation() {
        // `forgejo-runner create-runner-file --secret 0123…4567` writes this
        // UUID, and `forgejo forgejo-cli actions register` stores the same.
        assert_eq!(
            runner_uuid(&synthetic_runner_secret()).unwrap(),
            "30313233-3435-3637-3839-616263646566"
        );
        assert!(runner_uuid("not-a-secret").is_err());
    }

    #[test]
    fn the_runner_config_connects_to_the_service_with_its_secret() {
        let config = runner_config(&seed()).unwrap();
        assert!(config.contains("url: http://navigator-forgejo:3000/"));
        assert!(config.contains("uuid: 30313233-3435-3637-3839-616263646566"));
        assert!(config.contains(&format!("token: {}", synthetic_runner_secret())));
        assert!(config.contains("\"self-hosted:host\""));
        assert!(config.contains("cache:\n  enabled: false"));
    }

    #[test]
    fn align_issuer_moves_every_fixture_issuer_to_the_tier_port() {
        let rendered =
            "discovery_url: http://localhost:30080/auth/v1/.well-known/openid-configuration\n\
                        listen 127.0.0.1:30080;\nnodePort: 30080\n";
        let aligned = align_issuer(rendered, 20_445);
        assert!(aligned.contains("http://localhost:20445/auth/v1/"));
        assert!(aligned.contains("listen 127.0.0.1:20445;"));
        assert!(
            aligned.contains("nodePort: 30080"),
            "only the issuer authority moves; the NodePort stays fixed"
        );
        assert_eq!(align_issuer(rendered, 30_080), rendered);
    }

    #[test]
    fn solve_pow_meets_the_challenge_difficulty() {
        let challenge =
            "1:12:1791857091:ZPIaeOfkFH+clnVg:XzkjR30s2DMDxin9lN1QSoj86ixaxNfkRGRIrdPhfp4:";
        let solution = solve_pow(challenge).unwrap();
        let counter = solution.strip_prefix(challenge).unwrap();
        assert!(counter.parse::<u64>().is_ok());
        assert!(leading_zero_bits(&Sha256::digest(solution.as_bytes())) >= 12);
        assert!(solve_pow("2:12:x:y:z:").is_err());
        assert!(solve_pow("1:9:x:y:z:").is_err());
    }

    #[test]
    fn leading_zero_bits_counts_across_bytes() {
        assert_eq!(leading_zero_bits(&[0, 0b0001_0000, 0xff]), 11);
        assert_eq!(leading_zero_bits(&[0x80]), 0);
        assert_eq!(leading_zero_bits(&[0, 0]), 16);
    }

    #[test]
    fn the_cookie_jar_keeps_the_latest_value_and_drops_cleared_cookies() {
        let mut jar = CookieJar::default();
        assert_eq!(jar.header(), None);
        jar.store(["i_like_forgejo=abc; Path=/; HttpOnly", "_csrf=one; Path=/"]);
        jar.store(["_csrf=two; Path=/", "i_like_forgejo=; Max-Age=0"]);
        assert_eq!(jar.header().as_deref(), Some("_csrf=two"));
    }

    #[test]
    fn page_values_are_read_from_rauthy_and_forgejo_markup() {
        let rauthy = r#"<template id="tpl_is_reg_open">false</template><template id="tpl_csrf_token">Q4rY</template>"#;
        assert_eq!(
            rauthy_template(rauthy, "tpl_csrf_token").as_deref(),
            Some("Q4rY")
        );
        assert_eq!(rauthy_template(rauthy, "tpl_missing"), None);
        let forgejo = r#"<form><input type="hidden" name="_csrf" value="tok-123"><input name="user_name"></form>"#;
        assert_eq!(forgejo_csrf(forgejo).as_deref(), Some("tok-123"));
        assert_eq!(forgejo_csrf("<form></form>"), None);
    }

    #[test]
    fn forwarded_port_reads_kubectls_banner() {
        assert_eq!(
            forwarded_port("Forwarding from 127.0.0.1:54321 -> 3000"),
            Some(54_321)
        );
        assert_eq!(forwarded_port("Forwarding from [::1]:54321 -> 3000"), None);
        assert_eq!(forwarded_port("Handling connection for 54321"), None);
    }

    #[test]
    fn path_and_query_drops_the_origin() {
        assert_eq!(
            path_and_query("http://localhost:3300/user/oauth2/rauthy/callback?code=a&state=b")
                .unwrap(),
            "/user/oauth2/rauthy/callback?code=a&state=b"
        );
        assert_eq!(path_and_query("http://localhost:3300/").unwrap(), "/");
    }

    #[test]
    fn the_deployment_template_uses_oidc_without_a_local_second_factor() {
        let template =
            std::fs::read_to_string(workspace().join("k8s/forgejo/app.ini.example")).unwrap();
        let security = template
            .split("[security]")
            .nth(1)
            .and_then(|rest| rest.split("\n[").next())
            .unwrap();
        assert!(security
            .lines()
            .any(|line| line == "GLOBAL_TWO_FACTOR_REQUIREMENT = none"));
        let identity =
            std::fs::read_to_string(workspace().join("k8s/forgejo/identity.sh")).unwrap();
        assert!(
            identity.contains("--skip-local-2fa"),
            "the OIDC source must not add a local second-factor challenge"
        );
        // Forgejo checks the reserved administrator's address against
        // EMAIL_DOMAIN_ALLOWLIST, so a fixed `forgejo.invalid` crash-loops every
        // environment whose admission opened registration (staging, 26.10.16-rc.2).
        assert!(
            identity.contains("EMAIL_DOMAIN_ALLOWLIST")
                && identity.contains("navigator-admin@${admin_domain:-forgejo.invalid}"),
            "the reserved administrator must take an allowlisted domain"
        );
    }

    #[test]
    fn every_smoke_identity_is_a_synthetic_rauthy_fixture_user() {
        let fixture = std::fs::read_to_string(
            workspace().join("k8s/overlays/kind/rauthy/local-fixture.yaml"),
        )
        .unwrap();
        for email in std::iter::once(ALLOWED_IDENTITY).chain(DENIED_IDENTITIES.map(|(e, _)| e)) {
            assert!(
                fixture.contains(&format!("\"email\": \"{email}\"")),
                "{email} must be a Rauthy fixture user"
            );
            let domain = email.split('@').nth(1).unwrap();
            assert!(
                domain == "example.test" || domain == "notexample.test" || domain == "example.org",
                "{email} must use a reserved example domain"
            );
        }
        assert!(fixture.contains("\"id\": \"navigator-forgejo\""));
    }

    fn workspace() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the cli crate has a workspace parent")
            .to_path_buf()
    }

    /// Run `k8s/forgejo/admission.sh` against a synthetic base config.
    fn admission(allowlist: Option<&str>) -> (String, String) {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let admission = dir.path().join("admission");
        let out = dir.path().join("out");
        for path in [&base, &admission, &out] {
            std::fs::create_dir(path).unwrap();
        }
        let ini = kind_app_ini(&seed(), "navigator", 3300).unwrap();
        std::fs::write(base.join("app.ini"), ini).unwrap();
        std::fs::write(base.join("oauth2_jwt_secret"), "synthetic").unwrap();
        if let Some(allowlist) = allowlist {
            std::fs::write(admission.join(ALLOWLIST_KEY), allowlist).unwrap();
        }
        let output = Command::new("sh")
            .arg(workspace().join("k8s/forgejo/admission.sh"))
            .env("FORGEJO_BASE_CONFIG_DIR", &base)
            .env("FORGEJO_ADMISSION_DIR", &admission)
            .env("FORGEJO_CONFIG_DIR", &out)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(out.join("oauth2_jwt_secret").is_file());
        (
            std::fs::read_to_string(out.join("app.ini")).unwrap(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn assert_closed(ini: &str) {
        assert!(ini.contains("DISABLE_REGISTRATION = true\n"), "{ini}");
        assert!(ini.contains("EMAIL_DOMAIN_ALLOWLIST = \n"), "{ini}");
        assert!(ini.contains("ENABLE_AUTO_REGISTRATION = false\n"), "{ini}");
    }

    #[test]
    fn a_missing_or_empty_allowlist_admits_no_one() {
        assert_closed(&admission(None).0);
        assert_closed(&admission(Some("")).0);
        assert_closed(&admission(Some(" \n")).0);
    }

    #[test]
    fn a_malformed_allowlist_admits_no_one() {
        for allowlist in [
            "*",
            "*.example.test",
            "example",
            "example.test,",
            "example.test;evil.test",
            "exa mple.test",
            "-example.test",
            "example.test\nDISABLE_REGISTRATION = false",
        ] {
            let (ini, stderr) = admission(Some(allowlist));
            assert_closed(&ini);
            assert!(
                stderr.contains("registration stays closed"),
                "{allowlist:?}: {stderr}"
            );
            assert_eq!(
                ini.matches("DISABLE_REGISTRATION").count(),
                1,
                "{allowlist:?}"
            );
        }
    }

    #[test]
    fn a_valid_allowlist_opens_only_external_registration_for_those_domains() {
        let (ini, _) = admission(Some("Example.TEST, brand.example.org\n"));
        assert!(ini.contains("DISABLE_REGISTRATION = false\n"));
        assert!(ini.contains("EMAIL_DOMAIN_ALLOWLIST = example.test,brand.example.org\n"));
        assert!(ini.contains("ENABLE_AUTO_REGISTRATION = true\n"));
        // Every other gate stays shut: no local signup, no password sign-in,
        // no OpenID 2.0 signup, and no linking an external login by email.
        assert!(ini.contains("ALLOW_ONLY_EXTERNAL_REGISTRATION = true"));
        assert!(ini.contains("ENABLE_INTERNAL_SIGNIN = false"));
        assert!(ini.contains("ENABLE_OPENID_SIGNUP = false"));
        assert!(ini.contains("ACCOUNT_LINKING = disabled"));
    }
}
