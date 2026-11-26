#![allow(clippy::doc_markdown)]
//! PostgreSQL-backed matter reads and Person merge boundaries mounted by the portal:
//! `GET /app/api/projects`, `/app/api/projects/{id}`,
//! `/app/api/projects/{id}/{participants,notations}`, `/app/api/notations/{id}`,
//! `/app/api/playbooks`, `/app/api/playbooks/{id}`, `/app/api/contract-reviews/{id}`.
//!
//! The read functions are shared `store` reads; these tests focus on what the API
//! adds: `visible_projects` self-scoping, the by-id 404-on-out-of-scope gate,
//! and the tier split (matter reads are client-readable, firm tools are lawyer).
//! Person merges retain immutable billing evidence and expose a conflict only to a lawyer.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use portal::{AppState, SessionStore};
use store::persons::Role;
use store::postgres::PostgresDb;
use tower::ServiceExt;
use uuid::Uuid;

use portal::test_support::TEST_SESSION_KEY as KEY;

#[path = "common/bearer.rs"]
mod bearer_common;

use bearer_common::bearer;
/// Synthetic Firm-private coordinates the fixture matter carries so the
/// client-payload redaction has something to redact.
const PRIVATE_NOTION: &str = "https://notion.example/private-workup";
const INTERNAL_SLACK_ID: &str = "C0FIRMONLY1";
const PROJECT_CALENDAR: &str = "c_firmonly@group.calendar.example.com";
const BOOKING_URL: &str = "https://calendar.app.google/synthetic-booking";

// Serialize repository-backed fixture lifetimes to avoid Git spawn descriptor races on macOS.
static REPO_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Fixture {
    _repo_guard: tokio::sync::MutexGuard<'static, ()>,
    app: axum::Router,
    surreal: store::surreal::SurrealDb,
    storage: std::sync::Arc<dyn cloud::StorageService>,
    project_id: Uuid,
    project_code: String,
    notation_id: Uuid,
    lawyer: String,
    client: String,
    outsider: String,
    /// An Owner with no participation row on the fixture matter.
    unassigned_owner: String,
    /// An Admin with no participation row on the fixture matter.
    unassigned_admin: String,
    /// An Admin who *is* seeded onto the matter, so the firm-side gate is
    /// proved to be about the participation row rather than about the tier.
    participating_admin: String,
    /// A Clerk: the supervised non-lawyer tier, which `LawyerSession` refuses.
    clerk: String,
}

/// The privileged and supervised tiers the participation gate has to answer
/// for. None of them is needed as a `Person` elsewhere in the fixture, so they
/// are seeded straight to the bearer their session presents.
struct TierBearers {
    unassigned_owner: String,
    unassigned_admin: String,
    participating_admin: String,
    clerk: String,
}

/// Seed an Owner and an Admin who hold no row on `project_id`, an Admin who
/// does, and a Clerk. The unassigned pair is the point: it is what proves the
/// privileged tiers reach matter *content* through the participation ledger
/// like everybody else (ENG-81) rather than through their tier.
async fn seed_tier_bearers(surreal: &store::surreal::SurrealDb, project_id: Uuid) -> TierBearers {
    let mut seeded = Vec::new();
    for (name, email, role, seated) in [
        ("Owner", "owner@example.com", Role::Owner, false),
        ("Admin", "admin@example.com", Role::Admin, false),
        (
            "Seated Admin",
            "seated-admin@example.com",
            Role::Admin,
            true,
        ),
        ("Clerk", "clerk@example.com", Role::Clerk, false),
    ] {
        let person = store::persons::create(
            surreal,
            &store::persons::NewPerson::with_role(name, email, role),
        )
        .await
        .unwrap();
        if seated {
            store::projects::add_participation(surreal, project_id, person.id, "lawyer")
                .await
                .unwrap();
        }
        seeded.push(bearer(person.id, role));
    }
    let mut seeded = seeded.into_iter();
    TierBearers {
        unassigned_owner: seeded.next().unwrap(),
        unassigned_admin: seeded.next().unwrap(),
        participating_admin: seeded.next().unwrap(),
        clerk: seeded.next().unwrap(),
    }
}

/// Seed a notation on the matter and a playbook for the firm, so the reads
/// under test return content rather than empty arrays. Returns the notation id.
async fn seed_matter_content(
    surreal: &store::surreal::SurrealDb,
    project_id: Uuid,
    entity_id: Uuid,
    respondent_id: Uuid,
) -> Uuid {
    let tmpl = store::templates::save_version(
        surreal,
        None,
        "test__read_walk",
        store::templates::Version {
            title: "Read walk".into(),
            respondent_type: "person".into(),
            asset_id: None,
            form_code: None,
            kind: None,
            source_commit_sha: None,
        },
    )
    .await
    .unwrap()
    .into_model();
    let notation_id = store::notations::create(
        surreal,
        &store::notations::NewNotation::new(tmpl.id, respondent_id, project_id, "BEGIN"),
    )
    .await
    .unwrap()
    .id;
    store::playbooks::create(
        surreal,
        &store::playbooks::NewPlaybook {
            entity_id,
            name: "Vendor MSA",
            positions: &[store::playbooks::Position {
                topic: "Liability".into(),
                preferred: "cap".into(),
                fallback: "2x".into(),
                walkaway: "uncapped".into(),
                severity: store::playbooks::SEVERITY_HIGH.into(),
            }],
        },
    )
    .await
    .unwrap();
    notation_id
}

fn repo_store() -> repos::RepoStore {
    static ROOT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    let root = ROOT.get_or_init(|| {
        let root =
            std::env::temp_dir().join(format!("navigator-api-reads-repos-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var(repos::REPO_ROOT_ENV, &root);
        root
    });
    repos::RepoStore::new(root)
}

async fn seed_fixture_entity_and_brand(pg: &PostgresDb) -> Uuid {
    let jurisdiction = Uuid::now_v7();
    let entity_type = Uuid::now_v7();
    let entity_id = Uuid::now_v7();
    sqlx::query("INSERT INTO jurisdiction(id,name) VALUES ($1,'Synthetic Jurisdiction')")
        .bind(jurisdiction)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO entity_type(id,name) VALUES ($1,'Synthetic Entity Type')")
        .bind(entity_type)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO entity(id,name,entity_type_id,jurisdiction_id) VALUES ($1,'Synthetic Entity',$2,$3)")
        .bind(entity_id).bind(entity_type).bind(jurisdiction).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO brand(name,brand_key) VALUES ('Synthetic Brand','neon')")
        .execute(pg.pool())
        .await
        .unwrap();
    entity_id
}

async fn build_fixture() -> Fixture {
    let repo_guard = REPO_ENV_LOCK.lock().await;
    repo_store();
    let surreal = store::surreal::SurrealDb::uninitialized();
    let pg = PostgresDb::application().await.unwrap();
    let entity_id = seed_fixture_entity_and_brand(&pg).await;
    let project = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: format!("matter-{}", Uuid::now_v7()),
            name: "Matter".into(),
            status: "open".into(),
            entity_id,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    store::projects::set_private_notion_page_url(&surreal, project.id, PRIVATE_NOTION)
        .await
        .unwrap()
        .expect("the fixture matter records a private Notion page");
    store::projects::set_internal_slack_channel_id(&surreal, project.id, INTERNAL_SLACK_ID)
        .await
        .unwrap()
        .expect("the fixture matter records a firm-only Slack channel");
    store::projects::update_project(
        &surreal,
        project.id,
        &store::projects::UpdateProjectCommand {
            calendar_id: Some(PROJECT_CALENDAR.into()),
            booking_url: Some(BOOKING_URL.into()),
            ..Default::default()
        },
    )
    .await
    .expect("the fixture matter records a calendar and a booking link");
    let lawyer = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role("Lawyer", "lawyer@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    store::projects::add_participation(&surreal, project.id, lawyer.id, "lawyer")
        .await
        .unwrap();
    let client = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role("Client", "client@example.com", Role::Client),
    )
    .await
    .unwrap();
    store::projects::add_participation(&surreal, project.id, client.id, "client")
        .await
        .unwrap();
    let outsider = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role("Outsider", "outsider@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    let tiers = seed_tier_bearers(&surreal, project.id).await;

    let notation_id = seed_matter_content(&surreal, project.id, entity_id, client.id).await;

    let state = AppState {
        sessions: SessionStore::new(KEY),
        ..portal::test_support::app_state(surreal.clone()).await
    };
    let storage = state.storage.clone();
    Fixture {
        _repo_guard: repo_guard,
        storage,
        app: server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR)),
        surreal,
        project_id: project.id,
        project_code: project.code.clone(),
        notation_id,
        lawyer: bearer(lawyer.id, Role::Lawyer),
        client: bearer(client.id, Role::Client),
        outsider: bearer(outsider.id, Role::Lawyer),
        unassigned_owner: tiers.unassigned_owner,
        unassigned_admin: tiers.unassigned_admin,
        participating_admin: tiers.participating_admin,
        clerk: tiers.clerk,
    }
}

async fn get(fx: &Fixture, path: &str, auth: Option<&str>) -> axum::http::Response<Body> {
    let mut req = Request::builder().method("GET").uri(path);
    if let Some(auth) = auth {
        req = req.header("authorization", auth);
    }
    fx.app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

/// The caller's own view of the fixture matter, read through the list door
/// so the assertion covers the payload a portal actually receives.
async fn project_json(fx: &Fixture, auth: Option<&str>) -> serde_json::Value {
    let resp = get(fx, "/app/api/projects", auth).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .unwrap()
        .as_array()
        .unwrap()
        .first()
        .cloned()
        .expect("the caller sees the fixture matter")
}

async fn json_array_len(resp: axum::http::Response<Body>) -> usize {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .unwrap()
        .as_array()
        .unwrap()
        .len()
}

#[tokio::test]
async fn list_projects_is_scoped_to_the_caller() {
    store::test_support::with_postgres(async {
        let fx = build_fixture().await;

        let mine = get(&fx, "/app/api/projects", Some(&fx.lawyer)).await;
        assert_eq!(mine.status(), StatusCode::OK);
        assert_eq!(
            json_array_len(mine).await,
            1,
            "the participant sees their matter"
        );

        let theirs = get(&fx, "/app/api/projects", Some(&fx.outsider)).await;
        assert_eq!(theirs.status(), StatusCode::OK);
        assert_eq!(
            json_array_len(theirs).await,
            0,
            "a non-participant sees none"
        );

        assert_eq!(
            get(&fx, "/app/api/projects", None).await.status(),
            StatusCode::UNAUTHORIZED
        );
    })
    .await;
}

#[tokio::test]
async fn client_portal_uses_the_host_brand_for_the_list_detail_and_api() {
    // Dioxus renders on its own task. Give that task and the Axum request the same
    // owned database through the normal application environment in a fresh process.
    const CHILD: &str = "NAVIGATOR_TWO_BRAND_PORTAL_CHILD";
    if std::env::var_os(CHILD).is_some() {
        check_two_brand_portal().await;
        return;
    }
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let database: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(pg.pool())
            .await
            .unwrap();
        let status = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "client_portal_uses_the_host_brand_for_the_list_detail_and_api",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("NAV_BASE_URL", "https://www.neonlaw.com")
            .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
            .env(
                "NAVIGATOR_POSTGRES_PORT",
                std::env::var("NAVIGATOR_TEST_POSTGRES_PORT").unwrap(),
            )
            .env("NAVIGATOR_POSTGRES_DATABASE", database)
            .env("NAVIGATOR_POSTGRES_USER", "postgres")
            .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
            .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
            .status()
            .await
            .unwrap();
        assert!(
            status.success(),
            "render the client, clerk, and lawyer dashboards"
        );
    })
    .await;
}

async fn seed_sibling_brand(fx: &Fixture) -> (store::projects::Project, store::projects::Project) {
    let pg = PostgresDb::application().await.unwrap();
    let project = store::projects::find_by_id(&fx.surreal, fx.project_id)
        .await
        .unwrap()
        .unwrap();
    let firm: Uuid = sqlx::query_scalar(
        "INSERT INTO firm(name,slug,status,entity_id)
         VALUES ('Example Firm','example-firm','active',$1) RETURNING id",
    )
    .bind(project.entity_id)
    .fetch_one(pg.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO brand(name,brand_key) VALUES ('Sibling Brand','delete-your-data')")
        .execute(pg.pool())
        .await
        .unwrap();
    for key in ["neon", "delete-your-data"] {
        store::firms::move_brand(&fx.surreal, Role::Owner, None, key, firm)
            .await
            .unwrap();
    }
    store::firms::update(
        &fx.surreal,
        Role::Owner,
        None,
        firm,
        &store::firms::FirmEdit {
            portal_theme_brand_key: Some(Some("neon".into())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    store::projects::update_project(
        &fx.surreal,
        project.id,
        &store::projects::UpdateProjectCommand {
            firm_id: Some(firm),
            brand: Some(project.brand.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let sibling = store::projects::create(
        &fx.surreal,
        &store::projects::NewProject {
            code: "sibling-matter".into(),
            name: "Sibling Matter".into(),
            brand: "delete-your-data".into(),
            firm_id: Some(firm),
            status: "open".into(),
            entity_id: project.entity_id,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    (project, sibling)
}

async fn response_text(response: axum::response::Response) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

async fn seed_confirmed_plan_project(fx: &Fixture, project: &store::projects::Project) {
    let pg = PostgresDb::application().await.unwrap();
    let plan = store::legal_plans::create(
        &pg,
        project.firm_id.unwrap(),
        &store::legal_plans::NewLegalPlan {
            name: "Example Plan".into(),
            contact: None,
            portal: None,
            acceptance_sla_hours: Some(24),
            turnaround_sla_days: Some(7),
            reporting_requirements: None,
        },
    )
    .await
    .unwrap();
    store::projects::set_plan_columns(
        &fx.surreal,
        &pg,
        project.id,
        &store::projects::PlanColumns {
            legal_plan_id: Some(plan.id),
            plan_accepted_on: Some("2026-11-20".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    store::projects::set_repository_url(
        &fx.surreal,
        project.id,
        Some("https://forge.example.test/example-firm/sibling-matter"),
    )
    .await
    .unwrap();
    for (filename, visibility) in [
        ("released-welcome.pdf", store::documents::visibility::CLIENT),
        (
            "internal-review.pdf",
            store::documents::visibility::INTERNAL,
        ),
    ] {
        store::documents::ingest_bytes(
            &fx.surreal,
            &fx.storage,
            &store::documents::IngestArgs {
                project_id: project.id,
                source: store::documents::source::GENERATED,
                filename,
                kind: "unclassified",
                content_type: "application/pdf",
                description: None,
                secondary_storage_key: None,
                visibility,
            },
            filename.as_bytes(),
        )
        .await
        .unwrap();
    }
}

#[allow(clippy::too_many_lines)] // One mounted fixture supplies every Brand and access check.
async fn check_two_brand_portal() {
    let fx = build_fixture().await;
    let (project, sibling) = seed_sibling_brand(&fx).await;
    let sessions = SessionStore::new(KEY);
    let login = |auth: &str| {
        sessions
            .decode(auth.strip_prefix("Bearer ").unwrap())
            .unwrap()
    };
    let client = login(&fx.client);
    let lawyer = login(&fx.lawyer);
    let clerk = login(&fx.clerk);
    for id in [project.id, sibling.id] {
        store::projects::designate_dri_in_surreal(
            &fx.surreal,
            id,
            lawyer.person_id.unwrap(),
            store::projects::DriSide::Lawyer,
        )
        .await
        .unwrap();
        store::projects::add_participation(&fx.surreal, id, clerk.person_id.unwrap(), "clerk")
            .await
            .unwrap();
    }
    store::projects::add_participation(
        &fx.surreal,
        sibling.id,
        client.person_id.unwrap(),
        "client",
    )
    .await
    .unwrap();
    store::projects::designate_dri_in_surreal(
        &fx.surreal,
        sibling.id,
        client.person_id.unwrap(),
        store::projects::DriSide::Client,
    )
    .await
    .unwrap();
    seed_confirmed_plan_project(&fx, &sibling).await;

    let request = |host: &str, path: &str, session: &portal::SessionData| {
        let cookie = format!(
            "{}={}",
            portal::session::SESSION_COOKIE_NAME,
            sessions.encode(session)
        );
        fx.app.clone().oneshot(
            Request::builder()
                .uri(path)
                .header("host", host)
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
    };
    let invitation_path = format!("/app/api/projects/{}/portal-invitation", sibling.id);
    let response = request("www.neonlaw.com", &invitation_path, &lawyer)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let invitation: serde_json::Value =
        serde_json::from_str(&response_text(response).await).unwrap();
    assert_eq!(invitation["recipient"], "client@example.com");
    let url = url::Url::parse(invitation["portal_url"].as_str().unwrap()).unwrap();
    assert_eq!(url.host_str(), Some("www.deleteyourdata.com"));
    assert_eq!(url.path(), format!("/app/projects/{}", sibling.code));
    let invited_host = url.host_str().unwrap();
    // Follow the toolbar link as a browser. The destination supplies the CSRF
    // token for its own session and starts the read-only view on its own host.
    let preview_path = format!("{}/view-as-client", url.path());
    let source = request("www.neonlaw.com", url.path(), &lawyer)
        .await
        .unwrap();
    assert_eq!(source.status(), StatusCode::OK);
    let source = scraper::Html::parse_document(&response_text(source).await);
    let form = scraper::Selector::parse("form[target=_blank][method=post]").unwrap();
    let form = source.select(&form).next().expect("new-tab preview form");
    assert_eq!(form.attr("action"), Some(preview_path.as_str()));
    // This host checks the form, then vouches for the reviewer on the
    // Project's host for one short-lived, person-bound redirect.
    let post = |host: &str, session: &portal::SessionData| {
        fx.app.clone().oneshot(
            Request::post(&preview_path)
                .header("host", host)
                .header(
                    "cookie",
                    format!(
                        "{}={}",
                        portal::session::SESSION_COOKIE_NAME,
                        sessions.encode(session)
                    ),
                )
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!("_csrf={}", session.csrf_token)))
                .unwrap(),
        )
    };
    let handed = post("www.neonlaw.com", &lawyer).await.unwrap();
    assert_eq!(handed.status(), StatusCode::SEE_OTHER);
    assert!(!handed.headers().contains_key("set-cookie"));
    let handed = url::Url::parse(handed.headers()["location"].to_str().unwrap()).unwrap();
    assert_eq!(handed.host_str(), Some(invited_host));
    assert_eq!(handed.path(), preview_path);
    let handed = format!("{preview_path}?{}", handed.query().unwrap());
    let missing_dri = format!("/app/projects/{}/view-as-client", project.code);
    assert_eq!(
        request("www.neonlaw.com", &missing_dri, &lawyer)
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(invited_host, &preview_path, &login(&fx.outsider))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let brand_session = login(&bearer(lawyer.person_id.unwrap(), Role::Lawyer));
    let landing = request(invited_host, &preview_path, &brand_session)
        .await
        .unwrap();
    assert_eq!(landing.status(), StatusCode::OK);
    assert!(!landing.headers().contains_key("set-cookie"));
    // Another person's note changes nothing; the reviewer's own starts the view.
    let other_clerk = request(invited_host, &handed, &clerk).await.unwrap();
    assert_eq!(other_clerk.status(), StatusCode::OK);
    assert!(!other_clerk.headers().contains_key("set-cookie"));
    let one_click = request(invited_host, &handed, &brand_session)
        .await
        .unwrap();
    assert_eq!(one_click.status(), StatusCode::SEE_OTHER);
    assert_eq!(one_click.headers()["location"], url.path());
    assert!(one_click.headers().contains_key("set-cookie"));
    let page = scraper::Html::parse_document(&response_text(landing).await);
    let form = page
        .select(&scraper::Selector::parse("form[method=post]").unwrap())
        .next()
        .unwrap();
    assert_eq!(form.attr("action"), Some(preview_path.as_str()));
    let token = form
        .select(&scraper::Selector::parse("input[name=_csrf]").unwrap())
        .next()
        .unwrap()
        .attr("value")
        .unwrap();
    assert_eq!(token, brand_session.csrf_token);
    let cookie_name = portal::session::SESSION_COOKIE_NAME;
    let started = fx
        .app
        .clone()
        .oneshot(
            Request::post(&preview_path)
                .header("host", invited_host)
                .header(
                    "cookie",
                    format!("{cookie_name}={}", sessions.encode(&brand_session)),
                )
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!("_csrf={token}")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(started.status(), StatusCode::SEE_OTHER);
    assert_eq!(started.headers()["location"], url.path());
    let cookie = started.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let preview = sessions.decode(cookie.split_once('=').unwrap().1).unwrap();
    assert_eq!(preview.person_id, client.person_id);
    assert!(preview.viewing_as_dri.is_some());
    let detail = request(invited_host, url.path(), &preview).await.unwrap();
    assert_eq!(detail.status(), StatusCode::OK);
    let detail = response_text(detail).await;
    assert!(detail.contains("Stop viewing as client") && detail.contains("released-welcome.pdf"));
    assert!(detail.contains("brand-delete-your-data-tokens.css"));
    for path in [url.path(), &preview_path] {
        let signed_out = fx
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("host", invited_host)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(signed_out.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            signed_out.headers()["location"],
            format!("/auth/login?return_to={path}")
        );
    }
    let landing = portal::oauth::post_login_landing(Role::Client, url.path());
    assert_eq!(landing, url.path());
    let page = request(invited_host, &landing, &client).await.unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    let html = response_text(page).await;
    assert!(html.contains("brand-delete-your-data-tokens.css"));
    assert!(html.contains("/public/brand/delete-your-data/logo.svg"));
    assert!(html.contains("released-welcome.pdf"));
    assert!(!html.contains("internal-review.pdf") && !html.contains("forge.example"));
    assert_eq!(
        request("www.neonlaw.com", url.path(), &client)
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let nonparticipant = store::persons::create(
        &fx.surreal,
        &store::persons::NewPerson::with_role(
            "Other Client",
            "other-client@example.com",
            Role::Client,
        ),
    )
    .await
    .unwrap();
    let other = login(&bearer(nonparticipant.id, Role::Client));
    assert_eq!(
        request(invited_host, url.path(), &other)
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(invited_host, &invitation_path, &login(&fx.outsider))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(invited_host, &invitation_path, &client)
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    for (host, visible, hidden) in [
        ("www.neonlaw.com", &project, &sibling),
        ("www.deleteyourdata.com", &sibling, &project),
    ] {
        for path in ["/app/projects?brand=all", "/app/api/projects"] {
            let response = request(host, path, &client).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{host}{path}");
            let body = response_text(response).await;
            assert!(
                body.contains(&visible.code) && !body.contains(&hidden.code),
                "{host}{path}"
            );
            if path.starts_with("/app/projects") {
                assert!(body.contains(&format!("brand-{}-tokens.css", visible.brand)));
            }
        }
        for path in [
            format!("/app/projects/{}", hidden.code),
            format!("/app/projects/{}/documents.zip", hidden.code),
            format!("/app/api/projects/{}", hidden.id),
        ] {
            assert_eq!(
                request(host, &path, &client).await.unwrap().status(),
                StatusCode::NOT_FOUND,
                "{host}{path}"
            );
        }
        assert_eq!(
            request(host, &format!("/app/api/projects/{}", visible.id), &client)
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }
    for team_member in [&clerk, &lawyer] {
        let response = request(invited_host, "/app/projects", team_member)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_text(response).await;
        assert!(body.contains("brand-neon-tokens.css"));
        assert!(body.contains(&project.code) && body.contains(&sibling.code));
    }
}

/// The Firm-private integration coordinates are the reason
/// `/app/api/projects` serializes a narrower struct for a client caller.
/// Navigator stores an address only, so a leaked private Notion URL or a
/// private Slack channel id is a live handle to firm-only work product on a
/// matter the client is otherwise entitled to read — the row is in scope and
/// the coordinate is not. The paired assertion on the lawyer payload is what
/// keeps this a redaction rather than a column nobody reads.
#[tokio::test]
async fn a_client_project_payload_redacts_the_firm_private_coordinates() {
    store::test_support::with_postgres(async {
        let fx = build_fixture().await;

        let client_view = project_json(&fx, Some(&fx.client)).await;
        for field in [
            "private_notion_page_url",
            "internal_slack_channel_id",
            "internal_slack_channel_url",
            "calendar_id",
            "firm_id",
            "repository_url",
            "drive_folder_id",
            "git_initialized_at",
            "forge_provisioned_at",
            "closed_at",
        ] {
            assert!(
                client_view.get(field).is_none(),
                "a client payload must not carry `{field}`: {client_view}"
            );
        }
        assert_eq!(
            client_view.get("code").and_then(serde_json::Value::as_str),
            Some(fx.project_code.as_str()),
            "the client still reads the matter itself"
        );
        assert_eq!(
            client_view
                .get("booking_url")
                .and_then(serde_json::Value::as_str),
            Some(BOOKING_URL),
            "the client reads the booking link: {client_view}"
        );

        let firm_view = project_json(&fx, Some(&fx.lawyer)).await;
        assert_eq!(
            firm_view
                .get("private_notion_page_url")
                .and_then(serde_json::Value::as_str),
            Some(PRIVATE_NOTION),
            "the firm tier still reads the private page: {firm_view}"
        );
        assert_eq!(
            firm_view
                .get("internal_slack_channel_id")
                .and_then(serde_json::Value::as_str),
            Some(INTERNAL_SLACK_ID),
            "the firm tier still reads the private channel id: {firm_view}"
        );
        assert_eq!(
            firm_view
                .get("calendar_id")
                .and_then(serde_json::Value::as_str),
            Some(PROJECT_CALENDAR),
            "the firm tier still reads the calendar id: {firm_view}"
        );
    })
    .await;
}

#[tokio::test]
async fn matter_reads_are_scoped() {
    store::test_support::with_postgres(async {
        let fx = build_fixture().await;
        let pid = fx.project_id;

        for path in [
            format!("/app/api/projects/{pid}"),
            format!("/app/api/projects/{pid}/participants"),
            format!("/app/api/projects/{pid}/notations"),
            format!("/app/api/notations/{}", fx.notation_id),
        ] {
            assert_eq!(
                get(&fx, &path, Some(&fx.lawyer)).await.status(),
                StatusCode::OK,
                "{path}"
            );
            assert_eq!(
                get(&fx, &path, Some(&fx.outsider)).await.status(),
                StatusCode::NOT_FOUND,
                "{path} out of scope"
            );
            assert_eq!(
                get(&fx, &path, None).await.status(),
                StatusCode::UNAUTHORIZED,
                "{path} anon"
            );
        }

        // A client on the matter reads it too.
        assert_eq!(
            get(&fx, &format!("/app/api/projects/{pid}"), Some(&fx.client))
                .await
                .status(),
            StatusCode::OK
        );
    })
    .await;
}

#[tokio::test]
async fn notation_answers_are_lawyer_only_and_matter_scoped() {
    store::test_support::with_postgres(async {
        let fx = build_fixture().await;
        let path = format!("/app/api/notations/{}/answers", fx.notation_id);

        assert_eq!(
            get(&fx, &path, Some(&fx.lawyer)).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            get(&fx, &path, Some(&fx.client)).await.status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            get(&fx, &path, Some(&fx.outsider)).await.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            get(&fx, &path, None).await.status(),
            StatusCode::UNAUTHORIZED
        );
    })
    .await;
}

/// A one-matter input, enough to produce a stored finance aggregate.
fn finance_report(run_id: &str) -> store::finance_aggregate::FinanceReport {
    use store::finance_aggregate::{
        aggregate, AttorneyRef, FinanceInput, MatterRow, Parameters, StateRef, VerticalRef,
    };
    let input = FinanceInput {
        states: vec![StateRef {
            code: "A".into(),
            name: "State A".into(),
        }],
        verticals: vec![VerticalRef {
            key: "v".into(),
            name: "Vertical".into(),
        }],
        attorneys: vec![AttorneyRef {
            id: "atty-01".into(),
            name: "Sample Attorney".into(),
            home_state: "A".into(),
        }],
        months: vec!["2025-10".into()],
        matters: vec![MatterRow {
            id: "m-1".into(),
            month: "2025-10".into(),
            state: "A".into(),
            vertical: "v".into(),
            lead: "atty-01".into(),
            rain_maker: "atty-01".into(),
            billed_cents: 1_200_000,
            collected_cents: 1_000_000,
        }],
    };
    aggregate(&input, Parameters::default(), run_id, chrono::Utc::now()).unwrap()
}

async fn post(fx: &Fixture, path: &str, auth: Option<&str>) -> axum::http::Response<Body> {
    let mut req = Request::builder().method("POST").uri(path);
    if let Some(auth) = auth {
        req = req.header("authorization", auth);
    }
    fx.app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn finance_read_is_404_until_a_run_stores_a_result_then_matches_the_contract() {
    store::test_support::with_postgres(async {
        let fx = build_fixture().await;
        let path = format!("/app/api/projects/{}/finance", fx.project_id);

        assert_eq!(
            get(&fx, &path, Some(&fx.lawyer)).await.status(),
            StatusCode::NOT_FOUND,
            "no run yet: the portal falls back to its fixture on a 404"
        );

        store::finance_aggregate::upsert(&fx.surreal, fx.project_id, &finance_report("run-1"))
            .await
            .unwrap();

        for bearer in [&fx.lawyer, &fx.client] {
            let resp = get(&fx, &path, Some(bearer)).await;
            assert_eq!(resp.status(), StatusCode::OK);
            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let report: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(report["source"], "workflow");
            assert_eq!(report["run_id"], "run-1");
            assert_eq!(report["totals"]["collected_cents"], 1_000_000);
            assert_eq!(report["totals"]["overhead_cents"], 280_000);
            assert_eq!(report["by_attorney"][0]["home_state"], "A");
        }

        // Same scope rule as the other Project reads: a miss is a bare 404.
        assert_eq!(
            get(&fx, &path, Some(&fx.outsider)).await.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            get(&fx, &path, None).await.status(),
            StatusCode::UNAUTHORIZED
        );
    })
    .await;
}

#[tokio::test]
async fn finance_run_is_lawyer_tier_and_matter_scoped() {
    store::test_support::with_postgres(async {
        let fx = build_fixture().await;
        let path = format!("/app/api/projects/{}/finance/run", fx.project_id);

        assert_eq!(
            post(&fx, &path, Some(&fx.client)).await.status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            post(&fx, &path, Some(&fx.clerk)).await.status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            post(&fx, &path, Some(&fx.outsider)).await.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            post(&fx, &path, None).await.status(),
            StatusCode::UNAUTHORIZED
        );
    })
    .await;
}

fn commit_board_sources(fx: &Fixture, files: &[(&str, &[u8])]) {
    repo_store()
        .commit_as_code(
            &fx.project_code,
            repos::Author {
                name: "Synthetic Lawyer",
                email: "lawyer@example.com",
            },
            "update synthetic blueprints",
            files,
        )
        .unwrap();
}

async fn inventory_json(fx: &Fixture) -> serde_json::Value {
    let path = format!("/app/api/projects/{}/notation-inventory", fx.project_id);
    let response = get(fx, &path, Some(&fx.lawyer)).await;
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn notation_inventory_lists_unpersisted_repository_blueprints_and_current_workflows() {
    store::test_support::with_postgres(async {
    let fx = build_fixture().await;
    let body = b"---\nkind: memo\nworkflow:\n  BEGIN:\n    created: lawyer_review\n  lawyer_review:\n    approved: END\n  END: {}\n---\n\nSynthetic draft.\n";
    commit_board_sources(
        &fx,
        &[
            ("drafts/alpha.md", body),
            ("drafts/beta.md", body),
            ("drafts/gamma.md", body),
        ],
    );
    let templates_before = store::templates::list_for_project(&fx.surreal, fx.project_id)
        .await
        .unwrap();
    let inventory = inventory_json(&fx).await;
    let rows = inventory.as_array().unwrap();
    for code in ["alpha", "beta", "gamma"] {
        let row = rows
            .iter()
            .find(|row| row["template_code"] == code)
            .expect("repository blueprint is visible before a run");
        assert!(row["notation_id"].is_null());
        assert_eq!(
            row["workflow_states"],
            serde_json::json!(["BEGIN", "lawyer_review", "END"])
        );
    }
    assert_eq!(
        rows.len(),
        4,
        "three repository blueprints plus the fixture run"
    );
    assert_eq!(
        store::templates::list_for_project(&fx.surreal, fx.project_id)
            .await
            .unwrap(),
        templates_before,
        "listing must not persist templates"
    );
    assert_eq!(
        store::notations::list_by_project(&fx.surreal, fx.project_id)
            .await
            .unwrap()
            .len(),
        1,
        "listing must not create a run"
    );

    let revised = String::from_utf8(body.to_vec()).unwrap().replace(
        "    approved: END",
        "    approved: generate_pdf__memo\n  generate_pdf__memo:\n    pdf_persisted: END",
    );
    commit_board_sources(&fx, &[("drafts/beta.md", revised.as_bytes())]);
    let refreshed = inventory_json(&fx).await;
    let beta = refreshed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["template_code"] == "beta")
        .unwrap();
    assert_eq!(
        beta["workflow_states"],
        serde_json::json!(["BEGIN", "lawyer_review", "generate_pdf__memo", "END"])
    );
    }).await;
}

#[tokio::test]
async fn notation_inventory_keeps_started_workflows_pinned_when_repository_main_changes() {
    store::test_support::with_postgres(async {
        let fx = build_fixture().await;
        let body = include_str!("../../templates/notations/neon_law/onboarding__letter.md");
        commit_board_sources(&fx, &[("drafts/onboarding__letter.md", body.as_bytes())]);
        let pinned = store::template_source::persist_from_repo(
            &fx.surreal,
            &fx.storage,
            &repo_store(),
            fx.project_id,
            "onboarding__letter",
        )
        .await
        .unwrap()
        .template;
        let respondent = store::notations::find_by_id(&fx.surreal, fx.notation_id)
            .await
            .unwrap()
            .unwrap()
            .person_id;
        let run = store::notations::create(
            &fx.surreal,
            &store::notations::NewNotation::new(
                pinned.id,
                respondent,
                fx.project_id,
                "lawyer_review",
            ),
        )
        .await
        .unwrap();
        let inventory = inventory_json(&fx).await;
        let same: Vec<_> = inventory
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["template_code"] == "onboarding__letter")
            .collect();
        assert_eq!(
            same.len(),
            1,
            "an already started source version must not gain a duplicate no-run row"
        );
        assert_eq!(same[0]["notation_id"], run.id.to_string());

        let revised = body.replace("generate_pdf__retainer_pdf", "generate_pdf__engagement_pdf");
        commit_board_sources(&fx, &[("drafts/onboarding__letter.md", revised.as_bytes())]);
        let inventory = inventory_json(&fx).await;
        let same: Vec<_> = inventory
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["template_code"] == "onboarding__letter")
            .collect();
        assert_eq!(same.len(), 2);
        let started = same
            .iter()
            .find(|row| row["notation_id"] == run.id.to_string())
            .unwrap();
        let current = same
            .iter()
            .find(|row| row["notation_id"].is_null())
            .unwrap();
        assert!(started["workflow_states"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("generate_pdf__retainer_pdf")));
        assert!(started["template_stale"].as_bool().unwrap());
        assert!(current["workflow_states"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("generate_pdf__engagement_pdf")));
        assert!(!current["template_stale"].as_bool().unwrap());
        assert_eq!(
            store::templates::resolve(&fx.surreal, Some(fx.project_id), "onboarding__letter")
                .await
                .unwrap()
                .unwrap()
                .id,
            pinned.id,
            "reading main must not replace the persisted version"
        );
    })
    .await;
}

#[tokio::test]
async fn notation_inventory_flags_a_run_without_a_pinned_digest_through_the_template_binding() {
    store::test_support::with_postgres(async {
        let fx = build_fixture().await;
        let body = include_str!("../../templates/notations/neon_law/onboarding__letter.md");
        commit_board_sources(&fx, &[("drafts/onboarding__letter.md", body.as_bytes())]);
        let pinned = store::template_source::persist_from_repo(
            &fx.surreal,
            &fx.storage,
            &repo_store(),
            fx.project_id,
            "onboarding__letter",
        )
        .await
        .unwrap()
        .template;
        let respondent = store::notations::find_by_id(&fx.surreal, fx.notation_id)
            .await
            .unwrap()
            .unwrap()
            .person_id;
        let run = store::notations::create(
            &fx.surreal,
            &store::notations::NewNotation::new(
                pinned.id,
                respondent,
                fx.project_id,
                "lawyer_review",
            ),
        )
        .await
        .unwrap();
        // A run opened before per-run digests existed carries no pinned
        // digest of its own; staleness must still resolve through the
        // store-template binding it was opened against.
        sqlx::query("UPDATE notation SET template_body_sha256 = NULL WHERE id = $1")
            .bind(run.id)
            .execute(PostgresDb::application().await.unwrap().pool())
            .await
            .unwrap();
        let revised = body.replace("generate_pdf__retainer_pdf", "generate_pdf__engagement_pdf");
        commit_board_sources(&fx, &[("drafts/onboarding__letter.md", revised.as_bytes())]);
        let inventory = inventory_json(&fx).await;
        let started: Vec<_> = inventory
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| {
                row["template_code"] == "onboarding__letter" && !row["notation_id"].is_null()
            })
            .collect();
        assert_eq!(started.len(), 1);
        assert!(
            started[0]["template_stale"].as_bool().unwrap(),
            "a run without its own digest must still flag stale through the template binding"
        );
    })
    .await;
}

#[tokio::test]
async fn notation_inventory_refuses_an_unreadable_repository_instead_of_showing_an_empty_board() {
    store::test_support::with_postgres(async {
        let fx = build_fixture().await;
        std::fs::write(
            repo_store().path_for_code(&fx.project_code),
            "not a repository",
        )
        .unwrap();
        let path = format!("/app/api/projects/{}/notation-inventory", fx.project_id);
        assert_eq!(
            get(&fx, &path, Some(&fx.lawyer)).await.status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        std::fs::remove_file(repo_store().path_for_code(&fx.project_code)).unwrap();
        commit_board_sources(
            &fx,
            &[(
                "drafts/broken.md",
                b"---\nkind: memo\nworkflow: invalid\n---\n\nSynthetic draft.\n",
            )],
        );
        assert_eq!(
            get(&fx, &path, Some(&fx.lawyer)).await.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "a malformed workflow must not appear as an empty workflow"
        );
    })
    .await;
}

#[tokio::test]
async fn notation_inventory_reads_a_never_pushed_matter_without_creating_its_repository() {
    store::test_support::with_postgres(async {
        // The deployed `neon` image has no `git`, so the board must not provision
        // a bare repository just to find it empty (release 26.10.26's KIND gate).
        let fx = build_fixture().await;
        let path = format!("/app/api/projects/{}/notation-inventory", fx.project_id);
        assert_eq!(
            get(&fx, &path, Some(&fx.lawyer)).await.status(),
            StatusCode::OK
        );
        assert!(
            !repo_store().path_for_code(&fx.project_code).exists(),
            "reading the board must not create the Project repository"
        );
    })
    .await;
}

#[tokio::test]
async fn notation_inventory_is_lawyer_only_and_matter_scoped() {
    store::test_support::with_postgres(async {
        let fx = build_fixture().await;
        let path = format!("/app/api/projects/{}/notation-inventory", fx.project_id);

        let lawyer_inventory = get(&fx, &path, Some(&fx.lawyer)).await;
        assert_eq!(lawyer_inventory.status(), StatusCode::OK);
        let body = lawyer_inventory
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let inventory: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(inventory.as_array().unwrap().len(), 1);
        assert_eq!(inventory[0]["notation_id"], fx.notation_id.to_string());
        assert_eq!(inventory[0]["template_code"], "test__read_walk");
        assert_eq!(inventory[0]["respondent_email"], "client@example.com");

        assert_eq!(
            get(&fx, &path, Some(&fx.client)).await.status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            get(&fx, &path, Some(&fx.outsider)).await.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            get(&fx, &path, None).await.status(),
            StatusCode::UNAUTHORIZED
        );
    })
    .await;
}

/// The notation inventory and the filed answers are matter *content*, so the
/// participation ledger scopes them for every tier — Owner and Admin included
/// (ENG-81). Route admission is where a privileged tier skips project-scoping;
/// it is not a key to a matter's work product. An unassigned Owner or Admin
/// therefore gets the same 404 an unassigned Lawyer gets, and the fact that
/// they could name the matter's UUID changes nothing.
#[tokio::test]
async fn private_notation_reads_require_participation_from_every_tier() {
    store::test_support::with_postgres(async {
        let fx = build_fixture().await;
        let inventory = format!("/app/api/projects/{}/notation-inventory", fx.project_id);
        let answers = format!("/app/api/notations/{}/answers", fx.notation_id);

        for path in [&inventory, &answers] {
            for (label, auth) in [
                ("unassigned owner", &fx.unassigned_owner),
                ("unassigned admin", &fx.unassigned_admin),
                ("unassigned lawyer", &fx.outsider),
            ] {
                assert_eq!(
                    get(&fx, path, Some(auth)).await.status(),
                    StatusCode::NOT_FOUND,
                    "{label} must not read {path} without a participation row"
                );
            }

            assert_eq!(
                get(&fx, path, Some(&fx.participating_admin)).await.status(),
                StatusCode::OK,
                "an admin seated on the matter still reads {path}"
            );
        }
    })
    .await;
}

/// Clerk is the supervised non-lawyer tier, not a narrower Lawyer, so it is
/// refused at the extractor rather than scoped at the matter — 403, not 404.
#[tokio::test]
async fn private_notation_reads_refuse_the_clerk_tier() {
    store::test_support::with_postgres(async {
        let fx = build_fixture().await;

        for path in [
            format!("/app/api/projects/{}/notation-inventory", fx.project_id),
            format!("/app/api/notations/{}/answers", fx.notation_id),
        ] {
            assert_eq!(
                get(&fx, &path, Some(&fx.clerk)).await.status(),
                StatusCode::FORBIDDEN,
                "{path} is lawyer-tier, and a clerk is not lawyer tier"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn playbook_reads_are_lawyer_only() {
    store::test_support::with_postgres(async {
        let fx = build_fixture().await;

        let list = get(&fx, "/app/api/playbooks", Some(&fx.lawyer)).await;
        assert_eq!(list.status(), StatusCode::OK);
        assert_eq!(json_array_len(list).await, 1);

        assert_eq!(
            get(&fx, "/app/api/playbooks", Some(&fx.client))
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            get(&fx, "/app/api/playbooks", None).await.status(),
            StatusCode::UNAUTHORIZED
        );
    })
    .await;
}

#[tokio::test]
async fn contract_review_read_is_gated() {
    store::test_support::with_postgres(async {
        let fx = build_fixture().await;
        // No review seeded; an unknown id is 404 for a lawyer, 403 for a client, 401
        // for anonymous — exercising the tier gate and the non-disclosing miss.
        let path = format!("/app/api/contract-reviews/{}", Uuid::now_v7());
        assert_eq!(
            get(&fx, &path, Some(&fx.lawyer)).await.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            get(&fx, &path, Some(&fx.client)).await.status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            get(&fx, &path, None).await.status(),
            StatusCode::UNAUTHORIZED
        );
    })
    .await;
}

#[tokio::test]
async fn people_merge_preserves_billing_history_and_reports_conflict_to_lawyer() {
    store::test_support::with_postgres(async {
        let fx = build_fixture().await;
        let pg = PostgresDb::application().await.unwrap();
        let dropped = store::persons::create(
            &fx.surreal,
            &store::persons::NewPerson::with_role(
                "Synthetic billing author", "billing-author@example.test", Role::Lawyer,
            ),
        ).await.unwrap();
        store::projects::add_participation(&fx.surreal, fx.project_id, dropped.id, "lawyer")
            .await.unwrap();
        let annotation = store::billing_annotations::Annotation {
            id: Uuid::new_v4(), change_id: "k".repeat(32), commit_id: "a".repeat(40),
            tenths: 2, performed_on: "2026-11-21".parse().unwrap(),
        };
        store::billing_annotations::stage(&pg, fx.project_id, dropped.id, &annotation)
            .await.unwrap();
        store::persons::set_role(&fx.surreal, dropped.id, Role::Client)
            .await.unwrap().unwrap();
        let before: serde_json::Value = sqlx::query_scalar(
            "SELECT to_jsonb(a) FROM billing_annotation a WHERE id=$1",
        ).bind(annotation.id).fetch_one(pg.pool()).await.unwrap();
        for (auth, expected) in [
            (None, StatusCode::UNAUTHORIZED),
            (Some(fx.client.as_str()), StatusCode::FORBIDDEN),
            (Some(fx.lawyer.as_str()), StatusCode::CONFLICT),
        ] {
            let mut request = Request::builder().method("POST").uri("/app/api/people/merge")
                .header("content-type", "application/json");
            if let Some(auth) = auth {
                request = request.header("authorization", auth);
            }
            let response = fx.app.clone().oneshot(request.body(Body::from(
                serde_json::json!({"keep":"client@example.com","drop":"billing-author@example.test"}).to_string(),
            )).unwrap()).await.unwrap();
            assert_eq!(response.status(), expected);
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            if expected == StatusCode::CONFLICT {
                assert_eq!(body["error"], "retained_billing_history");
            }
        }
        let after: serde_json::Value = sqlx::query_scalar(
            "SELECT to_jsonb(a) FROM billing_annotation a WHERE id=$1",
        ).bind(annotation.id).fetch_one(pg.pool()).await.unwrap();
        assert_eq!(after, before);
        for email in ["client@example.com", "billing-author@example.test"] {
            assert!(store::persons::find_by_email_ci(&fx.surreal, email).await.unwrap().is_some());
        }
    }).await;
}

async fn document_status(fx: &Fixture, code: &str, auth: Option<&str>, upload: bool) -> StatusCode {
    let suffix = if upload {
        "documents/upload"
    } else {
        "documents.zip"
    };
    let mut request = Request::builder()
        .method(if upload { "POST" } else { "GET" })
        .uri(format!("/app/projects/{code}/{suffix}"));
    let mut csrf = None;
    if let Some(auth) = auth {
        if upload {
            let token = auth.strip_prefix("Bearer ").unwrap();
            let session = SessionStore::new(KEY).decode(token).unwrap();
            request = request.header(
                "cookie",
                format!("{}={token}", portal::session::SESSION_COOKIE_NAME),
            );
            csrf = Some(session.csrf_token);
        } else {
            request = request.header("authorization", auth);
        }
    }
    let body = if upload {
        request = request.header(
            "content-type",
            "multipart/form-data; boundary=project-lookup",
        );
        Body::from(csrf.map_or_else(
            || "--project-lookup--\r\n".to_owned(),
            |token| format!("--project-lookup\r\nContent-Disposition: form-data; name=\"_csrf\"\r\n\r\n{token}\r\n--project-lookup--\r\n"),
        ))
    } else {
        Body::empty()
    };
    fx.app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap()
        .status()
}

async fn assert_document_project_query_failure(upload: bool) {
    let fx = build_fixture().await;
    let success = if upload {
        StatusCode::SEE_OTHER
    } else {
        StatusCode::OK
    };
    assert_eq!(
        document_status(&fx, &fx.project_code, Some(&fx.lawyer), upload).await,
        success
    );
    for code in [fx.project_code.as_str(), "missing-project", "new"] {
        assert_eq!(
            document_status(&fx, code, Some(&fx.outsider), upload).await,
            StatusCode::NOT_FOUND
        );
    }
    for code in ["missing-project", "new"] {
        assert_eq!(
            document_status(&fx, code, Some(&fx.lawyer), upload).await,
            StatusCode::NOT_FOUND
        );
    }
    // The mounted session boundary redirects an anonymous browser before the
    // handler; a Project storage fault must not replace that private response.
    let anonymous = document_status(&fx, &fx.project_code, None, upload).await;
    assert_eq!(anonymous, StatusCode::SEE_OTHER);
    if upload {
        let token = fx.lawyer.strip_prefix("Bearer ").unwrap();
        let request = Request::builder()
            .method("POST")
            .uri(format!(
                "/app/projects/{}/documents/upload",
                fx.project_code
            ))
            .header(
                "cookie",
                format!("{}={token}", portal::session::SESSION_COOKIE_NAME),
            )
            .header(
                "content-type",
                "multipart/form-data; boundary=project-lookup",
            )
            .body(Body::from("--project-lookup--\r\n"))
            .unwrap();
        assert_eq!(
            fx.app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
    let pg = PostgresDb::application().await.unwrap();
    // Break only the Project lookup, after proving the real bearer session and
    // participation work. Authentication and all other tables remain intact.
    sqlx::query("ALTER TABLE project RENAME COLUMN code TO unavailable_code")
        .execute(pg.pool())
        .await
        .unwrap();
    assert!(PostgresDb::application().await.is_ok());
    assert!(store::projects::find_by_code(&fx.surreal, &fx.project_code)
        .await
        .is_err());
    assert_eq!(
        document_status(&fx, &fx.project_code, Some(&fx.lawyer), upload).await,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        document_status(&fx, &fx.project_code, None, upload).await,
        anonymous
    );
    assert_eq!(
        document_status(&fx, "new", Some(&fx.lawyer), upload).await,
        StatusCode::NOT_FOUND
    );
    sqlx::query("ALTER TABLE project RENAME COLUMN unavailable_code TO code")
        .execute(pg.pool())
        .await
        .unwrap();
    assert_eq!(
        document_status(&fx, &fx.project_code, Some(&fx.lawyer), upload).await,
        success
    );
}

#[tokio::test]
async fn documents_zip_reports_project_query_failure_after_authentication() {
    store::test_support::with_postgres(assert_document_project_query_failure(false)).await;
}

#[tokio::test]
async fn document_upload_reports_project_query_failure_after_authentication() {
    store::test_support::with_postgres(assert_document_project_query_failure(true)).await;
}

async fn merge_route(
    fx: &Fixture,
    keep: &str,
    drop: &str,
    auth: Option<&str>,
) -> axum::response::Response {
    let mut request = Request::builder()
        .method("POST")
        .uri("/app/api/people/merge")
        .header("content-type", "application/json");
    if let Some(auth) = auth {
        request = request.header("authorization", auth);
    }
    fx.app
        .clone()
        .oneshot(
            request
                .body(Body::from(
                    serde_json::json!({"keep":keep,"drop":drop}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn merge_identity_snapshot(pg: &PostgresDb) -> serde_json::Value {
    sqlx::query_scalar(
        "SELECT jsonb_build_object(
        'people',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM person p),
        'mailboxes',(SELECT jsonb_agg(to_jsonb(m) ORDER BY id) FROM person_mailbox m),
        'participation',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM person_project_role p))",
    )
    .fetch_one(pg.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn lower_tiers_cannot_transfer_client_authentication_into_an_owner_but_owner_can() {
    store::test_support::with_postgres(async {
        let fx = build_fixture().await;
        let pg = PostgresDb::application().await.unwrap();
        let owner = store::persons::find_by_email_ci(&fx.surreal, "owner@example.com")
            .await
            .unwrap()
            .unwrap();
        assert!(owner.microsoft_subject.is_none());
        let dropped = store::persons::create(
            &fx.surreal,
            &store::persons::NewPerson::new("Synthetic merge client", "merge-client@example.test"),
        )
        .await
        .unwrap();
        let subject = format!("synthetic-merge-provider-{}", Uuid::new_v4());
        store::persons::link_microsoft_subject(&fx.surreal, dropped.id, &subject)
            .await
            .unwrap()
            .unwrap();
        store::projects::add_participation(&fx.surreal, fx.project_id, dropped.id, "client")
            .await
            .unwrap();
        let before = merge_identity_snapshot(&pg).await;
        for (auth, status) in [
            (None, StatusCode::UNAUTHORIZED),
            (Some(fx.client.as_str()), StatusCode::FORBIDDEN),
            (Some(fx.clerk.as_str()), StatusCode::FORBIDDEN),
            (Some(fx.lawyer.as_str()), StatusCode::FORBIDDEN),
            (Some(fx.unassigned_admin.as_str()), StatusCode::FORBIDDEN),
        ] {
            let response = merge_route(&fx, &owner.email, &dropped.email, auth).await;
            assert_eq!(response.status(), status);
            if auth == Some(fx.lawyer.as_str()) || auth == Some(fx.unassigned_admin.as_str()) {
                let bytes = response.into_body().collect().await.unwrap().to_bytes();
                let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(body["error"], "identity_governance");
            }
            assert_eq!(merge_identity_snapshot(&pg).await, before);
            assert_eq!(
                store::persons::find_by_microsoft_subject(&fx.surreal, &subject)
                    .await
                    .unwrap()
                    .unwrap()
                    .id,
                dropped.id
            );
        }
        let accepted = merge_route(
            &fx,
            &owner.email,
            &dropped.email,
            Some(&fx.unassigned_owner),
        )
        .await;
        assert_eq!(accepted.status(), StatusCode::OK);
        assert_eq!(
            store::persons::find_by_microsoft_subject(&fx.surreal, &subject)
                .await
                .unwrap()
                .unwrap()
                .id,
            owner.id
        );
        assert_eq!(
            store::persons::find_by_any_email(&fx.surreal, &dropped.email)
                .await
                .unwrap()
                .unwrap()
                .id,
            owner.id
        );
        assert!(store::persons::find_by_id(&fx.surreal, dropped.id)
            .await
            .unwrap()
            .is_none());
    })
    .await;
}

#[tokio::test]
async fn lawyer_can_merge_an_eligible_client_into_a_lawyer_identity() {
    store::test_support::with_postgres(async {
        let fx = build_fixture().await;
        let lawyer = store::persons::find_by_email_ci(&fx.surreal, "lawyer@example.com")
            .await
            .unwrap()
            .unwrap();
        let dropped = store::persons::create(
            &fx.surreal,
            &store::persons::NewPerson::new(
                "Synthetic eligible client",
                "eligible-merge@example.test",
            ),
        )
        .await
        .unwrap();
        store::projects::add_participation(&fx.surreal, fx.project_id, dropped.id, "client")
            .await
            .unwrap();
        let response = merge_route(&fx, &lawyer.email, &dropped.email, Some(&fx.lawyer)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            store::persons::find_by_any_email(&fx.surreal, &dropped.email)
                .await
                .unwrap()
                .unwrap()
                .id,
            lawyer.id
        );
        assert!(store::persons::find_by_id(&fx.surreal, dropped.id)
            .await
            .unwrap()
            .is_none());
    })
    .await;
}
