//! Synthetic fixtures for this crate's tests.
//!
//! Every test runs inside `store::test_support::with_postgres`, so the store
//! accessors answer from PostgreSQL. The Source handle the accessors still
//! take is one no engine answers: a test that reached `SurrealDB` would fail.
//! Identities come from `fake`, so no fixture names a person by hand.

use fake::faker::company::en::CompanyName;
use fake::faker::name::en::{FirstName, LastName};
use fake::Fake as _;
use std::sync::LazyLock;
use uuid::Uuid;

/// One generated matter name for the test process, so an assertion can name
/// what a fixture wrote.
pub static MATTER: LazyLock<String> = LazyLock::new(surname);
/// A second generated matter name, never equal to [`MATTER`].
pub static ESTATE: LazyLock<String> = LazyLock::new(|| format!("{} Estate", surname()));
/// One generated organization name for the test process.
pub static ORGANIZATION: LazyLock<String> = LazyLock::new(organization_name);

/// The Source handle: connected to nothing.
pub fn handle() -> store::application_store::ApplicationStore {
    store::test_support::uninitialized_store()
}

/// A generated full name.
pub fn person_name() -> String {
    format!(
        "{} {}",
        FirstName().fake::<String>(),
        LastName().fake::<String>()
    )
}

/// A generated surname, the usual short name of a matter.
pub fn surname() -> String {
    LastName().fake()
}

/// A generated organization name.
pub fn organization_name() -> String {
    CompanyName().fake()
}

/// A unique address on a reserved domain.
pub fn email() -> String {
    format!("{}@example.test", Uuid::new_v4().simple())
}

/// Register the compiled Brands, which opening a Project requires.
pub async fn seed_brands(db: &store::application_store::ApplicationStore) {
    store::test_support::seed_compiled_brands_postgres(db).await;
}

/// An Entity named `name` whose Entity Type and Jurisdiction rows exist.
pub async fn seed_entity_named(
    db: &store::application_store::ApplicationStore,
    name: &str,
) -> Uuid {
    let (entity_type_id, jurisdiction_id) = store::test_support::seed_entity_parents(db).await;
    store::entities::create(
        db,
        &store::entities::NewEntity {
            name: name.into(),
            entity_type_id,
            jurisdiction_id,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        },
    )
    .await
    .expect("seed entity")
    .id
}
