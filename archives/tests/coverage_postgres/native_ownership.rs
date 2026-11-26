//! Domain-linked recovery over the actual canonical schema and SDK-created history.
//! This SDK probe exercises native receipt admission, not the `EmailSummary` controller.

use super::*;
use futures::FutureExt as _;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use store::email_receipts::summary_recovery::{self, DeliveryRule};
use store::project_slack_deliveries::{
    self,
    reconciliation::{self, Decision, Request},
};
use uuid::Uuid;

#[derive(Clone, Copy)]
struct Domain {
    receipt: Uuid,
    operation: Uuid,
    epoch: Uuid,
    retained: Uuid,
    unresolved: Uuid,
    deleted: Uuid,
}

#[derive(Debug, Clone, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[error("native archive admission failed")]
struct ProbeError;

#[tokio::test]
async fn canonical_native_owners_and_deleted_project_audit_restore_with_real_sdk_checkpoints() {
    let port = store::test_support::postgres_test_port();
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        );
    let admin = PgPool::connect_with(options.clone()).await.unwrap();
    // This constructor alone supplies known-fresh native ownership BEFORE any data.
    let fixture = store::test_support::PostgresFixture::create(options.clone())
        .await
        .unwrap();
    let mut owned = Vec::new();
    let result=std::panic::AssertUnwindSafe(async {
        let id=Uuid::now_v7().simple().to_string();
        for name in [format!("native_forge_{id}"),format!("native_restore_forge_{id}")] {
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}"))).execute(&admin).await.unwrap();
            owned.push((name.clone(), None));
            let pool=PgPool::connect_with(admin.connect_options().as_ref().clone().database(&name)).await.unwrap();
            owned.last_mut().unwrap().1=Some(pool);
        }
        let forge=owned[0].1.as_ref().unwrap().clone();let forge_target=owned[1].1.as_ref().unwrap().clone();
        for pool in [&forge,&forge_target] {sqlx::raw_sql(FORGE_SCHEMA).execute(pool).await.unwrap();}
        sqlx::query("INSERT INTO forge.repository VALUES(1,'synthetic',true,decode('00ff','hex'),'{\"native\":true}'::jsonb)")
            .execute(&forge).await.unwrap();
        roundtrip(fixture.postgres(),&options,&mut owned,&forge,&forge_target,&port.to_string(),2).await;
    }).catch_unwind().await;
    let mut cleanup_failed = false;
    for (name, pool) in owned {
        if let Some(pool) = pool {
            pool.close().await;
        }
        if sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP DATABASE {name}")))
            .execute(&admin)
            .await
            .is_err()
        {
            cleanup_failed = true;
        }
    }
    cleanup_failed |= fixture.close().await.is_err();
    admin.close().await;
    if let Err(panic) = result {
        if cleanup_failed {
            eprintln!("owned recovery fixture cleanup also failed");
        }
        std::panic::resume_unwind(panic);
    }
    assert!(!cleanup_failed, "owned recovery fixture cleanup failed");
}

async fn parents(pg: &PostgresDb) -> (Uuid, Uuid, String) {
    let (person, firm, entity, kind, jurisdiction) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    sqlx::query(
        "INSERT INTO person(id,name,email,role) VALUES($1,'Synthetic Operator',$2,'owner')",
    )
    .bind(person)
    .bind(format!("{person}@example.test"))
    .execute(pg.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO entity_type(id,name) VALUES($1,$2)")
        .bind(kind)
        .bind(kind.to_string())
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO jurisdiction(id,name) VALUES($1,$2)")
        .bind(jurisdiction)
        .bind(jurisdiction.to_string())
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO entity(id,name,entity_type_id,jurisdiction_id) VALUES($1,'Synthetic Entity',$2,$3)")
        .bind(entity).bind(kind).bind(jurisdiction).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO firm(id,name,status,slug,entity_id) VALUES($1,'Synthetic Practice','active',$2,$3)")
        .bind(firm).bind(firm.to_string()).bind(entity).execute(pg.pool()).await.unwrap();
    let brand = format!("archive-{}", Uuid::now_v7().simple());
    sqlx::query("INSERT INTO brand(name,brand_key,firm_id) VALUES('Synthetic Brand '||$1,$1,$2)")
        .bind(&brand)
        .bind(firm)
        .execute(pg.pool())
        .await
        .unwrap();
    (person, entity, brand)
}

async fn seed(pg: &PostgresDb) -> Domain {
    let (person, entity, brand) = parents(pg).await;
    let firm: Uuid = sqlx::query_scalar("SELECT firm_id FROM brand WHERE brand_key=$1")
        .bind(&brand)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    let epoch: Uuid =
        sqlx::query_scalar("SELECT ownership_epoch FROM durable_object_ownership WHERE singleton")
            .fetch_one(pg.pool())
            .await
            .unwrap();
    let mut projects = Vec::new();
    for _ in 0..3 {
        let project = store::postgres::projects::create_on(
            pg,
            &store::projects::NewProject {
                code: Uuid::now_v7().to_string(),
                name: "Synthetic Archive Project".into(),
                status: "open".into(),
                brand: brand.clone(),
                entity_id: entity,
                firm_id: Some(firm),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // Controlled unresolved evidence fixture; this is not a captured-source attestation.
        sqlx::query("UPDATE project_slack_handover SET client_view_disposition='unresolved' WHERE project_id=$1")
            .bind(project.id).execute(pg.pool()).await.unwrap();
        projects.push(project.id);
    }
    sqlx::query("INSERT INTO project_status_transition(project_id,to_status) VALUES($1,'open')")
        .bind(projects[1])
        .execute(pg.pool())
        .await
        .unwrap();
    for project in &projects[..2] {
        reconciliation::reconcile(
            pg,
            Request {
                operation_id: Uuid::now_v7(),
                project_id: *project,
                ownership_epoch: epoch,
                actor_person_id: person,
                evidence_reference: Uuid::now_v7(),
                decision: Decision::Posted,
            },
        )
        .await
        .unwrap();
    }
    let source = store::surreal::SurrealDb::uninitialized();
    let receipt = PostgresDb::with_test_db(pg.clone(), async {
        let mailbox = format!("archive-{firm}@example.test");
        store::projects::delete_project_with_surreal(&source, projects[0])
            .await
            .unwrap();
        let receipt = store::email_receipts::ensure(
            &source,
            &store::email_receipts::NewEmailReceipt {
                receiving_mailbox: &mailbox,
                deployment: "synthetic",
                firm_id: Some(firm),
                provider_account: None,
                sender_authenticated: false,
                raw_digest: "native-archive-digest",
                source_message_id: None,
                archive_key: "synthetic/mail.eml",
                letter_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap()
        .receipt
        .id;
        store::email_deliveries::ensure(&source, receipt, "C-SYNTHETIC")
            .await
            .unwrap();
        store::email_deliveries::mark_confirmed(&source, receipt, "C-SYNTHETIC", "synthetic-post")
            .await
            .unwrap();
        receipt
    })
    .await;
    Domain {
        receipt,
        operation: Uuid::now_v7(),
        epoch,
        retained: projects[1],
        unresolved: projects[2],
        deleted: projects[0],
    }
}

async fn sdk(
    pg: &PostgresDb,
    port: &str,
    domain: Option<Domain>,
    replay: Option<Domain>,
    calls: Arc<AtomicUsize>,
) {
    let database: String = sqlx::query_scalar("SELECT current_database()::text")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    let url = format!("postgresql://postgres:synthetic@127.0.0.1:{port}/{database}");
    let mut config = dbos::Config::new("native-ownership-archive", &url);
    config.schema = "dbos".into();
    config.app_version = Some("native-ownership-archive-v1".into());
    let runtime = dbos::DBOS::new(config);
    let native = pg.clone();
    let workflow = runtime
        .register_workflow(
            "native-archive-recovery-probe",
            move |(receipt, operation): (Uuid, Uuid)| {
                let native = native.clone();
                let calls = Arc::clone(&calls);
                async move {
                    dbos::step("claim-native-receipt-owner", move || {
                        let native = native.clone();
                        let calls = Arc::clone(&calls);
                        async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            summary_recovery::claim(
                                &native,
                                receipt,
                                operation,
                                None,
                                "native-archive-digest",
                                DeliveryRule::PreserveConfirmed,
                            )
                            .await
                            .map_err(|_| dbos::Error::Application(ProbeError))?;
                            Ok::<_, dbos::Error<ProbeError>>(operation)
                        }
                    })
                    .await
                }
            },
        )
        .unwrap();
    runtime.launch().await.unwrap();
    if let Some(domain) = domain.or(replay) {
        let id = format!("native-archive-recovery:{}", domain.operation);
        let result = workflow
            .run_with(
                (domain.receipt, domain.operation),
                dbos::RunOptions {
                    workflow_id: Some(&id),
                    ..Default::default()
                },
            )
            .await;
        runtime.shutdown().await;
        assert_eq!(result.unwrap(), domain.operation);
    } else {
        runtime.shutdown().await;
    }
}

async fn clear_sdk_target(pool: &PgPool) {
    let names: Vec<String> =
        sqlx::query_scalar("SELECT tablename::text FROM pg_tables WHERE schemaname='dbos'")
            .fetch_all(pool)
            .await
            .unwrap();
    let names = names
        .iter()
        .map(|name| format!("dbos.\"{name}\""))
        .collect::<Vec<_>>()
        .join(",");
    // Only the owned, schema-only target; no source or fabricated SDK history.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("TRUNCATE {names} CASCADE")))
        .execute(pool)
        .await
        .unwrap();
}

struct PreparedTarget {
    pending: store::postgres::native_origin::recovery::PendingRecovery,
    before_origin: serde_json::Value,
    target_index: usize,
}

async fn prepare_target(
    options: &PgConnectOptions,
    owned: &mut Vec<(String, Option<PgPool>)>,
    port: &str,
    domain: Domain,
    paired: &archives::coverage::PairedManifest,
    storage: &dyn StorageService,
) -> PreparedTarget {
    let marker = storage
        .get(&format!("postgres/_paired/{}.json", paired.generation))
        .await
        .unwrap()
        .bytes;
    let creation = Uuid::new_v4();
    let tier = store::postgres::native_origin::TierBinding {
        tier_id: Uuid::new_v4(),
        namespace_uid: Uuid::new_v4(),
        cluster_name: "native-recovery-acceptance".into(),
    };
    let created = store::postgres::native_origin::create(options.clone(), creation, tier)
        .await
        .unwrap();
    let target_name = store::postgres::native_origin::database_name(creation).unwrap();
    let target_index = owned.len();
    owned.push((target_name, None));
    let pending = created
        .initialize_recovery(
            store::postgres::native_origin::recovery::RecoveryInput::from_archive(
                Uuid::new_v4(),
                marker,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let before_origin: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(o) FROM database_origin o")
            .fetch_one(pending.postgres().pool())
            .await
            .unwrap();
    owned[target_index].1 = Some(pending.postgres().pool().clone());
    {
        let target_pg = pending.postgres();
        sdk(target_pg, port, None, None, Arc::new(AtomicUsize::new(0))).await;
        clear_sdk_target(target_pg.pool()).await;
        assert!(
            store::durable_object_handover::require_native_ownership(target_pg)
                .await
                .is_err()
        );
        assert!(matches!(
            project_slack_deliveries::admission(
                target_pg,
                domain.retained,
                "client_view",
                "client-view"
            )
            .await,
            Err(project_slack_deliveries::DeliveryError::HandoverRequired)
        ));
    }
    PreparedTarget {
        pending,
        before_origin,
        target_index,
    }
}

async fn assert_creation_evidence(
    source_pg: &PostgresDb,
    target_pg: &PostgresDb,
    before_origin: serde_json::Value,
) {
    let after_origin: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(o) FROM database_origin o")
            .fetch_one(target_pg.pool())
            .await
            .unwrap();
    assert_eq!(
        before_origin, after_origin,
        "restoration never rewrites target creation evidence"
    );
    let source_origin: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(o) FROM database_origin o")
            .fetch_one(source_pg.pool())
            .await
            .unwrap();
    assert_ne!(
        after_origin["creation_operation"], source_origin["creation_operation"],
        "source identity does not become target creation evidence"
    );
}

async fn roundtrip(
    source_pg: &PostgresDb,
    options: &PgConnectOptions,
    owned: &mut Vec<(String, Option<PgPool>)>,
    forge: &PgPool,
    forge_target: &PgPool,
    port: &str,
    repeat: u8,
) {
    let domain = seed(source_pg).await;
    let empty_largeobject: i64 = sqlx::query_scalar("SELECT lo_create(0)::bigint")
        .fetch_one(source_pg.pool())
        .await
        .unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    sdk(source_pg, port, Some(domain), None, Arc::clone(&calls)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        summary_recovery::current_operation(source_pg, domain.receipt)
            .await
            .unwrap(),
        Some(domain.operation)
    );
    let nav = manifest(source_pg.pool()).await;
    let forge_manifest = manifest(forge).await;
    let dir = tempfile::tempdir().unwrap();
    let storage = ZeroDelete {
        inner: FsStorage::new(dir.path()).await.unwrap(),
    };
    let base = match repeat {
        2 => "memory://native-ownership",
        1 => "file:///synthetic/native-ownership",
        _ => "gs://synthetic.invalid/native-ownership",
    };
    let paired = export_pair(
        source("navigator", source_pg.pool(), &nav),
        source("forgejo", forge, &forge_manifest),
        &storage,
        base,
    )
    .await
    .unwrap();
    assert!(
        !paired.cross_database_atomic,
        "two databases do not share a recovery point"
    );
    if repeat == 2 {
        rejects_mutations_during_pool_drain(options, owned, port, domain, &paired, &storage, base)
            .await;
    }
    let PreparedTarget {
        pending,
        before_origin,
        target_index,
    } = prepare_target(options, owned, port, domain, &paired, &storage).await;
    archives::recovery::rehydrate(&pending, &storage, base)
        .await
        .unwrap();
    restore::restore_forgejo(&paired, &storage, base, forge_target, pending.postgres()).await;
    let store::postgres::native_origin::recovery::CompletedRecovery::Open(recovered) =
        pending.verify_and_complete().await.unwrap()
    else {
        panic!("verified recovery must reopen this owned target");
    };
    let target_pg = recovered.postgres();
    owned[target_index].1 = Some(target_pg.pool().clone());
    assert_creation_evidence(source_pg, target_pg, before_origin).await;
    require_scope(target_pg, Family::Satellites).await.unwrap();
    let empty_restored:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_largeobject_metadata WHERE oid=$1::oid) AND NOT EXISTS(SELECT 1 FROM pg_largeobject WHERE loid=$1::oid)").bind(empty_largeobject).fetch_one(target_pg.pool()).await.unwrap();
    assert!(
        empty_restored,
        "an empty large object has identity even without data pages"
    );

    restore::assert_native_database_equal(&paired.databases[0], source_pg.pool(), target_pg.pool())
        .await;
    restore::assert_database_equal(&paired.databases[1], forge, forge_target).await;
    assert_native_projection(target_pg, domain).await;
    reject_target_only_fixed_reference(&paired.databases[0], source_pg.pool(), target_pg.pool())
        .await;
    check_incoherent_epoch(target_pg, domain).await;
    let replayed = Arc::new(AtomicUsize::new(0));
    sdk(target_pg, port, None, Some(domain), Arc::clone(&replayed)).await;
    assert_eq!(
        replayed.load(Ordering::SeqCst),
        0,
        "restored real SDK checkpoint/result must not rerun native admission"
    );
    assert_native_projection(target_pg, domain).await;
    assert_identity_continuity(source_pg.pool(), target_pg.pool()).await;
    if repeat > 0 {
        let second = prepare_forgejo_target(options, owned).await;
        Box::pin(roundtrip(
            target_pg,
            options,
            owned,
            forge,
            &second,
            port,
            repeat - 1,
        ))
        .await;
    }
}

/// Start from the same valid archive and fully rehydrated rows as the positive case.
async fn rejects_mutations_during_pool_drain(
    options: &PgConnectOptions,
    owned: &mut Vec<(String, Option<PgPool>)>,
    port: &str,
    domain: Domain,
    paired: &archives::coverage::PairedManifest,
    storage: &dyn StorageService,
    base: &str,
) {
    use store::postgres::native_origin::{recovery::CompletedRecovery, OriginError};
    let PreparedTarget {
        pending,
        before_origin,
        target_index,
    } = prepare_target(options, owned, port, domain, paired, storage).await;
    archives::recovery::rehydrate(&pending, storage, base)
        .await
        .unwrap();
    let creation = pending.creation_operation();
    let pool = pending.postgres().pool().clone();
    let mut held = pool.acquire().await.unwrap();
    let expected: serde_json::Value =
        sqlx::query_scalar("SELECT public.database_origin_schema_contract()")
            .fetch_one(&mut *held)
            .await
            .unwrap();
    let original: String = sqlx::query_scalar("SELECT prosrc FROM pg_proc WHERE oid='public.database_origin_schema_contract()'::regprocedure")
        .fetch_one(&mut *held).await.unwrap();
    let tamper = async move {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while !pool.is_closed() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("completion must pass its preflight and request pool drain");
        // Same result and function attributes: only its forbidden implementation changes.
        let literal = serde_json::to_string(&expected)
            .unwrap()
            .replace('\'', "''");
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE OR REPLACE FUNCTION public.database_origin_schema_contract() RETURNS jsonb \
             LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog AS $tamper$ \
             BEGIN RETURN '{literal}'::jsonb; END; $tamper$;"
        )))
        .execute(&mut *held)
        .await
        .unwrap();
        let unchanged: serde_json::Value =
            sqlx::query_scalar("SELECT public.database_origin_schema_contract()")
                .fetch_one(&mut *held)
                .await
                .unwrap();
        assert_eq!(
            unchanged, expected,
            "ledger output cannot be the refusal cause"
        );
        let changed: String = sqlx::query_scalar("SELECT prosrc FROM pg_proc WHERE oid='public.database_origin_schema_contract()'::regprocedure")
            .fetch_one(&mut *held).await.unwrap();
        assert_ne!(changed, original);
        held.close().await.unwrap();
    };
    // No detached task can survive an assertion or fixture cleanup.
    let (result, ()) = tokio::join!(pending.verify_and_complete(), tamper);
    let refused = matches!(&result, Err(OriginError::Recovery));
    let outcome = match &result {
        Ok(CompletedRecovery::Open(_)) => "opened",
        Ok(CompletedRecovery::CommitUnconfirmed { .. }) => "commit unconfirmed",
        Ok(CompletedRecovery::ReopenUnconfirmed { .. }) => "reopen unconfirmed",
        Err(_) => "refused",
    };
    if let Ok(CompletedRecovery::Open(native)) = &result {
        native.postgres().pool().close().await;
    }
    assert!(
        refused,
        "post-drain definition tampering must refuse before completion: {outcome}"
    );
    assert_sealed_pending_target(options, creation, before_origin, owned, target_index).await;
    rejects_helper_changed_during_pool_drain(options, owned, port, domain, paired, storage, base)
        .await;
}

/// Reuse a valid archive and target; only an ordinary standalone helper body changes.
async fn rejects_helper_changed_during_pool_drain(
    options: &PgConnectOptions,
    owned: &mut Vec<(String, Option<PgPool>)>,
    port: &str,
    domain: Domain,
    paired: &archives::coverage::PairedManifest,
    storage: &dyn StorageService,
    base: &str,
) {
    use store::postgres::native_origin::{recovery::CompletedRecovery, OriginError};
    let PreparedTarget {
        pending,
        before_origin,
        target_index,
    } = prepare_target(options, owned, port, domain, paired, storage).await;
    archives::recovery::rehydrate(&pending, storage, base)
        .await
        .unwrap();
    let creation = pending.creation_operation();
    let pool = pending.postgres().pool().clone();
    let mut held = pool.acquire().await.unwrap();
    let original: String = sqlx::query_scalar(
        "SELECT pg_get_functiondef('public.graph_project_attach(text)'::regprocedure)",
    )
    .fetch_one(&mut *held)
    .await
    .unwrap();
    let tamper = async move {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while !pool.is_closed() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("valid target must pass preflight and request pool drain");
        sqlx::raw_sql("CREATE OR REPLACE FUNCTION public.graph_project_attach(seed text) RETURNS SETOF ag_catalog.agtype LANGUAGE plpgsql SET search_path=public,ag_catalog AS $tamper$ BEGIN RETURN NEXT '{\"kind\":\"synthetic-recovery-drift\"}'::agtype; END; $tamper$")
            .execute(&mut *held).await.unwrap();
        let output: String =
            sqlx::query_scalar("SELECT v::text FROM public.graph_project_attach('{}') AS t(v)")
                .fetch_one(&mut *held)
                .await
                .unwrap();
        assert!(
            output.contains("synthetic-recovery-drift"),
            "the actual helper behavior changed"
        );
        let changed: String = sqlx::query_scalar(
            "SELECT pg_get_functiondef('public.graph_project_attach(text)'::regprocedure)",
        )
        .fetch_one(&mut *held)
        .await
        .unwrap();
        assert_ne!(changed, original);
        held.close().await.unwrap();
    };
    let (result, ()) = tokio::join!(pending.verify_and_complete(), tamper);
    let refused = matches!(&result, Err(OriginError::Recovery));
    let outcome = match &result {
        Ok(CompletedRecovery::Open(_)) => "opened",
        Ok(CompletedRecovery::CommitUnconfirmed { .. }) => "commit unconfirmed",
        Ok(CompletedRecovery::ReopenUnconfirmed { .. }) => "reopen unconfirmed",
        Err(_) => "refused",
    };
    if let Ok(CompletedRecovery::Open(native)) = &result {
        native.postgres().pool().close().await;
    }
    assert!(
        refused,
        "post-drain standalone helper drift must refuse: {outcome}"
    );
    assert_sealed_pending_target(options, creation, before_origin, owned, target_index).await;
}

async fn assert_sealed_pending_target(
    options: &PgConnectOptions,
    creation: Uuid,
    before_origin: serde_json::Value,
    owned: &mut [(String, Option<PgPool>)],
    target_index: usize,
) {
    let name = store::postgres::native_origin::database_name(creation).unwrap();
    let admin = PgPool::connect_with(options.clone()).await.unwrap();
    let (oid, allowed): (i64, bool) =
        sqlx::query_as("SELECT oid::bigint,datallowconn FROM pg_database WHERE datname=$1")
            .bind(&name)
            .fetch_one(&admin)
            .await
            .unwrap();
    assert_eq!(Some(oid), before_origin["database_oid"].as_i64());
    assert!(
        !allowed,
        "failed verification must retain its connection fence"
    );
    // Only this UUID-created negative fixture is reopened to inspect retained state.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "ALTER DATABASE \"{name}\" ALLOW_CONNECTIONS true"
    )))
    .execute(&admin)
    .await
    .unwrap();
    let inspect = PgPool::connect_with(options.clone().database(&name))
        .await
        .unwrap();
    assert_eq!(owned[target_index].0, name);
    owned[target_index].1 = Some(inspect.clone());
    let pending: (bool, bool) = sqlx::query_as(
        "SELECT completed_at IS NULL,verified_plan_sha256 IS NULL FROM database_recovery WHERE singleton",
    ).fetch_one(&inspect).await.unwrap();
    assert_eq!(pending, (true, true));
    let origin: serde_json::Value = sqlx::query_scalar("SELECT to_jsonb(o) FROM database_origin o")
        .fetch_one(&inspect)
        .await
        .unwrap();
    assert_eq!(origin, before_origin);
    inspect.close().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "ALTER DATABASE \"{name}\" ALLOW_CONNECTIONS false"
    )))
    .execute(&admin)
    .await
    .unwrap();
    admin.close().await;
}

async fn prepare_forgejo_target(
    options: &PgConnectOptions,
    owned: &mut Vec<(String, Option<PgPool>)>,
) -> PgPool {
    let admin = PgPool::connect_with(options.clone()).await.unwrap();
    let name = format!("native_repeat_forge_{}", Uuid::new_v4().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    owned.push((name.clone(), None));
    let second = PgPool::connect_with(options.clone().database(&name))
        .await
        .unwrap();
    owned.last_mut().unwrap().1 = Some(second.clone());
    sqlx::raw_sql(FORGE_SCHEMA).execute(&second).await.unwrap();
    admin.close().await;
    second
}

async fn assert_native_projection(pg: &PostgresDb, domain: Domain) {
    for table in [
        "durable_object_handover",
        "durable_object_ownership",
        "project_slack_handover",
        "project_slack_delivery",
        "project_slack_delivery_reconciliation",
    ] {
        let count: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(pg.pool())
                .await
                .unwrap();
        assert!(count > 0, "the roundtrip must exercise {table}");
    }
    assert_eq!(
        summary_recovery::current_operation(pg, domain.receipt)
            .await
            .unwrap(),
        Some(domain.operation)
    );
    let epoch: Uuid =
        sqlx::query_scalar("SELECT ownership_epoch FROM durable_object_ownership WHERE singleton")
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert_eq!(epoch, domain.epoch);
    assert_eq!(
        project_slack_deliveries::admission(pg, domain.retained, "client_view", "client-view")
            .await
            .unwrap(),
        project_slack_deliveries::Admission::Posted
    );
    assert!(matches!(
        project_slack_deliveries::admission(pg, domain.unresolved, "client_view", "client-view")
            .await,
        Err(project_slack_deliveries::DeliveryError::Unresolved)
    ));
    let deleted: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM project WHERE id=$1)")
        .bind(domain.deleted)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert!(
        !deleted,
        "restoring audit must not recreate its deleted Project"
    );
    let operation: Uuid = sqlx::query_scalar(
        "SELECT operation_id FROM project_slack_delivery_reconciliation WHERE project_id=$1",
    )
    .bind(domain.deleted)
    .fetch_one(pg.pool())
    .await
    .unwrap();
    for sql in ["UPDATE project_slack_delivery_reconciliation SET decision='verified_not_posted' WHERE operation_id=$1","DELETE FROM project_slack_delivery_reconciliation WHERE operation_id=$1"] {
        assert!(sqlx::query(sql).bind(operation).execute(pg.pool()).await.is_err(),"immutable trigger must survive restoration");
    }
}

async fn check_incoherent_epoch(pg: &PostgresDb, domain: Domain) {
    sqlx::query("UPDATE project_slack_handover SET ownership_epoch=$2 WHERE project_id=$1")
        .bind(domain.retained)
        .bind(Uuid::now_v7())
        .execute(pg.pool())
        .await
        .unwrap();
    assert!(matches!(
        project_slack_deliveries::admission(pg, domain.retained, "client_view", "client-view")
            .await,
        Err(project_slack_deliveries::DeliveryError::HandoverRequired)
    ));
    // Correct only this owned negative-fixture mutation before replaying the restored SDK.
    sqlx::query("UPDATE project_slack_handover SET ownership_epoch=$2 WHERE project_id=$1")
        .bind(domain.retained)
        .bind(domain.epoch)
        .execute(pg.pool())
        .await
        .unwrap();
}

async fn reject_target_only_fixed_reference(
    manifest: &archives::coverage::DatabaseManifest,
    source: &PgPool,
    target: &PgPool,
) {
    let srid:i32=sqlx::query_scalar("SELECT candidate FROM generate_series(990000,990100) AS candidate WHERE NOT EXISTS(SELECT 1 FROM spatial_ref_sys WHERE srid=candidate) LIMIT 1")
        .fetch_one(target).await.unwrap();
    sqlx::query("INSERT INTO spatial_ref_sys(srid,auth_name,auth_srid,srtext,proj4text) VALUES($1,'synthetic',1,'synthetic target-only reference','')")
        .bind(srid).execute(target).await.unwrap();
    let refused = std::panic::AssertUnwindSafe(restore::assert_native_database_equal(
        manifest, source, target,
    ))
    .catch_unwind()
    .await;
    sqlx::query("DELETE FROM spatial_ref_sys WHERE srid=$1")
        .bind(srid)
        .execute(target)
        .await
        .unwrap();
    assert!(
        refused.is_err(),
        "fixed-reference conflict handling must never certify target-only rows"
    );
    restore::assert_native_database_equal(manifest, source, target).await;
}

async fn assert_identity_continuity(source: &PgPool, restored: &PgPool) {
    let next_sql = "SELECT nextval(pg_get_serial_sequence('project_status_transition','sequence'))";
    let source_next: i64 = sqlx::query_scalar(next_sql)
        .fetch_one(source)
        .await
        .unwrap();
    let restored_next: i64 = sqlx::query_scalar(next_sql)
        .fetch_one(restored)
        .await
        .unwrap();
    assert_eq!(
        restored_next, source_next,
        "restored identity must continue the source counter"
    );
    let highest_restored: i64 =
        sqlx::query_scalar("SELECT max(sequence) FROM project_status_transition")
            .fetch_one(restored)
            .await
            .unwrap();
    assert!(
        restored_next > highest_restored,
        "the next identity must not collide with a restored row"
    );
}
