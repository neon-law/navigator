//! The billed-call rollup against PostgreSQL, the engine that holds the
//! table. These tests arm themselves only where a PostgreSQL test lane
//! exists; the pure rollup and validation tests live in the module.
use chrono::TimeZone;
use store::billed_calls::{self, BilledCallError, NewBilledCall};
use store::postgres::PostgresDb;
use uuid::Uuid;

async fn postgres() -> Option<PostgresDb> {
    let port = if let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") {
        port.parse::<u16>().unwrap()
    } else {
        assert!(
            std::env::var("NAV_REQUIRE_POSTGRES").as_deref() != Ok("1"),
            "PostgreSQL integration lane requires NAVIGATOR_TEST_POSTGRES_PORT"
        );
        return None;
    };
    let options = sqlx::postgres::PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        )
        .ssl_mode(sqlx::postgres::PgSslMode::Disable);
    let admin = sqlx::PgPool::connect_with(options.clone()).await.unwrap();
    let database = format!("billing_test_{}", Uuid::now_v7().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    let pg = PostgresDb::connect(options.database(&database))
        .await
        .unwrap();
    pg.migrate_embedded(None).await.unwrap();
    Some(pg)
}

fn completed_call(sid: &str, duration_seconds: i64) -> NewBilledCall {
    NewBilledCall {
        call_sid: sid.into(),
        conversation_id: Uuid::now_v7(),
        firm_id: Uuid::now_v7(),
        duration_seconds,
        call_started_at: chrono::Utc
            .with_ymd_and_hms(2026, 10, 30, 18, 0, 0)
            .unwrap(),
    }
}

/// A Twilio redelivery of the same call: the identity fields are the ones
/// the portal passes after its `CallSid` dedup returns the original
/// conversation, and only the reported duration can differ.
fn redelivery_of(first: &NewBilledCall, duration_seconds: i64) -> NewBilledCall {
    NewBilledCall {
        call_sid: first.call_sid.clone(),
        conversation_id: first.conversation_id,
        firm_id: first.firm_id,
        duration_seconds,
        call_started_at: first.call_started_at,
    }
}

/// One row per `CallSid`: an identical redelivery converges on the original
/// row, and the same `CallSid` with different call facts is a loud conflict —
/// the original tenths stay on the record, never a silent overwrite.
#[tokio::test]
async fn a_repeated_completed_event_converges_and_a_mismatch_conflicts() {
    let Some(pg) = postgres().await else {
        return;
    };
    let first_input = completed_call("CAredelivery-42", 42);
    let first = billed_calls::record(&pg, &first_input).await.unwrap();
    assert_eq!(first.tenths, 1, "42s bills one tenth");
    assert_eq!(first.project_id, None, "a fresh row parks at the Firm");

    let again = billed_calls::record(&pg, &redelivery_of(&first_input, 42))
        .await
        .unwrap();
    assert_eq!(
        first, again,
        "an identical redelivery returns the original row"
    );

    let mismatched = billed_calls::record(&pg, &redelivery_of(&first_input, 60))
        .await
        .unwrap_err();
    assert!(
        matches!(mismatched, BilledCallError::Conflict),
        "a different duration on the same CallSid must conflict loudly, got {mismatched:?}"
    );
    let stored = billed_calls::find_by_call_sid(&pg, "CAredelivery-42")
        .await
        .unwrap()
        .expect("the original row survives the conflict");
    assert_eq!(
        stored, first,
        "the failed overwrite leaves the original tenths intact"
    );
}

/// Attribution is the only post-landing mutation: it links the row to a
/// Project and never rewrites the call's facts. A redelivery after
/// attribution is a no-op that returns the attributed row.
#[tokio::test]
async fn attribution_moves_the_project_never_the_call_facts() {
    let Some(pg) = postgres().await else {
        return;
    };
    let parked_input = completed_call("CAattribution", 361);
    let parked = billed_calls::record(&pg, &parked_input).await.unwrap();
    assert_eq!(parked.tenths, 2, "6:01 bills two tenths");
    assert_eq!(parked.project_id, None, "the call parks at the Firm level");

    let project = Uuid::now_v7();
    let attributed = billed_calls::attribute(&pg, "CAattribution", project)
        .await
        .unwrap();
    assert_eq!(attributed.project_id, Some(project));
    assert_eq!(
        attributed.duration_seconds, parked.duration_seconds,
        "attribution never touches the duration"
    );
    assert_eq!(
        attributed.tenths, parked.tenths,
        "attribution never touches the tenths"
    );
    assert_eq!(
        attributed.call_started_at, parked.call_started_at,
        "attribution never touches the provider time"
    );

    let redelivered = billed_calls::record(&pg, &redelivery_of(&parked_input, 361))
        .await
        .unwrap();
    assert_eq!(
        redelivered, attributed,
        "a redelivery after attribution returns the attributed row"
    );

    assert!(
        matches!(
            billed_calls::attribute(&pg, "CAunknown", Uuid::now_v7())
                .await
                .unwrap_err(),
            BilledCallError::NotFound
        ),
        "attributing a call that never landed is a NotFound"
    );
}

/// A zero-duration completed call is recorded with zero tenths — the record
/// shows the client reached out, and nothing is billed for it.
#[tokio::test]
async fn a_zero_duration_completed_call_is_recorded_with_zero_tenths() {
    let Some(pg) = postgres().await else {
        return;
    };
    let first_input = completed_call("CAzero", 0);
    let recorded = billed_calls::record(&pg, &first_input).await.unwrap();
    assert_eq!(recorded.duration_seconds, 0);
    assert_eq!(recorded.tenths, 0, "a zero-duration call bills zero");

    let redelivered = billed_calls::record(&pg, &redelivery_of(&first_input, 0))
        .await
        .unwrap();
    assert_eq!(recorded, redelivered, "the zero row is idempotent too");
}
