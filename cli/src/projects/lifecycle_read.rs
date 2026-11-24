//! Read a Project's onboarding status (`optional`, `pitch`, or `official`) through its
//! authenticated, matter-scoped API.
//!
//! The status is derived server-side from completed `DocuSign` evidence. Navigator
//! must hold the onboarding envelope UUID and verify completion in the Firm account.
//! Envelopes can originate in Navigator or be adopted from the Firm's `DocuSign` app.

use anyhow::{anyhow, Result};
use std::process::ExitCode;

pub async fn run(host: Option<&str>, project: Option<&str>, json: bool) -> ExitCode {
    match read(host, project).await {
        Ok(value) => {
            let code = value["code"].as_str().unwrap_or("unknown");
            let status = value["onboarding_status"].as_str().unwrap_or("unknown");
            if json {
                println!("{value}");
            } else {
                println!("{code}: {status}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator project lifecycle: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn read(host: Option<&str>, project: Option<&str>) -> Result<serde_json::Value> {
    let status = super::status::read(host, project).await?;
    let onboarding_status = status["onboarding"]["status"]
        .as_str()
        .ok_or_else(|| anyhow!("the Project status read carried no onboarding status"))?;
    Ok(serde_json::json!({
        "code": status["code"],
        "onboarding_status": onboarding_status,
    }))
}

#[cfg(test)]
mod tests {
    use super::{read, run};
    use crate::projects::test_login::{answer, site};
    use serde_json::json;
    use std::process::ExitCode;

    const CODE: &str = "sample-matter";

    #[tokio::test]
    async fn the_onboarding_status_is_read_from_the_matter_scoped_door() {
        let id = uuid::Uuid::new_v4();
        let inventory = json!([{"id": id, "code": CODE}]);
        let (server, _login) = site(inventory.clone()).await;
        let host = server.uri();
        let host = Some(host.as_str());
        let door = format!("/app/api/projects/{id}/status");

        assert_eq!(run(host, Some(CODE), false).await, ExitCode::FAILURE);
        let error = read(host, Some(CODE)).await.unwrap_err();
        assert!(error.to_string().contains("failed: 404"), "{error}");

        answer(&server, "GET", &door, 200, json!({"code": CODE})).await;
        let error = read(host, Some(CODE)).await.unwrap_err();
        assert!(error.to_string().contains("carried no onboarding status"));

        server.reset().await;
        answer(&server, "GET", "/app/api/projects", 200, inventory).await;
        let status = json!({"code": CODE, "onboarding": {"status": "pitch"}});
        answer(&server, "GET", &door, 200, status).await;
        assert_eq!(
            read(host, Some(CODE)).await.unwrap(),
            json!({"code": CODE, "onboarding_status": "pitch"})
        );
        for as_json in [false, true] {
            assert_eq!(run(host, Some(CODE), as_json).await, ExitCode::SUCCESS);
        }
    }
}
