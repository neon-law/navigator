//! Canonical bearer-token builders for the web integration tests.
//!
//! Every helper signs a fresh [`portal::SessionData`] with
//! [`portal::test_support::TEST_SESSION_KEY`] — the same key the shared
//! app-state builder uses — so a token minted here is accepted by any app
//! whose `SessionStore` is keyed with that constant. Fixture `sub` values
//! are not asserted anywhere, so one shared subject suffices.
//!
//! Each test binary compiles this file into itself, and not every binary
//! uses every helper — so the module carries a scoped `dead_code` allow.

#![allow(dead_code)]

use portal::{SessionData, SessionStore};
use store::persons::Role;
use uuid::Uuid;

const TEST_SESSION_SUB: &str = "test-session-sub";

/// Bearer header for a session bound to an existing person.
pub fn bearer(person_id: Uuid, role: Role) -> String {
    bearer_opt(Some(person_id), role)
}

/// Bearer header for a session whose `person_id` may be absent — the
/// firm-admin shape, where the session carries a role but no person row.
pub fn bearer_opt(person_id: Option<Uuid>, role: Role) -> String {
    let mut session = SessionData::fresh(TEST_SESSION_SUB, role);
    session.person_id = person_id;
    header(&session)
}

/// Bearer header for a freshly seeded person, optionally admitted to a
/// project under a named participation (usually `"lawyer"`).
pub async fn bearer_new_person(
    application_store: &store::application_store::ApplicationStore,
    email: &str,
    role: Role,
    project: Option<(Uuid, &str)>,
) -> String {
    let actor = store::persons::create(
        application_store,
        &store::persons::NewPerson::with_role(email.to_string(), email.to_string(), role),
    )
    .await
    .expect("seed the session person");
    if let Some((project_id, participation)) = project {
        store::projects::add_participation(application_store, project_id, actor.id, participation)
            .await
            .expect("admit the session person to the project");
    }
    bearer(actor.id, role)
}

fn header(session: &SessionData) -> String {
    format!(
        "Bearer {}",
        SessionStore::new(portal::test_support::TEST_SESSION_KEY).encode(session)
    )
}

/// Bearer for an Admin of the Project's Firm with no Project participation.
pub async fn bearer_project_firm_admin(
    application_store: &store::application_store::ApplicationStore,
    project_id: Uuid,
) -> String {
    let admin = store::persons::create(
        application_store,
        &store::persons::NewPerson::with_role("Firm Admin", "admin@example.com", Role::Admin),
    )
    .await
    .expect("seed the Firm Admin");
    store::test_support::grant_project_firm_admin(application_store, project_id, admin.id).await;
    bearer(admin.id, Role::Admin)
}
