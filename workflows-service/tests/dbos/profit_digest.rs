//! `ProfitDigest` on DBOS: one post per UTC date, the change against the
//! previous day's journaled totals, and silence on a day with nothing to say.

use std::sync::{Arc, Mutex};

use chrono::{NaiveDate, TimeZone, Utc};
use store::outcome_calibration::OutcomeCalibration;
use store::projected_cash::{CashGroup, FirmProjectedCash};
use uuid::Uuid;
use workflows::dbos_engine::profit_digest::{
    self, Context, FirmProfit, ProfitDigestError, ProfitDigestReport, ProfitSource,
};
use workflows::{dbos_engine, CapturingSlackBot};

use crate::support::{database, APP_VERSION, DEADLINE};

/// Expected cash by day; a day absent from the map has no contingent matter.
#[derive(Clone, Default)]
struct DailyCash {
    firm: Uuid,
    by_day: Arc<Mutex<Vec<(NaiveDate, i64)>>>,
}

#[async_trait::async_trait]
impl ProfitSource for DailyCash {
    async fn read(&self, on: NaiveDate) -> Result<Vec<FirmProfit>, ProfitDigestError> {
        let cash = self
            .by_day
            .lock()
            .unwrap()
            .iter()
            .find(|(day, _)| *day == on)
            .map(|(_, c)| *c);
        Ok(cash
            .map(|cents| FirmProfit {
                firm_id: self.firm,
                firm_name: "Neon Law".to_owned(),
                cash: FirmProjectedCash {
                    expected_cash_cents: cents,
                    by_brand: vec![CashGroup {
                        label: "Neon Law".to_owned(),
                        expected_cash_cents: cents,
                        matters: 1,
                    }],
                    ..FirmProjectedCash::default()
                },
                calibration: OutcomeCalibration {
                    lookback_days: 30,
                    minimum_matters: 10,
                    scored_matters: 0,
                    unscored_matters: 0,
                    report: None,
                },
            })
            .into_iter()
            .collect())
    }
}

fn slot(day: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, day, 14, 0, 0).unwrap()
}

fn day(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, day).unwrap()
}

async fn report_of(dbos: &dbos::DBOS, id: &str) -> ProfitDigestReport {
    tokio::time::timeout(
        DEADLINE,
        dbos.retrieve_workflow::<ProfitDigestReport, ProfitDigestError>(id)
            .expect("retrieve ProfitDigest")
            .result(),
    )
    .await
    .expect("ProfitDigest did not finish")
    .expect("ProfitDigest succeeds")
}

#[tokio::test]
async fn one_post_a_day_reports_the_change_and_an_empty_day_posts_nothing() {
    let Some(db) = database().await else {
        return;
    };
    let source = DailyCash {
        firm: Uuid::new_v4(),
        by_day: Arc::new(Mutex::new(vec![(day(6), 100_000), (day(7), 150_000)])),
    };
    let bot = CapturingSlackBot::new();
    let executor = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = profit_digest::register(
        &executor,
        Context {
            source: Arc::new(source),
            slack: Arc::new(bot.clone()),
            channel_id: "C-MESSAGES".to_owned(),
            base_url: "https://navigator.example.invalid".to_owned(),
        },
    )
    .expect("register ProfitDigest");
    executor.launch().await.expect("launch");

    for date in [6, 6, 7, 8] {
        profit_digest::fire(&workflow, slot(date))
            .await
            .expect("start");
        report_of(&executor, &profit_digest::workflow_id(slot(date))).await;
    }
    let reports = [
        report_of(&executor, &profit_digest::workflow_id(slot(7))).await,
        report_of(&executor, &profit_digest::workflow_id(slot(8))).await,
    ];
    executor.shutdown().await;

    assert_eq!(
        reports,
        [
            ProfitDigestReport { firms: 1 },
            ProfitDigestReport { firms: 0 }
        ]
    );
    let posted = bot.posted_messages();
    assert_eq!(
        posted.len(),
        2,
        "one post per date with contingent matters: {posted:?}"
    );
    assert!(posted.iter().all(|(channel, _)| channel == "C-MESSAGES"));
    assert!(
        posted[0]
            .1
            .contains("$1,000.00 (range $0.00 to $0.00), no earlier digest"),
        "{}",
        posted[0].1
    );
    assert!(
        posted[1].1.contains("+$500.00 since yesterday"),
        "{}",
        posted[1].1
    );
}
