//! `navigator project` — the Project workspace group.
//!
//! Project is the organizing noun of the whole product: a matter's Drive
//! ingest folder and its one source repository are both addressed by one
//! `projects.code`. The verbs that operate on that pair live here rather than
//! under `crate::remote`, grouped by the noun they act on rather than by
//! mechanism — some ([`doctor`], [`repository`]) resolve deployment-owned
//! coordinates from [`model::workspace`] and inspect the operator's own
//! machine, while others ([`drift`], [`surfaces`]) are pure clients of the
//! logged-in deployment's admin API, exactly like every command in
//! `crate::remote`. Neither `drift` nor `surfaces` opens a database
//! connection of its own, even against a local deployment: the site does
//! that work and the CLI only ever holds a bearer token.

pub(crate) mod calendar;
pub mod cli_docs;
pub mod communication;
pub mod cost;
pub mod deadline;
pub mod deadline_brief;
pub mod deadline_proposal;
pub mod deadline_review;
pub mod docket;
pub mod doctor;
pub mod document_check;
pub mod drift;
pub mod drive_ingest;
pub mod efiling;
pub mod feedback_check;
pub mod finance;
pub mod firm_envelope;
pub mod forge;
pub mod forge_forgejo;
pub mod forge_github;
pub mod gate;
pub mod ics;
pub mod invite;
pub mod invoice;
pub mod jj;
pub mod jj_billing;
pub mod jj_commands;
pub mod jj_log;
pub mod labels;
pub mod lifecycle;
pub mod lifecycle_read;
pub mod local_bytes;
pub mod main_push;
pub mod manifest;
pub mod matter;
pub mod mirror;
pub mod mirror_repair;
pub mod mirror_status;
pub mod pages_init;
pub mod participant;
pub mod pinned_gate;
pub mod push;
pub mod reference;
pub(crate) mod release_preflight;
pub(crate) mod release_review;
pub mod repository;
mod reviews;
pub mod scaffold;
pub mod search;
pub mod service;
pub mod set;
pub mod setup;
pub mod skill;
pub mod skill_feedback;
pub mod source_import;
pub mod status;
pub mod surfaces;
pub mod surfaces_reconcile;
pub mod time;
pub mod watch;
mod workspace;
pub mod workspace_root;
pub mod workspace_skills;

/// Resolve the explicit matter scope or the enclosing Project checkout.
pub(crate) fn project_target(
    host: Option<&str>,
    project: Option<&str>,
) -> anyhow::Result<(Option<String>, String)> {
    if let (Some(host), Some(project)) = (host, project) {
        return Ok((Some(host.to_owned()), project.to_owned()));
    }
    let cwd = std::env::current_dir()?;
    for dir in cwd.ancestors() {
        if dir.join(manifest::FILE).is_file() {
            let (local_project, local_host) = crate::document_sync::read_manifest(dir)?;
            return Ok((
                host.map(str::to_owned).or(local_host),
                project.unwrap_or(&local_project).to_owned(),
            ));
        }
        if dir.join(".git").exists() || dir.join(".jj").exists() {
            break;
        }
    }
    Ok((
        host.map(str::to_owned),
        project
            .ok_or_else(|| anyhow::anyhow!("run in a Project checkout or provide --project"))?
            .to_owned(),
    ))
}

/// A stored login for one deployment, shared by the tests that drive a
/// command against a mock site.
#[cfg(test)]
pub(crate) mod test_login {
    use std::sync::LazyLock;

    use crate::credentials::{self, Credentials, HostCredential};

    // `NAVIGATOR_CREDENTIALS_FILE` is process-global, so holders run one at a time.
    static LOCK: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

    pub(crate) struct Login {
        _dir: tempfile::TempDir,
        _lock: tokio::sync::MutexGuard<'static, ()>,
    }

    /// Sign in to `base` with the bearer `test-token`.
    pub(crate) async fn signed_in(base: &str) -> Login {
        let lock = LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("navigator.json");
        let mut creds = Credentials::default();
        creds.set(
            base,
            HostCredential {
                token: "test-token".into(),
                person_email: Some("lawyer@example.test".into()),
                role: Some("lawyer".into()),
                expires_at: i64::MAX,
            },
        );
        credentials::save(&path, &creds).unwrap();
        std::env::set_var("NAVIGATOR_CREDENTIALS_FILE", path);
        Login {
            _dir: dir,
            _lock: lock,
        }
    }

    /// A mock site whose visible inventory is `projects`, with a login for it.
    pub(crate) async fn site(projects: serde_json::Value) -> (wiremock::MockServer, Login) {
        use wiremock::matchers::{method, path};
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(projects))
            .mount(&server)
            .await;
        let login = signed_in(&server.uri()).await;
        (server, login)
    }

    /// Answer one route on `server` with `status` and a JSON body.
    pub(crate) async fn answer(
        server: &wiremock::MockServer,
        verb: &str,
        route: &str,
        status: u16,
        body: serde_json::Value,
    ) {
        use wiremock::matchers::{method, path};
        wiremock::Mock::given(method(verb))
            .and(path(route))
            .respond_with(wiremock::ResponseTemplate::new(status).set_body_json(body))
            .mount(server)
            .await;
    }

    /// The Git half of each colocated command the checkout verbs run. `.jj/diff`
    /// and `.jj/log` hold what `jj diff` and a templated `jj log` print, and
    /// `.jj/calls` records each command with the Git overrides it carried.
    const JJ_STAND_IN: &str = r#"#!/bin/sh
set -eu
while [ "${1:-}" = --ignore-working-copy ] || [ "${1:-}" = --config ]; do
  [ "$1" = --config ] && shift
  shift
done
if [ -d .jj ]; then echo "$* [${GIT_CONFIG_PARAMETERS:-}]" >> .jj/calls; fi
case "${1:-} ${2:-}" in
  "--version "*) echo "jj 0.0.0 (test stand-in)" ;;
  "git clone") git clone --quiet --branch main -- "$7" "$8"
    mkdir "$8/.jj" && echo '/*' > "$8/.jj/.gitignore" ;;
  "git init") mkdir .jj && echo '/*' > .jj/.gitignore ;;
  "git fetch")
    shift 2
    if [ "$#" -eq 0 ]; then git fetch --quiet -- origin; fi
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --remote) git fetch --quiet -- "$2"; shift 2 ;;
        --branch) git fetch --quiet -- origin "refs/heads/$2:refs/remotes/origin/$2"; shift 2 ;;
        --all-remotes) git fetch --quiet --all; shift ;;
        *) echo "jj stand-in: unsupported fetch option: $1" >&2; exit 64 ;;
      esac
    done ;;
  "diff "*) [ -d .jj ] || exit 1; cat .jj/diff 2>/dev/null || true ;;
  "log --no-graph") case "$*" in
      *"present(main@origin) -T commit_id"*) git rev-parse --verify --quiet origin/main || true ;;
      *"parents(@) -T commit_id"*) git rev-parse --verify --quiet HEAD || true ;;
      *"-T conflict") echo false ;;
      *"main@origin -T commit_id") git rev-parse origin/main ;;
      *"-T commit_id") git rev-parse HEAD ;;
      *) cat .jj/log 2>/dev/null || true ;;
    esac ;;
  "new main@origin") git merge --quiet --ff-only origin/main ;;
  "config set" | "git push") ;;
  *) echo "jj stand-in: unsupported: $*" >&2; exit 64 ;;
esac
"#;

    /// Put a stand-in `jj` first on `PATH`, so the checkout verbs run where
    /// Jujutsu is not installed. `PATH` is process-global, like the login.
    pub(crate) fn jj_stand_in() -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt as _;
        let bin = tempfile::tempdir().unwrap();
        let jj = bin.path().join("jj");
        std::fs::write(&jj, JJ_STAND_IN).unwrap();
        std::fs::set_permissions(&jj, std::fs::Permissions::from_mode(0o755)).unwrap();
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let path =
            std::iter::once(bin.path().to_path_buf()).chain(std::env::split_paths(&inherited));
        std::env::set_var("PATH", std::env::join_paths(path).unwrap());
        bin
    }

    /// A Git checkout holding `navigator.yaml` and, when given, an `origin`.
    pub(crate) fn checkout(manifest: &str, origin: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("navigator.yaml"), manifest).unwrap();
        let git = |args: &[&str]| {
            let output = super::lifecycle::git(dir.path(), args).unwrap();
            assert!(output.status.success(), "git {args:?}");
        };
        git(&["init", "--quiet", "--initial-branch=main"]);
        if let Some(origin) = origin {
            git(&["remote", "add", "origin", origin]);
        }
        dir
    }
}
