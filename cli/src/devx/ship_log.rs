//! `navigator ops ship --quiet --log-sink <URI>`: run the ship with its whole
//! transcript sent to a private sink and only a one-line verdict on the console.
//!
//! The Actions log of a public repository is public, and masking only covers
//! values the runner already knows are secret. So the lane runs `ops ship` as a
//! child process with stdout and stderr both pointed at a file — which also
//! captures what `kubectl` and `gcloud` print, none of which this crate
//! controls — uploads that file to the sink, and prints a fixed-text verdict.
//! Nothing the child wrote, and no sink coordinate, reaches the console.
//!
//! Sinks are the [`cloud::StorageService`] backends: `gs://BUCKET[/PREFIX]` for
//! a deployment's private logs bucket, `file:///DIR` for local runs and tests.
//! The CI identity needs `storage.objects.create` on the sink and nothing
//! else, so a compromised workflow cannot read an earlier transcript back.

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use cloud::{FsStorage, GcsStorage, GcsStorageConfig, StorageService};
use sha2::{Digest, Sha256};

use super::ship::ShipOpts;

/// The env var the `--log-sink` flag also reads. The child is started without
/// it: it is the child's parent that owns the sink.
pub(super) const SINK_ENV: &str = "NAVIGATOR_SHIP_LOG_SINK";

/// Where a transcript is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SinkTarget {
    /// `gs://BUCKET[/PREFIX]`.
    Gcs { bucket: String, prefix: String },
    /// `file:///DIR`, for local runs and tests.
    Dir(PathBuf),
}

impl SinkTarget {
    /// Parse a sink URI. The error never echoes the URI: it usually arrives
    /// through a secret, and the console is the thing being kept clean.
    pub(super) fn parse(uri: &str) -> Result<Self> {
        let unsupported =
            || anyhow!("unsupported --log-sink; expected gs://BUCKET[/PREFIX] or file:///DIR");
        if let Some(rest) = uri.strip_prefix("gs://") {
            let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
            if bucket.is_empty() {
                return Err(unsupported());
            }
            return Ok(Self::Gcs {
                bucket: bucket.to_string(),
                prefix: prefix.trim_matches('/').to_string(),
            });
        }
        if let Some(dir) = uri.strip_prefix("file://") {
            if dir.starts_with('/') {
                return Ok(Self::Dir(PathBuf::from(dir)));
            }
        }
        Err(unsupported())
    }

    fn prefix(&self) -> &str {
        match self {
            Self::Gcs { prefix, .. } => prefix,
            Self::Dir(_) => "",
        }
    }

    async fn storage(&self) -> Result<Box<dyn StorageService>> {
        Ok(match self {
            Self::Gcs { bucket, .. } => Box::new(
                GcsStorage::new_from_config(GcsStorageConfig {
                    bucket: bucket.clone(),
                    endpoint: None,
                })
                .await
                .map_err(|_| anyhow!("could not open the log sink"))?,
            ),
            Self::Dir(dir) => Box::new(
                FsStorage::new(dir.clone())
                    .await
                    .map_err(|_| anyhow!("could not open the log sink"))?,
            ),
        })
    }
}

fn validate_lookup_component(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("invalid {label}; use letters, numbers, hyphens, and underscores only");
    }
    Ok(())
}

/// Read a transcript using the caller's storage credentials. This helper is
/// shared by the operator command and the `file://` sink contract test.
async fn get_transcript(target: &SinkTarget, deployment: &str, id: &str) -> Result<Vec<u8>> {
    validate_lookup_component(deployment, "deployment")?;
    validate_lookup_component(id, "run id")?;
    let storage = target.storage().await?;
    let key = transcript_key(target.prefix(), deployment, id);
    let object = storage
        .get(&key)
        .await
        .with_context(|| format!("read ship transcript for {deployment} run {id}"))?;
    Ok(object.bytes)
}

/// `navigator ops logs --deployment <row> --run <id>`.
pub fn run_logs(
    deployment_name: &str,
    id: &str,
    prefix: &str,
    deployments_dir: Option<&Path>,
    project_id: Option<&str>,
) -> Result<()> {
    validate_lookup_component(deployment_name, "deployment")?;
    validate_lookup_component(id, "run id")?;
    let deployment = if let Some(project) = project_id {
        anyhow::ensure!(
            deployments_dir.is_none(),
            "--project-id conflicts with --deployments-dir"
        );
        super::deployment_config::load(project, deployment_name)?
    } else {
        let root = super::deployments::root(deployments_dir)?;
        super::deployments::Deployment::load(&root, deployment_name)?
    };
    let bucket = deployment
        .coordinates
        .get("NAVIGATOR_LOGS_BUCKET")
        .cloned()
        .ok_or_else(|| {
            anyhow!("NAVIGATOR_LOGS_BUCKET must be set for deployment {deployment_name}")
        })?;
    let target = SinkTarget::Gcs {
        bucket,
        prefix: prefix.trim_matches('/').to_string(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("creating a runtime")?;
    let bytes = runtime.block_on(get_transcript(&target, deployment_name, id))?;
    std::io::stdout()
        .write_all(&bytes)
        .context("write ship transcript to the terminal")
}

/// An id an operator can quote to find the transcript. Under Actions it is the
/// run id and attempt, so the sink key is derivable from the run URL.
pub(super) fn correlation_id(lookup: impl Fn(&str) -> Option<String>, unix_secs: u64) -> String {
    match lookup("GITHUB_RUN_ID").filter(|id| !id.is_empty()) {
        Some(run) => {
            let attempt = lookup("GITHUB_RUN_ATTEMPT")
                .filter(|a| !a.is_empty())
                .unwrap_or_else(|| "1".into());
            format!("{run}-{attempt}")
        }
        None => format!("local-{unix_secs}"),
    }
}

/// The sink key for one transcript.
pub(super) fn transcript_key(prefix: &str, deployment: &str, id: &str) -> String {
    let name = format!("ship/{deployment}/{id}.log");
    if prefix.is_empty() {
        name
    } else {
        format!("{prefix}/{name}")
    }
}

/// The argv for the child: the same ship, minus the two flags that made this
/// the quiet lane.
pub(super) fn child_args(opts: &ShipOpts) -> Vec<String> {
    let mut args = vec![
        "ops".to_string(),
        "ship".to_string(),
        "--deployment".to_string(),
        opts.deployment.clone(),
    ];
    if let Some(dir) = &opts.deployments_dir {
        args.push("--deployments-dir".into());
        args.push(dir.display().to_string());
    }
    if let Some(project) = &opts.project_id {
        args.push("--project-id".into());
        args.push(project.clone());
    }
    for (on, flag) in [
        (opts.dry_run, "--dry-run"),
        (opts.restart_only, "--restart-only"),
        (opts.image_only, "--image-only"),
        (opts.assert_signing_iam, "--assert-signing-iam"),
    ] {
        if on {
            args.push(flag.into());
        }
    }
    if let Some(tag) = &opts.tag {
        args.push("--tag".into());
        args.push(tag.clone());
    }
    if let Some(digests) = &opts.image_digests {
        args.push("--image-digests".into());
        args.push(digests.display().to_string());
    }
    args
}

/// What the console is allowed to say. Fixed text plus numbers and a hash
/// prefix; nothing the child wrote can appear in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Verdict {
    pub exit_code: i32,
    pub id: String,
    pub bytes: usize,
    pub sha256: String,
    pub saved: bool,
}

impl Verdict {
    pub(super) fn ok(&self) -> bool {
        self.exit_code == 0
    }

    pub(super) fn render(&self) -> String {
        let outcome = if self.ok() {
            "ship ok".to_string()
        } else {
            format!("ship FAILED (exit {})", self.exit_code)
        };
        let saved = if self.saved {
            "transcript saved to the private sink"
        } else {
            "transcript NOT saved"
        };
        format!(
            "navigator: {outcome} — id {}, {} bytes, sha256 {}, {saved}",
            self.id,
            self.bytes,
            &self.sha256[..12],
        )
    }
}

/// Run the child through `run` (which must send stdout and stderr to the path
/// it is given and return the exit code), then upload and judge.
///
/// A transcript that cannot be uploaded does not turn a successful ship into a
/// failure — the cluster has already changed — but the verdict says so.
pub(super) fn quiet_run(
    target: &SinkTarget,
    deployment: &str,
    id: &str,
    run: impl FnOnce(&Path) -> Result<i32>,
) -> Result<Verdict> {
    let dir = tempfile::tempdir().context("creating a transcript directory")?;
    let log = dir.path().join("ship.log");
    let exit_code = run(&log)?;
    let bytes = std::fs::read(&log).unwrap_or_default();
    let sha256 = hex(&Sha256::digest(&bytes));

    let key = transcript_key(target.prefix(), deployment, id);
    let runtime = tokio::runtime::Runtime::new().context("creating a runtime")?;
    let saved = runtime
        .block_on(async {
            let storage = target.storage().await?;
            storage
                .put(&key, &bytes, "text/plain; charset=utf-8")
                .await
                .map_err(|_| anyhow!("upload failed"))
        })
        .is_ok();

    Ok(Verdict {
        exit_code,
        id: id.to_string(),
        bytes: bytes.len(),
        sha256,
        saved,
    })
}

fn hex(digest: &[u8]) -> String {
    use std::fmt::Write as _;
    digest.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Start `exe args…` with stdout and stderr both appended to `log`, wait, and
/// return its exit code. Stdin is closed so a prompt cannot hang the lane.
fn run_captured(exe: &Path, args: &[String], log: &Path) -> Result<i32> {
    let out = File::create(log).context("creating the transcript file")?;
    let err = out.try_clone().context("sharing the transcript file")?;
    let status = Command::new(exe)
        .args(args)
        .env_remove(SINK_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .status()
        .context("starting the ship")?;
    Ok(status.code().unwrap_or(1))
}

/// Entry point for `ops ship --quiet --log-sink`.
pub fn run_quiet(opts: &ShipOpts) -> Result<()> {
    if opts.dry_run {
        // Planning never uploads a transcript. Cloud configuration, when
        // explicitly selected, is still fetched by run_ship.
        return super::ship::run_ship(opts).map_err(|_| {
            anyhow!("ship plan failed validation (details suppressed in quiet mode)")
        });
    }
    let uri = opts
        .log_sink
        .as_deref()
        .context("--quiet needs --log-sink (or NAVIGATOR_SHIP_LOG_SINK)")?;
    // Refuse a bad sink before anything ships, not after.
    let target = SinkTarget::parse(uri)?;
    let exe = std::env::current_exe().context("locating this binary")?;
    let args = child_args(opts);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let id = correlation_id(|k| std::env::var(k).ok(), now);

    let verdict = quiet_run(&target, &opts.deployment, &id, |log| {
        run_captured(&exe, &args, log)
    })?;
    if verdict.ok() {
        println!("{}", verdict.render());
        Ok(())
    } else {
        bail!("{}", verdict.render())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> ShipOpts {
        ShipOpts {
            deployment: "example-prod".into(),
            tag: Some("26.10.11".into()),
            ..ShipOpts::default()
        }
    }

    #[test]
    fn quiet_child_keeps_the_explicit_cloud_project() {
        let mut options = opts();
        options.project_id = Some("example-project".into());
        let args = child_args(&options);
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--project-id", "example-project"]));
        assert!(!args.iter().any(|arg| arg == "--deployments-dir"));
    }

    #[test]
    fn a_gs_sink_splits_bucket_and_prefix() {
        assert_eq!(
            SinkTarget::parse("gs://example-logs/ci/").unwrap(),
            SinkTarget::Gcs {
                bucket: "example-logs".into(),
                prefix: "ci".into()
            }
        );
        assert_eq!(
            SinkTarget::parse("gs://example-logs").unwrap(),
            SinkTarget::Gcs {
                bucket: "example-logs".into(),
                prefix: String::new()
            }
        );
    }

    #[test]
    fn a_file_sink_needs_an_absolute_directory() {
        assert_eq!(
            SinkTarget::parse("file:///var/tmp/x").unwrap(),
            SinkTarget::Dir(PathBuf::from("/var/tmp/x"))
        );
        assert!(SinkTarget::parse("file://relative").is_err());
    }

    #[test]
    fn a_bad_sink_error_does_not_echo_the_uri() {
        for bad in ["s3://secret-bucket", "gs://", "secret-bucket", ""] {
            let message = SinkTarget::parse(bad).unwrap_err().to_string();
            assert!(!message.contains("secret-bucket"), "{message}");
        }
    }

    #[test]
    fn the_id_comes_from_the_actions_run_else_the_clock() {
        let env = |k: &str| match k {
            "GITHUB_RUN_ID" => Some("991".to_string()),
            "GITHUB_RUN_ATTEMPT" => Some("2".to_string()),
            _ => None,
        };
        assert_eq!(correlation_id(env, 5), "991-2");
        assert_eq!(correlation_id(|_| None, 5), "local-5");
        let no_attempt = |k: &str| (k == "GITHUB_RUN_ID").then(|| "991".to_string());
        assert_eq!(correlation_id(no_attempt, 5), "991-1");
    }

    #[test]
    fn the_key_nests_under_the_prefix() {
        assert_eq!(
            transcript_key("ci", "example-prod", "991-1"),
            "ci/ship/example-prod/991-1.log"
        );
        assert_eq!(
            transcript_key("", "example-prod", "991-1"),
            "ship/example-prod/991-1.log"
        );
    }

    #[test]
    fn the_child_runs_the_same_ship_without_the_quiet_flags() {
        let mut o = opts();
        o.deployments_dir = Some(PathBuf::from("/d"));
        o.assert_signing_iam = true;
        o.image_digests = Some(PathBuf::from("/tmp/image-digests.json"));
        o.log_sink = Some("gs://x".into());
        o.quiet = true;
        let args = child_args(&o);
        assert_eq!(
            args,
            [
                "ops",
                "ship",
                "--deployment",
                "example-prod",
                "--deployments-dir",
                "/d",
                "--assert-signing-iam",
                "--tag",
                "26.10.11",
                "--image-digests",
                "/tmp/image-digests.json"
            ]
        );
        assert!(!args
            .iter()
            .any(|a| a.contains("quiet") || a.contains("sink")));
    }

    #[test]
    fn the_verdict_carries_no_child_output() {
        let dir = tempfile::tempdir().unwrap();
        let target = SinkTarget::Dir(dir.path().to_path_buf());
        let verdict = quiet_run(&target, "example-prod", "7-1", |log| {
            std::fs::write(log, "project=SECRET-PROJECT-ID pod log line\n").unwrap();
            Ok(1)
        })
        .unwrap();
        let line = verdict.render();
        assert!(!line.contains("SECRET-PROJECT-ID"), "{line}");
        assert!(line.contains("ship FAILED (exit 1)"));
        assert!(line.contains("id 7-1"));
        assert!(verdict.saved);
    }

    #[test]
    fn the_full_transcript_lands_in_the_sink() {
        let dir = tempfile::tempdir().unwrap();
        let target = SinkTarget::Dir(dir.path().to_path_buf());
        let verdict = quiet_run(&target, "example-prod", "7-1", |log| {
            std::fs::write(log, "full transcript\n").unwrap();
            Ok(0)
        })
        .unwrap();
        assert!(verdict.ok() && verdict.saved);
        let stored = std::fs::read(dir.path().join("ship/example-prod/7-1.log.bin")).unwrap();
        assert_eq!(stored, b"full transcript\n");
    }

    #[test]
    fn the_operator_reader_recovers_the_file_sink_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let target = SinkTarget::Dir(dir.path().to_path_buf());
        let verdict = quiet_run(&target, "example-prod", "7-1", |log| {
            std::fs::write(log, "full transcript\n").unwrap();
            Ok(0)
        })
        .unwrap();
        assert!(verdict.saved);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        assert_eq!(
            runtime
                .block_on(get_transcript(&target, "example-prod", "7-1"))
                .unwrap(),
            b"full transcript\n"
        );
    }

    #[tokio::test]
    async fn the_operator_reader_rejects_path_components_before_storage_access() {
        let target = SinkTarget::Dir(PathBuf::from("/does/not/matter"));
        let error = get_transcript(&target, "../other", "7-1")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("invalid deployment"));
    }

    #[test]
    fn an_unwritable_sink_fails_the_verdict_not_the_ship() {
        // A file where the sink directory should be makes every put fail.
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "x").unwrap();
        let target = SinkTarget::Dir(blocker);
        let verdict = quiet_run(&target, "example-prod", "7-1", |log| {
            std::fs::write(log, "ok\n").unwrap();
            Ok(0)
        })
        .unwrap();
        assert!(verdict.ok());
        assert!(!verdict.saved);
        assert!(verdict.render().contains("NOT saved"));
    }

    #[cfg(unix)]
    #[test]
    fn a_real_child_has_stdout_and_stderr_captured_and_its_exit_code_kept() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("t.log");
        let args = vec![
            "-c".to_string(),
            "echo to-stdout; echo to-stderr >&2; exit 3".to_string(),
        ];
        let code = run_captured(Path::new("/bin/sh"), &args, &log).unwrap();
        assert_eq!(code, 3);
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("to-stdout") && text.contains("to-stderr"));
    }
}
