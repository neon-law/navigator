//! Test-only seeding for the Twilio webhook resolution spine: a PostgreSQL
//! registry lane and the Firm secrets its registrations pin. Arms only
//! where a PostgreSQL test lane exists — see `store/tests` for the pattern.

use crate::brand_phone_numbers::{self, ChannelKinds, NewBrandPhoneNumber};
use crate::postgres::PostgresDb;
use crate::surreal::SurrealDb;
use uuid::Uuid;

/// A fresh PostgreSQL database with every embedded migration applied, or
/// `None` where no test lane exists.
///
/// # Panics
///
/// The configured local test database must be reachable and migrate cleanly.
pub async fn postgres() -> Option<PostgresDb> {
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
///
/// # Panics
///
/// The synthetic Firm must have an Admin DRI, and its test stores must accept the binding.
pub async fn register_number(
    db: &SurrealDb,
    pg: &PostgresDb,
    firm_id: Uuid,
    brand_key: &str,
    number: &str,
    token: &str,
) -> Uuid {
    // The secret store admits only the Firm's Admin DRI, so the seed acts
    // as the Firm's seeded administrator rather than minting a synthetic
    // person no membership row would admit.
    let dri = crate::firms::memberships_for_firm(db, firm_id)
        .await
        .unwrap_or_else(|error| panic!("seed reads the Firm's memberships: {error}"))
        .into_iter()
        .find(|row| row.is_dri && row.membership == crate::firms::FirmMembership::Admin)
        .unwrap_or_else(|| panic!("seed firm has no Admin DRI to authorize the secret put"));
    let secret_ref = crate::firm_secrets::put_twilio_auth_token(
        db,
        crate::firm_secrets::SecretPutRequest {
            actor_role: crate::persons::Role::Admin,
            actor_person_id: Some(dri.person_id),
            firm_id,
            provider: crate::firm_secrets::IntegrationProvider::Twilio,
            kind: crate::firm_secrets::IntegrationSecretKind::TwilioAuthToken,
            value: token,
        },
        &cloud::FakeKms::new("spine-test"),
    )
    .await
    .unwrap_or_else(|error| panic!("seed stores the number's token: {error}"));
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
