use super::*;
use crate::inbox::{self, IngestInbound};
use crate::persons::{NewPerson, Role};
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::surreal::SurrealDb;
use crate::test_support::{mem_surreal, seed_entity};

struct Fixture {
    pg: PostgresDb,
    db: SurrealDb,
    firm: Uuid,
    owner: Uuid,
    admin: Uuid,
    project: Uuid,
    other_project: Uuid,
}

async fn fixture() -> Option<Fixture> {
    let (options, database) = fresh_schema().await?;
    let pg = connect(&options, &database).await;
    let db = mem_surreal().await;
    let owner = crate::persons::create(
        &db,
        &NewPerson::with_role("Owner", "call-owner@example.test", Role::Owner),
    )
    .await
    .unwrap()
    .id;
    let admin = crate::persons::create(
        &db,
        &NewPerson::with_role("Admin", "call-admin@example.test", Role::Admin),
    )
    .await
    .unwrap()
    .id;
    let firm = crate::firms::create(
        &db,
        &crate::firms::NewFirm {
            name: "Call Practice".into(),
            entity_id: seed_entity(&db).await,
            status: "active".into(),
            admin_dri_person_id: admin,
        },
    )
    .await
    .unwrap()
    .id;
    crate::brands::create(
        &db,
        Role::Admin,
        Some(admin),
        &crate::brands::NewBrand {
            key: "call-brand".into(),
            name: "Call Brand".into(),
            firm_id: Some(firm),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    crate::firms::attach_brand(&db, Role::Owner, None, firm, "call-brand")
        .await
        .unwrap();
    let mut projects = Vec::new();
    for code in ["call-project", "other-call-project"] {
        projects.push(
            crate::projects::create(
                &db,
                &crate::projects::NewProject {
                    code: code.into(),
                    name: code.into(),
                    status: "open".into(),
                    entity_id: seed_entity(&db).await,
                    firm_id: Some(firm),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .id,
        );
    }
    Some(Fixture {
        pg,
        db,
        firm,
        owner,
        admin,
        project: projects[0],
        other_project: projects[1],
    })
}

fn inbound<'a>(f: &Fixture, channel: &'a str, id: &'a str) -> IngestInbound<'a> {
    IngestInbound {
        project_id: None,
        firm_id: f.firm,
        brand_key: Some("call-brand"),
        interaction_type: None,
        provider_post_id: None,
        parent_provider_message_id: None,
        visibility: Some("private"),
        channel,
        provider_account: "+15555550123",
        provider_message_id: id,
        provider_thread_ref: None,
        external_address: "+15555550456",
        external_name: None,
        subject: None,
        body: "Synthetic metadata",
        source_page: None,
        source_channel: Some(channel),
        matter_consent: false,
        promotional_opt_in: false,
        email_conversation_id: None,
        attachments: Vec::new(),
        occurred_at: "2026-10-30T18:00:00Z",
        raw_storage_key: None,
    }
}

async fn call(f: &Fixture, sid: &str) -> NewBilledCall {
    let saved = inbox::ingest_inbound(&f.db, &inbound(f, inbox::channel::VOICE, sid))
        .await
        .unwrap();
    let input = NewBilledCall {
        call_sid: sid.into(),
        conversation_id: saved.conversation_id,
        firm_id: f.firm,
        duration_seconds: 361,
        call_started_at: "2026-10-30T18:00:00Z".parse().unwrap(),
    };
    record_for_conversation(&f.pg, &f.db, &input).await.unwrap();
    input
}

#[tokio::test]
async fn initial_call_attribution_replays_repairs_and_refuses_a_move_without_changing_facts() {
    let Some(f) = fixture().await else { return };
    let input = call(&f, "CAinitial").await;
    let first = find_by_call_sid(&f.pg, &input.call_sid)
        .await
        .unwrap()
        .unwrap();
    let delayed = inbox::ingest_inbound(&f.db, &inbound(&f, inbox::channel::VOICE, "CAlater"))
        .await
        .unwrap();
    assert_eq!(delayed.conversation_id, input.conversation_id);
    assign_conversation(&f.pg, &f.db, f.owner, input.conversation_id, f.project)
        .await
        .unwrap();
    assign_conversation(&f.pg, &f.db, f.owner, input.conversation_id, f.project)
        .await
        .unwrap();
    assert!(assign_conversation(
        &f.pg,
        &f.db,
        f.owner,
        input.conversation_id,
        f.other_project
    )
    .await
    .is_err());
    let saved = find_by_call_sid(&f.pg, &input.call_sid)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.project_id, Some(f.project));
    assert_eq!(saved.duration_seconds, first.duration_seconds);
    assert_eq!(saved.tenths, first.tenths);
    assert_eq!(saved.call_started_at, first.call_started_at);
    assert_eq!(saved.inserted_at, first.inserted_at);
    let replay = record_for_conversation(&f.pg, &f.db, &input).await.unwrap();
    assert_eq!(replay.project_id, Some(f.project));
    // Source saved before billing checkpoint: retry repairs the original target.
    sqlx::query("UPDATE billed_call SET project_id=NULL WHERE call_sid=$1")
        .bind(&input.call_sid)
        .execute(f.pg.pool())
        .await
        .unwrap();
    assign_conversation(&f.pg, &f.db, f.owner, input.conversation_id, f.project)
        .await
        .unwrap();
    assert_eq!(
        find_by_call_sid(&f.pg, &input.call_sid)
            .await
            .unwrap()
            .unwrap()
            .project_id,
        Some(f.project)
    );
    // A delayed terminal callback inherits the already-attributed Conversation.
    let next = NewBilledCall {
        call_sid: "CAlater".into(),
        conversation_id: delayed.conversation_id,
        firm_id: f.firm,
        duration_seconds: 42,
        call_started_at: input.call_started_at,
    };
    record_for_conversation(&f.pg, &f.db, &next).await.unwrap();
    assert_eq!(
        find_by_call_sid(&f.pg, &next.call_sid)
            .await
            .unwrap()
            .unwrap()
            .project_id,
        Some(f.project)
    );
}

#[tokio::test]
async fn unparticipating_lawyer_cannot_attribute_or_read_an_admin_brand_report() {
    let Some(f) = fixture().await else { return };
    let input = call(&f, "CAdenied").await;
    let unrelated = crate::persons::create(
        &f.db,
        &NewPerson::with_role("Unrelated", "unrelated@example.test", Role::Lawyer),
    )
    .await
    .unwrap()
    .id;
    assert!(
        assign_conversation(&f.pg, &f.db, unrelated, input.conversation_id, f.project)
            .await
            .is_err()
    );
    assert!(
        crate::communication_dashboard::for_brand(&f.pg, &f.db, unrelated, "call-brand")
            .await
            .is_err()
    );
    assert_eq!(
        find_by_call_sid(&f.pg, &input.call_sid)
            .await
            .unwrap()
            .unwrap()
            .project_id,
        None
    );
}

#[tokio::test]
async fn brand_report_counts_metadata_and_initial_attribution_changes_only_parked_count() {
    let Some(f) = fixture().await else { return };
    let input = call(&f, "CAreport").await;
    inbox::ingest_inbound(&f.db, &inbound(&f, inbox::channel::SMS, "SMreport"))
        .await
        .unwrap();
    sqlx::query("INSERT INTO brand_phone_number(number_e164,brand_key,twilio_account_sid,auth_token_secret_ref,channel_kinds) VALUES('+15555550123','call-brand','ACsynthetic',$1,'both')")
        .bind(Uuid::now_v7()).execute(f.pg.pool()).await.unwrap();
    let before = crate::communication_dashboard::for_brand(&f.pg, &f.db, f.admin, "call-brand")
        .await
        .unwrap();
    assert_eq!(
        (
            before.calls,
            before.sms,
            before.billed_tenths,
            before.parked_calls
        ),
        (1, 1, 2, 1)
    );
    assert_eq!(before.registered_numbers, ["+15555550123"]);
    assign_conversation(&f.pg, &f.db, f.owner, input.conversation_id, f.project)
        .await
        .unwrap();
    let after = crate::communication_dashboard::for_brand(&f.pg, &f.db, f.admin, "call-brand")
        .await
        .unwrap();
    assert_eq!(
        (
            after.calls,
            after.sms,
            after.billed_tenths,
            after.parked_calls
        ),
        (1, 1, 2, 0)
    );
    assert_eq!(
        crate::communication_dashboard::brand_keys_for_actor(&f.db, f.admin)
            .await
            .unwrap(),
        ["call-brand"]
    );
}

#[tokio::test]
async fn brand_reports_do_not_mix_other_brands_or_foreign_firm_members() {
    let Some(f) = fixture().await else { return };
    call(&f, "CAbrand-isolation").await;
    crate::brands::create(
        &f.db,
        Role::Admin,
        Some(f.admin),
        &crate::brands::NewBrand {
            key: "other-brand".into(),
            name: "Other Brand".into(),
            firm_id: Some(f.firm),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    crate::firms::attach_brand(&f.db, Role::Owner, None, f.firm, "other-brand")
        .await
        .unwrap();
    let mut other = inbound(&f, inbox::channel::VOICE, "CAother-brand");
    other.brand_key = Some("other-brand");
    other.provider_account = "+15555550789";
    let saved = inbox::ingest_inbound(&f.db, &other).await.unwrap();
    record_for_conversation(
        &f.pg,
        &f.db,
        &NewBilledCall {
            call_sid: "CAother-brand".into(),
            conversation_id: saved.conversation_id,
            firm_id: f.firm,
            duration_seconds: 721,
            call_started_at: "2026-10-30T18:00:00Z".parse().unwrap(),
        },
    )
    .await
    .unwrap();
    let first = crate::communication_dashboard::for_brand(&f.pg, &f.db, f.admin, "call-brand")
        .await
        .unwrap();
    let second = crate::communication_dashboard::for_brand(&f.pg, &f.db, f.admin, "other-brand")
        .await
        .unwrap();
    assert_eq!(
        (first.calls, first.billed_tenths, first.parked_calls),
        (1, 2, 1)
    );
    assert_eq!(
        (second.calls, second.billed_tenths, second.parked_calls),
        (1, 3, 1)
    );
    let foreign_admin = crate::persons::create(
        &f.db,
        &NewPerson::with_role(
            "Foreign Admin",
            "foreign-call-admin@example.test",
            Role::Admin,
        ),
    )
    .await
    .unwrap()
    .id;
    crate::firms::create(
        &f.db,
        &crate::firms::NewFirm {
            name: "Other Practice".into(),
            entity_id: seed_entity(&f.db).await,
            status: "active".into(),
            admin_dri_person_id: foreign_admin,
        },
    )
    .await
    .unwrap();
    assert!(
        crate::communication_dashboard::for_brand(&f.pg, &f.db, foreign_admin, "call-brand")
            .await
            .is_err()
    );
    assert!(
        crate::communication_dashboard::brand_keys_for_actor(&f.db, foreign_admin)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(visible_calls(&f.pg, &f.db, foreign_admin)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn concurrent_initial_choices_leave_one_project_and_an_immutable_bill() {
    let Some(f) = fixture().await else { return };
    let input = call(&f, "CAcompeting").await;
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = Vec::new();
    for index in 0..8 {
        let pg = f.pg.clone();
        let db = f.db.clone();
        let barrier = barrier.clone();
        let owner = f.owner;
        let conversation = input.conversation_id;
        let project = if index % 2 == 0 {
            f.project
        } else {
            f.other_project
        };
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            assign_conversation(&pg, &db, owner, conversation, project).await
        }));
    }
    let mut successes = Vec::new();
    for task in tasks {
        if let Ok(assignment) = task.await.unwrap() {
            successes.push(assignment.to_project_id);
        }
    }
    assert!(!successes.is_empty());
    assert!(successes.iter().all(|project| *project == successes[0]));
    let row = find_by_call_sid(&f.pg, &input.call_sid)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.project_id, Some(successes[0]));
    assert_eq!(row.duration_seconds, 361);
    assert_eq!(row.tenths, 2);
    let again = assign_conversation(&f.pg, &f.db, f.owner, input.conversation_id, successes[0])
        .await
        .unwrap();
    assert_eq!(again.to_project_id, successes[0]);
    assert_eq!(
        inbox::assignments(&f.db, input.conversation_id)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn missed_calls_park_without_invented_duration_or_billing_and_leave_after_assignment() {
    let Some(f) = fixture().await else { return };
    let saved = inbox::ingest_inbound(&f.db, &inbound(&f, inbox::channel::VOICE, "CAmissed"))
        .await
        .unwrap();
    let queued = parked_calls(&f.pg, &f.db, f.admin).await.unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].duration_seconds, None);
    assert_eq!(queued[0].tenths, None);
    let report = crate::communication_dashboard::for_brand(&f.pg, &f.db, f.admin, "call-brand")
        .await
        .unwrap();
    assert_eq!(
        (report.calls, report.parked_calls, report.billed_tenths),
        (1, 1, 0)
    );
    assign_conversation(&f.pg, &f.db, f.owner, saved.conversation_id, f.project)
        .await
        .unwrap();
    assert!(parked_calls(&f.pg, &f.db, f.admin)
        .await
        .unwrap()
        .is_empty());
    assert!(find_by_call_sid(&f.pg, "CAmissed").await.unwrap().is_none());
}
