//! Test-only seeding for the Twilio webhook resolution spine: a PostgreSQL
//! registry lane and the Firm secrets its registrations pin. Arms only
//! where a PostgreSQL test lane exists — see `store/tests` for the pattern.

use store::brand_phone_numbers::{self, ChannelKinds, NewBrandPhoneNumber};
use store::postgres::PostgresDb;
use store::surreal::SurrealDb;
use uuid::Uuid;

/// A fresh PostgreSQL database with every embedded migration applied, or
/// `None` where no test lane exists.
pub(crate) async fn postgres() -> Option<PostgresDb> {
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
    let database = format!("spine_test_{}", Uuid::now_v7().simple());
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

/// Seal `token` as `firm_id`'s Twilio auth token, register `number` for
/// `brand_key` over it, and return the secret ref the registration pins.
pub(crate) async fn register_number(
    db: &SurrealDb,
    pg: &PostgresDb,
    firm_id: Uuid,
    brand_key: &str,
    number: &str,
    token: &str,
) -> Uuid {
    let dri = store::persons::create(
        db,
        &store::persons::NewPerson::with_role(
            "Synthetic administrator",
            format!("spine-{}@example.com", Uuid::now_v7()),
            store::persons::Role::Admin,
        ),
    )
    .await
    .unwrap();
    store::firm_secrets::put(
        db,
        store::firm_secrets::SecretPutRequest {
            actor_role: store::persons::Role::Admin,
            actor_person_id: Some(dri.id),
            firm_id,
            provider: store::firm_secrets::IntegrationProvider::Twilio,
            kind: store::firm_secrets::IntegrationSecretKind::TwilioAuthToken,
            value: token,
        },
        &cloud::FakeKms::new("spine-test"),
    )
    .await
    .unwrap_or_else(|error| panic!("seed stores the number's token: {error}"));
    // The registry pins the version record's id, which the metadata read
    // does not expose; the seed reads it back the way a later admin surface
    // will.
    let mut response = db
        .query(
            "SELECT VALUE id FROM firm_integration_secret WHERE firm_id = $firm_id \
             AND provider = 'twilio' AND kind = 'twilio_auth_token' AND status = 'active'",
        )
        .bind(("firm_id", store::surreal::record_id("firm", firm_id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    let ids: Vec<surrealdb::types::RecordId> = response.take(0).unwrap();
    let secret_ref = store::surreal::record_uuid(&ids[0]).unwrap();
    brand_phone_numbers::put(
        pg,
        &NewBrandPhoneNumber {
            number_e164: number.into(),
            brand_key: brand_key.into(),
            twilio_account_sid: format!("AC{}", Uuid::now_v7().simple()),
            auth_token_secret_ref: secret_ref,
            channel_kinds: ChannelKinds::Both,
        },
    )
    .await
    .unwrap();
    secret_ref
}
