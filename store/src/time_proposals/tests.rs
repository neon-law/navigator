use chrono::{Duration, Utc};
use uuid::Uuid;

use super::*;
use crate::{
    attorney_rates::{self, NewAttorneyRate},
    billing_annotations::{self, Annotation},
    billing_report::{add_project, ProjectScope},
    postgres::fixtures::{connect, fresh_schema},
};
use model::billing_report::{BillingAction, BillingReport};

async fn priced_matter(pg: &PostgresDb) -> crate::firm_side::tests::Matter {
    let fixture = crate::firm_side::tests::matter(pg).await;
    // The billing view reads a branded Project.
    sqlx::query(
        "WITH owner AS (INSERT INTO brand(name,brand_key,firm_id)
         SELECT 'Synthetic Time Brand', 'time-fixture', firm_id FROM project WHERE id=$1
         RETURNING brand_key)
         UPDATE project SET brand=owner.brand_key FROM owner WHERE project.id=$1",
    )
    .bind(fixture.project_id)
    .execute(pg.pool())
    .await
    .unwrap();
    attorney_rates::set_hourly_rate(
        pg,
        NewAttorneyRate {
            person_id: fixture.lawyer,
            project_id: None,
            hourly_rate_cents: 30_000,
            effective_on: Utc::now().date_naive() - Duration::days(365),
        },
    )
    .await
    .unwrap();
    fixture
}

async fn project_report(
    pg: &PostgresDb,
    fixture: &crate::firm_side::tests::Matter,
) -> BillingReport {
    let project = crate::postgres::projects::find_by_id_on(pg, fixture.project_id)
        .await
        .unwrap()
        .unwrap();
    let mut report = BillingReport::default();
    add_project(
        &mut report,
        pg,
        &project,
        fixture.lawyer,
        Utc::now().date_naive(),
        ProjectScope {
            responsible: true,
            project_view: true,
            claims_firm: None,
        },
    )
    .await
    .unwrap();
    report
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // The acceptance walk: add, retry, edit, approve, read back.
async fn backfilled_time_is_edited_approved_and_read_back_once() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let fixture = priced_matter(&pg).await;
    let project = fixture.project_id;
    let last_week = Utc::now().date_naive() - Duration::days(7);
    let request = NewProposal {
        request_id: Uuid::new_v4(),
        timekeeper_id: fixture.lawyer,
        performed_on: last_week,
        minutes: 30,
        narrative: "Synthetic call with the client".into(),
    };
    let added = add(&pg, project, fixture.lawyer, &request).await.unwrap();
    assert_eq!(added.status, "pending_review");
    assert!(added.backfilled);
    assert_eq!(added.source, "lawyer");
    let report = project_report(&pg, &fixture).await;
    assert!(report.needs_my_action.iter().any(|action| matches!(
        action,
        BillingAction::PendingProposals {
            pending_proposals: 1,
            ..
        }
    )));
    let pending = &report.project_time.as_ref().unwrap()[0];
    assert_eq!(
        (pending.status.as_str(), pending.backfilled),
        ("pending_review", true)
    );

    let edited = edit(
        &pg,
        project,
        added.id,
        &ProposalEdit {
            minutes: Some(18),
            ..ProposalEdit::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(edited.minutes, 18);
    assert_eq!(edited.recorded_at, added.recorded_at);

    // A retry of the original add returns the same, edited row.
    let replayed = add(&pg, project, fixture.lawyer, &request).await.unwrap();
    assert_eq!(replayed.id, added.id);
    assert_eq!(replayed.minutes, 18);
    let approval = Approval {
        decision: Decision::Bill,
        claim_id: None,
        narrative: None,
        minutes: None,
    };
    let approved = approve(&pg, project, fixture.lawyer, added.id, &approval)
        .await
        .unwrap();
    assert_eq!(approved.status, "approved");
    let entry_id = approved.entry_id.unwrap();
    assert_eq!(
        approve(&pg, project, fixture.lawyer, added.id, &approval)
            .await
            .unwrap(),
        approved,
        "a retried approval changes nothing"
    );
    assert_eq!(
        add(&pg, project, fixture.lawyer, &request)
            .await
            .unwrap()
            .id,
        added.id
    );
    let entry = crate::time_entries::find_by_id(&pg, entry_id)
        .await
        .unwrap();
    assert_eq!(
        (entry.minutes, entry.rate_cents_per_hour, entry.performed_on),
        (18, 30_000, last_week)
    );

    let report = project_report(&pg, &fixture).await;
    let time = report.project_time.unwrap();
    assert_eq!(time.len(), 1, "one entry after the retries: {time:?}");
    assert_eq!(
        (
            time[0].id,
            time[0].minutes,
            time[0].backfilled,
            time[0].status.as_str()
        ),
        (entry_id, 18, true, "unbilled")
    );
    let unbilled = list(&pg, project, Some(StatusFilter::Unbilled))
        .await
        .unwrap();
    assert_eq!(unbilled.len(), 1);
    assert_eq!(
        (unbilled[0].source.as_str(), unbilled[0].backfilled),
        ("lawyer", true)
    );
    assert!(list(&pg, project, Some(StatusFilter::PendingReview))
        .await
        .unwrap()
        .is_empty());
    assert!(list(&pg, project, Some(StatusFilter::Billed))
        .await
        .unwrap()
        .is_empty());

    assert!(matches!(
        edit(
            &pg,
            project,
            added.id,
            &ProposalEdit {
                minutes: Some(6),
                ..ProposalEdit::default()
            }
        )
        .await,
        Err(ProposalError::Decided(_))
    ));
    assert!(matches!(
        discard(&pg, project, fixture.lawyer, added.id, None).await,
        Err(ProposalError::Decided(_))
    ));
    pg.pool().close().await;
}

#[tokio::test]
async fn proposals_refuse_future_work_outside_timekeepers_and_reused_requests() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let fixture = priced_matter(&pg).await;
    let project = fixture.project_id;
    let today = Utc::now().date_naive();
    let proposal = |timekeeper_id, performed_on, minutes| NewProposal {
        request_id: Uuid::new_v4(),
        timekeeper_id,
        performed_on,
        minutes,
        narrative: "Synthetic review".into(),
    };
    for (input, expected) in [
        (
            proposal(fixture.lawyer, today + Duration::days(1), 6),
            "future",
        ),
        (proposal(fixture.lawyer, today, 0), "minutes"),
        (proposal(fixture.lawyer, today, 1_441), "minutes"),
    ] {
        let error = add(&pg, project, fixture.lawyer, &input).await.unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }
    for outsider in [fixture.client, fixture.outside_lawyer] {
        assert!(matches!(
            add(&pg, project, fixture.lawyer, &proposal(outsider, today, 6)).await,
            Err(ProposalError::NotParticipant)
        ));
    }
    let first = proposal(fixture.lawyer, today, 6);
    let added = add(&pg, project, fixture.lawyer, &first).await.unwrap();
    assert!(!added.backfilled, "same-day time is not backfilled");
    let reused = NewProposal {
        timekeeper_id: fixture.clerk,
        ..first
    };
    assert!(matches!(
        add(&pg, project, fixture.lawyer, &reused).await,
        Err(ProposalError::RequestReused)
    ));
    let discarded = discard(&pg, project, fixture.lawyer, added.id, None)
        .await
        .unwrap();
    assert_eq!(discarded.status, "discarded");
    assert_eq!(
        discard(&pg, project, fixture.lawyer, added.id, None)
            .await
            .unwrap(),
        discarded
    );
    let approval = Approval {
        decision: Decision::NoCharge,
        claim_id: None,
        narrative: None,
        minutes: None,
    };
    assert!(matches!(
        approve(&pg, project, fixture.lawyer, added.id, &approval).await,
        Err(ProposalError::Decided(_))
    ));
    assert!(matches!(
        approve(&pg, project, fixture.lawyer, Uuid::new_v4(), &approval).await,
        Err(ProposalError::NotFound)
    ));
    assert_eq!(
        list(&pg, project, Some(StatusFilter::Discarded))
            .await
            .unwrap(),
        vec![discarded]
    );
    let deleted = sqlx::query("DELETE FROM time_entry_proposal WHERE id=$1")
        .bind(added.id)
        .execute(pg.pool())
        .await;
    assert!(deleted.is_err(), "proposals are retained");
    pg.pool().close().await;
}

#[tokio::test]
async fn staged_jujutsu_time_is_reviewed_through_the_same_verbs() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let fixture = priced_matter(&pg).await;
    let project = fixture.project_id;
    let today = Utc::now().date_naive();
    let staged = |change: char, tenths| Annotation {
        id: Uuid::new_v4(),
        change_id: std::iter::repeat_n(change, 32).collect(),
        commit_id: "a".repeat(40),
        tenths,
        performed_on: today,
    };
    let (kept, denied) = (staged('k', 10), staged('m', 2));
    for annotation in [&kept, &denied] {
        billing_annotations::stage(&pg, project, fixture.lawyer, annotation)
            .await
            .unwrap();
    }
    let pending = list(&pg, project, Some(StatusFilter::PendingReview))
        .await
        .unwrap();
    assert_eq!(pending.len(), 2);
    assert!(pending.iter().all(|row| row.source == "jujutsu"));
    assert!(matches!(
        edit(&pg, project, kept.id, &ProposalEdit::default()).await,
        Err(ProposalError::Decided(message)) if message.contains("--minutes")
    ));
    let mut approval = Approval {
        decision: Decision::Bill,
        claim_id: None,
        narrative: None,
        minutes: Some(30),
    };
    assert!(matches!(
        approve(&pg, project, fixture.lawyer, kept.id, &approval).await,
        Err(ProposalError::Invalid(message)) if message.contains("--narrative")
    ));
    approval.narrative = Some("Synthetic drafting, half the staged time".into());
    approval.minutes = Some(66);
    assert!(approve(&pg, project, fixture.lawyer, kept.id, &approval)
        .await
        .is_err());
    approval.minutes = Some(30);
    let approved = approve(&pg, project, fixture.lawyer, kept.id, &approval)
        .await
        .unwrap();
    assert_eq!(
        (approved.minutes, approved.status.as_str()),
        (30, "unbilled")
    );
    let denied = discard(&pg, project, fixture.lawyer, denied.id, Some("Duplicate"))
        .await
        .unwrap();
    assert_eq!(denied.status, "discarded");
    assert!(list(&pg, project, Some(StatusFilter::PendingReview))
        .await
        .unwrap()
        .is_empty());
    pg.pool().close().await;
}

async fn seed_record(pg: &PostgresDb, fixture: &crate::firm_side::tests::Matter) {
    let project = fixture.project_id;
    let now = Utc::now();
    for (channel, direction, subject, call) in [
        (
            "call",
            "internal",
            Some("Synthetic strategy call"),
            Some(serde_json::json!({"direction":"outbound","duration_minutes":25,"recorded":"no"})),
        ),
        (
            "email_outbound",
            "outbound",
            Some("Synthetic status letter"),
            None,
        ),
        ("email_inbound", "inbound", None, None),
    ] {
        sqlx::query(
            "INSERT INTO communication (id,project_id,channel,direction,author_person_id,subject,body,
             occurred_at,call,inserted_at,updated_at) VALUES ($1,$2,$3,$4,$5,$6,'Synthetic body',$7,$8,$7,$7)",
        )
        .bind(Uuid::new_v4())
        .bind(project)
        .bind(channel)
        .bind(direction)
        .bind(fixture.lawyer)
        .bind(subject)
        .bind(now - Duration::days(3))
        .bind(call)
        .execute(pg.pool())
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO asset (storage_key,content_type,byte_size,sha256_hex,project_id,filename,kind,source,slug,inserted_at)
         VALUES ('synthetic/key','application/pdf',4,$2,$1,'2026.11.20_demand-letter.pdf','correspondence','upload','2026.11.20_demand-letter.pdf',$3)",
    )
    .bind(project)
    .bind("0".repeat(64))
    .bind(now - Duration::days(4))
    .execute(pg.pool())
    .await
    .unwrap();
    for (status, title) in [
        ("complete", "Synthetic answer due"),
        ("open", "Synthetic reply due"),
    ] {
        sqlx::query(
            "INSERT INTO project_deadlines (id,project_id,due_at,authority_kind,authority,source,status,
             calendar_role,inserted_at,updated_at,title) VALUES ($1,$2,$3,'internal','Synthetic rule','lawyer',$4,'firm',$3,$3,$5)",
        )
        .bind(Uuid::new_v4())
        .bind(project)
        .bind(now - Duration::days(2))
        .bind(status)
        .bind(title)
        .execute(pg.pool())
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn backfill_proposes_each_dated_record_once_and_approves_none() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let fixture = priced_matter(&pg).await;
    let project = fixture.project_id;
    seed_record(&pg, &fixture).await;

    let preview = backfill(&pg, project, fixture.lawyer, None, true)
        .await
        .unwrap();
    assert!(preview.dry_run);
    assert_eq!(preview.default_minutes, DEFAULT_MINUTES);
    let mut kinds = preview
        .proposals
        .iter()
        .map(|candidate| (candidate.source, candidate.minutes))
        .collect::<Vec<_>>();
    kinds.sort_by_key(|(source, minutes)| (source.as_str(), *minutes));
    assert_eq!(
        kinds,
        vec![
            (SourceKind::Communication, 6),
            (SourceKind::Communication, 12),
            (SourceKind::Communication, 30),
            (SourceKind::Deadline, 30),
            (SourceKind::Document, 30),
        ]
    );
    assert!(preview
        .proposals
        .iter()
        .all(|candidate| candidate.proposal_id.is_none()
            && candidate.timekeeper_id == fixture.lawyer));
    assert!(
        list(&pg, project, None).await.unwrap().is_empty(),
        "a dry run writes nothing"
    );

    let written = backfill(&pg, project, fixture.lawyer, None, false)
        .await
        .unwrap();
    assert_eq!(written.proposals.len(), 5);
    let pending = list(&pg, project, Some(StatusFilter::PendingReview))
        .await
        .unwrap();
    assert_eq!(pending.len(), 5);
    assert!(pending
        .iter()
        .all(|row| row.backfilled && row.source_id.is_some()));
    assert!(list(&pg, project, Some(StatusFilter::Approved))
        .await
        .unwrap()
        .is_empty());

    // Discarding one still counts its source as proposed.
    discard(&pg, project, fixture.lawyer, pending[0].id, None)
        .await
        .unwrap();
    let again = backfill(&pg, project, fixture.lawyer, None, false)
        .await
        .unwrap();
    assert!(again.proposals.is_empty(), "{:?}", again.proposals);
    let since = backfill(
        &pg,
        project,
        fixture.lawyer,
        Some(Utc::now().date_naive() + Duration::days(1)),
        true,
    )
    .await
    .unwrap();
    assert!(since.proposals.is_empty());
    assert!(matches!(
        backfill(&pg, project, fixture.client, None, true).await,
        Err(ProposalError::NotParticipant)
    ));
    pg.pool().close().await;
}
