use super::PERSON_TABLE;
use crate::surreal::record_id;
use crate::surreal::test_support::mem;
use uuid::Uuid;

#[tokio::test]
async fn the_engine_itself_would_have_accepted_the_dangling_link() {
    let db = mem().await;

    // Captured Source rows may have dangling links; import must handle them.
    db.query("CREATE $id SET person_id = $person, system = 'github', external_id = '12345'")
        .bind(("id", record_id("person_external_identity", Uuid::now_v7())))
        .bind(("person", record_id(PERSON_TABLE, Uuid::now_v7())))
        .await
        .unwrap()
        .check()
        .expect("the engine does not validate a record link");
}

/// The ASSERT is the engine-side half of the closed vocabulary:
/// `parse` refuses to read an unknown system back, and this refuses
/// to write one in the first place.
#[tokio::test]
async fn the_schema_refuses_a_system_outside_the_vocabulary() {
    let db = mem().await;

    let rejected = db
        .query("CREATE $id SET person_id = $person, system = 'gitlab', external_id = '1'")
        .bind(("id", record_id("person_external_identity", Uuid::now_v7())))
        .bind(("person", record_id(PERSON_TABLE, Uuid::now_v7())))
        .await
        .unwrap()
        .check();

    assert!(rejected.is_err(), "the engine accepted system 'gitlab'");
}
