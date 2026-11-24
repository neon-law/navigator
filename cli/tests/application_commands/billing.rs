//! `navigator billing` renders the deployment's report as a worklist.

use serde_json::json;
use uuid::Uuid;

use crate::site::Site;

#[tokio::test]
#[allow(clippy::too_many_lines)] // one table of verbs; splitting it scatters the cases
async fn billing_prints_every_section_of_the_lawyers_worklist() {
    let site = Site::start().await;
    let (claim, paid, entry, timekeeper, agreement, settled) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    let scenario =
        |outcome: &str, likelihood: i32, gross: Option<i64>, bps: i32, fee: Option<i64>| {
            json!({
                "outcome": outcome, "probability_bps": likelihood, "gross_recovery_cents": gross,
                "fee_bps": bps, "fee_cents": fee,
            })
        };
    let report = json!({
        "needs_my_action": [
            {
                "project": "sample-estate", "id": claim, "benefit": "Will",
                "performed_on": "2099-01-02", "status": "ready", "next_action": "File the claim",
                "projected_fee_cents": 12500, "filed_cents": null,
                "approved_outstanding_cents": null, "net_cash_received_cents": 0,
            },
            { "project": "sample-transactional", "next_action": "Allocate the work" },
        ],
        "my_time": [{
            "project": "sample-estate", "id": entry, "performed_on": "2099-01-02",
            "timekeeper": timekeeper, "minutes": 30, "description": "Drafted the letter",
            "status": "staged",
        }],
        "verified_projected_fees": [
            { "project": "sample-estate", "basis": "flat fee", "projected_fee_cents": 12500 },
        ],
        "unverified_fees": [{ "project": "sample-litigation", "basis": "hourly" }],
        "contingency": [
            {
                "project": "sample-litigation", "fee_agreement_notation_id": agreement,
                "effective_on": "2099-01-01", "costs_deducted": "after_fee",
                "recovery_basis": "estimate", "tiers": [{ "stage": "Trial", "fee_bps": 4000 }],
                "hybrid_rate_cents_per_hour": 25000, "estimate_recorded_on": "2099-01-01",
                "stale_estimate": true,
                "outcomes": [
                    scenario("win_at_trial", 2500, Some(10_000_000), 4000, Some(4_000_000)),
                    scenario("settle", 5000, Some(5_000_000), 3333, Some(1_666_500)),
                    scenario("lose", 2500, None, 0, Some(0)),
                    scenario("remand", 0, None, 0, None),
                ],
                "projected_fee_cents": 1_833_250, "expected_cash_cents": 1_900_000,
                "fee_received_cents": null, "costs_recovered_cents": null,
            },
            {
                "project": "sample-estate", "fee_agreement_notation_id": settled,
                "effective_on": "2099-02-01", "costs_deducted": "by agreement",
                "recovery_basis": "recorded", "tiers": [],
                "hybrid_rate_cents_per_hour": null, "estimate_recorded_on": null,
                "stale_estimate": false,
                "outcomes": [
                    scenario("settle", 10000, Some(900_000), 3333, Some(300_000)),
                    scenario("lose", 0, None, 0, None),
                ],
                "projected_fee_cents": null, "expected_cash_cents": null,
                "fee_received_cents": 300_000, "costs_recovered_cents": 12000,
            },
        ],
        "other_claims": [{
            "project": "sample-transactional", "id": paid, "benefit": "Contract review",
            "performed_on": "2099-01-03", "status": "paid", "next_action": null,
            "projected_fee_cents": null, "filed_cents": 9000,
            "approved_outstanding_cents": 9000, "net_cash_received_cents": -250,
        }],
    });
    site.answer("GET", "/app/api/billing", 200, report).await;

    let ran = site.navigator(&["billing"]).await;

    let expected = format!(
        "Needs my action
  sample-estate · Will · {claim} · File the claim
    2099-01-02 · ready · Projected fee $125.00 · Filed unknown · Approved outstanding unknown · Net cash received $0.00
  sample-transactional · Allocate the work

My time
  sample-estate · 2099-01-02 · 30 min · staged · {entry} · Drafted the letter · timekeeper {timekeeper}

Verified projected fees for completed work
  Total remaining $125.00
  sample-estate · $125.00 · flat fee

Fees awaiting verification
  sample-litigation · hourly

Contingency fees
  sample-litigation · agreement {agreement} effective 2099-01-01 · costs deducted after calculating the fee · recovery basis estimate
    From Trial · fee 40.00%
    Hybrid hourly rate $250.00
    Outcome Estimate 2099-01-01 · newer Case Stage
    Win at trial · 25.00% likelihood · gross recovery $100000.00 · fee 40.00% ($40000.00)
    Settlement · 50.00% likelihood · gross recovery $50000.00 · fee 33.33% ($16665.00)
    Loss · 25.00% likelihood · gross recovery unknown · fee 0.00% ($0.00)
    remand · 0.00% likelihood · gross recovery unknown · fee 0.00% (unknown)
    Projected contingency fee $18332.50 · Expected cash including cost recovery $19000.00
  sample-estate · agreement {settled} effective 2099-02-01 · costs deducted by agreement · recovery basis recorded
    Recorded outcome · Settlement · gross recovery $9000.00
    Fee received $3000.00 · Costs recovered $120.00

Other Legal Plan Claims
  sample-transactional · Contract review · {paid}
    2099-01-03 · paid · Projected fee unknown · Filed $90.00 · Approved outstanding $90.00 · Net cash received -$2.50
"
    );
    assert_eq!(ran.succeeded(), expected);
}

#[tokio::test]
async fn billing_names_an_empty_worklist_and_a_refused_report() {
    let site = Site::start().await;
    site.navigator(&["billing"])
        .await
        .refused(1, "billing report refused (404 Not Found)");

    let empty = json!({
        "needs_my_action": [], "my_time": [], "verified_projected_fees": [],
        "unverified_fees": [], "contingency": [], "other_claims": [],
    });
    site.answer("GET", "/app/api/billing", 200, empty.clone())
        .await;
    let ran = site.navigator(&["billing"]).await;
    assert_eq!(
        ran.succeeded(),
        "Needs my action
  None

My time
  No Time Entries or pending Billing Annotations

Verified projected fees for completed work
  None
"
    );
    let ran = site.navigator(&["billing", "--json"]).await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(ran.succeeded()).unwrap(),
        empty
    );
}
