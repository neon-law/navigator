//! Prepare a lawyer-reviewable Gmail draft for a Legal Plan member.

use std::{io::Write, process::ExitCode};

use anyhow::{ensure, Context, Result};
use serde::Deserialize;

use crate::{mail, remote};

#[derive(Deserialize)]
struct Invitation {
    recipient: String,
    portal_url: String,
}

async fn read(base: &str, token: &str, id: uuid::Uuid) -> Result<Invitation> {
    let response = remote::authenticated_client_builder(base)?
        .build()?
        .get(format!("{base}/app/api/projects/{id}/portal-invitation"))
        .bearer_auth(token)
        .send()
        .await
        .context("resolve Project invitation")?;
    let status = response.status();
    ensure!(
        status.is_success(),
        "Project invitation unavailable ({status}): {}",
        response.text().await.unwrap_or_default()
    );
    response.json().await.context("read Project invitation")
}

fn draft_body(url: &str, code: &str) -> Result<String> {
    let url = url::Url::parse(url).context("parse the Project's portal URL")?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    ensure!(
        (url.scheme() == "https" || (local && url.scheme() == "http"))
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == format!("/app/projects/{code}"),
        "the invitation must link to the Project's portal page"
    );
    Ok(format!(
        "Hello,\n\nYou can use your secure matter portal to see your next steps \
         and open documents we have shared with you. Sign in with the email \
         address where you received this invitation.\n\n{url}\n\nPlease reply \
         if you need help signing in."
    ))
}

pub async fn run(host: Option<&str>, code: &str, draft_id: Option<String>) -> ExitCode {
    let result = async {
        ensure!(model::projects::is_valid_code(code), "invalid Project code");
        let (base, token) = remote::resolve(host)?;
        let project_id = super::surfaces::resolve_project_id(&base, &token, code).await?;
        let invitation = read(&base, &token, project_id).await?;
        let body = draft_body(&invitation.portal_url, code)?;
        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(body.as_bytes())?;
        Ok::<_, anyhow::Error>((invitation, file))
    }
    .await;
    let (invitation, file) = match result {
        Ok(invitation) => invitation,
        Err(error) => {
            eprintln!("navigator project invite: {error:#}");
            return ExitCode::FAILURE;
        }
    };
    mail::run(mail::MailAction::Draft {
        to: vec![invitation.recipient],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: Some("Your secure matter portal".to_owned()),
        body_file: Some(file.path().to_path_buf()),
        html_body_file: None,
        thread: None,
        draft_id,
        attach: Vec::new(),
        eml: None,
        in_reply_to: None,
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_draft_only_links_to_the_named_project_portal() {
        let url = "https://member.example.test/app/projects/sample-project";
        assert!(draft_body(url, "sample-project").unwrap().contains(url));
        for invalid in [
            "https://member.example.test/app/projects/another-project",
            "https://git.example.test/firm/sample-project",
            "https://member.example.test/app/projects/sample-project?preview=client",
        ] {
            assert!(draft_body(invalid, "sample-project").is_err());
        }
    }

    #[tokio::test]
    async fn an_invitation_is_read_before_any_draft_is_prepared() {
        use crate::projects::test_login::{answer, site};
        use serde_json::json;
        let id = uuid::Uuid::new_v4();
        let (server, _login) = site(json!([{"id": id, "code": "sample-project"}])).await;
        let host = server.uri();
        assert_eq!(
            run(Some(&host), "Not A Code", None).await,
            ExitCode::FAILURE
        );
        assert_eq!(
            run(Some(&host), "sample-project", None).await,
            ExitCode::FAILURE
        );
        let error = read(&host, "test-token", id).await.err().unwrap();
        assert!(error
            .to_string()
            .starts_with("Project invitation unavailable (404 Not Found)"));

        let door = format!("/app/api/projects/{id}/portal-invitation");
        let invitation = json!({
            "recipient": "member@example.test",
            "portal_url": "https://git.example.test/firm/sample-project",
        });
        answer(&server, "GET", &door, 200, invitation).await;
        let read = read(&host, "test-token", id).await.unwrap();
        assert_eq!(read.recipient, "member@example.test");
        // A portal URL that is not the Project's page stops before Gmail.
        assert_eq!(
            run(Some(&host), "sample-project", None).await,
            ExitCode::FAILURE
        );
    }
}
