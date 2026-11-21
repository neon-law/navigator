//! The existing isolated Iceberg recovery drill's source-specific restore procedure.

use super::*;
use archives::coverage::{DatabaseManifest, PairedManifest, RelationKind, TableSchema};
use arrow_array58::{
    Array, BinaryArray, BooleanArray, Int32Array, Int64Array, RecordBatch, StringArray,
};
use futures::TryStreamExt;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt::Write as _;

pub(super) async fn restore_pair(
    paired: &PairedManifest,
    storage: &dyn StorageService,
    base: &str,
    navigator: &PgPool,
    forgejo: &PgPool,
    ledger: &PostgresDb,
) {
    let io = iceberg::io::FileIO::new_with_memory();
    for object in storage.list("").await.unwrap() {
        let bytes = storage.get(&object.key).await.unwrap().bytes;
        io.new_output(format!("{base}/{}", object.key))
            .unwrap()
            .write(bytes.into())
            .await
            .unwrap();
    }
    for (db, target) in paired.databases.iter().zip([navigator, forgejo]) {
        restore_database(db, target, storage, base, &io, ledger).await;
    }
}

async fn restore_database(
    db: &DatabaseManifest,
    target: &PgPool,
    storage: &dyn StorageService,
    base: &str,
    io: &iceberg::io::FileIO,
    ledger: &PostgresDb,
) {
    let mut largeobjects = BTreeSet::new();
    let mut ordered = db
        .tables
        .iter()
        .zip(&db.schemas)
        .zip(&db.iceberg_metadata_keys)
        .collect::<Vec<_>>();
    ordered.sort_by_key(|((_, schema), _)| restore_order(&schema.relation.name));
    for ((entry, schema), metadata_key) in ordered {
        let object = storage.get(&entry.key).await.unwrap();
        let hash = Sha256::digest(&object.bytes)
            .iter()
            .fold(String::new(), |mut output, byte| {
                write!(output, "{byte:02x}").unwrap();
                output
            });
        assert_eq!(entry.sha256, hash);
        let batches = read_batches(io, base, metadata_key).await;
        assert_eq!(
            batches.iter().map(RecordBatch::num_rows).sum::<usize>(),
            entry.rows
        );
        restore_relation(schema, batches, target, &mut largeobjects).await;
        if schema.relation.name == "mirror_backfill_run" && entry.rows > 0 {
            assert!(
                matches!(
                    require_scope(ledger, Family::Satellites).await,
                    Err(FamilyError::NativeOrigin(
                        store::postgres::native_origin::OriginError::Recovery
                    ))
                ),
                "partial receipt rows cannot activate an unknown unmigrated database"
            );
        }
    }
}

fn restore_order(name: &str) -> u8 {
    match name {
        "workflow_status" | "mirror_backfill_run" | "database_origin" => 0,
        "database_origin_upgrade"
        | "mirror_backfill_checkpoint"
        | "mirror_backfill_failure"
        | "person"
        | "jurisdiction"
        | "entity_type"
        | "durable_object_handover" => 1,
        "entity" | "durable_object_ownership" => 2,
        "firm" => 3,
        "brand" | "person_firm_role" | "email_receipt" => 4,
        "project" | "email_delivery" => 5,
        "project_slack_handover" | "email_delivery_attempt" => 6,
        "project_slack_delivery" | "project_slack_delivery_reconciliation" => 7,
        _ => 8,
    }
}

async fn read_batches(io: &iceberg::io::FileIO, base: &str, key: &str) -> Vec<RecordBatch> {
    let table = iceberg::table::StaticTable::from_metadata_file(
        &format!("{base}/{key}"),
        iceberg::TableIdent::from_strs(["coverage", "probe"]).unwrap(),
        io.clone(),
    )
    .await
    .unwrap();
    let tasks = table.scan().build().unwrap().plan_files().await.unwrap();
    table
        .reader_builder()
        .build()
        .read(tasks)
        .unwrap()
        .stream()
        .try_collect()
        .await
        .unwrap()
}

async fn restore_relation(
    schema: &TableSchema,
    batches: Vec<RecordBatch>,
    target: &PgPool,
    largeobjects: &mut BTreeSet<String>,
) {
    if schema.relation.schema == "pg_catalog" && schema.relation.name == "pg_largeobject" {
        restore_largeobjects(target, batches, largeobjects).await;
        return;
    }
    if ["pg_catalog", "ag_catalog"].contains(&schema.relation.schema.as_str()) {
        // Native schema and large-object procedures reconstruct catalog identities.
        return;
    }
    if schema.relation.kind == RelationKind::Sequence {
        restore_sequence(target, schema, batches).await;
        return;
    }
    let generated: BTreeSet<String>=sqlx::query_scalar("SELECT a.attname::text FROM pg_attribute a WHERE a.attrelid=to_regclass(quote_ident($1)||'.'||quote_ident($2)) AND a.attgenerated<>'' AND NOT a.attisdropped")
        .bind(&schema.relation.schema).bind(&schema.relation.name).fetch_all(target).await.unwrap().into_iter().collect();
    for batch in batches {
        restore_rows(target, schema, &batch, &generated).await;
    }
}

async fn restore_rows(
    target: &PgPool,
    schema: &TableSchema,
    batch: &RecordBatch,
    generated: &BTreeSet<String>,
) {
    let included = schema
        .columns
        .iter()
        .enumerate()
        .filter(|(_, (name, _))| !generated.contains(name))
        .collect::<Vec<_>>();
    let columns = included
        .iter()
        .map(|(_, (name, _))| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(",");
    let placeholders = included
        .iter()
        .enumerate()
        .map(|(index, (_, (_, ty)))| format!("${}::text::{ty}", index + 1))
        .collect::<Vec<_>>()
        .join(",");
    // These two sources recreate fixed rows through their authoritative native DDL.
    // Final whole-table equality (including target-only rows/generated values) is mandatory.
    let fixed = schema.relation.schema == "public"
        && matches!(
            schema.relation.name.as_str(),
            "spatial_ref_sys" | "astrocartography_reference_geometry"
        );
    let conflict = if fixed { " ON CONFLICT DO NOTHING" } else { "" };
    let statement = format!(
        "INSERT INTO \"{}\".\"{}\" ({columns}) OVERRIDING SYSTEM VALUE VALUES ({placeholders}){conflict}",
        schema.relation.schema, schema.relation.name
    );
    for row in 0..batch.num_rows() {
        let mut query = sqlx::query(sqlx::AssertSqlSafe(statement.as_str()));
        for (index, (_, pg_type)) in &included {
            let column = batch.column(*index);
            let value = (!column.is_null(row))
                .then(|| arrow_cast58::display::array_value_to_string(column, row).unwrap());
            let value = if pg_type == "bytea" {
                value.map(|hex| format!("\\x{hex}"))
            } else {
                value
            };
            query = query.bind(value);
        }
        query.execute(target).await.unwrap();
    }
}

async fn restore_sequence(target: &PgPool, schema: &TableSchema, batches: Vec<RecordBatch>) {
    for batch in batches {
        let value = batch
            .column_by_name("last_value")
            .unwrap()
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap()
            .value(0);
        let called = batch
            .column_by_name("is_called")
            .unwrap()
            .as_any()
            .downcast_ref::<BooleanArray>()
            .unwrap()
            .value(0);
        let qualified = format!(
            "\"{}\".\"{}\"",
            schema.relation.schema, schema.relation.name
        );
        sqlx::query("SELECT setval($1::regclass,$2,$3)")
            .bind(qualified)
            .bind(value)
            .bind(called)
            .execute(target)
            .await
            .unwrap();
    }
}

async fn restore_largeobjects(
    target: &PgPool,
    batches: Vec<RecordBatch>,
    restored: &mut BTreeSet<String>,
) {
    for batch in batches {
        for row in 0..batch.num_rows() {
            let oid = batch
                .column_by_name("loid")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .value(row);
            let page = batch
                .column_by_name("pageno")
                .unwrap()
                .as_any()
                .downcast_ref::<Int32Array>()
                .unwrap()
                .value(row);
            let data = arrow_cast58::cast(
                batch.column_by_name("data").unwrap(),
                &arrow_schema58::DataType::Binary,
            )
            .unwrap();
            let bytes = data
                .as_any()
                .downcast_ref::<BinaryArray>()
                .unwrap()
                .value(row);
            if restored.insert(oid.to_string()) {
                sqlx::query("SELECT lo_create($1::text::oid)")
                    .bind(oid)
                    .execute(target)
                    .await
                    .unwrap();
            }
            sqlx::query("SELECT lo_put($1::text::oid,$2,$3)")
                .bind(oid)
                .bind(i64::from(page) * 2048)
                .bind(bytes)
                .execute(target)
                .await
                .unwrap();
        }
    }
}

/// Compare every stored relation, including target-only rows and recomputed columns.
/// AGE catalogs are reconstructed by native DDL and have different physical OIDs.
pub(super) async fn assert_database_equal(db: &DatabaseManifest, source: &PgPool, target: &PgPool) {
    assert_relations_equal(db, source, target, false).await;
}

pub(super) async fn assert_native_database_equal(
    db: &DatabaseManifest,
    source: &PgPool,
    target: &PgPool,
) {
    assert_relations_equal(db, source, target, true).await;
    let evidence: Vec<u8> = sqlx::query_scalar(
        "SELECT source_identity_bytes FROM database_recovery WHERE completed_at IS NOT NULL",
    )
    .fetch_one(target)
    .await
    .unwrap();
    let retained: store::postgres::native_origin::recovery_plan::RecoveryPlan =
        serde_json::from_slice(&evidence).unwrap();
    assert_eq!(
        db.recovery_plan.as_ref(),
        Some(&retained),
        "source lineage and logical values remain exact immutable provenance"
    );
    let origin: serde_json::Value = sqlx::query_scalar("SELECT to_jsonb(o) FROM database_origin o")
        .fetch_one(source)
        .await
        .unwrap();
    assert_eq!(retained.source_origin, origin);
}

async fn assert_relations_equal(
    db: &DatabaseManifest,
    source: &PgPool,
    target: &PgPool,
    native: bool,
) {
    for schema in &db.schemas {
        if schema.relation.schema == "ag_catalog" {
            continue;
        }
        if native
            && schema.relation.schema == "public"
            && [
                "database_origin",
                "database_origin_upgrade",
                "database_recovery",
                "_sqlx_migrations",
            ]
            .contains(&schema.relation.name.as_str())
        {
            continue;
        }
        let relation = format!(
            "\"{}\".\"{}\"",
            schema.relation.schema, schema.relation.name
        );
        let statement = if schema.relation.kind == RelationKind::Sequence {
            format!(
                "SELECT md5(last_value::text || ':' || is_called::text),1::bigint FROM {relation}"
            )
        } else {
            format!("SELECT md5(COALESCE(string_agg(row_to_json(t)::text, ',' ORDER BY row_to_json(t)::text), '')),count(*)::bigint FROM ONLY {relation} t")
        };
        let original: (String, i64) = sqlx::query_as(sqlx::AssertSqlSafe(statement.as_str()))
            .fetch_one(source)
            .await
            .unwrap();
        let restored: (String, i64) = sqlx::query_as(sqlx::AssertSqlSafe(statement.as_str()))
            .fetch_one(target)
            .await
            .unwrap();
        assert_eq!(
            original, restored,
            "complete stored-table restoration for {relation}"
        );
    }
}

pub(super) async fn prepare_target_migration_ledger(source: &PgPool, target: &PgPool) {
    use sqlx::migrate::Migrate as _;
    let present: bool =
        sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations') IS NOT NULL")
            .fetch_one(source)
            .await
            .unwrap();
    if present {
        let mut connection = target.acquire().await.unwrap();
        // SQLx0.9 creates only the ledger shape; no applied versions are invented.
        connection
            .ensure_migrations_table("public._sqlx_migrations")
            .await
            .unwrap();
        let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM public._sqlx_migrations")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(
            applied, 0,
            "schema-only target must not fabricate applied migrations"
        );
    }
}

pub(super) async fn restore_forgejo(
    paired: &PairedManifest,
    storage: &dyn StorageService,
    base: &str,
    target: &PgPool,
    ledger: &PostgresDb,
) {
    let io = iceberg::io::FileIO::new_with_memory();
    for object in storage.list("").await.unwrap() {
        let bytes = storage.get(&object.key).await.unwrap().bytes;
        io.new_output(format!("{base}/{}", object.key))
            .unwrap()
            .write(bytes.into())
            .await
            .unwrap();
    }
    restore_database(&paired.databases[1], target, storage, base, &io, ledger).await;
}
