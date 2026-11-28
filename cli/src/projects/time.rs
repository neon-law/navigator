//! `navigator project time`: record, edit, review, and backfill a Project's
//! time from the CLI (LAW-583, LAW-584).
//!
//! Recorded and backfilled time waits as a Time Entry Proposal until a lawyer
//! approves it; approval prices it into a Time Entry. Staged Jujutsu time
//! appears in the same review list and takes the same `approve` and
//! `discard` verbs.
use std::fmt::Write;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::NaiveDate;
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{legal_plans, remote};

#[derive(Subcommand)]
pub enum TimeCmd {
    /// Record time for review. A past date marks the entry backfilled.
    Add {
        /// The day the work was done (YYYY-MM-DD).
        #[arg(long)]
        performed_on: NaiveDate,
        /// Whole minutes, 1 to 1440.
        #[arg(long)]
        minutes: i32,
        /// What the work was, as the invoice should read.
        #[arg(long)]
        narrative: String,
        /// The Person who did the work, by email or UUID. Defaults to you.
        #[arg(long)]
        timekeeper: Option<String>,
        #[command(flatten)]
        request: RequestId,
    },
    /// Change a pending entry's minutes, narrative, or date.
    Edit {
        /// The pending entry's id, from `time list`.
        id: Uuid,
        #[arg(long)]
        minutes: Option<i32>,
        #[arg(long)]
        narrative: Option<String>,
        #[arg(long)]
        performed_on: Option<NaiveDate>,
    },
    /// Approve a pending entry or staged Jujutsu time as a Time Entry.
    Approve {
        /// The pending entry's id, from `time list`.
        id: Uuid,
        /// How the time is charged.
        #[arg(long, value_enum, default_value_t = Charge::Bill)]
        charge: Charge,
        /// The Legal Plan Claim the time belongs to, with `--charge plan-claim`.
        #[arg(long)]
        claim: Option<Uuid>,
        /// The invoice wording. Required for staged Jujutsu time.
        #[arg(long)]
        narrative: Option<String>,
        /// Fewer minutes than staged Jujutsu time attests.
        #[arg(long)]
        minutes: Option<i32>,
    },
    /// Discard a pending entry, or deny staged Jujutsu time.
    Discard {
        /// The pending entry's id, from `time list`.
        id: Uuid,
        /// Why, kept with a denied Jujutsu change.
        #[arg(long)]
        reason: Option<String>,
    },
    /// List the Project's time: pending review, approved, unbilled, billed, or discarded.
    List {
        #[arg(long, value_enum)]
        status: Option<Status>,
    },
    /// Propose entries from the Project's dated record for review.
    Backfill {
        /// Only records dated on or after this day (YYYY-MM-DD).
        #[arg(long)]
        since: Option<NaiveDate>,
        /// Print the proposals and the minutes table without writing them.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Args)]
pub struct RequestId {
    /// A stable id for this request. Repeating it returns the first entry
    /// instead of adding another. Defaults to a fresh id.
    #[arg(long)]
    request_id: Option<Uuid>,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Charge {
    Bill,
    NoCharge,
    PlanClaim,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Status {
    PendingReview,
    Approved,
    Unbilled,
    Billed,
    Discarded,
}

impl Status {
    const fn as_str(self) -> &'static str {
        match self {
            Self::PendingReview => "pending_review",
            Self::Approved => "approved",
            Self::Unbilled => "unbilled",
            Self::Billed => "billed",
            Self::Discarded => "discarded",
        }
    }
}

impl TimeCmd {
    /// The method, path under the Project route, and body of this verb.
    fn request(&self) -> Result<(reqwest::Method, String, Option<Value>)> {
        use reqwest::Method;
        const ROOT: &str = "time-entry-proposals";
        Ok(match self {
            Self::Add {
                performed_on,
                minutes,
                narrative,
                timekeeper,
                request,
            } => (
                Method::POST,
                ROOT.into(),
                Some(json!({
                    "request_id": request.request_id.unwrap_or_else(Uuid::now_v7),
                    "performed_on": performed_on,
                    "minutes": minutes,
                    "narrative": narrative,
                    "timekeeper": timekeeper,
                })),
            ),
            Self::Edit {
                id,
                minutes,
                narrative,
                performed_on,
            } => {
                let mut body = serde_json::Map::new();
                for (key, value) in [
                    ("minutes", minutes.map(Value::from)),
                    ("narrative", narrative.clone().map(Value::from)),
                    (
                        "performed_on",
                        performed_on.map(|day| Value::from(day.to_string())),
                    ),
                ] {
                    if let Some(value) = value {
                        body.insert(key.into(), value);
                    }
                }
                if body.is_empty() {
                    return Err(anyhow!("name --minutes, --narrative, or --performed-on"));
                }
                (
                    Method::PATCH,
                    format!("{ROOT}/{id}"),
                    Some(Value::Object(body)),
                )
            }
            Self::Approve {
                id,
                charge,
                claim,
                narrative,
                minutes,
            } => {
                let decision = match (charge, claim) {
                    (Charge::PlanClaim, None) => {
                        return Err(anyhow!("--charge plan-claim needs --claim <uuid>"))
                    }
                    (Charge::Bill | Charge::NoCharge, Some(_)) => {
                        return Err(anyhow!("--claim goes with --charge plan-claim"))
                    }
                    (Charge::Bill, None) => "bill",
                    (Charge::NoCharge, None) => "no_charge",
                    (Charge::PlanClaim, Some(_)) => "plan_claim",
                };
                (
                    Method::POST,
                    format!("{ROOT}/{id}/approve"),
                    Some(json!({
                        "decision": decision, "claim_id": claim, "narrative": narrative, "minutes": minutes,
                    })),
                )
            }
            Self::Discard { id, reason } => (
                Method::POST,
                format!("{ROOT}/{id}/discard"),
                Some(json!({ "reason": reason })),
            ),
            Self::List { status } => (
                Method::GET,
                status.map_or_else(
                    || ROOT.to_owned(),
                    |status| format!("{ROOT}?status={}", status.as_str()),
                ),
                None,
            ),
            Self::Backfill { since, dry_run } => (
                Method::POST,
                format!("{ROOT}/backfill"),
                Some(json!({ "since": since, "dry_run": dry_run })),
            ),
        })
    }
}

/// Run one `navigator project time` verb against the Project's site.
pub async fn run(
    host: Option<&str>,
    project: Option<&str>,
    json: bool,
    action: TimeCmd,
) -> ExitCode {
    let backfill = matches!(action, TimeCmd::Backfill { .. });
    let result = async {
        let (route, token, _) = legal_plans::project_route(host, project).await?;
        let value = send(&route, &token, &action).await?;
        Ok(if json {
            serde_json::to_string_pretty(&value)?
        } else if backfill {
            render_backfill(&value)?
        } else {
            render_rows(&value)?
        })
    }
    .await;
    crate::billing::finish("project time", result)
}

pub(crate) async fn send(route: &str, token: &str, action: &TimeCmd) -> Result<Value> {
    let (method, path, body) = action.request()?;
    let client = remote::authenticated_client_builder(route)?.build()?;
    let mut request = client
        .request(method, format!("{route}/{path}"))
        .bearer_auth(token)
        .timeout(Duration::from_secs(60));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.context("send the time request")?;
    let status = response.status();
    let text = response.text().await.context("read the time response")?;
    if !status.is_success() {
        let reason = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|body| body["error"].as_str().map(str::to_owned))
            .unwrap_or(text);
        return Err(anyhow!("refused ({status}): {reason}"));
    }
    serde_json::from_str(&text).context("parse the time response")
}

fn row_line(text: &mut String, row: &Value) -> std::fmt::Result {
    write!(
        text,
        "{} · {} · {} min · {} · {} · {}",
        row["performed_on"].as_str().unwrap_or_default(),
        row["id"].as_str().unwrap_or_default(),
        row["minutes"],
        row["status"].as_str().unwrap_or_default(),
        row["source"].as_str().unwrap_or_default(),
        row["narrative"].as_str().unwrap_or_default(),
    )?;
    if row["backfilled"].as_bool() == Some(true) {
        write!(text, " · backfilled")?;
    }
    if let Some(entry) = row["entry_id"].as_str() {
        write!(text, " · Time Entry {entry}")?;
    }
    writeln!(text)
}

fn render_rows(value: &Value) -> Result<String> {
    let mut text = String::new();
    match value.as_array() {
        Some(rows) if rows.is_empty() => text.push_str("No time\n"),
        Some(rows) => {
            for row in rows {
                row_line(&mut text, row)?;
            }
        }
        None => row_line(&mut text, value)?,
    }
    Ok(text)
}

fn render_backfill(value: &Value) -> Result<String> {
    let mut text = String::from("Default minutes\n");
    for row in value["default_minutes"].as_array().into_iter().flatten() {
        writeln!(
            text,
            "  {:<17} {:>3} min  {}",
            row["kind"].as_str().unwrap_or_default(),
            row["minutes"],
            row["rule"].as_str().unwrap_or_default()
        )?;
    }
    let proposals = value["proposals"].as_array().map_or(&[][..], Vec::as_slice);
    let verb = if value["dry_run"].as_bool() == Some(true) {
        "Would propose"
    } else {
        "Proposed for review"
    };
    writeln!(text, "\n{verb}: {}", proposals.len())?;
    for proposal in proposals {
        writeln!(
            text,
            "  {} · {} min · {} {} · {}{}",
            proposal["performed_on"].as_str().unwrap_or_default(),
            proposal["minutes"],
            proposal["source"].as_str().unwrap_or_default(),
            proposal["source_id"].as_str().unwrap_or_default(),
            proposal["narrative"].as_str().unwrap_or_default(),
            proposal["proposal_id"]
                .as_str()
                .map(|id| format!(" · {id}"))
                .unwrap_or_default(),
        )?;
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Time {
        #[command(subcommand)]
        action: TimeCmd,
    }

    fn parse(args: &[&str]) -> TimeCmd {
        Time::try_parse_from(std::iter::once("time").chain(args.iter().copied()))
            .unwrap()
            .action
    }

    #[test]
    fn each_verb_names_its_door_and_body() {
        let request = Uuid::now_v7().to_string();
        let (method, path, body) = parse(&[
            "add",
            "--performed-on",
            "2026-11-20",
            "--minutes",
            "30",
            "--narrative",
            "Synthetic call",
            "--request-id",
            &request,
        ])
        .request()
        .unwrap();
        assert_eq!(
            (method.as_str(), path.as_str()),
            ("POST", "time-entry-proposals")
        );
        assert_eq!(body.unwrap()["request_id"], request.as_str());
        let id = Uuid::now_v7();
        let (method, path, body) = parse(&["edit", &id.to_string(), "--minutes", "18"])
            .request()
            .unwrap();
        assert_eq!(
            (method.as_str(), path),
            ("PATCH", format!("time-entry-proposals/{id}"))
        );
        assert_eq!(body, Some(json!({"minutes": 18})));
        assert!(parse(&["edit", &id.to_string()]).request().is_err());
        assert!(
            parse(&["approve", &id.to_string(), "--charge", "plan-claim"])
                .request()
                .is_err()
        );
        let claim = Uuid::now_v7().to_string();
        let (_, path, body) = parse(&[
            "approve",
            &id.to_string(),
            "--charge",
            "plan-claim",
            "--claim",
            &claim,
        ])
        .request()
        .unwrap();
        assert_eq!(path, format!("time-entry-proposals/{id}/approve"));
        assert_eq!(body.unwrap()["decision"], "plan_claim");
        let (method, path, _) = parse(&["list", "--status", "pending-review"])
            .request()
            .unwrap();
        assert_eq!(
            (method.as_str(), path.as_str()),
            ("GET", "time-entry-proposals?status=pending_review")
        );
        let (_, _, body) = parse(&["backfill", "--dry-run"]).request().unwrap();
        assert_eq!(body, Some(json!({"since": null, "dry_run": true})));
    }

    #[test]
    fn a_backfill_prints_the_minutes_table_and_each_source() {
        let text = render_backfill(&json!({
            "dry_run": true,
            "default_minutes": [{"kind": "call", "minutes": 12, "rule": "stated duration"}],
            "proposals": [{"performed_on": "2026-11-20", "minutes": 30, "source": "document",
                "source_id": "00000000-0000-0000-0000-000000000001", "narrative": "Prepared a letter"}],
        }))
        .unwrap();
        assert!(
            text.contains("call") && text.contains("stated duration"),
            "{text}"
        );
        assert!(text.contains("Would propose: 1"), "{text}");
        assert!(
            text.contains("document 00000000-0000-0000-0000-000000000001"),
            "{text}"
        );
    }
}
