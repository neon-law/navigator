//! The sandbox usage ledger's money rules.
//!
//! The ledger is the only thing standing between a Nautilus subscriber's
//! $5 of model usage and a meter that lies. These tests pin the four
//! behaviors the meter must never get wrong: grant/debit arithmetic in
//! integer cents, the crossing completion (billed, flagged overage),
//! no double-spend under parallel debits, and a rollover that grants a
//! fresh period without rewriting the closed one.

use store::sandbox_usage::{self, MeterDecision, SandboxUsageError, MONTHLY_GRANT_CENTS};
use store::surreal::test_support::mem;
use uuid::Uuid;

/// A subscriber nobody else's test knows — the ledger keys rows on the
/// compound `person:period` id, so distinct persons never collide.
fn subscriber() -> Uuid {
    Uuid::now_v7()
}

/// `Charge`, unpacked — the assertions all want the fields.
fn charge(decision: MeterDecision) -> (i64, i64) {
    match decision {
        MeterDecision::Charge {
            spent_cents,
            remaining_cents,
        } => (spent_cents, remaining_cents),
        MeterDecision::Exhausted => panic!("the ledger refused a charge that should have applied"),
    }
}

#[tokio::test]
async fn grant_makes_one_row_with_the_monthly_credit() {
    let db = mem().await;
    let who = subscriber();

    let row = sandbox_usage::grant(&db, who, "2026-11").await.unwrap();

    assert_eq!(row.person_id, who);
    assert_eq!(row.period, "2026-11");
    assert_eq!(row.granted_cents, MONTHLY_GRANT_CENTS);
    assert_eq!(row.granted_cents, 500, "the glossary's $5.00, in cents");
    assert_eq!(row.spent_cents, 0);
    assert!(!row.overage);
    assert_eq!(row.remaining_cents(), 500);
}

#[tokio::test]
async fn grant_is_idempotent_and_never_resets_a_spent_row() {
    let db = mem().await;
    let who = subscriber();
    sandbox_usage::grant(&db, who, "2026-11").await.unwrap();
    charge(
        sandbox_usage::debit(&db, who, "2026-11", 125)
            .await
            .unwrap(),
    );

    let again = sandbox_usage::grant(&db, who, "2026-11").await.unwrap();

    assert_eq!(
        again.spent_cents, 125,
        "a re-grant must converge, not re-grant"
    );
    assert!(!again.overage);
}

#[tokio::test]
async fn debit_125_of_500_leaves_375() {
    let db = mem().await;
    let who = subscriber();
    sandbox_usage::grant(&db, who, "2026-11").await.unwrap();

    let (spent, remaining) = charge(
        sandbox_usage::debit(&db, who, "2026-11", 125)
            .await
            .unwrap(),
    );

    assert_eq!(spent, 125);
    assert_eq!(remaining, 375);

    let row = sandbox_usage::find(&db, who, "2026-11")
        .await
        .unwrap()
        .expect("the granted row");
    assert_eq!(row.spent_cents, 125);
    assert!(!row.overage);
}

#[tokio::test]
async fn the_crossing_debit_of_501_completes_and_flags_overage() {
    let db = mem().await;
    let who = subscriber();
    sandbox_usage::grant(&db, who, "2026-11").await.unwrap();

    let (spent, remaining) = charge(
        sandbox_usage::debit(&db, who, "2026-11", 501)
            .await
            .unwrap(),
    );

    assert_eq!(spent, 501, "the crossing completion is billed in full");
    assert_eq!(remaining, -1);

    let row = sandbox_usage::find(&db, who, "2026-11")
        .await
        .unwrap()
        .expect("the granted row");
    assert!(row.overage, "the balance went negative: overage is owed");
}

#[tokio::test]
async fn the_debit_after_crossing_is_exhausted() {
    let db = mem().await;
    let who = subscriber();
    sandbox_usage::grant(&db, who, "2026-11").await.unwrap();
    charge(
        sandbox_usage::debit(&db, who, "2026-11", 501)
            .await
            .unwrap(),
    );

    let decision = sandbox_usage::debit(&db, who, "2026-11", 1).await.unwrap();

    assert_eq!(decision, MeterDecision::Exhausted);
    let row = sandbox_usage::find(&db, who, "2026-11")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.spent_cents, 501, "a refused debit spends nothing");
}

#[tokio::test]
async fn spending_to_exactly_zero_is_not_crossing() {
    let db = mem().await;
    let who = subscriber();
    sandbox_usage::grant(&db, who, "2026-11").await.unwrap();

    let (spent, remaining) = charge(
        sandbox_usage::debit(&db, who, "2026-11", 500)
            .await
            .unwrap(),
    );
    assert_eq!((spent, remaining), (500, 0));
    let row = sandbox_usage::find(&db, who, "2026-11")
        .await
        .unwrap()
        .unwrap();
    assert!(!row.overage, "the balance never went negative");

    // The next completion is the one that crosses: billed, flagged.
    let (spent, remaining) = charge(sandbox_usage::debit(&db, who, "2026-11", 1).await.unwrap());
    assert_eq!((spent, remaining), (501, -1));

    // And only then does the meter close.
    assert_eq!(
        sandbox_usage::debit(&db, who, "2026-11", 1).await.unwrap(),
        MeterDecision::Exhausted
    );
}

#[tokio::test]
async fn the_overage_flag_counts_the_price_once() {
    let db = mem().await;
    let who = subscriber();
    sandbox_usage::grant(&db, who, "2026-11").await.unwrap();

    // Pins the conditional debit's field semantics: `overage` must
    // compare the once-applied new balance. An engine that re-evaluated
    // the price against the already-updated `spent_cents` would flag
    // overage here (600 > 500) while the subscriber is still in credit.
    charge(
        sandbox_usage::debit(&db, who, "2026-11", 300)
            .await
            .unwrap(),
    );
    let row = sandbox_usage::find(&db, who, "2026-11")
        .await
        .unwrap()
        .unwrap();
    assert!(!row.overage, "300 of 500 is inside credit");

    charge(
        sandbox_usage::debit(&db, who, "2026-11", 250)
            .await
            .unwrap(),
    );
    let row = sandbox_usage::find(&db, who, "2026-11")
        .await
        .unwrap()
        .unwrap();
    assert!(row.overage, "550 of 500 crossed");
    assert_eq!(row.spent_cents, 550, "the price must apply exactly once");
}

#[tokio::test]
async fn debit_on_first_touch_self_grants() {
    let db = mem().await;
    let who = subscriber();

    // No grant call: a subscriber's first completion of a month gets the
    // month's credit even if the rollover job has not run yet.
    let (spent, remaining) = charge(
        sandbox_usage::debit(&db, who, "2026-11", 125)
            .await
            .unwrap(),
    );
    assert_eq!((spent, remaining), (125, 375));

    let row = sandbox_usage::find(&db, who, "2026-11")
        .await
        .unwrap()
        .expect("first touch must materialize the period's row");
    assert_eq!(row.granted_cents, 500);
}

#[tokio::test]
async fn two_parallel_tabs_never_double_spend() {
    let db = mem().await;
    let who = subscriber();
    sandbox_usage::grant(&db, who, "2026-11").await.unwrap();

    // Eight tabs each charging 100 against a 500 grant: five charges fit,
    // the sixth is the crossing completion (billed, flagged), and the
    // last two are refused. Every serialization order lands here, so the
    // total is deterministic.
    let racers: Vec<_> = (0..8)
        .map(|_| {
            let db = db.clone();
            tokio::spawn(async move { sandbox_usage::debit(&db, who, "2026-11", 100).await })
        })
        .collect();

    let mut charged = 0;
    let mut refused = 0;
    for racer in racers {
        match racer.await.expect("racer task").unwrap() {
            MeterDecision::Charge { .. } => charged += 1,
            MeterDecision::Exhausted => refused += 1,
        }
    }

    assert_eq!(charged, 6, "five inside credit plus one crossing");
    assert_eq!(refused, 2, "a past-credit row refuses further completions");

    let row = sandbox_usage::find(&db, who, "2026-11")
        .await
        .unwrap()
        .expect("the granted row");
    assert_eq!(row.spent_cents, 600, "each debit applied exactly once");
    assert!(row.overage);
    assert_eq!(row.remaining_cents(), -100);
}

#[tokio::test(flavor = "multi_thread")]
async fn parallel_first_touch_debits_settle_on_one_row() {
    let db = mem().await;
    let who = subscriber();

    // Nobody called grant: eight racers materialize the same period's row
    // and debit it. The compound record id is the collision point — one
    // row must land. Every racer charges 125 against a 500 grant, so the
    // counts are order-independent: the first four debits stay inside
    // credit, the fifth is the crossing completion (billed, flagged), and
    // the rest are refused — whoever they are.
    let racers: Vec<_> = (0..8)
        .map(|_| {
            let db = db.clone();
            tokio::spawn(async move { sandbox_usage::debit(&db, who, "2026-11", 125).await })
        })
        .collect();

    let mut charged = 0;
    let mut refused = 0;
    for racer in racers {
        match racer.await.expect("racer task").unwrap() {
            MeterDecision::Charge { .. } => charged += 1,
            MeterDecision::Exhausted => refused += 1,
        }
    }

    assert_eq!(charged, 5, "four inside credit plus one crossing");
    assert_eq!(refused, 3, "a past-credit row refuses further completions");

    // Exactly one row exists — the race never forked the credit — and it
    // carries every applied debit exactly once.
    let periods: Vec<String> = db
        .query("SELECT VALUE period FROM sandbox_usage")
        .await
        .unwrap()
        .take(0)
        .unwrap();
    assert_eq!(
        periods,
        ["2026-11"],
        "a first-touch race must settle on one ledger row"
    );
    let spent: Vec<i64> = db
        .query("SELECT VALUE spent_cents FROM sandbox_usage")
        .await
        .unwrap()
        .take(0)
        .unwrap();
    assert_eq!(
        spent,
        [625],
        "five debits of 125, each applied exactly once"
    );
}

#[tokio::test]
async fn rollover_grants_a_fresh_period_and_closed_rows_survive() {
    let db = mem().await;
    let who = subscriber();

    // The closing month: spent into overage.
    sandbox_usage::grant(&db, who, "2026-10").await.unwrap();
    charge(
        sandbox_usage::debit(&db, who, "2026-10", 501)
            .await
            .unwrap(),
    );

    // The rollover job grants the new period's row.
    let fresh = sandbox_usage::grant(&db, who, "2026-11").await.unwrap();
    assert_eq!(fresh.granted_cents, 500);
    assert_eq!(fresh.spent_cents, 0);
    assert!(!fresh.overage);

    // The closed period's overage row is untouched — invoice-line-ready.
    let closed = sandbox_usage::find(&db, who, "2026-10")
        .await
        .unwrap()
        .expect("the closed period's row survives the rollover");
    assert!(closed.overage);
    assert_eq!(closed.spent_cents, 501);

    // The overage-invoice read finds it, and only it.
    let owing = sandbox_usage::overage_rows(&db, "2026-10").await.unwrap();
    assert_eq!(owing.len(), 1);
    assert_eq!(owing[0].person_id, who);
    assert_eq!(owing[0].spent_cents, 501);
    assert!(sandbox_usage::overage_rows(&db, "2026-11")
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn overage_rows_only_report_rows_that_owe() {
    let db = mem().await;
    let owing = subscriber();
    let inside = subscriber();
    sandbox_usage::grant(&db, owing, "2026-11").await.unwrap();
    sandbox_usage::grant(&db, inside, "2026-11").await.unwrap();
    charge(
        sandbox_usage::debit(&db, owing, "2026-11", 501)
            .await
            .unwrap(),
    );
    charge(
        sandbox_usage::debit(&db, inside, "2026-11", 100)
            .await
            .unwrap(),
    );

    let rows = sandbox_usage::overage_rows(&db, "2026-11").await.unwrap();

    assert_eq!(rows.len(), 1, "a subscriber inside credit owes nothing");
    assert_eq!(rows[0].person_id, owing);
    assert!(
        sandbox_usage::overage_rows(&db, "2026-12")
            .await
            .unwrap()
            .is_empty(),
        "another month's rows are not this read's business"
    );
}

#[test]
fn billing_period_spells_months_canonically() {
    assert_eq!(sandbox_usage::billing_period(2026, 11), "2026-11");
    assert_eq!(sandbox_usage::billing_period(2026, 3), "2026-03");
    assert_eq!(sandbox_usage::billing_period(1999, 12), "1999-12");
}

#[tokio::test]
async fn a_malformed_period_is_refused_not_forked() {
    let db = mem().await;
    let who = subscriber();

    for period in [
        "2026-13", "2026-00", "november", "2026-11 ", " 2026-11", "26-11", "2026-111",
    ] {
        assert!(
            matches!(
                sandbox_usage::grant(&db, who, period).await,
                Err(SandboxUsageError::InvalidPeriod(_))
            ),
            "grant must refuse {period:?}"
        );
        assert!(
            matches!(
                sandbox_usage::debit(&db, who, period, 100).await,
                Err(SandboxUsageError::InvalidPeriod(_))
            ),
            "debit must refuse {period:?}"
        );
        assert!(
            matches!(
                sandbox_usage::overage_rows(&db, period).await,
                Err(SandboxUsageError::InvalidPeriod(_))
            ),
            "overage_rows must refuse {period:?}"
        );
    }

    // A refusal writes nothing: the subscriber's credit cannot fork
    // across two spellings of one month.
    let periods: Vec<String> = db
        .query("SELECT VALUE period FROM sandbox_usage")
        .await
        .unwrap()
        .take(0)
        .unwrap();
    assert!(periods.is_empty(), "refused periods must write no rows");
}

#[tokio::test]
async fn a_negative_debit_is_refused() {
    let db = mem().await;
    let who = subscriber();

    assert!(matches!(
        sandbox_usage::debit(&db, who, "2026-11", -1).await,
        Err(SandboxUsageError::NegativePrice(-1))
    ));

    // And it wrote nothing.
    assert!(sandbox_usage::find(&db, who, "2026-11")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn find_reports_a_period_with_no_row_as_none() {
    let db = mem().await;
    assert!(sandbox_usage::find(&db, subscriber(), "2026-11")
        .await
        .unwrap()
        .is_none());
}
