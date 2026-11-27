//! PostgreSQL schema guards for Project repository metadata.
//!
//! Two invariants the schema must hold:
//!
//! 1. A freshly inserted Project starts with an uninitialized repo
//!    (`git_initialized_at = NULL`) — the bare repo is created lazily on
//!    first git access. The branch is always `main`, enforced by the repo's
//!    `pre-receive` hook and pinned in `repos::DEFAULT_BRANCH`, so there is
//!    no per-row branch column to assert.
//! 2. A `git_access_tokens` row round-trips: a Project-scoped, hashed
//!    PAT inserts and reads back with its Person, Project, scope, and hash intact.

#[path = "support/native_project.rs"]
mod native_project;

use chrono::{DateTime, Utc};
use store::persons::{self, NewPerson};

#[tokio::test]
async fn new_project_starts_with_uninitialized_repo() {
    store::test_support::with_postgres(async {
        let proj = native_project::create("git-schema").await;

        // Every Project is a single-branch (`main`) repo identity; the bare
        // repo itself is created lazily, so it starts uninitialized.
        assert_eq!(proj.git_initialized_at, None);
    })
    .await;
}

#[tokio::test]
async fn git_access_token_round_trips_scoped_to_a_project() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();

        let owner = persons::create(
            &application_store,
            &NewPerson::new("Libra", "libra@example.com"),
        )
        .await
        .expect("insert person");

        let proj = native_project::create("git-token-schema").await;

        let fetched = store::git_access_tokens::mint(
            &application_store,
            owner.id,
            Some(proj.id),
            store::git_access_tokens::SCOPE_WRITE,
            "the-secret",
            DateTime::parse_from_rfc3339("2099-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        )
        .await
        .expect("insert git access token");

        assert_eq!(fetched.person_id, owner.id);
        assert_eq!(fetched.project_id, Some(proj.id));
        assert_eq!(fetched.scope, store::git_access_tokens::SCOPE_WRITE);
        assert_eq!(fetched.token_hash.len(), 64);
    })
    .await;
}
