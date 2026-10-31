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
