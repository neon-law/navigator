//! The brand phone-number registry against PostgreSQL, the engine that
//! holds the table. These tests arm themselves only where a PostgreSQL
//! test lane exists; the pure validation tests live in the module.
use store::brand_phone_numbers::{self, ChannelKinds, NewBrandPhoneNumber};
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
    let database = format!("registry_test_{}", Uuid::now_v7().simple());
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

fn registration(number: &str, brand: &str, kinds: ChannelKinds) -> NewBrandPhoneNumber {
    // Deterministic credentials, as the store's own unit tests use: the
    // idempotent-put test relies on two calls producing IDENTICAL content,
    // which per-call `now_v7()` values never are.
    NewBrandPhoneNumber {
        number_e164: number.into(),
        brand_key: brand.into(),
        twilio_account_sid: "AC7f5ab4c8de0123456789abcdef01234".into(),
        auth_token_secret_ref: Uuid::from_u128(1),
        channel_kinds: kinds,
    }
}

/// Two Brands share a deployment: each callee resolves to its own row,
/// and a number that never registered resolves to nothing.
#[tokio::test]
async fn lookup_by_number_returns_the_registered_row() {
    let Some(pg) = postgres().await else {
        return;
    };
    let neon = brand_phone_numbers::put(
        &pg,
        &registration("+15551230001", "neon", ChannelKinds::Both),
    )
    .await
    .unwrap();
    let anchor = brand_phone_numbers::put(
        &pg,
        &registration("+15551230002", "anchor", ChannelKinds::Sms),
    )
    .await
    .unwrap();

    let found = brand_phone_numbers::find_by_number(&pg, "+15551230001")
        .await
        .unwrap()
        .expect("registered callee");
    assert_eq!(found, neon);
    assert_eq!(found.brand_key, "neon");
    assert!(found.channel_kinds.serves(ChannelKinds::Voice));

    let found = brand_phone_numbers::find_by_number(&pg, "+15551230002")
        .await
        .unwrap()
        .expect("registered callee");
    assert_eq!(found, anchor);

    assert!(
        brand_phone_numbers::find_by_number(&pg, "+15551239999")
            .await
            .unwrap()
            .is_none(),
        "an unregistered callee must resolve to nothing"
    );
}

/// `put` is idempotent on identical content and loud on a mismatch: the
/// registry is configuration, and a silent overwrite would misroute calls
/// billed against the old mapping.
#[tokio::test]
async fn a_repeated_identical_put_returns_the_original_row_and_a_mismatch_conflicts() {
    let Some(pg) = postgres().await else {
        return;
    };
    let first = brand_phone_numbers::put(
        &pg,
        &registration("+15551230003", "neon", ChannelKinds::Sms),
    )
    .await
    .unwrap();
    let again = brand_phone_numbers::put(
        &pg,
        &registration("+15551230003", "neon", ChannelKinds::Sms),
    )
    .await
    .unwrap();
    assert_eq!(
        first, again,
        "identical content converges on the original row"
    );

    let mut retargeted = registration("+15551230003", "anchor", ChannelKinds::Sms);
    retargeted.twilio_account_sid = first.twilio_account_sid.clone();
    retargeted.auth_token_secret_ref = first.auth_token_secret_ref;
    let conflict = brand_phone_numbers::put(&pg, &retargeted)
        .await
        .unwrap_err();
    assert!(
        matches!(
            conflict,
            brand_phone_numbers::BrandPhoneNumberError::Conflict
        ),
        "re-pointing a number at another Brand must conflict loudly, got {conflict:?}"
    );
    assert_eq!(
        brand_phone_numbers::find_by_number(&pg, "+15551230003")
            .await
            .unwrap()
            .unwrap(),
        first,
        "the failed overwrite leaves the original registration intact"
    );

    let mut other_kinds = registration("+15551230003", "neon", ChannelKinds::Both);
    other_kinds.twilio_account_sid = first.twilio_account_sid.clone();
    other_kinds.auth_token_secret_ref = first.auth_token_secret_ref;
    assert!(
        brand_phone_numbers::put(&pg, &other_kinds).await.is_err(),
        "the same number with different lanes is a mismatch too"
    );
}

/// Hold both registrations at their table writes without relying on task timing.
/// SHARE admits row reads but blocks either writer until both are observed.
async fn concurrent_first_puts(
    pg: &PostgresDb,
    left: NewBrandPhoneNumber,
    right: NewBrandPhoneNumber,
) -> [Result<brand_phone_numbers::BrandPhoneNumber, brand_phone_numbers::BrandPhoneNumberError>; 2]
{
    use std::time::Duration;

    let mut controller = pg.pool().begin().await.unwrap();
    sqlx::query("LOCK TABLE brand_phone_number IN SHARE MODE")
        .execute(&mut *controller)
        .await
        .unwrap();
    let first_pg = pg.clone();
    let second_pg = pg.clone();
    let mut first = tokio::spawn(async move { brand_phone_numbers::put(&first_pg, &left).await });
    let mut second =
        tokio::spawn(async move { brand_phone_numbers::put(&second_pg, &right).await });
    let waiting = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM pg_catalog.pg_locks \
                 WHERE database = (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database()) \
                 AND relation = 'brand_phone_number'::regclass \
                 AND mode = 'RowExclusiveLock' AND NOT granted",
            )
            .fetch_one(pg.pool())
            .await
            .unwrap();
            if count == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    // Release the controller even if the synchronization assertion fails.
    controller.commit().await.unwrap();
    if waiting.is_err() {
        first.abort();
        second.abort();
        let _ = tokio::join!(&mut first, &mut second);
        panic!("both first registrations must wait at their table writes");
    }
    let completed = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(&mut first, &mut second)
    })
    .await;
    let Ok((first_result, second_result)) = completed else {
        first.abort();
        second.abort();
        let _ = tokio::join!(&mut first, &mut second);
        panic!("both registrations must finish after the controller releases them");
    };
    [first_result.unwrap(), second_result.unwrap()]
}

#[tokio::test]
async fn concurrent_identical_first_registrations_return_the_same_immutable_row() {
    let Some(pg) = postgres().await else { return };
    let input = registration("+15551230004", "neon", ChannelKinds::Both);
    let [left, right] = concurrent_first_puts(&pg, input.clone(), input.clone()).await;
    let Ok(left) = left else {
        panic!("first identical registration must succeed");
    };
    let Ok(right) = right else {
        panic!("overlapping identical registration must be an idempotent success");
    };
    assert!(left == right, "content and original timestamps must match");
    let Ok(found) = brand_phone_numbers::find_by_number(&pg, &input.number_e164).await else {
        panic!("the stored registration must remain readable");
    };
    assert!(
        found.as_ref() == Some(&left),
        "the stored registration must match"
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM brand_phone_number")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn concurrent_different_first_registrations_conflict_without_overwriting_the_winner() {
    let Some(pg) = postgres().await else { return };
    let left = registration("+15551230005", "neon", ChannelKinds::Both);
    let right = registration("+15551230005", "anchor", ChannelKinds::Both);
    let results = concurrent_first_puts(&pg, left.clone(), right.clone()).await;
    let mut winner = None;
    let mut conflicts = 0;
    for result in results {
        match result {
            Ok(row) => assert!(winner.replace(row).is_none(), "only one registration wins"),
            Err(brand_phone_numbers::BrandPhoneNumberError::Conflict) => conflicts += 1,
            Err(
                brand_phone_numbers::BrandPhoneNumberError::Invalid(_)
                | brand_phone_numbers::BrandPhoneNumberError::Database(_)
                | brand_phone_numbers::BrandPhoneNumberError::Postgres(_),
            ) => panic!("a different registration must return a domain conflict"),
        }
    }
    assert_eq!(conflicts, 1);
    let winner = winner.unwrap();
    assert!(winner.brand_key == left.brand_key || winner.brand_key == right.brand_key);
    let Ok(found) = brand_phone_numbers::find_by_number(&pg, &left.number_e164).await else {
        panic!("the winning registration must remain readable");
    };
    assert!(
        found.as_ref() == Some(&winner),
        "the losing registration cannot change the winning content or timestamps"
    );
}

// The Brand letterhead (LAW-266) is the other Brand-scoped table born in
// PostgreSQL; it shares this binary's lane rather than adding a test binary.

/// A Brand's letterhead round-trips, a second save replaces the first, and
/// one Brand's row never answers for another.
#[tokio::test]
async fn a_saved_letterhead_round_trips_and_a_resave_replaces_it() {
    use store::brand_letterheads::{self, LetterheadFields, LetterheadMark};
    let Some(pg) = postgres().await else {
        return;
    };
    let (first, second) = (Uuid::now_v7(), Uuid::now_v7());
    assert_eq!(brand_letterheads::find(&pg, first).await.unwrap(), None);

    let fields = LetterheadFields {
        wordmark: Some("Example".into()),
        mark: LetterheadMark::Wordmark,
        phone: Some("+1 555 010 0000".into()),
        email: Some("hello@example.com".into()),
        site_url: Some("https://www.example.com".into()),
        postal_address: Some("1 Main St\nSpringfield".into()),
        font_family: None,
    };
    let saved = brand_letterheads::put(&pg, first, &fields).await.unwrap();
    assert_eq!(saved, fields);
    assert_eq!(
        brand_letterheads::find(&pg, first).await.unwrap(),
        Some(fields.clone())
    );
    assert_eq!(brand_letterheads::find(&pg, second).await.unwrap(), None);

    let cleared = LetterheadFields {
        phone: None,
        ..fields
    };
    brand_letterheads::put(&pg, first, &cleared).await.unwrap();
    assert_eq!(
        brand_letterheads::find(&pg, first).await.unwrap(),
        Some(cleared)
    );
}

/// The table refuses what validation would: a non-https site.
#[tokio::test]
async fn the_letterhead_table_refuses_an_unvalidated_site() {
    use store::brand_letterheads::{self, LetterheadFields};
    let Some(pg) = postgres().await else {
        return;
    };
    let refused = brand_letterheads::put(
        &pg,
        Uuid::now_v7(),
        &LetterheadFields {
            site_url: Some("http://www.example.com".into()),
            ..LetterheadFields::default()
        },
    )
    .await;
    assert!(refused.is_err());
}
