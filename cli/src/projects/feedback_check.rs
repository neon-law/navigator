//! `navigator project feedback-check`: the Pre-Filing PII Check.
//!
//! The draft never leaves this process. Authenticated scans resolve private
//! Project, Person, and Entity terms through the deployment's scoped
//! `GET /app/api/projects/{id}/private-terms` door with the `site login`
//! session. A lawyer's machine holds a bearer token, not database coordinates.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, ensure, Result};
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
    identifiers: Vec<String>,
}

#[derive(Deserialize)]
struct Participant {
    name: String,
    email: String,
    #[serde(default)]
    phone: Option<String>,
}

/// Resolve named Projects, or every visible Project when `codes` is empty.
/// Reads use stable ids so codes shared by different Firms remain distinct.
/// Unknown or ambiguous codes and incomplete reads refuse the entire check.
pub(crate) async fn private_terms(
    base: &str,
    token: &str,
    codes: &[String],
) -> Result<Vec<String>> {
    let client = reqwest::Client::new();
    let visible = surfaces::list_visible_projects(base, token)
        .await
        .map_err(|_| {
            anyhow!("authorized Project list unavailable; privacy coverage is incomplete")
        })?;
    let mut terms = Vec::new();
    let selected = if codes.is_empty() {
        visible.iter().collect::<Vec<_>>()
    } else {
        let mut selected = Vec::new();
        for code in codes {
            let code = code.trim();
            ensure!(!code.is_empty(), "a Project code cannot be blank");
            let mut matches = visible.iter().filter(|project| project.code == code);
            let project = matches
                .next()
                .ok_or_else(|| anyhow!("no visible matter with that code"))?;
            ensure!(
                matches.next().is_none(),
                "that Project code is ambiguous; use --all-projects"
            );
            selected.push(project);
        }
        selected
    };
    for project in selected {
        let id = project.id;
        let response = client
            .get(format!("{base}/app/api/projects/{id}/private-terms"))
            .timeout(Duration::from_secs(30))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| anyhow!("could not read private terms; privacy coverage is incomplete"))?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "private terms refused: {}; a firm-side participant on the Project reads them",
                response.status()
            ));
        }
        let read: PrivateTerms = response.json().await.map_err(|_| {
            anyhow!("incomplete private-terms response; privacy coverage is incomplete")
        })?;
        ensure!(
            read.code == project.code,
            "private terms do not match the selected Project"
        );
        terms.extend(read.identifiers);
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
    all_projects: bool,
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
    // Term-only checks work offline without any login.
    let resolved = if projects.is_empty() && !all_projects {
        Vec::new()
    } else {
        let read = async {
            let local = manifest::read(Path::new("."));
            let host = host.or_else(|| local.as_ref().and_then(|m| m.host.as_deref()));
            let (base, token) = remote::resolve(host).map_err(|_| {
                anyhow!("site login unavailable or expired; privacy coverage is incomplete")
            })?;
            private_terms(&base, &token, projects).await
        };
        match read.await {
            Ok(terms) => terms,
            Err(e) => {
                eprintln!("navigator: privacy coverage could not complete: {e:#}");
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
                "identifiers": [],
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
    async fn all_projects_scans_an_unmentioned_matter_and_its_entity() {
        let server = site().await;
        Mock::given(method("GET"))
            .and(path(
                "/app/api/projects/11111111-1111-1111-1111-111111111111/private-terms",
            ))
            .and(header("authorization", "Bearer a-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": "beta", "name": "Beta Matter", "participants": [],
                "identifiers": ["Synthetic Widget Company"]
            })))
            .mount(&server)
            .await;
        let terms = private_terms(&server.uri(), "a-token", &[]).await.unwrap();
        for draft in [
            "beta failed",
            "Synthetic Widget Company had a failure",
            "Harper Cruller",
        ] {
            assert!(!cli::feedback_scan::scan(draft, &terms).is_empty());
        }
        assert!(
            cli::feedback_scan::scan("the command fails on an empty directory", &terms).is_empty()
        );
    }

    #[tokio::test]
    async fn all_projects_refuses_incomplete_coverage() {
        let server = site().await;
        assert!(private_terms(&server.uri(), "a-token", &[]).await.is_err());
    }

    #[tokio::test]
    async fn failed_project_listing_does_not_echo_a_remote_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(403).set_body_string("Synthetic Private Marker"))
            .mount(&server)
            .await;
        let error = private_terms(&server.uri(), "a-token", &[])
            .await
            .unwrap_err();
        assert!(!format!("{error:#}").contains("Synthetic Private Marker"));
        assert!(format!("{error:#}").contains("privacy coverage is incomplete"));
    }

    #[tokio::test]
    async fn an_incomplete_term_response_cannot_pass_as_clean() {
        let server = site().await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/projects/{ID}/private-terms")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": "cruller-estate", "name": "Cruller Estate", "participants": []
            })))
            .with_priority(1)
            .mount(&server)
            .await;
        assert!(
            private_terms(&server.uri(), "a-token", &["cruller-estate".into()])
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn identical_codes_are_read_by_id_in_all_projects_and_refused_when_ambiguous() {
        let server = site().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": "11111111-1111-1111-1111-111111111111", "code": "shared"},
                {"id": ID, "code": "shared"}
            ])))
            .with_priority(1)
            .mount(&server)
            .await;
        for (id, name) in [
            (
                "11111111-1111-1111-1111-111111111111",
                "First Synthetic Entity",
            ),
            (ID, "Second Synthetic Entity"),
        ] {
            Mock::given(method("GET")).and(path(format!("/app/api/projects/{id}/private-terms")))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "code": "shared", "name": "Synthetic Matter", "participants": [], "identifiers": [name]
                }))).with_priority(1).mount(&server).await;
        }
        let terms = private_terms(&server.uri(), "a-token", &[]).await.unwrap();
        assert!(!cli::feedback_scan::scan("First Synthetic Entity", &terms).is_empty());
        assert!(!cli::feedback_scan::scan("Second Synthetic Entity", &terms).is_empty());
        assert!(private_terms(&server.uri(), "a-token", &["shared".into()])
            .await
            .is_err());
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
