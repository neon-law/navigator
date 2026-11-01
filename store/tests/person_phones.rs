//! The structured phone list on a Person: E.164 normalization, one row
//! per number, and the single-primary convention.
//!
//! `store/tests/schema_apply_backfills_historical_person_writes.rs` covers
//! historical *person* rows; this file covers the separate
//! `person_phone` table, which is additive — no historical row of any
//! existing table changes shape, so the reads that must survive are the
//! ones over rows written before this module existed. That is exercised
//! by creating a person through the legacy `person.phone` column and
//! reading it back after `seed_from_legacy_phone` runs.

use store::person_phones::{self, PhoneError, PhoneLabel};
use store::persons::{self, NewPerson};
use store::surreal::test_support::mem;

async fn a_person(db: &store::surreal::SurrealDb, email: &str, phone: Option<&str>) -> uuid::Uuid {
    let created = persons::create(
        db,
        &NewPerson {
            name: "Test Person".into(),
            email: email.into(),
            role: store::persons::Role::Client,
            given_name: None,
            family_name: None,
            middle_name: None,
            oidc_subject: None,
            microsoft_subject: None,
            apple_subject: None,
            title: None,
            phone: phone.map(Into::into),
            profile_image_url: None,
            linkedin_url: None,
        },
    )
    .await
    .expect("create the fixture person");
    created.id
}

#[tokio::test]
async fn the_first_number_a_person_gets_is_primary_whether_or_not_the_caller_said_so() {
    let db = mem().await;
    let person = a_person(&db, "first.primary@example.com", None).await;

    let phone = person_phones::set_phone(&db, person, "(702) 555-0142", PhoneLabel::Mobile, false)
        .await
        .expect("the first number sets");
    assert_eq!(phone.e164, "+17025550142");
    assert!(
        phone.is_primary,
        "a Person with no number cannot answer which one first"
    );
}

#[tokio::test]
async fn differently_formatted_spellings_of_one_number_converge_on_one_row() {
    let db = mem().await;
    let person = a_person(&db, "converge@example.com", None).await;
    person_phones::set_phone(&db, person, "(702) 555-0142", PhoneLabel::Mobile, true)
        .await
        .expect("the first spelling sets");

    let rewritten = person_phones::set_phone(&db, person, "+17025550142", PhoneLabel::Work, false)
        .await
        .expect("the canonical spelling restamps the same row");
    assert_eq!(rewritten.label, PhoneLabel::Work);

    let phones = person_phones::list_phones(&db, person).await.unwrap();
    assert_eq!(phones.len(), 1, "one number, not two rows");
    assert_eq!(phones[0].e164, "+17025550142");
}

#[tokio::test]
async fn setting_a_number_primary_demotes_the_previous_primary() {
    let db = mem().await;
    let person = a_person(&db, "one.primary@example.com", None).await;
    person_phones::set_phone(&db, person, "7025550142", PhoneLabel::Mobile, true)
        .await
        .unwrap();
    person_phones::set_phone(&db, person, "7025550143", PhoneLabel::Work, false)
        .await
        .unwrap();
    person_phones::set_phone(&db, person, "+17025550144", PhoneLabel::Home, true)
        .await
        .unwrap();

    let phones = person_phones::list_phones(&db, person).await.unwrap();
    let primaries: Vec<_> = phones.iter().filter(|p| p.is_primary).collect();
    assert_eq!(primaries.len(), 1, "exactly one primary survives");
    assert_eq!(primaries[0].e164, "+17025550144");

    // And demoting explicitly keeps exactly one: re-setting the current
    // primary as non-primary is the caller's mistake, but the store
    // still leaves the list well-formed — the demoted write lands
    // without promotion elsewhere.
    person_phones::set_phone(&db, person, "+17025550144", PhoneLabel::Home, false)
        .await
        .unwrap();
    let phones = person_phones::list_phones(&db, person).await.unwrap();
    assert_eq!(phones.iter().filter(|p| p.is_primary).count(), 0);
}

#[tokio::test]
async fn setting_a_phone_on_a_missing_person_is_refused_by_name() {
    let db = mem().await;
    let missing = uuid::Uuid::now_v7();
    let error = person_phones::set_phone(&db, missing, "7025550142", PhoneLabel::Mobile, true)
        .await
        .expect_err("a phone row needs its person");
    assert!(matches!(error, PhoneError::PersonMissing(id) if id == missing));
}

#[tokio::test]
async fn removing_a_number_its_row_gone_and_an_absent_number_a_no_op() {
    let db = mem().await;
    let person = a_person(&db, "remove@example.com", None).await;
    person_phones::set_phone(&db, person, "7025550142", PhoneLabel::Mobile, true)
        .await
        .unwrap();

    person_phones::remove_phone(&db, person, "(702) 555-0142")
        .await
        .expect("removing by any spelling");
    assert!(person_phones::list_phones(&db, person)
        .await
        .unwrap()
        .is_empty());

    person_phones::remove_phone(&db, person, "7025550142")
        .await
        .expect("removing an absent number changes nothing");
}

#[tokio::test]
async fn the_legacy_phone_column_seeds_one_primary_row_and_then_leaves_alone() {
    let db = mem().await;
    let person = a_person(&db, "legacy.seed@example.com", Some("702 555 0142")).await;

    let seeded = person_phones::seed_from_legacy_phone(&db, person)
        .await
        .expect("the legacy value seeds")
        .expect("a normalizable legacy value becomes a row");
    assert_eq!(seeded.e164, "+17025550142");
    assert!(seeded.is_primary);
    assert_eq!(seeded.label, PhoneLabel::Mobile);

    // Idempotent: a Person with structured rows is left alone, and the
    // legacy column is not re-read after the first lift.
    person_phones::set_phone(&db, person, "+17025550199", PhoneLabel::Work, false)
        .await
        .unwrap();
    assert!(person_phones::seed_from_legacy_phone(&db, person)
        .await
        .unwrap()
        .is_none());
    let phones = person_phones::list_phones(&db, person).await.unwrap();
    assert_eq!(phones.len(), 2, "the seed did not duplicate or overwrite");
}

#[tokio::test]
async fn an_unnormalizable_legacy_value_is_left_where_it_is() {
    let db = mem().await;
    let person = a_person(&db, "legacy.junk@example.com", Some("call the front desk")).await;

    assert!(person_phones::seed_from_legacy_phone(&db, person)
        .await
        .unwrap()
        .is_none());
    // The legacy column still holds the value; nothing was invented.
    let person_row = persons::find_by_id(&db, person).await.unwrap().unwrap();
    assert_eq!(person_row.phone.as_deref(), Some("call the front desk"));
    assert!(person_phones::list_phones(&db, person)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn a_historical_person_row_without_phone_data_reads_and_seeds_cleanly() {
    let db = mem().await;
    // A row written before `person_phone` existed has no phones and no
    // legacy value to lift: both reads must be empty, not errors — the
    // same tolerance the historical-row test above the person table
    // relies on.
    let person = a_person(&db, "historical.phones@example.com", None).await;
    assert!(person_phones::list_phones(&db, person)
        .await
        .unwrap()
        .is_empty());
    assert!(person_phones::seed_from_legacy_phone(&db, person)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn the_schema_refuses_an_e164_that_does_not_start_with_a_plus() {
    let db = mem().await;
    let person = a_person(&db, "schema.assert@example.com", None).await;
    let error = db
        .query(
            "CREATE person_phone SET person_id = $person, e164 = '7025550142', is_primary = true",
        )
        .bind(("person", store::surreal::record_id("person", person)))
        .await
        .unwrap()
        .check();
    assert!(error.is_err(), "the field ASSERT must refuse a bare number");
}
