//! The per-Person mail signature against PostgreSQL (LAW-280). These tests
//! arm themselves only where a PostgreSQL test lane exists; the pure
//! validation tests live in the module. The Person directory resolves
//! through the in-process `SurrealDB`, the same engine the write gate
//! authorizes against.

use store::mail_signatures::{self, MailSignatureError, MailSignatureFields};
use store::persons::{self, NewPerson};
use store::postgres::PostgresDb;
use store::test_support::mem_surreal;
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
    let database = format!("mail_signature_test_{}", Uuid::now_v7().simple());
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

/// A real Person row, so the gate's subject resolution answers it.
async fn person(name: &str) -> (store::surreal::SurrealDb, Uuid) {
    let surreal = mem_surreal().await;
    let created = persons::create(
        &surreal,
        &NewPerson {
            name: name.to_string(),
            email: format!("{name}@example.com"),
            ..NewPerson::default()
        },
    )
    .await
    .unwrap();
    (surreal, created.id)
}

fn block() -> MailSignatureFields {
    MailSignatureFields {
        body: "Example Attorney\nConfidential: intended recipient only.".to_string(),
    }
}

/// A saved signature round-trips, a second save replaces the first, and one
/// Person's row never answers for another.
#[tokio::test]
async fn a_saved_signature_round_trips_and_a_resave_replaces_it() {
    let Some(pg) = postgres().await else {
        return;
    };
    let (surreal, attorney) = person("attorney").await;
    let (other_surreal, colleague) = person("colleague").await;
    assert_eq!(mail_signatures::find(&pg, attorney).await.unwrap(), None);
    assert_eq!(
        mail_signatures::for_person(&surreal, &pg, attorney)
            .await
            .unwrap(),
        None,
        "a Person with no row reads as none"
    );

    let saved = mail_signatures::save_for_actor(&surreal, &pg, attorney, attorney, &block())
        .await
        .unwrap();
    assert_eq!(saved.body, block().body);
    assert!(saved.updated_at.is_some(), "the write stamps a revision");
    assert_eq!(
        mail_signatures::find(&pg, attorney).await.unwrap(),
        Some(block())
    );
    assert_eq!(
        mail_signatures::for_person(&surreal, &pg, attorney)
            .await
            .unwrap()
            .unwrap(),
        saved
    );
    assert_eq!(
        mail_signatures::find(&pg, colleague).await.unwrap(),
        None,
        "one Person's row is keyed to that Person"
    );

    let revised = MailSignatureFields {
        body: "Example Attorney, Of Counsel".to_string(),
    };
    let resaved =
        mail_signatures::save_for_actor(&other_surreal, &pg, attorney, attorney, &revised)
            .await
            .unwrap();
    assert_eq!(resaved.body, revised.body);
    assert_eq!(
        mail_signatures::find(&pg, attorney).await.unwrap(),
        Some(revised)
    );
}

/// The self-edit gate: another actor, and an unknown subject, both get the
/// same non-disclosing `NotFound`, and nothing is written either way.
#[tokio::test]
async fn save_for_actor_refuses_anyone_but_the_attorney_without_writing() {
    let Some(pg) = postgres().await else {
        return;
    };
    let (surreal, attorney) = person("attorney").await;
    let (_, impostor) = person("impostor").await;

    for (actor, subject) in [(impostor, attorney), (attorney, Uuid::now_v7())] {
        let error = mail_signatures::save_for_actor(&surreal, &pg, actor, subject, &block())
            .await
            .unwrap_err();
        assert!(matches!(error, MailSignatureError::NotFound), "{error:?}");
        assert_eq!(error.to_string(), "no such person");
        assert_eq!(
            mail_signatures::find(&pg, subject).await.unwrap(),
            None,
            "a refused save writes nothing"
        );
    }
}

/// The table refuses what validation would: a stray delimiter line.
#[tokio::test]
async fn the_table_refuses_an_unvalidated_body() {
    let Some(pg) = postgres().await else {
        return;
    };
    let refused = mail_signatures::put(
        &pg,
        Uuid::now_v7(),
        &MailSignatureFields {
            body: "Example Attorney\n--\nConfidential: intended recipient only.".to_string(),
        },
    )
    .await;
    assert!(refused.is_err(), "the delimiter check holds at the table");
}

/// A Person the directory does not know reads as no signature, the shape the
/// draft command's env fallback announces.
#[tokio::test]
async fn an_unknown_person_reads_as_no_signature() {
    let Some(pg) = postgres().await else {
        return;
    };
    let (surreal, attorney) = person("attorney").await;
    mail_signatures::put(&pg, attorney, &block()).await.unwrap();
    let stranger = Uuid::now_v7();
    assert_eq!(
        mail_signatures::for_person(&surreal, &pg, stranger)
            .await
            .unwrap(),
        None,
        "no Person, no signature row"
    );
}
