//! Immutable reference-only `DuckDB` files using an explicitly supplied, verified official executable.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use arrow::array::{Array, Float64Array, Int32Array, Int64Array, RecordBatch, StringArray};
use arrow::datatypes::DataType;
use chrono::NaiveDate;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::ephemeris_download::{hex_digest, open_regular};
use crate::ephemeris_tables::{
    digest, read_bounded, write_new, MAX_FILE_BYTES, MAX_MANIFEST_BYTES,
};

pub(super) const DATABASE: &str = "reference.duckdb";
pub(super) const RECEIPT: &str = "duckdb-manifest.json";
pub(super) const QUERY: &str = "query.sql";
const STDIN_CAP: usize = 65_536;
const STDOUT_CAP: usize = 4_194_304;
const STDERR_CAP: usize = 65_536;
const PROCESS_BUDGET: Duration = Duration::from_secs(60);

const OBSERVATION_SAMPLES: usize = 5;
const ENGINE_THREADS: u8 = 1;
const ENGINE_MEMORY_LIMIT: &str = "128MB";

#[derive(Serialize)]
pub(super) struct QueryObservations {
    engine_version: String,
    engine_threads: u8,
    engine_memory_limit: &'static str,
    samples_per_phase: usize,
    excluded_warmup_samples: usize,
    time_predicate_days: i64,
    cases: Vec<QueryObservation>,
}

#[derive(Serialize)]
struct QueryObservation {
    predicate: &'static str,
    matching_rows: usize,
    fresh_process_engine_seconds: SampleSummary,
    fresh_process_driver_seconds: SampleSummary,
    same_process_warm_engine_seconds: SampleSummary,
    fresh_engine_reported_read_bytes: Vec<u64>,
    warm_engine_reported_read_bytes: Vec<u64>,
    #[cfg(test)]
    #[serde(skip)]
    fresh_process_ids: Vec<Option<u32>>,
    #[cfg(test)]
    #[serde(skip)]
    warm_process_id: Option<u32>,
}

#[derive(Serialize)]
struct SampleSummary {
    samples: usize,
    nearest_rank_p50: f64,
    nearest_rank_p95: f64,
}

// Profile structs deliberately retain only scalar observations, never query text, paths or plan payloads.
#[derive(Deserialize)]
struct EngineProfile {
    query: QueryTiming,
    io: ReadObservation,
}

#[derive(Deserialize)]
struct QueryTiming {
    total_time: f64,
}

#[derive(Deserialize)]
struct ReadObservation {
    total_bytes_read: u64,
}

struct ProfileRun {
    profiles: Vec<EngineProfile>,
    wall_seconds: f64,
    #[cfg(test)]
    process_id: Option<u32>,
}

fn summarize(samples: &[f64]) -> Result<SampleSummary> {
    ensure!(
        !samples.is_empty() && samples.iter().all(|v| v.is_finite() && *v >= 0.0),
        "invalid query timing observations"
    );
    let mut ordered = samples.to_vec();
    ordered.sort_unstable_by(f64::total_cmp);
    let rank = |p: usize| (ordered.len() / 100) * p + ((ordered.len() % 100) * p).div_ceil(100);
    Ok(SampleSummary {
        samples: ordered.len(),
        nearest_rank_p50: ordered[rank(50) - 1],
        nearest_rank_p95: ordered[rank(95) - 1],
    })
}

fn decode_profiles(bytes: &[u8], count: usize) -> Result<Vec<EngineProfile>> {
    ensure!(
        bytes.len() <= STDOUT_CAP && (1..=OBSERVATION_SAMPLES + 1).contains(&count),
        "invalid bounded query profile"
    );
    let profiles = serde_json::Deserializer::from_slice(bytes)
        .into_iter::<EngineProfile>()
        .take(count + 1)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| anyhow::anyhow!("invalid bounded query profile"))?;
    ensure!(
        profiles.len() == count
            && profiles
                .iter()
                .all(|p| p.query.total_time.is_finite() && p.query.total_time >= 0.0),
        "invalid bounded query profile"
    );
    Ok(profiles)
}

fn without_warmup(profiles: Vec<EngineProfile>) -> Result<Vec<EngineProfile>> {
    ensure!(
        profiles.len() == OBSERVATION_SAMPLES + 1,
        "invalid bounded query profile"
    );
    Ok(profiles.into_iter().skip(1).collect())
}

fn script(setup: &str, sql: &str) -> Result<String> {
    let length = setup
        .len()
        .checked_add(sql.len())
        .and_then(|n| n.checked_add(1))
        .context("DuckDB SQL exceeds its bound")?;
    ensure!(length <= STDIN_CAP, "DuckDB SQL exceeds its bound");
    Ok(format!("{setup}\n{sql}"))
}

fn spawn(command: &mut tokio::process::Command) -> Result<tokio::process::Child> {
    command
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| anyhow::anyhow!("cannot start the verified DuckDB executable"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pins {
    schema_version: u32,
    engine_version: String,
    storage_version: String,
    tested_reader_versions: Vec<String>,
    binaries: Vec<BinaryPin>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BinaryPin {
    platform: String,
    archive_url: String,
    archive_bytes: u64,
    archive_sha256: String,
    binary_bytes: u64,
    binary_sha256: String,
}

pub(super) struct Tool {
    directory: tempfile::TempDir,
    executable: PathBuf,
    pin: BinaryPin,
    pins: Pins,
}

pub(super) struct Reference<'a> {
    pub snapshot_id: &'a str,
    pub manifest: &'a [u8],
    pub source: &'a [u8],
    pub batch: &'a RecordBatch,
    pub from: NaiveDate,
    pub to: NaiveDate,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    artifact_id: String,
    content: Content,
}

#[derive(PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Content {
    schema_version: u32,
    reference_snapshot_id: String,
    engine_version: String,
    storage_version: String,
    tested_reader_versions: Vec<String>,
    binary: BinaryPin,
    files: Vec<FileIdentity>,
}

#[derive(PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileIdentity {
    name: String,
    bytes: u64,
    sha256: String,
}

fn private_directory() -> Result<tempfile::TempDir> {
    let directory = tempfile::tempdir()?;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
    Ok(directory)
}

fn copy_checked(mut source: std::fs::File, target: &Path, bytes: u64, sha: &str) -> Result<()> {
    ensure!(
        source.metadata()?.len() == bytes,
        "DuckDB file size differs from its pin"
    );
    let mut target = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = vec![0_u8; 65_536];
    loop {
        let n = source.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        total += u64::try_from(n)?;
        ensure!(total <= bytes, "DuckDB file exceeds its pin");
        hasher.update(&buffer[..n]);
        target.write_all(&buffer[..n])?;
    }
    ensure!(
        total == bytes && hex_digest(&hasher.finalize()) == sha,
        "DuckDB checksum differs from its pin"
    );
    target.sync_all()?;
    Ok(())
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn path_literal(path: &Path) -> Result<String> {
    Ok(quote(
        path.to_str()
            .context("DuckDB requires a UTF-8 staging path")?,
    ))
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    name.into()
}

fn no_sidecars(path: &Path) -> Result<()> {
    for suffix in [".wal", ".wal.checkpoint"] {
        ensure!(
            matches!(std::fs::symlink_metadata(sidecar(path, suffix)), Err(e) if e.kind() == std::io::ErrorKind::NotFound),
            "DuckDB sidecar is present or unreadable"
        );
    }
    Ok(())
}

fn bootstrap(database: &Path, parquet: Option<&Path>, storage: &str) -> Result<String> {
    let mut paths = vec![
        path_literal(database)?,
        path_literal(&sidecar(database, ".wal"))?,
    ];
    let mut load = String::new();
    let mode = if let Some(parquet) = parquet {
        paths.push(path_literal(&sidecar(database, ".wal.checkpoint"))?);
        paths.push(path_literal(parquet)?);
        load.push_str("LOAD parquet;");
        format!("STORAGE_VERSION {}", quote(storage))
    } else {
        "READ_ONLY".into()
    };
    // The CLI's -safe setting applies after opening a positional database. Open only clean memory,
    // configure the exact private file graph, and attach the verified copy after restrictions exist.
    Ok(format!("SET autoinstall_known_extensions=false;SET autoload_known_extensions=false;SET threads={ENGINE_THREADS};SET memory_limit='{ENGINE_MEMORY_LIMIT}';SET temp_directory='';SET allow_persistent_secrets=false;{load}SET allowed_paths=[{}];SET enable_external_access=false;SET lock_configuration=true;ATTACH {} AS reference ({mode});USE reference;", paths.join(","), path_literal(database)?))
}

async fn bounded_read(reader: impl tokio::io::AsyncRead + Unpin, cap: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(u64::try_from(cap)? + 1)
        .read_to_end(&mut bytes)
        .await?;
    ensure!(
        bytes.len() <= cap,
        "DuckDB process output exceeds its bound"
    );
    Ok(bytes)
}

async fn capture(
    command: &mut tokio::process::Command,
    sql: &str,
    budget: Duration,
) -> Result<Vec<u8>> {
    ensure!(sql.len() <= STDIN_CAP, "DuckDB SQL exceeds its bound");
    let child = spawn(command)?;
    capture_child(child, sql, budget).await
}

async fn capture_child(
    mut child: tokio::process::Child,
    sql: &str,
    budget: Duration,
) -> Result<Vec<u8>> {
    let run = async {
        let mut input = child
            .stdin
            .take()
            .context("DuckDB input pipe unavailable")?;
        let output = child
            .stdout
            .take()
            .context("DuckDB output pipe unavailable")?;
        let errors = child
            .stderr
            .take()
            .context("DuckDB error pipe unavailable")?;
        let write = async {
            input.write_all(sql.as_bytes()).await?;
            input.shutdown().await?;
            drop(input);
            Ok::<_, anyhow::Error>(())
        };
        let ((), output, _, status) = tokio::try_join!(
            write,
            bounded_read(output, STDOUT_CAP),
            bounded_read(errors, STDERR_CAP),
            async { Ok::<_, anyhow::Error>(child.wait().await?) }
        )?;
        ensure!(status.success(), "DuckDB refused the operation");
        Ok::<_, anyhow::Error>(output)
    };
    if let Ok(Ok(bytes)) = tokio::time::timeout(budget, run).await {
        Ok(bytes)
    } else {
        // Reap explicitly on error or timeout; never promote a stage while its writer remains alive.
        let _ = child.kill().await;
        let _ = child.wait().await;
        bail!("DuckDB operation failed or exceeded its resource bounds")
    }
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    let parse = || -> Result<T> {
        let mut frames = serde_json::Deserializer::from_slice(bytes)
            .into_iter::<Box<RawValue>>()
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let last = frames.pop().context("DuckDB response missing")?;
        for frame in frames {
            ensure!(
                serde_json::from_str::<Vec<Box<RawValue>>>(frame.get())?.is_empty(),
                "unexpected DuckDB prefix response"
            );
        }
        Ok(serde_json::from_str(last.get())?)
    };
    parse().map_err(|_| anyhow::anyhow!("invalid bounded DuckDB response"))
}

impl Tool {
    pub(super) fn verified(path: &Path) -> Result<Self> {
        let pins: Pins = serde_json::from_str(include_str!("ephemeris_duckdb_pins.json"))?;
        ensure!(pins.schema_version == 1, "unsupported DuckDB pin schema");
        let source = open_regular(path)?;
        let size = source.metadata()?.len();
        let prefix = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => "macos-arm64",
            ("linux", "x86_64") => "linux-amd64-",
            _ => bail!("no approved DuckDB executable for this host"),
        };
        let pin = pins
            .binaries
            .iter()
            .find(|p| p.platform.starts_with(prefix) && p.binary_bytes == size)
            .context("DuckDB executable is not an approved platform/size")?
            .clone();
        let directory = private_directory()?;
        let executable = directory.path().join("duckdb");
        copy_checked(source, &executable, pin.binary_bytes, &pin.binary_sha256)?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o500))?;
        Ok(Self {
            directory,
            executable,
            pin,
            pins,
        })
    }

    fn command(&self, home: &Path) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.executable);
        command
            .args([
                "-no-init",
                "-no-agent",
                "-batch",
                "-bail",
                "-json",
                ":memory:",
            ])
            .env_clear()
            .env("HOME", home)
            .current_dir(home);
        command
    }

    async fn process(&self, home: &Path, setup: &str, sql: &str) -> Result<Vec<u8>> {
        let script = script(setup, sql)?;
        let mut command = self.command(home);
        ensure!(
            self.directory.path().is_dir(),
            "DuckDB executable stage unavailable"
        );
        capture(&mut command, &script, PROCESS_BUDGET).await
    }

    pub(super) async fn author(&self, dir: &Path, reference: &Reference<'_>) -> Result<()> {
        let work = private_directory()?;
        let database = work.path().join(DATABASE);
        let parquet = dir.join("reference.parquet");
        let setup = bootstrap(&database, Some(&parquet), &self.pins.storage_version)?;
        let schema = reference.batch.schema();
        let columns = schema
            .fields()
            .iter()
            .map(|f| {
                let ty = match f.data_type() {
                    DataType::Utf8 => "VARCHAR",
                    DataType::Int64 => "BIGINT",
                    DataType::Int32 => "INTEGER",
                    DataType::Float64 => "DOUBLE",
                    _ => return Err(anyhow::anyhow!("unsupported reference column type")),
                };
                Ok(format!(
                    "{} {ty}{}",
                    f.name(),
                    if f.is_nullable() { "" } else { " NOT NULL" }
                ))
            })
            .collect::<Result<Vec<_>>>()?
            .join(",");
        let manifest = quote(std::str::from_utf8(reference.manifest)?);
        let source = quote(std::str::from_utf8(reference.source)?);
        let sql = format!("CREATE TABLE ephemeris_reference_table({columns});INSERT INTO ephemeris_reference_table SELECT * FROM read_parquet({});CREATE TABLE reference_provenance(snapshot_id VARCHAR NOT NULL,reference_manifest_json VARCHAR NOT NULL,source_manifest_json VARCHAR NOT NULL);INSERT INTO reference_provenance VALUES({},{manifest},{source});CHECKPOINT;", path_literal(&parquet)?, quote(reference.snapshot_id));
        let output = self.process(work.path(), &setup, &sql).await?;
        ensure!(
            decode::<Vec<Box<RawValue>>>(&output)?.is_empty(),
            "unexpected DuckDB creation response"
        );
        no_sidecars(&database)?;
        let database_bytes = read_bounded(&database, MAX_FILE_BYTES)?;
        write_new(&dir.join(DATABASE), &database_bytes)?;
        let query = reference.query();
        write_new(&dir.join(QUERY), query.as_bytes())?;
        let content = Content {
            schema_version: 1,
            reference_snapshot_id: reference.snapshot_id.into(),
            engine_version: self.pins.engine_version.clone(),
            storage_version: self.pins.storage_version.clone(),
            tested_reader_versions: self.pins.tested_reader_versions.clone(),
            binary: self.pin.clone(),
            files: vec![
                file_identity(DATABASE, &database_bytes)?,
                file_identity(QUERY, query.as_bytes())?,
            ],
        };
        let receipt = Receipt {
            artifact_id: digest(&serde_json::to_vec(&content)?),
            content,
        };
        let bytes = serde_json::to_vec_pretty(&receipt)?;
        ensure!(
            bytes.len() <= MAX_MANIFEST_BYTES,
            "DuckDB receipt exceeds its bound"
        );
        write_new(&dir.join(RECEIPT), &bytes)?;
        self.verify(dir, reference).await
    }

    pub(super) async fn verify(&self, dir: &Path, reference: &Reference<'_>) -> Result<()> {
        self.verified_database(dir, reference).await?;
        Ok(())
    }

    async fn verified_database(
        &self,
        dir: &Path,
        reference: &Reference<'_>,
    ) -> Result<tempfile::TempDir> {
        let receipt: Receipt =
            serde_json::from_slice(&read_bounded(&dir.join(RECEIPT), MAX_MANIFEST_BYTES)?)?;
        let content = &receipt.content;
        ensure!(
            receipt.artifact_id == digest(&serde_json::to_vec(content)?)
                && content.schema_version == 1
                && content.reference_snapshot_id == reference.snapshot_id
                && self.pins.binaries.contains(&content.binary)
                && content.engine_version == self.pins.engine_version
                && content.storage_version == self.pins.storage_version
                && content.tested_reader_versions == self.pins.tested_reader_versions
                && content.files.len() == 2,
            "DuckDB receipt settings or identity differ"
        );
        let query = reference.query();
        ensure!(
            read_bounded(&dir.join(QUERY), STDIN_CAP)? == query.as_bytes(),
            "DuckDB query differs"
        );
        let database = &content.files[0];
        ensure!(
            database.name == DATABASE
                && database.bytes <= u64::try_from(MAX_FILE_BYTES)?
                && content.files[1] == file_identity(QUERY, query.as_bytes())?,
            "DuckDB file identities differ"
        );
        no_sidecars(&dir.join(DATABASE))?;
        let work = private_directory()?;
        let copy = work.path().join(DATABASE);
        copy_checked(
            open_regular(&dir.join(DATABASE))?,
            &copy,
            database.bytes,
            &database.sha256,
        )?;
        no_sidecars(&copy)?;
        let setup = bootstrap(&copy, None, &self.pins.storage_version)?;
        self.validate(work.path(), &setup, reference).await?;
        no_sidecars(&copy)?;
        no_sidecars(&dir.join(DATABASE))?;
        Ok(work)
    }

    async fn profile_process(
        &self,
        home: &Path,
        setup: &str,
        sql: &str,
        count: usize,
    ) -> Result<ProfileRun> {
        let input = script(setup, sql)?;
        ensure!(
            self.directory.path().is_dir(),
            "DuckDB executable stage unavailable"
        );
        let mut command = self.command(home);
        let started = std::time::Instant::now();
        let child = spawn(&mut command)?;
        #[cfg(test)]
        let process_id = child.id();
        let output = capture_child(child, &input, PROCESS_BUDGET).await?;
        let wall_seconds = started.elapsed().as_secs_f64();
        // Decode and validate after stopping the driver clock.
        let profiles = decode_profiles(&output, count)?;
        Ok(ProfileRun {
            profiles,
            wall_seconds,
            #[cfg(test)]
            process_id,
        })
    }

    pub(super) async fn measure(
        &self,
        dir: &Path,
        reference: &Reference<'_>,
    ) -> Result<QueryObservations> {
        let work = self.verified_database(dir, reference).await?;
        let copy = work.path().join(DATABASE);
        let setup = bootstrap(&copy, None, &self.pins.storage_version)?;
        let days = reference
            .to
            .signed_duration_since(reference.from)
            .num_days();
        ensure!(
            (1..=ephemeris::MAX_REFERENCE_BATCH_DAYS).contains(&days),
            "invalid measurement coverage"
        );
        let window = days.min(31);
        let end = reference
            .from
            .checked_add_signed(chrono::Duration::days(window))
            .context("valid measurement window")?;
        let bodies = reference
            .batch
            .column_by_name("body")
            .context("reference body missing")?
            .as_any()
            .downcast_ref::<StringArray>()
            .context("reference body type")?;
        let calendars = reference
            .batch
            .column_by_name("reference_calendar")
            .context("reference calendar missing")?
            .as_any()
            .downcast_ref::<StringArray>()
            .context("reference calendar type")?;
        let mut seen = BTreeSet::new();
        let order = (0..bodies.len())
            .filter_map(|i| seen.insert(bodies.value(i)).then_some(bodies.value(i)))
            .enumerate()
            .map(|(i, b)| format!("WHEN {} THEN {i}", quote(b)))
            .collect::<Vec<_>>()
            .join(" ");
        let mut cases = Vec::new();
        for (predicate, moon, bounded_time) in [
            ("all", false, false),
            ("moon", true, false),
            ("first_window", false, true),
            ("moon_first_window", true, true),
        ] {
            let until = if bounded_time { end } else { reference.to };
            let from_label = reference.from.to_string();
            let until_label = until.to_string();
            let filter = format!(
                "reference_calendar>={} AND reference_calendar<{}{}",
                quote(&reference.from.to_string()),
                quote(&until.to_string()),
                if moon { " AND body='moon'" } else { "" }
            );
            let query = format!("SELECT * FROM ephemeris_reference_table WHERE {filter} ORDER BY reference_calendar,CASE body {order} END;");
            let selected = (0..bodies.len())
                .filter(|i| {
                    (!moon || bodies.value(*i) == "moon")
                        && calendars.value(*i) >= from_label.as_str()
                        && calendars.value(*i) < until_label.as_str()
                })
                .collect::<Vec<_>>();
            let rows: Vec<NativeRow> = self.json(work.path(), &setup, &query).await?;
            compare_rows(&rows, reference.batch, &selected)?;
            cases.push(
                self.observe_query(work.path(), &setup, predicate, &query, selected.len())
                    .await?,
            );
        }
        no_sidecars(&copy)?;
        no_sidecars(&dir.join(DATABASE))?;
        Ok(QueryObservations {
            engine_version: self.pins.engine_version.clone(),
            engine_threads: ENGINE_THREADS,
            engine_memory_limit: ENGINE_MEMORY_LIMIT,
            samples_per_phase: OBSERVATION_SAMPLES,
            excluded_warmup_samples: 1,
            time_predicate_days: window,
            cases,
        })
    }

    async fn observe_query(
        &self,
        home: &Path,
        setup: &str,
        predicate: &'static str,
        query: &str,
        matching_rows: usize,
    ) -> Result<QueryObservation> {
        let explain = format!("EXPLAIN (ANALYZE,FORMAT JSON) {query}");
        let mut fresh = Vec::new();
        let mut wall = Vec::new();
        #[cfg(test)]
        let mut fresh_process_ids = Vec::new();
        for _ in 0..OBSERVATION_SAMPLES {
            let run = self.profile_process(home, setup, &explain, 1).await?;
            wall.push(run.wall_seconds);
            #[cfg(test)]
            fresh_process_ids.push(run.process_id);
            fresh.extend(run.profiles);
        }
        let run = self
            .profile_process(
                home,
                setup,
                &explain.repeat(OBSERVATION_SAMPLES + 1),
                OBSERVATION_SAMPLES + 1,
            )
            .await?;
        // One independent prewarm is deliberately outside the warm observation set.
        let warm = without_warmup(run.profiles)?;
        Ok(QueryObservation {
            predicate,
            matching_rows,
            fresh_process_engine_seconds: summarize(
                &fresh.iter().map(|p| p.query.total_time).collect::<Vec<_>>(),
            )?,
            fresh_process_driver_seconds: summarize(&wall)?,
            same_process_warm_engine_seconds: summarize(
                &warm.iter().map(|p| p.query.total_time).collect::<Vec<_>>(),
            )?,
            fresh_engine_reported_read_bytes: fresh.iter().map(|p| p.io.total_bytes_read).collect(),
            warm_engine_reported_read_bytes: warm.iter().map(|p| p.io.total_bytes_read).collect(),
            #[cfg(test)]
            fresh_process_ids,
            #[cfg(test)]
            warm_process_id: run.process_id,
        })
    }

    async fn json<T: DeserializeOwned>(&self, home: &Path, setup: &str, query: &str) -> Result<T> {
        decode(&self.process(home, setup, query).await?)
    }

    async fn validate(&self, home: &Path, setup: &str, reference: &Reference<'_>) -> Result<()> {
        let tables: Vec<TableName> = self.json(home, setup, "SELECT table_name FROM duckdb_tables() WHERE database_name='reference' AND NOT internal ORDER BY table_name;").await?;
        ensure!(
            tables
                .iter()
                .map(|r| r.table_name.as_str())
                .eq(["ephemeris_reference_table", "reference_provenance"]),
            "DuckDB table inventory differs"
        );
        let count: Vec<Count> = self.json(home, setup, "SELECT (SELECT count(*) FROM duckdb_views() WHERE database_name='reference' AND NOT internal)+(SELECT count(*) FROM duckdb_sequences() WHERE database_name='reference')+(SELECT count(*) FROM duckdb_indexes() WHERE database_name='reference')+(SELECT count(*) FROM duckdb_types() WHERE database_name='reference' AND NOT internal)+(SELECT count(*) FROM duckdb_functions() WHERE database_name='reference' AND NOT internal)+(SELECT count(*) FROM duckdb_schemas() WHERE database_name='reference' AND NOT internal AND schema_name<>'main') AS n;").await?;
        ensure!(
            count.len() == 1 && count[0].n == 0,
            "DuckDB contains unsupported objects"
        );
        let version: Vec<Version> = self
            .json(home, setup, "SELECT version() AS version;")
            .await?;
        ensure!(
            version.len() == 1 && version[0].version == self.pins.engine_version,
            "DuckDB engine version differs"
        );
        let columns: Vec<PhysicalColumn> = self
            .json(home, setup, "DESCRIBE ephemeris_reference_table;")
            .await?;
        let schema = reference.batch.schema();
        let expected = schema
            .fields()
            .iter()
            .map(|field| {
                let ty = match field.data_type() {
                    DataType::Utf8 => "VARCHAR",
                    DataType::Int64 => "BIGINT",
                    DataType::Int32 => "INTEGER",
                    DataType::Float64 => "DOUBLE",
                    _ => return Err(anyhow::anyhow!("unsupported reference column")),
                };
                Ok((field.name().as_str(), ty, field.is_nullable()))
            })
            .collect::<Result<Vec<_>>>()?;
        validate_columns(&columns, &expected)?;
        let provenance: Vec<PhysicalColumn> = self
            .json(home, setup, "DESCRIBE reference_provenance;")
            .await?;
        validate_columns(
            &provenance,
            &[
                ("snapshot_id", "VARCHAR", false),
                ("reference_manifest_json", "VARCHAR", false),
                ("source_manifest_json", "VARCHAR", false),
            ],
        )?;
        let meta: Vec<Provenance> = self
            .json(home, setup, "SELECT * FROM reference_provenance;")
            .await?;
        ensure!(
            meta.len() == 1
                && meta[0].snapshot_id == reference.snapshot_id
                && meta[0].reference_manifest_json.as_bytes() == reference.manifest
                && meta[0].source_manifest_json.as_bytes() == reference.source,
            "DuckDB provenance differs"
        );
        let body = reference
            .batch
            .column_by_name("body")
            .context("reference body missing")?
            .as_any()
            .downcast_ref::<StringArray>()
            .context("reference body type")?;
        let mut seen = BTreeSet::new();
        let order = (0..body.len())
            .filter_map(|i| seen.insert(body.value(i)).then_some(body.value(i)))
            .enumerate()
            .map(|(i, b)| format!("WHEN {} THEN {i}", quote(b)))
            .collect::<Vec<_>>()
            .join(" ");
        let query = format!("SELECT * FROM ephemeris_reference_table ORDER BY reference_calendar,CASE body {order} END;");
        let rows: Vec<NativeRow> = self.json(home, setup, &query).await?;
        compare_rows(
            &rows,
            reference.batch,
            &(0..reference.batch.num_rows()).collect::<Vec<_>>(),
        )?;
        let rows: Vec<NativeRow> = self.json(home, setup, &reference.query()).await?;
        let selected = (0..body.len())
            .filter(|i| body.value(*i) == "moon")
            .collect::<Vec<_>>();
        compare_rows(&rows, reference.batch, &selected)
    }
}

impl Reference<'_> {
    fn query(&self) -> String {
        format!("SELECT * FROM ephemeris_reference_table WHERE body='moon' AND reference_calendar>={} AND reference_calendar<{} ORDER BY reference_calendar;\n", quote(&self.from.to_string()), quote(&self.to.to_string()))
    }
}

fn validate_columns(actual: &[PhysicalColumn], expected: &[(&str, &str, bool)]) -> Result<()> {
    ensure!(
        actual.len() == expected.len(),
        "DuckDB column count differs"
    );
    for (column, (name, ty, nullable)) in actual.iter().zip(expected) {
        ensure!(
            column.column_name == *name
                && column.column_type == *ty
                && column.null == if *nullable { "YES" } else { "NO" }
                && column.key.is_none()
                && column.default.is_none()
                && column.extra.is_none(),
            "DuckDB physical schema differs"
        );
    }
    Ok(())
}

fn file_identity(name: &str, bytes: &[u8]) -> Result<FileIdentity> {
    Ok(FileIdentity {
        name: name.into(),
        bytes: u64::try_from(bytes.len())?,
        sha256: digest(bytes),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TableName {
    table_name: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Count {
    n: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Version {
    version: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PhysicalColumn {
    column_name: String,
    column_type: String,
    null: String,
    key: Option<String>,
    default: Option<String>,
    extra: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Provenance {
    snapshot_id: String,
    reference_manifest_json: String,
    source_manifest_json: String,
}
#[derive(Deserialize)]
#[serde(transparent)]
struct NativeRow(BTreeMap<String, Box<RawValue>>);

fn compare_rows(rows: &[NativeRow], batch: &RecordBatch, indices: &[usize]) -> Result<()> {
    ensure!(rows.len() == indices.len(), "DuckDB rows differ");
    for (row, index) in rows.iter().zip(indices) {
        ensure!(
            row.0.len() == batch.num_columns(),
            "DuckDB row columns differ"
        );
        for (field, array) in batch.schema().fields().iter().zip(batch.columns()) {
            let raw = row
                .0
                .get(field.name())
                .context("DuckDB row column missing")?
                .get();
            if array.is_null(*index) {
                ensure!(raw == "null", "DuckDB null mask differs");
                continue;
            }
            let result = match array.data_type() {
                DataType::Utf8 => {
                    serde_json::from_str::<String>(raw).ok().as_deref()
                        == Some(
                            array
                                .as_any()
                                .downcast_ref::<StringArray>()
                                .context("string type")?
                                .value(*index),
                        )
                }
                DataType::Int64 => {
                    raw.parse::<i64>().ok()
                        == Some(
                            array
                                .as_any()
                                .downcast_ref::<Int64Array>()
                                .context("i64 type")?
                                .value(*index),
                        )
                }
                DataType::Int32 => {
                    raw.parse::<i32>().ok()
                        == Some(
                            array
                                .as_any()
                                .downcast_ref::<Int32Array>()
                                .context("i32 type")?
                                .value(*index),
                        )
                }
                DataType::Float64 => {
                    raw.parse::<f64>().ok().map(f64::to_bits)
                        == Some(
                            array
                                .as_any()
                                .downcast_ref::<Float64Array>()
                                .context("f64 type")?
                                .value(*index)
                                .to_bits(),
                        )
                }
                _ => false,
            };
            ensure!(result, "DuckDB value differs");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn tool() -> Option<Tool> {
        let Some(path) = std::env::var_os("NAVIGATOR_TEST_DUCKDB_BIN") else {
            assert_ne!(
                std::env::var("NAVIGATOR_REQUIRE_DUCKDB").as_deref(),
                Ok("1"),
                "required real DuckDB fixture missing"
            );
            eprintln!("real DuckDB fixture unavailable: set NAVIGATOR_TEST_DUCKDB_BIN");
            return None;
        };
        Some(Tool::verified(Path::new(&path)).unwrap())
    }

    fn batch() -> RecordBatch {
        // Encoding fixtures only: no astronomical calculation or personal input is represented.
        let schema = crate::ephemeris_tables::schema();
        let arrays = schema
            .fields()
            .iter()
            .map(|field| -> Arc<dyn Array> {
                match field.data_type() {
                    DataType::Utf8 => Arc::new(StringArray::from(match field.name().as_str() {
                        "reference_calendar" => vec!["1900-01-01", "1900-01-01"],
                        "reference_scale" => vec!["ut1", "ut1"],
                        "delta_t_kind" => vec!["modeled", "modeled"],
                        "body" => vec!["moon", "mean_node"],
                        _ => panic!("fixture string field"),
                    })),
                    DataType::Int64 => Arc::new(Int64Array::from(vec![
                        -3_155_716_799_999_999_999_i64,
                        9_007_199_254_740_993_i64,
                    ])),
                    DataType::Int32 => Arc::new(Int32Array::from(vec![Some(301), None])),
                    DataType::Float64 => Arc::new(Float64Array::from(
                        if matches!(
                            field.name().as_str(),
                            "julian_day_utc" | "delta_t_reported_error_seconds"
                        ) {
                            vec![None, None]
                        } else if field.is_nullable() {
                            vec![Some(123_456.789_012_345_67), None]
                        } else {
                            vec![Some(-0.0), Some(f64::from_bits(0x3ff0_0000_0000_0001))]
                        },
                    )),
                    _ => panic!("fixture physical type"),
                }
            })
            .collect();
        RecordBatch::try_new(schema, arrays).unwrap()
    }

    fn reference(batch: &RecordBatch) -> Reference<'_> {
        Reference {
            snapshot_id: "synthetic-reference-content-id",
            manifest: br#"{"representationFixture":true,"model":"quoted 'model'","clock":"UT1","units":"degrees/km"}"#,
            source: br#"{"representationFixture":true,"source":"DE440 identity retained separately"}"#,
            batch, from: NaiveDate::from_ymd_opt(1900, 1, 1).unwrap(), to: NaiveDate::from_ymd_opt(1900, 1, 2).unwrap(),
        }
    }

    async fn authored(tool: &Tool, dir: &Path, reference: &Reference<'_>) {
        write_new(
            &dir.join("reference.parquet"),
            &archives::parquet_io::encode_parquet(reference.batch).unwrap(),
        )
        .unwrap();
        tool.author(dir, reference).await.unwrap();
    }

    fn rewrite_receipt(dir: &Path, update: impl FnOnce(&mut Content)) {
        let mut receipt: Receipt =
            serde_json::from_slice(&read_bounded(&dir.join(RECEIPT), MAX_MANIFEST_BYTES).unwrap())
                .unwrap();
        update(&mut receipt.content);
        receipt.artifact_id = digest(&serde_json::to_vec(&receipt.content).unwrap());
        std::fs::write(
            dir.join(RECEIPT),
            serde_json::to_vec_pretty(&receipt).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn real_tool_materializes_precise_portable_reference_without_source_files() {
        let Some(tool) = tool() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let batch = batch();
        let reference = reference(&batch);
        authored(&tool, dir.path(), &reference).await;
        std::fs::remove_file(dir.path().join("reference.parquet")).unwrap();
        let moved = dir.path().join("moved");
        std::fs::create_dir(&moved).unwrap();
        let before = [DATABASE, QUERY, RECEIPT].map(|name| {
            let bytes = std::fs::read(dir.path().join(name)).unwrap();
            std::fs::rename(dir.path().join(name), moved.join(name)).unwrap();
            (name, bytes)
        });
        tool.verify(&moved, &reference).await.unwrap();
        for (name, bytes) in before {
            assert_eq!(std::fs::read(moved.join(name)).unwrap(), bytes);
        }
    }

    #[tokio::test]
    async fn approved_foreign_creator_is_portable_but_unknown_creator_is_refused() {
        let Some(tool) = tool() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let batch = batch();
        let reference = reference(&batch);
        authored(&tool, dir.path(), &reference).await;
        // The actual reader is verified independently. Native layout is not tied to its creator's host.
        let foreign = tool
            .pins
            .binaries
            .iter()
            .find(|p| p.platform != tool.pin.platform)
            .unwrap()
            .clone();
        rewrite_receipt(dir.path(), |content| content.binary = foreign);
        tool.verify(dir.path(), &reference)
            .await
            .expect("an approved creator does not restrict the verified reader's host");
        rewrite_receipt(dir.path(), |content| {
            content.binary.binary_sha256 = "unknown-creator".into();
        });
        assert!(tool.verify(dir.path(), &reference).await.is_err());
    }

    #[tokio::test]
    async fn provenance_nullability_mutation_is_refused_even_with_valid_hashes() {
        let Some(tool) = tool() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let batch = batch();
        let reference = reference(&batch);
        authored(&tool, dir.path(), &reference).await;
        let database = dir.path().join(DATABASE);
        let setup = bootstrap(
            &database,
            Some(&dir.path().join("reference.parquet")),
            &tool.pins.storage_version,
        )
        .unwrap();
        tool.process(dir.path(), &setup, "ALTER TABLE reference_provenance ALTER reference_manifest_json DROP NOT NULL;CHECKPOINT;").await.unwrap();
        no_sidecars(&database).unwrap();
        let bytes = std::fs::read(&database).unwrap();
        rewrite_receipt(dir.path(), |content| {
            content.files[0] = file_identity(DATABASE, &bytes).unwrap();
        });
        assert!(
            tool.verify(dir.path(), &reference).await.is_err(),
            "unchanged provenance values cannot excuse a changed physical schema"
        );
    }

    #[tokio::test]
    async fn combined_setup_and_query_are_bounded_before_spawn() {
        let Some(tool) = tool() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let setup = format!("--{}\n", "x".repeat(STDIN_CAP));
        assert!(
            tool.process(dir.path(), &setup, "SELECT 1;")
                .await
                .err()
                .is_some_and(|error| error.to_string().contains("SQL exceeds its bound")),
            "setup belongs to the same bounded input as query"
        );
    }

    #[test]
    fn sql_and_staging_paths_never_enter_process_arguments() {
        let Some(tool) = tool() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let command = tool.command(dir.path());
        assert!(
            command
                .as_std()
                .get_args()
                .all(|arg| arg != dir.path().as_os_str()
                    && !arg.to_string_lossy().contains("SQL_PRIVATE_STAGE_SENTINEL")),
            "SQL must travel through bounded stdin"
        );
    }

    #[tokio::test]
    async fn deadlines_reap_the_actual_engine_and_output_errors_are_static() {
        let Some(tool) = tool() else { return };
        let home = tempfile::tempdir().unwrap();
        let mut command = tool.command(home.path());
        let child = command
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        let error = capture_child(
            child,
            "SELECT sum(sin(i)) FROM range(1000000000000) t(i);",
            Duration::from_millis(25),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "DuckDB operation failed or exceeded its resource bounds"
        );
        let status = std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(
            !status.success(),
            "timed-out engine must be reaped before stage cleanup"
        );
        let error = tool
            .process(
                home.path(),
                "",
                "SELECT 'SQL_PRIVATE_VALUE' FROM absent_private_table;",
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "DuckDB operation failed or exceeded its resource bounds"
        );
        let error = tool
            .process(home.path(), "", "SELECT repeat('x', 5000000);")
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "DuckDB operation failed or exceeded its resource bounds"
        );
    }

    #[tokio::test]
    async fn guarded_attach_refuses_external_files_and_self_consistent_extra_views() {
        let Some(tool) = tool() else { return };
        let dir = tempfile::tempdir().unwrap();
        let batch = batch();
        let reference = reference(&batch);
        authored(&tool, dir.path(), &reference).await;
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("outside.csv");
        std::fs::write(&secret, "marker\nSYNTHETIC_OUTSIDE_MARKER\n").unwrap();
        let database = dir.path().join(DATABASE);
        let guarded = bootstrap(&database, None, &tool.pins.storage_version).unwrap();
        let read = format!(
            "SELECT * FROM read_csv_auto({});",
            path_literal(&secret).unwrap()
        );
        assert!(tool.process(dir.path(), &guarded, &read).await.is_err());
        assert!(tool
            .process(dir.path(), &guarded, "INSTALL httpfs;")
            .await
            .is_err());
        assert!(tool
            .process(dir.path(), &guarded, "LOAD httpfs;")
            .await
            .is_err());
        assert!(tool
            .process(
                dir.path(),
                &guarded,
                "SELECT * FROM read_csv_auto('http://127.0.0.1:9/not-allowed');"
            )
            .await
            .is_err());
        let setup = bootstrap(
            &database,
            Some(&dir.path().join("reference.parquet")),
            &tool.pins.storage_version,
        )
        .unwrap();
        tool.process(
            dir.path(),
            &setup,
            "CREATE VIEW unexpected AS SELECT 1 AS marker;CHECKPOINT;",
        )
        .await
        .unwrap();
        let bytes = std::fs::read(&database).unwrap();
        rewrite_receipt(dir.path(), |content| {
            content.files[0] = file_identity(DATABASE, &bytes).unwrap();
        });
        assert!(
            tool.verify(dir.path(), &reference).await.is_err(),
            "matching file hashes do not authorize an external view"
        );
        assert_eq!(
            std::fs::read(&secret).unwrap(),
            b"marker\nSYNTHETIC_OUTSIDE_MARKER\n"
        );
    }

    #[tokio::test]
    async fn executable_is_private_and_startup_files_are_not_loaded() {
        use std::os::unix::fs::symlink;
        let Some(original) = tool() else { return };
        let operator = tempfile::tempdir().unwrap();
        let path = operator.path().join("operator-tool");
        std::fs::copy(&original.executable, &path).unwrap();
        let verified = Tool::verified(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"not an executable").unwrap();
        let link = operator.path().join("linked-tool");
        symlink(&original.executable, &link).unwrap();
        assert!(Tool::verified(&link).is_err());
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join(".duckdbrc"),
            "SELECT 'SYNTHETIC_INIT_MARKER';",
        )
        .unwrap();
        let bytes = verified
            .process(home.path(), "", "SELECT version() AS version;")
            .await
            .unwrap();
        let version: Vec<Version> = decode(&bytes).unwrap();
        assert_eq!(version.len(), 1);
        assert_eq!(version[0].version, verified.pins.engine_version);
        assert!(!String::from_utf8_lossy(&bytes).contains("SYNTHETIC_INIT_MARKER"));
        assert_eq!(std::fs::read(path).unwrap(), b"not an executable");
        assert_eq!(
            std::fs::metadata(verified.directory.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn profile_frames_reject_invalid_metrics_and_exclude_the_independent_warmup() {
        let frame = |seconds: &str| {
            format!(
                r#"{{"query":{{"total_time":{seconds},"sql":"PRIVATE_SQL_PATH_SENTINEL"}},"io":{{"total_bytes_read":17}},"operator":{{"private":"omitted"}}}}"#
            )
        };
        let profile = frame("0.25");
        assert_eq!(
            decode_profiles(profile.as_bytes(), 1).unwrap()[0]
                .io
                .total_bytes_read,
            17
        );
        for (bytes, count) in [
            (Vec::new(), 1),
            (profile.as_bytes().to_vec(), 0),
            (profile.as_bytes().to_vec(), 6),
            (format!("{profile}{profile}").into_bytes(), 1),
            (frame("-1").into_bytes(), 1),
            (frame("NaN").into_bytes(), 1),
            (profile.replace("17", "-17").into_bytes(), 1),
            (vec![b' '; STDOUT_CAP + 1], 1),
            (b"{PRIVATE_SQL_PATH_SENTINEL".to_vec(), 1),
        ] {
            assert_eq!(
                decode_profiles(&bytes, count).err().unwrap().to_string(),
                "invalid bounded query profile"
            );
        }
        let mut frames = frame("999");
        for sample in [5, 1, 4, 2, 3] {
            frames.push_str(&frame(&sample.to_string()));
        }
        let warm = without_warmup(decode_profiles(frames.as_bytes(), 6).unwrap()).unwrap();
        let summary =
            summarize(&warm.iter().map(|p| p.query.total_time).collect::<Vec<_>>()).unwrap();
        assert_eq!(summary.samples, 5);
        assert_eq!(summary.nearest_rank_p50.to_bits(), 3.0_f64.to_bits());
        assert_eq!(summary.nearest_rank_p95.to_bits(), 5.0_f64.to_bits());
        for invalid in [vec![], vec![-1.0], vec![f64::NAN], vec![f64::INFINITY]] {
            assert!(summarize(&invalid).is_err());
        }
    }

    #[tokio::test]
    async fn actual_measurements_verify_predicates_processes_and_immutable_files() {
        let Some(tool) = tool() else { return };
        let base = batch();
        let from = NaiveDate::from_ymd_opt(1900, 1, 1).unwrap();
        let mut chunks = Vec::new();
        for day in 0..34 {
            let date = from + chrono::Duration::days(day);
            let mut columns = base.columns().to_vec();
            columns[0] = Arc::new(StringArray::from(vec![date.to_string(); 2]));
            chunks.push(RecordBatch::try_new(base.schema(), columns).unwrap());
        }
        let batch = arrow::compute::concat_batches(&base.schema(), chunks.iter()).unwrap();
        let mut reference = reference(&batch);
        reference.to = from + chrono::Duration::days(34);
        let dir = tempfile::tempdir().unwrap();
        authored(&tool, dir.path(), &reference).await;
        let capture = || {
            std::fs::read_dir(dir.path())
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    let metadata = entry.metadata().unwrap();
                    (
                        entry.file_name(),
                        (
                            std::fs::read(entry.path()).unwrap(),
                            metadata.permissions().mode(),
                            metadata.modified().unwrap(),
                        ),
                    )
                })
                .collect::<BTreeMap<_, _>>()
        };
        let before = capture();
        let observed = tool.measure(dir.path(), &reference).await.unwrap();
        assert_eq!(observed.samples_per_phase, 5);
        assert_eq!(observed.excluded_warmup_samples, 1);
        assert_eq!(observed.time_predicate_days, 31);
        assert_eq!(
            observed
                .cases
                .iter()
                .map(|c| (c.predicate, c.matching_rows))
                .collect::<Vec<_>>(),
            [
                ("all", 68),
                ("moon", 34),
                ("first_window", 62),
                ("moon_first_window", 31)
            ]
        );
        for case in &observed.cases {
            assert_eq!(case.fresh_process_engine_seconds.samples, 5);
            assert_eq!(case.fresh_process_driver_seconds.samples, 5);
            assert_eq!(case.same_process_warm_engine_seconds.samples, 5);
            assert_eq!(case.fresh_engine_reported_read_bytes.len(), 5);
            assert_eq!(case.warm_engine_reported_read_bytes.len(), 5);
            assert!(case.fresh_process_ids.iter().all(Option::is_some));
            assert_eq!(
                case.fresh_process_ids.iter().collect::<BTreeSet<_>>().len(),
                5
            );
            assert!(case.warm_process_id.is_some());
            assert!(!case.fresh_process_ids.contains(&case.warm_process_id));
        }
        let report = serde_json::to_string(&observed).unwrap();
        for forbidden in [
            dir.path().to_str().unwrap(),
            "sql",
            "operator",
            "process_id",
            "PRIVATE_SQL_PATH_SENTINEL",
        ] {
            assert!(!report.contains(forbidden));
        }
        assert_eq!(
            capture(),
            before,
            "measurement must preserve bytes, modes, modification times and file inventory"
        );
    }
}
