#![allow(clippy::doc_markdown)]
//! PostgreSQL-only lifecycle and onboarding API regressions.
use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
};
use http_body_util::BodyExt;
use portal::{AppState, SessionStore};
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use store::{persons::Role, postgres::PostgresDb};
use tower::ServiceExt;
use uuid::Uuid;

#[path = "common/project_envelope_lookup.rs"]
mod project_envelope_lookup;

struct Fixture {
    app: axum::Router,
    pg: PostgresDb,
    project: Uuid,
    lawyer: Uuid,
}

async fn enroll_fixture_history(pg: &PostgresDb, with_notation: bool) {
    sqlx::query("INSERT INTO mirror_backfill_run(id,batch_size,status,scope,completed_at) VALUES($1,1,'completed','global',now())")
        .bind(Uuid::now_v7()).execute(pg.pool()).await.unwrap();
    for family in store::postgres::history::Family::all() {
        if with_notation || *family != store::postgres::history::Family::Notation {
            store::test_support::seed_postgres_family(pg, *family).await;
        }
    }
}

impl Fixture {
    async fn new(
        provider: Option<std::sync::Arc<dyn portal::signature::SignatureProvider>>,
    ) -> Option<Self> {
        Self::with_scope(provider, true).await
    }

    async fn with_scope(
        provider: Option<std::sync::Arc<dyn portal::signature::SignatureProvider>>,
        enrolled: bool,
    ) -> Option<Self> {
        let port = store::test_support::postgres_test_port();
        let options = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port)
            .username("postgres")
            .password("synthetic")
            .ssl_mode(PgSslMode::Disable);
        let admin = PostgresDb::connect(options.clone().database("smoke"))
            .await
            .unwrap();
        let database = format!("onboarding_test_{}", Uuid::now_v7().simple());
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
            .execute(admin.pool())
            .await
            .unwrap();
        let pg = PostgresDb::connect(options.database(&database))
            .await
            .unwrap();
        sqlx::raw_sql("CREATE EXTENSION IF NOT EXISTS age; LOAD 'age';")
            .execute(pg.pool())
            .await
            .unwrap();
        sqlx::raw_sql(store::postgres_schema::canonical_schema())
            .execute(pg.pool())
            .await
            .unwrap();
        enroll_fixture_history(&pg, enrolled).await;
        let jurisdiction = Uuid::now_v7();
        let entity_type = Uuid::now_v7();
        let entity = Uuid::now_v7();
        let project = Uuid::now_v7();
        let lawyer = Uuid::now_v7();
        sqlx::query("INSERT INTO jurisdiction(id,name) VALUES ($1,'Synthetic Jurisdiction')")
            .bind(jurisdiction)
            .execute(pg.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO entity_type(id,name) VALUES ($1,'Synthetic Entity')")
            .bind(entity_type)
            .execute(pg.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO entity(id,name,entity_type_id,jurisdiction_id) VALUES ($1,'Synthetic Entity',$2,$3)")
            .bind(entity).bind(entity_type).bind(jurisdiction).execute(pg.pool()).await.unwrap();
        sqlx::query(
            "INSERT INTO brand(id,name,brand_key) VALUES ($1,'Synthetic Brand','synthetic')",
        )
        .bind(Uuid::now_v7())
        .execute(pg.pool())
        .await
        .unwrap();
        sqlx::query("INSERT INTO project(id,code,name,status,entity_id,brand) VALUES ($1,'synthetic-onboarding','Synthetic Project','open',$2,'synthetic')")
            .bind(project).bind(entity).execute(pg.pool()).await.unwrap();
        sqlx::query("INSERT INTO person(id,name,email,role) VALUES ($1,'Synthetic Lawyer','lawyer@example.com','lawyer')")
            .bind(lawyer).execute(pg.pool()).await.unwrap();
        let state = AppState {
            sessions: SessionStore::new(portal::test_support::TEST_SESSION_KEY),
            signature_provider: provider.unwrap_or_else(|| {
                std::sync::Arc::new(portal::signature::FakeSignatureProvider::new())
            }),
            ..portal::test_support::app_state(
                store::application_store::ApplicationStore::uninitialized(),
            )
            .await
        };
        Some(Self {
            app: server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR)),
            pg,
            project,
            lawyer,
        })
    }

    async fn get(&self, path: &str, role: Option<Role>, person: Option<Uuid>) -> Response {
        let mut request = Request::builder().uri(path);
        if let Some(role) = role {
            let mut session = portal::session::SessionData::fresh("synthetic", role);
            session.person_id = person;
            let token = SessionStore::new(portal::test_support::TEST_SESSION_KEY).encode(&session);
            request = request.header("authorization", format!("Bearer {token}"));
        }
        PostgresDb::with_test_db(
            self.pg.clone(),
            self.app
                .clone()
                .oneshot(request.body(Body::empty()).unwrap()),
        )
        .await
        .unwrap()
    }

    async fn upload(&self, body: serde_json::Value) -> Response {
        self.post(
            &format!("/app/api/projects/{}/documents", self.project),
            self.lawyer,
            body,
        )
        .await
    }

    async fn post(&self, path: &str, lawyer: Uuid, body: serde_json::Value) -> Response {
        let mut session = portal::session::SessionData::fresh("synthetic", Role::Lawyer);
        session.person_id = Some(lawyer);
        let token = SessionStore::new(portal::test_support::TEST_SESSION_KEY).encode(&session);
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        PostgresDb::with_test_db(self.pg.clone(), self.app.clone().oneshot(request))
            .await
            .unwrap()
    }

    async fn participate(&self) {
        sqlx::query("INSERT INTO person_project_role(person_id,project_id,participation) VALUES ($1,$2,'lawyer')")
            .bind(self.lawyer).bind(self.project).execute(self.pg.pool()).await.unwrap();
    }

    async fn status(&self) -> serde_json::Value {
        let response = self
            .get(
                &format!("/app/api/projects/{}/status", self.project),
                Some(Role::Lawyer),
                Some(self.lawyer),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        json(response).await
    }
}

async fn json(response: Response) -> serde_json::Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test]
async fn postgres_lifecycle_refuses_unenrolled_notation_history() {
    let Some(fx) = Fixture::with_scope(None, false).await else {
        return;
    };
    assert_eq!(
        fx.get("/app/api/project-lifecycle", Some(Role::Admin), None)
            .await
            .status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test]
async fn postgres_lifecycle_read_retains_admin_admission_and_source_state() {
    let Some(fx) = Fixture::new(None).await else {
        return;
    };
    let path = "/app/api/project-lifecycle";
    assert_eq!(
        fx.get(path, None, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    for role in [Role::Lawyer, Role::Client] {
        assert_eq!(
            fx.get(path, Some(role), None).await.status(),
            StatusCode::FORBIDDEN
        );
    }
    let rows = json(fx.get(path, Some(Role::Admin), None).await).await;
    assert_eq!(rows[0]["status"], "open");
    assert_eq!(rows[0]["lifecycle"], "pitch");
    assert_eq!(rows[0]["source_state"], "not_enabled");
    sqlx::query(
        "UPDATE project SET repository_url='https://forge.example.test/synthetic' WHERE id=$1",
    )
    .bind(fx.project)
    .execute(fx.pg.pool())
    .await
    .unwrap();
    let rows = json(fx.get(path, Some(Role::Admin), None).await).await;
    assert_eq!(rows[0]["source_state"], "unknown");
}

#[tokio::test]
async fn postgres_status_requires_participation_and_verified_execution() {
    let Some(fx) = Fixture::new(None).await else {
        return;
    };
    let path = format!("/app/api/projects/{}/status", fx.project);
    assert_eq!(
        fx.get(&path, Some(Role::Lawyer), Some(fx.lawyer))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    fx.participate().await;
    let status = fx.status().await;
    assert_eq!(status["status"], "pitch");
    assert_eq!(status["drive_folder_id"], serde_json::Value::Null);
    sqlx::query("UPDATE project SET drive_folder_id='synthetic-drive-folder' WHERE id=$1")
        .bind(fx.project)
        .execute(fx.pg.pool())
        .await
        .unwrap();
    let status = fx.status().await;
    assert_eq!(status["drive_folder_id"], "synthetic-drive-folder");
    assert!(!status["missing"].to_string().contains("--drive-folder"));
    let asset = Uuid::now_v7();
    let envelope = Uuid::now_v7();
    sqlx::query("INSERT INTO asset(id,project_id,storage_key,content_type,byte_size,sha256_hex,kind,visibility,metadata)
                 VALUES ($1,$2,'synthetic','application/pdf',1,'synthetic','onboarding','client',$3)")
        .bind(asset).bind(fx.project).bind(serde_json::json!({"docusign_envelope_id": envelope}))
        .execute(fx.pg.pool()).await.unwrap();
    let status = fx.status().await;
    assert_eq!(status["status"], "pitch");
    assert_eq!(status["onboarding"]["filed"], true);
    assert_eq!(status["onboarding"]["execution_verified"], false);
    sqlx::query("UPDATE asset SET metadata=metadata||$2::jsonb WHERE id=$1")
        .bind(asset)
        .bind(serde_json::json!({"verified_onboarding_envelope_id": envelope}))
        .execute(fx.pg.pool())
        .await
        .unwrap();
    let status = fx.status().await;
    assert_eq!(status["status"], "active");
    assert_eq!(status["onboarding"]["execution_verified"], true);
    let rows = json(
        fx.get("/app/api/project-lifecycle", Some(Role::Admin), None)
            .await,
    )
    .await;
    assert_eq!(rows[0]["lifecycle"], "active");
    for state in ["closed", "archived"] {
        sqlx::query("UPDATE project SET status=$2,closed_at='2026-11-17T00:00:00Z' WHERE id=$1")
            .bind(fx.project)
            .bind(state)
            .execute(fx.pg.pool())
            .await
            .unwrap();
        assert_eq!(fx.status().await["status"], state);
    }
}

#[tokio::test]
async fn lawyer_verifies_the_exported_dropbox_retainer_on_its_project() {
    let Some(fx) = Fixture::new(None).await else {
        return;
    };
    fx.participate().await;
    let firm = Uuid::now_v7();
    let client = Uuid::now_v7();
    sqlx::query("INSERT INTO firm(id,name,status,slug) VALUES ($1,'Synthetic Firm','active','synthetic-firm')")
        .bind(firm).execute(fx.pg.pool()).await.unwrap();
    sqlx::query("UPDATE project SET firm_id=$2 WHERE id=$1")
        .bind(fx.project)
        .bind(firm)
        .execute(fx.pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO person(id,name,email,role) VALUES ($1,'Synthetic Client','client@example.test','client')")
        .bind(client).execute(fx.pg.pool()).await.unwrap();
    sqlx::query(
        "INSERT INTO person_project_role(person_id,project_id,participation,is_client_dri)
        VALUES ($1,$2,'client',true)",
    )
    .bind(client)
    .bind(fx.project)
    .execute(fx.pg.pool())
    .await
    .unwrap();

    let response = fx
        .upload(serde_json::json!({
            "filename": "signed.pdf", "kind": "onboarding", "content_base64": "JVBERi0xLjA=",
            "content_type": "application/pdf",
            "signing": {"method": "dropbox_sign", "signed_on": "2026-11-17"},
        }))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let asset = json(response).await;
    let revision = &asset["current_version"];
    let asset_id = revision["asset_id"].as_str().unwrap();
    let path = format!(
        "/app/api/projects/{}/documents/{asset_id}/verify-dropbox-sign",
        fx.project
    );
    let mut body = serde_json::json!({
        "request_id": "0123456789abcdef0123456789abcdef01234567",
        "account_email": "lawyer@example.test",
        "client_email": "client@example.test",
        "signed_on": "2026-11-17",
        "sha256": revision["sha256"],
    });

    assert_eq!(
        fx.post(&path, Uuid::now_v7(), body.clone()).await.status(),
        StatusCode::NOT_FOUND
    );
    let mut wrong_client = body.clone();
    wrong_client["client_email"] = serde_json::json!("wrong@example.test");
    assert_eq!(
        fx.post(&path, fx.lawyer, wrong_client).await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut wrong_bytes = body.clone();
    wrong_bytes["sha256"] = serde_json::json!("different");
    assert_eq!(
        fx.post(&path, fx.lawyer, wrong_bytes).await.status(),
        StatusCode::CONFLICT
    );

    let response = fx.post(&path, fx.lawyer, body.clone()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json(response).await["applied"], false);
    assert_eq!(fx.status().await["status"], "pitch");

    body["apply"] = serde_json::json!(true);
    let response = fx.post(&path, fx.lawyer, body).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json(response).await["applied"], true);
    let status = fx.status().await;
    assert_eq!(status["status"], "active");
    assert_eq!(status["onboarding"]["status"], "official");
    assert_eq!(status["onboarding"]["execution_verified"], true);
    let rows = json(
        fx.get("/app/api/project-lifecycle", Some(Role::Admin), None)
            .await,
    )
    .await;
    assert_eq!(rows[0]["lifecycle"], "active");
}

#[tokio::test]
async fn status_reports_the_projects_own_firm_integrations() {
    let Some(fx) = Fixture::new(None).await else {
        return;
    };
    fx.participate().await;
    let status = fx.status().await;
    assert_eq!(
        status["firm_integrations"],
        serde_json::json!({"calendar": false, "drive": false})
    );
    assert!(!status["missing"].to_string().contains("--drive-folder"));
    let (firm, other) = (Uuid::now_v7(), Uuid::now_v7());
    sqlx::query("INSERT INTO firm(id,name,slug,status) VALUES ($1,'Synthetic Firm','synthetic-firm','active'),($2,'Synthetic Other Firm','synthetic-other-firm','active')")
        .bind(firm).bind(other).execute(fx.pg.pool()).await.unwrap();
    sqlx::query(
        "INSERT INTO firm_drive_settings(firm_id,google_drive) VALUES ($1,true),($2,false)",
    )
    .bind(firm)
    .bind(other)
    .execute(fx.pg.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO firm_calendar_settings(firm_id,google_workspace_owner) VALUES ($1,NULL),($2,'calendar@example.com')")
        .bind(firm).bind(other).execute(fx.pg.pool()).await.unwrap();
    sqlx::query("UPDATE project SET firm_id=$2 WHERE id=$1")
        .bind(fx.project)
        .bind(firm)
        .execute(fx.pg.pool())
        .await
        .unwrap();
    let status = fx.status().await;
    assert_eq!(
        status["firm_integrations"],
        serde_json::json!({"calendar": false, "drive": true})
    );
    assert!(status["missing"].as_array().unwrap().iter().any(|line| line
        .as_str()
        .unwrap()
        .contains("navigator project set synthetic-onboarding --drive-folder <link>")));
    sqlx::query("UPDATE firm_calendar_settings SET google_workspace_owner='calendar@example.com' WHERE firm_id=$1")
        .bind(firm).execute(fx.pg.pool()).await.unwrap();
    assert_eq!(fx.status().await["firm_integrations"]["calendar"], true);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn docusign_upload_files_and_activates_only_in_postgres() {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    let account = Uuid::now_v7();
    let envelope = Uuid::now_v7();
    let provider = portal::signature::DocuSignSignatureProvider::new(
        server.uri(),
        account.to_string(),
        "synthetic-token",
        "firm@example.com",
        "Synthetic Firm",
    );
    let Some(fx) = Fixture::new(Some(std::sync::Arc::new(provider))).await else {
        return;
    };
    fx.participate().await;
    let mut body = serde_json::json!({
        "filename": "onboarding.pdf", "kind": "onboarding", "content_base64": "c2lnbmVk",
        "signing": {"method": "docusign", "signed_on": "2026-11-17"},
    });
    assert_eq!(
        fx.upload(body.clone()).await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM asset WHERE project_id=$1")
        .bind(fx.project)
        .fetch_one(fx.pg.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    body["envelope_id"] = serde_json::json!("invalid");
    assert!(!fx.upload(body.clone()).await.status().is_success());
    assert!(server.received_requests().await.unwrap().is_empty());

    let mut unsigned = body.clone();
    unsigned.as_object_mut().unwrap().remove("signing");
    unsigned.as_object_mut().unwrap().remove("envelope_id");
    unsigned["metadata"] = serde_json::json!({
        "docusign_envelope_id": envelope, "verified_onboarding_envelope_id": envelope,
    });
    let response = fx.upload(unsigned).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert!(json(response).await["docusign_envelope_id"].is_null());
    assert_eq!(fx.status().await["status"], "pitch");

    body["envelope_id"] = serde_json::json!(envelope);
    for status in ["sent", "delivered", "declined", "voided", "completed"] {
        server.reset().await;
        Mock::given(method("GET"))
            .and(path(format!("/v2.1/accounts/{account}/envelopes/{envelope}")))
            .and(header("authorization", "Bearer synthetic-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "envelopeId": envelope, "status": status, "completedDateTime": "2026-11-17T00:00:00Z",
            }))).mount(&server).await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/v2.1/accounts/{account}/envelopes/{envelope}/recipients"
            )))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"signers": []})),
            )
            .mount(&server)
            .await;
        if status == "completed" {
            let template = Uuid::now_v7();
            let notation = Uuid::now_v7();
            sqlx::query("INSERT INTO template(id,code,title,respondent_type,is_current,kind,inserted_at,updated_at)
                VALUES ($1,'synthetic-other','Synthetic Other','person',true,'letter',now(),now())")
                .bind(template).execute(fx.pg.pool()).await.unwrap();
            sqlx::query("INSERT INTO notation(id,template_id,person_id,project_id,state) VALUES ($1,$2,$3,$4,'END')")
                .bind(notation).bind(template).bind(fx.lawyer).bind(fx.project).execute(fx.pg.pool()).await.unwrap();
            sqlx::query("INSERT INTO signature(id,provider,provider_id,notation_id,state,inserted_at,updated_at)
                VALUES ($1,'docusign',$2,$3,'requested',now(),now())")
                .bind(Uuid::now_v7()).bind(envelope.to_string()).bind(notation).execute(fx.pg.pool()).await.unwrap();
            assert_eq!(
                fx.upload(body.clone()).await.status(),
                StatusCode::UNPROCESSABLE_ENTITY
            );
            assert_eq!(fx.status().await["status"], "pitch");
            sqlx::query("DELETE FROM signature WHERE notation_id=$1")
                .bind(notation)
                .execute(fx.pg.pool())
                .await
                .unwrap();
        }
        let response = fx.upload(body.clone()).await;
        if status == "completed" {
            assert_eq!(response.status(), StatusCode::CREATED);
            assert_eq!(
                json(response).await["docusign_envelope_id"],
                envelope.to_string()
            );
            assert_eq!(fx.status().await["status"], "active");
        } else {
            assert_eq!(
                response.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "{status}"
            );
            assert_eq!(fx.status().await["status"], "pitch");
        }
    }
    assert_eq!(fx.upload(body).await.status(), StatusCode::OK);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM asset WHERE project_id=$1")
        .bind(fx.project)
        .fetch_one(fx.pg.pool())
        .await
        .unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn postgres_status_reports_repository_remotes_and_requested_envelopes() {
    let Some(fx) = Fixture::new(None).await else {
        return;
    };
    fx.participate().await;
    let body = fx.status().await;
    assert!(body["repository_remotes"].is_object());
    assert!(body["repository_remotes"]["origin"].is_null());
    assert!(body["adopted_envelopes"].as_array().unwrap().is_empty());

    let template = Uuid::now_v7();
    let notation = Uuid::now_v7();
    let envelope = Uuid::now_v7().to_string();
    sqlx::query("INSERT INTO template(id,code,title,respondent_type,is_current,kind,inserted_at,updated_at) VALUES ($1,'signature-read','Synthetic signature read','person',true,'memo',now(),now())")
        .bind(template).execute(fx.pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO notation(id,template_id,person_id,project_id,state) VALUES ($1,$2,$3,$4,'begin')")
        .bind(notation).bind(template).bind(fx.lawyer).bind(fx.project).execute(fx.pg.pool()).await.unwrap();
    PostgresDb::with_test_db(
        fx.pg.clone(),
        store::signatures::record_request(
            &store::application_store::ApplicationStore::uninitialized(),
            notation,
            store::signatures::SignatureProvider::DocuSign,
            &envelope,
        ),
    )
    .await
    .unwrap();
    let body = fx.status().await;
    assert_eq!(body["envelopes"][0]["envelope"]["envelopeId"], envelope);
    assert_eq!(body["envelopes"][0]["envelope"]["status"], "requested");
    assert!(body["adopted_envelopes"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn agreements_list_existing_runs_and_filings_only_on_participating_projects() {
    let Some(fx) = Fixture::new(None).await else {
        return;
    };
    let path = "/app/api/onboarding-agreements";
    let pool = fx.pg.pool();
    let template = Uuid::now_v7();
    let run = Uuid::now_v7();
    let envelope = Uuid::now_v7().to_string();
    let second = Uuid::now_v7();
    sqlx::query("INSERT INTO project(id,code,name,status,entity_id,brand) SELECT $1,'another-project','Another Project',status,entity_id,brand FROM project WHERE id=$2")
        .bind(second).bind(fx.project).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO template(id,code,title,respondent_type,is_current,kind,inserted_at,updated_at) VALUES ($1,'retainer','Historic agreement','person',false,'onboarding',now(),now()),($2,'unused','Unused agreement','person',true,'onboarding',now(),now())")
        .bind(template).bind(Uuid::now_v7()).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO notation(id,template_id,person_id,project_id,state) VALUES ($1,$2,$3,$4,'begin')")
        .bind(run).bind(template).bind(fx.lawyer).bind(fx.project).execute(pool).await.unwrap();
    PostgresDb::with_test_db(
        fx.pg.clone(),
        store::signatures::record_request(
            &store::application_store::ApplicationStore::uninitialized(),
            run,
            store::signatures::SignatureProvider::DocuSign,
            &envelope,
        ),
    )
    .await
    .unwrap();
    let filed = Uuid::now_v7();
    sqlx::query("INSERT INTO asset(id,project_id,storage_key,filename,content_type,byte_size,sha256_hex,kind,metadata,source)
        VALUES ($1,$2,'standalone','Signed.pdf','application/pdf',1,'synthetic','onboarding',NULL,'upload'),
               ($3,$4,'duplicate','Duplicate.pdf','application/pdf',1,'synthetic','onboarding',$5,'upload'),
               (gen_random_uuid(),$4,'completed','Certificate.pdf','application/pdf',1,'synthetic','onboarding',$6,'docusign')")
        .bind(filed).bind(second).bind(Uuid::now_v7()).bind(fx.project)
        .bind(serde_json::json!({"docusign_envelope_id":envelope,"verified_onboarding_envelope_id":envelope}))
        .bind(serde_json::json!({"docusign_envelope_id":envelope}))
        .execute(pool).await.unwrap();
    let rows = json(fx.get(path, Some(Role::Owner), Some(fx.lawyer)).await).await;
    assert_eq!(rows, serde_json::json!([]));
    fx.participate().await;
    let rows = json(fx.get(path, Some(Role::Lawyer), Some(fx.lawyer)).await).await;
    assert_eq!(
        rows,
        serde_json::json!([{"project":"synthetic-onboarding","title":"Historic agreement","state":"requested","notation_id":run,"document_id":null,"envelope_id":envelope}])
    );
    sqlx::query("INSERT INTO person_project_role(person_id,project_id,participation) VALUES ($1,$2,'lawyer')")
        .bind(fx.lawyer).bind(second).execute(pool).await.unwrap();
    let rows = json(fx.get(path, Some(Role::Lawyer), Some(fx.lawyer)).await).await;
    assert_eq!(rows.as_array().unwrap().len(), 2);
    assert_eq!(
        rows[0],
        serde_json::json!({"project":"another-project","title":"Signed.pdf","state":"filed","notation_id":null,"document_id":filed,"envelope_id":null})
    );
}

#[tokio::test]
async fn deadline_inventory_matches_dashboard_firm_scope_and_open_projects() {
    let Some(fx) = Fixture::new(None).await else {
        return;
    };
    let pool = fx.pg.pool();
    let current_firm = Uuid::now_v7();
    let other_firm = Uuid::now_v7();
    let current_brand = Uuid::now_v7();
    let other_brand = Uuid::now_v7();
    let entity: Uuid = sqlx::query_scalar("SELECT entity_id FROM project WHERE id=$1")
        .bind(fx.project)
        .fetch_one(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO firm(id,name,slug,status,entity_id) VALUES ($1,'Synthetic Current Firm','synthetic-current-firm','active',$3),($2,'Synthetic Other Firm','synthetic-other-firm','active',NULL)")
        .bind(current_firm).bind(other_firm).bind(entity).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO brand(id,name,brand_key) VALUES ($1,'Synthetic Current Brand','neon'),($2,'Synthetic Other Brand','outside-synthetic')")
        .bind(current_brand).bind(other_brand).execute(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO firm_brand(firm_id,brand_key) VALUES ($1,'neon'),($2,'outside-synthetic')",
    )
    .bind(current_firm)
    .bind(other_firm)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("UPDATE project SET firm_id=$2,brand='neon' WHERE id=$1")
        .bind(fx.project)
        .bind(current_firm)
        .execute(pool)
        .await
        .unwrap();
    let sibling = Uuid::now_v7();
    let closed = Uuid::now_v7();
    let outside = Uuid::now_v7();
    let legacy = Uuid::now_v7();
    sqlx::query("INSERT INTO project(id,code,name,status,entity_id,brand,firm_id) VALUES ($1,'synthetic-no-checkout','Synthetic No Checkout','open',$5,'neon',$6),($2,'synthetic-closed','Synthetic Closed','closed',$5,'neon',$6),($3,'synthetic-outside-firm','Synthetic Outside','open',$5,'outside-synthetic',$7),($4,'synthetic-unscoped-legacy','Synthetic Unscoped','open',$5,'outside-synthetic',NULL)")
        .bind(sibling).bind(closed).bind(outside).bind(legacy).bind(entity).bind(current_firm).bind(other_firm)
        .execute(pool).await.unwrap();
    sqlx::query("INSERT INTO person_project_role(person_id,project_id,participation) VALUES ($1,$2,'lawyer'),($1,$3,'lawyer'),($1,$4,'lawyer'),($1,$5,'lawyer'),($1,$6,'lawyer')")
        .bind(fx.lawyer).bind(fx.project).bind(sibling).bind(closed).bind(outside).bind(legacy)
        .execute(pool).await.unwrap();

    let rows = json(
        fx.get(
            "/app/api/lawyer/deadline-projects",
            Some(Role::Lawyer),
            Some(fx.lawyer),
        )
        .await,
    )
    .await;
    let mut codes = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["code"].as_str().unwrap())
        .collect::<Vec<_>>();
    codes.sort_unstable();
    assert_eq!(codes, ["synthetic-no-checkout", "synthetic-onboarding"]);

    assert_deadline_batch_is_firm_scoped(&fx, sibling, outside).await;

    sqlx::query("UPDATE project SET status='pitch' WHERE id=$1")
        .bind(sibling)
        .execute(pool)
        .await
        .unwrap();
    let rows = json(
        fx.get(
            "/app/api/lawyer/deadline-projects?include_pitch=true",
            Some(Role::Lawyer),
            Some(fx.lawyer),
        )
        .await,
    )
    .await;
    let mut codes = rows["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["code"].as_str().unwrap())
        .collect::<Vec<_>>();
    codes.sort_unstable();
    assert_eq!(codes, ["synthetic-no-checkout", "synthetic-onboarding"]);

    sqlx::query("DELETE FROM firm_brand WHERE firm_id=$1 AND brand_key='neon'")
        .bind(current_firm)
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(
        fx.get(
            "/app/api/lawyer/deadline-projects",
            Some(Role::Lawyer),
            Some(fx.lawyer)
        )
        .await
        .status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "an unresolved host Firm fails closed instead of exposing unscoped Projects"
    );
}

async fn assert_deadline_batch_is_firm_scoped(fx: &Fixture, sibling: Uuid, outside: Uuid) {
    for project_id in [fx.project, sibling, outside] {
        sqlx::query("INSERT INTO project_deadlines(id,project_id,due_at,authority_kind,authority,source,status,calendar_role,category,inserted_at,updated_at) VALUES ($1,$2,'2028-05-03T12:00:00Z','internal','Synthetic authority','lawyer','open','firm','internal','2028-05-01T12:00:00Z','2028-05-01T12:00:00Z')")
            .bind(Uuid::now_v7()).bind(project_id).execute(fx.pg.pool()).await.unwrap();
    }
    let batch = json(
        fx.get(
            "/app/api/lawyer/deadline-projects?include_deadlines=true",
            Some(Role::Lawyer),
            Some(fx.lawyer),
        )
        .await,
    )
    .await;
    assert_eq!(batch["projects"].as_array().unwrap().len(), 2);
    let deadline_projects = batch["deadlines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["project_id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(deadline_projects.len(), 2);
    assert!(deadline_projects.contains(&fx.project.to_string().as_str()));
    assert!(deadline_projects.contains(&sibling.to_string().as_str()));
}
