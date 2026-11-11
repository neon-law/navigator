//! `navigator-deploy ops firms doctor` — the deployment-wide Admin-DRI invariant
//! report (ENG-499).
//!
//! Read-only: it connects to the `SurrealDB` the sourced `.devx/env` names —
//! the same store `web` reads — and prints which active Firms do not hold
//! exactly one eligible Admin DRI. It never appoints, clears, or otherwise
//! repairs a row; [`store::firms::appoint_admin_dri`] is the only writer, and
//! fixing a reported Firm means calling it (through the Owner surface) after
//! a human decides who the DRI should be.

use std::process::ExitCode;

use anyhow::{Context, Result};

use crate::palette;

/// `navigator-deploy ops firms doctor`.
pub fn run() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("could not start a runtime: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(diagnose()) {
        Ok(report) => print_report(&report),
        Err(e) => {
            eprintln!("could not read the Admin-DRI report: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn diagnose() -> Result<Vec<store::firms::AdminDriStatus>> {
    let surreal = store::surreal::connect_from_env()
        .await
        .context("connect to SurrealDB")?;
    store::firms::admin_dri_invariant_report(&surreal)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
}

fn print_report(report: &[store::firms::AdminDriStatus]) -> ExitCode {
    use store::firms::AdminDriProblem;

    if report.is_empty() {
        println!("{}", palette::dim("no active Firms"));
        return ExitCode::SUCCESS;
    }

    let mut healthy = true;
    for status in report {
        let (marker, detail) = match &status.problem {
            None => (palette::dim("ok  "), "one eligible Admin DRI".to_string()),
            Some(AdminDriProblem::Missing) => {
                healthy = false;
                (palette::highlight("FAIL"), "no Admin DRI".to_string())
            }
            Some(AdminDriProblem::Multiple(ids)) => {
                healthy = false;
                (
                    palette::highlight("FAIL"),
                    format!("{} people hold the designation: {ids:?}", ids.len()),
                )
            }
            Some(AdminDriProblem::Ineligible(person_id)) => {
                healthy = false;
                (
                    palette::highlight("FAIL"),
                    format!("{person_id} no longer carries an eligible admin membership"),
                )
            }
        };
        println!("{marker}  {:<24}  {detail}", status.firm_name);
    }

    println!();
    if healthy {
        println!(
            "{}",
            palette::dim("every active Firm holds exactly one eligible Admin DRI")
        );
        ExitCode::SUCCESS
    } else {
        println!(
            "{}",
            palette::highlight(
                "one or more Firms need a human to appoint or transfer their Admin DRI \
                 — this report never repairs one"
            )
        );
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::firms::{AdminDriProblem, AdminDriStatus};
    use uuid::Uuid;

    fn status(name: &str, problem: Option<AdminDriProblem>) -> AdminDriStatus {
        AdminDriStatus {
            firm_id: Uuid::now_v7(),
            firm_name: name.to_string(),
            problem,
        }
    }

    #[test]
    fn an_empty_report_is_a_success() {
        assert_eq!(print_report(&[]), ExitCode::SUCCESS);
    }

    #[test]
    fn every_firm_healthy_is_a_success() {
        let report = vec![status("Guieb & Shook", None)];
        assert_eq!(print_report(&report), ExitCode::SUCCESS);
    }

    #[test]
    fn a_missing_dri_fails() {
        let report = vec![status("Guieb & Shook", Some(AdminDriProblem::Missing))];
        assert_eq!(print_report(&report), ExitCode::FAILURE);
    }

    #[test]
    fn multiple_dris_fail() {
        let report = vec![status(
            "Guieb & Shook",
            Some(AdminDriProblem::Multiple(vec![
                Uuid::now_v7(),
                Uuid::now_v7(),
            ])),
        )];
        assert_eq!(print_report(&report), ExitCode::FAILURE);
    }

    #[test]
    fn an_ineligible_dri_fails() {
        let report = vec![status(
            "Guieb & Shook",
            Some(AdminDriProblem::Ineligible(Uuid::now_v7())),
        )];
        assert_eq!(print_report(&report), ExitCode::FAILURE);
    }

    #[test]
    fn one_bad_firm_fails_the_whole_report_even_beside_healthy_ones() {
        let report = vec![
            status("Healthy Firm", None),
            status("Guieb & Shook", Some(AdminDriProblem::Missing)),
            status("Another Healthy Firm", None),
        ];
        assert_eq!(print_report(&report), ExitCode::FAILURE);
    }
}
