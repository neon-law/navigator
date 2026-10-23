//! One file rewritten on its own branch and proposed to `main` as a pull
//! request with governed squash auto-merge. Nothing here writes to `main`.
//!
//! The branch names the change, so a re-run finds its own earlier work: an
//! open pull request for the branch is reused, a closed or merged one is left
//! alone, and a branch an earlier run wrote but never proposed is proposed.

use anyhow::{bail, ensure, Context, Result};
use base64::Engine as _;
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{repository_target, Forgejo, MergeRequestOutcome, Policy};

/// The change one proposal makes: `content` replaces `path` on `branch`,
/// which starts from `main`.
#[derive(Debug, Clone, Copy)]
pub struct FileUpdate<'a> {
    pub branch: &'a str,
    pub path: &'a str,
    pub content: &'a str,
    pub message: &'a str,
    pub title: &'a str,
    pub body: &'a str,
}

/// What a proposal found or did. Mechanism only: no URL, no repository name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateProposal {
    /// A pull request was opened and governed squash auto-merge scheduled.
    Opened,
    /// An earlier run's pull request is still open; auto-merge was asked
    /// for again, which merges it now if its required check already passed.
    Pending,
    /// A pull request for this branch was already closed or merged; none is
    /// reopened.
    Declined,
}

fn safe_ref(value: &str, what: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value != super::DEFAULT_BRANCH
            && !value.starts_with(['/', '.', '-'])
            && !value.ends_with(['/', '.'])
            && !value.contains("..")
            && !value.contains("//")
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-/".contains(&byte)),
        "{what} must be a plain relative name other than main"
    );
    Ok(())
}

impl Forgejo {
    /// Rewrite `update.path` on `update.branch`, open a pull request against
    /// `main`, and schedule governed squash auto-merge on it.
    ///
    /// # Errors
    /// An ungoverned repository; an invalid target, branch, or path; a file missing from `main`; a
    /// refused write; or a [`Self::schedule`] refusal on a new pull request.
    pub async fn propose_file_update(
        &self,
        repository: &str,
        context: &str,
        update: &FileUpdate<'_>,
    ) -> Result<UpdateProposal> {
        let repository = repository_target(repository)?;
        safe_ref(update.branch, "the update branch")?;
        safe_ref(update.path, "the updated path")?;
        // An ungoverned repository gets no branch and no pull request.
        self.verify(repository, &Policy::new(context)?).await?;
        if let Some(pull) = self.pull_for_branch(repository, update.branch).await? {
            if pull["state"] != "open" || pull["merged"] == true {
                return Ok(UpdateProposal::Declined);
            }
            let number = pull_number(&pull)?;
            // A merge already scheduled answers 409 too; the pull request
            // stays open either way and is counted as pending.
            match self
                .schedule_expected(repository, context, number, None)
                .await?
            {
                MergeRequestOutcome::Accepted | MergeRequestOutcome::ConflictNeedsInspection => {}
            }
            return Ok(UpdateProposal::Pending);
        }
        self.write_branch(repository, update).await?;
        let number = self.open_pull(repository, update).await?;
        self.schedule(repository, context, number).await?;
        Ok(UpdateProposal::Opened)
    }

    /// The pull request whose head is `branch` and base is `main`, in any
    /// state, or `None`.
    async fn pull_for_branch(&self, repository: &str, branch: &str) -> Result<Option<Value>> {
        let response = self
            .request(
                Method::GET,
                &format!(
                    "repos/{repository}/pulls/{}/{branch}",
                    super::DEFAULT_BRANCH
                ),
                None,
            )
            .await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let pull: Value = response
            .error_for_status()
            .context("read the update pull request")?
            .json()
            .await
            .context("parse the update pull request")?;
        ensure!(
            pull["head"]["ref"] == branch && pull["base"]["ref"] == super::DEFAULT_BRANCH,
            "Forgejo returned a pull request for another branch"
        );
        Ok(Some(pull))
    }

    /// The blob SHA and text of `path` at `reference`, or `None`.
    async fn file(
        &self,
        repository: &str,
        path: &str,
        reference: &str,
    ) -> Result<Option<(String, String)>> {
        let response = self
            .request(
                Method::GET,
                &format!("repos/{repository}/contents/{path}?ref={reference}"),
                None,
            )
            .await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let body: Value = response
            .error_for_status()
            .context("read repository file")?
            .json()
            .await
            .context("parse repository file")?;
        ensure!(
            body["type"] == "file" && body["encoding"] == "base64",
            "Forgejo described the path as something other than a base64 file"
        );
        let sha = body["sha"]
            .as_str()
            .filter(|sha| !sha.is_empty())
            .context("Forgejo omitted the file's SHA")?;
        let encoded: String = body["content"]
            .as_str()
            .context("Forgejo omitted the file's content")?
            .split_whitespace()
            .collect();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .context("decode repository file")?;
        let text = String::from_utf8(bytes).context("repository file is not UTF-8")?;
        Ok(Some((sha.to_owned(), text)))
    }

    /// Put `update.content` on `update.branch`: cut from `main` when the
    /// branch does not exist, rewritten in place when an earlier run left
    /// other text there, untouched when it already holds the content.
    async fn write_branch(&self, repository: &str, update: &FileUpdate<'_>) -> Result<()> {
        let (sha, branch_fields) = match self.file(repository, update.path, update.branch).await? {
            Some((_, text)) if text == update.content => return Ok(()),
            Some((sha, _)) => (sha, json!({"branch": update.branch})),
            None => {
                let (sha, _) = self
                    .file(repository, update.path, super::DEFAULT_BRANCH)
                    .await?
                    .context("the updated file is missing from main")?;
                (
                    sha,
                    json!({"branch": super::DEFAULT_BRANCH, "new_branch": update.branch}),
                )
            }
        };
        let mut body = json!({
            "content": base64::engine::general_purpose::STANDARD.encode(update.content),
            "sha": sha,
            "message": update.message,
        });
        body.as_object_mut()
            .context("file update body")?
            .extend(branch_fields.as_object().cloned().unwrap_or_default());
        self.request(
            Method::PUT,
            &format!("repos/{repository}/contents/{}", update.path),
            Some(&body),
        )
        .await?
        .error_for_status()
        .context("write the update branch")?;
        Ok(())
    }

    async fn open_pull(&self, repository: &str, update: &FileUpdate<'_>) -> Result<u64> {
        let response = self
            .request(
                Method::POST,
                &format!("repos/{repository}/pulls"),
                Some(&json!({
                    "base": super::DEFAULT_BRANCH,
                    "head": update.branch,
                    "title": update.title,
                    "body": update.body,
                })),
            )
            .await?;
        if response.status() == StatusCode::CONFLICT {
            // Another writer opened it first: use that one.
            let pull = self
                .pull_for_branch(repository, update.branch)
                .await?
                .context("Forgejo reported a pull request it then could not find")?;
            return pull_number(&pull);
        }
        if !matches!(response.status(), StatusCode::OK | StatusCode::CREATED) {
            bail!(
                "Forgejo refused to open the update pull request ({})",
                response.status()
            );
        }
        let pull: Value = response
            .json()
            .await
            .context("parse the opened pull request")?;
        pull_number(&pull)
    }
}

fn pull_number(pull: &Value) -> Result<u64> {
    pull["number"]
        .as_u64()
        .filter(|number| *number > 0)
        .context("Forgejo pull request omitted a positive number")
}

#[cfg(test)]
mod tests {
    use super::super::REQUIRED_CHECK_CONTEXT;
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path, query_param};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    const REPO: &str = "example-firm/sample-transactional";
    const BRANCH: &str = "navigator/update-26.10.5";
    const API: &str = "/api/v1/repos/example-firm/sample-transactional";

    fn update() -> FileUpdate<'static> {
        FileUpdate {
            branch: BRANCH,
            path: "navigator.yaml",
            content: "version: 26.10.5\nproject:\n  name: sample\n",
            message: "Update Navigator to 26.10.5",
            title: "Update Navigator to 26.10.5",
            body: "Synthetic body",
        }
    }

    fn contents(text: &str, sha: &str) -> Value {
        json!({
            "type": "file", "encoding": "base64", "sha": sha,
            "content": base64::engine::general_purpose::STANDARD.encode(text),
        })
    }

    fn pull(state: &str, merged: bool) -> Value {
        json!({
            "number": 7, "state": state, "merged": merged, "draft": false,
            "head": {"ref": BRANCH, "sha": "synthetic-head"}, "base": {"ref": "main"},
            "title": "Update Navigator to 26.10.5", "body": "Synthetic body",
        })
    }

    /// The governed repository, an empty review list, and pending checks:
    /// everything `schedule` reads before it asks for a merge.
    async fn governed(server: &MockServer) {
        let mut repo = Policy::repository();
        repo["allow_manual_merge"] = false.into();
        let branch = Policy::new(REQUIRED_CHECK_CONTEXT).unwrap().branch();
        Mock::given(method("GET"))
            .and(path(API))
            .respond_with(ResponseTemplate::new(200).set_body_json(repo))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{API}/branch_protections")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([branch.clone()])))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{API}/branch_protections/main")))
            .respond_with(ResponseTemplate::new(200).set_body_json(branch))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{API}/pulls/7")))
            .respond_with(ResponseTemplate::new(200).set_body_json(pull("open", false)))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{API}/pulls/7/reviews")))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-total-count", "0")
                    .set_body_json(json!([])),
            )
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{API}/commits/synthetic-head/status")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"statuses": []})))
            .mount(server)
            .await;
    }

    fn client(server: &MockServer) -> Forgejo {
        Forgejo::new(&server.uri(), "synthetic-token".into()).unwrap()
    }

    fn writes(requests: &[Request]) -> Vec<(String, String)> {
        requests
            .iter()
            .filter(|request| request.method.as_str() != "GET")
            .map(|request| (request.method.to_string(), request.url.path().to_owned()))
            .collect()
    }

    #[tokio::test]
    async fn a_stale_file_is_cut_onto_its_own_branch_proposed_and_scheduled_never_on_main() {
        let server = MockServer::start().await;
        governed(&server).await;
        Mock::given(method("GET"))
            .and(path(format!("{API}/pulls/main/{BRANCH}")))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{API}/contents/navigator.yaml")))
            .and(query_param("ref", BRANCH))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{API}/contents/navigator.yaml")))
            .and(query_param("ref", "main"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(contents("version: 26.9.1\n", "blob-1")),
            )
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path(format!("{API}/contents/navigator.yaml")))
            .and(body_partial_json(json!({
                "sha": "blob-1", "branch": "main", "new_branch": BRANCH,
                "content": base64::engine::general_purpose::STANDARD.encode(update().content),
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("{API}/pulls")))
            .and(body_partial_json(json!({"base": "main", "head": BRANCH})))
            .respond_with(ResponseTemplate::new(201).set_body_json(pull("open", false)))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("{API}/pulls/7/merge")))
            .and(body_partial_json(json!({
                "Do": "squash", "merge_when_checks_succeed": true, "force_merge": false,
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let proposal = client(&server)
            .propose_file_update(REPO, REQUIRED_CHECK_CONTEXT, &update())
            .await
            .unwrap();
        assert_eq!(proposal, UpdateProposal::Opened);
        let writes = writes(&server.received_requests().await.unwrap());
        assert_eq!(writes.len(), 3, "{writes:?}");
        assert!(
            writes
                .iter()
                .all(|(_, path)| !path.contains("/branches/main")),
            "nothing is pushed to main"
        );
    }

    #[tokio::test]
    async fn an_open_pull_request_for_the_branch_is_reused_not_duplicated() {
        let server = MockServer::start().await;
        governed(&server).await;
        Mock::given(method("GET"))
            .and(path(format!("{API}/pulls/main/{BRANCH}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(pull("open", false)))
            .mount(&server)
            .await;
        // Forgejo answers 409 when the merge is already scheduled.
        Mock::given(method("POST"))
            .and(path(format!("{API}/pulls/7/merge")))
            .respond_with(ResponseTemplate::new(409))
            .expect(1)
            .mount(&server)
            .await;

        let proposal = client(&server)
            .propose_file_update(REPO, REQUIRED_CHECK_CONTEXT, &update())
            .await
            .unwrap();
        assert_eq!(proposal, UpdateProposal::Pending);
        let writes = writes(&server.received_requests().await.unwrap());
        assert_eq!(writes, [("POST".into(), format!("{API}/pulls/7/merge"))]);
    }

    #[tokio::test]
    async fn a_closed_or_merged_pull_request_for_the_branch_is_not_reopened() {
        for (state, merged) in [("closed", false), ("closed", true)] {
            let server = MockServer::start().await;
            governed(&server).await;
            Mock::given(method("GET"))
                .and(path(format!("{API}/pulls/main/{BRANCH}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(pull(state, merged)))
                .mount(&server)
                .await;
            let proposal = client(&server)
                .propose_file_update(REPO, REQUIRED_CHECK_CONTEXT, &update())
                .await
                .unwrap();
            assert_eq!(proposal, UpdateProposal::Declined);
            assert!(writes(&server.received_requests().await.unwrap()).is_empty());
        }
    }

    #[tokio::test]
    async fn a_branch_an_earlier_run_wrote_is_proposed_without_a_second_write() {
        let server = MockServer::start().await;
        governed(&server).await;
        Mock::given(method("GET"))
            .and(path(format!("{API}/pulls/main/{BRANCH}")))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{API}/contents/navigator.yaml")))
            .and(query_param("ref", BRANCH))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(contents(update().content, "blob-2")),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("{API}/pulls")))
            .respond_with(ResponseTemplate::new(201).set_body_json(pull("open", false)))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("{API}/pulls/7/merge")))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(
            client(&server)
                .propose_file_update(REPO, REQUIRED_CHECK_CONTEXT, &update())
                .await
                .unwrap(),
            UpdateProposal::Opened
        );
        assert!(writes(&server.received_requests().await.unwrap())
            .iter()
            .all(|(method, _)| method != "PUT"));
    }

    #[tokio::test]
    async fn an_ungoverned_repository_gets_no_branch_and_no_pull_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(API))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{API}/branch_protections")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{API}/branch_protections/main")))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let error = client(&server)
            .propose_file_update(REPO, REQUIRED_CHECK_CONTEXT, &update())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("does not match"), "{error:#}");
        assert!(writes(&server.received_requests().await.unwrap()).is_empty());
    }

    #[test]
    fn the_branch_and_path_cannot_be_main_or_escape_the_repository() {
        for value in ["", "main", "../x", "/x", "a//b", "a b", "a?b", "x/", "-x"] {
            assert!(safe_ref(value, "ref").is_err(), "{value}");
        }
        assert!(safe_ref(BRANCH, "ref").is_ok());
        assert!(safe_ref("navigator.yaml", "path").is_ok());
    }
}
