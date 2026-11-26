//! Entity behaviors tied to the explicit Source compatibility contract.

use super::{
    classify_write, create, find_by_id, firm_anchor_holder, prime_claim_table, AlreadyExistsError,
    EntityError, ErrorDetails, NewEntity, CLAIM, CLAIM_TABLE, RELEASE, TABLE, WRITE_FIELDS,
};
use crate::surreal::test_support::mem;
use crate::surreal::{record_id, retry};
use uuid::Uuid;

fn input(name: &str) -> NewEntity {
    NewEntity {
        name: name.into(),
        entity_type_id: Uuid::now_v7(),
        jurisdiction_id: Uuid::now_v7(),
        phone: None,
        url: None,
        xero_id: None,
        firm_anchor_key: None,
    }
}

fn anchor(name: &str) -> NewEntity {
    NewEntity {
        firm_anchor_key: Some(name.to_lowercase()),
        ..input(name)
    }
}

#[tokio::test]
async fn reads_an_entity_row_written_before_avatar_url_was_defined() {
    let db = mem().await;
    db.query("REMOVE FIELD avatar_url ON entity").await.unwrap();
    let created = create(&db, &input("Epsilon LLC")).await.unwrap();
    db.query("DEFINE FIELD OVERWRITE avatar_url ON entity TYPE option<string>")
        .await
        .unwrap();

    assert_eq!(
        find_by_id(&db, created.id)
            .await
            .unwrap()
            .unwrap()
            .avatar_url,
        None,
        "an absent value reads as no avatar, not a deserialization failure"
    );
}

#[tokio::test]
async fn reads_an_entity_without_metadata_from_source() {
    let db = mem().await;
    let created = create(&db, &input("Example LLC")).await.unwrap();
    db.query("UPDATE $id UNSET metadata")
        .bind(("id", record_id(TABLE, created.id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(
        find_by_id(&db, created.id).await.unwrap().unwrap().metadata,
        None
    );
}

#[tokio::test]
async fn reads_an_entity_row_written_before_xero_id_was_defined() {
    let db = mem().await;
    db.query("REMOVE FIELD xero_id ON entity").await.unwrap();
    let created = create(&db, &input("Zeta Holdings LLC")).await.unwrap();
    db.query("DEFINE FIELD OVERWRITE xero_id ON entity TYPE option<string>")
        .await
        .unwrap();

    assert_eq!(
        find_by_id(&db, created.id).await.unwrap().unwrap().xero_id,
        None,
        "an absent value reads as no Xero contact, not a deserialization failure"
    );
}

/// The claim collision — the arm that actually runs — is typed, and
/// this is what says so. `classify_write` reads
/// [`AlreadyExistsError::Record`] rather than the message, so an
/// upstream reword of the prose cannot turn a refusal into a `Db`
/// fault. The day that detail stops arriving, this fails.
///
/// It must also not be retryable: a refusal that
/// [`crate::surreal::retry::is_retryable`] accepted would be re-run
/// until the write budget expired and then reported as a conflict, so
/// the caller would wait seconds for the wrong error.
#[tokio::test]
async fn the_claim_collision_is_typed_and_final() {
    let db = mem().await;
    create(&db, &anchor("Neon Law")).await.unwrap();

    let raw = db
        .query("CREATE type::record('firm_anchor', $key) SET entity_id = $id")
        .bind(("key", "neon law".to_string()))
        .bind(("id", record_id(TABLE, Uuid::now_v7())))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .expect_err("a second claim must collide");

    assert!(
        matches!(
            raw.details(),
            ErrorDetails::AlreadyExists(Some(AlreadyExistsError::Record { id }))
                if id.starts_with(CLAIM_TABLE)
        ),
        "the claim's typed detail is gone — classify_write would fall back \
             to the message; got {:?}",
        raw.details(),
    );
    assert!(
        !retry::is_retryable(&raw),
        "a refused claim is not a lost race and must not be re-run",
    );
    assert!(matches!(classify_write(raw), EntityError::FirmAnchorTaken));
}

/// The backstop arm, held against the engine the way
/// `store::persons` holds its own.
///
/// The UNIQUE index cannot serialize racers, but it still refuses a
/// fork that is not a race — and that refusal *is* untyped: the engine
/// raises it as the `Internal` catch-all, so the index name in the
/// message is the only discriminator left. It is a `DEFINE INDEX`
/// identifier this workspace chose, and this pins it, so a silent
/// reword fails here rather than in production.
#[tokio::test]
async fn a_unique_violation_carries_no_typed_detail_only_the_index_name() {
    let db = mem().await;
    let holder = create(&db, &anchor("Neon Law")).await.unwrap();
    // Around the claim on purpose: this exercises the index alone,
    // which is what a non-concurrent fork would meet.
    let raw = db
        .query(format!(
            "UPSERT $id SET {WRITE_FIELDS}, inserted_at = time::now()"
        ))
        .bind(("id", record_id(TABLE, Uuid::now_v7())))
        .bind(("name", "Neon Law".to_string()))
        .bind((
            "entity_type_id",
            record_id("entity_type", holder.entity_type_id),
        ))
        .bind((
            "jurisdiction_id",
            record_id("jurisdiction", holder.jurisdiction_id),
        ))
        .bind(("phone", None::<String>))
        .bind(("url", None::<String>))
        .bind(("xero_id", None::<String>))
        .bind(("firm_anchor_key", Some("neon law".to_string())))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .expect_err("the index must refuse the second row");

    assert!(
        matches!(raw.details(), ErrorDetails::Internal),
        "a typed detail is now available — classify_write should match on it \
             instead of the message; got {:?}",
        raw.details(),
    );
    assert!(
        raw.to_string().contains("entity_firm_anchor"),
        "the index name is the only discriminator, and it is gone: {raw}",
    );
    assert!(matches!(classify_write(raw), EntityError::FirmAnchorTaken));
}

/// An error that is neither refusal stays a `Db` fault. Widening
/// either arm into a catch-all would make a server fault read as a
/// clean refusal, which is exactly how a broken guard looks green.
#[tokio::test]
async fn an_unrelated_failure_is_not_mistaken_for_a_refusal() {
    let db = mem().await;
    let raw = db
        .query("SELECT * FROM entity WHERE")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .expect_err("a malformed statement must fail");

    assert!(matches!(classify_write(raw), EntityError::Db(_)));
}

/// [`CLAIM`] and [`RELEASE`] name the table in SurrealQL, where a
/// `const` cannot interpolate [`CLAIM_TABLE`]. Renaming the constant
/// without rewriting the statements would leave writes claiming one
/// table while [`claims_the_firm_anchor`] classified another — a
/// refusal silently demoted to a server fault.
#[test]
fn the_claim_statements_name_the_claim_table() {
    assert!(CLAIM.contains(CLAIM_TABLE), "{CLAIM}");
    assert!(RELEASE.contains(CLAIM_TABLE), "{RELEASE}");
}

#[tokio::test]
async fn priming_an_empty_claim_table_leaves_real_keys_free() {
    let db = mem().await;

    let before: Vec<surrealdb::types::RecordId> = db
        .query("SELECT VALUE id FROM firm_anchor")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .expect("read the empty claim table")
        .take(0)
        .expect("decode the empty claim table");
    assert!(before.is_empty());

    prime_claim_table(&db).await.expect("prime the claim table");

    let key = "__navigator_claim_sentinel__";
    assert_eq!(firm_anchor_holder(&db, key).await.unwrap(), None);
    let id = Uuid::now_v7();
    db.query(CLAIM)
        .bind(("id", record_id(TABLE, id)))
        .bind(("firm_anchor_key", key.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .expect("claim a real key next to the sentinel");
    assert_eq!(firm_anchor_holder(&db, key).await.unwrap(), Some(id));
    let after: Vec<surrealdb::types::RecordId> = db
        .query("SELECT VALUE id FROM firm_anchor")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .expect("read the primed claim table")
        .take(0)
        .expect("decode the primed claim table");
    assert_eq!(after.len(), 2);
}
