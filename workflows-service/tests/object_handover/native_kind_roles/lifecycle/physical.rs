//! Opt-in physical recovery on the same verified synthetic tier as role acceptance.
//! Local WAL transport is deliberate: this does not certify offsite IAM/retention.
use super::super::{healthy, process, Tier};
use super::Pending;
use serde_json::json;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use store::postgres::PostgresDb;
use tokio::sync::Mutex;
use uuid::Uuid;
use workflows::dbos_engine;

#[path = "physical/resources.rs"]
mod resources;
#[path = "physical/sdk.rs"]
mod sdk;

const DEADLINE: Duration = Duration::from_secs(30);

struct Fixture {
    pending: Pending,
    tier: Tier,
    scratch: resources::Scratch,
    source: Option<PostgresDb>,
    restored: Option<PostgresDb>,
    worker: Option<dbos::DBOS>,
    forward: Option<process::Forward>,
    database: String,
    operation: String,
    probe: Option<sdk::Probe>,
    tables_created: bool,
    slot_created: bool,
    success: bool,
}

impl Fixture {
    fn open() -> Self {
        let pending = Pending::read();
        let tier = Tier::open(&pending.operation);
        let database = std::env::var("NAVIGATOR_POSTGRES_DATABASE").unwrap();
        assert_eq!(
            database,
            store::postgres::native_origin::database_name(
                Uuid::parse_str(database.strip_prefix("navigator_native_").unwrap()).unwrap()
            )
            .unwrap()
        );
        pending.record(
            "physical_source",
            json!({"database":database,"context":tier.context,
            "namespace_uid":tier.namespace_uid,"postgres_pod_uid":tier.pod_uid}),
        );
        let scratch = resources::Scratch::new(&tier);
        Self {
            operation: format!("synthetic-physical-{}", pending.operation),
            pending,
            tier,
            scratch,
            source: None,
            restored: None,
            worker: None,
            forward: None,
            database,
            probe: None,
            tables_created: false,
            slot_created: false,
            success: false,
        }
    }

    async fn initialize(&mut self) {
        let source = PostgresDb::connect(self.tier.options(&self.database, 1))
            .await
            .unwrap();
        assert!(store::postgres::native_origin::admits(&source)
            .await
            .unwrap());
        self.source = Some(source);
        let app = PostgresDb::connect(self.tier.options(&self.database, 2))
            .await
            .unwrap();
        store::postgres_backfill::family::require_runtime_scopes(&app)
            .await
            .unwrap();
        // No IF NOT EXISTS: leftovers are unresolved ownership, not an empty fixture.
        self.tables_created = true;
        sqlx::raw_sql("CREATE TABLE synthetic_physical_effect(effect_key text PRIMARY KEY); \
            CREATE TABLE synthetic_physical_receipt(workflow_id text PRIMARY KEY); \
            CREATE TABLE synthetic_physical_marker(id uuid PRIMARY KEY, observed_at timestamptz NOT NULL);")
            .execute(self.source.as_ref().unwrap().pool()).await.unwrap();
        let probe = sdk::Probe::new(app);
        self.worker = Some(self.worker_at(self.tier.port, sdk::VERSION));
        self.probe = Some(probe);
    }

    fn worker_at(&self, port: u16, version: &str) -> dbos::DBOS {
        self.worker_for(port, version, sdk::EXECUTOR)
    }

    fn worker_for(&self, port: u16, version: &str, executor: &str) -> dbos::DBOS {
        let mut url = reqwest::Url::parse("postgresql://127.0.0.1/").unwrap();
        url.set_port(Some(port)).unwrap();
        url.set_username("navigator_dbos").unwrap();
        url.set_password(Some(&self.tier.passwords[3])).unwrap();
        url.set_path(&self.database);
        url.query_pairs_mut()
            .append_pair("sslmode", "verify-full")
            .append_pair("sslrootcert", self.tier.ca.to_str().unwrap());
        dbos::DBOS::new(dbos_engine::config_with_executor_id(
            url.as_str(),
            version,
            executor,
        ))
    }

    async fn prepare_pending(&self) {
        let worker = self.worker.as_ref().unwrap();
        let probe = self.probe.as_ref().unwrap();
        let workflow = sdk::register(worker, probe.clone());
        worker.launch().await.unwrap();
        let complete = format!("{}-complete", self.operation);
        tokio::time::timeout(
            DEADLINE,
            workflow.run_with(
                false,
                dbos::RunOptions {
                    workflow_id: Some(&complete),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let _started = workflow
            .start_with(
                true,
                dbos::StartOptions {
                    workflow_id: Some(&self.operation),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        tokio::time::timeout(DEADLINE, probe.reached.notified())
            .await
            .unwrap();
        let steps = worker.list_workflow_steps(&self.operation).await.unwrap();
        assert!(steps.iter().any(|step| step.step_name == "provider"
            && step.completed_at.is_some()
            && step.error.is_none()));
        assert!(!steps
            .iter()
            .any(|step| step.step_name == "receipt" && step.completed_at.is_some()));
        assert_eq!(probe.posts.load(Ordering::SeqCst), 2);
        tokio::time::timeout(DEADLINE, worker.shutdown())
            .await
            .unwrap();
        self.assert_pending(self.source.as_ref().unwrap(), self.tier.port)
            .await;
    }

    async fn assert_pending(&self, pg: &PostgresDb, port: u16) {
        let dbos = PostgresDb::connect(self.tier.options(&self.database, 3).port(port))
            .await
            .unwrap();
        let row:(String,String,String)=sqlx::query_as("SELECT status,application_version,executor_id FROM dbos.workflow_status WHERE workflow_uuid=$1")
            .bind(&self.operation).fetch_one(dbos.pool()).await.unwrap();
        tokio::time::timeout(DEADLINE, dbos.pool().close())
            .await
            .unwrap();
        assert_eq!(
            row,
            ("PENDING".into(), sdk::VERSION.into(), sdk::EXECUTOR.into())
        );
        let absent: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM synthetic_physical_receipt WHERE workflow_id=$1",
        )
        .bind(&self.operation)
        .fetch_one(pg.pool())
        .await
        .unwrap();
        assert_eq!(absent, 0);
    }

    async fn receive_base_and_post_base_wal(
        &mut self,
    ) -> (Uuid, chrono::DateTime<chrono::Utc>, Instant) {
        self.pending
            .record("physical_scratch_name", json!(self.scratch.name));
        let slot = format!("physical_{}", self.pending.operation);
        let absent: bool = sqlx::query_scalar(
            "SELECT NOT EXISTS(SELECT FROM pg_replication_slots WHERE slot_name=$1)",
        )
        .bind(&slot)
        .fetch_one(self.source.as_ref().unwrap().pool())
        .await
        .unwrap();
        assert!(absent, "an existing replication slot is never adopted");
        self.pending
            .record("physical_slot_creation_intent", json!(slot));
        self.scratch.create(&self.tier);
        self.slot_created = true;
        self.pending
            .record("physical_scratch_uid", json!(self.scratch.pod_uid));
        let pg = self.source.as_ref().unwrap();
        let slot = format!("physical_{}", self.pending.operation);
        tokio::time::timeout(DEADLINE, async {
            loop {
                let active: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT FROM pg_replication_slots WHERE slot_name=$1 AND active)",
                )
                .bind(&slot)
                .fetch_one(pg.pool())
                .await
                .unwrap();
                if active {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        let began = Instant::now();
        self.scratch
            .exec(&self.tier, &["/bin/sh", "/scripts/base-backup.sh"]);
        let marker = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO synthetic_physical_marker(id,observed_at) VALUES($1,clock_timestamp())",
        )
        .bind(marker)
        .execute(pg.pool())
        .await
        .unwrap();
        let committed: chrono::DateTime<chrono::Utc> =
            sqlx::query_scalar("SELECT clock_timestamp()")
                .fetch_one(pg.pool())
                .await
                .unwrap();
        let lsn: String = sqlx::query_scalar("SELECT pg_current_wal_flush_lsn()::text")
            .fetch_one(pg.pool())
            .await
            .unwrap();
        let admin = PostgresDb::connect(self.tier.options(&self.database, 0))
            .await
            .unwrap();
        let _: String = sqlx::query_scalar("SELECT pg_switch_wal()::text")
            .fetch_one(admin.pool())
            .await
            .unwrap();
        tokio::time::timeout(DEADLINE,async {
            loop {
                let ready:bool=sqlx::query_scalar("SELECT EXISTS(SELECT FROM pg_stat_replication WHERE application_name=$1 AND flush_lsn >= $2::text::pg_lsn)")
                    .bind(&slot).bind(&lsn).fetch_one(admin.pool()).await.unwrap();
                if ready {break;} tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }).await.unwrap();
        tokio::time::timeout(DEADLINE, admin.pool().close())
            .await
            .unwrap();
        self.pending.record(
            "physical_cutoff",
            json!({"marker":marker,"committed_observed":committed,"lsn":lsn}),
        );
        (marker, committed, began)
    }

    fn replay_and_verify(&self) -> Instant {
        let began = Instant::now();
        self.scratch.stop_receiver(&self.tier);
        let inventory = self.scratch.exec(
            &self.tier,
            &[
                "/bin/sh",
                "-ec",
                "cat /backup/stamp; sha256sum /backup/base/* /wal/spool/*",
            ],
        );
        self.pending.record(
            "physical_file_inventory",
            json!(String::from_utf8(inventory).unwrap()),
        );
        self.scratch.exec(
            &self.tier,
            &[
                "/bin/sh",
                "-ec",
                "mkdir -p /restore/base /restore/wal; cp /backup/base/* /restore/base/; \
             cp /backup/stamp /restore/base-stamp; cp /wal/spool/* /restore/wal/; \
             test -s /restore/base/base.tar.gz; test -s /restore/base/pg_wal.tar.gz; \
             find /restore/wal -type f | grep -q .",
            ],
        );
        // The exact production script remains verify-and-stop; no provider executor is added to it.
        let report = self
            .scratch
            .exec(&self.tier, &["/bin/sh", "/scripts/restore.sh"]);
        self.pending.record(
            "physical_readonly_verifier",
            json!(String::from_utf8(report).unwrap()),
        );
        self.scratch.exec(&self.tier,&["pg_ctl","--pgdata=/restore/pgdata","--log=/restore/resume.log",
            "--wait","--timeout=30","--options=-c listen_addresses=0.0.0.0 -c unix_socket_directories=/tmp -c ssl=on -c ssl_cert_file=/etc/postgres/tls/tls.crt -c ssl_key_file=/etc/postgres/tls/tls.key -c hba_file=/etc/postgres/config/pg_hba.conf -c archive_mode=off","start"]);
        began
    }

    async fn connect_restored(&mut self) -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        self.forward = Some(process::Forward::start(
            &self.tier.health,
            &mut self.tier.kubectl(&[
                "port-forward",
                "--address=127.0.0.1",
                &format!("pod/{}", self.scratch.name),
                &format!("{port}:5432"),
            ]),
        ));
        let options = self.tier.options(&self.database, 2).port(port);
        let pg = tokio::time::timeout(DEADLINE, async {
            loop {
                if let Ok(pg) = PostgresDb::connect(options.clone()).await {
                    break pg;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        // Bind the actual transport to copied native identity before any SDK launch/effect.
        assert!(store::postgres::native_origin::admits(&pg).await.unwrap());
        store::postgres_backfill::family::require_runtime_scopes(&pg)
            .await
            .unwrap();
        let original: serde_json::Value =
            sqlx::query_scalar("SELECT to_jsonb(database_origin) FROM database_origin")
                .fetch_one(self.source.as_ref().unwrap().pool())
                .await
                .unwrap();
        let restored: serde_json::Value =
            sqlx::query_scalar("SELECT to_jsonb(database_origin) FROM database_origin")
                .fetch_one(pg.pool())
                .await
                .unwrap();
        assert_eq!(original, restored);
        self.assert_pending(&pg, port).await;
        self.restored = Some(pg);
        port
    }

    async fn restored_roles_and_version(&self, port: u16) {
        let source = self.source.as_ref().unwrap();
        let pg = self.restored.as_ref().unwrap();
        let version: &str = "SELECT current_setting('server_version_num')";
        let original: String = sqlx::query_scalar(version)
            .fetch_one(source.pool())
            .await
            .unwrap();
        let restored: String = sqlx::query_scalar(version)
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert_eq!(
            original, restored,
            "same deployed PostgreSQL image/version required"
        );
        let role: String = sqlx::query_scalar("SELECT current_user::text")
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert_eq!(role, "navigator_app");
        let restricted:bool=sqlx::query_scalar("SELECT NOT has_schema_privilege(current_user,'dbos','USAGE') AND NOT has_table_privilege(current_user,'database_origin','INSERT,UPDATE,DELETE,TRUNCATE') AND has_table_privilege(current_user,'synthetic_physical_effect','INSERT,SELECT')")
            .fetch_one(pg.pool()).await.unwrap();
        assert!(restricted);
        let denied =
            sqlx::query("INSERT INTO database_origin SELECT * FROM database_origin WHERE false")
                .execute(pg.pool())
                .await
                .unwrap_err();
        assert_eq!(
            denied
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("42501")
        );
        let dbos = PostgresDb::connect(self.tier.options(&self.database, 3).port(port))
            .await
            .unwrap();
        let restricted:bool=sqlx::query_scalar("SELECT current_user='navigator_dbos' AND has_schema_privilege(current_user,'dbos','USAGE') AND NOT has_schema_privilege(current_user,'public','USAGE')")
            .fetch_one(dbos.pool()).await.unwrap();
        assert!(restricted);
        tokio::time::timeout(DEADLINE, dbos.pool().close())
            .await
            .unwrap();
        self.pending
            .record("physical_server_version_num", json!(restored));
    }

    async fn recover_effects(&mut self, port: u16) {
        let pg = self.restored.as_ref().unwrap();
        let mut probe = self.probe.as_ref().unwrap().clone();
        probe.pg = pg.clone();
        probe.pause = false;
        for (version, executor) in [
            ("synthetic-wrong-physical-version", sdk::EXECUTOR),
            (sdk::VERSION, "synthetic-wrong-physical-owner"),
        ] {
            let wrong = self.worker_for(port, version, executor);
            let _workflow = sdk::register(&wrong, probe.clone());
            self.worker = Some(wrong.clone());
            wrong.launch().await.unwrap();
            tokio::time::sleep(Duration::from_millis(500)).await;
            tokio::time::timeout(DEADLINE, wrong.shutdown())
                .await
                .unwrap();
            self.worker = None;
            self.assert_pending(pg, port).await;
            assert_eq!(probe.posts.load(Ordering::SeqCst), 2);
        }
        let worker = self.worker_at(port, sdk::VERSION);
        let workflow = sdk::register(&worker, probe.clone());
        self.worker = Some(worker.clone());
        worker.launch().await.unwrap();
        tokio::time::timeout(
            DEADLINE,
            worker
                .retrieve_workflow::<(), dbos::EngineOnly>(&self.operation)
                .unwrap()
                .result(),
        )
        .await
        .unwrap()
        .unwrap();
        // The same workflow ID joins completed history; no new provider/effect execution.
        tokio::time::timeout(
            DEADLINE,
            workflow.run_with(
                true,
                dbos::RunOptions {
                    workflow_id: Some(&self.operation),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            probe.posts.load(Ordering::SeqCst),
            2,
            "provider checkpoints from both runs survive physical restore"
        );
        assert_eq!(
            probe.attempts.load(Ordering::SeqCst),
            3,
            "only the uncheckpointed effect repeats"
        );
        for table in ["synthetic_physical_effect", "synthetic_physical_receipt"] {
            let sql = match table {
                "synthetic_physical_effect" => "SELECT count(*) FROM synthetic_physical_effect",
                _ => "SELECT count(*) FROM synthetic_physical_receipt",
            };
            let count: i64 = sqlx::query_scalar(sql).fetch_one(pg.pool()).await.unwrap();
            assert_eq!(count, 2);
        }
        let steps = worker.list_workflow_steps(&self.operation).await.unwrap();
        for name in ["provider", "keyed-effect", "receipt"] {
            assert!(steps.iter().any(|step| step.step_name == name
                && step.completed_at.is_some()
                && step.error.is_none()));
        }
        tokio::time::timeout(DEADLINE, worker.shutdown())
            .await
            .unwrap();
        self.worker = None;
    }

    async fn settle_source_after_restore(&mut self) {
        // Only after the restored worker has drained. No managed DBOS row is deleted/fabricated.
        // This resolves this test's original synthetic PENDING row before the native tier is reused.
        let mut probe = self.probe.as_ref().unwrap().clone();
        probe.pause = false;
        let worker = self.worker_at(self.tier.port, sdk::VERSION);
        let _workflow = sdk::register(&worker, probe.clone());
        self.worker = Some(worker.clone());
        worker.launch().await.unwrap();
        tokio::time::timeout(
            DEADLINE,
            worker
                .retrieve_workflow::<(), dbos::EngineOnly>(&self.operation)
                .unwrap()
                .result(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            probe.posts.load(Ordering::SeqCst),
            2,
            "settling the owned source must also reuse its provider checkpoint"
        );
        tokio::time::timeout(DEADLINE, worker.shutdown())
            .await
            .unwrap();
        self.worker = None;
    }

    async fn run(&mut self) {
        self.initialize().await;
        self.prepare_pending().await;
        self.worker = None; // shutdown and the interrupted handle were both proved above.
        let (marker, committed, backup_began) = self.receive_base_and_post_base_wal().await;
        let recovery_began = self.replay_and_verify();
        let port = self.connect_restored().await;
        let pg = self.restored.as_ref().unwrap();
        let markers: i64 =
            sqlx::query_scalar("SELECT count(*) FROM synthetic_physical_marker WHERE id=$1")
                .bind(marker)
                .fetch_one(pg.pool())
                .await
                .unwrap();
        assert_eq!(markers, 1, "post-base commit requires archived WAL replay");
        let last: chrono::DateTime<chrono::Utc> =
            sqlx::query_scalar("SELECT max(observed_at) FROM synthetic_physical_marker")
                .fetch_one(pg.pool())
                .await
                .unwrap();
        let rpo = (committed - last).num_milliseconds().max(0);
        assert!(rpo <= 120_000, "verified cutoff exceeds RPO target");
        self.restored_roles_and_version(port).await;
        self.recover_effects(port).await;
        let elapsed = recovery_began.elapsed();
        assert!(elapsed <= Duration::from_secs(3600));
        let current: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&self.pending.path).unwrap()).unwrap();
        let manifest = current["physical_file_inventory"].as_str().unwrap();
        let manifest_sha = workflows::attest::sha256_hex(manifest.as_bytes());
        self.pending.record(
            "physical_receipt",
            json!({"post_base_marker_recovered":true,"physical_file_inventory_sha256":manifest_sha,
            "rpo_observation_milliseconds":rpo,"restore_to_usable_milliseconds":elapsed.as_millis(),
            "base_creation_to_usable_milliseconds":backup_began.elapsed().as_millis(),
            "provider_effects":2,"keyed_effect_rows":2,"receipt_rows":2,"step_attempts":3,
            "transport":"owned-local-receivewal-copy","offsite_recovery_proven":false}),
        );
        self.settle_source_after_restore().await;
        self.success = true;
    }

    async fn finish(&mut self) -> bool {
        if let Some(worker) = self.worker.take() {
            if tokio::time::timeout(DEADLINE, worker.shutdown())
                .await
                .is_err()
            {
                return false;
            }
        }
        if let Some(pg) = self.restored.take() {
            if tokio::time::timeout(DEADLINE, pg.pool().close())
                .await
                .is_err()
            {
                return false;
            }
        }
        if let Some(forward) = &mut self.forward {
            if !forward.close() {
                return false;
            }
        }
        self.forward = None;
        if !healthy(&self.tier.health) || self.scratch.creation_inflight {
            return false;
        }
        if self.scratch.pod_uid.is_some() {
            self.scratch.stop_receiver(&self.tier);
            // No drain proof => leave Pod/storage/config plus durable pending record for diagnosis.
            self.scratch.exec(&self.tier,&["/bin/sh","-ec", "test -f /work/receiver-drained; \
                if test -f /restore/pgdata/postmaster.pid; then pg_ctl --pgdata=/restore/pgdata --wait --timeout=30 --mode=fast stop; fi"]);
        }
        self.scratch.delete(&self.tier);
        self.drop_slot().await;
        if self.success && self.tables_created {
            sqlx::raw_sql("DROP TABLE synthetic_physical_marker,synthetic_physical_receipt,synthetic_physical_effect")
                .execute(self.source.as_ref().unwrap().pool()).await.unwrap();
        }
        if let Some(probe) = self.probe.take() {
            if tokio::time::timeout(DEADLINE, probe.pg.pool().close())
                .await
                .is_err()
            {
                return false;
            }
        }
        if let Some(pg) = self.source.take() {
            if tokio::time::timeout(DEADLINE, pg.pool().close())
                .await
                .is_err()
            {
                return false;
            }
        }
        if self.success {
            self.persist_receipt();
            self.pending.complete();
        }
        self.success
    }

    fn persist_receipt(&self) {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let bytes = std::fs::read(&self.pending.path).unwrap();
        let parent = self.pending.path.parent().unwrap();
        let path = parent.join(format!("{}.physical-recovery.json", self.pending.operation));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.write_all(&bytes).unwrap();
        file.sync_all().unwrap();
        std::fs::File::open(parent).unwrap().sync_all().unwrap();
    }

    async fn drop_slot(&self) {
        if !self.slot_created {
            return;
        }
        let admin = PostgresDb::connect(self.tier.options(&self.database, 0))
            .await
            .unwrap();
        let slot = format!("physical_{}", self.pending.operation);
        let active: bool =
            sqlx::query_scalar("SELECT active FROM pg_replication_slots WHERE slot_name=$1")
                .bind(&slot)
                .fetch_one(admin.pool())
                .await
                .unwrap();
        assert!(!active);
        sqlx::query("SELECT pg_drop_replication_slot($1)")
            .bind(slot)
            .execute(admin.pool())
            .await
            .unwrap();
        tokio::time::timeout(DEADLINE, admin.pool().close())
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn base_and_wal_restore_resumes_checkpointed_effects() {
    // Pending::read requires the helper's durable intent BEFORE any mutation/process/SDK.
    let fixture = Arc::new(Mutex::new(Fixture::open()));
    let inner = Arc::clone(&fixture);
    let mut body = tokio::spawn(async move {
        inner.lock().await.run().await;
    });
    let outcome =
        if let Ok(outcome) = tokio::time::timeout(Duration::from_secs(210), &mut body).await {
            outcome
        } else {
            body.abort();
            tokio::time::timeout(Duration::from_secs(5), &mut body)
                .await
                .expect("body drain unproved; durable intent retains tier")
        };
    let inner = Arc::clone(&fixture);
    let mut cleanup = tokio::spawn(async move { inner.lock().await.finish().await });
    let cleaned = tokio::time::timeout(Duration::from_secs(60), &mut cleanup).await;
    if cleaned.is_err() {
        cleanup.abort();
        let _ = tokio::time::timeout(Duration::from_secs(5), &mut cleanup).await;
    }
    if let Err(error) = outcome {
        if error.is_panic() {
            std::panic::resume_unwind(error.into_panic());
        }
        panic!("physical recovery task cancelled; pending intent retained");
    }
    assert!(
        matches!(cleaned, Ok(Ok(true))),
        "physical cleanup incomplete; pending intent retains tier"
    );
    println!("native-physical-recovery:base-wal-sdk-effects:complete");
}
