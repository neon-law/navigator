//! Explicit native logical rehydration. The backup writer never calls this module.
//!
//! A separately authorized reader supplies the bound paired marker. The target
//! must come from opaque native creation; application admission remains pending.
use crate::coverage::{DatabaseManifest, PairedManifest, RelationKind, TableSchema};
use crate::postgres::{sha256_hex, PostgresTableEntry};
use anyhow::{bail, ensure, Context, Result};
use arrow_array58::{Array as _, BooleanArray, Int32Array, Int64Array, RecordBatch, StringArray};
use cloud::StorageService;
use futures::{StreamExt as _, TryStreamExt as _};
use sqlx::{AssertSqlSafe, Postgres, Transaction};
use std::collections::BTreeSet;
use store::postgres::native_origin::recovery::PendingRecovery;
use store::postgres::native_origin::recovery_plan::RecoveryPlan;

const MAX_OBJECT: u64 = 64 * 1024 * 1024;
const MAX_RELATION: u64 = 128 * 1024 * 1024;
const MAX_OBJECTS: usize = 128;

fn ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}
fn authority(name: &str) -> bool {
    [
        "database_origin",
        "database_origin_upgrade",
        "database_recovery",
        "_sqlx_migrations",
    ]
    .contains(&name)
}
fn stage(name: &str) -> String {
    format!("archive_source_{name}")
}

/// Decode and restore Navigator rows into its pending newly created resource.
/// This does not activate, grant connections, restore Forgejo, or accept external
/// document/Git/LFS bytes. The caller then invokes independent store completion.
///
/// Pinned native schema and the pinned DBOS SDK's schema must already exist;
/// this function never executes archive-supplied DDL or starts workflow workers.
/// Unknown schemas/types/catalog closure refuse. Relational writes roll back together;
/// sequence setval is nontransactional and a failed target remains pending.
///
/// # Errors
/// Unknown manifest, hash/schema/coverage drift, malformed Iceberg/Parquet,
/// unsupported source/target relation, or any database write/constraint failure.
pub async fn rehydrate(
    pending: &PendingRecovery,
    storage: &dyn StorageService,
    base: &str,
) -> Result<()> {
    ensure!(
        ["memory://", "gs://", "file:///"]
            .iter()
            .any(|prefix| base.starts_with(prefix))
            && !base.contains(".."),
        "unsupported archive URI"
    );
    let bytes:Vec<u8>=sqlx::query_scalar("SELECT manifest_bytes FROM public.database_recovery WHERE singleton AND completed_at IS NULL")
        .fetch_one(pending.postgres().pool()).await?;
    // Run the production store parser before any restoration write.
    let _ = store::postgres::native_origin::recovery::RecoveryInput::from_archive(
        uuid::Uuid::new_v4(),
        bytes.clone(),
    )?;
    let envelope: serde_json::Value = serde_json::from_slice(&bytes)?;
    let manifest: PairedManifest =
        serde_json::from_value(envelope.get("paired").unwrap_or(&envelope).clone())?;
    ensure!(
        manifest.format_version == 1
            && !manifest.cross_database_atomic
            && manifest.databases.len() == 2,
        "unsupported paired recovery format"
    );
    let db = &manifest.databases[0];
    ensure!(db.database == "navigator", "Navigator source required");
    ensure!(
        db.tables.len() == db.schemas.len() && db.tables.len() == db.iceberg_metadata_keys.len(),
        "incomplete recovery relation inventory"
    );
    ensure!(
        db.column_schema_sha256 == sha256_hex(&serde_json::to_vec(&db.schemas)?),
        "column schema hash differs"
    );
    let plan = db
        .recovery_plan
        .as_ref()
        .context("native recovery plan missing")?;
    let mut tx = pending.postgres().pool().begin().await?;
    sqlx::raw_sql("SET LOCAL TimeZone='UTC'; SET LOCAL DateStyle='ISO, YMD'; SET LOCAL extra_float_digits=3; SET LOCAL bytea_output='hex'; SET LOCAL IntervalStyle='iso_8601'; SET LOCAL lc_monetary='C'; SET LOCAL search_path=pg_catalog") .execute(&mut *tx).await?;
    // One catalog snapshot before writes; includes empty and inaccessible relations.
    let actual = crate::coverage::inventory(&mut tx).await?;
    db.coverage.verify(&actual)?;
    let entries = ordered_entries(&mut tx, db, plan).await?;
    let role: String = sqlx::query_scalar("SELECT current_user::text")
        .fetch_one(&mut *tx)
        .await?;
    ensure!(plan.large_objects.iter().all(|(_,owner,acl)|owner==&role && acl.is_none()),"this recovery version requires large-object owner to equal operator and NULL ACL; other policies refuse before archive writes");
    let mut largeobjects = BTreeSet::new();
    for (oid, _, _) in &plan.large_objects {
        let oid = u32::try_from(*oid)?;
        ensure!(
            largeobjects.insert(oid),
            "duplicate large-object metadata identity"
        );
        sqlx::query("SELECT pg_catalog.lo_create($1::oid)")
            .bind(i64::from(oid))
            .execute(&mut *tx)
            .await?;
    }
    let context = RestoreContext {
        db,
        storage,
        base,
        generation: manifest.generation,
        plan,
    };
    for ((entry, schema), metadata) in entries {
        context
            .restore_relation(&mut tx, entry, schema, metadata, &mut largeobjects)
            .await?;
    }
    validate_source_staging(&mut tx, plan).await?;
    sqlx::raw_sql("SET LOCAL search_path=pg_catalog,public")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

type ExportRelation<'a> = ((&'a PostgresTableEntry, &'a TableSchema), &'a String);

async fn ordered_entries<'a>(
    tx: &mut Transaction<'_, Postgres>,
    db: &'a DatabaseManifest,
    plan: &RecoveryPlan,
) -> Result<Vec<ExportRelation<'a>>> {
    let mut entries = db
        .tables
        .iter()
        .zip(&db.schemas)
        .zip(&db.iceberg_metadata_keys)
        .collect::<Vec<_>>();
    let expected_exports = plan
        .relations
        .iter()
        .filter(|r| r.kind != "v")
        .map(|r| (&r.schema, &r.name))
        .collect::<BTreeSet<_>>();
    let actual_exports = db
        .schemas
        .iter()
        .map(|s| (&s.relation.schema, &s.relation.name))
        .collect::<BTreeSet<_>>();
    ensure!(
        expected_exports == actual_exports && actual_exports.len() == db.schemas.len(),
        "complete stored relation coverage required"
    );
    let dependencies:Vec<(String,String,String,String)>=sqlx::query_as("SELECT nc.nspname::text,c.relname::text,np.nspname::text,p.relname::text FROM pg_catalog.pg_constraint f JOIN pg_catalog.pg_class c ON c.oid=f.conrelid JOIN pg_catalog.pg_namespace nc ON nc.oid=c.relnamespace JOIN pg_catalog.pg_class p ON p.oid=f.confrelid JOIN pg_catalog.pg_namespace np ON np.oid=p.relnamespace WHERE f.contype='f' AND NOT f.condeferrable AND f.conrelid<>f.confrelid").fetch_all(&mut **tx).await?;
    let mut ordered = Vec::new();
    while !entries.is_empty() {
        let found = entries.iter().position(|((_, s), _)| {
            dependencies.iter().all(|(cs, cn, ps, pn)| {
                cs != &s.relation.schema
                    || cn != &s.relation.name
                    || !entries
                        .iter()
                        .any(|((_, p), _)| &p.relation.schema == ps && &p.relation.name == pn)
            })
        });
        let Some(i) = found else {
            bail!("nondeferrable FK cycle requires its reviewed reconstruction procedure");
        };
        ordered.push(entries.remove(i));
    }
    sqlx::raw_sql("SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut **tx)
        .await?;
    Ok(ordered)
}

struct RestoreContext<'a> {
    db: &'a DatabaseManifest,
    storage: &'a dyn StorageService,
    base: &'a str,
    generation: uuid::Uuid,
    plan: &'a RecoveryPlan,
}

impl RestoreContext<'_> {
    async fn restore_relation(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        entry: &PostgresTableEntry,
        schema: &TableSchema,
        metadata: &str,
        largeobjects: &mut BTreeSet<u32>,
    ) -> Result<()> {
        ensure!(
            entry.table == format!("{}.{}", schema.relation.schema, schema.relation.name),
            "relation entry identity differs"
        );
        let expected = self
            .plan
            .relations
            .iter()
            .find(|r| r.schema == schema.relation.schema && r.name == schema.relation.name)
            .context("unclassified recovery relation")?;
        ensure!(
            expected.columns == schema.columns && expected.rows == entry.rows as u64,
            "relation shape/count differs"
        );
        let columns:Vec<(String,String)>=sqlx::query_as("SELECT a.attname::text,pg_catalog.format_type(a.atttypid,a.atttypmod) FROM pg_catalog.pg_attribute a WHERE a.attrelid=pg_catalog.to_regclass(pg_catalog.quote_ident($1)||'.'||pg_catalog.quote_ident($2)) AND a.attnum>0 AND NOT a.attisdropped ORDER BY a.attnum")
            .bind(&schema.relation.schema).bind(&schema.relation.name).fetch_all(&mut **tx).await?;
        ensure!(
            columns == schema.columns,
            "target columns differ; schema transforms are not inferred"
        );
        let metadata_table = schema.relation.schema == "public" && authority(&schema.relation.name);
        let ordinary = matches!(
            schema.relation.kind,
            RelationKind::Table | RelationKind::PartitionedTable
        ) && !["pg_catalog", "ag_catalog"]
            .contains(&schema.relation.schema.as_str());
        ensure!(
            schema.relation.kind != RelationKind::MaterializedView,
            "materialized-view reconstruction requires its reviewed native procedure"
        );
        let staging = if metadata_table {
            stage(&schema.relation.name)
        } else {
            "archive_relation_rows".to_owned()
        };
        if ordinary {
            sqlx::raw_sql(AssertSqlSafe(format!(
                "CREATE TEMP TABLE {} (LIKE {}.{}) ON COMMIT DROP",
                ident(&staging),
                ident(&schema.relation.schema),
                ident(&schema.relation.name)
            )))
            .execute(&mut **tx)
            .await?;
        }
        let target = RelationTarget {
            entry,
            schema,
            staging,
            ordinary,
        };
        let mut batches = read_table(
            self.db,
            entry,
            metadata,
            self.storage,
            self.base,
            self.generation,
        )
        .await?;
        let mut rows = 0usize;
        while let Some(batch) = batches.try_next().await? {
            rows = rows
                .checked_add(batch.num_rows())
                .context("restored row count overflow")?;
            ensure!(
                rows <= entry.rows && batch.num_rows() <= 65536,
                "decoded rows exceed bounded recovery plan"
            );
            target.restore_batch(tx, &batch, largeobjects).await?;
        }
        ensure!(rows == entry.rows, "decoded row count differs");
        if ordinary && !metadata_table {
            copy_staged_relation(tx, schema, &target.staging).await?;
        }
        Ok(())
    }
}

struct RelationTarget<'a> {
    entry: &'a PostgresTableEntry,
    schema: &'a TableSchema,
    staging: String,
    ordinary: bool,
}

impl RelationTarget<'_> {
    async fn restore_batch(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        batch: &RecordBatch,
        largeobjects: &mut BTreeSet<u32>,
    ) -> Result<()> {
        let entry = self.entry;
        let schema = self.schema;
        let staging = &self.staging;
        let ordinary = self.ordinary;
        match (
            schema.relation.schema.as_str(),
            schema.relation.name.as_str(),
            schema.relation.kind,
        ) {
            ("ag_catalog", _, _) | ("pg_catalog", "pg_largeobject_metadata", _) => {}
            ("pg_catalog", "pg_largeobject", _) => {
                restore_largeobjects(tx, batch, largeobjects).await?;
            }
            (_, _, RelationKind::Sequence) => {
                ensure!(
                    entry.rows == 1 && batch.num_rows() == 1,
                    "sequence state missing"
                );
                let value = batch
                    .column_by_name("last_value")
                    .and_then(|v| v.as_any().downcast_ref::<Int64Array>())
                    .context("sequence value type")?
                    .value(0);
                let called = batch
                    .column_by_name("is_called")
                    .and_then(|v| v.as_any().downcast_ref::<BooleanArray>())
                    .context("sequence called type")?
                    .value(0);
                let name = format!(
                    "{}.{}",
                    ident(&schema.relation.schema),
                    ident(&schema.relation.name)
                );
                sqlx::query("SELECT pg_catalog.setval($1::regclass,$2,$3)")
                    .bind(name)
                    .bind(value)
                    .bind(called)
                    .execute(&mut **tx)
                    .await?;
            }
            (_, _, RelationKind::Table | RelationKind::PartitionedTable) if ordinary => {
                insert_rows(tx, schema, batch, "pg_temp", staging).await?;
            }
            _ => bail!("unsupported stored relation"),
        }
        Ok(())
    }
}

async fn copy_staged_relation(
    tx: &mut Transaction<'_, Postgres>,
    schema: &TableSchema,
    staging: &str,
) -> Result<()> {
    let generated:Vec<String>=sqlx::query_scalar("SELECT a.attname::text FROM pg_catalog.pg_attribute a WHERE a.attrelid=pg_catalog.to_regclass(pg_catalog.quote_ident($1)||'.'||pg_catalog.quote_ident($2)) AND a.attgenerated<>'' AND NOT a.attisdropped")
                .bind(&schema.relation.schema).bind(&schema.relation.name).fetch_all(&mut **tx).await?;
    let columns = schema
        .columns
        .iter()
        .filter(|(name, _)| !generated.contains(name))
        .map(|(name, _)| ident(name))
        .collect::<Vec<_>>()
        .join(",");
    let fixed = schema.relation.schema == "public"
        && ["spatial_ref_sys", "astrocartography_reference_geometry"]
            .contains(&schema.relation.name.as_str());
    let conflict = if fixed { " ON CONFLICT DO NOTHING" } else { "" };
    // Canonical row triggers resolve their public relations on the
    // private target; catalog fingerprints retain their fixed path.
    sqlx::raw_sql("SET LOCAL search_path=pg_catalog,public")
        .execute(&mut **tx)
        .await?;
    sqlx::raw_sql(AssertSqlSafe(format!("INSERT INTO {}.{} ({columns}) OVERRIDING SYSTEM VALUE SELECT {columns} FROM pg_temp.{}{conflict}",ident(&schema.relation.schema),ident(&schema.relation.name),ident(staging)))) .execute(&mut **tx).await.with_context(|| format!("restore relation {}.{}", schema.relation.schema, schema.relation.name))?;
    sqlx::raw_sql("SET LOCAL search_path=pg_catalog")
        .execute(&mut **tx)
        .await?;
    sqlx::raw_sql(AssertSqlSafe(format!(
        "DROP TABLE pg_temp.{}",
        ident(staging)
    )))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn read_table(
    db: &DatabaseManifest,
    entry: &crate::postgres::PostgresTableEntry,
    metadata: &str,
    storage: &dyn StorageService,
    base: &str,
    generation: uuid::Uuid,
) -> Result<futures::stream::BoxStream<'static, Result<RecordBatch>>> {
    let lane = format!("postgres/{}/_paired/{generation}/", db.database);
    let components = entry.key.split('/').collect::<Vec<_>>();
    let write_only = components.len() >= 5
        && components[0] == "attempts"
        && components[1].parse::<uuid::Uuid>().is_ok();
    ensure!(
        (entry.key.starts_with(&lane) || write_only) && !entry.key.contains(".."),
        "foreign data key"
    );
    let (prefix, _) = entry.key.split_once("/data/").context("invalid data key")?;
    let prefix = format!("{prefix}/");
    ensure!(
        metadata.starts_with(&prefix) && !metadata.contains(".."),
        "foreign metadata key"
    );
    // StorageService is whole-object and whole-list, so enforce explicit caps
    // from listed sizes BEFORE downloads. A growing/misreported object can still
    // allocate inside the backend; this interface is not a streaming byte limit.
    let listed = storage.list(&prefix).await?;
    ensure!(listed.len() <= MAX_OBJECTS, "too many relation objects");
    let mut total = 0u64;
    let mut found = false;
    for object in &listed {
        ensure!(
            object.key.starts_with(&prefix)
                && !object.key.contains("..")
                && object.size_bytes <= MAX_OBJECT,
            "foreign or oversized Iceberg object"
        );
        total = total
            .checked_add(object.size_bytes)
            .context("relation size overflow")?;
        ensure!(
            total <= MAX_RELATION,
            "relation exceeds recovery memory cap"
        );
        if object.key == entry.key {
            ensure!(
                object.size_bytes == u64::try_from(entry.bytes)?,
                "listed data size differs"
            );
            found = true;
        }
    }
    ensure!(found, "data object absent from relation listing");
    let io = iceberg::io::FileIO::new_with_memory();
    for object in listed {
        let data = storage.get(&object.key).await?;
        ensure!(
            u64::try_from(data.bytes.len())? == object.size_bytes,
            "object changed after listing"
        );
        if object.key == entry.key {
            ensure!(
                entry.sha256 == sha256_hex(&data.bytes),
                "Parquet object hash differs"
            );
        }
        io.new_output(format!("{base}/{}", object.key))?
            .write(data.bytes.into())
            .await?;
    }
    let table = iceberg::table::StaticTable::from_metadata_file(
        &format!("{base}/{metadata}"),
        iceberg::TableIdent::from_strs(["recovery", "relation"])?,
        io,
    )
    .await?;
    let tasks = table.scan().build()?.plan_files().await?;
    Ok(table
        .reader_builder()
        .build()
        .read(tasks)?
        .stream()
        .map_err(anyhow::Error::from)
        .boxed())
}

async fn insert_rows(
    tx: &mut Transaction<'_, Postgres>,
    schema: &TableSchema,
    batch: &RecordBatch,
    target_schema: &str,
    target_table: &str,
) -> Result<()> {
    ensure!(
        batch.num_columns() == schema.columns.len(),
        "Parquet column count differs"
    );
    let generated:Vec<String>=sqlx::query_scalar("SELECT a.attname::text FROM pg_catalog.pg_attribute a WHERE a.attrelid=pg_catalog.to_regclass(pg_catalog.quote_ident($1)||'.'||pg_catalog.quote_ident($2)) AND a.attgenerated<>'' AND NOT a.attisdropped")
        .bind(target_schema).bind(target_table).fetch_all(&mut **tx).await?;
    let included = schema
        .columns
        .iter()
        .enumerate()
        .filter(|(_, (name, _))| !generated.contains(name))
        .collect::<Vec<_>>();
    // Type strings come only from the actual target catalog, equal-checked above.
    let columns = included
        .iter()
        .map(|(_, (name, _))| ident(name))
        .collect::<Vec<_>>()
        .join(",");
    let placeholders = included
        .iter()
        .enumerate()
        .map(|(i, (_, (_, ty)))| format!("${}::text::{ty}", i + 1))
        .collect::<Vec<_>>()
        .join(",");
    let fixed = target_schema == "public"
        && ["spatial_ref_sys", "astrocartography_reference_geometry"].contains(&target_table);
    let conflict = if fixed { " ON CONFLICT DO NOTHING" } else { "" };
    let sql = format!(
        "INSERT INTO {}.{} ({columns}) OVERRIDING SYSTEM VALUE VALUES ({placeholders}){conflict}",
        ident(target_schema),
        ident(target_table)
    );
    for row in 0..batch.num_rows() {
        let mut query = sqlx::query(AssertSqlSafe(sql.as_str()));
        for (index, (_, ty)) in &included {
            let col = batch.column(*index);
            let value = if col.is_null(row) {
                None
            } else {
                Some(arrow_cast58::display::array_value_to_string(col, row)?)
            };
            query = query.bind(if ty == "bytea" {
                value.map(|v| format!("\\x{v}"))
            } else {
                value
            });
        }
        query.execute(&mut **tx).await?;
    }
    Ok(())
}

async fn restore_largeobjects(
    tx: &mut Transaction<'_, Postgres>,
    batch: &RecordBatch,
    created: &mut BTreeSet<u32>,
) -> Result<()> {
    use arrow_array58::BinaryArray;
    for row in 0..batch.num_rows() {
        let oid = batch
            .column_by_name("loid")
            .and_then(|v| v.as_any().downcast_ref::<StringArray>())
            .context("largeobject OID type")?
            .value(row)
            .parse::<u32>()?;
        let page = batch
            .column_by_name("pageno")
            .and_then(|v| v.as_any().downcast_ref::<Int32Array>())
            .context("largeobject page type")?
            .value(row);
        let data = arrow_cast58::cast(
            batch
                .column_by_name("data")
                .context("largeobject byte column")?,
            &arrow_schema58::DataType::Binary,
        )?;
        let data = data
            .as_any()
            .downcast_ref::<BinaryArray>()
            .context("largeobject bytes type")?
            .value(row);
        ensure!(
            created.contains(&oid),
            "large-object pages have no captured metadata identity"
        );
        sqlx::query("SELECT pg_catalog.lo_put($1::oid,$2,$3)")
            .bind(i64::from(oid))
            .bind(i64::from(page) * 2048)
            .bind(data)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

async fn validate_source_staging(
    tx: &mut Transaction<'_, Postgres>,
    plan: &store::postgres::native_origin::recovery_plan::RecoveryPlan,
) -> Result<()> {
    let origin: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT to_jsonb(o) FROM pg_temp.archive_source_database_origin o")
            .fetch_all(&mut **tx)
            .await?;
    ensure!(
        origin == vec![plan.source_origin.clone()],
        "decoded Origin differs from bound lineage"
    );
    let upgrades:Vec<serde_json::Value>=sqlx::query_scalar("SELECT to_jsonb(u) FROM pg_temp.archive_source_database_origin_upgrade u ORDER BY upgrade_operation").fetch_all(&mut **tx).await?;
    ensure!(
        upgrades == plan.source_upgrades,
        "decoded upgrade history differs"
    );
    let prior:Vec<serde_json::Value>=sqlx::query_scalar("SELECT to_jsonb(r) FROM pg_temp.archive_source_database_recovery r ORDER BY recovery_operation").fetch_all(&mut **tx).await?;
    ensure!(
        store::postgres::native_origin::recovery_plan::summarize_recovery_rows(&prior)?
            == plan.prior_recovery,
        "decoded prior recovery lineage differs"
    );
    let migrations:Option<serde_json::Value>=sqlx::query_scalar("SELECT CASE WHEN bool_and(success) THEN jsonb_agg(jsonb_build_array(version,encode(checksum,'hex')) ORDER BY version) ELSE NULL END FROM pg_temp.archive_source__sqlx_migrations").fetch_one(&mut **tx).await?;
    ensure!(
        migrations.as_ref() == Some(&plan.source_migrations),
        "decoded SQLx source evidence differs"
    );
    Ok(())
}
