//! Payload-free, point-in-time preservation inventory for a Restate environment.
//! Counts are evidence of work to reconcile; they do not back up journals or
//! authorize stopping an engine while writers can admit new work.

use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const INVOCATIONS: &str = "SELECT status, completion_result, COUNT(*) AS invocation_count FROM sys_invocation GROUP BY status, completion_result LIMIT 32";
const STATE: &str = "SELECT COUNT(*) AS state_count FROM state";
const JOURNALS: &str = "SELECT COUNT(*) AS journal_count FROM sys_journal";

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Status {
    Pending,
    Scheduled,
    Ready,
    Running,
    Paused,
    BackingOff,
    Suspended,
    Completed,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Completion {
    Success,
    Failure,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InvocationGroup {
    status: Status,
    completion_result: Option<Completion>,
    invocation_count: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StateCount {
    state_count: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalCount {
    journal_count: u64,
}

#[derive(Debug, Serialize)]
struct Audit {
    observed_at: String,
    invocations: Vec<InvocationGroup>,
    retained_invocations: u64,
    nonterminal_invocations: u64,
    completed_failures: u64,
    state_entries: u64,
    journal_entries: u64,
    preservation_required: bool,
    limitation: &'static str,
}

fn parse<T: serde::de::DeserializeOwned>(output: &str) -> Result<T> {
    // Schema failures must not echo untrusted input, which could contain
    // privileged values if a mismatched query or executable is installed.
    serde_json::from_str(output).map_err(|_| anyhow::anyhow!("unexpected Restate aggregate schema"))
}

fn sum(total: &mut u64, count: u64) -> Result<()> {
    *total = total
        .checked_add(count)
        .context("Restate aggregate count overflow")?;
    Ok(())
}

fn audit(mut query: impl FnMut(&str) -> Result<String>) -> Result<Audit> {
    let invocations: Vec<InvocationGroup> = parse(&query(INVOCATIONS)?)?;
    if invocations.len() >= 32 {
        bail!("Restate aggregate may be truncated");
    }
    let mut retained_invocations = 0;
    let mut nonterminal_invocations = 0;
    let mut completed_failures = 0;
    let mut seen = std::collections::BTreeSet::new();
    for row in &invocations {
        let identity = serde_json::to_string(&(&row.status, &row.completion_result))?;
        if !seen.insert(identity) || row.invocation_count == 0 {
            bail!("invalid Restate aggregate row");
        }
        sum(&mut retained_invocations, row.invocation_count)?;
        match (&row.status, &row.completion_result) {
            (Status::Completed, Some(Completion::Success)) => {}
            (Status::Completed, Some(Completion::Failure)) => {
                sum(&mut completed_failures, row.invocation_count)?;
            }
            (Status::Completed, None) => bail!("completed Restate aggregate has no outcome"),
            (_, None) => sum(&mut nonterminal_invocations, row.invocation_count)?,
            (_, Some(_)) => bail!("nonterminal Restate aggregate has a completion outcome"),
        }
    }
    let state: Vec<StateCount> = parse(&query(STATE)?)?;
    let journal: Vec<JournalCount> = parse(&query(JOURNALS)?)?;
    let ([state], [journal]) = (state.as_slice(), journal.as_slice()) else {
        bail!("Restate aggregate count query must return exactly one row");
    };
    Ok(Audit {
        observed_at: chrono::Utc::now().to_rfc3339(),
        preservation_required: retained_invocations > 0 || state.state_count > 0 || journal.journal_count > 0,
        invocations,
        retained_invocations,
        nonterminal_invocations,
        completed_failures,
        state_entries: state.state_count,
        journal_entries: journal.journal_count,
        limitation: "Point-in-time counts only; not a consistent backup or authorization to stop Restate. Reconcile queues, timers, promises, locks, retained results and external effects before cutover.",
    })
}

fn query_with_runner(
    environment: &str,
    sql: &str,
    runner: impl FnOnce(&[&str]) -> std::io::Result<std::process::Output>,
) -> Result<String> {
    let output = runner(&[
        "--environment",
        environment,
        "--json",
        "--connect-timeout",
        "3000",
        "--request-timeout",
        "13000",
        "sql",
        sql,
    ])
    .context("could not start read-only Restate aggregate query")?;
    if !output.status.success() {
        bail!("Restate aggregate query failed; inspect credentials and connectivity privately");
    }
    String::from_utf8(output.stdout).context("Restate aggregate output was not UTF-8")
}

fn query(environment: &str, sql: &str) -> Result<String> {
    query_with_runner(environment, sql, |args| {
        Command::new("restate").args(args).output()
    })
}

pub(super) fn run(environment: &str) -> Result<()> {
    if environment.trim().is_empty() || environment.starts_with('-') {
        bail!("an explicit Restate environment is required");
    }
    let audit = audit(|sql| query(environment, sql))?;
    println!("{}", serde_json::to_string_pretty(&audit)?);
    if audit.preservation_required {
        bail!("Restate work or retained state still requires preservation and reconciliation");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inventory(invocations: &str, state: &str, journal: &str) -> Result<Audit> {
        audit(|sql| {
            Ok(match sql {
                INVOCATIONS => invocations,
                STATE => state,
                JOURNALS => journal,
                _ => panic!("unexpected query"),
            }
            .into())
        })
    }

    fn with_invocations(invocations: &str) -> Result<Audit> {
        inventory(
            invocations,
            r#"[{"state_count":0}]"#,
            r#"[{"journal_count":0}]"#,
        )
    }

    #[test]
    fn every_nonterminal_status_preserves_work() {
        for status in [
            "pending",
            "scheduled",
            "ready",
            "running",
            "paused",
            "backing-off",
            "suspended",
        ] {
            let audit = with_invocations(&format!(
                r#"[{{"status":"{status}","completion_result":null,"invocation_count":2}}]"#
            ))
            .unwrap();
            assert_eq!(audit.nonterminal_invocations, 2);
            assert!(audit.preservation_required);
        }
    }

    #[test]
    fn completed_failures_require_explicit_reconciliation() {
        let audit = with_invocations(
            r#"[{"status":"completed","completion_result":"failure","invocation_count":3}]"#,
        )
        .unwrap();
        assert_eq!(audit.completed_failures, 3);
        assert!(audit.preservation_required);
    }

    #[test]
    fn completed_success_does_not_dispose_of_object_state_or_journals() {
        for (state, journal) in [(1, 0), (0, 1)] {
            let audit = inventory(
                r#"[{"status":"completed","completion_result":"success","invocation_count":8}]"#,
                &format!(r#"[{{"state_count":{state}}}]"#),
                &format!(r#"[{{"journal_count":{journal}}}]"#),
            )
            .unwrap();
            assert_eq!(audit.nonterminal_invocations, 0);
            assert!(audit.preservation_required);
        }
    }

    #[test]
    fn successful_completions_still_retain_outcomes_and_idempotency_evidence() {
        let audit = with_invocations(
            r#"[{"status":"completed","completion_result":"success","invocation_count":8}]"#,
        )
        .unwrap();
        assert!(audit.preservation_required);
        assert_eq!(audit.retained_invocations, 8);
    }

    #[test]
    fn empty_environment_is_observation_only() {
        let audit = with_invocations("[]").unwrap();
        assert!(!audit.preservation_required);
        assert!(audit.limitation.contains("not a consistent backup"));
    }

    #[test]
    fn unknown_or_inconsistent_outcomes_fail_closed_without_echoing_input() {
        for input in [
            r#"[{"status":"private-value","completion_result":null,"invocation_count":1}]"#,
            r#"[{"status":"completed","completion_result":null,"invocation_count":1}]"#,
            r#"[{"status":"running","completion_result":"success","invocation_count":1}]"#,
            r#"[{"status":"completed","completion_result":"success","invocation_count":0}]"#,
            r#"[{"status":"completed","completion_result":"success","invocation_count":1,"payload":"private-value"}]"#,
        ] {
            let error = with_invocations(input).unwrap_err().to_string();
            assert!(!error.contains("private-value"));
        }
    }

    #[test]
    fn missing_or_duplicate_count_rows_and_query_failures_fail_closed() {
        assert!(inventory("[]", "[]", r#"[{"journal_count":0}]"#).is_err());
        assert!(inventory("[]", r#"[{"state_count":0}]"#, "[]").is_err());
        assert!(inventory(
            "[]",
            r#"[{"state_count":0},{"state_count":0}]"#,
            r#"[{"journal_count":0}]"#
        )
        .is_err());
        assert!(audit(|_| bail!("synthetic query failure")).is_err());
        assert!(with_invocations(r#"[{"status":"pending","completion_result":null,"invocation_count":1},{"status":"pending","completion_result":null,"invocation_count":1}]"#).is_err());
    }

    #[test]
    fn ambiguous_truncated_output_and_implicit_environments_are_refused() {
        let row = r#"{"status":"completed","completion_result":"success","invocation_count":1}"#;
        let input = format!("[{}]", [row; 32].join(","));
        assert!(with_invocations(&input)
            .unwrap_err()
            .to_string()
            .contains("truncated"));
        assert!(run(" ").is_err());
        assert!(run("--unknown").is_err());
    }

    #[test]
    fn queries_are_aggregate_only_and_do_not_select_privileged_columns() {
        for query in [INVOCATIONS, STATE, JOURNALS] {
            assert!(query.starts_with("SELECT ") && query.contains("COUNT(*)"));
            for column in [
                "payload",
                "service_key",
                "value_utf8",
                "entry_json",
                "last_failure",
            ] {
                assert!(
                    !query.contains(column),
                    "privileged column in aggregate query"
                );
            }
        }
    }

    fn process_output(status: i32, stdout: Vec<u8>, stderr: Vec<u8>) -> std::process::Output {
        use std::os::unix::process::ExitStatusExt;
        std::process::Output {
            status: std::process::ExitStatus::from_raw(status),
            stdout,
            stderr,
        }
    }

    #[test]
    fn query_explicitly_scopes_and_bounds_the_read_only_child_process() {
        let result = query_with_runner("selected-environment", INVOCATIONS, |args| {
            assert_eq!(
                args,
                [
                    "--environment",
                    "selected-environment",
                    "--json",
                    "--connect-timeout",
                    "3000",
                    "--request-timeout",
                    "13000",
                    "sql",
                    INVOCATIONS
                ]
            );
            Ok(process_output(0, b"[]".to_vec(), Vec::new()))
        })
        .unwrap();
        assert_eq!(result, "[]");
    }

    #[test]
    fn subprocess_errors_never_echo_private_diagnostics_or_partial_results() {
        let error = query_with_runner("selected-environment", INVOCATIONS, |_| {
            Ok(process_output(
                256,
                b"private-partial-result".to_vec(),
                b"private-credential-diagnostic".to_vec(),
            ))
        })
        .unwrap_err()
        .to_string();
        assert!(!error.contains("private-"));
        assert!(query_with_runner("selected-environment", INVOCATIONS, |_| {
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        })
        .is_err());
        assert!(query_with_runner("selected-environment", INVOCATIONS, |_| {
            Ok(process_output(0, vec![0xff], Vec::new()))
        })
        .is_err());
    }

    #[test]
    fn count_overflow_fails_closed() {
        let input = format!(
            r#"[{{"status":"running","completion_result":null,"invocation_count":{}}},{{"status":"pending","completion_result":null,"invocation_count":1}}]"#,
            u64::MAX
        );
        assert!(with_invocations(&input).is_err());
    }
}
