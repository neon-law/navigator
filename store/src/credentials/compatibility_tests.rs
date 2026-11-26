use super::{JURISDICTION_TABLE, PERSON_TABLE};
use crate::surreal::record_id;
use crate::surreal::test_support::mem;
use uuid::Uuid;

#[tokio::test]
async fn the_engine_itself_would_have_accepted_the_dangling_links() {
    let db = mem().await;

    // Captured Source rows may have dangling links; import must handle them.
    db.query("CREATE $id SET person_id = $person, jurisdiction_id = $j, license_number = 'X'")
        .bind(("id", record_id("credential", Uuid::now_v7())))
        .bind(("person", record_id(PERSON_TABLE, Uuid::now_v7())))
        .bind(("j", record_id(JURISDICTION_TABLE, Uuid::now_v7())))
        .await
        .unwrap()
        .check()
        .expect("the engine does not validate a record link");
}
