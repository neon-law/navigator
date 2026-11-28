//! The signed-in lawyer's private billing worklist.
use std::{fmt::Write, process::ExitCode, time::Duration};

use anyhow::{anyhow, Context, Result};
use model::billing_report::{BillingAction, BillingReport, ClaimBilling, ContingencyBilling};

use crate::{legal_plans, remote};

pub use crate::legal_plans::ClaimCmd as ProjectBillingCmd;

pub async fn personal(host: Option<&str>, json: bool) -> ExitCode {
    let result = async {
        let (base, token) = remote::resolve(host)?;
        read(&format!("{base}/app/api/billing"), &token, json).await
    }
    .await;
    finish("billing", result)
}

pub async fn project(
    host: Option<&str>,
    project: Option<&str>,
    json: bool,
    action: Option<ProjectBillingCmd>,
) -> ExitCode {
    let result = async {
        let (route, token, id) = legal_plans::project_route(host, project).await?;
        match action {
            None | Some(ProjectBillingCmd::Status) => {
                read(&format!("{route}/billing"), &token, json).await
            }
            Some(action) => {
                let result = legal_plans::claim_command(&route, &token, id, action).await?;
                Ok(serde_json::to_string_pretty(&result)?)
            }
        }
    }
    .await;
    finish("project billing", result)
}

async fn read(route: &str, token: &str, json: bool) -> Result<String> {
    let response = remote::authenticated_client_builder(route)?
        .build()?
        .get(route)
        .bearer_auth(token)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .context("request billing report")?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!("billing report refused ({status})"));
    }
    let report = response
        .json::<BillingReport>()
        .await
        .context("read billing report")?;
    if json {
        Ok(serde_json::to_string_pretty(&report)?)
    } else {
        Ok(render(&report)?)
    }
}

pub(crate) fn finish(command: &str, result: Result<String>) -> ExitCode {
    match result {
        Ok(text) => {
            println!("{}", text.trim_end());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator {command}: {error:#}");
            ExitCode::FAILURE
        }
    }
}

pub(crate) fn money(cents: Option<i64>) -> String {
    cents.map_or_else(
        || "unknown".into(),
        |cents| {
            let sign = if cents < 0 { "-" } else { "" };
            let amount = cents.unsigned_abs();
            format!("{sign}${}.{:02}", amount / 100, amount % 100)
        },
    )
}

fn render(report: &BillingReport) -> Result<String, std::fmt::Error> {
    let mut text = String::new();
    write_actions(&mut text, &report.needs_my_action)?;
    let (heading, time) = report
        .project_time
        .as_ref()
        .map_or(("My time", &report.my_time), |time| ("Project time", time));
    writeln!(text, "\n{heading}")?;
    if time.is_empty() {
        writeln!(text, "  No Time Entries or pending Billing Annotations")?;
    }
    for time in time {
        write!(
            text,
            "  {} · {} · {} min · {} · {} · {}",
            time.project, time.performed_on, time.minutes, time.status, time.id, time.description
        )?;
        if time.backfilled {
            write!(text, " · backfilled")?;
        }
        if let Some(timekeeper) = time.timekeeper {
            write!(text, " · timekeeper {timekeeper}")?;
        }
        writeln!(text)?;
    }
    writeln!(text, "\nVerified projected fees for completed work")?;
    let projected = report
        .verified_projected_fees
        .iter()
        .map(|fee| fee.projected_fee_cents)
        .sum();
    if projected > 0 {
        writeln!(text, "  Total remaining {}", money(Some(projected)))?;
    }
    if report.verified_projected_fees.is_empty() {
        writeln!(text, "  None")?;
    }
    for fee in &report.verified_projected_fees {
        writeln!(
            text,
            "  {} · {} · {}",
            fee.project,
            money(Some(fee.projected_fee_cents)),
            fee.basis
        )?;
    }
    if !report.unverified_fees.is_empty() {
        writeln!(text, "\nFees awaiting verification")?;
        for fee in &report.unverified_fees {
            writeln!(text, "  {} · {}", fee.project, fee.basis)?;
        }
    }
    if !report.contingency.is_empty() {
        writeln!(text, "\nContingency fees")?;
        for matter in &report.contingency {
            write_contingency(&mut text, matter)?;
        }
    }
    if !report.other_claims.is_empty() {
        writeln!(text, "\nOther Legal Plan Claims")?;
        for claim in &report.other_claims {
            writeln!(
                text,
                "  {} · {} · {}",
                claim.project, claim.benefit, claim.id
            )?;
            write_claim(&mut text, claim)?;
        }
    }
    Ok(text)
}

fn write_actions(text: &mut String, actions: &[BillingAction]) -> std::fmt::Result {
    writeln!(text, "Needs my action")?;
    if actions.is_empty() {
        writeln!(text, "  None")?;
    }
    for action in actions {
        match action {
            BillingAction::Claim(claim) => {
                if let Some(action) = &claim.next_action {
                    writeln!(
                        text,
                        "  {} · {} · {} · {action}",
                        claim.project, claim.benefit, claim.id
                    )?;
                }
                write_claim(text, claim)?;
            }
            BillingAction::PendingAnnotation {
                project,
                change_id,
                performed_on,
                next_action,
                ..
            } => {
                writeln!(
                    text,
                    "  {project} · {performed_on} · {change_id} · {next_action}"
                )?;
            }
            BillingAction::PendingProposals {
                project,
                next_action,
                ..
            }
            | BillingAction::UnallocatedWork {
                project,
                next_action,
            } => {
                writeln!(text, "  {project} · {next_action}")?;
            }
        }
    }
    Ok(())
}

fn write_claim(text: &mut String, claim: &ClaimBilling) -> std::fmt::Result {
    writeln!(
        text,
        "    {} · {} · Projected fee {} · Filed {} · Approved outstanding {} · Net cash received {}",
        claim.performed_on,
        claim.status,
        money(claim.projected_fee_cents),
        money(claim.filed_cents.map(i64::from)),
        money(claim.approved_outstanding_cents),
        money(Some(claim.net_cash_received_cents))
    )
}

fn percent(basis_points: i32) -> String {
    format!("{}.{:02}%", basis_points / 100, basis_points % 100)
}

fn write_contingency(text: &mut String, matter: &ContingencyBilling) -> std::fmt::Result {
    let cost_timing = match matter.costs_deducted.as_str() {
        "before_fee" => "before calculating the fee",
        "after_fee" => "after calculating the fee",
        other => other,
    };
    writeln!(
        text,
        "  {} · agreement {} effective {} · costs deducted {} · recovery basis {}",
        matter.project,
        matter.fee_agreement_notation_id,
        matter.effective_on,
        cost_timing,
        matter.recovery_basis,
    )?;
    for tier in &matter.tiers {
        writeln!(
            text,
            "    From {} · fee {}",
            tier.stage,
            percent(tier.fee_bps)
        )?;
    }
    if let Some(rate) = matter.hybrid_rate_cents_per_hour {
        writeln!(
            text,
            "    Hybrid hourly rate {}",
            money(Some(i64::from(rate)))
        )?;
    }
    if let Some(on) = matter.estimate_recorded_on {
        let stale = if matter.stale_estimate {
            " · newer Case Stage"
        } else {
            ""
        };
        writeln!(text, "    Outcome Estimate {on}{stale}")?;
    }
    if matter.outcomes.is_empty() {
        writeln!(text, "    No recovery scenarios recorded")?;
    }
    for scenario in &matter.outcomes {
        let outcome = match scenario.outcome.as_str() {
            "win_at_trial" => "Win at trial",
            "settle" => "Settlement",
            "lose" => "Loss",
            other => other,
        };
        if matter.fee_received_cents.is_some() {
            if scenario.probability_bps > 0 {
                writeln!(
                    text,
                    "    Recorded outcome · {outcome} · gross recovery {}",
                    money(scenario.gross_recovery_cents)
                )?;
            }
            continue;
        }
        writeln!(
            text,
            "    {outcome} · {} likelihood · gross recovery {} · fee {} ({})",
            percent(scenario.probability_bps),
            money(scenario.gross_recovery_cents),
            percent(scenario.fee_bps),
            money(scenario.fee_cents)
        )?;
    }
    if matter.fee_received_cents.is_some() {
        writeln!(
            text,
            "    Fee received {} · Costs recovered {}",
            money(matter.fee_received_cents),
            money(matter.costs_recovered_cents)
        )
    } else {
        writeln!(
            text,
            "    Projected contingency fee {} · Expected cash including cost recovery {}",
            money(matter.projected_fee_cents),
            money(matter.expected_cash_cents)
        )
    }
}
