use super::*;
use crate::{
    contingency_terms::{
        self,
        tests::database::{fee_agreement, standard},
    },
    postgres::fixtures::{connect, fresh_schema},
    time_entries::{NewTimeEntry, TimeEntrySource},
};
use chrono::Utc;

async fn billing_matter(pg: &PostgresDb) -> (crate::firm_side::tests::Matter, Project) {
    let fixture = crate::firm_side::tests::matter(pg).await;
    sqlx::query(
        "WITH owner AS (INSERT INTO brand(name,brand_key,firm_id)
         SELECT 'Synthetic Billing Brand', 'billing-fixture', firm_id FROM project WHERE id=$1
         RETURNING brand_key)
         UPDATE project SET brand=owner.brand_key FROM owner WHERE project.id=$1",
    )
    .bind(fixture.project_id)
    .execute(pg.pool())
    .await
    .unwrap();
    let project = crate::postgres::projects::find_by_id_on(pg, fixture.project_id)
        .await
        .unwrap()
        .unwrap();
    (fixture, project)
}

#[tokio::test]
async fn reports_time_by_authenticated_timekeeper_and_project_totals_once() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (fixture, project) = billing_matter(&pg).await;
    sqlx::query("INSERT INTO person_project_role (person_id,project_id,participation) VALUES ($1,$2,'lawyer')")
        .bind(fixture.outside_lawyer).bind(project.id).execute(pg.pool()).await.unwrap();
    let on = Utc::now().date_naive();
    for (attorney, minutes, rate) in [
        (fixture.lawyer, 60, 15_000),
        (fixture.outside_lawyer, 30, 18_000),
    ] {
        time_entries::create(
            &pg,
            project.id,
            attorney,
            &NewTimeEntry {
                attorney_id: attorney,
                performed_on: on,
                minutes,
                description: format!("Synthetic {minutes} minutes"),
                billable: true,
                rate_cents_per_hour: rate,
                source: TimeEntrySource::Manual,
            },
        )
        .await
        .unwrap();
    }
    for (attorney, project_view, expected_minutes, expected_fee) in [
        (fixture.lawyer, false, vec![60], 15_000),
        (fixture.outside_lawyer, false, vec![30], 9_000),
        (fixture.lawyer, true, vec![30, 60], 24_000),
    ] {
        let responsible = attorney == fixture.lawyer;
        let mut report = BillingReport::default();
        add_project(
            &mut report,
            &pg,
            &project,
            attorney,
            on,
            ProjectScope {
                responsible,
                project_view,
                claims_firm: project.firm_id,
            },
        )
        .await
        .unwrap();
        let all_time = report.project_time.as_ref().unwrap_or(&report.my_time);
        let mut minutes = all_time.iter().map(|time| time.minutes).collect::<Vec<_>>();
        minutes.sort_unstable();
        assert_eq!(minutes, expected_minutes);
        assert_eq!(report.verified_projected_fees.len(), 1);
        assert_eq!(
            report.verified_projected_fees[0].projected_fee_cents,
            expected_fee
        );
    }
    check_plan_claim(&pg, project, &fixture, on).await;
    pg.pool().close().await;
}

async fn check_plan_claim(
    pg: &PostgresDb,
    mut project: Project,
    fixture: &crate::firm_side::tests::Matter,
    on: NaiveDate,
) {
    use plan_claims::{Authorization, Change, Event, Service, Work};
    let firm = project.firm_id.unwrap();
    let plan_id: Uuid = sqlx::query_scalar(
        "INSERT INTO legal_plan (firm_id,name,acceptance_sla_hours,turnaround_sla_days)
         VALUES ($1,'ARAG',24,7) RETURNING id",
    )
    .bind(firm)
    .fetch_one(pg.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE project SET legal_plan_id=$1, plan_case_number='SYNTHETIC' WHERE id=$2")
        .bind(plan_id)
        .bind(project.id)
        .execute(pg.pool())
        .await
        .unwrap();
    project.legal_plan_id = Some(plan_id);
    let entries = time_entries::list_for_project(pg, project.id)
        .await
        .unwrap();
    let claim_id = Uuid::new_v4();
    let mut work = Work {
        service: Service::AragRepresentation,
        benefit: "Case-specific benefit".into(),
        performed_on: on,
        attorney_minutes: None,
        time_entry_ids: entries
            .iter()
            .filter(|entry| entry.attorney_id == fixture.lawyer)
            .map(|entry| entry.id)
            .collect(),
        authorization: Some(Authorization::FixedFee {
            reference: "verified-benefit".into(),
            cents: 12_000,
        }),
    };
    plan_claims::record(
        pg,
        project.id,
        firm,
        fixture.lawyer,
        Change {
            claim_id,
            event_id: Uuid::new_v4(),
            occurred_on: on,
            event: Event::WorkRecorded { work: work.clone() },
        },
    )
    .await
    .unwrap();
    let mut report = BillingReport::default();
    add_project(
        &mut report,
        pg,
        &project,
        fixture.lawyer,
        on,
        ProjectScope {
            responsible: true,
            project_view: false,
            claims_firm: Some(firm),
        },
    )
    .await
    .unwrap();
    assert_eq!(report.my_time.len(), 1);
    assert_eq!(report.my_time[0].minutes, 60);
    assert!(matches!(&report.needs_my_action[..], [
        BillingAction::Claim(_), BillingAction::UnallocatedWork { project: code, .. },
    ] if code == &project.code));
    assert_eq!(report.verified_projected_fees.len(), 1);
    assert_eq!(
        report.verified_projected_fees[0].projected_fee_cents,
        12_000
    );

    work.time_entry_ids = entries.iter().map(|entry| entry.id).collect();
    check_reconciled_claim(pg, &project, fixture, claim_id, work).await;
}

async fn check_reconciled_claim(
    pg: &PostgresDb,
    project: &Project,
    fixture: &crate::firm_side::tests::Matter,
    claim_id: Uuid,
    work: plan_claims::Work,
) {
    use plan_claims::{Change, Event};
    let firm = project.firm_id.unwrap();
    let on = work.performed_on;
    let events = [
        Event::WorkRecorded { work },
        Event::Prepared,
        Event::Submission {
            provider_reference: "submission-receipt".into(),
            claimed_cents: Some(11_000),
        },
        Event::Approval {
            reference: "decision".into(),
            cents: 10_000,
        },
        Event::Payment {
            reference: "remittance".into(),
            cents: 4_000,
        },
    ];
    for event in events {
        plan_claims::record(
            pg,
            project.id,
            firm,
            fixture.lawyer,
            Change {
                claim_id,
                event_id: Uuid::new_v4(),
                occurred_on: on,
                event,
            },
        )
        .await
        .unwrap();
    }
    for (attorney, responsible, project_view) in [
        (fixture.lawyer, true, false),
        (fixture.outside_lawyer, false, false),
        (fixture.outside_lawyer, false, true),
    ] {
        let mut report = BillingReport::default();
        add_project(
            &mut report,
            pg,
            project,
            attorney,
            on,
            ProjectScope {
                responsible,
                project_view,
                claims_firm: Some(firm),
            },
        )
        .await
        .unwrap();
        assert_eq!(report.needs_my_action.len(), usize::from(responsible));
        if responsible || project_view {
            assert_eq!(report.needs_my_action.len() + report.other_claims.len(), 1);
            let claim = if responsible {
                let BillingAction::Claim(claim) = &report.needs_my_action[0] else {
                    panic!("expected a claim action")
                };
                claim
            } else {
                &report.other_claims[0]
            };
            assert_eq!(claim.projected_fee_cents, Some(12_000));
            assert_eq!(claim.filed_cents, Some(11_000));
            assert_eq!(claim.approved_outstanding_cents, Some(6_000));
            assert_eq!(claim.net_cash_received_cents, 4_000);
            assert_eq!(report.verified_projected_fees.len(), 1);
            assert_eq!(report.verified_projected_fees[0].projected_fee_cents, 6_000);
            assert!(report.unverified_fees.is_empty());
        } else {
            assert!(report.other_claims.is_empty());
            assert!(report.verified_projected_fees.is_empty());
        }
    }
}

#[tokio::test]
async fn contingency_without_recovery_still_lists_the_agreement_and_its_tier() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (fixture, project) = billing_matter(&pg).await;
    let on = Utc::now().date_naive();
    let agreement = fee_agreement(&pg, &fixture).await;
    let mut term = standard(&fixture, agreement, on);
    term.rate_cents_per_hour = Some(12_500);
    contingency_terms::record(&pg, project.id, fixture.lawyer, &term)
        .await
        .unwrap();
    time_entries::create(
        &pg,
        project.id,
        fixture.lawyer,
        &NewTimeEntry {
            attorney_id: fixture.lawyer,
            performed_on: on,
            minutes: 30,
            description: "Synthetic completed contingent work".into(),
            billable: false,
            rate_cents_per_hour: 0,
            source: TimeEntrySource::Manual,
        },
    )
    .await
    .unwrap();

    let mut billing = BillingReport::default();
    add_project(
        &mut billing,
        &pg,
        &project,
        fixture.lawyer,
        on,
        ProjectScope {
            responsible: true,
            project_view: false,
            claims_firm: project.firm_id,
        },
    )
    .await
    .unwrap();
    assert!(billing.verified_projected_fees.is_empty());
    let report = &billing.contingency[0];
    assert_eq!(report.fee_agreement_notation_id, agreement);
    assert_eq!(report.hybrid_rate_cents_per_hour, Some(12_500));
    assert_eq!(report.tiers[0].stage, "Intake");
    assert_eq!(report.tiers[0].fee_bps, 3_330);
    assert_eq!(report.tiers[1].stage, "Trial");
    assert_eq!(report.tiers[1].fee_bps, 4_000);
    assert!(report.outcomes.is_empty());
    assert_eq!(report.projected_fee_cents, None);
    assert_eq!(report.fee_received_cents, None);
    pg.pool().close().await;
}

#[tokio::test]
async fn only_a_completed_engagement_projects_the_full_flat_fee() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (fixture, mut project) = billing_matter(&pg).await;
    attorney_rates::set_flat_fee(&pg, project.id, 50_000)
        .await
        .unwrap();
    let on = Utc::now().date_naive();
    time_entries::create(
        &pg,
        project.id,
        fixture.lawyer,
        &NewTimeEntry {
            attorney_id: fixture.lawyer,
            performed_on: on,
            minutes: 30,
            billable: true,
            rate_cents_per_hour: 10_000,
            description: "Synthetic work".into(),
            source: TimeEntrySource::Manual,
        },
    )
    .await
    .unwrap();
    project.status = "closed".into();
    for reason in [
        model::projects::ClosureReason::ClientTerminated,
        model::projects::ClosureReason::EngagementCompleted,
    ] {
        project.closure_reason = Some(reason.as_str().into());
        let mut report = BillingReport::default();
        add_project(
            &mut report,
            &pg,
            &project,
            fixture.lawyer,
            on,
            ProjectScope {
                responsible: true,
                project_view: true,
                claims_firm: project.firm_id,
            },
        )
        .await
        .unwrap();
        if reason == model::projects::ClosureReason::EngagementCompleted {
            assert_eq!(
                report.verified_projected_fees[0].projected_fee_cents,
                50_000
            );
        } else {
            assert!(report.verified_projected_fees.is_empty());
            assert_eq!(report.unverified_fees.len(), 1);
        }
    }
    pg.pool().close().await;
}

#[tokio::test]
async fn pending_annotations_are_actions_for_their_timekeeper_in_both_billing_views() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (fixture, project) = billing_matter(&pg).await;
    sqlx::query("INSERT INTO person_project_role (person_id,project_id,participation) VALUES ($1,$2,'lawyer')")
        .bind(fixture.outside_lawyer).bind(project.id).execute(pg.pool()).await.unwrap();
    let on = Utc::now().date_naive();
    let own = Uuid::new_v4();
    let colleague = Uuid::new_v4();
    for (attorney, id, change) in [
        (fixture.lawyer, own, "k".repeat(32)),
        (fixture.outside_lawyer, colleague, "l".repeat(32)),
    ] {
        billing_annotations::stage(
            &pg,
            project.id,
            attorney,
            &billing_annotations::Annotation {
                id,
                change_id: change,
                commit_id: "a".repeat(40),
                tenths: 5,
                performed_on: on,
            },
        )
        .await
        .unwrap();
    }
    for (reviewed, project_view, time_count) in
        [(false, false, 1), (false, true, 2), (true, true, 2)]
    {
        if reviewed {
            billing_annotations::review(
                &pg,
                project.id,
                fixture.lawyer,
                own,
                billing_annotations::Decision::NoCharge,
                "Reviewed as no charge",
            )
            .await
            .unwrap();
        }
        let mut report = BillingReport::default();
        add_project(
            &mut report,
            &pg,
            &project,
            fixture.lawyer,
            on,
            ProjectScope {
                responsible: true,
                project_view,
                claims_firm: project.firm_id,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            report
                .project_time
                .as_ref()
                .unwrap_or(&report.my_time)
                .len(),
            time_count
        );
        assert!(report.verified_projected_fees.is_empty());
        if reviewed {
            assert!(report.needs_my_action.is_empty());
        } else {
            assert!(matches!(&report.needs_my_action[..], [
                BillingAction::PendingAnnotation { annotation_id, .. }
            ] if *annotation_id == own));
        }
    }
    pg.pool().close().await;
}
