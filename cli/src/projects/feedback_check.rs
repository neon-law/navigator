//! `navigator project feedback-check`: the Pre-Filing PII Check.
//!
//! The draft never leaves this process. A `--project` draft resolves that
//! Project's private terms — its code, matter name, and every participant's
//! name, mailbox, and phone — through the deployment's
//! `GET /app/api/projects/{id}/private-terms` door with the `site login`
//! session, the same way the other `project` verbs authenticate. A lawyer's
//! machine holds a bearer token, not database coordinates.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, ensure, Context, Result};
use serde::Deserialize;

use crate::{
    projects::{manifest, surfaces},
    remote,
};

#[derive(Deserialize)]
struct PrivateTerms {
    code: String,
    name: String,
    participants: Vec<Participant>,
}

#[derive(Deserialize)]
struct Participant {
    name: String,
    email: String,
    #[serde(default)]
    phone: Option<String>,
}

/// The private terms the named Projects carry, read from `base` with `token`.
/// A code that resolves to no visible Project is an error, never an empty
/// scan — a typo must not read as a clean draft.
pub(crate) async fn private_terms(
    base: &str,
    token: &str,
    codes: &[String],
) -> Result<Vec<String>> {
    let client = reqwest::Client::new();
    let visible = surfaces::list_visible_projects(base, token).await?;
    let mut terms = Vec::new();
    for code in codes {
        let code = code.trim();
        ensure!(!code.is_empty(), "a Project code cannot be blank");
        // Never interpolate the CLI's own argument into a message; see
        // `surfaces::resolve_project_id`.
        let id = visible
            .iter()
            .find(|project| project.code == code)
            .map(|project| project.id)
            .ok_or_else(|| anyhow!("no visible matter with that code"))?;
        let response = client
            .get(format!("{base}/app/api/projects/{id}/private-terms"))
            .timeout(Duration::from_secs(30))
            .bearer_auth(token)
            .send()
            .await
            .context("read the Project's private terms")?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "private terms refused: {}; a firm-side participant on the Project reads them",
                response.status()
            ));
        }
        let read: PrivateTerms = response
            .json()
            .await
            .context("parse the Project's private terms")?;
        terms.push(read.code);
        terms.push(read.name);
        for person in read.participants {
            terms.push(person.name);
            terms.push(person.email);
            terms.extend(person.phone);
        }
    }
    Ok(terms)
}

/// Run the check over `file` (stdin when `None`). Exits 0 on a clean draft,
/// 1 naming each placeholder when it finds an identifier, 2 when the draft or
/// the terms cannot be read.
pub async fn run(
    file: Option<PathBuf>,
    terms: &[String],
    projects: &[String],
    host: Option<&str>,
) -> ExitCode {
    let text = match file {
        Some(path) => std::fs::read_to_string(path),
        None => std::io::read_to_string(std::io::stdin()),
    };
    let Ok(text) = text else {
        eprintln!("navigator: could not read the draft");
        return ExitCode::from(2);
    };
    // The site opens only for a --project draft: a term-only check must work
    // offline, without any login.
    let resolved = if projects.is_empty() {
        Vec::new()
    } else {
        let read = async {
            let local = manifest::read(Path::new("."));
            let host = host.or_else(|| local.as_ref().and_then(|m| m.host.as_deref()));
            let (base, token) = remote::resolve(host)?;
            private_terms(&base, &token, projects).await
        };
        match read.await {
            Ok(terms) => terms,
            Err(e) => {
                eprintln!("navigator: {e:#}");
                return ExitCode::from(2);
            }
        }
    };
    let terms = cli::pii_audit::merge_terms(&resolved, terms);
    let findings = cli::feedback_scan::scan(&text, &terms);
    for f in &findings {
        eprintln!(
            "line {}: {} found; write {} instead",
            f.line, f.kind, f.placeholder
        );
    }
    if findings.is_empty() {
        println!("feedback check: clean");
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const ID: &str = "22222222-2222-2222-2222-222222222222";

    async fn site() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .and(header("authorization", "Bearer a-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": "11111111-1111-1111-1111-111111111111", "code": "beta"},
                {"id": ID, "code": "cruller-estate"},
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/projects/{ID}/private-terms")))
            .and(header("authorization", "Bearer a-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": "cruller-estate",
                "name": "Cruller Estate",
                "participants": [
                    {"name": "Harper Cruller", "email": "harper@crullers.example", "phone": "702-555-0142"},
                    {"name": "Lena Counsel", "email": "lena@firm.example", "phone": null},
                ],
            })))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn resolves_a_projects_code_and_participants_over_the_site_api() {
        let server = site().await;

        let terms = private_terms(&server.uri(), "a-token", &["cruller-estate".to_string()])
            .await
            .expect("resolve the private terms");

        for known in [
            "cruller-estate",
            "Cruller Estate",
            "Harper Cruller",
            "harper@crullers.example",
            "702-555-0142",
            "Lena Counsel",
        ] {
            assert!(
                terms.iter().any(|t| t == known),
                "terms lack {known:?}: {terms:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_resolved_terms_flag_a_participants_name_mailbox_and_phone() {
        let server = site().await;
        let resolved = private_terms(&server.uri(), "a-token", &["cruller-estate".to_string()])
            .await
            .expect("resolve the private terms");
        let terms = cli::pii_audit::merge_terms(&resolved, &[]);

        for draft in [
            "Harper Cruller asked where the filing stands",
            "write to harper@crullers.example",
            "call 702-555-0142",
            "the cruller-estate matter",
        ] {
            assert!(
                !cli::feedback_scan::scan(draft, &terms).is_empty(),
                "{draft:?} scanned clean"
            );
        }
        assert!(cli::feedback_scan::scan(
            "the status command exits 0 when the folder is empty",
            &terms
        )
        .is_empty());
    }

    #[tokio::test]
    async fn an_unknown_or_blank_code_is_an_error_not_a_clean_scan() {
        let server = site().await;

        for code in ["no-such-project", "  "] {
            assert!(
                private_terms(&server.uri(), "a-token", &[code.to_string()])
                    .await
                    .is_err(),
                "{code:?} must not scan as nothing"
            );
        }
    }

    #[tokio::test]
    async fn a_refused_read_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([{"id": ID, "code": "cruller-estate"}])),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/projects/{ID}/private-terms")))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let error = private_terms(&server.uri(), "a-token", &["cruller-estate".to_string()])
            .await
            .expect_err("a 404 must not scan as nothing");

        assert!(format!("{error:#}").contains("404"));
    }
}
