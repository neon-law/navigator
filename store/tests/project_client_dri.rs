//! `project.client_dri_person_id` — the Project's pinned client DRI (LAW-177).
//!
//! The participation model marks accountability
//! (`person_project_role.is_client_dri`); this column pins *which* of the
//! client-side rows a `CLIENT_DRI` special question and a
//! `project.client_dri` Clause binding resolve to at render time. What these
//! tests pin: a matter opens with none, the column round-trips, and the write
//! fails closed — a Person with no `is_client_dri` row on *this* matter is
//! refused, never guessed into the pin.

use store::projects::{create, find_by_id, set_client_dri_person_id, NewProject};
use store::surreal::record_id;
use store::test_support::{mem_surreal, seed_entity};
use uuid::Uuid;

async fn project(surreal: &store::surreal::SurrealDb, code: &str) -> store::projects::Project {
    create(
        surreal,
        &NewProject {
            code: code.to_string(),
            name: code.to_string(),
            status: "open".to_string(),
            entity_id: seed_entity(surreal).await,
            ..Default::default()
        },
    )
    .await
    .expect("create matter")
}

async fn client_person(surreal: &store::surreal::SurrealDb, name: &str) -> store::persons::Person {
    store::persons::find_or_create(
        surreal,
        &store::persons::NewPerson::new(name, format!("{name}@client.test")),
    )
    .await
    .expect("seed client person")
}

/// Mark an existing participation row as the matter's client DRI. Tests reach
/// for the raw flag rather than the designation door because the door's
/// authorization is its own concern — pinned by `store::participation` — and
/// this file pins the *column's* rule.
async fn mark_client_dri(surreal: &store::surreal::SurrealDb, role_id: Uuid) {
    surreal
        .query("UPDATE $id SET is_client_dri = true, updated_at = $now")
        .bind(("id", record_id("person_project_role", role_id)))
        .bind(("now", chrono::Utc::now().to_rfc3339()))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .expect("mark the participation row as the client DRI");
}

/// The Project's client DRI participation row, inserted directly.
async fn add_client_dri_participation(
    surreal: &store::surreal::SurrealDb,
    project_id: Uuid,
    person_id: Uuid,
) -> Uuid {
    let role = store::projects::add_participation(surreal, project_id, person_id, "client")
        .await
        .expect("add the client participation");
    mark_client_dri(surreal, role.id).await;
    role.id
}

/// A matter opens with no client DRI pinned, and nothing invents one.
#[tokio::test]
async fn a_new_matter_records_no_client_dri() {
    let surreal = mem_surreal().await;
    let row = project(&surreal, "client-dri-absent").await;
    assert_eq!(
        row.client_dri_person_id, None,
        "a matter opens before its client DRI is pinned"
    );
}

/// A row written before the field existed — a raw `CREATE` that bypasses
/// every command seam — reads back through the production accessor with
/// `None`. No backfill may run between deploy and read.
#[tokio::test]
async fn a_row_written_without_the_field_reads_as_none() {
    let surreal = mem_surreal().await;
    let entity_id = seed_entity(&surreal).await;
    let now = chrono::Utc::now().to_rfc3339();
    let historical_id = Uuid::now_v7();
    surreal
        .query(
            "CREATE $id SET code = 'client-dri-historical', name = 'Historical Row', \
             status = 'open', brand = 'neon', entity_id = $entity_id, \
             inserted_at = $now, updated_at = $now",
        )
        .bind(("id", record_id("project", historical_id)))
        .bind(("entity_id", record_id("entity", entity_id)))
        .bind(("now", now))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .expect("create a project row with no client_dri_person_id field");

    let read = find_by_id(&surreal, historical_id)
        .await
        .expect("read the historical row")
        .expect("the row exists");
    assert_eq!(
        read.client_dri_person_id, None,
        "a historical row has no value and none may be inferred"
    );
}

/// The column round-trips: what was pinned is exactly what a render reads.
#[tokio::test]
async fn the_pinned_client_dri_round_trips() {
    let surreal = mem_surreal().await;
    let row = project(&surreal, "client-dri-roundtrip").await;
    let person = client_person(&surreal, "Roundtrip Client").await;
    add_client_dri_participation(&surreal, row.id, person.id).await;

    let pinned = set_client_dri_person_id(&surreal, row.id, Some(person.id))
        .await
        .expect("pin the client DRI")
        .expect("the matter exists");
    assert_eq!(pinned.client_dri_person_id, Some(person.id));

    let read = find_by_id(&surreal, row.id)
        .await
        .expect("read back")
        .expect("the row exists");
    assert_eq!(read.client_dri_person_id, Some(person.id));
}

/// Fails closed: a Person with a participation row on the matter but no
/// `is_client_dri` marker is refused — the column never promotes a plain
/// contact into the accountability seat.
#[tokio::test]
async fn a_client_side_person_without_the_marker_is_refused() {
    let surreal = mem_surreal().await;
    let row = project(&surreal, "client-dri-undesignated").await;
    let person = client_person(&surreal, "Undesignated Client").await;

    store::projects::add_participation(&surreal, row.id, person.id, "client")
        .await
        .expect("add the participation without the DRI marker");

    let refused = set_client_dri_person_id(&surreal, row.id, Some(person.id))
        .await
        .expect_err("an unmarked person must be refused");
    assert!(
        refused
            .to_string()
            .contains("no client-DRI participation row"),
        "the refusal names the participation rule: {refused}"
    );

    // The refusal left the column untouched.
    let read = find_by_id(&surreal, row.id)
        .await
        .expect("read back")
        .expect("the row exists");
    assert_eq!(read.client_dri_person_id, None, "nothing was written");
}

/// Fails closed: an unknown Person is refused before any write.
#[tokio::test]
async fn an_unknown_person_is_refused() {
    let surreal = mem_surreal().await;
    let row = project(&surreal, "client-dri-unknown").await;

    let refused = set_client_dri_person_id(&surreal, row.id, Some(Uuid::now_v7()))
        .await
        .expect_err("an unknown person must be refused");
    assert!(
        refused.to_string().contains("no such person"),
        "the refusal names the missing Person: {refused}"
    );
}

/// Fails closed: a designated client DRI on a *different* matter does not
/// carry over — the participation must be on the Project being pinned.
#[tokio::test]
async fn a_designation_on_another_matter_does_not_carry_over() {
    let surreal = mem_surreal().await;
    let row = project(&surreal, "client-dri-cross-matter").await;
    let other = project(&surreal, "client-dri-cross-matter-other").await;
    let person = client_person(&surreal, "Cross-matter Client").await;
    add_client_dri_participation(&surreal, other.id, person.id).await;

    let refused = set_client_dri_person_id(&surreal, row.id, Some(person.id))
        .await
        .expect_err("a designation on another matter must be refused");
    assert!(
        refused
            .to_string()
            .contains("no client-DRI participation row"),
        "the refusal names the participation rule: {refused}"
    );
}

/// Clearing the pin is always allowed and leaves the Person untouched.
#[tokio::test]
async fn clearing_the_pin_is_allowed() {
    let surreal = mem_surreal().await;
    let row = project(&surreal, "client-dri-clear").await;
    let person = client_person(&surreal, "Fixture Client").await;
    add_client_dri_participation(&surreal, row.id, person.id).await;
    set_client_dri_person_id(&surreal, row.id, Some(person.id))
        .await
        .expect("pin first")
        .expect("the matter exists");

    let cleared = set_client_dri_person_id(&surreal, row.id, None)
        .await
        .expect("clear the pin")
        .expect("the matter exists");
    assert_eq!(cleared.client_dri_person_id, None);
}
