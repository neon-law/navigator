//! `CeoDigest` on DBOS: one child per Firm with isolation between them, the
//! prose model's input pinned to the journaled query output, and a weekday
//! alert that posts once and stays silent on weekends and empty days.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{TimeZone, Utc};
use store::firm_digest::{FirmDeadlines, MatterLine, NeedsYouItem};
use uuid::Uuid;
use workflows::dbos_engine::ceo_digest::{
    self, AlertReport, AssumptionState, CeoDigestReport, Context, DigestBrief, DigestError,
    DigestSource, FirmRef, OwnerContact, ProseProvider,
};
use workflows::email::service::CapturingEmail;
use workflows::{dbos_engine, CapturingSlackBot};

use crate::support::{database, APP_VERSION, DEADLINE};

struct FirmFixture {
    firm_id: Uuid,
    name: &'static str,
    owner_email: &'static str,
    open_plan: i64,
    open_other: i64,
    needs_you: usize,
    target_minutes: i64,
}

/// A two-Firm digest source: every query is Firm-keyed and serves only the
/// fixture's own figures, so a child that reads another Firm's data fails the
/// assertions below.
#[derive(Clone)]
struct TwoFirmSource {
    fixtures: Arc<Vec<FirmFixture>>,
    briefs_seen: Arc<Mutex<Vec<DigestBrief>>>,
}

impl TwoFirmSource {
    fn fixture(&self, firm_id: Uuid) -> &FirmFixture {
        self.fixtures
            .iter()
            .find(|fixture| fixture.firm_id == firm_id)
            .expect("fixture for the requested Firm")
    }
}

#[async_trait::async_trait]
impl DigestSource for TwoFirmSource {
    async fn firms(&self) -> Result<Vec<FirmRef>, DigestError> {
        Ok(self
            .fixtures
            .iter()
            .map(|fixture| FirmRef {
                firm_id: fixture.firm_id,
                name: fixture.name.to_owned(),
            })
            .collect())
    }

    async fn firm_brief(&self, firm_id: Uuid) -> Result<DigestBrief, DigestError> {
        let fixture = self.fixture(firm_id);
        let brief = DigestBrief {
            firm_id,
            firm_name: fixture.name.to_owned(),
            open_matters: [
                (MatterLine::Plan, fixture.open_plan),
                (MatterLine::Other, fixture.open_other),
            ]
            .into_iter()
            .collect::<BTreeMap<_, _>>(),
            money: BTreeMap::default(),
            deadlines: FirmDeadlines {
                due_soon: Vec::new(),
                missed: Vec::new(),
            },
            needs_you: Vec::new(),
            minutes: Vec::new(),
            last_activity: Vec::new(),
            economics: Vec::new(),
            assumptions: vec![AssumptionState {
                key: "plan.target_minutes.document_review".to_owned(),
                version: 1,
                value: serde_json::json!(fixture.target_minutes),
            }],
        };
        self.briefs_seen
            .lock()
            .expect("briefs lock poisoned")
            .push(brief.clone());
        Ok(brief)
    }

    async fn needs_you(&self, firm_id: Uuid) -> Result<Vec<NeedsYouItem>, DigestError> {
        let fixture = self.fixture(firm_id);
        Ok((0..fixture.needs_you)
            .map(|n| NeedsYouItem {
                project_id: Uuid::new_v4(),
                conversation_id: Uuid::new_v4(),
                external_name: None,
                subject: Some(format!("question {n}")),
                last_inbound: Utc::now(),
            })
            .collect())
    }

    async fn owners(&self, firm_id: Uuid) -> Result<Vec<OwnerContact>, DigestError> {
        let fixture = self.fixture(firm_id);
        Ok(vec![OwnerContact {
            name: fixture.name.to_owned(),
            email: fixture.owner_email.to_owned(),
        }])
    }
}

/// The prose double: records every brief it is handed — the model's input is
/// the journaled query output and nothing else — and returns fixed prose.
#[derive(Clone, Default)]
struct RecordingProse;

#[async_trait::async_trait]
impl ProseProvider for RecordingProse {
    async fn write_brief(&self, _brief: &DigestBrief) -> Result<String, DigestError> {
        Ok("The week in review.".to_owned())
    }
}

fn lookup() -> impl Fn(&str) -> Option<String> {
    |key: &str| match key {
        "SLACK_BOT_TOKEN" => Some("xoxb-test".to_owned()),
        "SLACK_MESSAGES_CHANNEL_ID" => Some("C-MESSAGES".to_owned()),
        _ => None,
    }
}

fn monday_slot() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 9, 14, 0, 0).unwrap()
}

async fn report_of(dbos: &dbos::DBOS, id: &str) -> CeoDigestReport {
    tokio::time::timeout(
        DEADLINE,
        dbos.retrieve_workflow::<CeoDigestReport, DigestError>(id)
            .expect("retrieve CeoDigest")
            .result(),
    )
    .await
    .expect("CeoDigest did not finish")
    .expect("CeoDigest succeeds")
}

#[tokio::test]
async fn each_firm_gets_its_own_child_and_the_model_sees_only_that_firms_figures() {
    let Some(db) = database().await else {
        return;
    };
    let fixtures = vec![
        FirmFixture {
            firm_id: Uuid::new_v4(),
            name: "Guieb & Shook PC",
            owner_email: "owner@guiebshook.example.com",
            open_plan: 3,
            open_other: 1,
            needs_you: 1,
            target_minutes: 45,
        },
        FirmFixture {
            firm_id: Uuid::new_v4(),
            name: "Lex Tecnica",
            owner_email: "owner@lextecnica.example.com",
            open_plan: 7,
            open_other: 0,
            needs_you: 0,
            target_minutes: 60,
        },
    ];
    let prose = RecordingProse;
    let email = CapturingEmail::new();
    let bot = CapturingSlackBot::new();
    let source = TwoFirmSource {
        fixtures: Arc::new(fixtures),
        briefs_seen: Arc::new(Mutex::new(Vec::new())),
    };
    let context = Context::from_lookup(
        Arc::new(source.clone()),
        Arc::new(prose),
        Arc::new(email.clone()),
        false,
        lookup(),
    )
    .expect("delivery configuration");
    let executor = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let registered = ceo_digest::register(&executor, context).expect("register CeoDigest");
    executor.launch().await.expect("launch");
    ceo_digest::fire_weekly(&registered.weekly, monday_slot())
        .await
        .expect("start the weekly slot");
    let report = report_of(&executor, &ceo_digest::workflow_id(monday_slot())).await;
    executor.shutdown().await;

    // One child per Firm, each reporting only its own figures.
    assert_eq!(report.firms.len(), 2);
    for (fixture, firm_report) in source.fixtures.iter().zip(&report.firms) {
        assert_eq!(firm_report.firm_id, fixture.firm_id);
        assert_eq!(
            firm_report.open_matters,
            fixture.open_plan + fixture.open_other
        );
        assert_eq!(firm_report.needs_you, fixture.needs_you);
        assert_eq!(firm_report.emailed, 1);
        assert_eq!(
            firm_report.assumptions,
            vec![("plan.target_minutes.document_review".to_owned(), 1)]
        );
    }

    // The model's input was the journaled brief of each Firm and nothing
    // else: exactly two calls, each brief matching its own fixture.
    let briefs = source.briefs_seen.lock().expect("briefs lock poisoned");
    assert_eq!(briefs.len(), 2);
    for brief in briefs.iter() {
        let fixture = source.fixture(brief.firm_id);
        assert_eq!(brief.firm_name, fixture.name);
        assert_eq!(brief.open_matters[&MatterLine::Plan], fixture.open_plan);
        assert_eq!(
            brief.assumptions[0].value,
            serde_json::json!(fixture.target_minutes)
        );
    }

    // Each owner received their own Firm's email, addressed to them alone.
    let sent = email.captured();
    assert_eq!(sent.len(), 2);
    for fixture in source.fixtures.iter() {
        let mail = sent
            .iter()
            .find(|mail| mail.to == fixture.owner_email)
            .unwrap_or_else(|| panic!("email for {}", fixture.name));
        assert!(
            mail.body.contains(fixture.name),
            "{} brief body",
            fixture.name
        );
        assert!(mail.body.contains(&fixture.target_minutes.to_string()));
    }

    // The Slack line echoes the assumptions' versions, never their values.
    let posts = bot.posted_messages();
    assert_eq!(posts.len(), 2);
    for fixture in source.fixtures.iter() {
        let post = posts
            .iter()
            .find(|(_, text)| text.contains(fixture.name))
            .unwrap_or_else(|| panic!("Slack post for {}", fixture.name));
        assert!(post.1.contains("plan.target_minutes.document_review v1"));
        assert!(!post.1.contains(&fixture.target_minutes.to_string()));
    }
}

#[tokio::test]
async fn the_alert_posts_once_on_a_weekday_and_stays_silent_on_a_weekend_or_empty_day() {
    let Some(db) = database().await else {
        return;
    };
    let fixture = FirmFixture {
        firm_id: Uuid::new_v4(),
        name: "Guieb & Shook PC",
        owner_email: "owner@guiebshook.example.com",
        open_plan: 3,
        open_other: 1,
        needs_you: 1,
        target_minutes: 45,
    };
    let source = TwoFirmSource {
        fixtures: Arc::new(vec![fixture]),
        briefs_seen: Arc::new(Mutex::new(Vec::new())),
    };
    let email = CapturingEmail::new();
    let bot = CapturingSlackBot::new();
    let context = Context::from_lookup(
        Arc::new(source.clone()),
        Arc::new(RecordingProse),
        Arc::new(email),
        false,
        lookup(),
    )
    .expect("delivery configuration");
    let executor = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let registered = ceo_digest::register(&executor, context).expect("register CeoDigest");
    executor.launch().await.expect("launch");
    ceo_digest::fire_alert(&registered.alert, monday_slot())
        .await
        .expect("start the weekday slot");
    let monday = tokio::time::timeout(
        DEADLINE,
        executor
            .retrieve_workflow::<AlertReport, DigestError>(&ceo_digest::alert_workflow_id(
                monday_slot(),
            ))
            .expect("retrieve alert")
            .result(),
    )
    .await
    .expect("alert did not finish")
    .expect("alert succeeds");

    assert_eq!(monday.firms.len(), 1);
    assert!(monday.firms[0].posted);
    assert_eq!(monday.firms[0].threads, 1);
    assert_eq!(bot.posted_messages().len(), 1);

    // A replayed start of the same slot joins the journaled run: no second
    // post.
    ceo_digest::fire_alert(&registered.alert, monday_slot())
        .await
        .expect("restart the weekday slot");
    executor.shutdown().await;
    assert_eq!(bot.posted_messages().len(), 1);
}

#[tokio::test]
async fn a_weekend_alert_slot_posts_nothing() {
    let Some(db) = database().await else {
        return;
    };
    let fixture = FirmFixture {
        firm_id: Uuid::new_v4(),
        name: "Guieb & Shook PC",
        owner_email: "owner@guiebshook.example.com",
        open_plan: 3,
        open_other: 1,
        needs_you: 1,
        target_minutes: 45,
    };
    let source = TwoFirmSource {
        fixtures: Arc::new(vec![fixture]),
        briefs_seen: Arc::new(Mutex::new(Vec::new())),
    };
    let email = CapturingEmail::new();
    let bot = CapturingSlackBot::new();
    let context = Context::from_lookup(
        Arc::new(source.clone()),
        Arc::new(RecordingProse),
        Arc::new(email),
        false,
        lookup(),
    )
    .expect("delivery configuration");
    let executor = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let registered = ceo_digest::register(&executor, context).expect("register CeoDigest");
    executor.launch().await.expect("launch");
    let saturday = Utc.with_ymd_and_hms(2026, 11, 14, 12, 0, 0).unwrap();
    ceo_digest::fire_alert(&registered.alert, saturday)
        .await
        .expect("start the weekend slot");
    let report = tokio::time::timeout(
        DEADLINE,
        executor
            .retrieve_workflow::<AlertReport, DigestError>(&ceo_digest::alert_workflow_id(saturday))
            .expect("retrieve alert")
            .result(),
    )
    .await
    .expect("alert did not finish")
    .expect("alert succeeds");
    executor.shutdown().await;

    assert!(report.firms.iter().all(|firm| !firm.posted));
    assert!(bot.posted_messages().is_empty());
}
