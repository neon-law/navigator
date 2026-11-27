use super::postgres;
use super::*;
use crate::assistant_turns::postgres_tests::person;
use crate::postgres::fixtures::{connect, fresh_schema};

async fn database() -> Option<PostgresDb> {
    let (options, database) = fresh_schema().await?;
    Some(connect(&options, &database).await)
}

const PERIOD: &str = "2026-11";

#[tokio::test]
async fn grant_converges_and_the_crossing_debit_completes_then_exhausts() {
    let Some(pg) = database().await else { return };
    let subscriber = person(&pg).await;
    assert_eq!(postgres::find(&pg, subscriber, PERIOD).await.unwrap(), None);

    let granted = postgres::grant(&pg, subscriber, PERIOD).await.unwrap();
    assert_eq!(
        (granted.granted_cents, granted.spent_cents),
        (MONTHLY_GRANT_CENTS, 0)
    );
    assert_eq!(
        postgres::debit(&pg, subscriber, PERIOD, 500).await.unwrap(),
        MeterDecision::Charge {
            spent_cents: 500,
            remaining_cents: 0
        },
        "spending to exactly zero is not crossing"
    );
    let regranted = postgres::grant(&pg, subscriber, PERIOD).await.unwrap();
    assert_eq!(regranted.spent_cents, 500, "a re-grant never resets spend");
    assert_eq!(
        postgres::debit(&pg, subscriber, PERIOD, 40).await.unwrap(),
        MeterDecision::Charge {
            spent_cents: 540,
            remaining_cents: -40
        }
    );
    assert_eq!(
        postgres::debit(&pg, subscriber, PERIOD, 1).await.unwrap(),
        MeterDecision::Exhausted
    );
    let owing = postgres::overage_rows(&pg, PERIOD).await.unwrap();
    assert_eq!(owing.len(), 1);
    assert_eq!(
        (owing[0].person_id, owing[0].spent_cents),
        (subscriber, 540)
    );
    assert!(postgres::overage_rows(&pg, "2026-12")
        .await
        .unwrap()
        .is_empty());
    pg.0.close().await;
}

/// Parallel first-use debits share one row: each applies exactly once, the
/// crossing one completes, and every later one is refused.
#[tokio::test]
async fn parallel_debits_apply_exactly_once() {
    let Some(pg) = database().await else { return };
    let subscriber = person(&pg).await;
    let debits = (0..10).map(|_| {
        let pg = pg.clone();
        tokio::spawn(async move { postgres::debit(&pg, subscriber, PERIOD, 100).await })
    });
    let mut charged = 0;
    for debit in debits {
        if matches!(debit.await.unwrap().unwrap(), MeterDecision::Charge { .. }) {
            charged += 1;
        }
    }
    assert_eq!(charged, 6, "five fit the grant and the sixth crosses it");
    let row = postgres::find(&pg, subscriber, PERIOD)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((row.spent_cents, row.overage), (600, true));
    pg.0.close().await;
}
