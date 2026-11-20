//! Opt-in acceptance on the already verified, drained, task-owned native KIND tier.
use super::{matter, new_project, push, Matter, Source};
use base64::Engine as _;
use serde::Deserialize as _;
use serde_json::{json, Value};
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};
use store::durable_object_handover::{self, Capture, HandoverError};
use store::postgres::PostgresDb;
use store::surreal::SurrealDb;
use uuid::Uuid;
use workflows_service::dbos_project_slack::{self, LaneRequest};
use workflows_service::firm_slack::SharedSlackBot;

const CASE: &str = "canonical_fixed_roles_preserve_operator_and_runtime_boundaries";
const OWNER_LABEL: &str = "navigator.neonlaw.org/role-acceptance";
const GLOBALS: [&str; 4] = [
    "database_origin",
    "database_origin_upgrade",
    "durable_object_handover",
    "durable_object_ownership",
];

#[path = "native_kind_roles/process.rs"]
mod process;
use process::{execute, healthy, run, Health};

fn assert_canonical_fields(actual: &Value, expected: &Value) {
    match expected {
        Value::Object(fields) => {
            for (key, value) in fields {
                assert_canonical_fields(&actual[key], value);
            }
        }
        Value::Array(values) => {
            let observed = actual.as_array().unwrap();
            assert_eq!(observed.len(), values.len());
            for (observed, expected) in observed.iter().zip(values) {
                assert_canonical_fields(observed, expected);
            }
        }
        _ => assert_eq!(
            actual, expected,
            "created roles Job changed a canonical field"
        ),
    }
}

struct Tier {
    root: PathBuf,
    health: Health,
    hudson: PathBuf,
    hudson_sha256: String,
    context: String,
    namespace_uid: String,
    pod_uid: String,
    operation: String,
    port: u16,
    ca: PathBuf,
    template: Value,
    passwords: [String; 4],
    jobs: Vec<(String, String)>,
    sql_copy: Option<(String, String)>,
    creation_inflight: bool,
}

impl Tier {
    fn kubectl(&self, args: &[&str]) -> Command {
        let mut command = Command::new("kubectl");
        command
            .arg("--kubeconfig")
            .arg(self.root.join(".devx/kubeconfig"))
            .args([
                "--context",
                &self.context,
                "--namespace",
                "navigator",
                "--request-timeout=30s",
            ])
            .args(args);
        command
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_slice(&run(&self.health, &mut self.kubectl(args), None)).unwrap()
    }

    fn verify(&self) {
        assert_eq!(
            workflows::attest::sha256_hex(&std::fs::read(&self.hudson).unwrap()),
            self.hudson_sha256
        );
        let result = run(
            &self.health,
            Command::new(&self.hudson)
                .current_dir(&self.root)
                .args(["ops", "postgres", "ready", "--json"]),
            None,
        );
        let ready: Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(
            ready["native_kind_verified"], true,
            "a general ready result does not prove private KIND ownership"
        );
        assert_eq!(
            self.json(&["get", "namespace", "navigator", "-o", "json"])["metadata"]["uid"],
            self.namespace_uid
        );
        let pods = self.json(&["get", "pods", "-l", "app=navigator-postgres", "-o", "json"]);
        assert_eq!(pods["items"].as_array().unwrap().len(), 1);
        assert_eq!(pods["items"][0]["metadata"]["uid"], self.pod_uid);
        for key in ["PORT", "WORKFLOWS_SERVICE_HEALTH_LISTEN"] {
            let value = std::env::var(key).expect("drained host coordinate missing");
            let port = value.rsplit(':').next().unwrap().parse::<u16>().unwrap();
            assert!(
                std::net::TcpStream::connect_timeout(
                    &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                    Duration::from_millis(250)
                )
                .is_err(),
                "host listener must be drained"
            );
        }
    }

    fn open(operation: &str) -> Self {
        let health = Arc::new(AtomicBool::new(true));
        assert_eq!(
            std::env::var("NAVIGATOR_FORBID_SURREAL").as_deref(),
            Ok("1")
        );
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap();
        let hudson = PathBuf::from(
            std::env::var_os("NAV_NATIVE_KIND_HUDSON").expect("built Hudson path missing"),
        )
        .canonicalize()
        .unwrap();
        assert_eq!(
            hudson,
            root.join("target/debug/hudson").canonicalize().unwrap()
        );
        let hudson_sha256 =
            std::env::var("NAV_NATIVE_KIND_HUDSON_SHA256").expect("built Hudson digest missing");
        assert_eq!(
            workflows::attest::sha256_hex(&std::fs::read(&hudson).unwrap()),
            hudson_sha256
        );
        let source = run(
            &health,
            Command::new("git")
                .current_dir(&root)
                .args(["rev-parse", "HEAD"]),
            None,
        );
        assert_eq!(
            String::from_utf8(source).unwrap().trim(),
            std::env::var("NAV_NATIVE_KIND_SOURCE_SHA").unwrap()
        );
        // This existing command validates intent/registry/namespace/TLS identity before any Secret read.
        let ready: Value = serde_json::from_slice(&run(
            &health,
            Command::new(&hudson)
                .current_dir(&root)
                .args(["ops", "postgres", "ready", "--json"]),
            None,
        ))
        .unwrap();
        assert_eq!(ready["native_kind_verified"], true);
        let intent: Value = serde_json::from_slice(
            &std::fs::read(root.join(".devx/postgres/native-origin.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(intent["worktree"].as_str().unwrap(), root.to_str().unwrap());
        let mut tier = Self {
            context: format!("kind-{}", intent["cluster"].as_str().unwrap()),
            namespace_uid: intent["namespace"].as_str().unwrap().to_owned(),
            pod_uid: String::new(),
            operation: operation.to_owned(),
            port: std::env::var("NAVIGATOR_POSTGRES_PORT")
                .unwrap()
                .parse()
                .unwrap(),
            ca: root.join(".devx/postgres/ca.crt"),
            hudson,
            hudson_sha256,
            root,
            health,
            template: Value::Null,
            passwords: std::array::from_fn(|_| String::new()),
            jobs: Vec::new(),
            sql_copy: None,
            creation_inflight: false,
        };
        // Ready verification has already bound these exported coordinates to the owned target.
        tier.ca = std::env::var_os("NAVIGATOR_POSTGRES_ROOT_CERT")
            .unwrap()
            .into();
        let pods = tier.json(&["get", "pods", "-l", "app=navigator-postgres", "-o", "json"]);
        assert_eq!(pods["items"].as_array().unwrap().len(), 1);
        pods["items"][0]["metadata"]["uid"]
            .as_str()
            .unwrap()
            .clone_into(&mut tier.pod_uid);
        tier.verify();
        tier.load_role_credentials_and_template();
        tier
    }

    fn load_role_credentials_and_template(&mut self) {
        for (i, name) in [
            "navigator-postgres-auth",
            "navigator-postgres-migrator",
            "navigator-postgres-app",
            "navigator-postgres-dbos",
        ]
        .iter()
        .enumerate()
        {
            let secret = self.json(&["get", "secret", name, "-o", "json"]);
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(secret["data"]["password"].as_str().unwrap())
                .unwrap();
            self.passwords[i] = String::from_utf8(bytes)
                .unwrap_or_else(|_| panic!("owned role credential encoding invalid"));
            assert!(!self.passwords[i].is_empty());
        }
        let config = self.json(&["get", "configmap", "navigator-postgres-roles", "-o", "json"]);
        assert_eq!(
            config["data"]["roles.sql"].as_str().unwrap().as_bytes(),
            std::fs::read(self.root.join("k8s/postgres/access/roles.sql")).unwrap()
        );
        // Render the current canonical Job, not a remembered/TTL-expired live Job or copied SQL.
        let rendered = run(
            &self.health,
            Command::new("kubectl")
                .current_dir(&self.root)
                .args(["kustomize", "k8s/overlays/kind-postgres-dbos-deps"]),
            None,
        );
        for document in serde_yaml::Deserializer::from_slice(&rendered) {
            let value = Value::deserialize(document).unwrap();
            if value["kind"] == "Job" && value["metadata"]["name"] == "navigator-postgres-roles" {
                assert!(self.template.is_null());
                self.template = value;
            }
        }
        assert!(!self.template.is_null());
        self.verify_job_template();
    }

    fn verify_job_template(&self) {
        let pod = &self.template["spec"]["template"]["spec"];
        assert_eq!(pod["automountServiceAccountToken"], false);
        assert_eq!(pod["securityContext"]["runAsUser"], 999);
        assert_eq!(pod["containers"].as_array().unwrap().len(), 1);
        let container = &pod["containers"][0];
        let pinned: Value = serde_yaml::from_slice(
            &std::fs::read(self.root.join("k8s/postgres/kustomization.yaml")).unwrap(),
        )
        .unwrap();
        let image = pinned["images"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == "navigator/postgres")
            .unwrap();
        assert_eq!(
            container["image"],
            format!(
                "{}@{}",
                image["newName"].as_str().unwrap(),
                image["digest"].as_str().unwrap()
            )
        );
        assert_eq!(
            container["securityContext"]["allowPrivilegeEscalation"],
            false
        );
        assert_eq!(container["securityContext"]["readOnlyRootFilesystem"], true);
        assert_eq!(
            container["securityContext"]["capabilities"]["drop"],
            json!(["ALL"])
        );
        assert_eq!(
            container["command"],
            json!([
                "psql",
                "--no-psqlrc",
                "--quiet",
                "--file=/etc/navigator/postgres/sql/roles.sql"
            ])
        );
        let mounts = container["volumeMounts"].as_array().unwrap();
        for (name, suffix) in [
            ("sql", "sql"),
            ("app", "app"),
            ("migrator", "migrator"),
            ("backup", "backup"),
            ("dbos", "dbos"),
            ("archive", "archive"),
        ] {
            let mount = mounts.iter().find(|row| row["name"] == name).unwrap();
            assert_eq!(
                mount["mountPath"],
                format!("/etc/navigator/postgres/{suffix}")
            );
            assert_eq!(mount["readOnly"], true);
        }
        assert_eq!(mounts.len(), 7);
        let volumes = pod["volumes"].as_array().unwrap();
        assert_eq!(volumes.len(), 7);
        assert!(volumes.iter().all(|row| row.get("hostPath").is_none()));
        assert_eq!(
            volumes.iter().find(|row| row["name"] == "sql").unwrap()["configMap"]["name"],
            "navigator-postgres-roles"
        );
        for name in ["app", "migrator", "backup", "dbos", "archive"] {
            assert_eq!(
                volumes.iter().find(|row| row["name"] == name).unwrap()["secret"]["secretName"],
                format!("navigator-postgres-{name}")
            );
        }
    }

    fn options(&self, database: &str, index: usize) -> PgConnectOptions {
        let options = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(self.port)
            .database(database)
            .username(
                [
                    "postgres",
                    "navigator_owner",
                    "navigator_app",
                    "navigator_dbos",
                ][index],
            )
            .password(&self.passwords[index])
            .ssl_mode(PgSslMode::VerifyFull)
            .ssl_root_cert(&self.ca);
        if index == 0 {
            options.options([("session_preload_libraries", "age")])
        } else {
            options
        }
    }

    fn immutable_sql(&mut self) -> String {
        if let Some((name, _)) = &self.sql_copy {
            return name.clone();
        }
        self.verify();
        let name = format!("role-acceptance-{}", &self.operation[..12]);
        let sql = std::fs::read_to_string(self.root.join("k8s/postgres/access/roles.sql")).unwrap();
        let mut config = json!({"apiVersion":"v1","kind":"ConfigMap","immutable":true,
            "metadata":{"name":name,"namespace":"navigator","labels":{}},"data":{"roles.sql":sql}});
        config["metadata"]["labels"][OWNER_LABEL] = json!(self.operation);
        self.creation_inflight = true;
        let observed: Value = serde_json::from_slice(&run(
            &self.health,
            &mut self.kubectl(&["create", "-f", "-", "-o", "json"]),
            Some(&serde_json::to_vec(&config).unwrap()),
        ))
        .unwrap();
        self.sql_copy = Some((
            name.clone(),
            observed["metadata"]["uid"].as_str().unwrap().to_owned(),
        ));
        self.creation_inflight = false;
        assert_eq!(observed["immutable"], true);
        assert_eq!(observed["data"], config["data"]);
        name
    }

    fn initialize_roles(&mut self, database: &str) {
        self.verify();
        assert!(database
            .strip_prefix("native_test_")
            .is_some_and(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())));
        let name = format!(
            "role-acceptance-{}-{}",
            &self.operation[..12],
            self.jobs.len()
        );
        let sql_copy = self.immutable_sql();
        let mut job = self.template.clone();
        let volumes = job["spec"]["template"]["spec"]["volumes"]
            .as_array_mut()
            .unwrap();
        volumes.iter_mut().find(|row| row["name"] == "sql").unwrap()["configMap"]["name"] =
            json!(sql_copy);
        job["metadata"] = json!({"name":name,"namespace":"navigator","labels":{}});
        job["metadata"]["labels"][OWNER_LABEL] = json!(self.operation);
        job["spec"]["ttlSecondsAfterFinished"] = json!(3600);
        let container = &mut job["spec"]["template"]["spec"]["containers"][0];
        container["command"]
            .as_array_mut()
            .unwrap()
            .push(json!(format!("--set=navigator_database={database}")));
        let env = container["env"].as_array_mut().unwrap();
        let target = env
            .iter_mut()
            .find(|row| row["name"] == "PGDATABASE")
            .unwrap();
        *target = json!({"name":"PGDATABASE","value":database});
        self.creation_inflight = true;
        let observed: Value = serde_json::from_slice(&run(
            &self.health,
            &mut self.kubectl(&["create", "-f", "-", "-o", "json"]),
            Some(&serde_json::to_vec(&job).unwrap()),
        ))
        .unwrap();
        let uid = observed["metadata"]["uid"].as_str().unwrap().to_owned();
        assert!(!self.jobs.iter().any(|(_, other)| *other == uid));
        assert_eq!(observed["metadata"]["labels"][OWNER_LABEL], self.operation);
        self.jobs.push((name.clone(), uid.clone()));
        self.creation_inflight = false;
        let actual = &observed["spec"]["template"]["spec"];
        let expected = &job["spec"]["template"]["spec"];
        for key in [
            "containers",
            "volumes",
            "securityContext",
            "automountServiceAccountToken",
        ] {
            assert_canonical_fields(&actual[key], &expected[key]);
        }
        let config = self.json(&["get", "configmap", "navigator-postgres-roles", "-o", "json"]);
        assert_eq!(
            config["data"]["roles.sql"].as_str().unwrap().as_bytes(),
            std::fs::read(self.root.join("k8s/postgres/access/roles.sql")).unwrap()
        );
        run(
            &self.health,
            &mut self.kubectl(&[
                "wait",
                "--for=condition=complete",
                "--timeout=120s",
                &format!("job/{name}"),
            ]),
            None,
        );
        self.verify_completed_job(&uid, &job);
    }

    fn verify_completed_job(&self, uid: &str, job: &Value) {
        let pods = self.json(&["get", "pods", "-o", "json"]);
        let successful: Vec<_> = pods["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|pod| {
                pod["status"]["phase"] == "Succeeded"
                    && pod["metadata"]["ownerReferences"]
                        .as_array()
                        .is_some_and(|owners| owners.iter().any(|owner| owner["uid"] == uid))
            })
            .collect();
        assert_eq!(successful.len(), 1);
        let pod = successful[0];
        let expected = &job["spec"]["template"]["spec"];
        for key in [
            "containers",
            "volumes",
            "securityContext",
            "automountServiceAccountToken",
        ] {
            assert_canonical_fields(&pod["spec"][key], &expected[key]);
        }
        for key in ["hostPID", "hostIPC", "hostNetwork"] {
            assert!(!pod["spec"][key].as_bool().unwrap_or(false));
        }
        assert!(pod["spec"].get("initContainers").is_none());
        let image = expected["containers"][0]["image"].as_str().unwrap();
        let digest = image.split_once('@').unwrap().1;
        let statuses = pod["status"]["containerStatuses"].as_array().unwrap();
        assert_eq!(statuses.len(), 1);
        assert!(statuses[0]["imageID"].as_str().unwrap().ends_with(digest));
        assert_eq!(statuses[0]["state"]["terminated"]["exitCode"], 0);
        let (name, expected_uid) = self.sql_copy.as_ref().unwrap();
        let config = self.json(&["get", "configmaps", name, "-o", "json"]);
        assert_eq!(config["metadata"]["uid"], *expected_uid);
        assert_eq!(config["immutable"], true);
        assert_eq!(
            config["data"]["roles.sql"].as_str().unwrap().as_bytes(),
            std::fs::read(self.root.join("k8s/postgres/access/roles.sql")).unwrap()
        );
    }

    fn owned_pods(&self, job_uid: &str) -> Vec<String> {
        let pods = self.json(&["get", "pods", "-o", "json", "--request-timeout=5s"]);
        pods["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|pod| {
                pod["metadata"]["ownerReferences"]
                    .as_array()
                    .is_some_and(|owners| owners.iter().any(|owner| owner["uid"] == job_uid))
            })
            .map(|pod| pod["metadata"]["uid"].as_str().unwrap().to_owned())
            .collect()
    }

    fn delete_uid(&self, kind: &str, name: &str, uid: &str) {
        let observed = self.json(&["get", kind, name, "-o", "json", "--request-timeout=5s"]);
        assert_eq!(observed["metadata"]["uid"], uid);
        assert_eq!(observed["metadata"]["labels"][OWNER_LABEL], self.operation);
        let path = match kind {
            "jobs" => format!("/apis/batch/v1/namespaces/navigator/jobs/{name}"),
            "configmaps" => format!("/api/v1/namespaces/navigator/configmaps/{name}"),
            _ => panic!("unsupported acceptance resource"),
        };
        // kubectl's supported raw DELETE transports this actual DeleteOptions body.
        // The apiserver enforces the UID atomically; a GET/name race cannot delete a replacement.
        let options = json!({"apiVersion":"v1","kind":"DeleteOptions",
            "preconditions":{"uid":uid},"propagationPolicy":"Foreground"});
        run(
            &self.health,
            &mut self.kubectl(&["delete", "--raw", &path, "-f", "-", "--request-timeout=5s"]),
            Some(&serde_json::to_vec(&options).unwrap()),
        );
    }

    fn wait_absent(&self, kind: &str, name: &str, uid: &str) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            let objects = self.json(&["get", kind, "-o", "json", "--request-timeout=5s"]);
            let present = objects["items"].as_array().unwrap().iter().any(|item| {
                if item["metadata"]["name"] == name {
                    assert_eq!(
                        item["metadata"]["uid"], uid,
                        "replacement resource must be retained"
                    );
                    true
                } else {
                    false
                }
            });
            if !present && (kind != "jobs" || self.owned_pods(uid).is_empty()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("owned resource/Job Pod drain unproved; retain SQL and database support");
    }

    fn cleanup_jobs(&self) -> bool {
        self.verify();
        // A lost CREATE reply may leave an unknown UID. Retain its SQL/database
        // support; never infer that an unrecorded resource was not created.
        let mut all_drained = !self.creation_inflight;
        // Each exact Job is attempted independently, even if another Job refuses cleanup.
        for (name, uid) in &self.jobs {
            let cleaned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let pod_uids = self.owned_pods(uid);
                self.delete_uid("jobs", name, uid);
                self.wait_absent("jobs", name, uid);
                let pods = self.json(&["get", "pods", "-o", "json", "--request-timeout=5s"]);
                assert!(!pods["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|pod| pod_uids.iter().any(|uid| pod["metadata"]["uid"] == *uid)));
            }));
            all_drained &= cleaned.is_ok();
        }
        all_drained
    }

    fn cleanup_sql(&self) {
        if let Some((name, uid)) = &self.sql_copy {
            let observed = self.json(&["get", "configmaps", name, "-o", "json"]);
            assert_eq!(observed["immutable"], true);
            self.delete_uid("configmaps", name, uid);
            self.wait_absent("configmaps", name, uid);
        }
    }
}

async fn role_acl_fingerprint(pg: &PostgresDb) -> Value {
    // Deliberately omit passwords/role authentication hashes and arbitrary settings.
    sqlx::query_scalar(r"
SELECT jsonb_build_object(
 'roles',(SELECT jsonb_agg(jsonb_build_array(rolname,rolsuper,rolcreatedb,rolcreaterole,
       rolreplication,rolbypassrls,rolcanlogin,rolconnlimit) ORDER BY rolname)
       FROM pg_roles WHERE rolname IN ('navigator_owner','navigator_app','navigator_dbos')),
 'database',(SELECT jsonb_build_array(datdba::regrole::text,datacl::text)
       FROM pg_database WHERE datname=current_database()),
 'schemas',(SELECT jsonb_agg(jsonb_build_array(nspname,nspowner::regrole::text,nspacl::text)
       ORDER BY nspname) FROM pg_namespace WHERE nspname IN ('public','dbos','ag_catalog')),
 'relations',(SELECT jsonb_agg(jsonb_build_array(c.relname,c.relowner::regrole::text,c.relacl::text)
       ORDER BY c.relname) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
       WHERE n.nspname='public'),
 'defaults',(SELECT jsonb_agg(jsonb_build_array(defaclrole::regrole::text,
       defaclnamespace,defaclobjtype,defaclacl::text) ORDER BY defaclrole,defaclnamespace,defaclobjtype)
       FROM pg_default_acl WHERE defaclrole IN ('navigator_owner'::regrole,'navigator_app'::regrole)),
 'functions',(SELECT jsonb_agg(jsonb_build_array(p.proname,pg_get_function_identity_arguments(p.oid),
       p.proowner::regrole::text,p.proacl::text) ORDER BY p.proname,p.oid)
       FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
       WHERE n.nspname='public' AND p.proname IN
       ('protect_database_origin_runtime','database_origin_schema_contract')),
 'age_preload',(SELECT jsonb_agg(jsonb_build_array(r.rolname,setting) ORDER BY r.rolname)
       FROM pg_db_role_setting s JOIN pg_roles r ON r.oid=s.setrole,
       unnest(s.setconfig) setting WHERE s.setdatabase=(SELECT oid FROM pg_database
       WHERE datname=current_database()) AND setting='session_preload_libraries=age'))
").fetch_one(pg.pool()).await.unwrap()
}

async fn snapshot(pg: &PostgresDb) -> Vec<Value> {
    let mut rows = Vec::new();
    for table in GLOBALS.into_iter().chain([
        "notation_machine_state",
        "project_slack_handover",
        "project_slack_delivery",
    ]) {
        let sql = sqlx::AssertSqlSafe(format!("SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]'::jsonb) FROM {table} t"));
        rows.push(sqlx::query_scalar(sql).fetch_one(pg.pool()).await.unwrap());
    }
    rows
}

async fn canonical_initializer_converges_origin_role_rails(app: &PostgresDb, owner: &PostgresDb) {
    for (pg, expected) in [(app, "navigator_app"), (owner, "navigator_owner")] {
        let actual: String = sqlx::query_scalar("SELECT current_user::text")
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert_eq!(actual, expected);
        let privileged: bool = sqlx::query_scalar("SELECT rolsuper OR rolcreaterole OR rolcreatedb OR rolreplication OR rolbypassrls FROM pg_roles WHERE rolname=current_user")
            .fetch_one(pg.pool()).await.unwrap();
        assert!(!privileged);
    }
    let before = snapshot(owner).await;
    for (table, key) in [
        ("database_origin", "singleton"),
        ("database_origin_upgrade", "upgrade_operation"),
        ("durable_object_handover", "capture_id"),
        ("durable_object_ownership", "singleton"),
    ] {
        for sql in [
            format!("INSERT INTO {table} ({key}) SELECT {key} FROM {table} WHERE false"),
            format!("UPDATE {table} SET {key}={key} WHERE false"),
            format!("DELETE FROM {table} WHERE false"),
            format!("TRUNCATE TABLE {table}"),
        ] {
            let error = sqlx::query(sqlx::AssertSqlSafe(sql))
                .execute(app.pool())
                .await
                .unwrap_err();
            assert_eq!(
                error
                    .as_database_error()
                    .and_then(sqlx::error::DatabaseError::code)
                    .as_deref(),
                Some("42501")
            );
        }
    }
    let error =
        sqlx::query("SELECT public.protect_database_origin_runtime('navigator_app'::regrole)")
            .execute(app.pool())
            .await
            .unwrap_err();
    assert_eq!(
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("42501")
    );
    assert_eq!(snapshot(owner).await, before);
}

async fn admitted_owner_imports_and_activates_while_app_metadata_writes_roll_back(
    owner: &PostgresDb,
    app: &PostgresDb,
    source: &Source,
    capture: &Capture,
) {
    PostgresDb::with_test_db(owner.clone(), async {
        PostgresDb::application().await.unwrap();
    })
    .await;
    let before = snapshot(owner).await;
    assert!(matches!(
        durable_object_handover::import(app, capture, true).await,
        Err(HandoverError::Storage)
    ));
    assert_eq!(snapshot(owner).await, before);
    let preview = durable_object_handover::import(owner, capture, false)
        .await
        .unwrap();
    assert!(!preview.committed && !preview.native_activation);
    assert_eq!(snapshot(owner).await, before);
    let receipt = durable_object_handover::import(owner, capture, true)
        .await
        .unwrap();
    assert_eq!(
        (
            receipt.notation_machines,
            receipt.slack_projects,
            receipt.slack_posted_events
        ),
        (2, 2, 2)
    );
    assert_eq!(
        durable_object_handover::import(owner, capture, true)
            .await
            .unwrap(),
        receipt
    );
    let copied = snapshot(owner).await;
    assert!(matches!(
        source.activate(app, capture).await,
        Err(HandoverError::Storage)
    ));
    assert_eq!(snapshot(owner).await, copied);
    source.activate(owner, capture).await.unwrap();
    durable_object_handover::require_native_ownership(app)
        .await
        .unwrap();
}

async fn app_retains_normal_project_delivery_operations(
    app: &PostgresDb,
    worker: &dbos::DBOS,
    first: &Matter,
) {
    let before = snapshot(app).await;
    let bot = Arc::new(workflows::CapturingSlackBot::new());
    let db = SurrealDb::uninitialized();
    let slack = dbos_project_slack::register(
        worker,
        dbos_project_slack::Context::new(
            db.clone(),
            Arc::new(SharedSlackBot(bot.clone())),
            app.clone(),
            false,
        ),
    )
    .unwrap();
    worker.launch().await.unwrap();
    for request in [
        LaneRequest::MainPush(push("abc1234")),
        LaneRequest::ClientView,
    ] {
        let id = match &request {
            LaneRequest::MainPush(event) => {
                dbos_project_slack::main_push_workflow_id(first.project, event)
            }
            LaneRequest::ClientView => dbos_project_slack::client_view_workflow_id(first.project),
        };
        slack
            .start_with(
                request,
                dbos::StartOptions {
                    workflow_id: Some(&id),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
    }
    assert!(bot.posted_messages().is_empty());
    let fresh = push("def5678");
    let id = dbos_project_slack::main_push_workflow_id(first.project, &fresh);
    for _ in 0..2 {
        slack
            .start_with(
                LaneRequest::MainPush(fresh.clone()),
                dbos::StartOptions {
                    workflow_id: Some(&id),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
    }
    assert_eq!(bot.posted_messages().len(), 1);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM project_slack_delivery WHERE project_id=$1 AND event_key='def5678:1'",
    )
    .bind(first.project)
    .fetch_one(app.pool())
    .await
    .unwrap();
    assert_eq!(count, 1);
    PostgresDb::with_test_db(app.clone(), async {
        PostgresDb::application().await.unwrap();
        let future = new_project(&db, first.firm, &first.brand, first.entity).await;
        assert!(store::projects::find_by_id(&db, future)
            .await
            .unwrap()
            .is_some());
        assert!(matches!(
            store::project_slack_deliveries::admission(app, future, "client_view", "client-view")
                .await,
            Ok(store::project_slack_deliveries::Admission::Allowed)
        ));
    })
    .await;
    // Per-Project rows legitimately grow; protected global metadata never changes.
    assert_eq!(
        &snapshot(app).await[..GLOBALS.len()],
        &before[..GLOBALS.len()]
    );
}

fn unadmitted_owner_refuses_actual_operator_capture(
    hudson: &Path,
    port: u16,
    database: &str,
    password: &str,
    ca: &Path,
    health: &Health,
) {
    use std::os::unix::fs::PermissionsExt as _;
    let private = tempfile::tempdir().unwrap();
    std::fs::set_permissions(private.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let sentinel = private.path().join("restate");
    let marker = private.path().join("source-called");
    let output = private.path().join("capture.json");
    std::fs::write(
        &sentinel,
        b"#!/bin/sh\n: > \"$NATIVE_ROLE_SOURCE_MARKER\"\nexit 97\n",
    )
    .unwrap();
    std::fs::set_permissions(&sentinel, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut paths = vec![private.path().to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let (success, stdout, stderr) = execute(
        health,
        Command::new(hudson)
            .args([
                "ops",
                "restate",
                "object-handover",
                "capture",
                "--environment",
                "native-role-acceptance",
                "--output",
            ])
            .arg(&output)
            .arg("--operator-writers-held")
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("NATIVE_ROLE_SOURCE_MARKER", &marker)
            .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
            .env("NAVIGATOR_POSTGRES_PORT", port.to_string())
            .env("NAVIGATOR_POSTGRES_DATABASE", database)
            .env("NAVIGATOR_POSTGRES_USER", "navigator_owner")
            .env("NAVIGATOR_POSTGRES_PASSWORD", password)
            .env("NAVIGATOR_POSTGRES_SSLMODE", "verify-full")
            .env("NAVIGATOR_POSTGRES_ROOT_CERT", ca)
            .env("NAVIGATOR_FORBID_SURREAL", "1"),
        None,
    );
    assert!(!success, "unadmitted operator unexpectedly succeeded");
    assert!(
        String::from_utf8(stderr)
            .unwrap()
            .lines()
            .any(|line| line == "Error: admitted PostgreSQL unavailable"),
        "operator failed outside its admission boundary"
    );
    assert!(stdout.is_empty());
    assert!(
        !marker.exists(),
        "refused operator reached the source transport"
    );
    assert!(!output.exists(), "refused operator created a capture");
}

fn phase_completed(phase: u8) {
    println!("native-kind-role-phase:{phase}:complete");
}

struct RejectedOwner {
    hudson: PathBuf,
    port: u16,
    database: String,
    password: String,
    ca: PathBuf,
    health: Health,
}

async fn run_phases(
    target: PostgresDb,
    runtime: PostgresDb,
    executor: dbos::DBOS,
    unadmitted_pg: PostgresDb,
    rejected: RejectedOwner,
) {
    canonical_initializer_converges_origin_role_rails(&runtime, &target).await;
    phase_completed(1);
    let db = SurrealDb::uninitialized();
    let first = PostgresDb::with_test_db(target.clone(), matter(&db)).await;
    PostgresDb::with_test_db(
        target.clone(),
        new_project(&db, first.firm, &first.brand, first.entity),
    )
    .await;
    let source = Source::controlled(Uuid::new_v4(), first.project);
    let capture = source.capture(&target).await;
    admitted_owner_imports_and_activates_while_app_metadata_writes_roll_back(
        &target, &runtime, &source, &capture,
    )
    .await;
    phase_completed(2);
    app_retains_normal_project_delivery_operations(&runtime, &executor, &first).await;
    phase_completed(3);
    let before = snapshot(&unadmitted_pg).await;
    unadmitted_owner_refuses_actual_operator_capture(
        &rejected.hudson,
        rejected.port,
        &rejected.database,
        &rejected.password,
        &rejected.ca,
        &rejected.health,
    );
    assert_eq!(snapshot(&unadmitted_pg).await, before);
    phase_completed(4);
}

#[path = "native_kind_roles/lifecycle.rs"]
mod lifecycle;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canonical_fixed_roles_preserve_operator_and_runtime_boundaries() {
    lifecycle::run_canonical_case().await;
    println!("native-kind-role-acceptance:{CASE}:4-phases-complete");
}
