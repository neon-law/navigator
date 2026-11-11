//! Operational `SurrealDB` exports and restore drills.
//!
//! This lane is deliberately separate from `archives`: the latter writes
//! analytical Parquet snapshots, while this module emits a `SurrealQL` export a
//! responder can import to recover the operational store. Routine exports
//! are marked best-effort; strict exports require the operator to assert that
//! application writers are quiesced for the duration. Counts and schema
//! fingerprints are bound to the artifact by its receipt digest.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use store::surreal::{SurrealConfig, SurrealDb};
use uuid::Uuid;

const ARCHIVE_PREFIX: &str = "surreal-backups";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ArchiveReceipt {
    format_version: u32,
    consistency: String,
    export_bytes: u64,
    export_sha256: String,
    schema_sha256: String,
    table_counts: BTreeMap<String, i64>,
}

/// Write an operational `SurrealQL` export to the configured archive lane.
pub fn export() -> Result<()> {
    runtime().block_on(async {
        let strict = strict_consistency_requested()?;
        let config = SurrealConfig::from_env().context("read SurrealDB coordinates")?;
        let db = connect_http(&config).await?;
        let (counts, schema_sha256) = archive_inventory(&db).await?;
        let bytes = export_bytes(&db).await?;
        if strict {
            let second_export = export_bytes(&db).await?;
            let after = archive_inventory(&db).await?;
            verify_strict_snapshot(&counts, &schema_sha256, &bytes, &after, &second_export)?;
        }
        let receipt = ArchiveReceipt {
            format_version: 1,
            consistency: if strict {
                "operator_quiesced"
            } else {
                "best_effort"
            }
            .into(),
            export_bytes: bytes.len() as u64,
            export_sha256: sha256(&bytes),
            schema_sha256: schema_sha256.clone(),
            table_counts: counts,
        };
        let key = archive_key(&config);
        let storage = cloud::surreal_archives_from_env()
            .await
            .context("open Surreal archive storage")?;
        storage
            .put(&key, &bytes, "application/surrealql")
            .await
            .with_context(|| format!("upload SurrealDB export {key}"))?;
        storage
            .put(
                &receipt_key(&key),
                &serde_json::to_vec(&receipt)?,
                "application/json",
            )
            .await
            .with_context(|| format!("upload SurrealDB export receipt for {key}"))?;

        println!("surreal archive written: {key} ({} bytes)", bytes.len());
        Ok(())
    })
}

/// Restore one export into a one-off namespace and prove it is recoverable.
pub fn restore_drill(key: &str) -> Result<()> {
    runtime().block_on(async {
        let config = SurrealConfig::from_env().context("read SurrealDB coordinates")?;
        let storage = cloud::surreal_archives_from_env()
            .await
            .context("open Surreal archive storage")?;
        let export = storage
            .get(key)
            .await
            .with_context(|| format!("download SurrealDB export {key}"))?;
        let has_receipt = has_receipt(storage.head(&receipt_key(key)).await, key)?;
        let (receipt, legacy) = if has_receipt {
            let receipt_object = storage.get(&receipt_key(key)).await
                .with_context(|| format!("download SurrealDB export receipt for {key}"))?;
            (serde_json::from_slice(&receipt_object.bytes).context("parse SurrealDB export receipt")?, false)
        } else {
            let source = connect_http(&config).await?;
            let (table_counts, schema_sha256) = archive_inventory(&source).await?;
            (ArchiveReceipt { format_version: 0, consistency: "legacy_live_source_comparison".into(),
                export_bytes: export.bytes.len() as u64, export_sha256: sha256(&export.bytes),
                schema_sha256, table_counts }, true)
        };
        if !legacy {
            verify_receipt(&export.bytes, &receipt)?;
        }

        let file = tempfile::NamedTempFile::new().context("create restore-drill file")?;
        // An older export may still carry the derived tables; importing one
        // fails (the view is defined before its source), so drop them.
        let export = without_derived_tables(&export.bytes)?;
        std::fs::write(file.path(), export).context("write downloaded SurrealDB export")?;

        let scratch_namespace = format!("restore_{}", Uuid::now_v7().simple());
        let scratch_config = SurrealConfig {
            namespace: scratch_namespace.clone(),
            ..config
        };
        let scratch = connect_http(&scratch_config).await?;
        let result = restore_and_verify(&scratch, file.path(), &receipt).await;
        let cleanup = remove_namespace(&scratch, &scratch_namespace).await;
        result?;
        cleanup?;

        if legacy {
            println!("legacy Surreal restore drill passed: {key} (no export-time receipt; not cutover parity evidence)");
        } else {
            println!("surreal restore drill passed: {key} ({})", consistency_label(&receipt.consistency)?);
        }
        Ok(())
    })
}

async fn restore_and_verify(
    scratch: &SurrealDb,
    file: &Path,
    receipt: &ArchiveReceipt,
) -> Result<()> {
    scratch
        .import(file)
        .await
        .context("import SurrealDB export")?;
    store::schema::apply(scratch)
        .await
        .context("apply Navigator schema to restored namespace")?;
    let version = store::schema::installed_version(scratch)
        .await
        .context("read restored schema_version:current")?;
    if version != Some(store::schema::SCHEMA_VERSION) {
        bail!(
            "restored schema_version:current is {version:?}, expected {}",
            store::schema::SCHEMA_VERSION
        );
    }

    let (actual, schema_hash) = archive_inventory(scratch).await?;
    if actual != receipt.table_counts || schema_hash != receipt.schema_sha256 {
        bail!("restored database differs from the export-time receipt");
    }
    Ok(())
}

async fn export_bytes(db: &SurrealDb) -> Result<Vec<u8>> {
    let mut stream = db.export(()).await.context("start SurrealDB export")?;
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        bytes.extend_from_slice(&chunk.context("read SurrealDB export stream")?);
    }
    if bytes.is_empty() {
        bail!("SurrealDB export returned no bytes");
    }
    without_derived_tables(&bytes)
}

/// An export's bytes with every derived table's definition and rows removed.
fn without_derived_tables(export: &[u8]) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(export).context("SurrealDB export is not UTF-8")?;
    Ok(store::schema::strip_derived_tables(text).into_bytes())
}

async fn table_counts(db: &SurrealDb) -> Result<BTreeMap<String, i64>> {
    let mut counts = BTreeMap::new();
    for table in live_table_names(db).await? {
        // `GROUP ALL` makes one count of the table; without it `count()` is
        // one `1` per row, which is not a single value to read back. An
        // empty table yields no row at all.
        let mut response = db
            .query("SELECT VALUE count() FROM type::table($table) GROUP ALL")
            .bind(("table", table.clone()))
            .await
            .with_context(|| format!("count rows in {table}"))?;
        let count: Vec<i64> = response
            .take(0)
            .with_context(|| format!("read row count for {table}"))?;
        counts.insert(table, count.first().copied().unwrap_or_default());
    }
    Ok(counts)
}

async fn live_table_names(db: &SurrealDb) -> Result<Vec<String>> {
    let info = read_database_info(db).await?;
    let inventory = info
        .get("tables")
        .and_then(serde_json::Value::as_object)
        .context("SurrealDB INFO FOR DB omitted its tables inventory")?;
    let mut tables: std::collections::BTreeSet<String> =
        store::schema::table_names().into_iter().collect();
    if let Some(table) = inventory.keys().find(|table| !safe_identifier(table)) {
        bail!("live SurrealDB table name is outside the supported identifier form: {table:?}");
    }
    tables.extend(inventory.keys().cloned());
    Ok(tables.into_iter().collect())
}

async fn archive_inventory(db: &SurrealDb) -> Result<(BTreeMap<String, i64>, String)> {
    let database_info = read_database_info(db).await?;
    let tables = live_table_names(db).await?;
    let mut complete_schema = serde_json::Map::new();
    complete_schema.insert("database".into(), database_info);
    for table in &tables {
        let quoted = format!("`{table}`");
        let mut response = db
            .query(format!("INFO FOR TABLE {quoted}"))
            .await
            .with_context(|| format!("read schema for SurrealDB table {table}"))?;
        let rows: Vec<serde_json::Value> = response
            .take(0)
            .with_context(|| format!("decode schema for SurrealDB table {table}"))?;
        complete_schema.insert(table.clone(), serde_json::Value::Array(rows));
    }
    let schema_sha256 = sha256(&serde_json::to_vec(&complete_schema)?);
    Ok((table_counts(db).await?, schema_sha256))
}

async fn read_database_info(db: &SurrealDb) -> Result<serde_json::Value> {
    let mut response = db
        .query("INFO FOR DB")
        .await
        .context("read SurrealDB database schema")?;
    let rows: Vec<serde_json::Value> = response
        .take(0)
        .context("decode SurrealDB database schema")?;
    let info = rows
        .into_iter()
        .next()
        .context("SurrealDB returned no database schema inventory")?;
    if !info.get("tables").is_some_and(serde_json::Value::is_object) {
        bail!("SurrealDB returned no live table inventory");
    }
    Ok(info)
}

fn sha256(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut hex = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        write!(&mut hex, "{byte:02x}").expect("writing a digest to String cannot fail");
    }
    hex
}

fn receipt_key(key: &str) -> String {
    format!("{key}.manifest.json")
}

fn has_receipt(
    head: Result<Option<cloud::ObjectHead>, cloud::StorageError>,
    key: &str,
) -> Result<bool> {
    Ok(head
        .with_context(|| format!("check SurrealDB export receipt for {key}"))?
        .is_some())
}

fn strict_consistency_requested() -> Result<bool> {
    match std::env::var("NAVIGATOR_SURREAL_ARCHIVE_QUIESCED") {
        Ok(value) if value == "true" => Ok(true),
        Ok(value) if value == "false" => Ok(false),
        Ok(_) => bail!("NAVIGATOR_SURREAL_ARCHIVE_QUIESCED must be 'true' or 'false'"),
        Err(std::env::VarError::NotPresent) => Ok(false),
        Err(error) => Err(error).context("read archive consistency setting"),
    }
}

fn safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn verify_receipt(export: &[u8], receipt: &ArchiveReceipt) -> Result<()> {
    if receipt.format_version != 1
        || consistency_label(&receipt.consistency).is_err()
        || receipt.export_bytes != export.len() as u64
        || receipt.export_sha256 != sha256(export)
    {
        bail!("SurrealDB export does not match its archive receipt");
    }
    Ok(())
}

fn consistency_label(consistency: &str) -> Result<&'static str> {
    match consistency {
        "operator_quiesced" => Ok("operator-quiesced strict snapshot"),
        "best_effort" => Ok("best-effort recovery snapshot"),
        _ => bail!("unknown archive receipt consistency level"),
    }
}

fn verify_strict_snapshot(
    before_counts: &BTreeMap<String, i64>,
    before_schema: &str,
    first_export: &[u8],
    after: &(BTreeMap<String, i64>, String),
    second_export: &[u8],
) -> Result<()> {
    if &after.0 != before_counts
        || after.1 != before_schema
        || sha256(second_export) != sha256(first_export)
    {
        bail!("SurrealDB changed during strict archive export; counts, schema, and full export bytes must remain stable");
    }
    Ok(())
}

async fn remove_namespace(db: &SurrealDb, namespace: &str) -> Result<()> {
    db.query(format!("REMOVE NAMESPACE {namespace}"))
        .await
        .with_context(|| format!("remove restore-drill namespace {namespace}"))?;
    Ok(())
}

async fn connect_http(config: &SurrealConfig) -> Result<SurrealDb> {
    let endpoint = http_endpoint(&config.endpoint)?;
    store::surreal::connect(&SurrealConfig {
        endpoint,
        ..config.clone()
    })
    .await
    .context("connect to SurrealDB's HTTP backup endpoint")
}

fn http_endpoint(endpoint: &str) -> Result<String> {
    if let Some(rest) = endpoint.strip_prefix("ws://") {
        return Ok(format!("http://{rest}"));
    }
    if let Some(rest) = endpoint.strip_prefix("wss://") {
        return Ok(format!("https://{rest}"));
    }
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        return Ok(endpoint.to_string());
    }
    bail!(
        "SurrealDB archive needs a ws://, wss://, http://, or https:// endpoint; got {endpoint:?}"
    )
}

fn archive_key(config: &SurrealConfig) -> String {
    let timestamp = Utc::now().format("%Y-%m-%dT%H-%M-%SZ");
    format!(
        "{ARCHIVE_PREFIX}/{}/{}/{}-{}.surql",
        key_component(&config.namespace),
        key_component(&config.database),
        timestamp,
        Uuid::now_v7().simple()
    )
}

fn key_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().expect("create Surreal archive runtime")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use uuid::Uuid;

    use super::{
        archive_inventory, archive_key, export_bytes, http_endpoint, key_component,
        restore_and_verify, table_counts, verify_strict_snapshot, ArchiveReceipt,
    };
    use store::surreal::{SurrealAuth, SurrealConfig};

    #[test]
    fn backup_uses_the_http_surface_for_every_remote_scheme() {
        assert_eq!(
            http_endpoint("ws://localhost:8000").unwrap(),
            "http://localhost:8000"
        );
        assert_eq!(
            http_endpoint("wss://example.surreal.cloud").unwrap(),
            "https://example.surreal.cloud"
        );
        assert_eq!(
            http_endpoint("http://localhost:8000").unwrap(),
            "http://localhost:8000"
        );
        assert_eq!(
            http_endpoint("https://example.surreal.cloud").unwrap(),
            "https://example.surreal.cloud"
        );
        assert!(http_endpoint("mem://").is_err());
        assert!(http_endpoint("file:///tmp/surreal").is_err());
    }

    #[test]
    fn object_keys_are_timestamped_and_safe_to_select_by_point_in_time() {
        let config = SurrealConfig {
            endpoint: "ws://localhost:8000".into(),
            namespace: "firm/archive".into(),
            database: "navigator db".into(),
            auth: SurrealAuth::default(),
        };
        let key = archive_key(&config);
        assert!(key.starts_with("surreal-backups/firm_archive/navigator_db/"));
        assert!(Path::new(&key)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("surql")));
        assert_eq!(key_component("firm/archive"), "firm_archive");
    }

    #[test]
    fn archive_receipt_detects_bytes_changed_after_export() {
        let bytes = b"DEFINE TABLE person SCHEMALESS;";
        let receipt = ArchiveReceipt {
            format_version: 1,
            consistency: "best_effort".into(),
            export_bytes: bytes.len() as u64,
            export_sha256: super::sha256(bytes),
            schema_sha256: "schema-hash".into(),
            table_counts: BTreeMap::new(),
        };
        assert!(super::verify_receipt(bytes, &receipt).is_ok());
        assert!(super::verify_receipt(b"different export", &receipt).is_err());
    }

    #[test]
    fn only_a_missing_receipt_selects_the_legacy_restore_path() {
        assert!(!super::has_receipt(Ok(None), "backup.surql").unwrap());
        assert!(super::has_receipt(
            Ok(Some(cloud::ObjectHead { size_bytes: 20 })),
            "backup.surql"
        )
        .unwrap());
        assert!(super::has_receipt(
            Err(cloud::StorageError::Gcs {
                key: "backup.surql.manifest.json".into(),
                message: "synthetic unavailable".into(),
            }),
            "backup.surql"
        )
        .is_err());
    }

    #[test]
    fn strict_snapshot_rejects_same_count_row_changes() {
        let counts = BTreeMap::from([("person".to_string(), 1)]);
        let schema = "same schema";
        let first = b"person: one name: before";
        let second = b"person: one name: after";
        let after = (counts.clone(), schema.to_string());
        assert!(super::verify_strict_snapshot(&counts, schema, first, &after, second).is_err());
        assert!(super::verify_strict_snapshot(&counts, schema, first, &after, first).is_ok());
    }

    #[test]
    fn row_count_reconciliation_uses_the_shipped_schema_tables() {
        let tables = store::schema::table_names();
        assert!(tables.contains(&"schema_version".to_string()));
        assert!(tables.contains(&"person".to_string()));
    }

    /// The projection is counted like any table, so a restore drill proves
    /// the rebuilt `draft` has a row for every restored Template.
    #[test]
    fn row_counts_include_the_derived_projection() {
        assert!(store::schema::table_names().contains(&"draft".to_string()));
    }

    /// Export a populated database, restore it into a fresh one, and verify
    /// it the way the drill does: the canonical rows come back, the derived
    /// projection is rebuilt with the same count, and a second apply and a
    /// second restore both hold. The export never carries the view.
    #[tokio::test]
    async fn a_restore_drill_rebuilds_the_draft_projection_it_does_not_export() {
        use store::surreal::test_support::{mem, unmigrated};
        use store::templates::{save_version, Version};

        let source = mem().await;
        for (code, title) in [("lease", "Lease"), ("lease", "Lease v2"), ("nda", "NDA")] {
            save_version(
                &source,
                None,
                code,
                Version {
                    title: title.into(),
                    respondent_type: "entity".into(),
                    asset_id: None,
                    form_code: None,
                    kind: None,
                    source_commit_sha: None,
                },
            )
            .await
            .unwrap();
        }
        let export = export_bytes(&source).await.unwrap();
        let text = String::from_utf8(export.clone()).unwrap();
        assert!(text.contains("DEFINE TABLE template"));
        assert!(
            !text.contains("TABLE: draft"),
            "the export leaves the view out"
        );

        let expected = table_counts(&source).await.unwrap();
        let (_, schema_sha256) = archive_inventory(&source).await.unwrap();
        let stable_reexport = export_bytes(&source).await.unwrap();
        verify_strict_snapshot(
            &expected,
            &schema_sha256,
            &export,
            &(expected.clone(), schema_sha256.clone()),
            &stable_reexport,
        )
        .expect("unchanged in-memory source exports identically");
        let receipt = ArchiveReceipt {
            format_version: 1,
            consistency: "best_effort".into(),
            export_bytes: export.len() as u64,
            export_sha256: super::sha256(&export),
            schema_sha256: schema_sha256.clone(),
            table_counts: expected.clone(),
        };
        assert_eq!(expected["template"], 3);
        assert_eq!(expected["draft"], 3);

        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), &export).unwrap();
        let before_mutation = table_counts(&source).await.unwrap();
        source
            .query("UPDATE template SET title = 'changed after archive' WHERE code = 'lease'")
            .await
            .unwrap();
        assert_eq!(table_counts(&source).await.unwrap(), before_mutation);
        let mutated_export = export_bytes(&source).await.unwrap();
        assert!(
            verify_strict_snapshot(
                &expected,
                &schema_sha256,
                &export,
                &(before_mutation.clone(), schema_sha256.clone()),
                &mutated_export,
            )
            .is_err(),
            "a same-count row update must fail strict consistency"
        );
        source
            .query("DEFINE FIELD archive_probe ON TABLE template TYPE string")
            .await
            .unwrap();
        let schema_changed = archive_inventory(&source).await.unwrap();
        assert_eq!(schema_changed.0, expected);
        assert_ne!(schema_changed.1, schema_sha256);
        assert!(
            verify_strict_snapshot(
                &before_mutation,
                &schema_sha256,
                &export,
                &schema_changed,
                &mutated_export,
            )
            .is_err(),
            "a same-count schema change must fail strict consistency"
        );
        for _ in 0..2 {
            let scratch = unmigrated().await;
            restore_and_verify(&scratch, file.path(), &receipt)
                .await
                .unwrap();
        }
    }

    /// The same drill over a real server, through the HTTP export and
    /// import the operational lane uses. Skips without
    /// `NAVIGATOR_SURREAL_ENDPOINT`; `NAV_REQUIRE_SURREAL=1` makes a skip a
    /// failure, as in CI.
    #[tokio::test]
    async fn a_server_backed_restore_drill_rebuilds_the_draft_projection() {
        use store::surreal::SurrealConfig;
        use store::templates::{save_version, Version};

        let database = format!("draft_drill_{}", Uuid::now_v7().simple());
        let Some(server) = store::test_support::server_surreal(&database).await else {
            return;
        };
        for (code, title) in [("lease", "Lease"), ("lease", "Lease v2"), ("nda", "NDA")] {
            save_version(
                &server.db,
                None,
                code,
                Version {
                    title: title.into(),
                    respondent_type: "entity".into(),
                    asset_id: None,
                    form_code: None,
                    kind: None,
                    source_commit_sha: None,
                },
            )
            .await
            .unwrap();
        }
        let source = super::connect_http(&server.config).await.unwrap();
        let export = export_bytes(&source).await.unwrap();
        assert!(!String::from_utf8_lossy(&export).contains("TABLE: draft"));
        let expected = table_counts(&source).await.unwrap();
        let (_, schema_sha256) = archive_inventory(&source).await.unwrap();
        let second_export = export_bytes(&source).await.unwrap();
        let after = archive_inventory(&source).await.unwrap();
        verify_strict_snapshot(&expected, &schema_sha256, &export, &after, &second_export)
            .expect("unchanged server source exports identically over HTTP");
        let receipt = ArchiveReceipt {
            format_version: 1,
            consistency: "best_effort".into(),
            export_bytes: export.len() as u64,
            export_sha256: super::sha256(&export),
            schema_sha256,
            table_counts: expected.clone(),
        };
        assert_eq!(expected["draft"], expected["template"]);
        assert_eq!(expected["template"], 3);

        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), &export).unwrap();
        source
            .query("UPDATE template SET title = 'changed after archive' WHERE code = 'lease'")
            .await
            .unwrap();
        assert_eq!(table_counts(&source).await.unwrap(), expected);
        let scratch_namespace = format!("restore_{}", Uuid::now_v7().simple());
        let scratch = super::connect_http(&SurrealConfig {
            namespace: scratch_namespace.clone(),
            ..server.config.clone()
        })
        .await
        .unwrap();
        let result = restore_and_verify(&scratch, file.path(), &receipt).await;
        super::remove_namespace(&scratch, &scratch_namespace)
            .await
            .unwrap();
        result.unwrap();
    }
}
