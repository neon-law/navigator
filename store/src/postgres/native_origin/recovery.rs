//! Pending native logical recovery is fenced before any archive row is restored.
//!
//! This phase retains bytes as unverified input, not as an activation receipt.
//! Completion independently parses the bound archive and recomputes the exact
//! target relation closure behind a resource-level connection fence.
use super::{CreatedDatabase, NativeDatabase, OriginError};
use crate::postgres::PostgresDb;
use sha2::{Digest as _, Sha256};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

type PredicateDefinition = (String, String, bool, String, Option<Vec<String>>);

/// Immutable archive inputs retained for the recovery verifier.
/// Construction does not authenticate an archive or authorize activation.
pub struct RecoveryInput {
    operation: Uuid,
    manifest: Vec<u8>,
    source_identity: Vec<u8>,
}

pub(super) fn marker(creation: Uuid, recovery: Uuid) -> String {
    format!("{}:recovery:{recovery}", super::marker(creation))
}

impl RecoveryInput {
    pub(super) fn target_marker(&self, creation: Uuid) -> String {
        marker(creation, self.operation)
    }
    /// Bound input sizes before any target is created or connected.
    ///
    /// # Errors
    /// Nil recovery operation, empty or oversized input.
    pub fn new(
        operation: Uuid,
        manifest: Vec<u8>,
        source_identity: Vec<u8>,
    ) -> Result<Self, OriginError> {
        if operation.is_nil()
            || manifest.is_empty()
            || source_identity.is_empty()
            || manifest.len() > 16 * 1024 * 1024
            || source_identity.len() > 1024 * 1024
        {
            return Err(OriginError::Recovery);
        }
        Ok(Self {
            operation,
            manifest,
            source_identity,
        })
    }
}

/// A newly created resource fenced for logical recovery, without a fabricated
/// fresh object-ownership epoch. Dropping the handle never deletes the resource.
pub struct PendingRecovery {
    native: NativeDatabase,
    admin_options: sqlx::postgres::PgConnectOptions,
    recovery_operation: Uuid,
    database_oid: i64,
}

impl PendingRecovery {
    /// Privileged restoration connection; application admission remains refused.
    #[must_use]
    pub fn postgres(&self) -> &PostgresDb {
        self.native.postgres()
    }
    #[must_use]
    pub fn database(&self) -> String {
        self.native.database()
    }
    #[must_use]
    pub const fn creation_operation(&self) -> Uuid {
        self.native.operation()
    }
}

impl CreatedDatabase {
    /// Migrate the acknowledged new resource and record its own origin together
    /// with a pending recovery fence. Source identity bytes never become its Origin.
    ///
    /// # Errors
    /// Actual migration/checksum/identity validation or durable intent write fails.
    pub async fn initialize_recovery(
        self,
        input: RecoveryInput,
    ) -> Result<PendingRecovery, OriginError> {
        if self.operation == input.operation {
            return Err(OriginError::Recovery);
        }
        let admin_options = self.admin_options.clone();
        let native = self.initialize_with_recovery(Some(&input)).await?;
        let database_oid = super::identity(native.postgres()).await?.1;
        Ok(PendingRecovery {
            native,
            admin_options,
            recovery_operation: input.operation,
            database_oid,
        })
    }
}

pub(super) async fn record_pending(
    tx: &mut Transaction<'_, Postgres>,
    creation: Uuid,
    input: &RecoveryInput,
) -> Result<(), OriginError> {
    let digest = hex::encode(Sha256::digest(&input.manifest));
    sqlx::query("INSERT INTO public.database_recovery(singleton,recovery_operation,creation_operation,manifest_sha256,manifest_bytes,source_identity_bytes) VALUES(true,$1,$2,$3,$4,$5)")
        .bind(input.operation).bind(creation).bind(digest).bind(&input.manifest).bind(&input.source_identity)
        .execute(&mut **tx).await?;
    Ok(())
}

async fn require_fixed_predicate<'e>(
    executor: impl sqlx::Executor<'e, Database = Postgres>,
    name: &str,
    sql: &str,
    language: &str,
) -> Result<(), OriginError> {
    let marker = format!("CREATE FUNCTION {name}()");
    let expected = sql
        .split_once(&marker)
        .and_then(|(_, s)| s.split_once(" AS $$"))
        .and_then(|(_, s)| s.split_once("$$;"))
        .map(|(body, _)| body)
        .ok_or(OriginError::Recovery)?;
    let actual:Option<PredicateDefinition>=sqlx::query_as("SELECT p.prosrc,l.lanname::text,p.prosecdef,p.provolatile::text,p.proconfig FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace JOIN pg_catalog.pg_language l ON l.oid=p.prolang WHERE n.nspname='public' AND p.proname=$1 AND p.pronargs=0 AND p.prorettype=CASE WHEN $1='database_recovery_pending' THEN 'pg_catalog.bool'::regtype ELSE 'pg_catalog.jsonb'::regtype END AND p.prokind='f'")
        .bind(name).fetch_optional(executor).await?;
    let Some((body, lang, definer, volatility, config)) = actual else {
        return Err(OriginError::Recovery);
    };
    if body != expected
        || lang != language
        || !definer
        || volatility != "s"
        || config.as_deref() != Some(&["search_path=pg_catalog".to_owned()][..])
    {
        return Err(OriginError::Recovery);
    }
    Ok(())
}

pub(super) async fn require_no_pending(pg: &PostgresDb) -> Result<(), OriginError> {
    let (table,attestation,origin):(bool,bool,bool)=sqlx::query_as("SELECT to_regclass('public.database_recovery') IS NOT NULL,to_regprocedure('public.database_origin_schema_contract()') IS NOT NULL,to_regclass('public.database_origin') IS NOT NULL")
        .fetch_one(pg.pool()).await?;
    if table {
        require_fixed_predicate(
            pg.pool(),
            "database_recovery_pending",
            include_str!(
                "../../../postgres/migrations/20261121000000_create_database_recovery.sql"
            ),
            "sql",
        )
        .await?;
        let pending: bool = sqlx::query_scalar("SELECT public.database_recovery_pending()")
            .fetch_one(pg.pool())
            .await?;
        if pending {
            return Err(OriginError::Recovery);
        }
    } else if attestation {
        require_fixed_predicate(
            pg.pool(),
            "database_origin_schema_contract",
            include_str!("../../../postgres/migrations/20261119050000_create_database_origin.sql"),
            "plpgsql",
        )
        .await?;
        let recovery_bound:bool=sqlx::query_scalar("SELECT COALESCE(shobj_description(oid,'pg_database'),'') LIKE 'navigator-native-origin-v1:%:recovery:%' FROM pg_catalog.pg_database WHERE datname=current_database()") .fetch_one(pg.pool()).await?;
        if recovery_bound {
            return Err(OriginError::Recovery);
        }
        let actual: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT public.database_origin_schema_contract()")
                .fetch_one(pg.pool())
                .await?;
        let expected = super::schema_contract();
        let allowed = match (
            actual.as_ref().and_then(serde_json::Value::as_array),
            expected.as_array(),
        ) {
            (Some(old), Some(current)) => {
                !old.is_empty()
                    && current.starts_with(old)
                    && old.iter().all(|entry| {
                        entry
                            .get(0)
                            .and_then(serde_json::Value::as_i64)
                            .is_some_and(|v| v < 20_261_121_000_000)
                    })
            }
            _ => false,
        };
        if !allowed {
            return Err(OriginError::Recovery);
        }
    } else {
        let _ = origin;
        return Err(OriginError::Recovery);
    }
    Ok(())
}

/// A committed recovery can have unconfirmed connectivity if reopening fails.
/// This is not an instruction to replay restoration or reset its completed intent.
pub enum CompletedRecovery {
    Open(NativeDatabase),
    CommitUnconfirmed {
        database: String,
        creation_operation: Uuid,
        recovery_operation: Uuid,
    },
    ReopenUnconfirmed {
        database: String,
        creation_operation: Uuid,
        recovery_operation: Uuid,
    },
}

impl RecoveryInput {
    /// Parse the actual paired marker and retain its exact native source evidence.
    /// This checks source lineage, not the eventual restored target.
    ///
    /// # Errors
    /// Unknown formats, unsupported lineage/checksums or oversized inputs.
    pub fn from_archive(operation: Uuid, manifest: Vec<u8>) -> Result<Self, OriginError> {
        if manifest.len() > 16 * 1024 * 1024 {
            return Err(OriginError::Recovery);
        }
        let plan = super::recovery_plan::parse(&manifest)?;
        let source_identity = serde_json::to_vec(&plan).map_err(|_| OriginError::Recovery)?;
        Self::new(operation, manifest, source_identity)
    }
}

impl PendingRecovery {
    /// Verify every restored relation independently, then commit the bound intent.
    /// No callback, caller-created receipt, or boolean can substitute for verification.
    ///
    /// All pool clones are drained. The exact created resource is sealed against
    /// new connections; unknown existing backends refuse without termination.
    /// Errors before completion retain a pending target. A lost commit acknowledgement
    /// returns CommitUnconfirmed with connections sealed; inspect durable state
    /// explicitly and never automatically replay restoration. No automatic reopen
    /// or generic adoption is attempted on failure.
    ///
    /// # Errors
    /// Foreign identity, CONNECT grants, active backend, input/schema/row drift,
    /// unsafe sequence state or failed transaction. Partial targets stay unadmitted.
    pub async fn verify_and_complete(self) -> Result<CompletedRecovery, OriginError> {
        use sqlx::Connection as _;
        require_fixed_predicate(
            self.native.postgres().pool(),
            "database_recovery_pending",
            include_str!(
                "../../../postgres/migrations/20261121000000_create_database_recovery.sql"
            ),
            "sql",
        )
        .await?;
        require_fixed_predicate(
            self.native.postgres().pool(),
            "database_origin_schema_contract",
            include_str!("../../../postgres/migrations/20261119050000_create_database_origin.sql"),
            "plpgsql",
        )
        .await?;
        let target_options = self
            .native
            .postgres
            .pool()
            .connect_options()
            .as_ref()
            .clone();
        self.native.postgres.pool().close().await;
        let admin = PostgresDb::connect(self.admin_options.clone())
            .await
            .map_err(|_| OriginError::Storage)?;
        let name = super::database_name(self.native.operation)?;
        let catalog: (i64,Option<String>,bool)=sqlx::query_as("SELECT oid::bigint,shobj_description(oid,'pg_database'),NOT EXISTS(SELECT 1 FROM aclexplode(COALESCE(datacl,acldefault('d',datdba))) a WHERE a.privilege_type='CONNECT' AND a.grantee<>datdba) FROM pg_catalog.pg_database WHERE datname=$1")
            .bind(&name).fetch_one(admin.pool()).await?;
        if catalog.0 != self.database_oid
            || catalog.1.as_deref()
                != Some(marker(self.native.operation, self.recovery_operation).as_str())
            || !catalog.2
        {
            admin.pool().close().await;
            return Err(OriginError::Recovery);
        }
        let mut connection = sqlx::PgConnection::connect_with(&target_options).await?;
        let actual:(String,i64)=sqlx::query_as("SELECT current_database()::text,oid::bigint FROM pg_catalog.pg_database WHERE datname=current_database()")
            .fetch_one(&mut connection).await?;
        if actual != (name.clone(), catalog.0) {
            admin.pool().close().await;
            return Err(OriginError::Identity);
        }
        // UUID-derived exact name only; no arbitrary caller identifier.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER DATABASE \"{name}\" ALLOW_CONNECTIONS false"
        )))
        .execute(admin.pool())
        .await?;
        let mut commit_attempted = false;
        let result = self
            .verify_on(&mut connection, &name, catalog.0, &mut commit_attempted)
            .await;
        // No early error path below may reopen a failed target.
        let operation = match result {
            Ok(v) => v,
            Err(error) => {
                let _ = connection.close().await;
                admin.pool().close().await;
                if commit_attempted {
                    return Ok(CompletedRecovery::CommitUnconfirmed {
                        database: name,
                        creation_operation: self.native.operation,
                        recovery_operation: self.recovery_operation,
                    });
                }
                return Err(error);
            }
        };
        let reopened = sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER DATABASE \"{name}\" ALLOW_CONNECTIONS true"
        )))
        .execute(admin.pool())
        .await
        .is_ok();
        let _ = connection.close().await;
        admin.pool().close().await;
        if !reopened {
            return Ok(CompletedRecovery::ReopenUnconfirmed {
                database: name,
                creation_operation: self.native.operation,
                recovery_operation: operation,
            });
        }
        let Ok(postgres) = PostgresDb::connect(target_options).await else {
            return Ok(CompletedRecovery::ReopenUnconfirmed {
                database: name,
                creation_operation: self.native.operation,
                recovery_operation: operation,
            });
        };
        Ok(CompletedRecovery::Open(NativeDatabase {
            postgres,
            operation: self.native.operation,
            tier: self.native.tier,
        }))
    }
    /// Keep the unknown-session check, full verification transaction and commit
    /// together after the caller seals the acknowledged resource.
    async fn verify_on(
        &self,
        connection: &mut sqlx::PgConnection,
        name: &str,
        database_oid: i64,
        commit_attempted: &mut bool,
    ) -> Result<Uuid, OriginError> {
        use sqlx::{Connection as _, Row as _};

        let other:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datid=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND pid<>pg_backend_pid())")
                .fetch_one(&mut *connection).await?;
        if other {
            return Err(OriginError::Recovery);
        }
        // A draining owner connection could alter these after the initial check.
        // Recheck on the sole connection only after the resource is sealed.
        require_fixed_predicate(
            &mut *connection,
            "database_recovery_pending",
            include_str!(
                "../../../postgres/migrations/20261121000000_create_database_recovery.sql"
            ),
            "sql",
        )
        .await?;
        require_fixed_predicate(
            &mut *connection,
            "database_origin_schema_contract",
            include_str!("../../../postgres/migrations/20261119050000_create_database_origin.sql"),
            "plpgsql",
        )
        .await?;
        let mut tx = connection.begin().await?;
        sqlx::raw_sql("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
            .execute(&mut *tx)
            .await?;
        let intent=sqlx::query("SELECT recovery_operation,creation_operation,manifest_bytes,source_identity_bytes,manifest_sha256,completed_at IS NOT NULL AS completed FROM public.database_recovery WHERE singleton FOR UPDATE")
                .fetch_one(&mut *tx).await?;
        if intent.try_get::<Uuid, _>("creation_operation")? != self.native.operation
            || intent.try_get::<Uuid, _>("recovery_operation")? != self.recovery_operation
            || intent.try_get::<bool, _>("completed")?
        {
            return Err(OriginError::Recovery);
        }
        let manifest: Vec<u8> = intent.try_get("manifest_bytes")?;
        let evidence: Vec<u8> = intent.try_get("source_identity_bytes")?;
        let digest = hex::encode(Sha256::digest(&manifest));
        if digest != intent.try_get::<String, _>("manifest_sha256")? {
            return Err(OriginError::Recovery);
        }
        let plan = super::recovery_plan::parse(&manifest)?;
        let canonical = serde_json::to_vec(&plan).map_err(|_| OriginError::Recovery)?;
        if canonical != evidence {
            return Err(OriginError::Recovery);
        }
        let origin: serde_json::Value =
            sqlx::query_scalar("SELECT to_jsonb(o) FROM public.database_origin o WHERE singleton")
                .fetch_one(&mut *tx)
                .await?;
        if origin["creation_operation"] != serde_json::json!(self.native.operation)
            || origin["database_name"] != serde_json::json!(name)
            || origin["database_oid"] != serde_json::json!(database_oid)
            || origin["tier_id"] != serde_json::json!(self.native.tier.tier_id)
            || origin["namespace_uid"] != serde_json::json!(self.native.tier.namespace_uid)
            || origin["cluster_name"] != serde_json::json!(self.native.tier.cluster_name)
            || origin["authority_contract"] != super::authority_contract()
            || origin["schema_contract"] != super::schema_contract()
        {
            return Err(OriginError::Recovery);
        }
        let schema: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT public.database_origin_schema_contract()")
                .fetch_one(&mut *tx)
                .await?;
        let mixed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM public.mirror_backfill_run) OR EXISTS(SELECT 1 FROM public.database_origin_upgrade)").fetch_one(&mut *tx).await?;
        if schema.as_ref() != Some(&super::schema_contract()) || mixed {
            return Err(OriginError::Recovery);
        }
        super::recovery_plan::verify_target(&mut tx, &plan).await?;
        let operation: Uuid = intent.try_get("recovery_operation")?;
        let updated=sqlx::query("UPDATE public.database_recovery SET completed_at=now(),verified_plan_sha256=$1 WHERE singleton AND completed_at IS NULL AND recovery_operation=$2 AND creation_operation=$3")
                .bind(hex::encode(Sha256::digest(&canonical))).bind(self.recovery_operation).bind(self.native.operation).execute(&mut *tx).await?;
        if updated.rows_affected() != 1 {
            return Err(OriginError::Recovery);
        }
        *commit_attempted = true;
        tx.commit().await?;
        Ok::<_, OriginError>(operation)
    }
}
