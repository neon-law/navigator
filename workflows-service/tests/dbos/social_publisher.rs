//! `SocialPublisher` and `SocialPublishReconciler` on DBOS: the scheduled
//! publish timer survives an executor cut-off and publishes exactly once,
//! and the store claim keeps a recovered or duplicated run from writing to
//! a provider a second time.
//!
//! Providers are a wiremock origin every endpoint points at, so a request
//! count on the publish path is the observable for "sent again or not".

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use uuid::Uuid;
use wiremock::{matchers, Mock, MockServer, ResponseTemplate};
use workflows::dbos_engine;
use workflows::SocialPublishRequest;
use workflows_service::dbos_social_publisher::{self, PublisherContext, ReconcilerContext};
use workflows_service::social_publisher::{Publisher, SocialPublisherService};

use crate::support::{database, TestDatabase, APP_VERSION, DEADLINE};

const BRAND: &str = "synthetic-brand";
const PAGE: &str = "facebook-page";
const FEED_PATH: &str = "/facebook-page/feed";
const PROVIDER_POST: &str = "provider-post-1";

struct Fixture {
    db: store::surreal::SurrealDb,
    admin: Uuid,
    kms: cloud::FakeKms,
    storage: Arc<cloud::FsStorage>,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn new(name: &str) -> Self {
        let db = store::test_support::mem_surreal().await;
        let admin = store::persons::create(
            &db,
            &store::persons::NewPerson::with_role(
                name,
                format!("{name}-{}@example.com", Uuid::now_v7()),
                store::persons::Role::Admin,
            ),
        )
        .await
        .unwrap();
        let firm = store::firms::create(
            &db,
            &store::firms::NewFirm {
                name: format!("{name} Firm"),
                status: "active".into(),
                entity_id: store::test_support::seed_entity(&db).await,
                admin_dri_person_id: admin.id,
            },
        )
        .await
        .unwrap();
        store::brands::create(
            &db,
            store::persons::Role::Admin,
            Some(admin.id),
            &store::brands::NewBrand {
                name: format!("{name} Brand"),
                key: BRAND.into(),
                firm_id: Some(firm.id),
                primary_color: None,
                accent_color: None,
                typeface: None,
            },
        )
        .await
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let storage = cloud::FsStorage::new(dir.path()).await.unwrap();
        Self {
            db,
            admin: admin.id,
            kms: cloud::FakeKms::new(name),
            storage: Arc::new(storage),
            _dir: dir,
        }
    }
}

/// The publisher both engines drive, pointed at the mock provider origin.
fn publisher(fixture: &Fixture, server: &MockServer) -> Publisher {
    SocialPublisherService::new(
        fixture.db.clone(),
        Arc::new(fixture.kms.clone()),
        fixture.storage.clone(),
    )
    .publisher()
    // The twin's integration process has no egress deployment config, and the
    // gate denies by default; allow the mock origin the way the Restate
    // in-file tests construct their publisher directly.
    .with_egress(store::config::SocialEgressGate::new(|| Ok(())))
    .with_provider_base(&server.uri())
}

/// A scheduled Post with Facebook in its selected destinations, and the
/// account that selection resolves to.
async fn scheduled_facebook_post(fixture: &Fixture, publish_at: DateTime<Utc>) -> Uuid {
    let account = store::social_accounts::connect(
        &fixture.db,
        store::social_accounts::ConnectSocialAccount {
            actor_role: store::persons::Role::Admin,
            actor_person_id: fixture.admin,
            brand_key: BRAND,
            provider: store::social_accounts::SocialProvider::Facebook,
            provider_account_id: PAGE,
            display_name: None,
            scopes: &store::social_accounts::SocialProvider::Facebook
                .required_scopes()
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>(),
            expires_at: None,
            access_token: "synthetic-access-token",
            refresh_token: None,
        },
        &fixture.kms,
    )
    .await
    .unwrap();
    let id = Uuid::now_v7();
    fixture
        .db
        .query(
            "CREATE $id SET brand_key = $brand, slug = $slug, title = 'Synthetic', \
             description = 'Synthetic description', body_html = '', status = 'scheduled', \
             publish_at = <datetime>$publish_at, facebook_caption = 'Facebook copy', \
             selected_social_accounts = [$account], inserted_at = time::now(), \
             updated_at = time::now()",
        )
        .bind(("id", store::surreal::record_id("post", id)))
        .bind(("brand", BRAND))
        .bind(("slug", id.to_string()))
        .bind(("publish_at", publish_at.to_rfc3339()))
        .bind(("account", account.id))
        .await
        .unwrap()
        .check()
        .unwrap();
    id
}

async fn mount_feed(server: &MockServer) {
    Mock::given(matchers::method("POST"))
        .and(matchers::path(FEED_PATH))
        .and(matchers::body_string_contains("message=Facebook+copy"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": PROVIDER_POST,
            "permalink_url": "https://www.facebook.com/provider-post-1"
        })))
        .mount(server)
        .await;
}

async fn feed_post_count(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path() == FEED_PATH)
        .count()
}

async fn wait_for_status(db: &TestDatabase, workflow_id: &str, status: &str) {
    tokio::time::timeout(DEADLINE, async {
        loop {
            let current: Option<String> = sqlx::query_scalar(
                "SELECT status FROM dbos.workflow_status WHERE workflow_uuid = $1",
            )
            .bind(workflow_id)
            .fetch_optional(&db.pool)
            .await
            .expect("read the workflow status");
            if current.as_deref() == Some(status) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {workflow_id} to reach {status}"));
}

/// The second-resolution schedule the store can round-trip unchanged: the
/// twin's snapshot loader requires the requested time to equal the row's.
fn scheduled_in(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(Utc::now().timestamp() + seconds, 0)
        .single()
        .expect("a valid whole-second timestamp")
}

/// An executor cut off while the run waits on the scheduled publish time
/// leaves the run pending; a relaunched executor resumes at the checkpointed
/// timer and the provider write happens exactly once.
#[tokio::test]
async fn a_cut_off_scheduled_wait_resumes_and_publishes_once() {
    let Some(db) = database().await else {
        return;
    };
    let fixture = Fixture::new("social-replay").await;
    let server = MockServer::start().await;
    mount_feed(&server).await;
    let publish_at = scheduled_in(10);
    let post_id = scheduled_facebook_post(&fixture, publish_at).await;

    let worker = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = dbos_social_publisher::register_publisher(
        &worker,
        PublisherContext::new(publisher(&fixture, &server)),
    )
    .expect("register SocialPublisher");
    worker.launch().await.expect("launch");

    dbos_social_publisher::fire_publisher(
        &workflow,
        SocialPublishRequest {
            post_id,
            publish_at,
        },
    )
    .await
    .expect("start the scheduled publish");

    // The snapshot step completes inside this wait and the run parks on its
    // checkpointed timer; the cut-off must leave it pending, not failed.
    tokio::time::sleep(Duration::from_millis(500)).await;
    worker.shutdown().await;

    let resumed = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let _ = dbos_social_publisher::register_publisher(
        &resumed,
        PublisherContext::new(publisher(&fixture, &server)),
    )
    .expect("register SocialPublisher on the resumed executor");
    resumed.launch().await.expect("launch resumed");
    let workflow_id = dbos_social_publisher::publisher_workflow_id(post_id, publish_at);
    wait_for_status(&db, &workflow_id, "SUCCESS").await;
    resumed.shutdown().await;

    assert_eq!(
        feed_post_count(&server).await,
        1,
        "the recovered run publishes exactly once"
    );
    let publication = store::social_publications::for_posts(&fixture.db, &[post_id])
        .await
        .unwrap()
        .remove(0);
    assert_eq!(publication.provider_post_id.as_deref(), Some(PROVIDER_POST));
}

/// A publication the store already confirmed never reaches a provider
/// again: the write step re-runs the claim and settles with the recorded
/// outcome, and re-firing the run's deterministic id starts nothing new.
#[tokio::test]
async fn a_confirmed_publication_never_publishes_twice() {
    let Some(db) = database().await else {
        return;
    };
    let fixture = Fixture::new("social-duplicate").await;
    let server = MockServer::start().await;
    mount_feed(&server).await;
    let publish_at = scheduled_in(2);
    let post_id = scheduled_facebook_post(&fixture, publish_at).await;
    store::social_publications::admit(&fixture.db, post_id, "facebook")
        .await
        .unwrap();
    store::social_publications::mark_confirmed(
        &fixture.db,
        post_id,
        "facebook",
        PROVIDER_POST,
        Some("https://www.facebook.com/provider-post-1"),
    )
    .await
    .unwrap();

    let worker = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = dbos_social_publisher::register_publisher(
        &worker,
        PublisherContext::new(publisher(&fixture, &server)),
    )
    .expect("register SocialPublisher");
    worker.launch().await.expect("launch");
    let workflow_id = dbos_social_publisher::publisher_workflow_id(post_id, publish_at);
    dbos_social_publisher::fire_publisher(
        &workflow,
        SocialPublishRequest {
            post_id,
            publish_at,
        },
    )
    .await
    .expect("start the duplicate delivery");
    wait_for_status(&db, &workflow_id, "SUCCESS").await;

    // The same deterministic id re-fired: a duplicate delivery, not a new run.
    dbos_social_publisher::fire_publisher(
        &workflow,
        SocialPublishRequest {
            post_id,
            publish_at,
        },
    )
    .await
    .expect("re-fire the same run");
    tokio::time::sleep(Duration::from_millis(300)).await;
    worker.shutdown().await;

    assert_eq!(
        feed_post_count(&server).await,
        0,
        "a confirmed publication is never written to a provider again"
    );
    let runs: i64 = sqlx::query_scalar("SELECT count(*) FROM dbos.workflow_status")
        .fetch_one(&db.pool)
        .await
        .expect("count the workflow rows");
    assert_eq!(runs, 1, "re-firing the deterministic id starts no new run");
}

/// The reconciler over an empty store completes its slot: no scheduled
/// posts, no pending notices, nothing to reconcile, no provider traffic.
#[tokio::test]
async fn the_reconciler_runs_an_empty_slot_without_provider_requests() {
    let Some(db) = database().await else {
        return;
    };
    let fixture = Fixture::new("social-reconcile").await;
    let server = MockServer::start().await;

    let worker = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let publisher_workflow = dbos_social_publisher::register_publisher(
        &worker,
        PublisherContext::new(publisher(&fixture, &server)),
    )
    .expect("register SocialPublisher");
    let reconciler = dbos_social_publisher::register_reconciler(
        &worker,
        ReconcilerContext::from_lookup(
            fixture.db.clone(),
            publisher(&fixture, &server),
            publisher_workflow,
            // The reconciler fails closed without a syntactically valid
            // ingress until InboundMessageAlert has a DBOS twin. An empty
            // slot never dials it, so a dead origin satisfies the gate.
            |key| match key {
                "RESTATE_BROKER_URL" => Some("http://restate.invalid".to_owned()),
                _ => None,
            },
        ),
    )
    .expect("register SocialPublishReconciler");
    worker.launch().await.expect("launch");

    let slot = dbos_social_publisher::reconcile_slot(Utc::now());
    dbos_social_publisher::fire(&reconciler, slot)
        .await
        .expect("start the reconciler slot");
    wait_for_status(
        &db,
        &dbos_social_publisher::reconciler_workflow_id(slot),
        "SUCCESS",
    )
    .await;
    worker.shutdown().await;

    assert_eq!(
        feed_post_count(&server).await,
        0,
        "an empty slot requests nothing from providers"
    );
}

/// A stopped reconciler schedule waits without firing a slot.
#[tokio::test]
async fn a_stopped_reconciler_schedule_starts_nothing() {
    let Some(db) = database().await else {
        return;
    };
    let fixture = Fixture::new("social-stopped").await;
    let server = MockServer::start().await;

    let worker = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let publisher_workflow = dbos_social_publisher::register_publisher(
        &worker,
        PublisherContext::new(publisher(&fixture, &server)),
    )
    .expect("register SocialPublisher");
    let reconciler = dbos_social_publisher::register_reconciler(
        &worker,
        ReconcilerContext::from_lookup(
            fixture.db.clone(),
            publisher(&fixture, &server),
            publisher_workflow,
            |_| None,
        ),
    )
    .expect("register SocialPublishReconciler");
    worker.launch().await.expect("launch");

    let stop = dbos::CancellationToken::new();
    stop.cancel();
    tokio::time::timeout(
        DEADLINE,
        dbos_social_publisher::schedule(&reconciler, Arc::new(Utc::now), stop),
    )
    .await
    .expect("the schedule did not stop on cancellation");
    worker.shutdown().await;

    let started: i64 = sqlx::query_scalar("SELECT count(*) FROM dbos.workflow_status")
        .fetch_one(&db.pool)
        .await
        .expect("count the workflow rows");
    assert_eq!(
        started, 0,
        "a stopped pre-slot schedule waits without firing"
    );
}
