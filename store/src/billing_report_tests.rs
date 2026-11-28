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
        chrono_tz::UTC,
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
            provider_reference: Some("submission-receipt".into()),
            sent_message_id: None,
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
            chrono_tz::UTC,
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

async fn two_matching_plan_claims(
    pg: &PostgresDb,
    project: &Project,
    lawyer: Uuid,
    on: chrono::NaiveDate,
    synthetic_rate: i32,
) -> (Uuid, Uuid) {
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
    sqlx::query("UPDATE project SET legal_plan_id=$1,plan_case_number='SYNTHETIC' WHERE id=$2")
        .bind(plan_id)
        .bind(project.id)
        .execute(pg.pool())
        .await
        .unwrap();
    let first = Uuid::new_v4();
    let selected = Uuid::new_v4();
    for (claim_id, benefit) in [
        (first, "First synthetic service"),
        (selected, "Second synthetic service"),
    ] {
        plan_claims::record(
            pg,
            project.id,
            firm,
            lawyer,
            chrono_tz::UTC,
            Change {
                claim_id,
                event_id: Uuid::new_v4(),
                occurred_on: on,
                event: Event::WorkRecorded {
                    work: Work {
                        service: Service::AragRepresentation,
                        benefit: benefit.into(),
                        performed_on: on,
                        attorney_minutes: Some(18),
                        time_entry_ids: Vec::new(),
                        authorization: Some(Authorization::CappedHours {
                            reference: "verified-synthetic".into(),
                            rate_cents_per_hour: synthetic_rate,
                            covered_minutes: 240,
                            pending_minutes: 0,
                        }),
                    },
                },
            },
        )
        .await
        .unwrap();
    }
    (first, selected)
}

#[tokio::test]
async fn approval_prices_only_the_selected_plan_claim_and_replaces_attested_time() {
    use billing_annotations::{Annotation, Decision};
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (fixture, project) = billing_matter(&pg).await;
    let firm = project.firm_id.unwrap();
    let on = Utc::now().date_naive();
    let synthetic_rate = 10_000;
    let (first, selected) =
        two_matching_plan_claims(&pg, &project, fixture.lawyer, on, synthetic_rate).await;
    let annotation = Annotation {
        id: Uuid::new_v4(),
        change_id: "k".repeat(32),
        commit_id: "a".repeat(40),
        tenths: 3,
        performed_on: on,
    };
    billing_annotations::stage(&pg, project.id, fixture.lawyer, &annotation)
        .await
        .unwrap();
    let approve = |claim| {
        billing_annotations::review_with_claim(
            &pg,
            project.id,
            fixture.lawyer,
            annotation.id,
            Decision::PlanClaim,
            "Reviewed synthetic work",
            Some(claim),
        )
    };
    assert!(matches!(
        approve(Uuid::new_v4()).await,
        Err(time_entries::TimeEntryError::NotFound)
    ));
    approve(selected).await.unwrap();
    assert!(matches!(
        approve(selected).await,
        Err(time_entries::TimeEntryError::Conflict)
    ));
    let entries = time_entries::list_for_project(&pg, project.id)
        .await
        .unwrap();
    let [entry] = entries.as_slice() else {
        panic!("expected one entry");
    };
    assert_eq!(
        (entry.minutes, entry.rate_cents_per_hour),
        (18, synthetic_rate)
    );
    assert!(time_entries::unbilled_billable(&pg, project.id)
        .await
        .unwrap()
        .is_empty());
    let ledger: Uuid = sqlx::query_scalar(
        "INSERT INTO matter_invoice (id,project_id,source,external_id,reference,status,amount_cents,currency)
         VALUES ($1,$2,'stripe','in_synthetic','Matter synthetic','sent',1000,'USD') RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(project.id)
    .fetch_one(pg.pool())
    .await
    .unwrap();
    assert!(matches!(
        time_entries::mark_invoiced(&pg, &[entry.id], fixture.lawyer, ledger, false).await,
        Err(time_entries::TimeEntryError::Invalid(_))
    ), "claim-linked time is never invoiced directly");
    let mut connection = pg.pool().acquire().await.unwrap();
    let rows = plan_claims::for_matter_billing(&mut connection, &project, firm)
        .await
        .unwrap();
    let linked = rows.iter().find(|row| row.billing.id == selected).unwrap();
    assert_eq!(
        (linked.attorney_minutes, linked.covered_minutes),
        (Some(18), Some(240))
    );
    assert_eq!(linked.time_entry_ids, [entry.id]);
    assert!(rows
        .iter()
        .find(|row| row.billing.id == first)
        .unwrap()
        .time_entry_ids
        .is_empty());
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Exercise configuration, claims and their shared docket in one Project.
async fn configured_plan_keeps_benefit_filing_and_deadlines_independent() {
    use crate::plan_claims::{self, Authorization, Change, Event, Service, Work};
    use model::plan_filing::PlanBillingConfiguration;

    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (fixture, project) = billing_matter(&pg).await;
    let firm = project.firm_id.unwrap();
    let on = Utc::now().date_naive();
    let original_effective_on = on.pred_opt().unwrap();
    let config: PlanBillingConfiguration = serde_json::from_value(serde_json::json!({
        "name": "Synthetic configured plan",
        "filing_rules": [
            { "benefit_code": "FIXED", "effective_on": original_effective_on, "trigger": "service_completed",
              "filing_window_days": 10, "filing_destination": "https://claims.example/",
              "filer": "Handling lawyer" },
            { "benefit_code": "CAPPED", "effective_on": on, "trigger": "hours_exhausted",
              "filing_window_days": 20, "filing_destination": "https://claims.example/",
              "filer": "Handling lawyer" }
        ]
    }))
    .unwrap();
    crate::legal_plans::configure(&pg, firm, &config, false)
        .await
        .unwrap();
    let plan = crate::legal_plans::find_by_name(&pg, firm, &config.name)
        .await
        .unwrap()
        .unwrap();
    sqlx::query(
        "UPDATE project SET legal_plan_id=$1, plan_case_number='synthetic-case' WHERE id=$2",
    )
    .bind(plan.id)
    .bind(project.id)
    .execute(pg.pool())
    .await
    .unwrap();
    let record = |claim_id, event_id, event| {
        plan_claims::record(
            &pg,
            project.id,
            firm,
            fixture.lawyer,
            chrono_tz::America::Los_Angeles,
            Change {
                claim_id,
                event_id,
                occurred_on: on,
                event,
            },
        )
    };
    let fixed_claim = Uuid::new_v4();
    let capped_claim = Uuid::new_v4();
    let fixed = Authorization::FixedFee {
        reference: "synthetic-benefit".into(),
        cents: 100,
    };
    let capped = Authorization::CappedHours {
        reference: "synthetic-authorization".into(),
        rate_cents_per_hour: 200,
        covered_minutes: 120,
        pending_minutes: 0,
    };
    for (id, benefit, authorization) in [
        (fixed_claim, "FIXED", fixed),
        (capped_claim, "CAPPED", capped),
    ] {
        let event_id = Uuid::new_v4();
        let event = Event::WorkRecorded {
            work: Work {
                benefit: benefit.into(),
                service: Service::Manual,
                performed_on: on,
                attorney_minutes: Some(60),
                time_entry_ids: vec![],
                authorization: Some(authorization),
            },
        };
        record(id, event_id, event.clone()).await.unwrap();
        record(id, event_id, event).await.unwrap();
    }
    let mut revision = config.clone();
    revision.filing_rules.truncate(1);
    revision.filing_rules[0].effective_on = on;
    revision.filing_rules[0].filing_window_days = Some(90);
    crate::legal_plans::configure(&pg, firm, &revision, false)
        .await
        .unwrap();

    let report = serde_json::to_value(
        plan_claims::worklist(&pg, firm, Some(&[project.id]))
            .await
            .unwrap(),
    )
    .unwrap();
    let totals = &report["plans"][&config.name];
    assert_eq!(totals["eligible_unsubmitted_count"], 1);
    assert_eq!(totals["accruing_count"], 1);
    let ready = totals["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["filing_status"] == "ready")
        .unwrap();
    assert_eq!(
        ready["filing_due_on"],
        on.checked_add_days(chrono::Days::new(10))
            .unwrap()
            .to_string()
    );
    assert_eq!(
        report["filing_review"][&config.name]["eligible_unsubmitted_count"],
        1
    );
    let private =
        serde_json::to_value(plan_claims::worklist(&pg, firm, None).await.unwrap()).unwrap();
    assert_eq!(
        private["plans"][&config.name]["items"],
        serde_json::json!([])
    );
    assert!(record(capped_claim, Uuid::new_v4(), Event::Prepared)
        .await
        .is_err());
    record(fixed_claim, Uuid::new_v4(), Event::Prepared)
        .await
        .unwrap();
    let deadlines =
        async || {
            sqlx::query_as::<_, (String, serde_json::Value, Option<i64>)>(
        "SELECT status, due_on, client_lead_days FROM project_deadlines WHERE project_id=$1",
    ).bind(project.id).fetch_all(pg.pool()).await.unwrap()
        };
    let saved = deadlines().await;
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].0, "open");
    assert_eq!(
        saved[0].1["date"],
        on.checked_add_days(chrono::Days::new(10))
            .unwrap()
            .to_string()
    );
    assert_eq!(saved[0].1["timezone"], "America/Los_Angeles");
    assert_eq!(saved[0].2, None);
    record(
        fixed_claim,
        Uuid::new_v4(),
        Event::Submission {
            provider_reference: Some("synthetic-filing".into()),
            sent_message_id: None,
            claimed_cents: Some(100),
        },
    )
    .await
    .unwrap();
    let completed_deadlines = deadlines().await;
    assert_eq!(completed_deadlines.len(), 1);
    assert_eq!(completed_deadlines[0].0, "complete");
    assert_eq!(completed_deadlines[0].1, saved[0].1);
}
