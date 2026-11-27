//! Submit a batch of independently replayable proposals and revise a returned proposal.
use std::{path::PathBuf, process::ExitCode};

use anyhow::{ensure, Context, Result};
use clap::Subcommand;
use serde_json::{json, Value};
use uuid::Uuid;

use super::deadline;

#[derive(Subcommand)]
pub enum Action {
    /// Show the current Project review queue.
    List,
    /// Queue one submission or an array. Each submission has a stable request ID and a draft.
    Queue {
        #[arg(long)]
        file: PathBuf,
        /// Display trial preparation suggestions without queuing the proposal.
        #[arg(long)]
        preview: bool,
    },
    /// Replace the draft at the given revision. Use the browser to confirm a court date.
    Correct {
        id: Uuid,
        #[arg(long)]
        revision: i64,
        #[arg(long)]
        file: PathBuf,
    },
}

pub async fn run(host: Option<&str>, project: Option<&str>, action: Action) -> ExitCode {
    deadline::finish(execute(host, project, action).await)
}

fn read(file: PathBuf) -> Result<Value> {
    let data = std::fs::read(file).context("reading the court-date JSON file")?;
    serde_json::from_slice(&data).context("invalid court-date JSON file")
}

fn suggest_trial_prep(submission: &mut Value) -> Result<()> {
    use model::project_deadlines::{Category, Hearing};
    let draft = &mut submission["draft"];
    if draft["category"] == "trial_day" && draft["hearing"]["prep"].is_null() {
        let hearing: Hearing = serde_json::from_value(draft["hearing"].clone())?;
        let start = draft["due_at"]
            .as_str()
            .context("set the first trial start in due_at")?
            .parse()?;
        hearing
            .validate_appearance(Some(Category::TrialDay), start)
            .map_err(anyhow::Error::msg)?;
        draft["hearing"]["prep"] = json!(hearing.preparation(start));
    }
    Ok(())
}

async fn execute(host: Option<&str>, project: Option<&str>, action: Action) -> Result<Value> {
    if let Action::Queue {
        file,
        preview: true,
    } = action
    {
        let mut input = read(file)?;
        if let Some(rows) = input.as_array_mut() {
            for row in rows {
                suggest_trial_prep(row)?;
            }
        } else {
            suggest_trial_prep(&mut input)?;
        }
        return Ok(input);
    }
    let (base, token, project_id) = deadline::coordinates(host, project).await?;
    let url = format!("{base}/app/api/projects/{project_id}/court-date-proposals");
    let client = reqwest::Client::new();
    match action {
        Action::List => {
            deadline::call(client.get(&url).bearer_auth(&token), "list proposals").await
        }
        Action::Queue { file, .. } => {
            let submissions = match read(file)? {
                Value::Array(rows) => rows,
                row => vec![row],
            };
            ensure!(!submissions.is_empty(), "supply at least one proposal");
            let mut ids = std::collections::HashSet::new();
            for row in &submissions {
                let id = row["request_id"]
                    .as_str()
                    .and_then(|id| Uuid::parse_str(id).ok())
                    .context("each submission needs a stable UUID request_id")?;
                ensure!(ids.insert(id), "request_id appears twice in the batch");
                ensure!(row["draft"].is_object(), "each submission needs a draft");
            }
            let mut rows = Vec::with_capacity(submissions.len());
            for row in submissions {
                rows.push(
                    deadline::call(
                        client.post(&url).bearer_auth(&token).json(&row),
                        "queue proposal",
                    )
                    .await?,
                );
            }
            Ok(json!({"proposals":rows}))
        }
        Action::Correct { id, revision, file } => {
            let draft = read(file)?;
            deadline::call(
                client
                    .patch(format!("{url}/{id}"))
                    .bearer_auth(&token)
                    .json(&json!({"revision":revision,"draft":draft})),
                "revise proposal",
            )
            .await
        }
    }
}
