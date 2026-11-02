//! The billed-call rollup against PostgreSQL, the engine that holds the
//! table. These tests arm themselves only where a PostgreSQL test lane
//! exists; the pure rollup and validation tests live in the module.
use chrono::TimeZone;
use store::billed_calls::{self, BilledCall, BilledCallError, NewBilledCall};
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

/// One Firm row with its own Entity, EntityType, and Jurisdiction, so a
/// Project can be seeded against it: attribution is firm-scoped, so the
/// tests need real tenant rows, not just identifiers.
async fn seed_firm(pg: &PostgresDb) -> Uuid {
    let unique = Uuid::now_v7().simple().to_string();
    let entity_type: Uuid = sqlx::query_scalar(
        "INSERT INTO entity_type (name) VALUES ('test-law-firm-' || $1::text) RETURNING id",
    )
    .bind(&unique)
    .fetch_one(pg.pool())
    .await
    .unwrap();
    let jurisdiction: Uuid = sqlx::query_scalar(
        "INSERT INTO jurisdiction (name) VALUES ('Test ' || $1::text) RETURNING id",
    )
    .bind(&unique)
    .fetch_one(pg.pool())
    .await
    .unwrap();
    let entity: Uuid = sqlx::query_scalar(
        "INSERT INTO entity (name, entity_type_id, jurisdiction_id) \
         VALUES ('Test Entity', $1, $2) RETURNING id",
    )
    .bind(entity_type)
    .bind(jurisdiction)
    .fetch_one(pg.pool())
    .await
    .unwrap();
    sqlx::query_scalar(
        "INSERT INTO firm (name, status, entity_id) VALUES ('Test Firm', 'active', $1) RETURNING id",
    )
    .bind(entity)
    .fetch_one(pg.pool())
    .await
    .unwrap()
}

/// One open Project owned by the Firm.
async fn seed_project(pg: &PostgresDb, firm_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO project (code, name, status, entity_id, firm_id) \
         VALUES ('P' || $2::text, 'Test Project', 'open', \
                 (SELECT entity_id FROM firm WHERE id = $1), $1) RETURNING id",
    )
    .bind(firm_id)
    .bind(Uuid::now_v7().simple().to_string())
    .fetch_one(pg.pool())
    .await
    .unwrap()
}

/// One parked completed call for the conversation, attributed to no Project.
async fn parked_call(pg: &PostgresDb, firm_id: Uuid, sid: &str, hour: u32) -> BilledCall {
    let mut input = completed_call(sid, 361);
    input.firm_id = firm_id;
    input.call_started_at = chrono::Utc
        .with_ymd_and_hms(2026, 10, 30, hour, 0, 0)
        .unwrap();
    billed_calls::record(pg, &input).await.unwrap()
}

/// Attribution through the conversation-keyed door links every parked call
/// of the conversation to the Project and never rewrites the call's facts.
#[tokio::test]
async fn conversation_attribution_moves_the_project_never_the_facts() {
    let Some(pg) = postgres().await else {
        return;
    };
    let firm = seed_firm(&pg).await;
    let project = seed_project(&pg, firm).await;
    let parked = parked_call(&pg, firm, "CAconversation-42", 18).await;

    billed_calls::attribute_conversation(&pg, firm, parked.conversation_id, project)
        .await
        .expect("attribution succeeds");
    let stored = billed_calls::find_by_call_sid(&pg, "CAconversation-42")
        .await
        .unwrap()
        .expect("the row survives attribution");
    assert_eq!(stored.project_id, Some(project));
    assert_eq!(
        stored.duration_seconds, parked.duration_seconds,
        "attribution never touches the duration"
    );
    assert_eq!(
        stored.tenths, parked.tenths,
        "attribution never touches the tenths"
    );
    assert_eq!(
        stored.call_started_at, parked.call_started_at,
        "attribution never touches the provider time"
    );
}

/// Re-linking a conversation to the Project it is already on converges —
/// the portal's re-assign must be idempotent, never a conflict.
#[tokio::test]
async fn conversation_attribution_is_idempotent_for_a_same_project_relink() {
    let Some(pg) = postgres().await else {
        return;
    };
    let firm = seed_firm(&pg).await;
    let project = seed_project(&pg, firm).await;
    let parked = parked_call(&pg, firm, "CAconversation-idem", 18).await;

    billed_calls::attribute_conversation(&pg, firm, parked.conversation_id, project)
        .await
        .expect("first attribution succeeds");
    billed_calls::attribute_conversation(&pg, firm, parked.conversation_id, project)
        .await
        .expect("a re-link to the same Project is idempotent, not a conflict");
    let stored = billed_calls::find_by_call_sid(&pg, "CAconversation-idem")
        .await
        .unwrap()
        .expect("the row survives the re-link");
    assert_eq!(stored.project_id, Some(project));
}

/// Re-attributing a conversation to a different Project fails loudly; the
/// original attribution stands and the billing facts never move.
#[tokio::test]
async fn conversation_re_attribution_to_a_different_project_conflicts() {
    let Some(pg) = postgres().await else {
        return;
    };
    let firm = seed_firm(&pg).await;
    let first = seed_project(&pg, firm).await;
    let second = seed_project(&pg, firm).await;
    let parked = parked_call(&pg, firm, "CAconversation-move", 18).await;

    billed_calls::attribute_conversation(&pg, firm, parked.conversation_id, first)
        .await
        .expect("the first attribution succeeds");
    let error = billed_calls::attribute_conversation(&pg, firm, parked.conversation_id, second)
        .await
        .unwrap_err();
    assert!(
        matches!(error, BilledCallError::Conflict),
        "re-attribution to a different Project must conflict loudly, got {error:?}"
    );
    let stored = billed_calls::find_by_call_sid(&pg, "CAconversation-move")
        .await
        .unwrap()
        .expect("the row survives the refused move");
    assert_eq!(
        stored.project_id,
        Some(first),
        "the refused move leaves the original attribution standing"
    );
    assert_eq!(stored.duration_seconds, parked.duration_seconds);
    assert_eq!(stored.tenths, parked.tenths);
}

/// A Project belonging to another Firm is refused — billing cannot move
/// across tenants. An unknown Project is refused the same way, without
/// confirming that it exists.
#[tokio::test]
async fn conversation_attribution_to_another_firms_project_is_rejected() {
    let Some(pg) = postgres().await else {
        return;
    };
    let firm_a = seed_firm(&pg).await;
    let firm_b = seed_firm(&pg).await;
    let project_b = seed_project(&pg, firm_b).await;
    let parked = parked_call(&pg, firm_a, "CAconversation-cross", 18).await;

    let error =
        billed_calls::attribute_conversation(&pg, firm_a, parked.conversation_id, project_b)
            .await
            .unwrap_err();
    assert!(
        matches!(
            error,
            BilledCallError::Invalid("project does not belong to this firm")
        ),
        "another Firm's Project must be rejected, got {error:?}"
    );
    let missing =
        billed_calls::attribute_conversation(&pg, firm_a, parked.conversation_id, Uuid::now_v7())
            .await
            .unwrap_err();
    assert!(
        matches!(
            missing,
            BilledCallError::Invalid("project does not belong to this firm")
        ),
        "an unknown Project must be rejected without confirming existence, got {missing:?}"
    );
    let stored = billed_calls::find_by_call_sid(&pg, "CAconversation-cross")
        .await
        .unwrap()
        .expect("the row survives the refused attribution");
    assert_eq!(
        stored.project_id, None,
        "the row stays parked at the Firm level"
    );
}

/// The parked queue lists only the Firm's own unattributed rows, newest
/// first, and attribution removes a row from it.
#[tokio::test]
async fn the_parked_queue_lists_only_the_firms_own_unattributed_rows() {
    let Some(pg) = postgres().await else {
        return;
    };
    let firm_a = seed_firm(&pg).await;
    let firm_b = seed_firm(&pg).await;
    let earlier = parked_call(&pg, firm_a, "CAqueue-a-early", 17).await;
    let later = parked_call(&pg, firm_a, "CAqueue-a-late", 19).await;
    let other = parked_call(&pg, firm_b, "CAqueue-b", 18).await;

    let queue_a = billed_calls::parked_for_firm(&pg, firm_a).await.unwrap();
    assert_eq!(
        queue_a
            .iter()
            .map(|row| row.call_sid.clone())
            .collect::<Vec<_>>(),
        vec![later.call_sid, earlier.call_sid.clone()],
        "the queue is newest first and holds only this Firm's rows"
    );
    let queue_b = billed_calls::parked_for_firm(&pg, firm_b).await.unwrap();
    assert_eq!(
        queue_b
            .iter()
            .map(|row| row.call_sid.clone())
            .collect::<Vec<_>>(),
        vec![other.call_sid],
        "another Firm's row never appears in the queue"
    );

    let project = seed_project(&pg, firm_a).await;
    billed_calls::attribute_conversation(&pg, firm_a, later.conversation_id, project)
        .await
        .expect("attribution succeeds");
    let queue_after = billed_calls::parked_for_firm(&pg, firm_a).await.unwrap();
    assert_eq!(
        queue_after
            .iter()
            .map(|row| row.call_sid.clone())
            .collect::<Vec<_>>(),
        vec![earlier.call_sid],
        "an attributed row leaves the parked queue"
    );
}
