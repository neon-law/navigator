//! Stage a single timekeeper's explicit billing trailer from a Project checkout.
use std::{path::Path, process::Command, time::Duration};

use anyhow::{ensure, Context, Result};
use chrono::NaiveDate;
use uuid::Uuid;

use super::{manifest, surfaces};

/// Format: Billing-Time: <hours in tenths> <work date> <entry UUID>.
/// Only machine fields leave the checkout. The lawyer supplies the client narrative at review.
fn trailer(description: &str) -> Result<(i32, NaiveDate, Uuid)> {
    let block = description.trim_end().rsplit("\n\n").next().unwrap_or("");
    let mut lines = block
        .lines()
        .filter_map(|line| line.strip_prefix("Billing-Time: "));
    let fields: Vec<_> = lines
        .next()
        .context("missing Billing-Time trailer")?
        .split_whitespace()
        .collect();
    ensure!(
        lines.next().is_none() && fields.len() == 3,
        "expected one Billing-Time trailer with hours, date, and UUID"
    );
    let (whole, fraction) = fields[0]
        .split_once('.')
        .context("use hours in exact tenths, such as 0.2")?;
    ensure!(
        !whole.is_empty()
            && whole.bytes().all(|b| b.is_ascii_digit())
            && fraction.len() == 1
            && fraction.as_bytes()[0].is_ascii_digit(),
        "use hours in exact tenths"
    );
    let tenths = whole
        .parse::<i32>()?
        .checked_mul(10)
        .and_then(|v| v.checked_add(i32::from(fraction.as_bytes()[0] - b'0')))
        .context("invalid hours")?;
    ensure!(
        (1..=240).contains(&tenths),
        "hours must be between 0.1 and 24.0"
    );
    Ok((tenths, fields[1].parse()?, fields[2].parse()?))
}

pub async fn stage(revision: &str) -> Result<()> {
    let dir = Path::new(".");
    ensure!(dir.join(".jj").is_dir(), "run from a Jujutsu Project root");
    let manifest = manifest::read(dir).context("missing Project manifest")?;
    let host = manifest.host.context("missing Project host")?;
    let project = manifest.project.context("missing Project code")?;
    // JSON escaping keeps embedded newlines and control characters inside the description.
    let template = r#"json({"commit_id" => commit_id, "change_id" => change_id, "description" => description, "conflict" => conflict, "divergent" => divergent}) ++ "\n""#;
    let output = Command::new("jj")
        .args([
            "log",
            "--no-graph",
            "--no-pager",
            "-r",
            revision,
            "-T",
            template,
        ])
        .output()
        .context("read Jujutsu change")?;
    ensure!(output.status.success(), "could not read Jujutsu revision");
    let commit: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("select exactly one Jujutsu change")?;
    ensure!(
        commit["conflict"] == false && commit["divergent"] == false,
        "resolve the change before staging time"
    );
    let (tenths, performed_on, id) = trailer(
        commit["description"]
            .as_str()
            .context("missing change description")?,
    )?;
    let crate::remote::MatchingSession::Valid(base, token) =
        crate::remote::matching_stored_session(&host)?
    else {
        anyhow::bail!("sign in to the host in navigator.yaml before staging time")
    };
    let project_id = surfaces::resolve_project_id(&base, &token, &project).await?;
    let body = serde_json::json!({
        "id":id,
        "change_id":commit["change_id"],
        "commit_id":commit["commit_id"],
        "tenths":tenths,
        "performed_on":performed_on,
    });
    let response = reqwest::Client::new()
        .post(format!(
            "{base}/app/api/projects/{project_id}/billing-annotations"
        ))
        .bearer_auth(token)
        .timeout(Duration::from_secs(30))
        .json(&body)
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "staging refused ({})",
        response.status()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_explicit_final_trailer_can_stage_exact_tenths() {
        let row = "Billing-Time: 0.2 2026-11-20 00000000-0000-4000-8000-000000000001";
        assert_eq!(
            trailer(&format!("Private subject\n\n{row}\n")).unwrap().0,
            2
        );
        for invalid in [
            row.replace("0.2", "0.01"),
            row.replace("0.2", "24.1"),
            row.replace("0.2", "-0.2"),
            format!("{row}\n{row}"),
            format!("{row}\n\nUnrelated body"),
        ] {
            assert!(trailer(&invalid).is_err());
        }
    }
}
