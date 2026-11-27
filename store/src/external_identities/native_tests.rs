use super::{
    find_by_account, find_by_person_and_system, find_or_link, find_person_by_account, for_person,
    link, notion_user_ids_for_project_leads, set_for_person, unlink, ExternalIdentityError,
    ExternalSystem,
};
use crate::git_access_tokens;
use crate::persons::{self, NewPerson, Role};
use crate::test_support::uninitialized_store;
use uuid::Uuid;

async fn a_person(email: &str, role: Role) -> Uuid {
    let db = uninitialized_store();
    persons::create(&db, &NewPerson::with_role("Scorpio", email, role))
        .await
        .unwrap()
        .id
}

/// The vocabulary is closed, and it fails closed. There is no
/// `Other(String)` to fall into, so a system Navigator cannot name
/// is refused here rather than reaching a call site as a value that
/// looks legitimate.
#[test]
fn parse_refuses_a_system_the_vocabulary_does_not_name() {
    for unknown in ["", "GitHub", "github ", "gitlab", "bitbucket", "xero"] {
        assert_eq!(
            ExternalSystem::parse(unknown),
            None,
            "{unknown:?} is not a system this build knows"
        );
    }
}

#[test]
fn every_system_round_trips_through_its_stored_spelling() {
    for system in ExternalSystem::ALL {
        assert_eq!(ExternalSystem::parse(system.as_str()), Some(*system));
    }
}

#[tokio::test]
async fn a_linked_identity_reads_back_by_person_and_by_account() {
    crate::test_support::with_required_postgres(async {
        let db = uninitialized_store();
        let person_id = a_person("scorpio@example.com", Role::Lawyer).await;

        let linked = link(
            &db,
            person_id,
            ExternalSystem::Github,
            "12345",
            Some("scorpio"),
        )
        .await
        .unwrap();

        assert_eq!(linked.person_id, person_id);
        assert_eq!(linked.system, ExternalSystem::Github);
        assert_eq!(linked.external_id, "12345");
        assert_eq!(linked.handle.as_deref(), Some("scorpio"));

        assert_eq!(
            find_by_person_and_system(&db, person_id, ExternalSystem::Github)
                .await
                .unwrap(),
            Some(linked.clone())
        );
        assert_eq!(
            find_by_account(&db, ExternalSystem::Github, "12345")
                .await
                .unwrap(),
            Some(linked.clone()),
            "the reverse lookup is what a provisioning call resolves through"
        );
        assert_eq!(for_person(&db, person_id).await.unwrap(), vec![linked]);
    })
    .await;
}

#[tokio::test]
async fn notion_user_ids_resolve_to_people_by_stable_id_only() {
    crate::test_support::with_required_postgres(async {
        let db = uninitialized_store();
        let person_id = a_person("notion@example.com", Role::Lawyer).await;
        link(
            &db,
            person_id,
            ExternalSystem::Notion,
            "notion-user-123",
            Some("Renamed"),
        )
        .await
        .unwrap();

        assert_eq!(
            find_person_by_account(&db, ExternalSystem::Notion, "notion-user-123")
                .await
                .unwrap()
                .unwrap()
                .id,
            person_id
        );
        assert!(
            find_person_by_account(&db, ExternalSystem::Notion, "Renamed")
                .await
                .unwrap()
                .is_none(),
            "a Notion display name is never a relation key"
        );
        assert!(
            find_person_by_account(&db, ExternalSystem::Notion, "notion@example.com")
                .await
                .unwrap()
                .is_none(),
            "a Person email is never a Notion relation key"
        );
    })
    .await;
}

#[tokio::test]
async fn notion_identity_correction_preserves_uniqueness_and_supports_clearing() {
    crate::test_support::with_required_postgres(async {
        let db = uninitialized_store();
        let first = a_person("first@example.com", Role::Lawyer).await;
        let second = a_person("second@example.com", Role::Lawyer).await;
        set_for_person(&db, first, ExternalSystem::Notion, Some("notion-1"))
            .await
            .unwrap();

        let duplicate = set_for_person(&db, second, ExternalSystem::Notion, Some("notion-1")).await;
        assert!(matches!(
            duplicate,
            Err(ExternalIdentityError::AccountTaken {
                system: ExternalSystem::Notion
            })
        ));

        set_for_person(&db, first, ExternalSystem::Notion, Some("notion-2"))
            .await
            .unwrap();
        assert!(find_by_account(&db, ExternalSystem::Notion, "notion-1")
            .await
            .unwrap()
            .is_none());
        set_for_person(&db, first, ExternalSystem::Notion, None)
            .await
            .unwrap();
        assert!(
            find_by_person_and_system(&db, first, ExternalSystem::Notion)
                .await
                .unwrap()
                .is_none()
        );
    })
    .await;
}

#[tokio::test]
async fn project_lead_notion_ids_include_every_attorney_dri() {
    crate::test_support::with_required_postgres(async {
        let db = uninitialized_store();
        let first = a_person("first@example.com", Role::Lawyer).await;
        let second = a_person("second@example.com", Role::Admin).await;
        let unrelated = a_person("unrelated@example.com", Role::Lawyer).await;
        let project =
            crate::test_support::seed_project_on_firm(&db, None, "Sample Notion Leads").await;
        let project_id = project.id;
        let pg = crate::postgres::PostgresDb::application().await.unwrap();
        for person_id in [first, second] {
            crate::postgres::participation::designate_dri_on(
                &pg,
                project_id,
                person_id,
                crate::projects::DriSide::Lawyer,
            )
            .await
            .unwrap();
        }
        link(&db, first, ExternalSystem::Notion, "notion-z", None)
            .await
            .unwrap();
        link(&db, second, ExternalSystem::Notion, "notion-a", None)
            .await
            .unwrap();
        link(
            &db,
            unrelated,
            ExternalSystem::Notion,
            "notion-unrelated",
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            notion_user_ids_for_project_leads(&db, project_id)
                .await
                .unwrap(),
            ["notion-a", "notion-z"]
        );
    })
    .await;
}

/// The immutable id is the key and the handle is not, so a rename
/// leaves the mapping intact — the failure mode `github_handle` had.
#[tokio::test]
async fn the_account_lookup_keys_on_the_id_and_not_the_handle() {
    crate::test_support::with_required_postgres(async {
        let db = uninitialized_store();
        let person_id = a_person("scorpio@example.com", Role::Lawyer).await;
        link(
            &db,
            person_id,
            ExternalSystem::Github,
            "12345",
            Some("old-handle"),
        )
        .await
        .unwrap();

        assert!(find_by_account(&db, ExternalSystem::Github, "12345")
            .await
            .unwrap()
            .is_some());
        assert!(
            find_by_account(&db, ExternalSystem::Github, "old-handle")
                .await
                .unwrap()
                .is_none(),
            "the handle is display only and must not resolve an account"
        );
    })
    .await;
}

/// The `(system, external_id)` unique: two Persons cannot claim one
/// account.
#[tokio::test]
async fn a_second_person_claiming_one_account_is_refused() {
    crate::test_support::with_required_postgres(async {
        let db = uninitialized_store();
        let first = a_person("first@example.com", Role::Lawyer).await;
        let second = a_person("second@example.com", Role::Lawyer).await;

        link(&db, first, ExternalSystem::Github, "12345", None)
            .await
            .unwrap();
        let duplicate = link(&db, second, ExternalSystem::Github, "12345", None).await;

        assert!(
            matches!(
                duplicate,
                Err(ExternalIdentityError::AccountTaken {
                    system: ExternalSystem::Github
                })
            ),
            "the unique system/account constraint is the gate, got {duplicate:?}"
        );
        assert!(for_person(&db, second).await.unwrap().is_empty());
    })
    .await;
}

/// The `(person_id, system)` unique: one Person holds at most one
/// account per system.
#[tokio::test]
async fn a_second_account_for_one_person_on_one_system_is_refused() {
    crate::test_support::with_required_postgres(async {
        let db = uninitialized_store();
        let person_id = a_person("scorpio@example.com", Role::Lawyer).await;

        link(&db, person_id, ExternalSystem::Github, "12345", None)
            .await
            .unwrap();
        let duplicate = link(&db, person_id, ExternalSystem::Github, "67890", None).await;

        assert!(
            matches!(
                duplicate,
                Err(ExternalIdentityError::SystemAlreadyLinked {
                    system: ExternalSystem::Github
                })
            ),
            "the unique person/system constraint is the gate, \
             got {duplicate:?}"
        );
        assert_eq!(for_person(&db, person_id).await.unwrap().len(), 1);
    })
    .await;
}

#[tokio::test]
async fn one_person_may_hold_an_account_on_every_system() {
    crate::test_support::with_required_postgres(async {
        let db = uninitialized_store();
        let person_id = a_person("scorpio@example.com", Role::Lawyer).await;

        for (index, system) in ExternalSystem::ALL.iter().enumerate() {
            link(&db, person_id, *system, &format!("id-{index}"), None)
                .await
                .unwrap();
        }

        assert_eq!(
            for_person(&db, person_id).await.unwrap().len(),
            ExternalSystem::ALL.len()
        );
    })
    .await;
}

#[tokio::test]
async fn an_identity_for_a_person_who_does_not_exist_is_refused() {
    crate::test_support::with_required_postgres(async {
        let db = uninitialized_store();
        let nobody = Uuid::now_v7();

        // The accessor identifies the missing Person before the foreign key rejects it.
        let refused = link(&db, nobody, ExternalSystem::Github, "12345", None).await;
        assert!(matches!(
            refused,
            Err(ExternalIdentityError::NoSuchPerson(id)) if id == nobody
        ));
    })
    .await;
}

#[tokio::test]
async fn find_or_link_is_idempotent_on_the_person_system_pair() {
    crate::test_support::with_required_postgres(async {
        let db = uninitialized_store();
        let person_id = a_person("scorpio@example.com", Role::Lawyer).await;

        let first = find_or_link(&db, person_id, ExternalSystem::Slack, "U123", Some("s"))
            .await
            .unwrap();
        let second = find_or_link(&db, person_id, ExternalSystem::Slack, "U999", Some("t"))
            .await
            .unwrap();

        assert_eq!(first, second, "the second call returns the existing row");
        assert_eq!(
            second.external_id, "U123",
            "an existing identity is returned as it stands, not repointed"
        );
        assert_eq!(for_person(&db, person_id).await.unwrap().len(), 1);
    })
    .await;
}

/// An account another person holds is a real conflict, not a race,
/// so find-or-link must surface it rather than swallow it the way it
/// swallows its own index collision.
#[tokio::test]
async fn find_or_link_still_reports_an_account_another_person_holds() {
    crate::test_support::with_required_postgres(async {
        let db = uninitialized_store();
        let first = a_person("first@example.com", Role::Lawyer).await;
        let second = a_person("second@example.com", Role::Lawyer).await;
        link(&db, first, ExternalSystem::Github, "12345", None)
            .await
            .unwrap();

        let refused = find_or_link(&db, second, ExternalSystem::Github, "12345", None).await;
        assert!(matches!(
            refused,
            Err(ExternalIdentityError::AccountTaken {
                system: ExternalSystem::Github
            })
        ));
    })
    .await;
}

#[tokio::test]
async fn an_identity_is_scoped_to_its_own_person() {
    crate::test_support::with_required_postgres(async {
        let db = uninitialized_store();
        let linked = a_person("linked@example.com", Role::Lawyer).await;
        let unlinked = a_person("unlinked@example.com", Role::Lawyer).await;
        link(&db, linked, ExternalSystem::Github, "12345", None)
            .await
            .unwrap();

        assert!(for_person(&db, unlinked).await.unwrap().is_empty());
        assert_eq!(
            find_by_person_and_system(&db, unlinked, ExternalSystem::Github)
                .await
                .unwrap(),
            None
        );
    })
    .await;
}

#[tokio::test]
async fn unlinking_forgets_the_identity_and_is_idempotent() {
    crate::test_support::with_required_postgres(async {
        let db = uninitialized_store();
        let person_id = a_person("scorpio@example.com", Role::Lawyer).await;
        let linked = link(&db, person_id, ExternalSystem::Github, "12345", None)
            .await
            .unwrap();

        unlink(&db, linked.id).await.unwrap();
        assert!(for_person(&db, person_id).await.unwrap().is_empty());

        // Unlinking again, and unlinking one that never existed, are
        // no-ops.
        unlink(&db, linked.id).await.unwrap();
        unlink(&db, Uuid::now_v7()).await.unwrap();
    })
    .await;
}

/// An account released by one person can be recorded for another —
/// the unique index must not keep the pair reserved after the row is
/// gone.
#[tokio::test]
async fn a_released_account_can_be_claimed_by_someone_else() {
    crate::test_support::with_required_postgres(async {
        let db = uninitialized_store();
        let leaver = a_person("leaver@example.com", Role::Lawyer).await;
        let joiner = a_person("joiner@example.com", Role::Lawyer).await;

        let held = link(&db, leaver, ExternalSystem::Github, "12345", None)
            .await
            .unwrap();
        unlink(&db, held.id).await.unwrap();

        let reclaimed = link(&db, joiner, ExternalSystem::Github, "12345", None)
            .await
            .unwrap();
        assert_eq!(reclaimed.person_id, joiner);
    })
    .await;
}

/// The glossary rule, held as behaviour: a Clerk "never receives
/// lawyer-work, advice, Git, MCP, or `/app/lawyer` authority by
/// inheritance." Recording that a Clerk *is* a GitHub user is a
/// record of who they are; it is not a grant, and every authority
/// answer is the same before and after the row exists.
#[tokio::test]
async fn a_clerk_carrying_a_github_identity_is_granted_no_git_authority() {
    crate::test_support::with_required_postgres(async {
        let db = uninitialized_store();
        let clerk = a_person("clerk@example.com", Role::Clerk).await;

        // The Git transport's own authority is a token, and a Clerk who
        // was never minted one has none — before the identity, and
        // after it.
        let before = git_access_tokens::validate(&db, "not-a-token", chrono::Utc::now())
            .await
            .unwrap();
        assert!(before.is_none());

        let identity = link(
            &db,
            clerk,
            ExternalSystem::Github,
            "12345",
            Some("clerk-handle"),
        )
        .await
        .unwrap();

        assert!(
            git_access_tokens::validate(&db, "not-a-token", chrono::Utc::now())
                .await
                .unwrap()
                .is_none(),
            "an external identity is not a Git credential"
        );
        assert!(
            git_access_tokens::validate(&db, &identity.external_id, chrono::Utc::now())
                .await
                .unwrap()
                .is_none(),
            "the account id must not authenticate anything"
        );

        // And the tier is untouched: the row records who they are, it
        // does not promote them.
        assert_eq!(
            persons::find_by_id(&db, clerk).await.unwrap().unwrap().role,
            Role::Clerk
        );
    })
    .await;
}

/// The other half of the same rule, from ENG-45 and
/// `docs/project-repositories.md`: Project participation never grants
/// GHE access, and an identity does not *become* participation. A
/// `client` holding a `google` identity for Drive sharing is
/// legitimate, and scope still comes from `person_project_role`
/// alone.
#[tokio::test]
async fn linking_an_identity_creates_no_participation() {
    crate::test_support::with_required_postgres(async {
        let db = uninitialized_store();
        let client = a_person("client@example.com", Role::Client).await;

        link(&db, client, ExternalSystem::Google, "sub-12345", None)
            .await
            .unwrap();

        let pg = crate::postgres::PostgresDb::application().await.unwrap();
        let participations: i64 =
            sqlx::query_scalar("SELECT count(*) FROM person_project_role WHERE person_id = $1")
                .bind(client)
                .fetch_one(pg.pool())
                .await
                .unwrap();
        assert_eq!(
            participations, 0,
            "an identity row creates no participation"
        );

        // One row, in one table, and it is this one.
        assert_eq!(for_person(&db, client).await.unwrap().len(), 1);
    })
    .await;
}
