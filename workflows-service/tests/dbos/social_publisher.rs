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
use cloud::StorageService;
use uuid::Uuid;
use wiremock::{matchers, Mock, MockServer, ResponseTemplate};
use workflows::dbos_engine;
use workflows::SocialPublishRequest;
use workflows_service::dbos_social_publisher::{self, Pacing, PublisherContext, ReconcilerContext};
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
        storage
            .put("synthetic/video.mp4", b"synthetic video", "video/mp4")
            .await
            .unwrap();
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
    // The twin's integration process resolves bucket assets to the
    // same-origin route; the Restate in-file tests pin a synthetic
    // https origin directly, so mirror it through the builder.
    .with_video_url(|key| format!("https://assets.example.test/{key}"))
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

    // Cut off during the durable timer, then recover after its deadline.
    // Recovery must replay that timer before loading the scheduled Post.
    tokio::time::sleep(Duration::from_millis(500)).await;
    worker.shutdown().await;
    let downtime = (publish_at - Utc::now()).to_std().unwrap_or(Duration::ZERO);
    tokio::time::sleep(downtime + Duration::from_millis(100)).await;

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

/// A scheduled Post with Instagram in its selected destinations: a vertical
/// video asset, an Instagram caption, and the connected Instagram account
/// the selection resolves to.
async fn scheduled_instagram_post(fixture: &Fixture, publish_at: DateTime<Utc>) -> Uuid {
    let account = store::social_accounts::connect(
        &fixture.db,
        store::social_accounts::ConnectSocialAccount {
            actor_role: store::persons::Role::Admin,
            actor_person_id: fixture.admin,
            brand_key: BRAND,
            provider: store::social_accounts::SocialProvider::Instagram,
            provider_account_id: "instagram-user",
            display_name: None,
            scopes: &store::social_accounts::SocialProvider::Instagram
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
             publish_at = <datetime>$publish_at, instagram_caption = 'Instagram copy', \
             video_asset = 'synthetic/video.mp4', vertical_video = true, \
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

async fn mount_instagram_container(server: &MockServer) {
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/instagram-user/media"))
        .and(matchers::body_string_contains("media_type=REELS"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "container-1"
        })))
        .mount(server)
        .await;
}

async fn mount_instagram_status(server: &MockServer, status: &str, times: Option<u64>) {
    let template = ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "status_code": status,
        "id": "container-1"
    }));
    let mock = Mock::given(matchers::method("GET"))
        .and(matchers::path("/container-1"))
        .and(matchers::query_param("fields", "status_code"))
        .respond_with(template);
    match times {
        Some(times) => mock.up_to_n_times(times).mount(server).await,
        None => mock.mount(server).await,
    }
}

async fn mount_instagram_publish(server: &MockServer) {
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/instagram-user/media_publish"))
        .and(matchers::body_string_contains("creation_id=container-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "ig-media-1"
        })))
        .mount(server)
        .await;
}

async fn mount_instagram_permalink(server: &MockServer) {
    Mock::given(matchers::method("GET"))
        .and(matchers::path("/ig-media-1"))
        .and(matchers::query_param("fields", "permalink"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "permalink": "https://www.instagram.com/reel/ig-media-1"
        })))
        .mount(server)
        .await;
}

async fn request_count(server: &MockServer, path: &str) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path() == path)
        .count()
}

/// The container-readiness poll on DBOS: the run waits on durable timers
/// between checks and publishes the container exactly once when the provider
/// reports it ready, then reads the public link back.
#[tokio::test]
async fn an_instagram_container_waits_until_ready_then_publishes_once() {
    let Some(db) = database().await else {
        return;
    };
    let fixture = Fixture::new("social-container").await;
    let server = MockServer::start().await;
    mount_instagram_container(&server).await;
    mount_instagram_status(&server, "IN_PROGRESS", Some(2)).await;
    mount_instagram_status(&server, "FINISHED", Some(1)).await;
    mount_instagram_publish(&server).await;
    mount_instagram_permalink(&server).await;
    let publish_at = scheduled_in(1);
    let post_id = scheduled_instagram_post(&fixture, publish_at).await;

    let worker = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let pacing = Pacing::default()
        .with_container_interval(Duration::from_millis(50))
        .with_permalink_read_after(Duration::from_millis(50));
    let workflow = dbos_social_publisher::register_publisher(
        &worker,
        PublisherContext::new(publisher(&fixture, &server)).with_pacing(pacing),
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
    .expect("start the scheduled publish");
    wait_for_status(&db, &workflow_id, "SUCCESS").await;
    worker.shutdown().await;

    assert_eq!(
        request_count(&server, "/instagram-user/media_publish").await,
        1,
        "the container is published exactly once"
    );
    let publication = store::social_publications::for_posts(&fixture.db, &[post_id])
        .await
        .unwrap()
        .remove(0);
    assert_eq!(publication.provider_post_id.as_deref(), Some("ig-media-1"));
    assert_eq!(
        publication.permalink.as_deref(),
        Some("https://www.instagram.com/reel/ig-media-1"),
        "the read-back records the container's public link"
    );
}

/// A container the provider never marks ready exhausts its check budget on
/// durable timers and fails the destination without publishing it.
#[tokio::test]
async fn a_container_that_never_becomes_ready_times_out_without_publishing() {
    let Some(db) = database().await else {
        return;
    };
    let fixture = Fixture::new("social-container-timeout").await;
    let server = MockServer::start().await;
    mount_instagram_container(&server).await;
    mount_instagram_status(&server, "IN_PROGRESS", None).await;
    let publish_at = scheduled_in(1);
    let post_id = scheduled_instagram_post(&fixture, publish_at).await;

    let worker = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let pacing = Pacing::default().with_container_interval(Duration::from_millis(30));
    let workflow = dbos_social_publisher::register_publisher(
        &worker,
        PublisherContext::new(publisher(&fixture, &server)).with_pacing(pacing),
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
    .expect("start the scheduled publish");
    wait_for_status(&db, &workflow_id, "SUCCESS").await;
    worker.shutdown().await;

    assert_eq!(
        request_count(&server, "/instagram-user/media_publish").await,
        0,
        "a never-ready container is never published"
    );
    let status: String = fixture
        .db
        .query("SELECT VALUE status FROM ONLY $id")
        .bind(("id", store::surreal::record_id("post", post_id)))
        .await
        .unwrap()
        .take::<Option<String>>(0)
        .unwrap()
        .expect("read the post status");
    assert_eq!(status, "scheduled", "the timed-out post is not published");
}

/// An egress gate that denies every destination settles the prepare step
/// without reaching a provider, and the run completes with nothing sent.
#[tokio::test]
async fn a_denying_egress_gate_records_blocked_without_provider_requests() {
    let Some(db) = database().await else {
        return;
    };
    let fixture = Fixture::new("social-egress").await;
    let server = MockServer::start().await;
    mount_feed(&server).await;
    let publish_at = scheduled_in(1);
    let post_id = scheduled_facebook_post(&fixture, publish_at).await;

    let worker = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    // No `with_egress`: the process has no deployment egress configuration,
    // so the gate denies and prepare settles blocked.
    let denied = SocialPublisherService::new(
        fixture.db.clone(),
        Arc::new(fixture.kms.clone()),
        fixture.storage.clone(),
    )
    .publisher()
    .with_provider_base(&server.uri());
    let workflow =
        dbos_social_publisher::register_publisher(&worker, PublisherContext::new(denied))
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
    .expect("start the scheduled publish");
    wait_for_status(&db, &workflow_id, "SUCCESS").await;
    worker.shutdown().await;

    assert_eq!(
        feed_post_count(&server).await,
        0,
        "a denied destination never reaches a provider"
    );
}

fn atom(kind: [u8; 4], body: &[u8]) -> Vec<u8> {
    let mut bytes = u32::try_from(body.len() + 8)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    bytes.extend(kind);
    bytes.extend(body);
    bytes
}

async fn scheduled_tiktok_post(fixture: &Fixture, publish_at: DateTime<Utc>) -> Uuid {
    let provider = store::social_accounts::SocialProvider::Tiktok;
    let account = store::social_accounts::connect(
        &fixture.db,
        store::social_accounts::ConnectSocialAccount {
            actor_role: store::persons::Role::Admin,
            actor_person_id: fixture.admin,
            brand_key: BRAND,
            provider,
            provider_account_id: "tiktok-user",
            display_name: None,
            scopes: &provider
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
    let post_id = scheduled_instagram_post(fixture, publish_at).await;
    fixture
        .db
        .query("UPDATE $id SET selected_social_accounts = [$account], tiktok_ai_generated = false")
        .bind(("id", store::surreal::record_id("post", post_id)))
        .bind(("account", account.id))
        .await
        .unwrap()
        .check()
        .unwrap();
    store::posts::record_video(&fixture.db, BRAND, "synthetic/video.mp4", 1080, 1920)
        .await
        .unwrap();
    let mut handler = vec![0; 8];
    handler.extend(b"vide");
    let mut timing = vec![0; 12];
    timing.extend(3000_u32.to_be_bytes());
    timing.extend(90000_u32.to_be_bytes());
    let mut samples = vec![0; 4];
    samples.extend(1_u32.to_be_bytes());
    samples.extend(900_u32.to_be_bytes());
    samples.extend(100_u32.to_be_bytes());
    let mut media = atom(*b"hdlr", &handler);
    media.extend(atom(*b"mdhd", &timing));
    media.extend(atom(*b"minf", &atom(*b"stbl", &atom(*b"stts", &samples))));
    let video = atom(*b"moov", &atom(*b"trak", &atom(*b"mdia", &media)));
    fixture
        .storage
        .put("synthetic/video.mp4", &video, "video/mp4")
        .await
        .unwrap();
    post_id
}

async fn mount_tiktok(server: &MockServer) {
    for (path, body) in [
        (
            "property/list",
            serde_json::json!({"url_property_info_list":[{"property_type":1,"property_status":1,"url":"example.test"}]}),
        ),
        (
            "video/settings",
            serde_json::json!({"privacy_level_options":["PUBLIC_TO_EVERYONE"],"max_video_post_duration_sec":600}),
        ),
        (
            "video/list",
            serde_json::json!({"videos":[{"item_id":"synthetic-public","share_url":"https://www.tiktok.com/@synthetic/video/123","media_type":"VIDEO","is_ad":false}]}),
        ),
    ] {
        Mock::given(matchers::method("GET"))
            .and(matchers::path(format!("/tiktok/business/{path}/")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":body})),
            )
            .mount(server)
            .await;
    }
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/tiktok/business/video/publish/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"code":0,"data":{"share_id":"synthetic-task"}})),
        )
        .expect(1)
        .mount(server)
        .await;
}

/// A retained provider task survives executor loss during polling. Recovery
/// reads its status and public link without submitting the video twice.
#[tokio::test]
async fn a_tiktok_task_resumes_polling_without_resubmitting() {
    let Some(db) = database().await else { return };
    let fixture = Fixture::new("social-tiktok-recovery").await;
    let server = MockServer::start().await;
    let publish_at = scheduled_in(1);
    let post_id = scheduled_tiktok_post(&fixture, publish_at).await;
    mount_tiktok(&server).await;
    let pending =
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/tiktok/business/publish/status/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"code":0,"data":{"status":"PROCESSING_DOWNLOAD"}}),
            ))
            .mount_as_scoped(&server)
            .await;
    let make_context = || {
        let refresh = workflows::social_publish::TokenRefreshConfig {
            tiktok_client: Some(("synthetic-app".into(), "synthetic-secret".into())),
            ..workflows::social_publish::TokenRefreshConfig::from_env()
        };
        let publisher = Publisher::with_parts(
            fixture.db.clone(),
            Arc::new(fixture.kms.clone()),
            fixture.storage.clone(),
            store::config::SocialEgressGate::new(|| Ok(())),
            refresh,
            reqwest::Client::new(),
            |key| format!("https://assets.example.test/{key}"),
        )
        .with_provider_base(&server.uri());
        PublisherContext::new(publisher)
            .with_pacing(Pacing::default().with_tiktok_poll(Duration::from_millis(50)))
    };
    let worker = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = dbos_social_publisher::register_publisher(&worker, make_context()).unwrap();
    worker.launch().await.unwrap();
    dbos_social_publisher::fire_publisher(
        &workflow,
        SocialPublishRequest {
            post_id,
            publish_at,
        },
    )
    .await
    .unwrap();
    tokio::time::timeout(DEADLINE, async {
        while request_count(&server, "/tiktok/business/publish/status/").await < 2 {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("the accepted task is polled before executor loss");
    worker.shutdown().await;
    drop(pending);
    Mock::given(matchers::method("GET")).and(matchers::path("/tiktok/business/publish/status/"))
        .and(matchers::query_param("publish_id", "synthetic-task"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":{"status":"PUBLISH_COMPLETE","post_ids":["synthetic-public"]}})))
        .mount(&server).await;
    let resumed = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let _workflow = dbos_social_publisher::register_publisher(&resumed, make_context()).unwrap();
    resumed.launch().await.unwrap();
    wait_for_status(
        &db,
        &dbos_social_publisher::publisher_workflow_id(post_id, publish_at),
        "SUCCESS",
    )
    .await;
    resumed.shutdown().await;
    assert_eq!(
        request_count(&server, "/tiktok/business/video/publish/").await,
        1
    );
    let publication = store::social_publications::for_posts(&fixture.db, &[post_id])
        .await
        .unwrap()
        .remove(0);
    assert_eq!(publication.state, "confirmed");
    assert_eq!(
        publication.provider_post_id.as_deref(),
        Some("synthetic-public")
    );
    assert_eq!(
        publication.permalink.as_deref(),
        Some("https://www.tiktok.com/@synthetic/video/123")
    );
}
