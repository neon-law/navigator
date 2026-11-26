//! Private operator file and authenticated CLI transport for retained objects.

use std::process::Command;

use anyhow::{bail, Result};
use workflows::attest::sha256_hex;
use workflows::object_handover::{self, Readback};

fn restate_sql(environment: &str, sql: &str) -> Command {
    let mut command = Command::new("restate");
    command.args([
        "--environment",
        environment,
        "--json",
        "--connect-timeout",
        "3000",
        "--request-timeout",
        "13000",
        "sql",
        sql,
    ]);
    command
}

fn bounded_stdout(mut command: Command) -> Result<String> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| anyhow::anyhow!("object capture transport unavailable"))?;
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("object capture transport unavailable"))?
        .take(128 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes);
    if read.is_err() || bytes.len() > 128 * 1024 * 1024 {
        let _ = child.kill();
        let _ = child.wait();
        bail!("object capture transport refused or exceeded its bound");
    }
    if !child
        .wait()
        .map_err(|_| anyhow::anyhow!("object capture transport unavailable"))?
        .success()
    {
        bail!("object capture transport refused");
    }
    String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("unsupported object capture response"))
}

fn frozen_readback(environment: &str) -> Result<Readback> {
    object_handover::check_environment(environment)?;
    Ok(object_handover::frozen_readback_with(|sql| {
        bounded_stdout(restate_sql(environment, sql))
            .map_err(|_| object_handover::CaptureError("object capture transport refused"))
    })?)
}

/// Retained object operations are explicit and never run during ordinary boot.
#[derive(clap::Subcommand)]
pub enum Operation {
    /// Capture two matching authenticated source readbacks into a private file.
    Capture {
        #[arg(long)]
        environment: String,
        #[arg(long)]
        output: std::path::PathBuf,
        /// Acknowledge the independently maintained old-writer hold.
        #[arg(long)]
        operator_writers_held: bool,
    },
    /// Check offline capture parity; --apply atomically imports without activation.
    Import {
        #[arg(long)]
        environment: String,
        #[arg(long)]
        capture: std::path::PathBuf,
        #[arg(long)]
        sha256: String,
        #[arg(long)]
        apply: bool,
        /// Maintain the independent old-writer hold while applying copied state.
        #[arg(long)]
        operator_writers_held: bool,
    },
    /// Verify fresh source readback and complete target inventory before activation.
    Activate {
        #[arg(long)]
        environment: String,
        #[arg(long)]
        capture: std::path::PathBuf,
        #[arg(long)]
        sha256: String,
        /// Fresh acknowledgement, separate from all deserialized evidence.
        #[arg(long)]
        operator_writers_held: bool,
    },
    /// Activate DBOS ownership with historical source objects left behind.
    ActivateWithoutCapture {
        #[arg(long)]
        environment: String,
        /// Confirm legacy writers are stopped and remain stopped through the launch.
        #[arg(long)]
        operator_writers_held: bool,
        /// Confirm retained object state and replay evidence will not transfer.
        #[arg(long)]
        acknowledge_discard: bool,
    },
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PrivateCapture {
    source: Readback,
    capture: store::durable_object_handover::Capture,
}

fn now() -> Result<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| anyhow::anyhow!("object capture clock unavailable"))
}

fn require_fresh(captured_at: u64, current: u64) -> Result<()> {
    if current.checked_sub(captured_at).is_none_or(|age| age > 900) {
        bail!("object capture evidence expired or has an invalid time");
    }
    Ok(())
}

fn require_hold(held: bool) -> Result<()> {
    if !held {
        bail!(
            "explicit operator-writers-held acknowledgement is required; maintain the independent writer hold through activation"
        );
    }
    Ok(())
}

fn private_parent(path: &std::path::Path) -> Result<std::path::PathBuf> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let parent = parent
        .canonicalize()
        .map_err(|_| anyhow::anyhow!("private capture directory unavailable"))?;
    if parent
        .ancestors()
        .any(|ancestor| ancestor.join(".git").exists())
    {
        bail!("object captures cannot be stored in a repository");
    }
    Ok(parent)
}

fn write_private(path: &std::path::Path, capture: &PrivateCapture) -> Result<String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let parent = private_parent(path)?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("private capture filename required"))?;
    let bytes = serde_json::to_vec(capture)
        .map_err(|_| anyhow::anyhow!("object capture encoding failed"))?;
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(parent.join(name))
        .map_err(|_| anyhow::anyhow!("private capture destination unavailable"))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| anyhow::anyhow!("private capture write failed"))?;
    Ok(sha256_hex(&bytes))
}

fn read_private(
    path: &std::path::Path,
    expected: &str,
    environment: &str,
) -> Result<PrivateCapture> {
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    private_parent(path)?;
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| anyhow::anyhow!("private capture unavailable"))?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        bail!("object capture requires a private regular file");
    }
    let file =
        std::fs::File::open(path).map_err(|_| anyhow::anyhow!("private capture unavailable"))?;
    let mut bytes = Vec::new();
    file.take(128 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("private capture read failed"))?;
    if bytes.len() > 128 * 1024 * 1024 || sha256_hex(&bytes) != expected {
        bail!("object capture digest mismatch or bound exceeded");
    }
    let captured: PrivateCapture = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("unsupported private object capture"))?;
    if captured.capture.source_environment != environment {
        bail!("object source environment mismatch");
    }
    let inventory = captured
        .capture
        .slack_projects
        .iter()
        .map(|row| row.project_id)
        .collect();
    let mapped = object_handover::map_capture(
        &captured.source,
        environment,
        captured.capture.capture_id,
        captured.capture.ownership_epoch,
        &inventory,
        captured.capture.captured_at_unix,
    )?;
    if mapped != captured.capture {
        bail!("object capture evidence and mapped rows differ");
    }
    Ok(captured)
}

async fn inventory(
    postgres: &store::postgres::PostgresDb,
) -> Result<std::collections::BTreeSet<uuid::Uuid>> {
    Ok(sqlx::query_scalar("SELECT id FROM project ORDER BY id")
        .fetch_all(postgres.pool())
        .await
        .map_err(|_| anyhow::anyhow!("object target inventory unavailable"))?
        .into_iter()
        .collect())
}

pub(super) fn run(operation: Operation) -> Result<()> {
    // Check acknowledgements before constructing any database or source connection.
    match &operation {
        Operation::Capture {
            operator_writers_held,
            ..
        }
        | Operation::Activate {
            operator_writers_held,
            ..
        }
        | Operation::Import {
            apply: true,
            operator_writers_held,
            ..
        } => require_hold(*operator_writers_held)?,
        Operation::ActivateWithoutCapture {
            operator_writers_held,
            acknowledge_discard,
            ..
        } => {
            require_hold(*operator_writers_held)?;
            if !acknowledge_discard {
                bail!("explicit acknowledge-discard is required for a cutover without capture");
            }
        }
        Operation::Import { .. } => {}
    }
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|_| anyhow::anyhow!("object operator runtime unavailable"))?;
    runtime.block_on(async move {
        match operation {
            Operation::Capture {environment,output,..}=>{
                object_handover::check_environment(&environment)?;
                private_parent(&output)?;
                let postgres=store::postgres::PostgresDb::application().await.map_err(|_|anyhow::anyhow!("admitted PostgreSQL unavailable"))?;
                let projects=inventory(&postgres).await?;
                let source=frozen_readback(&environment)?;
                let capture=object_handover::map_capture(&source,&environment,uuid::Uuid::new_v4(),uuid::Uuid::new_v4(),&projects,now()?)?;
                let file_digest=write_private(&output,&PrivateCapture {source,capture})?;
                println!("{}",serde_json::json!({"format_version":1,"private_capture_sha256":file_digest,"native_activation":false,"limitation":"Matching authenticated readbacks do not prove a writer freeze or a consistent archive. Maintain the independent writer hold."}));
            },
            Operation::Import {environment,capture,sha256,apply,..}=>{
                object_handover::check_environment(&environment)?;
                let captured=read_private(&capture,&sha256,&environment)?;
                if apply { require_fresh(captured.capture.captured_at_unix,now()?)?; }
                let postgres=store::postgres::PostgresDb::application().await.map_err(|_|anyhow::anyhow!("admitted PostgreSQL unavailable"))?;
                let receipt=store::durable_object_handover::import(&postgres,&captured.capture,apply).await?;
                println!("{}",serde_json::to_string(&receipt)?);
            },
            Operation::Activate {environment,capture,sha256,..}=>{
                object_handover::check_environment(&environment)?;
                let captured=read_private(&capture,&sha256,&environment)?;
                require_fresh(captured.capture.captured_at_unix,now()?)?;
                let postgres=store::postgres::PostgresDb::application().await.map_err(|_|anyhow::anyhow!("admitted PostgreSQL unavailable"))?;
                store::durable_object_handover::activate(&postgres,&captured.capture,||async {
                    // Deliberately re-read the actual authenticated source while the
                    // complete-inventory creation fence and native lanes are held.
                    let source=frozen_readback(&environment).map_err(|_|store::durable_object_handover::HandoverError::InvalidCapture)?;
                    if source!=captured.source { return Err(store::durable_object_handover::HandoverError::Conflict); }
                    require_fresh(captured.capture.captured_at_unix,now().map_err(|_|store::durable_object_handover::HandoverError::InvalidCapture)?)
                        .map_err(|_|store::durable_object_handover::HandoverError::InvalidCapture)?;
                    Ok(())
                }).await?;
                println!("{}",serde_json::json!({"format_version":1,"capture_id":captured.capture.capture_id,"native_activation":true,"limitation":"Object parity only. Independent writer hold, paused-work reconciliation and recovery acceptance remain required."}));
            },
            Operation::ActivateWithoutCapture { environment, .. } => {
                object_handover::check_environment(&environment)?;
                super::restate_handover::require_drained(&environment)?;
                let postgres = store::postgres::PostgresDb::application().await
                    .map_err(|_| anyhow::anyhow!("admitted PostgreSQL unavailable"))?;
                let projects = store::durable_object_handover::activate_without_capture(&postgres, &environment).await?;
                println!("{}", serde_json::json!({
                    "origin": "source_discarded",
                    "native_activation": true,
                    "historical_projects": projects,
                }));
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_writer_hold_and_expired_evidence_refuse_before_connections() {
        assert!(require_hold(false).is_err());
        assert!(require_fresh(1, 902).is_err());
        assert!(require_fresh(100, 99).is_err());
        assert!(require_fresh(1, 901).is_ok());
        let result = run(Operation::Activate {
            environment: "synthetic".to_owned(),
            capture: "unread-private-capture".into(),
            sha256: "unused".to_owned(),
            operator_writers_held: false,
        });
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("operator-writers-held"));
    }

    #[test]
    fn no_capture_requires_explicit_discard_before_connections() {
        let error = run(Operation::ActivateWithoutCapture {
            environment: "synthetic".into(),
            operator_writers_held: true,
            acknowledge_discard: false,
        })
        .unwrap_err();
        assert!(error.to_string().contains("acknowledge-discard"));
    }

    fn refusal(operation: Operation) -> String {
        run(operation).expect_err("refused").to_string()
    }

    #[test]
    fn held_operations_refuse_bad_sources_and_captures_before_any_connection() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing.json");
        assert!(run(Operation::Capture {
            environment: "--untrusted-option".to_owned(),
            output: missing.clone(),
            operator_writers_held: true,
        })
        .is_err());
        assert_eq!(
            refusal(Operation::Capture {
                environment: "synthetic".to_owned(),
                output: std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("capture.json"),
                operator_writers_held: true,
            }),
            "object captures cannot be stored in a repository"
        );
        assert_eq!(
            refusal(Operation::Import {
                environment: "synthetic".to_owned(),
                capture: missing.clone(),
                sha256: "unused".to_owned(),
                apply: false,
                operator_writers_held: false,
            }),
            "private capture unavailable"
        );
        assert!(refusal(Operation::Import {
            environment: "synthetic".to_owned(),
            capture: missing.clone(),
            sha256: "unused".to_owned(),
            apply: true,
            operator_writers_held: false,
        })
        .contains("operator-writers-held"));
        assert_eq!(
            refusal(Operation::Activate {
                environment: "synthetic".to_owned(),
                capture: missing,
                sha256: "unused".to_owned(),
                operator_writers_held: true,
            }),
            "private capture unavailable"
        );
    }

    #[test]
    fn source_queries_run_the_authenticated_cli_with_bounded_timeouts() {
        let command = restate_sql("synthetic", "SELECT 1");
        assert_eq!(command.get_program(), "restate");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "--environment",
                "synthetic",
                "--json",
                "--connect-timeout",
                "3000",
                "--request-timeout",
                "13000",
                "sql",
                "SELECT 1"
            ]
        );
    }

    #[test]
    fn transport_returns_stdout_and_refuses_failures_without_echoing_them() {
        let shell = |script: &str| {
            let mut command = Command::new("sh");
            command.args(["-c", script]);
            bounded_stdout(command)
        };
        assert_eq!(shell("printf rows").unwrap(), "rows");
        assert_eq!(
            shell("echo private >&2; exit 3").unwrap_err().to_string(),
            "object capture transport refused"
        );
        assert_eq!(
            shell("printf '\\377'").unwrap_err().to_string(),
            "unsupported object capture response"
        );
        assert_eq!(
            bounded_stdout(Command::new("synthetic-missing-transport"))
                .unwrap_err()
                .to_string(),
            "object capture transport unavailable"
        );
    }

    #[test]
    fn public_repository_capture_destination_is_refused() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(private_parent(&root.join("synthetic-private-capture.json")).is_err());
    }

    #[test]
    fn private_input_requires_digest_and_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("capture.json");
        std::fs::write(&file, b"{}").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_private(&file, &sha256_hex(b"{}"), "synthetic").is_err());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read_private(&file, "wrong-digest", "synthetic").is_err());
        assert!(object_handover::check_environment("--untrusted-option").is_err());
    }
    #[test]
    fn private_capture_roundtrip_binds_source_identity_and_mapped_values() {
        use std::os::unix::fs::PermissionsExt;
        let source:Readback=serde_json::from_value(serde_json::json!({"services":[{"name":"notation","revision":1,"deployment_id":"synthetic"},{"name":"project-slack","revision":1,"deployment_id":"synthetic"}],"state":[],"invocations":[],"start_journal":[]})).unwrap();
        let capture = object_handover::map_capture(
            &source,
            "synthetic",
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            &std::collections::BTreeSet::new(),
            1,
        )
        .unwrap();
        let captured = PrivateCapture { source, capture };
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("private.json");
        let digest = write_private(&file, &captured).unwrap();
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(read_private(&file, &digest, "synthetic").is_ok());
        assert!(read_private(&file, &digest, "different-source").is_err());
        assert!(write_private(&file, &captured).is_err());
        let mut drift = captured;
        drift.capture.source_readback_sha256 = "0".repeat(64);
        let second = directory.path().join("drift.json");
        let digest = write_private(&second, &drift).unwrap();
        assert!(read_private(&second, &digest, "synthetic").is_err());
    }
}
