//! Snapshot-bound native recovery plans and independent target verification.
use super::{authority_contract, database_name, schema_contract, OriginError};
use futures::TryStreamExt as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use sqlx::{AssertSqlSafe, Postgres, Transaction};
use std::collections::BTreeSet;
use uuid::Uuid;

type ForeignKeyReference = (
    String,
    String,
    String,
    String,
    Vec<String>,
    Vec<String>,
    String,
);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryRelation {
    pub schema: String,
    pub name: String,
    pub kind: String,
    pub columns: Vec<(String, String)>,
    pub column_contract: Vec<Value>,
    pub view_definition: Option<String>,
    pub sequence_options: Option<Value>,
    pub rows: u64,
    pub sha256: String,
}

/// Deterministic logical values, captured in the export's actual readonly snapshot.
/// Constructing this data structure confers no target activation capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryPlan {
    pub format_version: u32,
    pub database_name: String,
    pub source_origin: Value,
    pub source_upgrades: Vec<Value>,
    pub source_migrations: Value,
    pub prior_recovery: Vec<Value>,
    pub relations: Vec<RecoveryRelation>,
    pub constraints: Vec<Value>,
    pub large_objects: Vec<(i64, String, Option<String>)>,
}

fn refusal<T>() -> Result<T, OriginError> {
    Err(OriginError::Recovery)
}
fn uuid(value: &Value, key: &str) -> Result<Uuid, OriginError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
        .ok_or(OriginError::Recovery)
}
fn prefix(older: &Value, newer: &Value) -> bool {
    match (older.as_array(), newer.as_array()) {
        (Some(a), Some(b)) => !a.is_empty() && b.starts_with(a),
        _ => false,
    }
}

impl RecoveryPlan {
    /// Use the actual exported sequence bytes, which are outside MVCC, rather
    /// than silently fingerprinting a later sequence read in the same transaction.
    pub fn bind_exported_sequence(
        &mut self,
        schema: &str,
        name: &str,
        last: i64,
        called: bool,
    ) -> Result<(), OriginError> {
        let r = self
            .relations
            .iter_mut()
            .find(|r| r.schema == schema && r.name == name && r.kind == "S")
            .ok_or(OriginError::Recovery)?;
        let value = format!("[{last}, {called}]");
        let mut hash = Sha256::new();
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
        r.rows = 1;
        r.sha256 = hex::encode(hash.finalize());
        Ok(())
    }

    /// Check actual captured migration checksums against this release independently
    /// of the target's own SQLx ledger. Newer and unknown source schemas refuse.
    pub fn validate_source(&self) -> Result<(), OriginError> {
        let o = &self.source_origin;
        let creation = uuid(o, "creation_operation")?;
        if self.format_version != 1
            || creation.is_nil()
            || o.get("format_version").and_then(Value::as_i64) != Some(1)
            || o.get("origin").and_then(Value::as_str) != Some("fresh_native")
            || o.get("database_name").and_then(Value::as_str) != Some(self.database_name.as_str())
            || self.database_name != database_name(creation)?
            || o.get("database_oid")
                .and_then(Value::as_i64)
                .is_none_or(|v| v <= 0)
            || o.get("authority_contract") != Some(&authority_contract())
            || !prefix(&self.source_migrations, &schema_contract())
        {
            return refusal();
        }
        super::TierBinding {
            tier_id: uuid(o, "tier_id")?,
            namespace_uid: uuid(o, "namespace_uid")?,
            cluster_name: o
                .get("cluster_name")
                .and_then(Value::as_str)
                .ok_or(OriginError::Recovery)?
                .into(),
        }
        .validate()?;
        let mut schema = o
            .get("schema_contract")
            .ok_or(OriginError::Recovery)?
            .clone();
        if !prefix(&schema, &self.source_migrations) {
            return refusal();
        }
        let mut remaining = self.source_upgrades.iter().collect::<Vec<_>>();
        let mut predecessor = creation;
        let mut seen = BTreeSet::from([creation]);
        while !remaining.is_empty() {
            let matching = remaining
                .iter()
                .enumerate()
                .filter(|(_, v)| uuid(v, "predecessor_operation").ok() == Some(predecessor))
                .map(|(i, _)| i)
                .collect::<Vec<_>>();
            let [i] = matching.as_slice() else {
                return refusal();
            };
            let step = remaining.remove(*i);
            let operation = uuid(step, "upgrade_operation")?;
            let next = step
                .get("to_schema_contract")
                .ok_or(OriginError::Recovery)?;
            if operation.is_nil()
                || !seen.insert(operation)
                || uuid(step, "creation_operation")? != creation
                || step.get("from_schema_contract") != Some(&schema)
                || step.get("authority_contract") != Some(&authority_contract())
                || step.get("completed_at").is_none_or(Value::is_null)
                || next == &schema
                || !prefix(&schema, next)
                || !prefix(next, &self.source_migrations)
            {
                return refusal();
            }
            schema = next.clone();
            predecessor = operation;
        }
        if schema != self.source_migrations || self.relations.is_empty() {
            return refusal();
        }
        self.validate_inventory(creation)
    }

    fn validate_inventory(&self, creation: Uuid) -> Result<(), OriginError> {
        if self.prior_recovery.len() > 1 {
            return refusal();
        }
        for prior in &self.prior_recovery {
            if uuid(prior, "creation_operation")? != creation
                || uuid(prior, "recovery_operation")?.is_nil()
            {
                return refusal();
            }
            for field in ["manifest_sha256", "verified_plan_sha256", "row_sha256"] {
                if prior.get(field).and_then(Value::as_str).is_none_or(|s| {
                    s.len() != 64
                        || !s
                            .bytes()
                            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                }) {
                    return refusal();
                }
            }
        }
        let mut identities = BTreeSet::new();
        for r in &self.relations {
            if r.schema.is_empty()
                || r.name.is_empty()
                || r.schema.contains('\0')
                || r.name.contains('\0')
                || !identities.insert((&r.schema, &r.name))
                || r.sha256.len() != 64
                || !r
                    .sha256
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                || !["r", "p", "m", "S", "v"].contains(&r.kind.as_str())
            {
                return refusal();
            }
        }
        Ok(())
    }
}

/// Quote catalog-returned identifiers; never interpolate an archive SQL type or expression.
fn ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

async fn observed(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<Vec<RecoveryRelation>, OriginError> {
    let catalog: Vec<(String,String,String,bool)> = sqlx::query_as(
        "SELECT n.nspname::text,c.relname::text,c.relkind::text,has_schema_privilege(n.oid,'USAGE') AND CASE WHEN c.relkind='S' THEN has_sequence_privilege(c.oid,'SELECT') ELSE has_table_privilege(c.oid,'SELECT') AND NOT row_security_active(c.oid) END FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE c.relkind IN ('r','p','m','S','v','f') AND ((n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname !~ '^pg_toast' AND n.nspname !~ '^pg_temp_') OR (n.nspname='pg_catalog' AND c.relname IN ('pg_largeobject','pg_largeobject_metadata'))) ORDER BY n.nspname,c.relname"
    ).fetch_all(&mut **tx).await?;
    let age: bool = sqlx::query_scalar("SELECT to_regclass('ag_catalog.ag_graph') IS NOT NULL")
        .fetch_one(&mut **tx)
        .await?;
    if age {
        let orphan:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM ag_catalog.ag_graph g LEFT JOIN pg_catalog.pg_namespace n ON n.oid=g.namespace WHERE n.oid IS NULL OR n.nspname<>g.name) OR EXISTS(SELECT 1 FROM ag_catalog.ag_label l LEFT JOIN ag_catalog.ag_graph g ON g.graphid=l.graph LEFT JOIN pg_catalog.pg_class c ON c.oid=l.relation LEFT JOIN pg_catalog.pg_namespace n ON n.oid=g.namespace LEFT JOIN pg_catalog.pg_class s ON s.relnamespace=n.oid AND s.relname=l.seq_name AND s.relkind='S' WHERE g.graphid IS NULL OR c.oid IS NULL OR n.oid IS NULL OR c.relnamespace<>n.oid OR c.relname<>l.name OR s.oid IS NULL)").fetch_one(&mut **tx).await?;
        if orphan {
            return refusal();
        }
    }
    let mut result = Vec::new();
    for (schema, name, kind, accessible) in catalog {
        if !accessible || kind == "f" {
            return refusal();
        }
        let columns = sqlx::query_as("SELECT a.attname::text,pg_catalog.format_type(a.atttypid,a.atttypmod) FROM pg_catalog.pg_attribute a WHERE a.attrelid=pg_catalog.to_regclass(pg_catalog.quote_ident($1)||'.'||pg_catalog.quote_ident($2)) AND a.attnum>0 AND NOT a.attisdropped ORDER BY a.attnum")
            .bind(&schema).bind(&name).fetch_all(&mut **tx).await?;
        let column_contract:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_array(a.attname,pg_catalog.format_type(a.atttypid,a.atttypmod),a.attnotnull,a.attidentity,a.attgenerated,a.attinhcount,a.attislocal,cn.nspname,co.collname,pg_catalog.pg_get_expr(d.adbin,d.adrelid)) FROM pg_catalog.pg_attribute a LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum LEFT JOIN pg_catalog.pg_collation co ON co.oid=a.attcollation LEFT JOIN pg_catalog.pg_namespace cn ON cn.oid=co.collnamespace WHERE a.attrelid=pg_catalog.to_regclass(pg_catalog.quote_ident($1)||'.'||pg_catalog.quote_ident($2)) AND a.attnum>0 AND NOT a.attisdropped ORDER BY a.attnum")
            .bind(&schema).bind(&name).fetch_all(&mut **tx).await?;
        let view_definition = if kind == "v" || kind == "m" {
            Some(sqlx::query_scalar("SELECT pg_catalog.pg_get_viewdef(pg_catalog.to_regclass(pg_catalog.quote_ident($1)||'.'||pg_catalog.quote_ident($2)),false)").bind(&schema).bind(&name).fetch_one(&mut **tx).await?)
        } else {
            None
        };
        let sequence_options = if kind == "S" {
            Some(sqlx::query_scalar("SELECT jsonb_build_array(seqstart,seqincrement,seqmax,seqmin,seqcache,seqcycle,seqtypid::regtype::text) FROM pg_catalog.pg_sequence WHERE seqrelid=to_regclass(quote_ident($1)||'.'||quote_ident($2))").bind(&schema).bind(&name).fetch_one(&mut **tx).await?)
        } else {
            None
        };
        let qualified = format!("{}.{}", ident(&schema), ident(&name));
        let select = match (schema.as_str(),name.as_str(),kind.as_str()) {
            (_,_,"v") => None,
            (_,_,"S") => Some(format!("SELECT jsonb_build_array(last_value,is_called)::text FROM {qualified}")),
            ("ag_catalog","ag_graph",_) => Some("SELECT jsonb_build_array(name,namespace::regnamespace::text)::text FROM ag_catalog.ag_graph".into()),
            ("ag_catalog","ag_label",_) => Some("SELECT jsonb_build_array(l.name,g.name,l.id,l.kind,l.relation::regclass::text,l.seq_name)::text FROM ag_catalog.ag_label l LEFT JOIN ag_catalog.ag_graph g ON g.graphid=l.graph".into()),
            ("pg_catalog","pg_largeobject_metadata",_) => Some("SELECT jsonb_build_array(oid,lomowner::regrole::text,lomacl::text)::text FROM pg_catalog.pg_largeobject_metadata".into()),
            _ => Some(format!("SELECT row_to_json(r)::text FROM ONLY {qualified} r")),
        };
        let mut hash = Sha256::new();
        let mut rows = 0u64;
        if let Some(select) = select {
            let statement = format!("SELECT logical_value FROM ({select}) AS recovery_values(logical_value) ORDER BY logical_value COLLATE \"C\"");
            let mut stream =
                sqlx::query_scalar::<_, String>(AssertSqlSafe(statement)).fetch(&mut **tx);
            while let Some(value) = stream.try_next().await? {
                hash.update((value.len() as u64).to_be_bytes());
                hash.update(value.as_bytes());
                rows = rows.checked_add(1).ok_or(OriginError::Recovery)?;
            }
        }
        result.push(RecoveryRelation {
            schema,
            name,
            kind,
            columns,
            column_contract,
            view_definition,
            sequence_options,
            rows,
            sha256: hex::encode(hash.finalize()),
        });
    }
    Ok(result)
}

async fn constraints(tx: &mut Transaction<'_, Postgres>) -> Result<Vec<Value>, OriginError> {
    let disabled:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_trigger t JOIN pg_catalog.pg_constraint c ON c.oid=t.tgconstraint WHERE c.contype='f' AND t.tgenabled NOT IN ('O','A'))").fetch_one(&mut **tx).await?;
    if disabled {
        return refusal();
    }
    // NOT VALID is an intentional canonical migration state. Preserve its exact
    // definition while independently checking rows; never fabricate validation.
    let references:Vec<ForeignKeyReference>=sqlx::query_as("SELECT nc.nspname::text,child.relname::text,np.nspname::text,parent.relname::text,ARRAY(SELECT a.attname::text FROM unnest(c.conkey) WITH ORDINALITY k(attnum,ord) JOIN pg_catalog.pg_attribute a ON a.attrelid=c.conrelid AND a.attnum=k.attnum ORDER BY k.ord),ARRAY(SELECT a.attname::text FROM unnest(c.confkey) WITH ORDINALITY k(attnum,ord) JOIN pg_catalog.pg_attribute a ON a.attrelid=c.confrelid AND a.attnum=k.attnum ORDER BY k.ord),c.confmatchtype::text FROM pg_catalog.pg_constraint c JOIN pg_catalog.pg_class child ON child.oid=c.conrelid JOIN pg_catalog.pg_namespace nc ON nc.oid=child.relnamespace JOIN pg_catalog.pg_class parent ON parent.oid=c.confrelid JOIN pg_catalog.pg_namespace np ON np.oid=parent.relnamespace WHERE c.contype='f' AND nc.nspname NOT IN ('pg_catalog','information_schema') AND nc.nspname !~ '^pg_temp_'").fetch_all(&mut **tx).await?;
    for (cs, ct, ps, pt, children, parents, mode) in references {
        if children.len() != parents.len()
            || children.is_empty()
            || !["s", "f"].contains(&mode.as_str())
        {
            return refusal();
        }
        let all = children
            .iter()
            .map(|c| format!("child.{} IS NOT NULL", ident(c)))
            .collect::<Vec<_>>()
            .join(" AND ");
        let any = children
            .iter()
            .map(|c| format!("child.{} IS NOT NULL", ident(c)))
            .collect::<Vec<_>>()
            .join(" OR ");
        let equality = children
            .iter()
            .zip(&parents)
            .map(|(c, p)| format!("parent.{}=child.{}", ident(p), ident(c)))
            .collect::<Vec<_>>()
            .join(" AND ");
        let absent = format!(
            "NOT EXISTS(SELECT 1 FROM {}.{} parent WHERE {equality})",
            ident(&ps),
            ident(&pt)
        );
        let invalid = if mode == "f" {
            format!("({any}) AND (NOT ({all}) OR {absent})")
        } else {
            format!("({all}) AND {absent}")
        };
        let query = format!(
            "SELECT EXISTS(SELECT 1 FROM {}.{} child WHERE {invalid})",
            ident(&cs),
            ident(&ct)
        );
        let orphan: bool = sqlx::query_scalar(AssertSqlSafe(query))
            .fetch_one(&mut **tx)
            .await?;
        if orphan {
            return refusal();
        }
    }
    let mut definitions:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_array(c.conrelid::regclass::text,c.conname,c.contype,c.condeferrable,c.condeferred,c.convalidated,pg_catalog.pg_get_constraintdef(c.oid,false)) FROM pg_catalog.pg_constraint c JOIN pg_catalog.pg_namespace n ON n.oid=c.connamespace WHERE c.conrelid<>0 AND n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname !~ '^pg_temp_' AND n.nspname !~ '^pg_toast' ORDER BY c.conrelid::regclass::text COLLATE \"C\",c.conname COLLATE \"C\"") .fetch_all(&mut **tx).await?;
    let broken_index:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_index i JOIN pg_catalog.pg_class t ON t.oid=i.indrelid JOIN pg_catalog.pg_namespace n ON n.oid=t.relnamespace WHERE n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname !~ '^pg_temp_' AND n.nspname !~ '^pg_toast' AND (NOT i.indisvalid OR NOT i.indisready OR NOT i.indislive))").fetch_one(&mut **tx).await?;
    if broken_index {
        return refusal();
    }
    let indexes:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_array('index',t.oid::regclass::text,pg_catalog.pg_get_indexdef(i.indexrelid,0,false),i.indisunique,i.indisprimary,i.indisexclusion,i.indimmediate,i.indisvalid,i.indisready) FROM pg_catalog.pg_index i JOIN pg_catalog.pg_class t ON t.oid=i.indrelid JOIN pg_catalog.pg_namespace n ON n.oid=t.relnamespace WHERE n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname !~ '^pg_temp_' AND n.nspname !~ '^pg_toast'").fetch_all(&mut **tx).await?;
    definitions.extend(indexes);
    let triggers:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_array('trigger',t.tgrelid::regclass::text,t.tgname,t.tgenabled,pg_catalog.pg_get_triggerdef(t.oid,false),pg_catalog.pg_get_functiondef(t.tgfoid)) FROM pg_catalog.pg_trigger t JOIN pg_catalog.pg_class c ON c.oid=t.tgrelid JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE NOT t.tgisinternal AND n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname !~ '^pg_temp_' AND n.nspname !~ '^pg_toast'").fetch_all(&mut **tx).await?;
    definitions.extend(triggers);
    let inheritance:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_array('inherits',inhrelid::regclass::text,inhparent::regclass::text,inhseqno) FROM pg_catalog.pg_inherits").fetch_all(&mut **tx).await?;
    definitions.extend(inheritance);
    let tables:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_array('relation_options',c.oid::regclass::text,c.relrowsecurity,c.relforcerowsecurity,c.relreplident,c.relpersistence,c.relispartition,pg_catalog.pg_get_expr(c.relpartbound,c.oid),c.reloptions) FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE c.relkind IN ('r','p','m','v') AND n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname !~ '^pg_temp_' AND n.nspname !~ '^pg_toast'").fetch_all(&mut **tx).await?;
    definitions.extend(tables);
    definitions.sort_by_key(Value::to_string);
    Ok(definitions)
}

/// Capture only a native source; this must run in the SAME readonly transaction
/// as its exported Parquet bytes, not from another pool or later snapshot.
///
/// # Errors
/// Malformed native lineage, unknown schemas, inaccessible or foreign relations.
pub async fn capture(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<Option<RecoveryPlan>, OriginError> {
    sqlx::raw_sql(
        "SET LOCAL TimeZone='UTC'; SET LOCAL DateStyle='ISO, YMD'; SET LOCAL extra_float_digits=3; SET LOCAL bytea_output='hex'; SET LOCAL IntervalStyle='iso_8601'; SET LOCAL lc_monetary='C'; SET LOCAL search_path=pg_catalog",
    )
    .execute(&mut **tx)
    .await?;
    let present: bool =
        sqlx::query_scalar("SELECT to_regclass('public.database_origin') IS NOT NULL")
            .fetch_one(&mut **tx)
            .await?;
    if !present {
        return Ok(None);
    }
    let origin: Vec<Value> = sqlx::query_scalar("SELECT to_jsonb(o) FROM public.database_origin o")
        .fetch_all(&mut **tx)
        .await?;
    if origin.is_empty() {
        return Ok(None);
    }
    let [source_origin] = origin.as_slice() else {
        return refusal();
    };
    let physical:(String,i64,Option<String>)=sqlx::query_as("SELECT datname::text,oid::bigint,shobj_description(oid,'pg_database') FROM pg_catalog.pg_database WHERE datname=current_database()") .fetch_one(&mut **tx).await?;
    let source_creation = uuid(source_origin, "creation_operation")?;
    if source_origin.get("database_name").and_then(Value::as_str) != Some(physical.0.as_str())
        || source_origin.get("database_oid").and_then(Value::as_i64) != Some(physical.1)
        || !physical.2.as_deref().is_some_and(|m| {
            m == super::marker(source_creation)
                || m.starts_with(&format!("{}:recovery:", super::marker(source_creation)))
        })
    {
        return refusal();
    }

    let database_name = sqlx::query_scalar("SELECT current_database()::text")
        .fetch_one(&mut **tx)
        .await?;
    let source_upgrades = sqlx::query_scalar(
        "SELECT to_jsonb(u) FROM public.database_origin_upgrade u ORDER BY upgrade_operation",
    )
    .fetch_all(&mut **tx)
    .await?;
    let source_migrations: Option<Value> =
        sqlx::query_scalar("SELECT public.database_origin_schema_contract()")
            .fetch_one(&mut **tx)
            .await?;
    let prior_rows: Vec<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(r) FROM public.database_recovery r ORDER BY recovery_operation",
    )
    .fetch_all(&mut **tx)
    .await?;
    let expected_marker = match prior_rows.as_slice() {
        [] => super::marker(source_creation),
        [prior] => super::recovery::marker(source_creation, uuid(prior, "recovery_operation")?),
        _ => return refusal(),
    };
    if physical.2.as_deref() != Some(expected_marker.as_str()) {
        return refusal();
    }
    let prior_recovery = summarize_recovery_rows(&prior_rows)?;
    let plan = RecoveryPlan {
        format_version: 1,
        database_name,
        source_origin: source_origin.clone(),
        source_upgrades,
        source_migrations: source_migrations.ok_or(OriginError::Recovery)?,
        prior_recovery,
        relations: observed(tx).await?,
        constraints: constraints(tx).await?,
        large_objects: sqlx::query_as("SELECT l.oid::bigint,r.rolname::text,l.lomacl::text FROM pg_catalog.pg_largeobject_metadata l JOIN pg_catalog.pg_roles r ON r.oid=l.lomowner ORDER BY l.oid").fetch_all(&mut **tx).await?,
    };
    plan.validate_source()?;
    Ok(Some(plan))
}

/// Validate actual decoded authority rows, then summarize without recursively
/// embedding their manifest/plan bytes. Full rows remain in this generation's
/// database_recovery Parquet object, bound by its manifest checksum.
pub fn summarize_recovery_rows(rows: &[Value]) -> Result<Vec<Value>, OriginError> {
    if rows.len() > 1 {
        return refusal();
    }
    let mut summaries = Vec::new();
    for row in rows {
        if row.get("completed_at").is_none_or(Value::is_null) {
            return refusal();
        }
        let decode = |name: &str| {
            row.get(name)
                .and_then(Value::as_str)
                .and_then(|v| v.strip_prefix("\\x"))
                .and_then(|v| hex::decode(v).ok())
                .ok_or(OriginError::Recovery)
        };
        let bytes = decode("manifest_bytes")?;
        if row.get("manifest_sha256").and_then(Value::as_str)
            != Some(hex::encode(Sha256::digest(&bytes)).as_str())
        {
            return refusal();
        }
        let previous = parse(&bytes)?;
        let canonical = serde_json::to_vec(&previous).map_err(|_| OriginError::Recovery)?;
        if decode("source_identity_bytes")? != canonical
            || row.get("verified_plan_sha256").and_then(Value::as_str)
                != Some(hex::encode(Sha256::digest(&canonical)).as_str())
        {
            return refusal();
        }
        summaries.push(serde_json::json!({"creation_operation":uuid(row,"creation_operation")?,"recovery_operation":uuid(row,"recovery_operation")?,"manifest_sha256":row["manifest_sha256"],"verified_plan_sha256":row["verified_plan_sha256"],"row_sha256":hex::encode(Sha256::digest(serde_json::to_vec(row).map_err(|_|OriginError::Recovery)?))}));
    }
    Ok(summaries)
}

/// Parse the exact bound paired marker, refusing unknown archive formats.
pub(super) fn parse(bytes: &[u8]) -> Result<RecoveryPlan, OriginError> {
    if bytes.len() > 16 * 1024 * 1024 {
        return refusal();
    }
    let envelope: Value = serde_json::from_slice(bytes).map_err(|_| OriginError::Recovery)?;
    if envelope.get("format_version").and_then(Value::as_u64) != Some(1) {
        return refusal();
    }
    let root = if envelope.get("paired").is_some() {
        envelope.get("paired").ok_or(OriginError::Recovery)?
    } else {
        &envelope
    };
    if root.get("format_version").and_then(Value::as_u64) != Some(1)
        || root.get("cross_database_atomic").and_then(Value::as_bool) != Some(false)
    {
        return refusal();
    }
    let dbs = root
        .get("databases")
        .and_then(Value::as_array)
        .ok_or(OriginError::Recovery)?;
    if dbs.len() != 2
        || dbs[0].get("database").and_then(Value::as_str) != Some("navigator")
        || dbs[1].get("database").and_then(Value::as_str) != Some("forgejo")
    {
        return refusal();
    }
    let plan: RecoveryPlan = serde_json::from_value(
        dbs[0]
            .get("recovery_plan")
            .ok_or(OriginError::Recovery)?
            .clone(),
    )
    .map_err(|_| OriginError::Recovery)?;
    if dbs[0].get("database_name").and_then(Value::as_str) != Some(plan.database_name.as_str()) {
        return refusal();
    }
    plan.validate_source()?;
    Ok(plan)
}

/// AGE 1.6 allocates label IDs by cycling over its graph registry and skipping
/// occupied IDs. This is not the domain identity or per-label entry allocator.
async fn registered_age_label_allocator(
    tx: &mut Transaction<'_, Postgres>,
    relation: &RecoveryRelation,
) -> Result<bool, OriginError> {
    if relation.name != "_label_id_seq" {
        return Ok(false);
    }
    let age:bool=sqlx::query_scalar("SELECT to_regclass('ag_catalog.ag_graph') IS NOT NULL AND EXISTS(SELECT 1 FROM pg_catalog.pg_extension WHERE extname='age' AND extversion='1.6.0')").fetch_one(&mut **tx).await?;
    if !age {
        return Ok(false);
    }
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM ag_catalog.ag_graph g JOIN pg_catalog.pg_namespace n ON n.oid=g.namespace JOIN pg_catalog.pg_class c ON c.relnamespace=n.oid AND c.relname='_label_id_seq' AND c.relkind='S' JOIN pg_catalog.pg_sequence q ON q.seqrelid=c.oid WHERE n.nspname::text=$1 AND q.seqstart=1 AND q.seqincrement=1 AND q.seqmax=65535 AND q.seqmin=1 AND q.seqcache=1 AND q.seqcycle AND q.seqtypid='pg_catalog.int4'::regtype AND NOT EXISTS(SELECT 1 FROM pg_catalog.pg_depend d WHERE d.objid=c.oid AND d.deptype IN ('a','i')) AND (SELECT count(*)=count(DISTINCT id) AND count(*)<65535 AND COALESCE(bool_and(id BETWEEN 1 AND 65535),true) FROM ag_catalog.ag_label WHERE graph=g.graphid))") .bind(&relation.schema).fetch_one(&mut **tx).await?)
}

/// Exact catalog closure and all logical stored rows. Authority evidence belongs
/// to the target resource, so its archived values are compared as provenance.
pub(super) async fn verify_target(
    tx: &mut Transaction<'_, Postgres>,
    plan: &RecoveryPlan,
) -> Result<(), OriginError> {
    sqlx::raw_sql("SET LOCAL TimeZone='UTC'; SET LOCAL DateStyle='ISO, YMD'; SET LOCAL extra_float_digits=3; SET LOCAL bytea_output='hex'; SET LOCAL IntervalStyle='iso_8601'; SET LOCAL lc_monetary='C'; SET LOCAL search_path=pg_catalog") .execute(&mut **tx).await?;
    let target = observed(tx).await?;
    if constraints(tx).await? != plan.constraints {
        return refusal();
    }
    let expected = plan
        .relations
        .iter()
        .map(|r| ((&r.schema, &r.name), r))
        .collect::<std::collections::BTreeMap<_, _>>();
    if target.len() != expected.len() {
        return refusal();
    }
    for r in target {
        let Some(source) = expected.get(&(&r.schema, &r.name)) else {
            return refusal();
        };
        if source.kind != r.kind
            || source.columns != r.columns
            || source.column_contract != r.column_contract
            || source.view_definition != r.view_definition
            || source.sequence_options != r.sequence_options
        {
            return refusal();
        }
        if r.kind == "S" {
            verify_sequence(tx, &r).await?;
        }
        if r.schema == "public"
            && [
                "database_origin",
                "database_origin_upgrade",
                "database_recovery",
                "_sqlx_migrations",
            ]
            .contains(&r.name.as_str())
        {
            continue;
        }
        if **source != r {
            return refusal();
        }
    }
    // Every sequence's captured state is non-MVCC. A single-database restore
    // can accept it only if no generated identity would be reused.
    let sequences:Vec<(String,String,String,String,String,bool)>=sqlx::query_as("SELECT ns.nspname::text,s.relname::text,nt.nspname::text,t.relname::text,a.attname::text,(ty.typname='graphid' AND ny.nspname='ag_catalog') FROM pg_catalog.pg_class s JOIN pg_catalog.pg_namespace ns ON ns.oid=s.relnamespace JOIN pg_catalog.pg_depend d ON d.objid=s.oid AND d.deptype IN ('a','i') JOIN pg_catalog.pg_class t ON t.oid=d.refobjid JOIN pg_catalog.pg_namespace nt ON nt.oid=t.relnamespace JOIN pg_catalog.pg_attribute a ON a.attrelid=t.oid AND a.attnum=d.refobjsubid JOIN pg_catalog.pg_type ty ON ty.oid=a.atttypid JOIN pg_catalog.pg_namespace ny ON ny.oid=ty.typnamespace WHERE s.relkind='S'").fetch_all(&mut **tx).await?;
    for (schema, seq, table_schema, table, col, graphid) in sequences {
        let identifier = ident(&col);
        // AGE graph IDs embed the label in high bits; their sequence owns the
        // local entry ID, and inherited child labels have separate sequences.
        let value = if graphid {
            format!("({identifier}::text::bigint & 281474976710655)")
        } else {
            identifier
        };
        let only = if graphid { "ONLY " } else { "" };
        let q=format!("SELECT NOT EXISTS(SELECT 1 FROM {only}{}.{} WHERE {value} >= (SELECT last_value FROM {}.{}) AND (SELECT NOT is_called FROM {}.{})) AND COALESCE((SELECT max({value}) FROM {only}{}.{}),0) <= (SELECT last_value FROM {}.{})",ident(&table_schema),ident(&table),ident(&schema),ident(&seq),ident(&schema),ident(&seq),ident(&table_schema),ident(&table),ident(&schema),ident(&seq));
        let safe: bool = sqlx::query_scalar(AssertSqlSafe(q))
            .fetch_one(&mut **tx)
            .await?;
        if !safe {
            return refusal();
        }
    }
    Ok(())
}

async fn verify_sequence(
    tx: &mut Transaction<'_, Postgres>,
    r: &RecoveryRelation,
) -> Result<(), OriginError> {
    let options = r
        .sequence_options
        .as_ref()
        .and_then(Value::as_array)
        .ok_or(OriginError::Recovery)?;
    let registry_allocator = registered_age_label_allocator(tx, r).await?;
    if !registry_allocator {
        if options
            .get(1)
            .and_then(Value::as_i64)
            .is_none_or(|v| v <= 0)
            || options.get(5).and_then(Value::as_bool) != Some(false)
        {
            return refusal();
        }
        let query = format!(
            "SELECT last_value<=$1-$2 FROM {}.{}",
            ident(&r.schema),
            ident(&r.name)
        );
        let safe: bool = sqlx::query_scalar(AssertSqlSafe(query))
            .bind(
                options
                    .get(2)
                    .and_then(Value::as_i64)
                    .ok_or(OriginError::Recovery)?,
            )
            .bind(
                options
                    .get(1)
                    .and_then(Value::as_i64)
                    .ok_or(OriginError::Recovery)?,
            )
            .fetch_one(&mut **tx)
            .await?;
        if !safe {
            return refusal();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::postgres::PostgresDb;

    #[tokio::test]
    async fn real_snapshot_fingerprints_preserve_duplicates_nulls_and_empty_target_extras_refuse() {
        crate::test_support::with_postgres(async {
            let pg=PostgresDb::application().await.unwrap();
            sqlx::raw_sql("CREATE TABLE public.recovery_fingerprint_probe(value text); INSERT INTO public.recovery_fingerprint_probe VALUES(NULL),(''),('same'),('same')").execute(pg.pool()).await.unwrap();
            let mut tx=pg.pool().begin().await.unwrap();
            sqlx::raw_sql("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY; SET LOCAL search_path=pg_catalog").execute(&mut *tx).await.unwrap();
            let plan=capture(&mut tx).await.unwrap().unwrap();tx.rollback().await.unwrap();
            let proof=plan.relations.iter().find(|r|r.name=="recovery_fingerprint_probe").unwrap();
            assert_eq!(proof.rows,4);
            assert_eq!(proof.columns,vec![("value".into(),"text".into())]);
            let mut verify=pg.pool().begin().await.unwrap();verify_target(&mut verify,&plan).await.unwrap();verify.rollback().await.unwrap();
            let sequence:String=sqlx::query_scalar("SELECT pg_get_serial_sequence('public.project_status_transition','sequence')").fetch_one(pg.pool()).await.unwrap();
            sqlx::raw_sql(AssertSqlSafe(format!("ALTER SEQUENCE {sequence} INCREMENT BY -1"))).execute(pg.pool()).await.unwrap();
            let mut verify=pg.pool().begin().await.unwrap();assert!(verify_target(&mut verify,&plan).await.is_err(),"same sequence rows do not authorize descending generation");verify.rollback().await.unwrap();
            sqlx::raw_sql(AssertSqlSafe(format!("ALTER SEQUENCE {sequence} INCREMENT BY 1"))).execute(pg.pool()).await.unwrap();
            let mut verify=pg.pool().begin().await.unwrap();verify_target(&mut verify,&plan).await.unwrap();verify.rollback().await.unwrap();
            sqlx::query("INSERT INTO public.recovery_fingerprint_probe VALUES('same')").execute(pg.pool()).await.unwrap();
            let mut verify=pg.pool().begin().await.unwrap();assert!(verify_target(&mut verify,&plan).await.is_err());verify.rollback().await.unwrap();
            sqlx::raw_sql("DELETE FROM public.recovery_fingerprint_probe WHERE ctid IN (SELECT ctid FROM public.recovery_fingerprint_probe WHERE value='same' LIMIT 1); CREATE TABLE public.recovery_empty_extra(id bigint)").execute(pg.pool()).await.unwrap();
            let mut verify=pg.pool().begin().await.unwrap();assert!(verify_target(&mut verify,&plan).await.is_err(),"even an empty unclassified target relation refuses");verify.rollback().await.unwrap();
            let mut newer=plan.clone();newer.source_migrations.as_array_mut().unwrap().push(serde_json::json!([29_991_231_999_999_i64,"unknown"]));
            assert!(newer.validate_source().is_err());
            let mut changed=plan.clone();changed.source_origin["database_name"]=serde_json::json!("foreign");
            assert!(changed.validate_source().is_err());
        }).await;
    }
    #[tokio::test]
    async fn actual_age_label_no_inherit_refuses_with_unchanged_stored_rows() {
        crate::test_support::with_postgres(async {
            let pg=PostgresDb::application().await.unwrap();
            let mut tx=pg.pool().begin().await.unwrap();
            sqlx::raw_sql("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY").execute(&mut *tx).await.unwrap();
            let plan=capture(&mut tx).await.unwrap().unwrap();tx.rollback().await.unwrap();
            let mut verify=pg.pool().begin().await.unwrap();verify_target(&mut verify,&plan).await.unwrap();verify.rollback().await.unwrap();
            let (child,parent):(String,String)=sqlx::query_as("SELECT i.inhrelid::regclass::text,i.inhparent::regclass::text FROM pg_inherits i JOIN pg_class c ON c.oid=i.inhrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='navigator_graph' AND c.relname='Person'").fetch_one(pg.pool()).await.unwrap();
            sqlx::raw_sql(AssertSqlSafe(format!("ALTER TABLE {child} NO INHERIT {parent}"))).execute(pg.pool()).await.unwrap();
            let mut verify=pg.pool().begin().await.unwrap();assert!(verify_target(&mut verify,&plan).await.is_err(),"AGE traversal inheritance is part of the logical schema");verify.rollback().await.unwrap();
        }).await;
    }
    #[tokio::test]
    async fn registered_age_allocator_does_not_allow_an_ordinary_cyclic_sequence() {
        crate::test_support::with_postgres(async {
            let pg = PostgresDb::application().await.unwrap();
            let mut tx = pg.pool().begin().await.unwrap();
            sqlx::raw_sql("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
                .execute(&mut *tx)
                .await
                .unwrap();
            let plan = capture(&mut tx).await.unwrap().unwrap();
            tx.rollback().await.unwrap();
            assert!(plan.relations.iter().any(|r| r.name == "_label_id_seq"
                && r.sequence_options
                    .as_ref()
                    .is_some_and(|v| v[5] == serde_json::json!(true))));
            let mut tx = pg.pool().begin().await.unwrap();
            verify_target(&mut tx, &plan).await.unwrap();
            tx.rollback().await.unwrap();
            sqlx::raw_sql("CREATE SEQUENCE public.recovery_domain_cycle CYCLE")
                .execute(pg.pool())
                .await
                .unwrap();
            let mut tx = pg.pool().begin().await.unwrap();
            sqlx::raw_sql("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
                .execute(&mut *tx)
                .await
                .unwrap();
            let plan = capture(&mut tx).await.unwrap().unwrap();
            tx.rollback().await.unwrap();
            let mut tx = pg.pool().begin().await.unwrap();
            assert!(
                verify_target(&mut tx, &plan).await.is_err(),
                "ordinary cyclic domain allocation still refuses"
            );
            tx.rollback().await.unwrap();
        })
        .await;
    }
}
