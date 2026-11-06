//! The contingent-fee doors: a matter's dated Contingency Terms, its Outcome
//! Estimates, and its Matter Outcome. Firm work
//! product — a firm-side lawyer records and reads it, and everyone else gets
//! the same refusals the stage doors give.

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use super::{body_json, build_fixture, get, lane, post, seed_definitions, stage_store, Fixture};

/// A Notation on `project_id`: the signed fee agreement a term references.
async fn fee_agreement(fx: &Fixture, project_id: Uuid) -> Uuid {
    let template = store::test_support::seed_unpinned_project_template(
        &fx.surreal,
        project_id,
        &format!("fee_agreement_{}", Uuid::now_v7().simple()),
        "Contingent Fee Agreement",
        "person",
        None,
    )
    .await;
    let signer = store::persons::create(
        &fx.surreal,
        &store::persons::NewPerson::with_role(
            "Fee Agreement Client",
            format!("fee-client-{}@example.com", Uuid::now_v7().simple()),
            store::persons::Role::Client,
        ),
    )
    .await
    .unwrap();
    store::notations::create(
        &fx.surreal,
        &store::notations::NewNotation::new(template.id, signer.id, project_id, "executed"),
    )
    .await
    .unwrap()
    .id
}

fn terms_path(project_id: Uuid) -> String {
    format!("/app/api/projects/{project_id}/contingency-terms")
}

fn estimates_path(project_id: Uuid) -> String {
    format!("/app/api/projects/{project_id}/outcome-estimates")
}

fn estimate(settle_bps: i64, lose_bps: i64) -> serde_json::Value {
    json!({
        "expected_resolution_on": "2027-06-30",
        "note": "After the deposition",
        "outcomes": {
            "win_at_trial": {"probability_bps": 1500, "gross_recovery_cents": 40_000_000},
            "settle": {"probability_bps": settle_bps, "gross_recovery_cents": 15_000_000},
            "lose": {"probability_bps": lose_bps, "gross_recovery_cents": 0},
        },
    })
}

fn term(intake: Uuid, discovery: Uuid, notation: Uuid) -> serde_json::Value {
    json!({
        "effective_on": "2026-11-01",
        "tiers": [
            {"stage_definition_id": intake, "fee_bps": 3330},
            {"stage_definition_id": discovery, "fee_bps": 4000},
        ],
        "costs_deducted": "after_fee",
        "fee_agreement_notation_id": notation,
    })
}

#[test]
fn a_firm_side_lawyer_records_and_reads_a_contingency_term() {
    lane().block_on(async {
        let Some(_) = stage_store().await else {
            return;
        };
        let fx = build_fixture().await;
        let (intake, discovery) = seed_definitions(fx.firm_id).await;
        let notation = fee_agreement(&fx, fx.project_id).await;
        let path = terms_path(fx.project_id);

        let body = body_json(get(&fx, &path, Some(&fx.lawyer)).await).await;
        assert_eq!(body["terms"], json!([]), "no term yet");

        let resp = post(
            &fx,
            &path,
            Some(&fx.lawyer),
            term(intake, discovery, notation),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let recorded = body_json(resp).await;
        assert_eq!(recorded["tiers"][1]["fee_bps"], 4000);
        assert_eq!(recorded["costs_deducted"], "after_fee");

        let body = body_json(get(&fx, &path, Some(&fx.lawyer)).await).await;
        assert_eq!(body["terms"], json!([recorded]));

        // A fee agreement on another matter is refused, and so are tiers
        // out of the Firm's stage order.
        let elsewhere = fee_agreement(&fx, fx.other_project_id).await;
        let resp = post(
            &fx,
            &path,
            Some(&fx.lawyer),
            term(intake, discovery, elsewhere),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let resp = post(
            &fx,
            &path,
            Some(&fx.lawyer),
            term(discovery, intake, notation),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    });
}

#[test]
fn only_a_firm_side_lawyer_reaches_the_contingency_doors() {
    lane().block_on(async {
        let Some(_) = stage_store().await else {
            return;
        };
        let fx = build_fixture().await;
        let (intake, discovery) = seed_definitions(fx.firm_id).await;
        let notation = fee_agreement(&fx, fx.project_id).await;
        let path = terms_path(fx.project_id);
        let body = term(intake, discovery, notation);
        let estimates = estimates_path(fx.project_id);
        assert_eq!(
            post(&fx, &estimates, Some(&fx.outsider), estimate(6000, 2500))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        for actor in [&fx.client, &fx.clerk] {
            assert_eq!(
                get(&fx, &estimates, Some(actor)).await.status(),
                StatusCode::FORBIDDEN
            );
            assert_eq!(
                post(&fx, &estimates, Some(actor), estimate(6000, 2500))
                    .await
                    .status(),
                StatusCode::FORBIDDEN
            );
        }

        // Off the matter: the matter does not exist for them.
        assert_eq!(
            get(&fx, &path, Some(&fx.outsider)).await.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            post(&fx, &path, Some(&fx.outsider), body.clone())
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        // The client and a clerk are not lawyer tier.
        for actor in [&fx.client, &fx.clerk] {
            assert_eq!(
                get(&fx, &path, Some(actor)).await.status(),
                StatusCode::FORBIDDEN
            );
            assert_eq!(
                post(&fx, &path, Some(actor), body.clone()).await.status(),
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(
            get(&fx, &path, None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        // Nothing was written by any refused caller.
        let listed = body_json(get(&fx, &path, Some(&fx.lawyer)).await).await;
        assert_eq!(listed["terms"], json!([]));
    });
}

#[test]
fn the_newest_outcome_estimate_is_current_and_the_sum_is_checked() {
    lane().block_on(async {
        let Some(_) = stage_store().await else {
            return;
        };
        let fx = build_fixture().await;
        let path = estimates_path(fx.project_id);

        let resp = post(&fx, &path, Some(&fx.lawyer), estimate(6000, 2400)).await;
        assert_eq!(
            resp.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "odds that do not sum to 10000 are refused"
        );
        let first = post(&fx, &path, Some(&fx.lawyer), estimate(6000, 2500)).await;
        assert_eq!(first.status(), StatusCode::CREATED);
        let first = body_json(first).await;
        let second =
            body_json(post(&fx, &path, Some(&fx.lawyer), estimate(5000, 3500)).await).await;
        let listed = body_json(get(&fx, &path, Some(&fx.lawyer)).await).await;
        assert_eq!(listed["estimates"], json!([first, second]));
        assert_eq!(
            listed["estimates"][1]["outcomes"]["settle"]["probability_bps"],
            5000
        );
    });
}

#[test]
fn a_matter_has_one_outcome_and_a_correction_supersedes_it() {
    lane().block_on(async {
        let Some(_) = stage_store().await else {
            return;
        };
        let fx = build_fixture().await;
        let path = format!("/app/api/projects/{}/matter-outcome", fx.project_id);
        let outcome = json!({
            "result": "settled",
            "resolved_on": "2027-03-01",
            "gross_recovery_cents": 15_000_000,
            "fee_received_cents": 4_995_000,
            "costs_recovered_cents": 1_200_000,
        });
        for actor in [&fx.client, &fx.clerk] {
            assert_eq!(
                post(&fx, &path, Some(actor), json!({"outcome": outcome}))
                    .await
                    .status(),
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(
            post(&fx, &path, Some(&fx.outsider), json!({"outcome": outcome}))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );

        let original = post(&fx, &path, Some(&fx.lawyer), json!({"outcome": outcome})).await;
        assert_eq!(original.status(), StatusCode::CREATED);
        let original = body_json(original).await;
        assert_eq!(
            post(&fx, &path, Some(&fx.lawyer), json!({"outcome": outcome}))
                .await
                .status(),
            StatusCode::CONFLICT,
            "one outcome per matter"
        );
        let correction = body_json(
            post(
                &fx,
                &path,
                Some(&fx.lawyer),
                json!({"outcome": outcome, "correction_reason": "Wire fee netted out"}),
            )
            .await,
        )
        .await;
        assert_eq!(correction["supersedes_id"], original["id"]);
        let listed = body_json(get(&fx, &path, Some(&fx.lawyer)).await).await;
        assert_eq!(listed["outcomes"], json!([original, correction]));
    });
}
