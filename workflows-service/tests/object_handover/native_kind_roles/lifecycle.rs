//! Own only the fixtures created after the existing private-tier verification.
use super::{healthy, role_acl_fingerprint, run_phases, RejectedOwner, Tier};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use store::postgres::PostgresDb;
use store::test_support::PostgresFixture;
use uuid::Uuid;

pub(super) struct Pending {
    path: PathBuf,
    pub operation: String,
}

impl Pending {
    pub fn read() -> Self {
        use std::os::unix::fs::PermissionsExt as _;
        let path = PathBuf::from(
            std::env::var_os("NAV_NATIVE_KIND_PENDING").expect("durable pending intent missing"),
        );
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        assert!(metadata.file_type().is_file());
        assert_eq!(metadata.permissions().mode() & 0o077, 0);
        let parent = path.parent().unwrap().canonicalize().unwrap();
        assert_eq!(
            std::fs::metadata(&parent).unwrap().permissions().mode() & 0o077,
            0
        );
        assert!(!parent.ancestors().any(|path| path.join(".git").exists()));
        let data: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(data["case"], std::env::var("NAV_NATIVE_KIND_CASE").unwrap());
        assert_eq!(
            data["source_sha"],
            std::env::var("NAV_NATIVE_KIND_SOURCE_SHA").unwrap()
        );
        assert_eq!(
            data["hudson_sha256"],
            std::env::var("NAV_NATIVE_KIND_HUDSON_SHA256").unwrap()
        );
        let operation = data["operation"].as_str().unwrap().to_owned();
        assert!(!Uuid::parse_str(&operation).unwrap().is_nil());
        assert_eq!(
            operation,
            Uuid::parse_str(&operation).unwrap().simple().to_string()
        );
        Self { path, operation }
    }

    fn record(&self, key: &str, value: Value) {
        use std::io::Write as _;
        let mut data: Value = serde_json::from_slice(&std::fs::read(&self.path).unwrap()).unwrap();
        assert_eq!(data["operation"], self.operation);
        data[key] = value;
        let parent = self.path.parent().unwrap();
        let mut file = tempfile::NamedTempFile::new_in(parent).unwrap();
        file.write_all(&serde_json::to_vec(&data).unwrap()).unwrap();
        file.as_file().sync_all().unwrap();
        file.persist(&self.path)
            .unwrap_or_else(|_| panic!("private pending intent persistence failed"));
        std::fs::File::open(parent).unwrap().sync_all().unwrap();
    }

    fn complete(&self) {
        let data: Value = serde_json::from_slice(&std::fs::read(&self.path).unwrap()).unwrap();
        assert_eq!(data["operation"], self.operation);
        std::fs::remove_file(&self.path).unwrap();
        std::fs::File::open(self.path.parent().unwrap())
            .unwrap()
            .sync_all()
            .unwrap();
    }
}

#[path = "lifecycle/physical.rs"]
mod physical;

#[derive(Clone)]
struct CleanupProbe {
    databases: [String; 2],
    actual_sql: (String, String),
}

#[derive(Clone, Copy)]
enum Run {
    Canonical,
    RejectedOwnerLogin,
}

pub(super) struct Fixture {
    pub tier: Tier,
    pending: Pending,
    capture: Option<PostgresFixture>,
    unadmitted: Option<PostgresFixture>,
    owner: Option<PostgresDb>,
    app: Option<PostgresDb>,
    worker: Option<dbos::DBOS>,
    database_cleanup_failed: bool,
    database_creation_inflight: bool,
    cleanup_probe: Option<CleanupProbe>,
    last_cleanup: Vec<&'static str>,
}

impl Fixture {
    pub fn open() -> Self {
        let pending = Pending::read();
        let tier = Tier::open(&pending.operation);
        pending.record(
            "verified_tier",
            json!({"worktree":tier.root.to_str(), "context":tier.context.as_str(),
            "namespace_uid":tier.namespace_uid.as_str(), "postgres_pod_uid":tier.pod_uid.as_str()}),
        );
        Self {
            tier,
            pending,
            capture: None,
            unadmitted: None,
            owner: None,
            app: None,
            worker: None,
            database_cleanup_failed: false,
            database_creation_inflight: false,
            cleanup_probe: None,
            last_cleanup: Vec::new(),
        }
    }

    async fn create_targets(&mut self) {
        self.database_creation_inflight = true;
        self.capture = Some(
            PostgresFixture::create_capture_target(self.tier.options("postgres", 0))
                .await
                .unwrap(),
        );
        self.pending.record(
            "capture_database",
            json!(self.capture.as_ref().unwrap().database()),
        );
        self.unadmitted = Some(
            PostgresFixture::create_unactivated(self.tier.options("postgres", 0))
                .await
                .unwrap(),
        );
        self.pending.record(
            "unadmitted_database",
            json!(self.unadmitted.as_ref().unwrap().database()),
        );
        self.database_creation_inflight = false;
    }

    async fn initialize(&mut self) {
        self.create_targets().await;
        for fixture in [
            self.capture.as_ref().unwrap(),
            self.unadmitted.as_ref().unwrap(),
        ] {
            self.tier.initialize_roles(fixture.database());
            self.pending.record("jobs", json!(self.tier.jobs));
            let after_first = role_acl_fingerprint(fixture.postgres()).await;
            self.tier.initialize_roles(fixture.database());
            self.pending.record("jobs", json!(self.tier.jobs));
            assert_eq!(
                role_acl_fingerprint(fixture.postgres()).await,
                after_first,
                "the actual canonical initializer must converge effective role/ACL state"
            );
        }
        assert_eq!(self.tier.jobs.len(), 4);
        let database = self.capture.as_ref().unwrap().database();
        self.owner = Some(
            PostgresDb::connect(self.tier.options(database, 1))
                .await
                .unwrap(),
        );
        self.app = Some(
            PostgresDb::connect(self.tier.options(database, 2))
                .await
                .unwrap(),
        );
        let mut url = reqwest::Url::parse("postgresql://127.0.0.1/").unwrap();
        url.set_port(Some(self.tier.port)).unwrap();
        url.set_username("navigator_dbos").unwrap();
        url.set_password(Some(&self.tier.passwords[3])).unwrap();
        url.set_path(database);
        url.query_pairs_mut()
            .append_pair("sslmode", "verify-full")
            .append_pair("sslrootcert", self.tier.ca.to_str().unwrap());
        self.worker = Some(dbos::DBOS::new(workflows::dbos_engine::config(
            url.as_str(),
            "native-kind-role-acceptance",
        )));
    }

    pub async fn run(&mut self) {
        self.initialize().await;
        run_phases(
            self.owner.as_ref().unwrap().clone(),
            self.app.as_ref().unwrap().clone(),
            self.worker.as_ref().unwrap().clone(),
            self.unadmitted.as_ref().unwrap().postgres().clone(),
            RejectedOwner {
                hudson: self.tier.hudson.clone(),
                port: self.tier.port,
                database: self.unadmitted.as_ref().unwrap().database().to_owned(),
                password: self.tier.passwords[1].clone(),
                ca: self.tier.ca.clone(),
                health: Arc::clone(&self.tier.health),
            },
        )
        .await;
    }

    async fn reject_owner_login_with_cleanup_refusal(&mut self) {
        self.create_targets().await;
        self.tier.immutable_sql();
        self.cleanup_probe = Some(CleanupProbe {
            databases: [
                self.capture.as_ref().unwrap().database().to_owned(),
                self.unadmitted.as_ref().unwrap().database().to_owned(),
            ],
            actual_sql: self.tier.sql_copy.clone().unwrap(),
        });
        // The finalizer must refuse this real UID mismatch without skipping safe DB closes.
        self.tier.sql_copy.as_mut().unwrap().1 = Uuid::new_v4().to_string();
        let database = self.capture.as_ref().unwrap().database();
        match PostgresDb::connect(
            self.tier
                .options(database, 1)
                .password("intentionally-invalid-synthetic"),
        )
        .await
        {
            Err(error) => std::panic::panic_any(error),
            Ok(pg) => {
                pg.pool().close().await;
                panic!("invalid synthetic owner credential unexpectedly connected");
            }
        }
    }

    // Neither failed login nor a cleanup panic can bypass independently safe cleanup.
    pub async fn finish(&mut self) -> Vec<&'static str> {
        let mut errors = Vec::new();
        let sdk_drained = if let Some(worker) = &self.worker {
            tokio::time::timeout(Duration::from_secs(15), worker.shutdown())
                .await
                .is_ok()
        } else {
            true
        };
        if !sdk_drained {
            errors.push("SDK drain unproved");
        }
        let jobs_drained = if healthy(&self.tier.health) {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.tier.cleanup_jobs()))
                .unwrap_or(false)
        } else {
            false
        };
        if !jobs_drained {
            errors.push("Job/Pod drain unproved");
        }
        if jobs_drained
            && healthy(&self.tier.health)
            && std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.tier.cleanup_sql()))
                .is_err()
        {
            errors.push("immutable SQL cleanup incomplete");
        }
        let owner_closed = close_pool(&mut self.owner).await;
        let app_closed = if sdk_drained {
            close_pool(&mut self.app).await
        } else {
            false
        };
        if !owner_closed || !app_closed {
            errors.push("application pool drain unproved");
        }
        if sdk_drained
            && jobs_drained
            && owner_closed
            && app_closed
            && healthy(&self.tier.health)
            && !self.database_creation_inflight
        {
            // Both independent target closes are attempted; the first failure does not skip the second.
            let capture_closed = close_fixture(&mut self.capture).await;
            let unadmitted_closed = close_fixture(&mut self.unadmitted).await;
            if !capture_closed {
                errors.push("capture target cleanup incomplete");
            }
            if !unadmitted_closed {
                errors.push("unadmitted target cleanup incomplete");
            }
            self.database_cleanup_failed |= !capture_closed || !unadmitted_closed;
        } else {
            errors.push("dependent databases retained");
        }
        if self.database_creation_inflight {
            errors.push("database creation outcome is unresolved");
        }
        if self.database_cleanup_failed {
            errors.push("previous database cleanup is unproved");
        }
        self.last_cleanup.clone_from(&errors);
        if errors.is_empty() {
            self.pending.complete();
        } else {
            self.pending.record("cleanup", json!(errors));
        }
        errors
    }
}

async fn close_pool(pool: &mut Option<PostgresDb>) -> bool {
    if let Some(pg) = pool {
        if tokio::time::timeout(Duration::from_secs(15), pg.pool().close())
            .await
            .is_err()
        {
            return false;
        }
    }
    pool.take();
    true
}

async fn close_fixture(fixture: &mut Option<PostgresFixture>) -> bool {
    match fixture.take() {
        Some(fixture) => matches!(
            tokio::time::timeout(Duration::from_secs(15), fixture.close()).await,
            Ok(Ok(()))
        ),
        None => true,
    }
}

pub(super) async fn run_canonical_case() {
    run_case(
        Arc::new(tokio::sync::Mutex::new(Fixture::open())),
        Run::Canonical,
    )
    .await;
}

// Both the four-phase case and the failure regression use this exact primary-outcome path.
async fn run_case(fixture: Arc<tokio::sync::Mutex<Fixture>>, operation: Run) {
    let target = Arc::clone(&fixture);
    let mut task = tokio::spawn(async move {
        let mut fixture = target.lock().await;
        match operation {
            Run::Canonical => fixture.run().await,
            Run::RejectedOwnerLogin => fixture.reject_owner_login_with_cleanup_refusal().await,
        }
    });
    let result = tokio::time::timeout(Duration::from_secs(180), &mut task).await;
    let outcome = if let Ok(outcome) = result {
        Some(outcome)
    } else {
        task.abort();
        tokio::time::timeout(Duration::from_secs(5), &mut task)
            .await
            .ok()
    };
    let Some(outcome) = outcome else {
        panic!("role task drain unproved; durable pending intent retains tier/resources");
    };
    let cleanup_target = Arc::clone(&fixture);
    let mut cleanup = tokio::spawn(async move { cleanup_target.lock().await.finish().await });
    let errors = match tokio::time::timeout(Duration::from_secs(120), &mut cleanup).await {
        Ok(Ok(errors)) => errors,
        Ok(Err(_)) => vec!["cleanup failed; private pending intent retained"],
        Err(_) => {
            cleanup.abort();
            let _ = tokio::time::timeout(Duration::from_secs(5), &mut cleanup).await;
            vec!["cleanup drain unproved; private pending intent retained"]
        }
    };
    if let Err(error) = outcome {
        if !errors.is_empty() {
            eprintln!("owned role cleanup incomplete; private pending intent retained");
        }
        if error.is_panic() {
            std::panic::resume_unwind(error.into_panic());
        }
        panic!("owned role task cancelled; cleanup outcome recorded privately");
    }
    assert!(
        errors.is_empty(),
        "owned role cleanup incomplete; private pending intent retained"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_independent_cleanup_retains_the_original_login_error_and_closes_both_safe_targets()
{
    let fixture = Arc::new(tokio::sync::Mutex::new(Fixture::open()));
    let rejected = Arc::clone(&fixture);
    let outcome = tokio::spawn(async move {
        run_case(rejected, Run::RejectedOwnerLogin).await;
    })
    .await;
    let Err(error) = outcome else {
        panic!("the real rejected login must remain the primary failure");
    };
    assert!(error.is_panic());
    let cause = error.into_panic();
    assert!(
        matches!(
            cause.downcast_ref::<store::postgres::PostgresError>(),
            Some(store::postgres::PostgresError::Connection)
        ),
        "the central finalizer must resume the real login error, not its cleanup refusal"
    );
    let mut fixture = fixture.lock().await;
    assert_eq!(
        fixture.last_cleanup,
        vec!["immutable SQL cleanup incomplete"]
    );
    let proof = fixture.cleanup_probe.clone().unwrap();
    let admin = PostgresDb::connect(fixture.tier.options("postgres", 0))
        .await
        .unwrap();
    for name in proof.databases {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname=$1)")
                .bind(name)
                .fetch_one(admin.pool())
                .await
                .unwrap();
        assert!(
            !exists,
            "cleanup refusal must not skip either independently safe database close"
        );
    }
    admin.pool().close().await;
    fixture.tier.sql_copy = Some(proof.actual_sql);
    assert!(fixture.finish().await.is_empty());
}
