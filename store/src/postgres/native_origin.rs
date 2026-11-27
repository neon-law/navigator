//! Fresh-native authority is minted by creation, never inferred from target rows.
//!
//! A tier binding is the privileged provisioner's verified Kubernetes identity.
//! This module binds it to successful database creation; it does not authenticate
//! Kubernetes itself. Captured-source enrollment keeps its independent guards.
use std::sync::OnceLock;

use super::{embedded_migrator, PostgresDb};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgConnectOptions;
use sqlx::{AssertSqlSafe, PgConnection, Row};
use uuid::Uuid;

#[cfg(any(test, feature = "test-support"))]
pub(crate) mod fixture;
pub mod recovery;
pub mod recovery_plan;
mod upgrade;
pub use upgrade::upgrade;
#[cfg(any(test, feature = "test-support"))]
pub(crate) mod fixture_upgrade;

/// Verified identity supplied by the owning local-tier provisioner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TierBinding {
    pub tier_id: Uuid,
    pub namespace_uid: Uuid,
    pub cluster_name: String,
}

impl TierBinding {
    fn validate(&self) -> Result<(), OriginError> {
        if self.tier_id.is_nil()
            || self.namespace_uid.is_nil()
            || self.cluster_name.is_empty()
            || self.cluster_name.len() > 63
            || !self
                .cluster_name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(OriginError::Identity);
        }
        Ok(())
    }
}

/// Errors expose no credentials, SQL values or failing-row contents.
#[derive(Debug, thiserror::Error)]
pub enum OriginError {
    #[error("Database Origin identity is invalid or foreign")]
    Identity,
    #[error("Database Origin creation failed; an existing database is never adopted")]
    Creation,
    #[error("Database Origin schema initialization failed")]
    Migration,
    #[error(
        "Database Origin is absent, ambiguous or does not match the current authority contract"
    )]
    Authority,
    #[error("Database Origin storage is unavailable")]
    Storage,
    #[error("Database Origin has a pending or conflicting upgrade")]
    Upgrade,
    #[error("another Database Origin upgrade holds the resource")]
    Busy,
    #[error("Database recovery is pending or its provenance is unavailable")]
    Recovery,
}

impl From<sqlx::Error> for OriginError {
    fn from(_: sqlx::Error) -> Self {
        Self::Storage
    }
}

/// An opaque creation capability. It cannot be constructed from an existing pool,
/// deserialized artifact, observed empty schema, or environment switch.
pub struct CreatedDatabase {
    postgres: PostgresDb,
    operation: Uuid,
    tier: TierBinding,
    admin_options: PgConnectOptions,
}

/// A fully initialized resource. Dropping this handle never deletes its database.
pub struct NativeDatabase {
    postgres: PostgresDb,
    operation: Uuid,
    tier: TierBinding,
}

/// The deterministic identifier reserved by one valid creation operation.
/// Computing its name neither creates nor admits an existing database.
///
/// # Errors
/// A nil operation has no reserved resource.
pub fn database_name(operation: Uuid) -> Result<String, OriginError> {
    if operation.is_nil() {
        return Err(OriginError::Identity);
    }
    Ok(format!("navigator_native_{}", operation.simple()))
}

fn marker(operation: Uuid) -> String {
    format!("navigator-native-origin-v1:{operation}")
}

/// Create exactly this operation's new database. A duplicate operation races on
/// PostgreSQL's own unique database creation; no caller adopts the other result.
///
/// # Errors
/// Invalid identity, unavailable administrative connection or failed CREATE.
/// Administrative credentials are explicit; no runtime role gains CREATEDB.
pub async fn create(
    admin_options: PgConnectOptions,
    operation: Uuid,
    tier: TierBinding,
) -> Result<CreatedDatabase, OriginError> {
    tier.validate()?;
    let name = database_name(operation)?;
    let admin = PostgresDb::connect(admin_options.clone())
        .await
        .map_err(|_| OriginError::Creation)?;
    let result = async {
        let mut connection = admin
            .pool()
            .acquire()
            .await
            .map_err(|_| OriginError::Creation)?;
        create_database_on(&mut connection, &name, || {}).await
    }
    .await;
    admin.pool().close().await;
    result?;
    let postgres = PostgresDb::connect(admin_options.clone().database(&name))
        .await
        .map_err(|_| OriginError::Creation)?;
    stamp_creation(postgres.pool(), &name, operation).await?;
    Ok(CreatedDatabase {
        postgres,
        operation,
        tier,
        admin_options,
    })
}

/// CREATE is exclusive. Report acknowledgement before the later fencing
/// operations so the owning fixture can clean up only its own resource.
async fn create_database_on(
    admin: &mut PgConnection,
    name: &str,
    observe_created: impl FnOnce(),
) -> Result<(), OriginError> {
    // The database name derives solely from an operation UUID.
    sqlx::query(AssertSqlSafe(format!(
        "CREATE DATABASE \"{name}\" TEMPLATE template0 ALLOW_CONNECTIONS false"
    )))
    .execute(&mut *admin)
    .await
    .map_err(|_| OriginError::Creation)?;
    observe_created();
    // Keep runtime roles out until the provisioner deliberately grants them access.
    sqlx::query(AssertSqlSafe(format!(
        "REVOKE ALL ON DATABASE \"{name}\" FROM PUBLIC"
    )))
    .execute(&mut *admin)
    .await
    .map_err(|_| OriginError::Creation)?;
    sqlx::query(AssertSqlSafe(format!(
        "ALTER DATABASE \"{name}\" ALLOW_CONNECTIONS true"
    )))
    .execute(&mut *admin)
    .await
    .map_err(|_| OriginError::Creation)?;
    Ok(())
}

async fn stamp_creation<'a>(
    executor: impl sqlx::Executor<'a, Database = sqlx::Postgres>,
    name: &str,
    operation: Uuid,
) -> Result<(), OriginError> {
    sqlx::query(AssertSqlSafe(format!(
        "COMMENT ON DATABASE \"{name}\" IS '{}'",
        marker(operation)
    )))
    .execute(executor)
    .await?;
    Ok(())
}

impl CreatedDatabase {
    /// Migrate the actual newly created resource before persisting its origin.
    /// Failure leaves it unadmitted; retry must never adopt it from emptiness.
    ///
    /// # Errors
    /// Incomplete migrations, mixed enrollment, changed database identity or storage failure.
    pub async fn initialize(self) -> Result<NativeDatabase, OriginError> {
        self.initialize_with_recovery(None).await
    }

    async fn initialize_with_recovery(
        self,
        recovery: Option<&recovery::RecoveryInput>,
    ) -> Result<NativeDatabase, OriginError> {
        self.initialize_through(None, recovery).await
    }

    async fn initialize_through(
        self,
        through: Option<i64>,
        recovery: Option<&recovery::RecoveryInput>,
    ) -> Result<NativeDatabase, OriginError> {
        self.postgres
            .migrate_embedded(through)
            .await
            .map_err(|_| OriginError::Migration)?;
        let mut initial_schema = schema_contract();
        if let Some(version) = through {
            initial_schema
                .as_array_mut()
                .ok_or(OriginError::Migration)?
                .retain(|entry| {
                    entry
                        .get(0)
                        .and_then(serde_json::Value::as_i64)
                        .is_some_and(|v| v <= version)
                });
        }
        require_schema_for(&self.postgres, &initial_schema).await?;
        require_no_captured_history(&self.postgres).await?;
        let (name, oid, actual_marker) = identity(&self.postgres).await?;
        if name != database_name(self.operation)?
            || actual_marker.as_deref() != Some(marker(self.operation).as_str())
        {
            return Err(OriginError::Identity);
        }
        let mut transaction = self.postgres.pool().begin().await?;
        sqlx::query("INSERT INTO public.database_origin(singleton,format_version,origin,creation_operation,database_name,database_oid,tier_id,namespace_uid,cluster_name,schema_contract,authority_contract) VALUES(true,1,'fresh_native',$1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(self.operation).bind(name).bind(oid).bind(self.tier.tier_id).bind(self.tier.namespace_uid)
            .bind(&self.tier.cluster_name).bind(initial_schema).bind(authority_contract())
            .execute(&mut *transaction).await?;
        if let Some(input) = recovery {
            recovery::record_pending(&mut transaction, self.operation, input).await?;
            sqlx::query(AssertSqlSafe(format!(
                "COMMENT ON DATABASE \"{}\" IS '{}'",
                database_name(self.operation)?,
                input.target_marker(self.operation)
            )))
            .execute(&mut *transaction)
            .await?;
        } else {
            crate::durable_object_handover::initialize_fresh_on(&mut transaction).await?;
        }
        transaction.commit().await?;
        Ok(NativeDatabase {
            postgres: self.postgres,
            operation: self.operation,
            tier: self.tier,
        })
    }
}

impl NativeDatabase {
    #[must_use]
    pub fn postgres(&self) -> &PostgresDb {
        &self.postgres
    }
    #[must_use]
    pub fn database(&self) -> String {
        format!("navigator_native_{}", self.operation.simple())
    }
    #[must_use]
    pub const fn operation(&self) -> Uuid {
        self.operation
    }
    #[must_use]
    pub const fn tier(&self) -> &TierBinding {
        &self.tier
    }
}

/// Resume only an already initialized origin matching the exact provisioner's
/// binding. This never creates or stamps anything, even on an empty target.
///
/// # Errors
/// Missing/foreign origin, mismatched schema, mixed history or unavailable storage.
pub async fn resume(
    admin_options: PgConnectOptions,
    operation: Uuid,
    tier: TierBinding,
) -> Result<NativeDatabase, OriginError> {
    tier.validate()?;
    let postgres = PostgresDb::connect(admin_options.database(&database_name(operation)?))
        .await
        .map_err(|_| OriginError::Storage)?;
    let verified = async {
        if !admits(&postgres).await? {
        return Err(OriginError::Authority);
    }
    let row = sqlx::query("SELECT creation_operation,tier_id,namespace_uid,cluster_name FROM public.database_origin WHERE singleton")
        .fetch_one(postgres.pool()).await?;
    if row.try_get::<Uuid, _>("creation_operation")? != operation
        || row.try_get::<Uuid, _>("tier_id")? != tier.tier_id
        || row.try_get::<Uuid, _>("namespace_uid")? != tier.namespace_uid
        || row.try_get::<String, _>("cluster_name")? != tier.cluster_name
    {
        return Err(OriginError::Identity);
    }
        Ok::<(),OriginError>(())
    }.await;
    if let Err(error) = verified {
        postgres.pool().close().await;
        return Err(error);
    }
    Ok(NativeDatabase {
        postgres,
        operation,
        tier,
    })
}

fn schema_contract() -> serde_json::Value {
    static CONTRACT: OnceLock<serde_json::Value> = OnceLock::new();
    CONTRACT
        .get_or_init(|| schema_contract_for(&embedded_migrator()))
        .clone()
}

fn schema_contract_for(runner: &sqlx::migrate::Migrator) -> serde_json::Value {
    let mut entries = runner
        .iter()
        .map(|m| (m.version, hex::encode(&m.checksum)))
        .collect::<Vec<_>>();
    entries.sort();
    serde_json::json!(entries)
}

/// Exact current release contract for runtime admission and offline restore checks.
/// Contains schema checksums and canonical table names, never database values.
#[must_use]
pub fn release_contract() -> serde_json::Value {
    serde_json::json!({"format_version":1,"schema_contract":schema_contract(),"authority_contract":authority_contract()})
}

fn authority_contract() -> serde_json::Value {
    serde_json::json!(crate::postgres::history::Family::all()
        .iter()
        .map(|family| (family.scope(), family.tables()))
        .collect::<Vec<_>>())
}

async fn schema_attestation(pg: &PostgresDb) -> Result<serde_json::Value, OriginError> {
    let actual: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT public.database_origin_schema_contract()")
            .fetch_one(pg.pool())
            .await?;
    actual.ok_or(OriginError::Authority)
}

async fn require_schema_for(
    pg: &PostgresDb,
    expected: &serde_json::Value,
) -> Result<(), OriginError> {
    if &schema_attestation(pg).await? != expected {
        return Err(OriginError::Authority);
    }
    Ok(())
}

async fn require_no_captured_history(pg: &PostgresDb) -> Result<(), OriginError> {
    let mixed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mirror_backfill_run)")
        .fetch_one(pg.pool())
        .await?;
    if mixed {
        return Err(OriginError::Authority);
    }
    Ok(())
}

async fn identity(pg: &PostgresDb) -> Result<(String, i64, Option<String>), OriginError> {
    Ok(sqlx::query_as("SELECT datname::text,oid::bigint,shobj_description(oid,'pg_database') FROM pg_catalog.pg_database WHERE datname=pg_catalog.current_database()")
        .fetch_one(pg.pool()).await?)
}

/// Check a complete fresh-native origin. Its absence on a recognized schema
/// preserves captured-source admission. Unknown schemas, damaged recovery fences
/// and invalid present origins are errors, never a fallback.
///
/// # Errors
/// Unknown version, mixed histories, schema/authority drift or foreign database.
pub async fn admits(pg: &PostgresDb) -> Result<bool, OriginError> {
    admits_for(pg, &schema_contract()).await
}

struct Origin {
    operation: Uuid,
    tier: TierBinding,
    schema: serde_json::Value,
}

async fn origin(pg: &PostgresDb) -> Result<Option<Origin>, OriginError> {
    recovery::require_no_pending(pg).await?;
    let exists: bool =
        sqlx::query_scalar("SELECT to_regclass('public.database_origin') IS NOT NULL")
            .fetch_one(pg.pool())
            .await?;
    if !exists {
        return Ok(None);
    }
    let rows = sqlx::query("SELECT * FROM public.database_origin")
        .fetch_all(pg.pool())
        .await?;
    if rows.is_empty() {
        return Ok(None);
    }
    let [row] = rows.as_slice() else {
        return Err(OriginError::Authority);
    };
    let operation: Uuid = row.try_get("creation_operation")?;
    let tier = TierBinding {
        tier_id: row.try_get("tier_id")?,
        namespace_uid: row.try_get("namespace_uid")?,
        cluster_name: row.try_get("cluster_name")?,
    };
    tier.validate()?;
    let (name, oid, actual_marker) = identity(pg).await?;
    if row.try_get::<i32, _>("format_version")? != 1
        || row.try_get::<String, _>("origin")? != "fresh_native"
        || name != database_name(operation)?
        || row.try_get::<String, _>("database_name")? != name
        || row.try_get::<i64, _>("database_oid")? != oid
        || (actual_marker.as_deref() != Some(marker(operation).as_str())
            && !actual_marker
                .as_deref()
                .is_some_and(|m| m.starts_with(&format!("{}:recovery:", marker(operation)))))
        || row.try_get::<serde_json::Value, _>("authority_contract")? != authority_contract()
    {
        return Err(OriginError::Authority);
    }
    require_no_captured_history(pg).await?;
    Ok(Some(Origin {
        operation,
        tier,
        schema: row.try_get("schema_contract")?,
    }))
}

async fn admits_for(pg: &PostgresDb, expected: &serde_json::Value) -> Result<bool, OriginError> {
    let Some(origin) = origin(pg).await? else {
        return Ok(false);
    };
    let chain = upgrade::chain(pg, &origin, expected).await?;
    if chain.pending.is_some() {
        return Err(OriginError::Upgrade);
    }
    if &chain.schema != expected {
        return Err(OriginError::Authority);
    }
    require_schema_for(pg, expected).await?;
    Ok(true)
}

#[cfg(test)]
mod tests;
