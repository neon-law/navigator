//! The engine shape of the firm glossary's reference data (#894).
//!
//! The data-path tests — materialization and slug resolution — run against
//! the PostgreSQL catalog authority in `store::glossary::postgres_tests`.
//! The load-bearing test here stays engine-shaped on purpose. A contract's
//! **defined terms** are matter data — per-project, never seeded, covered
//! by the no-client-data rule — and the **firm glossary** is universal
//! reference data. If they ever shared a table, firm vocabulary would be
//! polluted with client content and unwinding it would be very hard. So
//! this suite does not merely assert that no matter-scoped rows are
//! present today; it introspects the applied engine schema and asserts the
//! table has no field one could be written into.

use store::surreal::SurrealDb;

async fn mem() -> SurrealDb {
    store::test_support::mem_surreal().await
}

/// **A contract's defined terms must never share this table.** Asserting
/// "no matter-scoped rows exist" would only describe today's data; this
/// introspects the applied schema, so a later schema edit cannot quietly
/// add the field that would make the mixture possible.
#[tokio::test]
async fn the_glossary_table_cannot_hold_matter_scoped_rows() {
    let db = mem().await;

    let introspection = store::schema::introspect(&db).await.expect("introspect");
    let table = introspection
        .get("glossary_term")
        .expect("the applied schema defines glossary_term");

    let mut fields: Vec<&str> = table.fields.keys().map(String::as_str).collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        vec!["body", "inserted_at", "slug", "title", "updated_at"],
        "glossary_term carries firm vocabulary and nothing else"
    );
    for scoped in ["project_id", "person_id", "entity_id", "notation_id"] {
        assert!(
            !table.fields.contains_key(scoped),
            "`{scoped}` would let a contract's defined term be filed as firm vocabulary"
        );
    }

    // Nor may it reach a matter-scoped table by reference: a `record<T>`
    // field is Surreal's foreign key, and this table must define none.
    for (name, definition) in &table.fields {
        assert!(
            !definition.contains("record<"),
            "field `{name}` links to another table: {definition}"
        );
    }
}
