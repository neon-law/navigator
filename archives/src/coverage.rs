//! Fail-closed relation inventory for a Data Export.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};

/// Catalog identity, independent of the connection's search path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relation {
    pub schema: String,
    pub name: String,
    pub kind: RelationKind,
    pub accessible: bool,
}

/// Views have no stored rows. Every other supported kind must be exported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Table,
    PartitionedTable,
    MaterializedView,
    Sequence,
    View,
    ForeignTable,
}

/// An exclusion is exact and includes the reconstruction reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Rule {
    Export(Relation),
    Derived { relation: Relation, reason: String },
}

/// Expected inventory of one database, supplied from its reviewed schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageManifest {
    pub relations: Vec<Rule>,
}

impl Rule {
    fn relation(&self) -> &Relation {
        match self {
            Self::Export(relation) | Self::Derived { relation, .. } => relation,
        }
    }
}

impl CoverageManifest {
    /// Refuse drift, inaccessible rows, duplicate identities, and exclusions
    /// of stored data. An empty table still has an Export rule.
    pub fn verify(&self, observed: &[Relation]) -> Result<()> {
        let mut expected = BTreeMap::new();
        for rule in &self.relations {
            let relation = rule.relation();
            if expected
                .insert((&relation.schema, &relation.name), rule)
                .is_some()
            {
                bail!("duplicate coverage identity");
            }
            match rule {
                Rule::Export(relation) if !relation.accessible => {
                    bail!("export rules must require accessible stored data");
                }
                Rule::Derived { reason, .. }
                    if relation.kind != RelationKind::View || reason.trim().is_empty() =>
                {
                    bail!("only an explicitly justified ordinary view can be excluded");
                }
                Rule::Export(_)
                    if matches!(
                        relation.kind,
                        RelationKind::View | RelationKind::ForeignTable
                    ) =>
                {
                    bail!("view or foreign source requires a separate coverage contract");
                }
                _ => {}
            }
        }
        let mut seen = BTreeSet::new();
        for relation in observed {
            let identity = (&relation.schema, &relation.name);
            if !seen.insert(identity) {
                bail!("duplicate observed identity");
            }
            let Some(rule) = expected.get(&identity) else {
                bail!(
                    "unclassified relation {}.{}",
                    relation.schema,
                    relation.name
                );
            };
            if rule.relation().kind != relation.kind {
                bail!(
                    "relation kind changed for {}.{}",
                    relation.schema,
                    relation.name
                );
            }
            if matches!(rule, Rule::Export(_)) && !relation.accessible {
                bail!(
                    "inaccessible relation {}.{}",
                    relation.schema,
                    relation.name
                );
            }
        }
        if seen.len() != expected.len() {
            bail!("expected relation dropped from database");
        }
        Ok(())
    }
}

/// Read `pg_catalog`, rather than `information_schema`, so missing grants do
/// not hide persistent tables. Include extension-owned AGE catalogs and graph
/// tables, DBOS, migration ledgers, and PostgreSQL large-object storage.
pub async fn inventory(tx: &mut Transaction<'_, Postgres>) -> Result<Vec<Relation>> {
    let rows: Vec<(String, String, String, bool)> = sqlx::query_as(
        "SELECT n.nspname::text, c.relname::text, c.relkind::text,
         has_schema_privilege(n.oid, 'USAGE') AND CASE WHEN c.relkind = 'S'
         THEN has_sequence_privilege(c.oid, 'SELECT')
         ELSE has_table_privilege(c.oid, 'SELECT') AND NOT row_security_active(c.oid) END
         FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
         WHERE c.relkind IN ('r','p','m','S','v','f') AND
         ((n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname !~ '^pg_toast'
           AND n.nspname !~ '^pg_temp_')
          OR (n.nspname = 'pg_catalog' AND c.relname IN ('pg_largeobject','pg_largeobject_metadata')))
         ORDER BY n.nspname, c.relname",
    ).fetch_all(&mut **tx).await?;
    rows.into_iter()
        .map(|(schema, name, kind, accessible)| {
            let kind = match kind.as_str() {
                "r" => RelationKind::Table,
                "p" => RelationKind::PartitionedTable,
                "m" => RelationKind::MaterializedView,
                "S" => RelationKind::Sequence,
                "v" => RelationKind::View,
                "f" => RelationKind::ForeignTable,
                _ => bail!("unsupported relation kind"),
            };
            Ok(Relation {
                schema,
                name,
                kind,
                accessible,
            })
        })
        .collect()
}

/// Reviewed inventories for the two required databases. Configuration contains
/// schema identities and exclusion reasons, never row data or credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfiguredCoverage {
    pub navigator: CoverageManifest,
    pub forgejo: CoverageManifest,
}

/// One independently owned PostgreSQL database and its reviewed inventory.
pub struct DatabaseSource<'a> {
    pub name: &'a str,
    pub pool: &'a sqlx::PgPool,
    pub coverage: &'a CoverageManifest,
}

/// One source's independently acquired extraction watermark.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseManifest {
    pub database: String,
    pub database_name: String,
    pub cutoff_at: chrono::DateTime<chrono::Utc>,
    pub snapshot: String,
    pub server_version: String,
    pub extensions: Vec<(String, String)>,
    pub column_schema_sha256: String,
    pub coverage: CoverageManifest,
    pub tables: Vec<crate::postgres::PostgresTableEntry>,
    /// Sequence values are read outside MVCC. They require source-specific
    /// reconciliation before a restored database admits writes.
    pub non_mvcc_relations: Vec<Relation>,
    pub iceberg_metadata_keys: Vec<String>,
    pub schemas: Vec<TableSchema>,
}

/// Column input types needed by a source-specific schema/restore procedure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableSchema {
    pub relation: Relation,
    pub columns: Vec<(String, String)>,
}

/// Both sources completed. This is explicitly not a shared recovery point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedManifest {
    pub generation: uuid::Uuid,
    pub databases: Vec<DatabaseManifest>,
    pub cross_database_atomic: bool,
}

/// Extract both databases before writing objects, then publish the paired
/// manifest last. No source generation is published independently. Failed
/// writes may leave unreferenced objects; readers require the final manifest.
/// Git, LFS, attachments and document-object bytes require separate backups.
pub async fn export_pair(
    navigator: DatabaseSource<'_>,
    forgejo: DatabaseSource<'_>,
    storage: &dyn cloud::StorageService,
    location_base: &str,
) -> Result<PairedManifest> {
    if navigator.name != "navigator" || forgejo.name != "forgejo" {
        bail!("paired coverage requires navigator and forgejo source identities");
    }
    let generation = uuid::Uuid::new_v4();
    let mut databases = Vec::new();
    let mut objects = Vec::new();
    for source in [navigator, forgejo] {
        let extracted = extract_database(source, generation).await?;
        databases.push(extracted.manifest);
        objects.extend(extracted.objects);
    }
    if databases[0].database_name == databases[1].database_name {
        bail!("paired sources must connect to distinct databases");
    }
    for (key, bytes) in objects {
        storage
            .put(&key, &bytes, "application/vnd.apache.parquet")
            .await?;
    }
    for database in &mut databases {
        let lane = format!("postgres/{}/_paired/{generation}", database.database);
        for entry in &database.tables {
            let table = entry
                .key
                .strip_prefix(&format!("{lane}/"))
                .and_then(|key| key.split_once("/data/").map(|(table, _)| table))
                .ok_or_else(|| anyhow::anyhow!("invalid generation data key"))?;
            let authored = crate::author_iceberg_for_prefix(
                storage,
                crate::PromotionRequest {
                    lane: &lane,
                    table,
                    location_base,
                    run_date: database.cutoff_at.date_naive(),
                    data_key: Some(&entry.key),
                    snapshot_id: 1,
                    timestamp_ms: database.cutoff_at.timestamp_millis(),
                    expire_before_ms: None,
                },
            )
            .await?
            .ok_or_else(|| anyhow::anyhow!("Iceberg metadata missing"))?;
            database.iceberg_metadata_keys.push(format!(
                "{lane}/{table}/metadata/v{}.metadata.json",
                authored.version
            ));
        }
    }
    let manifest = PairedManifest {
        generation,
        databases,
        cross_database_atomic: false,
    };
    storage
        .put(
            &format!("postgres/_paired/{generation}.json"),
            &serde_json::to_vec_pretty(&manifest)?,
            "application/json",
        )
        .await?;
    Ok(manifest)
}

struct ExtractedDatabase {
    manifest: DatabaseManifest,
    objects: Vec<(String, Vec<u8>)>,
}

async fn extract_database(
    source: DatabaseSource<'_>,
    generation: uuid::Uuid,
) -> Result<ExtractedDatabase> {
    let mut objects = Vec::new();
    let mut tx = source.pool.begin().await?;
    sqlx::raw_sql("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY; SET LOCAL search_path = pg_catalog; SET LOCAL row_security = off; SET LOCAL statement_timeout = '300s'")
        .execute(&mut *tx).await?;
    let (cutoff_at, snapshot, database_name, server_version): (chrono::DateTime<chrono::Utc>, String, String, String) =
        sqlx::query_as(
            "SELECT clock_timestamp(), pg_current_snapshot()::text, current_database()::text, current_setting('server_version')",
        )
        .fetch_one(&mut *tx)
        .await?;
    source.coverage.verify(&inventory(&mut tx).await?)?;
    let extensions =
        sqlx::query_as("SELECT extname::text, extversion::text FROM pg_extension ORDER BY extname")
            .fetch_all(&mut *tx)
            .await?;
    let mut tables = Vec::new();
    let mut schemas = Vec::new();
    for rule in &source.coverage.relations {
        let Rule::Export(relation) = rule else {
            continue;
        };
        let columns: Vec<(String, String)> = sqlx::query_as(
            "SELECT a.attname::text, format_type(a.atttypid, a.atttypmod) FROM pg_attribute a
             JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = $1 AND c.relname = $2 AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum"
        ).bind(&relation.schema).bind(&relation.name).fetch_all(&mut *tx).await?;
        schemas.push(TableSchema {
            relation: relation.clone(),
            columns,
        });
        let (rows, bytes) =
            crate::postgres::export_table_in_schema(&mut tx, &relation.schema, &relation.name)
                .await?;
        let table = format!("{}.{}", relation.schema, relation.name);
        // Encode each name component to avoid object-prefix collisions.
        let key = format!(
            "postgres/{}/_paired/{generation}/{}_{}/data/dt={}/snapshot.parquet",
            source.name,
            hex_component(&relation.schema),
            hex_component(&relation.name),
            cutoff_at.date_naive()
        );
        tables.push(crate::postgres::PostgresTableEntry {
            table,
            rows,
            bytes: bytes.len(),
            key: key.clone(),
            sha256: crate::postgres::sha256_hex(&bytes),
        });
        objects.push((key, bytes));
    }
    tx.rollback().await?;
    let column_schema_sha256 = crate::postgres::sha256_hex(&serde_json::to_vec(&schemas)?);
    let manifest = DatabaseManifest {
        database: source.name.into(),
        database_name,
        cutoff_at,
        snapshot,
        server_version,
        extensions,
        column_schema_sha256,
        coverage: source.coverage.clone(),
        tables,
        non_mvcc_relations: source
            .coverage
            .relations
            .iter()
            .filter_map(|rule| match rule {
                Rule::Export(relation) if relation.kind == RelationKind::Sequence => {
                    Some(relation.clone())
                }
                _ => None,
            })
            .collect(),
        iceberg_metadata_keys: Vec::new(),
        schemas,
    };
    Ok(ExtractedDatabase { manifest, objects })
}

fn hex_component(value: &str) -> String {
    use std::fmt::Write as _;
    value
        .as_bytes()
        .iter()
        .fold(String::new(), |mut output, byte| {
            let _ = write!(output, "{byte:02x}");
            output
        })
}
