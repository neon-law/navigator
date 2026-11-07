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
    Ok(format!("SET autoinstall_known_extensions=false;SET autoload_known_extensions=false;SET threads=1;SET memory_limit='128MB';SET temp_directory='';SET allow_persistent_secrets=false;{load}SET allowed_paths=[{}];SET enable_external_access=false;SET lock_configuration=true;ATTACH {} AS reference ({mode});USE reference;", paths.join(","), path_literal(database)?))
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
    let child = command
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| anyhow::anyhow!("cannot start the verified DuckDB executable"))?;
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
        let length = setup
            .len()
            .checked_add(sql.len())
            .and_then(|n| n.checked_add(1))
            .context("DuckDB SQL exceeds its bound")?;
        ensure!(length <= STDIN_CAP, "DuckDB SQL exceeds its bound");
        let script = format!("{setup}\n{sql}");
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
        Ok(())
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
}
