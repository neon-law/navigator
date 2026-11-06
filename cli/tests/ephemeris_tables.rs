//! The local table command authorizes before opening any local reference artifact.

use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn cached_owner_cannot_open_sources_or_publish_without_server_authorization() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app/api/ephemeris/sources"))
        .and(header("authorization", "Bearer synthetic-denied-token"))
        .respond_with(ResponseTemplate::new(403))
        .expect(1)
        .mount(&server)
        .await;
    let parent = tempfile::tempdir().unwrap();
    let credentials = parent.path().join("credentials.json");
    std::fs::write(
        &credentials,
        serde_json::to_vec(&serde_json::json!({"hosts": {server.uri(): {
            "token": "synthetic-denied-token", "role": "owner", "expires_at": i64::MAX
        }}}))
        .unwrap(),
    )
    .unwrap();
    let destination = parent.path().join("output");
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_navigator"))
        .args([
            "ephemeris",
            "tables",
            "--host",
            &server.uri(),
            "--source-dir",
        ])
        .arg(parent.path().join("missing-source"))
        .args([
            "--from",
            "1900-01-01",
            "--to-exclusive",
            "1900-01-02",
            "--dir",
        ])
        .arg(&destination)
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("HTTP 403"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!destination.exists());
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 1);
}

fn pinned_source_root() -> Option<std::path::PathBuf> {
    let root = std::env::var_os("NAVIGATOR_EPHEMERIS_DIR").map(std::path::PathBuf::from);
    if root.as_ref().is_some_and(|p| p.join("de440.bsp").is_file()) {
        return root;
    }
    assert_ne!(
        std::env::var("NAVIGATOR_REQUIRE_TABLE_BUNDLE_KERNELS").as_deref(),
        Ok("1"),
        "required DE440 source unavailable"
    );
    eprintln!("DE440 bundle roundtrip skipped: set NAVIGATOR_EPHEMERIS_DIR");
    None
}

fn source_manifest() -> serde_json::Value {
    let mut manifest: ephemeris::EphemerisManifest =
        serde_json::from_str(include_str!("../../ephemeris/manifest.json")).unwrap();
    manifest.files.retain(|f| f.name == "de440.bsp");
    serde_json::to_value(manifest).unwrap()
}

async fn invoke(
    server: &MockServer,
    credentials: &std::path::Path,
    source: &std::path::Path,
    dir: &std::path::Path,
    from: &str,
    to: &str,
) -> std::process::Output {
    tokio::process::Command::new(env!("CARGO_BIN_EXE_navigator"))
        .args([
            "ephemeris",
            "tables",
            "--host",
            &server.uri(),
            "--source-dir",
        ])
        .arg(source)
        .args(["--from", from, "--to-exclusive", to, "--dir"])
        .arg(dir)
        .env("NAVIGATOR_CREDENTIALS_FILE", credentials)
        .output()
        .await
        .unwrap()
}

fn assert_roundtrip(dir: &std::path::Path, table: &ephemeris::EphemerisReferenceTable) {
    use arrow::datatypes::DataType;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    let reader = ParquetRecordBatchReaderBuilder::try_new(
        std::fs::File::open(dir.join("reference.parquet")).unwrap(),
    )
    .unwrap();
    let schema = reader.schema().clone();
    assert_eq!(schema.fields().len(), 20);
    assert_eq!(
        schema
            .field_with_name("tt_nanoseconds_since_j2000")
            .unwrap()
            .data_type(),
        &DataType::Int64
    );
    for name in ["julian_day_utc", "delta_t_reported_error_seconds"] {
        let field = schema.field_with_name(name).unwrap();
        assert_eq!(field.data_type(), &DataType::Float64);
        assert!(field.is_nullable());
    }
    let mut csv = csv::Reader::from_path(dir.join("reference.csv")).unwrap();
    assert_eq!(
        csv.headers().unwrap().iter().collect::<Vec<_>>(),
        schema
            .fields()
            .iter()
            .map(|f| f.name().as_str())
            .collect::<Vec<_>>()
    );
    let records = csv.records().collect::<Result<Vec<_>, _>>().unwrap();
    let mut row = 0;
    for batch in reader.build().unwrap() {
        let batch = batch.unwrap();
        for i in 0..batch.num_rows() {
            assert_row_values(&batch, i, &records[row], &table.rows[row]);
            row += 1;
        }
    }
    assert_eq!(row, table.rows.len());
    assert_eq!(records.len(), row);
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("derived-manifest.json")).unwrap()).unwrap();
    for name in ["reference.parquet", "reference.csv", "source-manifest.json"] {
        assert!(std::fs::metadata(dir.join(name)).unwrap().len() <= 4_194_304);
    }
    assert!(
        std::fs::metadata(dir.join("derived-manifest.json"))
            .unwrap()
            .len()
            <= 65_536
    );
    assert_content_identity(&manifest);
    assert_eq!(manifest["content"]["row_count"], row);
    assert_eq!(
        manifest["content"]["metadata"]["interpolation_supported"],
        false
    );
    assert!(manifest["content"]["metadata"]
        .get("source_manifest")
        .is_none());
    for forbidden in [
        "chart_id",
        "birth_time",
        "location",
        "timezone",
        "consent",
        "project_id",
        "request",
        "cache",
    ] {
        assert!(!serde_json::to_string(&manifest)
            .unwrap()
            .contains(&format!("\"{forbidden}\"")));
    }
}

fn assert_row_values(
    batch: &arrow::array::RecordBatch,
    i: usize,
    record: &csv::StringRecord,
    expected_row: &ephemeris::EphemerisReferenceRow,
) {
    use arrow::array::{Array, Float64Array, Int32Array, Int64Array, StringArray};
    use arrow::datatypes::DataType;
    let expected = serde_json::to_value(expected_row).unwrap();
    for (j, field) in batch.schema().fields().iter().enumerate() {
        assert_eq!(field.metadata()["PARQUET:field_id"], (j + 1).to_string());
        let a = batch.column(j);
        let value = &expected[field.name()];
        assert_eq!(a.is_null(i), value.is_null(), "{}", field.name());
        if value.is_null() {
            assert_eq!(&record[j], "");
            continue;
        }
        match field.data_type() {
            DataType::Int64 => {
                let actual = a.as_any().downcast_ref::<Int64Array>().unwrap().value(i);
                assert_eq!(actual, value.as_i64().unwrap());
                assert_eq!(record[j].parse::<i64>().unwrap(), actual);
                assert!(actual.unsigned_abs() > (1_u64 << 53));
            }
            DataType::Int32 => {
                let actual = a.as_any().downcast_ref::<Int32Array>().unwrap().value(i);
                assert_eq!(i64::from(actual), value.as_i64().unwrap());
                assert_eq!(record[j].parse::<i32>().unwrap(), actual);
            }
            DataType::Float64 => {
                let actual = a.as_any().downcast_ref::<Float64Array>().unwrap().value(i);
                assert_eq!(
                    actual.to_bits(),
                    value.as_f64().unwrap().to_bits(),
                    "{}",
                    field.name()
                );
                assert_eq!(
                    record[j].parse::<f64>().unwrap().to_bits(),
                    actual.to_bits()
                );
            }
            DataType::Utf8 => {
                let actual = a.as_any().downcast_ref::<StringArray>().unwrap().value(i);
                let expected = if field.name() == "reference_calendar" {
                    expected_row.reference_calendar.to_string()
                } else {
                    value.as_str().unwrap().to_owned()
                };
                assert_eq!(actual, expected);
                assert_eq!(&record[j], actual);
            }
            _ => panic!("unexpected reference type"),
        }
    }
}

#[tokio::test]
async fn real_command_publishes_typed_exact_immutable_reference_files() {
    let Some(root) = pinned_source_root() else {
        return;
    };
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app/api/ephemeris/sources"))
        .respond_with(ResponseTemplate::new(200).set_body_json(source_manifest()))
        .mount(&server)
        .await;
    let parent = tempfile::tempdir().unwrap();
    let credentials = parent.path().join("credentials.json");
    std::fs::write(
        &credentials,
        serde_json::to_vec(&serde_json::json!({"hosts": {server.uri(): {
            "token": "synthetic-admin-token", "role": "client", "expires_at": i64::MAX
        }}}))
        .unwrap(),
    )
    .unwrap();
    let source = parent.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::hard_link(root.join("de440.bsp"), source.join("de440.bsp")).unwrap();
    let manifest: ephemeris::EphemerisManifest = serde_json::from_value(source_manifest()).unwrap();
    std::fs::write(
        source.join("source-manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let original = std::fs::metadata(source.join("de440.bsp")).unwrap();
    let original_hash = file_hash(&source.join("de440.bsp"));
    for (from, to) in [
        ("1900-01-01", "1900-01-03"),
        ("1971-12-31", "1972-01-02"),
        ("2016-12-31", "2017-01-02"),
        ("1904-01-01", "1905-01-01"),
    ] {
        let destination = parent.path().join(from);
        let output = invoke(&server, &credentials, &source, &destination, from, to).await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let table = ephemeris::EphemerisReferenceTable::calculate(
            &ephemeris::FilesystemStore::new(&source),
            from.parse().unwrap(),
            to.parse().unwrap(),
        )
        .unwrap();
        assert_roundtrip(&destination, &table);
        assert_immutable_rerun(&server, &credentials, &source, &destination, from, to).await;
    }
    verify_refusals(&server, &credentials, &source, parent.path()).await;
    assert_eq!(file_hash(&source.join("de440.bsp")), original_hash);
    let after = std::fs::metadata(source.join("de440.bsp")).unwrap();
    assert_eq!(after.len(), original.len());
    assert_eq!(after.modified().unwrap(), original.modified().unwrap());
    assert!(server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|r| r.url.path() == "/app/api/ephemeris/sources"));
}

async fn assert_immutable_rerun(
    server: &MockServer,
    credentials: &std::path::Path,
    source: &std::path::Path,
    destination: &std::path::Path,
    from: &str,
    to: &str,
) {
    let before = std::fs::read(destination.join("derived-manifest.json")).unwrap();
    let modified = std::fs::metadata(destination.join("reference.parquet"))
        .unwrap()
        .modified()
        .unwrap();
    assert!(invoke(server, credentials, source, destination, from, to)
        .await
        .status
        .success());
    assert_eq!(
        std::fs::read(destination.join("derived-manifest.json")).unwrap(),
        before
    );
    assert_eq!(
        std::fs::metadata(destination.join("reference.parquet"))
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
    assert!(!invoke(
        server,
        credentials,
        source,
        destination,
        from,
        &from
            .parse::<chrono::NaiveDate>()
            .unwrap()
            .succ_opt()
            .unwrap()
            .to_string()
    )
    .await
    .status
    .success());
    assert_eq!(
        std::fs::read(destination.join("derived-manifest.json")).unwrap(),
        before
    );
    std::fs::write(
        destination.join("reference.parquet"),
        b"corrupt preserved file",
    )
    .unwrap();
    assert!(!invoke(server, credentials, source, destination, from, to)
        .await
        .status
        .success());
    assert_eq!(
        std::fs::read(destination.join("reference.parquet")).unwrap(),
        b"corrupt preserved file"
    );
}

#[tokio::test]
async fn invalid_catalogue_never_opens_local_table_paths() {
    for body in [
        b"{truncated".to_vec(),
        vec![b' '; 65_537],
        serde_json::to_vec(&serde_json::json!({"version": 999})).unwrap(),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/ephemeris/sources"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .expect(1)
            .mount(&server)
            .await;
        let parent = tempfile::tempdir().unwrap();
        let credentials = parent.path().join("credentials.json");
        std::fs::write(
            &credentials,
            serde_json::to_vec(&serde_json::json!({"hosts": {server.uri(): {
                "token": "synthetic-admin-token", "role": "owner", "expires_at": i64::MAX
            }}}))
            .unwrap(),
        )
        .unwrap();
        let source = parent.path().join("missing-source");
        let destination = parent.path().join("output");
        let output = invoke(
            &server,
            &credentials,
            &source,
            &destination,
            "1900-01-01",
            "1900-01-02",
        )
        .await;
        assert!(!output.status.success());
        assert!(!destination.exists());
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 1);
    }
}

fn file_hash(path: &std::path::Path) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    Sha256::digest(std::fs::read(path).unwrap()).to_vec()
}

fn update_identity(manifest: &mut serde_json::Value) {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let mut content = manifest["content"].clone();
    content.sort_all_objects();
    let hash = Sha256::digest(serde_json::to_vec(&content).unwrap());
    manifest["snapshot_id"] = hash
        .iter()
        .fold(String::new(), |mut s, b| {
            write!(s, "{b:02x}").unwrap();
            s
        })
        .into();
}

async fn verify_refusals(
    server: &MockServer,
    credentials: &std::path::Path,
    source: &std::path::Path,
    parent: &std::path::Path,
) {
    use std::os::unix::fs::symlink;
    let base = parent.join("verified");
    assert!(invoke(
        server,
        credentials,
        source,
        &base,
        "1900-01-01",
        "1900-01-03"
    )
    .await
    .status
    .success());
    for case in ["unknown", "schema", "model", "partial", "extra", "link"] {
        let dir = parent.join(case);
        std::fs::create_dir(&dir).unwrap();
        for name in [
            "derived-manifest.json",
            "source-manifest.json",
            "reference.csv",
            "reference.parquet",
        ] {
            std::fs::copy(base.join(name), dir.join(name)).unwrap();
        }
        let manifest_path = dir.join("derived-manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        match case {
            "unknown" => manifest["unexpected_field"] = true.into(),
            "schema" => manifest["content"]["columns"][3]["type"] = "Float64".into(),
            "model" => manifest["content"]["metadata"]["atmospheric_refraction"] = true.into(),
            "partial" => {
                std::fs::remove_file(dir.join("reference.csv")).unwrap();
            }
            "extra" => {
                std::fs::write(dir.join("unrelated"), b"preserve").unwrap();
            }
            "link" => {
                std::fs::remove_file(dir.join("reference.parquet")).unwrap();
                symlink(
                    base.join("reference.parquet"),
                    dir.join("reference.parquet"),
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        update_identity(&mut manifest);
        let before = serde_json::to_vec_pretty(&manifest).unwrap();
        std::fs::write(&manifest_path, &before).unwrap();
        let output = invoke(
            server,
            credentials,
            source,
            &dir,
            "1900-01-01",
            "1900-01-03",
        )
        .await;
        assert!(!output.status.success(), "accepted {case}");
        assert_eq!(std::fs::read(&manifest_path).unwrap(), before);
    }
    verify_path_refusals(server, credentials, source, parent, &base).await;
}

async fn verify_path_refusals(
    server: &MockServer,
    credentials: &std::path::Path,
    source: &std::path::Path,
    parent: &std::path::Path,
    base: &std::path::Path,
) {
    use std::os::unix::fs::symlink;
    let linked = parent.join("linked-output");
    symlink(base, &linked).unwrap();
    assert!(!invoke(
        server,
        credentials,
        source,
        &linked,
        "1900-01-01",
        "1900-01-03"
    )
    .await
    .status
    .success());
    assert!(std::fs::symlink_metadata(&linked)
        .unwrap()
        .file_type()
        .is_symlink());
    let missing = parent.join("missing-source");
    for (from, to) in [
        ("1900-01-01", "1900-01-01"),
        ("1899-12-31", "1900-01-01"),
        ("2000-01-01", "2001-01-02"),
    ] {
        let dir = parent.join("invalid-range");
        let output = invoke(server, credentials, &missing, &dir, from, to).await;
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("invalid bounded reference interval")
        );
        assert!(!dir.exists());
    }
    let kernel = source.join("de440.bsp");
    let held = source.join("held-kernel");
    std::fs::rename(&kernel, &held).unwrap();
    symlink(&held, &kernel).unwrap();
    // Keep exactly the expected inventory so the ordinary-file gate itself must refuse the link.
    let held_outside = parent.join("held-kernel");
    std::fs::rename(&held, &held_outside).unwrap();
    std::fs::remove_file(&kernel).unwrap();
    symlink(&held_outside, &kernel).unwrap();
    assert!(!invoke(
        server,
        credentials,
        source,
        &parent.join("linked-source-output"),
        "1900-01-01",
        "1900-01-03"
    )
    .await
    .status
    .success());
    std::fs::remove_file(&kernel).unwrap();
    std::fs::rename(&held_outside, &kernel).unwrap();
    assert!(!std::fs::read_dir(parent).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".navigator-reference-")));
}

fn assert_content_identity(manifest: &serde_json::Value) {
    let mut reordered = manifest.clone();
    let metadata = reordered["content"]["metadata"].as_object_mut().unwrap();
    let entries: Vec<_> = std::mem::take(metadata).into_iter().rev().collect();
    metadata.extend(entries);
    reordered["generated_at"] = "2099-01-01T00:00:00Z".into();
    update_identity(&mut reordered);
    assert_eq!(
        reordered["snapshot_id"], manifest["snapshot_id"],
        "content identity must ignore JSON object order and generation time"
    );
}
