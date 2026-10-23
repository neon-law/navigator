//! Trusted operator-side discovery for Forgejo pull requests.

use super::{repository_target, Forgejo, MergeRequestOutcome, Policy};
use anyhow::{ensure, Context, Result};
use reqwest::Method;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;

const PAGE_SIZE: usize = 50;

#[derive(Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PullRequestOutcome {
    Scheduled {
        number: u64,
        head_sha: String,
    },
    Skipped {
        number: u64,
        head_sha: String,
        reason: &'static str,
    },
    ConflictNeedsInspection {
        number: u64,
        head_sha: String,
    },
    Failed {
        number: u64,
        head_sha: String,
        reason: String,
    },
}

#[derive(Debug)]
struct Candidate {
    number: u64,
    head_sha: String,
    base_ref: String,
    draft: bool,
}

impl Candidate {
    fn parse(value: &Value) -> Result<Self> {
        ensure!(
            value["state"] == "open" && value["merged"] == false,
            "Forgejo returned a non-open pull request in the open list"
        );
        let number = value["number"]
            .as_u64()
            .filter(|number| *number > 0)
            .context("Forgejo pull request omitted a positive number")?;
        let head_sha = value["head"]["sha"]
            .as_str()
            .filter(|sha| !sha.is_empty())
            .context("Forgejo pull request omitted its head SHA")?
            .to_owned();
        let base_ref = value["base"]["ref"]
            .as_str()
            .filter(|name| !name.is_empty())
            .context("Forgejo pull request omitted its base branch")?
            .to_owned();
        // Forgejo 15.0.9 `PullRequest.Draft` is `json:"draft"`:
        // https://codeberg.org/forgejo/forgejo/raw/tag/v15.0.9/modules/structs/pull.go
        let draft = value["draft"]
            .as_bool()
            .context("Forgejo pull request omitted its draft state")?;
        Ok(Self {
            number,
            head_sha,
            base_ref,
            draft,
        })
    }
}

/// Discover every open PR before making any merge request, then schedule only
/// candidates that target `main` and are ready for review. Each PR is fetched
/// again by `Forgejo::schedule_expected` and must still have the discovered
/// head before the guarded merge request is sent.
pub async fn schedule_open(
    forgejo: &Forgejo,
    repository: &str,
    context: &str,
) -> Result<Vec<PullRequestOutcome>> {
    repository_target(repository)?;
    Policy::new(context)?;
    let candidates = discover(forgejo, repository).await?;
    let mut outcomes = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if candidate.base_ref != "main" {
            outcomes.push(PullRequestOutcome::Skipped {
                number: candidate.number,
                head_sha: candidate.head_sha,
                reason: "base_is_not_main",
            });
            continue;
        }
        if candidate.draft {
            outcomes.push(PullRequestOutcome::Skipped {
                number: candidate.number,
                head_sha: candidate.head_sha,
                reason: "draft",
            });
            continue;
        }
        match forgejo
            .schedule_expected(
                repository,
                context,
                candidate.number,
                Some(&candidate.head_sha),
            )
            .await
        {
            Ok(MergeRequestOutcome::Accepted) => {
                outcomes.push(PullRequestOutcome::Scheduled {
                    number: candidate.number,
                    head_sha: candidate.head_sha,
                });
            }
            Ok(MergeRequestOutcome::ConflictNeedsInspection) => {
                outcomes.push(PullRequestOutcome::ConflictNeedsInspection {
                    number: candidate.number,
                    head_sha: candidate.head_sha,
                });
            }
            Err(error) => outcomes.push(PullRequestOutcome::Failed {
                number: candidate.number,
                head_sha: candidate.head_sha,
                reason: error.to_string(),
            }),
        }
    }
    Ok(outcomes)
}

async fn discover(forgejo: &Forgejo, repository: &str) -> Result<Vec<Candidate>> {
    let mut page = 1_usize;
    let mut expected_total = None;
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    loop {
        let response = forgejo
            .request(
                Method::GET,
                &format!("repos/{repository}/pulls?state=open&page={page}&limit={PAGE_SIZE}"),
                None,
            )
            .await?
            .error_for_status()
            .context("list open Forgejo pull requests")?;
        let total = response
            .headers()
            .get("x-total-count")
            .context("Forgejo open pull-request list omitted its total count")?
            .to_str()
            .context("Forgejo pull-request total is not valid text")?
            .parse::<usize>()
            .context("Forgejo pull-request total is not a nonnegative integer")?;
        if let Some(previous) = expected_total {
            ensure!(
                previous == total,
                "Forgejo open pull-request list changed while reading; no merges scheduled"
            );
        } else {
            expected_total = Some(total);
        }
        let values: Vec<Value> = response
            .json()
            .await
            .context("parse Forgejo open pull-request list")?;
        ensure!(
            !values.is_empty() || candidates.len() == total,
            "Forgejo open pull-request list ended before its reported total; no merges scheduled"
        );
        for value in values {
            let candidate = Candidate::parse(&value)?;
            ensure!(
                seen.insert(candidate.number),
                "Forgejo repeated a pull request across pages; no merges scheduled"
            );
            candidates.push(candidate);
            ensure!(
                candidates.len() <= total,
                "Forgejo returned more pull requests than its reported total; no merges scheduled"
            );
        }
        if candidates.len() == total {
            break;
        }
        page = page
            .checked_add(1)
            .context("Forgejo pull-request pagination overflow")?;
    }
    candidates.sort_by_key(|candidate| candidate.number);
    Ok(candidates)
}

/// Fail when any outcome needs an operator: a conflict or a failure. Skips
/// are expected and pass.
///
/// # Errors
/// The count of outcomes needing inspection.
pub fn require_no_actionable_outcomes(outcomes: &[PullRequestOutcome]) -> Result<()> {
    let count = outcomes
        .iter()
        .filter(|outcome| {
            matches!(
                outcome,
                PullRequestOutcome::ConflictNeedsInspection { .. }
                    | PullRequestOutcome::Failed { .. }
            )
        })
        .count();
    ensure!(
        count == 0,
        "{count} Forgejo pull request outcome(s) need inspection"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::State,
        http::{StatusCode, Uri},
        response::IntoResponse,
        routing::any,
        Json, Router,
    };
    use serde_json::json;
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    #[derive(Clone)]
    struct Fixture {
        pages: Vec<Vec<Value>>,
        totals: Vec<usize>,
        repo: Value,
        branch: Value,
        pull_requests: HashMap<u64, Value>,
        writes: Vec<(String, Value)>,
        reads: Vec<String>,
        merge_status: StatusCode,
    }

    struct Server {
        forgejo: Forgejo,
        state: Arc<Mutex<Fixture>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn pull(number: u64, base: &str, draft: bool, head: &str) -> Value {
        json!({
            "number": number,
            "state": "open",
            "merged": false,
            "draft": draft,
            "base": {"ref": base},
            "head": {"sha": head},
            "title": "Synthetic change",
            "body": "Synthetic body",
        })
    }

    #[test]
    fn pull_request_schema_requires_explicit_draft_state() {
        let mut response = pull(7, "main", false, "head-seven");
        assert!(Candidate::parse(&response).is_ok());
        response.as_object_mut().unwrap().remove("draft");
        assert!(Candidate::parse(&response)
            .unwrap_err()
            .to_string()
            .contains("omitted its draft state"));
    }

    fn make_fixture(pulls: &[Value]) -> Fixture {
        let mut repo = super::super::Policy::repository();
        repo["allow_manual_merge"] = false.into();
        repo["autodetect_manual_merge"] = false.into();
        let branch = super::super::Policy::new("ci").unwrap().branch();
        let pull_requests: HashMap<u64, Value> = pulls
            .iter()
            .filter_map(|pull| Some((pull["number"].as_u64()?, pull.clone())))
            .collect();
        let pages = pulls
            .chunks(PAGE_SIZE)
            .map(<[Value]>::to_vec)
            .collect::<Vec<_>>();
        let totals = vec![pull_requests.len(); pages.len()];
        Fixture {
            pages,
            totals,
            repo,
            branch,
            pull_requests,
            writes: Vec::new(),
            reads: Vec::new(),
            merge_status: StatusCode::CREATED,
        }
    }

    fn total_response(total: usize, body: Value) -> axum::response::Response {
        let mut response = (StatusCode::OK, Json(body)).into_response();
        response
            .headers_mut()
            .insert("x-total-count", total.to_string().parse().unwrap());
        response
    }

    fn get(state: &Fixture, uri: &Uri) -> axum::response::Response {
        let path = uri.path();
        if path.ends_with("/pulls") {
            let page = uri
                .query()
                .and_then(|query| query.split('&').find_map(|pair| pair.strip_prefix("page=")))
                .and_then(|page| page.parse::<usize>().ok())
                .unwrap_or(1);
            let index = page.saturating_sub(1);
            let body = state.pages.get(index).cloned().unwrap_or_default();
            let total = state.totals.get(index).copied().unwrap_or_default();
            return total_response(total, json!(body));
        }
        if path.ends_with("/reviews") {
            return total_response(0, json!([]));
        }
        // No check has reported yet, so every eligible PR is scheduled.
        if path.contains("/commits/") && path.ends_with("/status") {
            return (StatusCode::OK, Json(json!({"statuses": []}))).into_response();
        }
        if path.ends_with("/branch_protections") {
            return (StatusCode::OK, Json(json!([state.branch]))).into_response();
        }
        if path.ends_with("/branch_protections/main") {
            return (StatusCode::OK, Json(state.branch.clone())).into_response();
        }
        if let Some(number) = path
            .strip_prefix("/api/v1/repos/example/sample/pulls/")
            .and_then(|suffix| suffix.parse::<u64>().ok())
        {
            return state
                .pull_requests
                .get(&number)
                .cloned()
                .map_or((StatusCode::NOT_FOUND, Json(Value::Null)), |pull| {
                    (StatusCode::OK, Json(pull))
                })
                .into_response();
        }
        if path.ends_with("/repos/example/sample") {
            return (StatusCode::OK, Json(state.repo.clone())).into_response();
        }
        (StatusCode::NOT_FOUND, Json(Value::Null)).into_response()
    }

    async fn handler(
        State(state): State<Arc<Mutex<Fixture>>>,
        method: Method,
        uri: Uri,
        body: Option<Json<Value>>,
    ) -> axum::response::Response {
        let mut state = state.lock().unwrap();
        if method == Method::GET {
            state.reads.push(uri.path().to_owned());
            return get(&state, &uri);
        }
        let value = body.map_or(Value::Null, |Json(value)| value);
        state.writes.push((uri.path().to_owned(), value));
        (state.merge_status, Json(Value::Null)).into_response()
    }

    async fn serve(fixture: Fixture) -> Server {
        let state = Arc::new(Mutex::new(fixture));
        let router = Router::new()
            .route("/{*path}", any(handler))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Server {
            forgejo: Forgejo::new(&origin, "synthetic-token".into()).unwrap(),
            state,
            task,
        }
    }

    #[test]
    fn outcomes_are_structured_without_pull_request_content() {
        let outcome = PullRequestOutcome::ConflictNeedsInspection {
            number: 4,
            head_sha: "synthetic-head".into(),
        };
        let json = serde_json::to_value(outcome).unwrap();
        assert_eq!(json["outcome"], "conflict_needs_inspection");
        assert_eq!(json["number"], 4);
        assert_eq!(json["head_sha"], "synthetic-head");
        assert!(json.get("title").is_none());
        assert_eq!(StatusCode::CONFLICT.as_u16(), 409);
    }

    #[tokio::test]
    async fn discovers_before_writing_then_schedules_sorted_eligible_pulls() {
        let pulls = vec![
            pull(9, "release", false, "head-nine"),
            pull(8, "main", true, "head-eight"),
            pull(7, "main", false, "head-seven"),
        ];
        let server = serve(make_fixture(&pulls)).await;
        let outcomes = schedule_open(&server.forgejo, "example/sample", "ci")
            .await
            .unwrap();
        assert!(matches!(
            outcomes[0],
            PullRequestOutcome::Scheduled { number: 7, .. }
        ));
        assert!(matches!(
            outcomes[1],
            PullRequestOutcome::Skipped {
                number: 8,
                reason: "draft",
                ..
            }
        ));
        assert!(matches!(
            outcomes[2],
            PullRequestOutcome::Skipped {
                number: 9,
                reason: "base_is_not_main",
                ..
            }
        ));
        let state = server.state.lock().unwrap();
        assert_eq!(state.writes.len(), 1);
        assert!(state.writes[0].0.ends_with("/pulls/7/merge"));
        assert_eq!(state.writes[0].1["head_commit_id"], "head-seven");
    }

    #[tokio::test]
    async fn discovers_all_pages_before_scheduling_first_candidate() {
        let pulls: Vec<Value> = (1..=51)
            .map(|number| {
                let base = if number == 1 { "main" } else { "release" };
                pull(number, base, false, &format!("head-{number}"))
            })
            .collect();
        let server = serve(make_fixture(&pulls)).await;
        let outcomes = schedule_open(&server.forgejo, "example/sample", "ci")
            .await
            .unwrap();
        assert_eq!(outcomes.len(), 51);
        assert!(matches!(
            outcomes.first(),
            Some(PullRequestOutcome::Scheduled { number: 1, .. })
        ));
        assert!(matches!(
            outcomes.last(),
            Some(PullRequestOutcome::Skipped {
                number: 51,
                reason: "base_is_not_main",
                ..
            })
        ));
        assert_eq!(server.state.lock().unwrap().writes.len(), 1);
    }

    #[tokio::test]
    async fn incomplete_or_changing_discovery_makes_no_merge_requests() {
        let mut fixture = make_fixture(&[pull(7, "main", false, "head-seven")]);
        fixture.pages.push(vec![]);
        fixture.totals = vec![2, 2];
        let server = serve(fixture).await;
        assert!(schedule_open(&server.forgejo, "example/sample", "ci")
            .await
            .is_err());
        assert!(server.state.lock().unwrap().writes.is_empty());

        let mut fixture = make_fixture(&[pull(7, "main", false, "head-seven")]);
        fixture.pages.push(vec![]);
        fixture.totals = vec![2, 3];
        let server = serve(fixture).await;
        assert!(schedule_open(&server.forgejo, "example/sample", "ci")
            .await
            .unwrap_err()
            .to_string()
            .contains("changed while reading"));
        assert!(server.state.lock().unwrap().writes.is_empty());
    }

    #[tokio::test]
    async fn duplicate_pull_request_numbers_fail_discovery_before_writes() {
        let pull = pull(7, "main", false, "head-seven");
        let mut fixture = make_fixture(std::slice::from_ref(&pull));
        fixture.pages.push(vec![pull]);
        fixture.totals = vec![2, 2];
        let server = serve(fixture).await;
        assert!(schedule_open(&server.forgejo, "example/sample", "ci")
            .await
            .unwrap_err()
            .to_string()
            .contains("repeated a pull request"));
        assert!(server.state.lock().unwrap().writes.is_empty());
    }

    #[tokio::test]
    async fn invalid_repository_fails_before_discovery_requests() {
        let server = serve(make_fixture(&[])).await;
        assert!(schedule_open(&server.forgejo, "../sample", "ci")
            .await
            .unwrap_err()
            .to_string()
            .contains("owner/name"));
        assert!(server.state.lock().unwrap().reads.is_empty());
    }

    #[tokio::test]
    async fn changed_head_and_merge_conflict_are_explicit_outcomes() {
        let server = serve(make_fixture(&[pull(7, "main", false, "listed-head")])).await;
        server
            .state
            .lock()
            .unwrap()
            .pull_requests
            .insert(7, pull(7, "main", false, "current-head"));
        let outcomes = schedule_open(&server.forgejo, "example/sample", "ci")
            .await
            .unwrap();
        assert!(matches!(
            &outcomes[0],
            PullRequestOutcome::Failed { reason, .. }
                if reason.contains("head changed after scheduler discovery")
        ));
        assert!(server.state.lock().unwrap().writes.is_empty());

        let mut fixture = make_fixture(&[pull(7, "main", false, "head-seven")]);
        fixture.merge_status = StatusCode::CONFLICT;
        let server = serve(fixture).await;
        let outcomes = schedule_open(&server.forgejo, "example/sample", "ci")
            .await
            .unwrap();
        assert!(matches!(
            outcomes[0],
            PullRequestOutcome::ConflictNeedsInspection { number: 7, .. }
        ));
        assert_eq!(server.state.lock().unwrap().writes.len(), 1);
    }

    #[test]
    fn scheduler_exits_nonzero_for_conflicts_and_failures_but_accepts_skips() {
        let outcomes = [
            PullRequestOutcome::Scheduled {
                number: 1,
                head_sha: "head-one".into(),
            },
            PullRequestOutcome::Skipped {
                number: 2,
                head_sha: "head-two".into(),
                reason: "draft",
            },
            PullRequestOutcome::Failed {
                number: 3,
                head_sha: "head-three".into(),
                reason: "synthetic failure".into(),
            },
        ];
        assert!(require_no_actionable_outcomes(&outcomes)
            .unwrap_err()
            .to_string()
            .contains("1 Forgejo pull request outcome(s)"));
        assert!(require_no_actionable_outcomes(&outcomes[..2]).is_ok());
    }
}
