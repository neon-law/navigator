//! Export safe completion receipts without reading workflow step output.

use std::collections::HashSet;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::controller::Forgejo;

const PAGE_SIZE: usize = 50;

#[derive(Deserialize)]
struct TaskPage {
    workflow_runs: Vec<Task>,
}

#[derive(Deserialize)]
struct Task {
    id: u64,
    run_number: u64,
    name: String,
    status: String,
    updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, PartialEq, Eq)]
struct Receipt {
    id: u64,
    run_number: u64,
    job: &'static str,
    outcome: &'static str,
}

impl Task {
    fn receipt(&self) -> Option<Receipt> {
        let outcome = match self.status.as_str() {
            "success" => "success",
            "failure" => "failure",
            "cancelled" => "cancelled",
            "skipped" => "skipped",
            _ => return None,
        };
        let job = match self.name.as_str() {
            "gate" => "gate",
            "documents" => "documents",
            "seeds" => "seeds",
            "notify" => "notify",
            _ => "other",
        };
        (self.id > 0 && self.run_number > 0).then_some(Receipt {
            id: self.id,
            run_number: self.run_number,
            job,
            outcome,
        })
    }
}

/// Keep only the terminal task IDs returned by the previous bounded poll.
pub(super) struct JobReceipts {
    started: DateTime<Utc>,
    seen: HashSet<u64>,
}

impl JobReceipts {
    pub(super) fn new(started: DateTime<Utc>) -> Self {
        Self {
            started,
            seen: HashSet::new(),
        }
    }

    fn refresh(&mut self, page: TaskPage) -> Vec<Receipt> {
        let mut current = HashSet::new();
        let receipts = page
            .workflow_runs
            .into_iter()
            .take(PAGE_SIZE)
            .filter(|task| task.updated_at.is_some_and(|time| time >= self.started))
            .filter_map(|task| task.receipt())
            .filter(|receipt| current.insert(receipt.id) && !self.seen.contains(&receipt.id))
            .collect();
        self.seen = current;
        receipts
    }

    pub(super) async fn observe(&mut self, forgejo: &Forgejo, repository: &str) -> Result<()> {
        let response = forgejo
            .get(&format!(
                "/repos/{repository}/actions/tasks?limit={PAGE_SIZE}"
            ))
            .await?
            .context("the selected repository's Actions tasks are unavailable")?;
        let page = serde_json::from_value(response).context("parse Forgejo Actions task list")?;
        for receipt in self.refresh(page) {
            tracing::info!(
                target: "forgejo", event = "forgejo.task.finished",
                forgejo.repository = repository, forgejo.run_number = receipt.run_number,
                forgejo.task_id = receipt.id, job = receipt.job, outcome = receipt.outcome,
                "Forgejo Actions job finished"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_new_completions_with_safe_fields_become_receipts() {
        let mut observer = JobReceipts::new("2026-01-01T12:00:00Z".parse().unwrap());
        let page = || {
            serde_json::from_value(json!({"workflow_runs": [
                {"id": 12, "run_number": 1, "name": "gate", "status": "failure",
                 "updated_at": "2026-01-01T12:02:00Z", "display_title": "Private example title"},
                {"id": 13, "run_number": 1, "name": "Private example job name", "status": "success",
                 "updated_at": "2026-01-01T12:03:00Z"},
                {"id": 14, "run_number": 1, "name": "notify", "status": "running",
                 "updated_at": "2026-01-01T12:03:00Z"},
                {"id": 11, "run_number": 1, "name": "seeds", "status": "success",
                 "updated_at": "2026-01-01T11:55:00Z"}
            ]}))
            .unwrap()
        };
        assert_eq!(
            observer.refresh(page()),
            [
                Receipt {
                    id: 12,
                    run_number: 1,
                    job: "gate",
                    outcome: "failure"
                },
                Receipt {
                    id: 13,
                    run_number: 1,
                    job: "other",
                    outcome: "success"
                },
            ]
        );
        assert!(observer.refresh(page()).is_empty());
    }
}
