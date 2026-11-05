//! `navigator project notify-main-push --ci`: report one push to `main` to
//! the Project's private Slack channel from CI.
//!
//! Forgejo's `cd.yml` calls this after its `documents` and `seeds` jobs; it
//! is the same `POST /app/api/projects/{id}/events/main-push` GitHub's
//! `project-gate.yml` makes with `curl` and `jq`, neither of which a Forgejo
//! runner image is guaranteed to carry. The run's OIDC token is exchanged
//! for the document-scoped session bound to this repository's live Project,
//! and the body carries mechanism fields only: a short SHA, the run URL, the
//! pusher's login, the outcome and the attempt. No commit subject is sent.

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

/// The run's mechanism fields, read from the runner's environment. Forgejo
/// exports `FORGEJO_*` and the `GITHUB_*` compatibility names; GitHub only the
/// latter.
fn event_body(get: impl Fn(&str) -> Option<String>, gate: Outcome) -> Result<Value> {
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
    Ok(json!({
        "short_sha": sha[..7].to_ascii_lowercase(),
        "run_url": format!("{}/{repository}/actions/runs/{run}", server.trim_end_matches('/')),
        "author_login": author,
        "gate": gate.as_str(),
        "run_attempt": attempt,
    }))
}

/// Exchange the run's token, find the bound Project, and post the event.
pub async fn notify(dir: &Path, gate: Outcome) -> Result<()> {
    let manifest = super::manifest::read(dir).context("navigator.yaml is missing or invalid")?;
    let host = super::manifest::non_empty(manifest.host.as_deref())
        .ok_or_else(|| anyhow!("navigator.yaml has no host"))?;
    let body = event_body(|name| std::env::var(name).ok(), gate)?;
    let (base, token) = crate::remote::resolve_ci_document(&host).await?;
    let client = reqwest::Client::new();
    let projects: Value = client
        .get(format!("{base}/app/api/projects"))
        .bearer_auth(&token)
        .send()
        .await?
        .error_for_status()
        .context("list the Project this run is bound to")?
        .json()
        .await?;
    let id = projects
        .as_array()
        .and_then(|projects| projects.first())
        .and_then(|project| project["id"].as_str())
        .context("the CI session sees no Project")?
        .to_owned();
    client
        .post(format!("{base}/app/api/projects/{id}/events/main-push"))
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await?
        .error_for_status()
        .context("report the push to main")?;
    println!(
        "Reported this push to the Project channel ({})",
        gate.as_str()
    );
    Ok(())
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
                ("GITHUB_REPOSITORY", "shook-law-pc/sample-estate"),
                ("GITHUB_RUN_NUMBER", "12"),
                ("GITHUB_RUN_ID", "9001"),
                ("GITHUB_ACTOR", "nick_s.law"),
            ]),
            Outcome::Fail,
        )
        .unwrap();
        assert_eq!(
            body,
            json!({
                "short_sha": "abcdef0",
                "run_url": "https://forge.example/shook-law-pc/sample-estate/actions/runs/12",
                "author_login": "nick_s.law",
                "gate": "fail",
                "run_attempt": 1,
            })
        );
    }

    #[test]
    fn a_run_without_a_commit_reports_nothing() {
        assert!(event_body(env(&[("GITHUB_SHA", "not-hex!")]), Outcome::Pass).is_err());
        assert!(event_body(env(&[]), Outcome::Pass).is_err());
    }
}
