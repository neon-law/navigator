//! `dev forgejo smoke`: prove a `dev forgejo up` deployment end to end.
//!
//! 1. PostgreSQL: Forgejo connects over TLS, its login is unprivileged, owns
//!    only its own database, and cannot reach Navigator's.
//! 2. Registration: local signup and the admin API refuse an anonymous
//!    caller, and an empty allowlist creates no account through OIDC.
//! 3. Admission: with `example.test` allowed, a verified allowed address gets
//!    an account bound to its issuer subject; another domain, a suffix
//!    lookalike, and an unverified address get none.
//! 4. OIDC sign-in opens settings directly without a local second-factor
//!    enrollment, and the account can use its scoped API credential.
//! 5. Git and Actions: the account clones, commits, and pushes a private
//!    repository over HTTP, and the pushed workflow runs on the KIND runner.
//! 6. Merge policy: drift-only reads, convergent protection, removed-check
//!    repair, and rejected direct pushes by ordinary and admin identities.
//! 7. Squash merge: in a repository whose every `main` commit is signed, the
//!    gate workflow's own pull-request context becomes the required check. A
//!    ready, signed, green pull request squash-merges on its own with its
//!    title and body, its branch is deleted, and the publication workflow
//!    runs on the merge commit. A red one stays open through an immediate and
//!    the owner's forced merge; a draft is refused until it is marked ready. Signed direct pushes by the account and an
//!    administrator, and an Actions job token's push, are rejected.
//! 8. Repository path: a synthetic Firm and Project seeded in an in-memory
//!    store map through `store::firms::forgejo::repository_path`; the Firm's
//!    organization and the Project's private repository are created, and the
//!    API and database return exactly `<firm-slug>/<project-code>`.
//! 9. Retention: the commit, the repository row, and the claim survive pod
//!    deletion, then scale-to-zero and back; the account still signs in by
//!    the same issuer subject once the allowlist is closed again.
//!
//! It finishes with the allowlist empty. Every identity, repository,
//! and credential is synthetic. Shells out to `kubectl` and `git`
//! and talks HTTP to a port-forward, so it is excluded from the coverage gate.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};

use super::super::KindConfig;
use super::cluster::Kube;
use super::oidc::{self, Outcome};
use super::{
    actions_context, forwarded_port, CookieJar, Seed, ADMISSION_CONFIG_MAP, ALLOWED_IDENTITY,
    ALLOWLIST_KEY, BOT_PUSH_WORKFLOW, DATA_CLAIM, DENIED_IDENTITIES, GATE_WORKFLOW, POD,
    POSTGRES_POD, PUBLISH_WORKFLOW, SEED_SECRET, SMOKE_ALLOWLIST, STATEFULSET,
};

/// The allowed identity's Forgejo user name (`USERNAME = email` keeps the
/// address's local part).
const OWNER: &str = "forge-allowed";
const REPOSITORY: &str = "retained-proof";
/// The squash-merge proof's repository. Every commit on its `main` is signed.
const MERGE_REPOSITORY: &str = "merge-proof";
/// The synthetic Firm and Project the repository-path phase seeds.
const SMOKE_FIRM: &str = "Example Law PC";
const SMOKE_PROJECT: &str = "sample-litigation";
const WORKFLOW: &str = "on: [push]\n\
jobs:\n  proof:\n    runs-on: self-hosted\n    steps:\n      \
- run: echo \"navigator forgejo actions proof\"\n";

/// A `kubectl port-forward` to the Forgejo Service on an ephemeral loopback
/// port, killed on drop. A pod replacement breaks a forward, so each phase
/// opens a fresh one.
struct Forward {
    child: Child,
    origin: String,
}

impl Forward {
    fn open(kube: &Kube) -> Result<Self> {
        let mut child = kube
            .command()
            .args(["port-forward", &format!("service/{STATEFULSET}"), ":3000"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("spawn kubectl port-forward")?;
        let stdout = child.stdout.take().context("port-forward stdout")?;
        let mut lines = BufReader::new(stdout).lines();
        let port = loop {
            let Some(Ok(line)) = lines.next() else {
                let _ = child.kill();
                bail!("kubectl port-forward exited before it was ready");
            };
            if let Some(port) = forwarded_port(&line) {
                break port;
            }
        };
        // Keep draining so kubectl never blocks on a full pipe.
        std::thread::spawn(move || for _ in lines {});
        Ok(Self {
            child,
            origin: format!("http://127.0.0.1:{port}"),
        })
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `dev forgejo smoke`: the phases in the module documentation, in order.
#[allow(clippy::too_many_lines)] // One linear proof reads best as one function.
pub(in crate::devx) fn smoke(cfg: &KindConfig) -> Result<()> {
    super::super::require_tools(&["kubectl", "git"])?;
    let kube = Kube::new(cfg);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build the smoke runtime")?;
    let http = oidc::client()?;
    let seed = Seed::from_map(
        &kube
            .secret(SEED_SECRET)?
            .context("no KIND Forgejo here; run `navigator dev forgejo up` first")?,
    )?;

    if !current_allowlist(&kube)?.is_empty() {
        set_allowlist(&kube, "")?;
        replace_pod(&kube)?;
    }
    purge_smoke_accounts(&kube)?;

    step("PostgreSQL: TLS, an unprivileged login, and no reach into Navigator");
    database_proofs(&kube, &seed)?;

    let forward = Forward::open(&kube)?;
    step("registration: closed to local signup, the API, and OIDC");
    runtime.block_on(registration_closed(&kube, &http, &forward.origin))?;
    drop(forward);

    step(&format!(
        "admission: allow `{SMOKE_ALLOWLIST}` and restart Forgejo"
    ));
    set_allowlist(&kube, SMOKE_ALLOWLIST)?;
    replace_pod(&kube)?;
    let forward = Forward::open(&kube)?;
    let mut session = CookieJar::default();
    let subject = runtime.block_on(admission_open(&kube, &http, &forward.origin, &mut session))?;

    step("OIDC: the provider session opens settings and permits API access directly");
    let token = user_token(&kube)?;
    runtime.block_on(provider_session_proofs(
        &http,
        &forward.origin,
        &mut session,
        &token,
    ))?;

    step("git: clone, commit, and push a private repository over HTTP");
    let sha = runtime.block_on(push_repository(&http, &forward.origin, &token))?;
    step("Actions: the pushed workflow runs to success on the KIND runner");
    let repository = format!("{OWNER}/{REPOSITORY}");
    let status = runtime.block_on(wait_for_run(
        &http,
        &forward.origin,
        &token,
        &repository,
        "proof.yaml",
        &sha,
    ))?;
    ensure!(
        status == "success",
        "the proof workflow finished with {status}"
    );

    step(
        "merge policy: protected main, squash-only settings, drift repair, and denied direct pushes",
    );
    runtime.block_on(merge_policy_proofs(
        &kube,
        &http,
        &forward.origin,
        &token,
        &sha,
    ))?;

    step(
        "squash merge: the gate's own check, a green PR merging itself, publication on main, \
         and red, draft, direct, and bot paths refused",
    );
    runtime.block_on(squash_merge_proofs(&kube, &http, &forward.origin, &token))?;

    step("repository path: a seeded Firm's organization holds its Project's repository");
    let path = runtime.block_on(seeded_repository_path())?;
    ensure!(
        path.to_string() == format!("example-law-pc/{SMOKE_PROJECT}"),
        "the seeded Firm and Project map to {path}"
    );
    provision_firm_repository(&kube, &runtime, &http, &forward.origin, &token, &path)?;
    drop(forward);

    let claim = uid(&kube, "pvc", DATA_CLAIM)?;
    step("retention: delete the pod, keep the commit");
    let before = uid(&kube, "pod", POD)?;
    replace_pod(&kube)?;
    ensure!(
        uid(&kube, "pod", POD)? != before,
        "the Forgejo pod was not replaced"
    );
    ensure!(
        uid(&kube, "pvc", DATA_CLAIM)? == claim,
        "the Forgejo data claim changed"
    );
    let forward = Forward::open(&kube)?;
    runtime.block_on(verify_retained(&http, &forward.origin, &token, &sha))?;
    verify_repository_row(&kube)?;
    drop(forward);

    step("retention: scale to zero and back, closing the allowlist again");
    kube.run(&[
        "scale",
        &format!("statefulset/{STATEFULSET}"),
        "--replicas=0",
    ])?;
    kube.run(&[
        "wait",
        "--for=delete",
        &format!("pod/{POD}"),
        "--timeout=180s",
    ])?;
    ensure!(
        uid(&kube, "pvc", DATA_CLAIM)? == claim,
        "scaling to zero released the Forgejo data claim"
    );
    set_allowlist(&kube, "")?;
    kube.run(&[
        "scale",
        &format!("statefulset/{STATEFULSET}"),
        "--replicas=1",
    ])?;
    kube.wait_rollout(&format!("statefulset/{STATEFULSET}"), "600s")?;
    ensure!(
        uid(&kube, "pvc", DATA_CLAIM)? == claim,
        "the Forgejo data claim changed after scaling back"
    );
    let forward = Forward::open(&kube)?;
    runtime.block_on(verify_retained(&http, &forward.origin, &token, &sha))?;
    verify_repository_row(&kube)?;
    runtime.block_on(existing_account_keeps_its_subject(
        &kube,
        &http,
        &forward.origin,
        &subject,
    ))?;

    println!(
        "PASS: Forgejo on KIND PostgreSQL (TLS, isolated login), closed registration, \
         email-domain admission through Rauthy, direct OIDC sign-in, git clone/push, an Actions \
         run, protected main with rejected ordinary/admin pushes, a signed green PR \
         squash-merging itself with publication on its merge commit while red, draft, direct, \
         and bot paths are refused, a Firm organization's \
         Project repository at {path}, and retention across pod \
         deletion and scale-to-zero"
    );
    Ok(())
}

fn step(message: &str) {
    eprintln!("==> {message}");
}

fn uid(kube: &Kube, kind: &str, name: &str) -> Result<String> {
    let uid = kube.output(&["get", kind, name, "-o", "jsonpath={.metadata.uid}"])?;
    ensure!(!uid.trim().is_empty(), "{kind}/{name} has no UID");
    Ok(uid.trim().to_owned())
}

fn replace_pod(kube: &Kube) -> Result<()> {
    kube.run(&["delete", "pod", POD, "--wait=true"])?;
    kube.wait_rollout(&format!("statefulset/{STATEFULSET}"), "600s")
}

fn current_allowlist(kube: &Kube) -> Result<String> {
    if !kube.succeeds(&["get", "configmap", ADMISSION_CONFIG_MAP]) {
        return Ok(String::new());
    }
    Ok(kube
        .output(&[
            "get",
            "configmap",
            ADMISSION_CONFIG_MAP,
            "-o",
            &format!("jsonpath={{.data.{ALLOWLIST_KEY}}}"),
        ])?
        .trim()
        .to_owned())
}

/// Write the allowlist seam exactly as a later Brand-domain sync would. The
/// next Forgejo start reads it.
fn set_allowlist(kube: &Kube, allowlist: &str) -> Result<()> {
    let manifest = serde_yaml::to_string(&serde_json::json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {"name": ADMISSION_CONFIG_MAP, "namespace": kube.namespace()},
        "data": {ALLOWLIST_KEY: allowlist},
    }))
    .context("serialize the admission ConfigMap")?;
    kube.apply(&manifest)?;
    Ok(())
}

fn psql(kube: &Kube, database: &str, sql: &str) -> Result<String> {
    Ok(kube
        .output(&[
            "exec",
            POSTGRES_POD,
            "--",
            "psql",
            "-U",
            "postgres",
            "-d",
            database,
            "-v",
            "ON_ERROR_STOP=1",
            "-Atc",
            sql,
        ])?
        .trim()
        .to_owned())
}

/// Connect as the `forgejo` login over TCP inside the server pod.
fn as_forgejo(kube: &Kube, seed: &Seed, database: &str, sslmode: &str) -> Result<String> {
    let dsn = format!(
        "host=navigator-postgres.{namespace}.svc.cluster.local hostaddr=127.0.0.1 \
         dbname={database} user=forgejo sslmode={sslmode} sslrootcert=/etc/postgres/tls/tls.crt",
        namespace = kube.namespace()
    );
    let password = format!("PGPASSWORD={}", seed.database_password);
    kube.output(&[
        "exec",
        POSTGRES_POD,
        "--",
        "env",
        &password,
        "psql",
        &dsn,
        "-Atc",
        "SELECT 1",
    ])
}

fn database_proofs(kube: &Kube, seed: &Seed) -> Result<()> {
    let tls = psql(
        kube,
        "forgejo",
        "SELECT count(*) FROM pg_stat_ssl s JOIN pg_stat_activity a ON a.pid = s.pid \
         WHERE a.usename = 'forgejo' AND s.ssl",
    )?;
    ensure!(
        tls.parse::<u32>().unwrap_or(0) > 0,
        "Forgejo holds no TLS connection to PostgreSQL"
    );
    let plain = psql(
        kube,
        "forgejo",
        "SELECT count(*) FROM pg_stat_ssl s JOIN pg_stat_activity a ON a.pid = s.pid \
         WHERE a.usename = 'forgejo' AND NOT s.ssl",
    )?;
    ensure!(plain == "0", "Forgejo holds an unencrypted connection");
    let attributes = psql(
        kube,
        "forgejo",
        "SELECT rolsuper::text || rolcreaterole::text || rolcreatedb::text || \
         rolreplication::text || rolbypassrls::text FROM pg_roles WHERE rolname = 'forgejo'",
    )?;
    ensure!(
        attributes == "falsefalsefalsefalsefalse",
        "the forgejo login is privileged: {attributes}"
    );
    let owner = psql(
        kube,
        "forgejo",
        "SELECT pg_get_userbyid(datdba) FROM pg_database WHERE datname = 'forgejo'",
    )?;
    ensure!(
        owner == "forgejo",
        "the forgejo database is owned by {owner}"
    );
    let owned_databases = psql(
        kube,
        "forgejo",
        "SELECT count(*) FROM pg_database WHERE pg_get_userbyid(datdba) = 'forgejo'",
    )?;
    ensure!(
        owned_databases == "1",
        "the forgejo login owns {owned_databases} databases"
    );
    as_forgejo(kube, seed, "forgejo", "verify-full")
        .context("the forgejo login cannot reach its own database over verified TLS")?;
    ensure!(
        as_forgejo(kube, seed, "navigator", "verify-full").is_err(),
        "the forgejo login connected to the navigator database"
    );
    ensure!(
        as_forgejo(kube, seed, "forgejo", "disable").is_err(),
        "PostgreSQL accepted an unencrypted forgejo connection"
    );
    Ok(())
}

fn count_users(kube: &Kube, email: &str) -> Result<u32> {
    psql(
        kube,
        "forgejo",
        &format!("SELECT count(*) FROM \"user\" WHERE lower(email) = lower('{email}')"),
    )?
    .parse()
    .context("parse the user count")
}

/// Remove accounts earlier smoke runs created, so each run starts clean.
fn purge_smoke_accounts(kube: &Kube) -> Result<()> {
    let identities = std::iter::once(ALLOWED_IDENTITY)
        .chain(DENIED_IDENTITIES.map(|(email, _)| email))
        .chain(["forge-local-signup@example.test"]);
    for email in identities {
        let name = email.split('@').next().unwrap_or(email);
        if count_users(kube, email)? > 0 {
            kube.forgejo(
                &["admin", "user", "delete", "--username", name, "--purge"],
                None,
            )?;
        }
    }
    Ok(())
}

async fn registration_closed(kube: &Kube, http: &reqwest::Client, forge: &str) -> Result<()> {
    let (page, status) = oidc::try_local_signup(http, forge).await?;
    ensure!(
        !page.contains("name=\"user_name\""),
        "the sign-up page offers a local registration form"
    );
    ensure!(
        !(200..400).contains(&status) || count_users(kube, "forge-local-signup@example.test")? == 0,
        "local signup answered {status}"
    );
    ensure!(
        count_users(kube, "forge-local-signup@example.test")? == 0,
        "local signup created an account"
    );

    let api = http
        .post(format!("{forge}/api/v1/admin/users"))
        .json(&serde_json::json!({
            "username": "forge-api-signup",
            "email": "forge-api-signup@example.test",
            "password": "synthetic-api-password",
        }))
        .send()
        .await
        .context("POST /api/v1/admin/users")?;
    ensure!(
        matches!(api.status().as_u16(), 401 | 403),
        "anonymous API user creation answered {}",
        api.status()
    );

    let mut jar = CookieJar::default();
    let outcome = oidc::sign_in(http, forge, ALLOWED_IDENTITY, &mut jar).await?;
    ensure!(
        outcome == Outcome::LinkAccount,
        "with an empty allowlist {ALLOWED_IDENTITY} reached {outcome:?}, not the link-account page"
    );
    let status = oidc::try_link_account_signup(http, forge, ALLOWED_IDENTITY, &mut jar).await?;
    ensure!(
        count_users(kube, ALLOWED_IDENTITY)? == 0,
        "an empty allowlist still created an account for {ALLOWED_IDENTITY} (link signup answered {status})"
    );
    Ok(())
}

fn api(
    http: &reqwest::Client,
    token: &str,
    method: reqwest::Method,
    url: &str,
) -> reqwest::RequestBuilder {
    http.request(method, url)
        .header("Authorization", format!("token {token}"))
}

/// Run `git` with the token as an HTTP header, never in a URL or argv.
fn git(dir: &Path, token: &str, args: &[&str]) -> Result<String> {
    git_as(dir, token, None, args)
}

/// [`git`], signing commits with `signer`'s key when one is given.
fn git_as(dir: &Path, token: &str, signer: Option<&Signer>, args: &[&str]) -> Result<String> {
    let mut config = vec![
        ("http.extraHeader", format!("Authorization: token {token}")),
        ("user.name", "Forgejo Smoke".to_owned()),
        ("user.email", ALLOWED_IDENTITY.to_owned()),
    ];
    let mut command = Command::new("git");
    if let Some(signer) = signer {
        config.push(("commit.gpgsign", "true".to_owned()));
        config.push(("user.signingKey", signer.fingerprint.clone()));
        command.env("GNUPGHOME", signer.home.path());
    } else {
        config.push(("commit.gpgsign", "false".to_owned()));
    }
    command
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_COUNT", config.len().to_string());
    for (index, (key, value)) in config.iter().enumerate() {
        command
            .env(format!("GIT_CONFIG_KEY_{index}"), key)
            .env(format!("GIT_CONFIG_VALUE_{index}"), value);
    }
    let output = command
        .output()
        .with_context(|| format!("spawn git {args:?}"))?;
    if !output.status.success() {
        bail!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// A synthetic `OpenPGP` signing key for the smoke account, in a keyring of
/// its own that is removed on drop.
struct Signer {
    home: tempfile::TempDir,
    fingerprint: String,
}

impl Signer {
    fn generate() -> Result<Self> {
        let mut signer = Self {
            home: tempfile::tempdir().context("create a GnuPG home")?,
            fingerprint: String::new(),
        };
        signer.gpg(
            &[
                "--quick-gen-key",
                &format!("Forgejo Smoke <{ALLOWED_IDENTITY}>"),
                "ed25519",
                "sign",
                "never",
            ],
            None,
        )?;
        let listing = signer.gpg(&["--with-colons", "--list-secret-keys"], None)?;
        signer.fingerprint = listing
            .lines()
            .find_map(|line| line.strip_prefix("fpr:"))
            .map(|rest| rest.trim_matches(':').to_owned())
            .filter(|fingerprint| !fingerprint.is_empty())
            .context("the generated key has no fingerprint")?;
        Ok(signer)
    }

    fn gpg(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<String> {
        let mut child = Command::new("gpg")
            .env("GNUPGHOME", self.home.path())
            .args(["--batch", "--pinentry-mode", "loopback", "--passphrase", ""])
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("spawn gpg")?;
        if let Some(input) = stdin {
            std::io::Write::write_all(&mut child.stdin.take().context("gpg stdin")?, input)
                .context("write gpg stdin")?;
        }
        let output = child.wait_with_output().context("wait for gpg")?;
        ensure!(
            output.status.success(),
            "gpg {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

impl Drop for Signer {
    fn drop(&mut self) {
        let _ = Command::new("gpgconf")
            .env("GNUPGHOME", self.home.path())
            .args(["--kill", "gpg-agent"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

async fn get_json(http: &reqwest::Client, token: &str, url: &str) -> Result<serde_json::Value> {
    api(http, token, reqwest::Method::GET, url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await
        .with_context(|| format!("parse GET {url}"))
}

/// Send `body`, require success, and return the parsed answer (or `null`).
async fn send_json(
    http: &reqwest::Client,
    token: &str,
    method: reqwest::Method,
    url: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value> {
    let text = api(http, token, method.clone(), url)
        .json(body)
        .send()
        .await
        .with_context(|| format!("{method} {url}"))?
        .error_for_status()
        .with_context(|| format!("{method} {url}"))?
        .text()
        .await
        .with_context(|| format!("read {method} {url}"))?;
    if text.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_str(&text).with_context(|| format!("parse {method} {url}"))
}

fn clone_url(forge: &str) -> String {
    format!("{forge}/{OWNER}/{REPOSITORY}.git")
}

async fn branch_sha(http: &reqwest::Client, forge: &str, token: &str) -> Result<String> {
    let branch: serde_json::Value = api(
        http,
        token,
        reqwest::Method::GET,
        &format!("{forge}/api/v1/repos/{OWNER}/{REPOSITORY}/branches/main"),
    )
    .send()
    .await
    .context("read the main branch")?
    .error_for_status()
    .context("read the main branch")?
    .json()
    .await
    .context("parse the main branch")?;
    branch["commit"]["id"]
        .as_str()
        .map(str::to_owned)
        .context("the main branch has no commit id")
}

/// Create the repository, then clone it, commit a file and the proof
/// workflow, and push. Returns the pushed commit.
async fn push_repository(http: &reqwest::Client, forge: &str, token: &str) -> Result<String> {
    let repo = format!("{forge}/api/v1/repos/{OWNER}/{REPOSITORY}");
    let _ = api(http, token, reqwest::Method::DELETE, &repo)
        .send()
        .await;
    api(
        http,
        token,
        reqwest::Method::POST,
        &format!("{forge}/api/v1/user/repos"),
    )
    .json(&serde_json::json!({
        "name": REPOSITORY,
        "private": true,
        "auto_init": true,
        "default_branch": "main",
    }))
    .send()
    .await
    .context("create the smoke repository")?
    .error_for_status()
    .context("create the smoke repository")?;

    let dir = tempfile::tempdir().context("create a clone directory")?;
    git(dir.path(), token, &["clone", &clone_url(forge), "repo"])?;
    let work = dir.path().join("repo");
    std::fs::write(work.join("proof.txt"), "synthetic retained proof\n").context("write proof")?;
    let workflows = work.join(".forgejo").join("workflows");
    std::fs::create_dir_all(&workflows).context("create .forgejo/workflows")?;
    std::fs::write(workflows.join("proof.yaml"), WORKFLOW).context("write the workflow")?;
    git(
        &work,
        token,
        &["add", "proof.txt", ".forgejo/workflows/proof.yaml"],
    )?;
    git(
        &work,
        token,
        &["commit", "--quiet", "-m", "Add the retained proof"],
    )?;
    git(&work, token, &["push", "--quiet", "origin", "main"])?;
    let sha = git(&work, token, &["rev-parse", "HEAD"])?;
    ensure!(
        branch_sha(http, forge, token).await? == sha,
        "Forgejo's main branch is not the pushed commit"
    );
    Ok(sha)
}

/// The synthetic repository first proves Actions, then acquires its merge
/// protection. Scheduling must fail closed until its required check exists.
async fn merge_policy_proofs(
    kube: &Kube,
    http: &reqwest::Client,
    forge: &str,
    token: &str,
    sha: &str,
) -> Result<()> {
    use super::policy::Forgejo;
    let client = Forgejo::new(forge, token.to_owned())?;
    let repository = format!("{OWNER}/{REPOSITORY}");
    // This proof uses a distinct pending context; the Project Actions port
    // must independently prove the context its gate actually emits.
    let context = "navigator-governance-proof";
    ensure!(
        !client
            .reconcile(&repository, context, true)
            .await?
            .is_empty(),
        "the unprotected synthetic repository reported no drift"
    );
    client.reconcile(&repository, context, false).await?;
    ensure!(
        client
            .reconcile(&repository, context, false)
            .await?
            .is_empty(),
        "a second reconcile changed the same policy"
    );
    let url = format!("{forge}/api/v1/repos/{repository}/branch_protections/main");
    api(http, token, reqwest::Method::PATCH, &url)
        .json(&serde_json::json!({"enable_status_check": false}))
        .send()
        .await?
        .error_for_status()?;
    ensure!(
        client
            .reconcile(&repository, context, true)
            .await?
            .contains(&"main.enable_status_check".to_owned()),
        "missing check was not detected"
    );
    client.reconcile(&repository, context, false).await?;

    let dir = tempfile::tempdir()?;
    git(dir.path(), token, &["clone", &clone_url(forge), "repo"])?;
    let work = dir.path().join("repo");
    std::fs::write(work.join("policy-proof.txt"), "synthetic policy proof\n")?;
    git(&work, token, &["add", "policy-proof.txt"])?;
    git(
        &work,
        token,
        &["commit", "--quiet", "-m", "Synthetic unsigned policy proof"],
    )?;
    git(
        &work,
        token,
        &["push", "--quiet", "origin", "HEAD:policy-proof"],
    )?;
    ensure!(
        git(&work, token, &["push", "--quiet", "origin", "HEAD:main"]).is_err(),
        "a direct unsigned push to protected main succeeded"
    );
    // Forgejo must apply the same protection to administrators.
    set_site_admin(kube, true)?;
    let admin_push = git(&work, token, &["push", "--quiet", "origin", "HEAD:main"]);
    let admin_force = git(
        &work,
        token,
        &["push", "--quiet", "--force", "origin", "HEAD:main"],
    );
    let admin_delete = git(&work, token, &["push", "--quiet", "origin", ":main"]);
    set_site_admin(kube, false)?;
    ensure!(
        admin_push.is_err() && admin_force.is_err() && admin_delete.is_err(),
        "an administrator bypassed protected main"
    );
    ensure!(
        branch_sha(http, forge, token).await? == sha,
        "a rejected push moved main"
    );
    api(
        http,
        token,
        reqwest::Method::DELETE,
        &format!("{forge}/api/v1/repos/{repository}/branches/policy-proof"),
    )
    .send()
    .await?
    .error_for_status()?;
    Ok(())
}

/// One signed pull request the squash-merge proof opens against `main`.
struct ProofPull {
    number: u64,
    head: String,
    branch: &'static str,
    title: &'static str,
    body: &'static str,
}

/// Prove the merge path end to end in a repository whose every `main`
/// commit is signed: the gate's own context is required, a green pull
/// request merges itself, and every other path to `main` is refused.
#[allow(clippy::too_many_lines)] // One linear proof reads best as one function.
async fn squash_merge_proofs(
    kube: &Kube,
    http: &reqwest::Client,
    forge: &str,
    token: &str,
) -> Result<()> {
    use super::policy::scheduler::{schedule_open, PullRequestOutcome};
    use super::policy::Forgejo;

    let repository = format!("{OWNER}/{MERGE_REPOSITORY}");
    let repo_api = format!("{forge}/api/v1/repos/{repository}");
    let signer = Signer::generate()?;
    register_signing_key(http, forge, token, &signer).await?;

    let _ = api(http, token, reqwest::Method::DELETE, &repo_api)
        .send()
        .await;
    send_json(
        http,
        token,
        reqwest::Method::POST,
        &format!("{forge}/api/v1/user/repos"),
        &serde_json::json!({
            "name": MERGE_REPOSITORY,
            "private": true,
            "auto_init": true,
            "default_branch": "main",
        }),
    )
    .await
    .context("create the merge-proof repository")?;
    let initial = main_head(http, token, &repo_api).await?;
    signed_commit(http, token, &repo_api, &initial)
        .await
        .context("Forgejo's initial commit")?;

    // The workflows reach main in one signed commit, before protection.
    let dir = tempfile::tempdir().context("create a clone directory")?;
    git(
        dir.path(),
        token,
        &[
            "clone",
            "--quiet",
            &format!("{forge}/{repository}.git"),
            "repo",
        ],
    )?;
    let work = dir.path().join("repo");
    let workflows = work.join(".forgejo").join("workflows");
    std::fs::create_dir_all(&workflows).context("create .forgejo/workflows")?;
    for (file, workflow) in [
        ("gate.yaml", GATE_WORKFLOW),
        ("publish.yaml", PUBLISH_WORKFLOW),
        ("bot-push.yaml", BOT_PUSH_WORKFLOW),
    ] {
        std::fs::write(workflows.join(file), workflow).with_context(|| format!("write {file}"))?;
    }
    let git_signed = |args: &[&str]| git_as(&work, token, Some(&signer), args);
    git_signed(&["add", ".forgejo/workflows"])?;
    git_signed(&[
        "commit",
        "--quiet",
        "-m",
        "Add the synthetic gate and publication",
    ])?;
    git_signed(&["push", "--quiet", "origin", "main"])?;
    let base = git_signed(&["rev-parse", "HEAD"])?;
    ensure!(
        main_head(http, token, &repo_api).await? == base,
        "the workflow commit is not main"
    );
    signed_commit(http, token, &repo_api, &base)
        .await
        .context("the workflow commit")?;
    let publication = wait_for_run(http, forge, token, &repository, "publish.yaml", &base).await?;
    ensure!(
        publication == "success",
        "publication on the workflow commit finished with {publication}"
    );

    // The single KIND runner takes jobs in order, so the green gate opened
    // last is still pending while auto-merge is scheduled.
    let mut pulls = Vec::new();
    for (branch, title, body) in [
        (
            "red-proof",
            "Add the red proof",
            "A synthetic change whose gate fails.",
        ),
        (
            "draft-proof",
            "WIP: Add the draft proof",
            "A synthetic change opened as a draft.",
        ),
        (
            "green-proof",
            "Add the green proof",
            "A synthetic change whose gate passes.\n\nIts squash commit carries this body.",
        ),
    ] {
        git_signed(&["checkout", "--quiet", "-B", branch, &base])?;
        std::fs::write(
            work.join(format!("{branch}.txt")),
            format!("synthetic {branch}\n"),
        )
        .with_context(|| format!("write {branch}.txt"))?;
        git_signed(&["add", &format!("{branch}.txt")])?;
        git_signed(&[
            "commit",
            "--quiet",
            "-m",
            &format!("Synthetic {branch} commit"),
        ])?;
        git_signed(&["push", "--quiet", "origin", branch])?;
        let head = git_signed(&["rev-parse", "HEAD"])?;
        let pull = send_json(
            http,
            token,
            reqwest::Method::POST,
            &format!("{repo_api}/pulls"),
            &serde_json::json!({"head": branch, "base": "main", "title": title, "body": body}),
        )
        .await
        .with_context(|| format!("open the {branch} pull request"))?;
        pulls.push(ProofPull {
            number: pull["number"]
                .as_u64()
                .context("the pull request has no number")?,
            head,
            branch,
            title,
            body,
        });
    }
    let [red, draft, green] = <[ProofPull; 3]>::try_from(pulls)
        .map_err(|_| anyhow::anyhow!("the proof opened the wrong number of pull requests"))?;

    // The required check is the context the gate run actually posts.
    let context = observed_context(http, token, &repo_api, &green.head).await?;
    ensure!(
        context == actions_context(GATE_WORKFLOW, "pull_request")?,
        "the gate posted {context:?}"
    );
    eprintln!("    the gate posts {context:?}");
    let client = Forgejo::new(forge, token.to_owned())?;
    ensure!(
        client
            .schedule(&repository, &context, green.number)
            .await
            .is_err(),
        "auto-merge was scheduled before main required the gate"
    );
    ensure!(
        !client
            .reconcile(&repository, &context, true)
            .await?
            .is_empty(),
        "the unprotected merge-proof repository reported no drift"
    );
    client.reconcile(&repository, &context, false).await?;
    ensure!(
        client
            .reconcile(&repository, &context, false)
            .await?
            .is_empty(),
        "a second reconcile changed the merge-proof policy"
    );

    // The operator command schedules the ready pull requests and skips the
    // draft, while the green gate has yet to finish.
    let pending = combined_state(http, token, &repo_api, &green.head).await?;
    let outcomes = schedule_open(&client, &repository, &context).await?;
    let summary: Vec<(u64, &str)> = outcomes
        .iter()
        .map(|outcome| match outcome {
            PullRequestOutcome::Scheduled { number, .. } => (*number, "scheduled"),
            PullRequestOutcome::Skipped { number, reason, .. } => (*number, *reason),
            PullRequestOutcome::ConflictNeedsInspection { number, .. } => (*number, "conflict"),
            PullRequestOutcome::Failed { number, .. } => (*number, "failed"),
        })
        .collect();
    ensure!(
        summary
            == [
                (red.number, "scheduled"),
                (draft.number, "draft"),
                (green.number, "scheduled"),
            ],
        "schedule-open reported {outcomes:?}"
    );
    eprintln!("    scheduled the green pull request while its gate was {pending}");

    // Green merges with no further action, as one signed squash commit.
    let merged = wait_merged(http, token, &repo_api, green.number).await?;
    verify_squash(http, token, &repo_api, &green, &merged, &base).await?;
    let publication =
        wait_for_run(http, forge, token, &repository, "publish.yaml", &merged).await?;
    ensure!(
        publication == "success",
        "publication on the merge commit finished with {publication}"
    );
    eprintln!(
        "    {} merged as {merged}; publication ran on it",
        green.branch
    );

    // Red: its gate fails, and neither an immediate nor the owner's forced
    // merge lands it. Forgejo 15.0.9 lets a site administrator's forced merge
    // skip protection whatever `apply_to_admins` says
    // (`services/pull/check.go`), so that path stays a cutover gate.
    let gate = wait_for_run(http, forge, token, &repository, "gate.yaml", &red.head).await?;
    ensure!(gate == "failure", "the red gate finished with {gate}");
    for force in [false, true] {
        let status = api(
            http,
            token,
            reqwest::Method::POST,
            &format!("{repo_api}/pulls/{}/merge", red.number),
        )
        .json(&serde_json::json!({
            "Do": "squash",
            "MergeTitleField": red.title,
            "force_merge": force,
            "head_commit_id": red.head,
        }))
        .send()
        .await
        .context("request an immediate merge of the red pull request")?
        .status();
        ensure!(
            status == reqwest::StatusCode::METHOD_NOT_ALLOWED,
            "an immediate merge of the red pull request (force {force}) answered {status}"
        );
    }
    let pull = get_json(http, token, &format!("{repo_api}/pulls/{}", red.number)).await?;
    ensure!(
        pull["state"] == "open" && pull["merged"] == false,
        "the red pull request did not stay open"
    );
    ensure!(
        main_head(http, token, &repo_api).await? == merged,
        "a red merge attempt moved main"
    );

    // Draft: the guarded command refuses it until it is marked ready.
    ensure!(
        client
            .schedule(&repository, &context, draft.number)
            .await
            .is_err(),
        "auto-merge was scheduled for a draft"
    );
    let ready_title = draft.title.trim_start_matches("WIP: ");
    send_json(
        http,
        token,
        reqwest::Method::PATCH,
        &format!("{repo_api}/pulls/{}", draft.number),
        &serde_json::json!({"title": ready_title}),
    )
    .await
    .context("mark the draft pull request ready")?;
    client.schedule(&repository, &context, draft.number).await?;
    let ready_merged = wait_merged(http, token, &repo_api, draft.number).await?;
    let ready = ProofPull {
        title: ready_title,
        ..draft
    };
    verify_squash(http, token, &repo_api, &ready, &ready_merged, &merged).await?;
    eprintln!("    the draft merged as {ready_merged} once marked ready");

    // Direct pushes carry a signed commit, so only the push rule refuses them.
    git_signed(&["fetch", "--quiet", "origin", "main"])?;
    git_signed(&["checkout", "--quiet", "-B", "direct-proof", "FETCH_HEAD"])?;
    std::fs::write(work.join("direct-proof.txt"), "synthetic direct push\n")
        .context("write direct-proof.txt")?;
    git_signed(&["add", "direct-proof.txt"])?;
    git_signed(&["commit", "--quiet", "-m", "Synthetic signed direct push"])?;
    let refused = |result: Result<String>| {
        result
            .err()
            .is_some_and(|error| error.to_string().contains("protected branch"))
    };
    let ordinary = git_signed(&["push", "--quiet", "origin", "HEAD:main"]);
    set_site_admin(kube, true)?;
    let admin = git_signed(&["push", "--quiet", "origin", "HEAD:main"]);
    let admin_force = git_signed(&["push", "--quiet", "--force", "origin", "HEAD:main"]);
    let admin_delete = git_signed(&["push", "--quiet", "origin", ":main"]);
    set_site_admin(kube, false)?;
    ensure!(
        refused(ordinary) && refused(admin) && admin_force.is_err() && admin_delete.is_err(),
        "a signed direct push, force-push, or deletion of protected main succeeded"
    );

    // The Actions job token: Forgejo's bot identity inside a workflow.
    send_json(
        http,
        token,
        reqwest::Method::POST,
        &format!("{repo_api}/actions/workflows/bot-push.yaml/dispatches"),
        &serde_json::json!({"ref": "main"}),
    )
    .await
    .context("dispatch the bot-push workflow")?;
    let bot = wait_for_run(
        http,
        forge,
        token,
        &repository,
        "bot-push.yaml",
        &ready_merged,
    )
    .await?;
    ensure!(
        bot == "success",
        "the bot-push workflow finished with {bot}; its token may have pushed to main"
    );
    ensure!(
        main_head(http, token, &repo_api).await? == ready_merged,
        "a direct or bot push moved main"
    );
    ensure!(
        client
            .reconcile(&repository, &context, false)
            .await?
            .is_empty(),
        "the merges left policy drift"
    );
    Ok(())
}

/// Register the signer's public key with the smoke account, proving
/// possession by signing Forgejo's verification token.
async fn register_signing_key(
    http: &reqwest::Client,
    forge: &str,
    token: &str,
    signer: &Signer,
) -> Result<()> {
    let challenge = api(
        http,
        token,
        reqwest::Method::GET,
        &format!("{forge}/api/v1/user/gpg_key_token"),
    )
    .send()
    .await
    .context("GET the GPG key token")?
    .error_for_status()
    .context("GET the GPG key token")?
    .text()
    .await
    .context("read the GPG key token")?;
    let signature = signer.gpg(
        &[
            "--armor",
            "--detach-sign",
            "--local-user",
            &signer.fingerprint,
        ],
        Some(challenge.trim().as_bytes()),
    )?;
    let public = signer.gpg(&["--armor", "--export", &signer.fingerprint], None)?;
    send_json(
        http,
        token,
        reqwest::Method::POST,
        &format!("{forge}/api/v1/user/gpg_keys"),
        &serde_json::json!({"armored_public_key": public, "armored_signature": signature}),
    )
    .await
    .context("register the smoke signing key")?;
    Ok(())
}

async fn main_head(http: &reqwest::Client, token: &str, repo_api: &str) -> Result<String> {
    get_json(http, token, &format!("{repo_api}/branches/main")).await?["commit"]["id"]
        .as_str()
        .map(str::to_owned)
        .context("main has no commit id")
}

/// Read `sha`, requiring Forgejo to report its signature verified.
async fn signed_commit(
    http: &reqwest::Client,
    token: &str,
    repo_api: &str,
    sha: &str,
) -> Result<serde_json::Value> {
    let commit = get_json(http, token, &format!("{repo_api}/git/commits/{sha}")).await?;
    let verification = &commit["commit"]["verification"];
    ensure!(
        verification["verified"] == true,
        "commit {sha} is not verified: {}",
        verification["reason"]
    );
    Ok(commit)
}

/// The one status context posted on `sha`, once Forgejo has posted it.
async fn observed_context(
    http: &reqwest::Client,
    token: &str,
    repo_api: &str,
    sha: &str,
) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let status = get_json(http, token, &format!("{repo_api}/commits/{sha}/status")).await?;
        let contexts: Vec<&str> = status["statuses"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|status| status["context"].as_str())
            .collect();
        match contexts.as_slice() {
            [context] => return Ok((*context).to_owned()),
            [] if Instant::now() < deadline => tokio::time::sleep(Duration::from_secs(2)).await,
            _ => bail!("commit {sha} carries contexts {contexts:?}, not exactly one"),
        }
    }
}

async fn combined_state(
    http: &reqwest::Client,
    token: &str,
    repo_api: &str,
    sha: &str,
) -> Result<String> {
    get_json(http, token, &format!("{repo_api}/commits/{sha}/status")).await?["state"]
        .as_str()
        .map(str::to_owned)
        .context("the combined status has no state")
}

/// Wait for pull request `number` to merge, and return its merge commit.
async fn wait_merged(
    http: &reqwest::Client,
    token: &str,
    repo_api: &str,
    number: u64,
) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        let pull = get_json(http, token, &format!("{repo_api}/pulls/{number}")).await?;
        if pull["merged"] == true {
            return pull["merge_commit_sha"]
                .as_str()
                .map(str::to_owned)
                .context("the merged pull request has no merge commit");
        }
        ensure!(
            pull["state"] == "open",
            "pull request {number} closed without merging"
        );
        ensure!(
            Instant::now() < deadline,
            "pull request {number} did not merge within five minutes"
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// `merged` is `main`, one signed commit on `parent` titled and bodied from
/// the pull request, and the head branch is gone.
async fn verify_squash(
    http: &reqwest::Client,
    token: &str,
    repo_api: &str,
    pull: &ProofPull,
    merged: &str,
    parent: &str,
) -> Result<()> {
    ensure!(
        main_head(http, token, repo_api).await? == merged,
        "main is not the merge commit of {}",
        pull.branch
    );
    let commit = signed_commit(http, token, repo_api, merged)
        .await
        .with_context(|| format!("the squash commit of {}", pull.branch))?;
    let parents: Vec<&str> = commit["parents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|parent| parent["sha"].as_str())
        .collect();
    ensure!(
        parents == [parent],
        "the merge of {} has parents {parents:?}, not one squash onto {parent}",
        pull.branch
    );
    let message = commit["commit"]["message"].as_str().unwrap_or_default();
    ensure!(
        message.trim() == format!("{}\n\n{}", pull.title, pull.body),
        "the merge of {} is titled {message:?}, not from its pull request",
        pull.branch
    );
    let branch = api(
        http,
        token,
        reqwest::Method::GET,
        &format!("{repo_api}/branches/{}", pull.branch),
    )
    .send()
    .await
    .context("read the merged head branch")?;
    ensure!(
        branch.status() == reqwest::StatusCode::NOT_FOUND,
        "{} survived its merge ({})",
        pull.branch,
        branch.status()
    );
    Ok(())
}

/// Seed the synthetic Firm and its Project in an in-memory store, the way
/// Navigator creates them, and compose their repository path.
async fn seeded_repository_path() -> Result<store::firms::forgejo::RepositoryPath> {
    let surreal = store::test_support::mem_surreal().await;
    let entity_id = store::test_support::seed_entity(&surreal).await;
    let admin = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Synthetic Admin",
            "forge-smoke-admin@example.test",
            store::persons::Role::Admin,
        ),
    )
    .await
    .context("seed the synthetic Admin DRI")?;
    let firm = store::firms::create(
        &surreal,
        &store::firms::NewFirm {
            name: SMOKE_FIRM.to_owned(),
            status: "active".to_owned(),
            entity_id,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .context("seed the synthetic Firm")?;
    let project = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: SMOKE_PROJECT.to_owned(),
            name: "Sample Litigation".to_owned(),
            status: "open".to_owned(),
            entity_id,
            firm_id: Some(firm.id),
            ..store::projects::NewProject::default()
        },
    )
    .await
    .context("seed the synthetic Project")?;
    Ok(store::firms::forgejo::repository_path(&firm, &project)?)
}

/// Grant or revoke site administration for the smoke account.
fn set_site_admin(kube: &Kube, admin: bool) -> Result<()> {
    psql(
        kube,
        "forgejo",
        &format!("UPDATE \"user\" SET is_admin = {admin} WHERE lower_name = '{OWNER}'"),
    )?;
    Ok(())
}

/// Create the Firm's organization and the Project's private repository at
/// `path`, then read both back.
///
/// Organization creation is closed to regular accounts
/// (`DISABLE_REGULAR_ORG_CREATION`), so the admitted smoke account stands in for Navigator's provisioning
/// identity as a site administrator for this phase only, and is demoted
/// again whatever the outcome.
fn provision_firm_repository(
    kube: &Kube,
    runtime: &tokio::runtime::Runtime,
    http: &reqwest::Client,
    forge: &str,
    token: &str,
    path: &store::firms::forgejo::RepositoryPath,
) -> Result<()> {
    set_site_admin(kube, true)?;
    let created = runtime.block_on(create_firm_repository(http, forge, token, path));
    let verified = created.and_then(|()| {
        let stored = psql(
            kube,
            "forgejo",
            &format!(
                "SELECT o.lower_name || '/' || r.lower_name FROM repository r \
                 JOIN \"user\" o ON o.id = r.owner_id \
                 WHERE o.lower_name = '{owner}' AND o.type = 1",
                owner = path.owner
            ),
        )?;
        ensure!(
            stored == path.to_string(),
            "Forgejo stores the Firm repository as {stored:?}"
        );
        Ok(())
    });
    let cleaned = runtime.block_on(delete_firm_repository(http, forge, token, path));
    set_site_admin(kube, false)?;
    verified?;
    cleaned?;
    let admin = psql(
        kube,
        "forgejo",
        &format!("SELECT is_admin FROM \"user\" WHERE lower_name = '{OWNER}'"),
    )?;
    ensure!(admin == "f", "the smoke account kept site administration");
    eprintln!("    created and verified {path}");
    Ok(())
}

async fn create_firm_repository(
    http: &reqwest::Client,
    forge: &str,
    token: &str,
    path: &store::firms::forgejo::RepositoryPath,
) -> Result<()> {
    delete_firm_repository(http, forge, token, path).await?;
    api(
        http,
        token,
        reqwest::Method::POST,
        &format!("{forge}/api/v1/orgs"),
    )
    .json(&serde_json::json!({"username": path.owner, "visibility": "private"}))
    .send()
    .await
    .context("create the Firm organization")?
    .error_for_status()
    .context("create the Firm organization")?;
    api(
        http,
        token,
        reqwest::Method::POST,
        &format!("{forge}/api/v1/orgs/{}/repos", path.owner),
    )
    .json(&serde_json::json!({
        "name": path.repository,
        "private": true,
        "auto_init": true,
        "default_branch": "main",
    }))
    .send()
    .await
    .context("create the Project repository")?
    .error_for_status()
    .context("create the Project repository")?;

    let repository: serde_json::Value = api(
        http,
        token,
        reqwest::Method::GET,
        &format!("{forge}/api/v1/repos/{path}"),
    )
    .send()
    .await
    .context("read the Project repository")?
    .error_for_status()
    .context("read the Project repository")?
    .json()
    .await
    .context("parse the Project repository")?;
    let full_name = repository["full_name"].as_str().unwrap_or_default();
    ensure!(
        full_name == path.to_string(),
        "the API names the Project repository {full_name:?}, not {path}"
    );
    ensure!(
        repository["owner"]["login"] == path.owner.as_str()
            && repository["name"] == path.repository.as_str()
            && repository["private"] == true,
        "the Project repository is not the Firm organization's private {}",
        path.repository
    );
    let organization: serde_json::Value = api(
        http,
        token,
        reqwest::Method::GET,
        &format!("{forge}/api/v1/orgs/{}", path.owner),
    )
    .send()
    .await
    .context("read the Firm organization")?
    .error_for_status()
    .context("read the Firm organization")?
    .json()
    .await
    .context("parse the Firm organization")?;
    ensure!(
        organization["username"] == path.owner.as_str(),
        "the Firm organization is named {}",
        organization["username"]
    );
    Ok(())
}

/// Remove the Firm repository and organization, so each run starts clean.
async fn delete_firm_repository(
    http: &reqwest::Client,
    forge: &str,
    token: &str,
    path: &store::firms::forgejo::RepositoryPath,
) -> Result<()> {
    for url in [
        format!("{forge}/api/v1/repos/{path}"),
        format!("{forge}/api/v1/orgs/{}", path.owner),
    ] {
        let response = api(http, token, reqwest::Method::DELETE, &url)
            .send()
            .await
            .with_context(|| format!("DELETE {url}"))?;
        ensure!(
            response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND,
            "DELETE {url} answered {}",
            response.status()
        );
    }
    Ok(())
}

/// Wait for `workflow`'s run on `sha` to finish, and return its status.
async fn wait_for_run(
    http: &reqwest::Client,
    forge: &str,
    token: &str,
    repository: &str,
    workflow: &str,
    sha: &str,
) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(300);
    let url = format!("{forge}/api/v1/repos/{repository}/actions/tasks");
    loop {
        let tasks = get_json(http, token, &url).await?;
        let status = tasks["workflow_runs"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|run| run["head_sha"] == sha && run["workflow_id"] == workflow)
            .and_then(|run| run["status"].as_str())
            .unwrap_or("not queued")
            .to_owned();
        match status.as_str() {
            "success" | "failure" | "cancelled" | "skipped" => return Ok(status),
            _ if Instant::now() > deadline => {
                bail!("{workflow} on {sha} is still {status} after five minutes")
            }
            _ => tokio::time::sleep(Duration::from_secs(3)).await,
        }
    }
}

async fn verify_retained(
    http: &reqwest::Client,
    forge: &str,
    token: &str,
    sha: &str,
) -> Result<()> {
    ensure!(
        branch_sha(http, forge, token).await? == sha,
        "the pushed commit did not survive"
    );
    let dir = tempfile::tempdir().context("create a clone directory")?;
    git(
        dir.path(),
        token,
        &["clone", "--quiet", &clone_url(forge), "repo"],
    )?;
    let cloned = git(&dir.path().join("repo"), token, &["rev-parse", "HEAD"])?;
    ensure!(cloned == sha, "a fresh clone is at {cloned}, not {sha}");
    Ok(())
}

fn verify_repository_row(kube: &Kube) -> Result<()> {
    let rows = psql(
        kube,
        "forgejo",
        &format!("SELECT count(*) FROM repository WHERE lower_name = '{REPOSITORY}'"),
    )?;
    ensure!(rows == "1", "the repository row did not survive ({rows})");
    Ok(())
}

/// The issuer subject Forgejo bound the allowed account to: an account that
/// `OAuth2` auto-registration created carries its source and the provider's
/// `sub` on the user row itself.
fn linked_subject(kube: &Kube) -> Result<String> {
    let subject = psql(
        kube,
        "forgejo",
        &format!(
            "SELECT u.login_name FROM \"user\" u \
             JOIN login_source s ON s.id = u.login_source \
             WHERE lower(u.email) = lower('{ALLOWED_IDENTITY}') AND s.name = 'rauthy' \
             AND u.login_type = 6"
        ),
    )?;
    ensure!(
        !subject.is_empty() && !subject.contains('\n'),
        "{ALLOWED_IDENTITY} is not bound to exactly one Rauthy subject"
    );
    Ok(subject)
}

/// A synthetic API credential for the admitted smoke account, minted by the
/// admin CLI; people authenticate through OIDC.
fn user_token(kube: &Kube) -> Result<String> {
    let name = format!("smoke-{}", super::hex(&rand::random::<[u8; 6]>()));
    let token = kube.forgejo(
        &[
            "admin",
            "user",
            "generate-access-token",
            "--username",
            OWNER,
            "--token-name",
            &name,
            "--scopes",
            "all",
            "--raw",
        ],
        None,
    )?;
    Ok(token.trim().to_owned())
}

/// With the smoke allowlist active: the refused identities get no account,
/// and the allowed one gets exactly one, bound to its Rauthy subject. Leaves
/// that account's session in `session`. Returns the subject.
async fn admission_open(
    kube: &Kube,
    http: &reqwest::Client,
    forge: &str,
    session: &mut CookieJar,
) -> Result<String> {
    for (email, why) in DENIED_IDENTITIES {
        let outcome = oidc::sign_in(http, forge, email, &mut CookieJar::default()).await?;
        ensure!(
            outcome != Outcome::SignedIn,
            "{why} ({email}) signed in: {outcome:?}"
        );
        ensure!(
            count_users(kube, email)? == 0,
            "{why} ({email}) got an account"
        );
    }
    let outcome = oidc::sign_in(http, forge, ALLOWED_IDENTITY, session).await?;
    ensure!(
        outcome == Outcome::SignedIn,
        "{ALLOWED_IDENTITY} on the allowlist did not sign in: {outcome:?}"
    );
    ensure!(
        count_users(kube, ALLOWED_IDENTITY)? == 1,
        "{ALLOWED_IDENTITY} did not get exactly one account"
    );
    let admin = psql(
        kube,
        "forgejo",
        &format!("SELECT is_admin FROM \"user\" WHERE lower_name = '{OWNER}'"),
    )?;
    ensure!(
        admin == "f",
        "a self-registered account became a site administrator"
    );
    linked_subject(kube)
}

/// Provider sign-in opens a session without a local enrollment or challenge.
async fn provider_session_proofs(
    http: &reqwest::Client,
    forge: &str,
    session: &mut CookieJar,
    token: &str,
) -> Result<()> {
    let (status, target) = oidc::landing(http, forge, "/user/settings", session).await?;
    ensure!(
        status == 200,
        "OIDC session cannot open settings directly: {status} {target}"
    );
    let (status, _) = api_user(http, forge, token).await?;
    ensure!(
        status == 200,
        "admitted account API access was refused: {status}"
    );
    ensure!(
        git_refs_status(http, forge, token).await? == 404,
        "admitted git access did not reach the missing repository"
    );
    Ok(())
}

/// Sign in directly through the configured OIDC provider.
async fn provider_sign_in(http: &reqwest::Client, forge: &str) -> Result<()> {
    let mut jar = CookieJar::default();
    ensure!(
        oidc::sign_in(http, forge, ALLOWED_IDENTITY, &mut jar).await? == Outcome::SignedIn,
        "provider sign-in did not open a session directly"
    );
    let (status, target) = oidc::landing(http, forge, "/user/settings", &mut jar).await?;
    ensure!(
        status == 200,
        "OIDC session cannot open settings directly: {status} {target}"
    );
    Ok(())
}

/// With the allowlist closed again, the existing account still signs in by
/// its issuer subject, and no second account appears.
async fn existing_account_keeps_its_subject(
    kube: &Kube,
    http: &reqwest::Client,
    forge: &str,
    subject: &str,
) -> Result<()> {
    provider_sign_in(http, forge).await?;
    ensure!(
        count_users(kube, ALLOWED_IDENTITY)? == 1,
        "a second account appeared for {ALLOWED_IDENTITY}"
    );
    ensure!(
        linked_subject(kube)? == subject,
        "the account's issuer subject changed"
    );
    Ok(())
}

async fn api_user(http: &reqwest::Client, forge: &str, token: &str) -> Result<(u16, String)> {
    let response = api(
        http,
        token,
        reqwest::Method::GET,
        &format!("{forge}/api/v1/user"),
    )
    .send()
    .await
    .context("GET /api/v1/user")?;
    let status = response.status().as_u16();
    Ok((status, response.text().await.unwrap_or_default()))
}

/// The status of a smart-HTTP ref advertisement for the not-yet-created
/// smoke repository, authenticated with `token`.
async fn git_refs_status(http: &reqwest::Client, forge: &str, token: &str) -> Result<u16> {
    let url = format!("{forge}/{OWNER}/{REPOSITORY}.git/info/refs?service=git-upload-pack");
    Ok(api(http, token, reqwest::Method::GET, &url)
        .send()
        .await
        .context("GET info/refs")?
        .status()
        .as_u16())
}
