//! Per-Person mail signatures and their authorization resolve through one
//! exclusively owned PostgreSQL fixture. Pure validation lives in the module.

use store::mail_signatures::{self, MailSignatureError, MailSignatureFields};
use store::persons::{self, NewPerson};
use store::postgres::PostgresDb;
use uuid::Uuid;

/// A real Person row in the admitted fixture, so subject resolution uses
/// the same PostgreSQL authority as the signature read and write gates.
async fn person(
    application_store: &store::application_store::ApplicationStore,
    name: &str,
) -> Uuid {
    persons::create(
        application_store,
        &NewPerson {
            name: name.to_string(),
            email: format!("{name}@example.com"),
            ..NewPerson::default()
        },
    )
    .await
    .unwrap()
    .id
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
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let attorney = person(&application_store, "attorney").await;
        let colleague = person(&application_store, "colleague").await;
        assert_eq!(mail_signatures::find(&pg, attorney).await.unwrap(), None);
        assert_eq!(
            mail_signatures::for_person(&application_store, &pg, attorney)
                .await
                .unwrap(),
            None,
            "a Person with no row reads as none"
        );

        let saved =
            mail_signatures::save_for_actor(&application_store, &pg, attorney, attorney, &block())
                .await
                .unwrap();
        assert_eq!(saved.body, block().body);
        assert!(saved.updated_at.is_some(), "the write stamps a revision");
        assert_eq!(
            mail_signatures::find(&pg, attorney).await.unwrap(),
            Some(block())
        );
        assert_eq!(
            mail_signatures::for_person(&application_store, &pg, attorney)
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
            mail_signatures::save_for_actor(&application_store, &pg, attorney, attorney, &revised)
                .await
                .unwrap();
        assert_eq!(resaved.body, revised.body);
        assert_eq!(
            mail_signatures::find(&pg, attorney).await.unwrap(),
            Some(revised)
        );
    })
    .await;
}

/// The self-edit gate: another actor, and an unknown subject, both get the
/// same non-disclosing `NotFound`, and nothing is written either way.
#[tokio::test]
async fn save_for_actor_refuses_anyone_but_the_attorney_without_writing() {
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let attorney = person(&application_store, "attorney").await;
        let impostor = person(&application_store, "impostor").await;

        for (actor, subject) in [(impostor, attorney), (attorney, Uuid::now_v7())] {
            let error =
                mail_signatures::save_for_actor(&application_store, &pg, actor, subject, &block())
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
    })
    .await;
}

/// The table refuses what validation would: a stray delimiter line.
#[tokio::test]
async fn the_table_refuses_an_unvalidated_body() {
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let db = store::application_store::ApplicationStore::uninitialized();
        let id = person(&db, "unvalidated").await;
        let refused = mail_signatures::put(
            &pg,
            id,
            &MailSignatureFields {
                body: "Example Attorney\n--\nConfidential: intended recipient only.".to_string(),
            },
        )
        .await;
        assert!(
            matches!(&refused, Err(MailSignatureError::Database(error))
                if error.as_database_error().is_some_and(|error| error.code().as_deref() == Some("23514"))),
            "the delimiter check holds at the table: {refused:?}"
        );
        assert_eq!(mail_signatures::find(&pg, id).await.unwrap(), None);
    })
    .await;
}

/// A Person the directory does not know reads as no signature, the shape the
/// draft command's env fallback announces.
#[tokio::test]
async fn an_unknown_person_reads_as_no_signature() {
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let attorney = person(&application_store, "attorney").await;
        mail_signatures::put(&pg, attorney, &block()).await.unwrap();
        let stranger = Uuid::now_v7();
        assert_eq!(
            mail_signatures::for_person(&application_store, &pg, stranger)
                .await
                .unwrap(),
            None,
            "no Person, no signature row"
        );
    })
    .await;
}
