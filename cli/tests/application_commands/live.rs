//! A live site for end-to-end CLI tests: the real portal router over HTTP on
//! a disposable PostgreSQL database, signed in as one synthetic lawyer.
//! Only session establishment is supplied here; the API enforces scope.
use assert_cmd::Command;
use serde_json::{json, Value};
use tempfile::TempDir;
use uuid::Uuid;

/// The runtime KMS key version every live site seals Firm credentials with.
pub const LIVE_KMS: &str = "live-site-kms";

pub struct LiveSite {
    pub host: String,
    pub project: store::projects::Project,
    pub lawyer: Uuid,
    pub pg: store::postgres::PostgresDb,
    pub storage: std::sync::Arc<dyn cloud::StorageService>,
    pub root: TempDir,
    credentials: std::path::PathBuf,
    server: tokio::task::JoinHandle<()>,
}

impl LiveSite {
    /// Serve the API for a fresh Project on `firm`, with `lawyer` signed in
    /// and participating firm-side, as the lawyer DRI when `responsible`.
    pub async fn start(firm: Option<Uuid>, lawyer: Option<Uuid>, responsible: bool) -> Self {
        let pg = store::postgres::PostgresDb::application().await.unwrap();
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let project =
            store::test_support::seed_project_on_firm(&application_store, firm, "Synthetic Time")
                .await;
        let lawyer = match lawyer {
            Some(lawyer) => lawyer,
            None => {
                store::persons::create(
                    &application_store,
                    &store::persons::NewPerson::with_role(
                        "Synthetic Lawyer",
                        format!("lawyer-{}@example.com", Uuid::now_v7().simple()),
                        store::persons::Role::Lawyer,
                    ),
                )
                .await
                .unwrap()
                .id
            }
        };
        store::participation::add_participant(
            &application_store,
            &store::participation::AddParticipantCommand {
                project_id: project.id,
                person_id: lawyer,
                dri: if responsible {
                    store::participation::DriRequest::Designate(store::projects::DriSide::Lawyer)
                } else {
                    store::participation::DriRequest::Unchanged
                },
                actor: store::participation::DriActor::System,
            },
        )
        .await
        .unwrap();
        let state = portal::test_support::app_state(application_store.clone()).await;
        let storage = state.storage.clone();
        let request_pg = pg.clone();
        let app = portal::api::routes()
            .with_state(portal::api::ApiState {
                application_store: state.application_store,
                email: state.email,
                bootstrap_owner_email: state.bootstrap_owner_email,
                bootstrap_company: "Synthetic Firm".into(),
                questionnaire_runtime: state.questionnaire_runtime,
                storage: state.storage,
                workflow_runtime: state.workflow_runtime,
                assets_storage: state.assets_storage,
                forms_registry: state.forms_registry,
                signature_provider: state.signature_provider,
                contract_reviewer: state.contract_reviewer,
                integration_providers: state.integration_providers,
                billing_provider: state.billing_provider.clone(),
                canonical_base_url: None,
                jev: None,
            })
            .layer(axum::Extension(
                std::sync::Arc::new(cloud::FakeKms::new(LIVE_KMS))
                    as std::sync::Arc<dyn cloud::RuntimeKms>,
            ))
            .layer(axum::middleware::from_fn(
                move |request, next: axum::middleware::Next| {
                    store::postgres::PostgresDb::with_test_db(request_pg.clone(), next.run(request))
                },
            ))
            .layer(axum::Extension(portal::SessionData {
                person_id: Some(lawyer),
                ..portal::SessionData::fresh("lawyer@example.com", store::persons::Role::Lawyer)
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let root = TempDir::new().unwrap();
        let credentials = root.path().join("credentials.json");
        std::fs::write(
            &credentials,
            json!({"hosts": {host.clone(): {
                "token": "test-token", "person_email": "lawyer@example.com", "role": "lawyer",
                "expires_at": i64::MAX
            }}})
            .to_string(),
        )
        .unwrap();
        Self {
            host,
            project,
            lawyer,
            pg,
            storage,
            root,
            credentials,
            server,
        }
    }

    /// `navigator project <args> --project <code> --host <site>`.
    pub fn navigator(&self, args: &[&str]) -> Command {
        self.navigator_on(&self.project.code, args)
    }

    /// The same signed-in lawyer, aimed at another Project's code.
    pub fn navigator_on(&self, code: &str, args: &[&str]) -> Command {
        let mut command = Command::cargo_bin("navigator").unwrap();
        command
            .current_dir(self.root.path())
            .env("NAVIGATOR_CREDENTIALS_FILE", &self.credentials)
            .arg("project")
            .args(args)
            .args(["--project", code, "--host", &self.host]);
        command
    }

    /// Run a verb that must succeed and parse its JSON output. `time` verbs
    /// and the billing report print JSON with `--json`; claim verbs always do.
    pub fn json(&self, args: &[&str]) -> Value {
        let mut command = self.navigator(args);
        if matches!(args.first(), Some(&"time" | &"invoice")) || args == ["billing"] {
            command.arg("--json");
        }
        let output = command.assert().success();
        serde_json::from_slice(&output.get_output().stdout).unwrap()
    }
}

impl Drop for LiveSite {
    fn drop(&mut self) {
        self.server.abort();
    }
}
