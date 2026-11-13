//! `hudson ops forgejo logs <owner>/<repository> --run <n>`: print every
//! job log of one Actions run (ENG-1238).
//!
//! Forgejo 15.0.9 serves job logs only through a web route that refuses token
//! authentication. A finished job's log is a zstd file on the Forgejo
//! volume, `actions_log/<owner>/<repository>/<task % 256, hex>/<task>.log.zst`,
//! so this verb lists the run's tasks through the API and reads each file
//! through the Forgejo pod, read-only.

use std::process::Stdio;

use anyhow::{bail, ensure, Context, Result};
use serde_json::Value;

use super::cluster::Kube;
use super::controller::Forgejo;

const LOG_ROOT: &str = "/var/lib/gitea/data/actions_log";

/// The log file of task `task` in `repository`, without its compression
/// suffix.
fn log_path(repository: &str, task: u64) -> String {
    format!("{LOG_ROOT}/{repository}/{:02x}/{task}.log", task % 256)
}

/// The tasks (one per job attempt) of run `number`, oldest first.
fn tasks_of_run(tasks: &[Value], number: u64) -> Vec<(u64, String)> {
    let mut found: Vec<(u64, String)> = tasks
        .iter()
        .filter(|task| task["run_number"].as_u64() == Some(number))
        .filter_map(|task| {
            Some((
                task["id"].as_u64()?,
                task["name"].as_str().unwrap_or("job").to_owned(),
            ))
        })
        .collect();
    found.sort_unstable();
    found
}

/// A stored log's bytes as text: zstd-compressed once the job finished, plain
/// while Forgejo still holds it uncompressed.
fn decode(bytes: &[u8]) -> Result<String> {
    let plain = if bytes.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        zstd::stream::decode_all(bytes).context("decompress the job log")?
    } else {
        bytes.to_vec()
    };
    Ok(String::from_utf8_lossy(&plain).into_owned())
}

fn read_file(kube: &Kube, pod: &str, path: &str) -> Result<Option<Vec<u8>>> {
    let output = kube
        .command()
        .args(["exec", pod, "-c", "forgejo", "--", "cat", path])
        .stdin(Stdio::null())
        .output()
        .context("spawn kubectl exec")?;
    Ok(output.status.success().then_some(output.stdout))
}

/// The repository's tasks, newest first, paged until run `number` is passed.
async fn listed_tasks(forgejo: &Forgejo, repository: &str, number: u64) -> Result<Vec<Value>> {
    let mut tasks = Vec::new();
    for page in 1..=10 {
        let listed = forgejo
            .get(&format!(
                "/repos/{repository}/actions/tasks?limit=50&page={page}"
            ))
            .await?
            .unwrap_or(Value::Null);
        let batch = listed["workflow_runs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let done = batch.len() < 50
            || batch
                .iter()
                .any(|task| task["run_number"].as_u64() < Some(number));
        tasks.extend(batch);
        if done {
            break;
        }
    }
    Ok(tasks)
}

pub(in crate::devx) fn run(
    repository: &str,
    number: u64,
    url: &str,
    context: &str,
    namespace: &str,
) -> Result<()> {
    super::runner::repository_scope(repository)?;
    ensure!(!context.trim().is_empty(), "Kubernetes context is required");
    let token = std::env::var("FORGEJO_TOKEN")
        .context("FORGEJO_TOKEN must be configured on the trusted operator host")?;
    let forgejo = Forgejo::new(url, token)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let tasks = runtime.block_on(listed_tasks(&forgejo, repository, number))?;
    let found = tasks_of_run(&tasks, number);
    if found.is_empty() {
        bail!("run {number} of {repository} has no started job; a queued job has no log yet");
    }
    let kube = Kube::selected(context, namespace);
    for (task, name) in found {
        println!("== {name} (task {task}) ==");
        let path = log_path(repository, task);
        let bytes = match read_file(&kube, "navigator-forgejo-0", &format!("{path}.zst"))? {
            Some(bytes) => bytes,
            None => read_file(&kube, "navigator-forgejo-0", &path)?.with_context(|| {
                format!("task {task}'s log is not on the Forgejo volume yet; a running job's log is still in the database")
            })?,
        };
        print!("{}", decode(&bytes)?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_task_log_lives_under_its_hex_bucket() {
        assert_eq!(
            log_path("example-firm/sample-estate", 300),
            "/var/lib/gitea/data/actions_log/example-firm/sample-estate/2c/300.log"
        );
        assert_eq!(
            log_path("o/r", 7),
            "/var/lib/gitea/data/actions_log/o/r/07/7.log"
        );
    }

    #[test]
    fn only_the_named_runs_tasks_are_printed_in_order() {
        let tasks = vec![
            json!({"id": 12, "name": "seeds", "run_number": 4}),
            json!({"id": 11, "name": "documents", "run_number": 4}),
            json!({"id": 10, "name": "gate", "run_number": 3}),
        ];
        assert_eq!(
            tasks_of_run(&tasks, 4),
            vec![(11, "documents".to_owned()), (12, "seeds".to_owned())]
        );
    }

    #[test]
    fn compressed_and_plain_logs_both_decode() {
        let text = "2026-11-05T10:00:00Z navigator project gate --ci\n";
        let compressed = zstd::stream::encode_all(text.as_bytes(), 3).unwrap();
        assert_eq!(decode(&compressed).unwrap(), text);
        assert_eq!(decode(text.as_bytes()).unwrap(), text);
    }

    #[tokio::test]
    async fn task_pages_are_read_until_the_run_is_passed() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let page = |run: u64| {
            json!({"workflow_runs": (0..50)
                .map(|id| json!({"id": run * 100 + id, "name": "gate", "run_number": run}))
                .collect::<Vec<_>>()})
        };
        for (number, body) in [("1", page(9)), ("2", page(4))] {
            Mock::given(method("GET"))
                .and(path("/api/v1/repos/o/a/actions/tasks"))
                .and(query_param("page", number))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .expect(1)
                .mount(&server)
                .await;
        }
        let forgejo = Forgejo::new(&server.uri(), "operator".into()).unwrap();
        let tasks = listed_tasks(&forgejo, "o/a", 5).await.unwrap();
        assert_eq!(tasks.len(), 100);
        assert!(tasks_of_run(&tasks, 5).is_empty());
        assert_eq!(tasks_of_run(&tasks, 4).len(), 50);
    }
}
