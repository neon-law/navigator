//! `navigator project notify-main-push --ci`: report one push to `main` to
//! the Project's private Slack channel from CI.
//!
//! Forgejo's `cd.yml` calls this after its `documents` and `seeds` jobs; it
//! is the same `POST /app/api/projects/{id}/events/main-push` GitHub's
//! `project-gate.yml` makes with `curl` and `jq`, neither of which a Forgejo
//! runner image is guaranteed to carry. The run's OIDC token is exchanged
//! for the document-scoped session bound to this repository's live Project,
//! and the body carries the run's mechanism fields — a short SHA, the run
//! URL, the pusher's login, the outcome and the attempt — plus the subject
//! line of the checked-out `HEAD` commit, so the channel reads as a short log
//! of what changed.

use std::path::Path;

use anyhow::{anyhow, ensure, Context, Result};
use serde_json::{json, Value};

/// The gate outcome the workflow passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Outcome {
    Pass,
    Fail,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
        }
    }
}

/// The run's mechanism fields, read from the runner's environment, and the
/// commit subject when one is known. Forgejo exports `FORGEJO_*` and the
/// `GITHUB_*` compatibility names; GitHub only the latter.
fn event_body(
    get: impl Fn(&str) -> Option<String>,
    gate: Outcome,
    subject: Option<&str>,
) -> Result<Value> {
    let read = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| get(name).filter(|value| !value.trim().is_empty()))
    };
    let sha = read(&["FORGEJO_SHA", "GITHUB_SHA"]).context("the run has no commit SHA")?;
    ensure!(
        sha.len() >= 7 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "the run's commit SHA is malformed"
    );
    let server =
        read(&["FORGEJO_SERVER_URL", "GITHUB_SERVER_URL"]).context("the run has no server URL")?;
    let repository =
        read(&["FORGEJO_REPOSITORY", "GITHUB_REPOSITORY"]).context("the run has no repository")?;
    // Forgejo's run page is addressed by the run number, GitHub's by its id.
    let run = if get("FORGEJO_ACTIONS").is_some_and(|value| value.trim() == "true") {
        read(&["FORGEJO_RUN_NUMBER", "GITHUB_RUN_NUMBER"])
    } else {
        read(&["GITHUB_RUN_ID"])
    }
    .context("the run has no run number")?;
    let attempt: u32 = read(&["FORGEJO_RUN_ATTEMPT", "GITHUB_RUN_ATTEMPT"])
        .and_then(|attempt| attempt.parse().ok())
        .unwrap_or(1)
        .max(1);
    let author = read(&["FORGEJO_ACTOR", "GITHUB_ACTOR"]).context("the run has no actor")?;
    let mut body = json!({
        "short_sha": sha[..7].to_ascii_lowercase(),
        "run_url": format!("{}/{repository}/actions/runs/{run}", server.trim_end_matches('/')),
        "author_login": author,
        "gate": gate.as_str(),
        "run_attempt": attempt,
    });
    if let Some(subject) = subject.map(str::trim).filter(|subject| !subject.is_empty()) {
        body["subject"] = json!(subject);
    }
    Ok(body)
}

/// The subject line of the commit checked out in `dir`, or `None` when the
/// checkout is not a Git repository the runner can read. A missing subject
/// never blocks the notice; the server renders an event without one as it
/// always has.
fn head_subject(dir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["log", "-1", "--format=%s"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let subject = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!subject.is_empty()).then_some(subject)
}

/// Tries per notify request before the notice gives up — the same budget the
/// GitHub path's `--retry 3` curls carry.
const NOTIFY_ATTEMPTS: u32 = 3;
/// Wait before the nth retry of a notify request.
const NOTIFY_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(250);

/// One attempt of a notify request: the response body on success, classified
/// for the shared gate retry otherwise — a connect error, a timeout, or a
/// 500/502-504 status is transient; every 4xx is a deterministic refusal.
async fn attempt_notify(
    request: reqwest::RequestBuilder,
    what: &str,
) -> crate::remote::AttemptOutcome<Vec<u8>> {
    use crate::remote::{transient_status, transient_transport, AttemptOutcome};
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) if transient_transport(&error) => {
            return AttemptOutcome::Transient(anyhow::Error::new(error).context(what.to_owned()));
        }
        Err(error) => {
            return AttemptOutcome::Terminal(anyhow::Error::new(error).context(what.to_owned()));
        }
    };
    let status = response.status();
    let body = match response.bytes().await {
        Ok(body) => body,
        Err(error) => {
            return AttemptOutcome::Terminal(anyhow::Error::new(error).context(what.to_owned()));
        }
    };
    if status.is_success() {
        return AttemptOutcome::Done(body.to_vec());
    }
    let failure = anyhow!(
        "{what} failed: {status}: {}",
        String::from_utf8_lossy(&body)
    );
    if transient_status(status) {
        AttemptOutcome::Transient(failure)
    } else {
        AttemptOutcome::Terminal(failure)
    }
}

/// Post `body` as the main-push event of the one Project the CI session sees.
/// Each request is retried before the notice gives up, so a one-off 503 or a
/// dropped connection does not silently swallow the notice.
async fn post(base: &str, token: &str, body: &Value) -> Result<()> {
    use crate::remote::retry_gate_call;
    let client = reqwest::Client::new();
    let list_what = "list the Project this run is bound to";
    let projects_body: Vec<u8> = retry_gate_call(NOTIFY_ATTEMPTS, NOTIFY_RETRY_DELAY, |_| {
        let request = client
            .get(format!("{base}/app/api/projects"))
            .bearer_auth(token);
        async move { attempt_notify(request, list_what).await }
    })
    .await?;
    let projects: Value = serde_json::from_slice(&projects_body)
        .with_context(|| "parse the projects list response".to_string())?;
    let id = projects
        .as_array()
        .and_then(|projects| projects.first())
        .and_then(|project| project["id"].as_str())
        .context("the CI session sees no Project")?
        .to_owned();
    retry_gate_call(NOTIFY_ATTEMPTS, NOTIFY_RETRY_DELAY, |_| {
        let request = client
            .post(format!("{base}/app/api/projects/{id}/events/main-push"))
            .bearer_auth(token)
            .json(body);
        async move { attempt_notify(request, "report the push to main").await }
    })
    .await?;
    Ok(())
}

/// Exchange the run's token, find the bound Project, and post the event.
pub async fn notify(dir: &Path, gate: Outcome) -> Result<()> {
    let manifest = super::manifest::read(dir).context("navigator.yaml is missing or invalid")?;
    let host = super::manifest::non_empty(manifest.host.as_deref())
        .ok_or_else(|| anyhow!("navigator.yaml has no host"))?;
    let subject = head_subject(dir);
    let body = event_body(|name| std::env::var(name).ok(), gate, subject.as_deref())?;
    if let Err(error) = send_notice(&host, &body).await {
        return warn_notice_lost(gate, error);
    }
    // The server has queued the notice for the Project's channel, to be
    // posted with the Firm's own Slack bot; a Firm that has not connected
    // Slack is refused above and warned about, never reported as delivered.
    println!(
        "Queued the main-push notice for the Project channel ({})",
        gate.as_str()
    );
    Ok(())
}

/// The mint and the post, in order — the remote half of a notice.
async fn send_notice(host: &str, body: &Value) -> Result<()> {
    let (base, token) = crate::remote::resolve_ci_document(host).await?;
    post(&base, &token, body).await
}

/// A notice that never went out is still visible: the same `::warning::`
/// annotation the GitHub path prints when its curls give up, before the
/// original error — the workflow, not this command, decides whether a lost
/// notice blocks the deployment.
fn warn_notice_lost(gate: Outcome, error: anyhow::Error) -> Result<()> {
    eprintln!(
        "::warning::could not report this push to the Project channel (gate: {}): {error:#}",
        gate.as_str()
    );
    Err(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn a_forgejo_run_reports_its_run_number_page_and_login() {
        let body = event_body(
            env(&[
                ("FORGEJO_ACTIONS", "true"),
                ("GITHUB_SHA", "ABCDEF0123456789abcdef0123456789abcdef01"),
                ("GITHUB_SERVER_URL", "https://forge.example/"),
                ("GITHUB_REPOSITORY", "example-firm/sample-estate"),
                ("GITHUB_RUN_NUMBER", "12"),
                ("GITHUB_RUN_ID", "9001"),
                ("GITHUB_ACTOR", "nick_s.law"),
            ]),
            Outcome::Fail,
            None,
        )
        .unwrap();
        assert_eq!(
            body,
            json!({
                "short_sha": "abcdef0",
                "run_url": "https://forge.example/example-firm/sample-estate/actions/runs/12",
                "author_login": "nick_s.law",
                "gate": "fail",
                "run_attempt": 1,
            })
        );
    }

    /// ENG-1285: the checked-out commit's subject rides along when known, and
    /// a blank one is left out rather than sent as an empty string.
    #[test]
    fn the_head_subject_is_sent_when_known() {
        let runner = [
            ("GITHUB_SHA", "abcdef0123456789abcdef0123456789abcdef01"),
            ("GITHUB_SERVER_URL", "https://github.com"),
            ("GITHUB_REPOSITORY", "neon-law/sample-estate"),
            ("GITHUB_RUN_ID", "9001"),
            ("GITHUB_ACTOR", "octocat"),
        ];
        let body = event_body(env(&runner), Outcome::Pass, Some(" feat: add the will ")).unwrap();
        assert_eq!(body["subject"], "feat: add the will");
        let blank = event_body(env(&runner), Outcome::Pass, Some("   ")).unwrap();
        assert!(blank.get("subject").is_none());
    }

    #[test]
    fn the_head_subject_comes_from_the_checked_out_commit() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(head_subject(dir.path()), None, "not a repository");
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        git(&[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "feat: add the will\n\nA body line.",
        ]);
        assert_eq!(
            head_subject(dir.path()).as_deref(),
            Some("feat: add the will")
        );
    }

    #[test]
    fn a_run_without_a_commit_reports_nothing() {
        assert!(event_body(env(&[("GITHUB_SHA", "not-hex!")]), Outcome::Pass, None).is_err());
        assert!(event_body(env(&[]), Outcome::Pass, None).is_err());
    }

    #[tokio::test]
    async fn the_event_goes_to_the_one_project_the_session_sees() {
        use wiremock::matchers::{body_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .and(header("authorization", "Bearer ci-session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"id": "p-1"}])))
            .mount(&server)
            .await;
        let body = json!({"short_sha": "abcdef0", "gate": "pass"});
        Mock::given(method("POST"))
            .and(path("/app/api/projects/p-1/events/main-push"))
            .and(body_json(body.clone()))
            .respond_with(ResponseTemplate::new(202))
            .expect(1)
            .mount(&server)
            .await;
        post(&server.uri(), "ci-session", &body).await.unwrap();
    }

    #[tokio::test]
    async fn a_session_that_sees_no_project_reports_nothing() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(202))
            .expect(0)
            .mount(&server)
            .await;
        let error = post(&server.uri(), "ci-session", &json!({}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no Project"), "{error}");
    }

    /// LAW-285: a one-off 503 on the projects lookup does not swallow the
    /// notice — the second attempt lists the Project and the event posts.
    #[tokio::test]
    async fn a_transient_projects_lookup_recovers_before_the_notice_gives_up() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"id": "p-1"}])))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/app/api/projects/p-1/events/main-push"))
            .respond_with(ResponseTemplate::new(202))
            .expect(1)
            .mount(&server)
            .await;

        post(&server.uri(), "ci-session", &json!({"gate": "pass"}))
            .await
            .unwrap();
    }

    /// A one-off 503 on the event post recovers the same way — the notice is
    /// retried before it can be lost.
    #[tokio::test]
    async fn a_transient_event_post_recovers_before_the_notice_gives_up() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"id": "p-1"}])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/app/api/projects/p-1/events/main-push"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/app/api/projects/p-1/events/main-push"))
            .respond_with(ResponseTemplate::new(202))
            .expect(1)
            .mount(&server)
            .await;

        post(&server.uri(), "ci-session", &json!({"gate": "pass"}))
            .await
            .unwrap();
    }

    /// A refused notice is terminal with a visible warning naming what could
    /// not be sent — never a silent drop, never a blocked deployment.
    #[tokio::test]
    async fn a_refused_notice_is_terminal() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"id": "p-1"}])))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/app/api/projects/p-1/events/main-push"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;

        let error = post(&server.uri(), "ci-session", &json!({"gate": "pass"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("401"), "{error}");
    }
}
