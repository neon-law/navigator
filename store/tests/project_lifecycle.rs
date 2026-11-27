//! Matter lifecycle transitions (#770): `open` → `closed` → `archived`,
//! and the `status` / `closed_at` invariant they exist to own.
//!
//! The invariant is the whole point. Open-matter routing keys off
//! `status` while the ten-year retention purge keys off `closed_at`, so a
//! row where the two disagree is routed as open while retention treats it
//! as closed, or the reverse. These tests assert the pair together after
//! every transition, never `status` alone.

#[path = "support/native_project.rs"]
mod native_project;

use store::projects::{
    transition_project, transition_project_with_reason, ClosureReason, Project,
    ProjectCommandError, Transition,
};
use store::test_support::{dri_person, seed_signed_onboarding};
use uuid::Uuid;

fn at(value: &str) -> chrono::DateTime<chrono::Utc> {
    value.parse().expect("valid RFC 3339 test time")
}

async fn open_matter(_: &store::application_store::ApplicationStore, code: &str) -> Uuid {
    native_project::create(code).await.id
}

/// Supply valid reopening attestation before exercising lifecycle retention rules.
async fn reopen(
    application_store: &store::application_store::ApplicationStore,
    id: Uuid,
    effective_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<Project, ProjectCommandError> {
    let lawyer = store::persons::create(
        application_store,
        &store::persons::NewPerson::with_role(
            "Synthetic attesting lawyer",
            "attesting-lawyer@example.com",
            store::persons::Role::Lawyer,
        ),
    )
    .await
    .expect("create attesting lawyer");
    store::projects::transition_project_attested(
        application_store,
        id,
        Transition::Reopen,
        None,
        Some(store::projects::ReopenReason::NewMatterEvent),
        effective_at,
        Some(lawyer.id),
    )
    .await
}

async fn reload(
    application_store: &store::application_store::ApplicationStore,
    id: Uuid,
) -> Project {
    store::projects::find_by_id(application_store, id)
        .await
        .expect("query")
        .expect("matter still exists")
}

async fn set_opened_at(_: &store::application_store::ApplicationStore, id: Uuid, opened_at: &str) {
    let pg = store::postgres::PostgresDb::application().await.unwrap();
    sqlx::query("UPDATE project SET inserted_at=$2 WHERE id=$1")
        .bind(id)
        .bind(at(opened_at))
        .execute(pg.pool())
        .await
        .unwrap();
}

async fn file_asset(_: &store::application_store::ApplicationStore, project_id: Uuid, kind: &str) {
    let pg = store::postgres::PostgresDb::application().await.unwrap();
    sqlx::query("INSERT INTO asset(id,project_id,storage_key,content_type,byte_size,sha256_hex,kind,visibility) VALUES ($1,$2,$3,'application/pdf',1,$4,$5,'internal')")
        .bind(Uuid::new_v4()).bind(project_id)
        .bind(format!("projects/{project_id}/documents/test"))
        .bind(store::assets::sha256_hex(format!("synthetic-{project_id}-{kind}").as_bytes()))
        .bind(kind).execute(pg.pool()).await.unwrap();
}

async fn close_active(
    application_store: &store::application_store::ApplicationStore,
    id: Uuid,
    effective_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<Project, ProjectCommandError> {
    seed_signed_onboarding(application_store, id, dri_person(application_store).await).await;
    transition_project_with_reason(
        application_store,
        id,
        Transition::Close,
        Some(ClosureReason::EngagementCompleted),
        effective_at,
    )
    .await
}

#[tokio::test]
async fn close_reasons_follow_the_pre_close_lifecycle_and_pitch_offboarding_gate() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let pitch = open_matter(&application_store, "pitch-reason").await;

        assert!(matches!(
            transition_project_with_reason(
                &application_store,
                pitch,
                Transition::Close,
                None,
                None,
            )
            .await,
            Err(ProjectCommandError::Invalid(_))
        ));
        file_asset(&application_store, pitch, "onboarding").await;
        assert!(matches!(
            transition_project_with_reason(
                &application_store,
                pitch,
                Transition::Close,
                Some(ClosureReason::EngagementCompleted),
                None,
            )
            .await,
            Err(ProjectCommandError::Invalid(_))
        ));

        file_asset(&application_store, pitch, "offboarding").await;
        let closed = transition_project_with_reason(
            &application_store,
            pitch,
            Transition::Close,
            Some(ClosureReason::PitchDeclined),
            None,
        )
        .await
        .expect("pitch close");
        assert_eq!(closed.closure_reason.as_deref(), Some("pitch_declined"));
    })
    .await;
}

#[tokio::test]
async fn close_reason_is_exposed_and_reopen_clears_it_without_touching_historical_rows() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let id = open_matter(&application_store, "reason-round-trip").await;
        seed_signed_onboarding(&application_store, id, dri_person(&application_store).await).await;

        let closed = transition_project_with_reason(
            &application_store,
            id,
            Transition::Close,
            Some(ClosureReason::ClientTerminated),
            None,
        )
        .await
        .expect("active close");
        assert_eq!(closed.closure_reason.as_deref(), Some("client_terminated"));

        let reopened = reopen(&application_store, id, None).await.expect("reopen");
        assert!(reopened.closure_reason.is_none());

        let historical = open_matter(&application_store, "historical-close").await;
        let pg = store::postgres::PostgresDb::application().await.unwrap();
        sqlx::query("UPDATE project SET status='closed',closed_at=$2 WHERE id=$1")
            .bind(historical)
            .bind(at("2001-02-03T04:05:06Z"))
            .execute(pg.pool())
            .await
            .expect("historical write");
        let unchanged = transition_project_with_reason(
            &application_store,
            historical,
            Transition::Close,
            Some(ClosureReason::PitchDeclined),
            None,
        )
        .await
        .expect("repeat historical close");
        assert!(unchanged.closure_reason.is_none());
    })
    .await;
}

/// The invariant, asserted as a pair: an `open` matter carries no close
/// date, and a `closed`/`archived` one carries exactly one.
fn assert_invariant(row: &Project) {
    match row.status.as_str() {
        "open" => assert!(
            row.closed_at.is_none(),
            "an open matter must carry no close date, got {:?}",
            row.closed_at
        ),
        "closed" | "archived" => assert!(
            row.closed_at.is_some(),
            "a {} matter must carry a close date — retention keys off it",
            row.status
        ),
        other => panic!("unexpected status {other}"),
    }
}

#[tokio::test]
async fn closing_stamps_the_close_date_that_starts_retention() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let id = open_matter(&application_store, "close-me").await;

        let closed = close_active(&application_store, id, None)
            .await
            .expect("close");
        assert_eq!(closed.status, "closed");
        assert!(closed.closed_at.is_some());
        assert_invariant(&closed);
    })
    .await;
}

#[tokio::test]
async fn an_effective_date_corrects_an_already_closed_matter() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let id = open_matter(&application_store, "correct-close").await;
        set_opened_at(&application_store, id, "2000-01-01T00:00:00Z").await;
        close_active(&application_store, id, None)
            .await
            .expect("initial close");

        let corrected = transition_project_with_reason(
            &application_store,
            id,
            Transition::Close,
            Some(ClosureReason::EngagementCompleted),
            Some(at("2001-02-03T04:05:06Z")),
        )
        .await
        .expect("correct close time");

        assert_eq!(corrected.status, "closed");
        assert_eq!(corrected.closed_at.as_deref(), Some("2001-02-03T04:05:06Z"));
        assert_invariant(&corrected);
    })
    .await;
}

#[tokio::test]
async fn archiving_accepts_an_effective_date() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let id = open_matter(&application_store, "effective-archive").await;
        set_opened_at(&application_store, id, "2000-01-01T00:00:00Z").await;

        let archived = transition_project(
            &application_store,
            id,
            Transition::Archive,
            Some(at("2001-02-03T04:05:06Z")),
        )
        .await
        .expect("archive at effective time");

        assert_eq!(archived.status, "archived");
        assert_eq!(archived.closed_at.as_deref(), Some("2001-02-03T04:05:06Z"));
        assert_invariant(&archived);
    })
    .await;
}

#[tokio::test]
async fn effective_dates_are_bounded_by_matter_open_and_now() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let id = open_matter(&application_store, "bounded-close").await;
        set_opened_at(&application_store, id, "2000-01-01T00:00:00Z").await;
        seed_signed_onboarding(&application_store, id, dri_person(&application_store).await).await;

        for (label, effective_at) in [
            ("before matter-open", "1999-12-31T23:59:59Z"),
            ("in the future", "9999-01-01T00:00:00Z"),
        ] {
            let error = transition_project_with_reason(
                &application_store,
                id,
                Transition::Close,
                Some(ClosureReason::EngagementCompleted),
                Some(at(effective_at)),
            )
            .await
            .expect_err(label);
            assert!(
                matches!(error, ProjectCommandError::Invalid(_)),
                "{error:?}"
            );
            let unchanged = reload(&application_store, id).await;
            assert_eq!(unchanged.status, "open", "{label}");
            assert!(unchanged.closed_at.is_none(), "{label}");
        }
    })
    .await;
}

#[tokio::test]
async fn attested_reopening_records_a_backdated_effective_date_without_retention() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let id = open_matter(&application_store, "dated-reopen").await;
        set_opened_at(&application_store, id, "2000-01-01T00:00:00Z").await;
        close_active(&application_store, id, None)
            .await
            .expect("close");

        let effective = at("2001-02-03T04:05:06Z");
        let reopened = reopen(&application_store, id, Some(effective))
            .await
            .expect("attested backdated reopen");
        let (current, history) = store::projects::status_with_history(&application_store, reopened)
            .await
            .expect("read committed lifecycle history");
        assert_eq!(current.status, "open");
        assert!(current.closed_at.is_none());
        assert_invariant(&current);
        let event = history.last().expect("recorded reopening");
        assert_eq!(event.from_status.as_deref(), Some("closed"));
        assert_eq!(event.to_status, "open");
        assert_eq!(event.effective_at, Some(effective));
        assert!(event
            .recorded_at
            .is_some_and(|recorded| recorded > effective));
        assert_eq!(event.reason.as_deref(), Some("new_matter_event"));
        let lawyer =
            store::persons::find_by_email_ci(&application_store, "attesting-lawyer@example.com")
                .await
                .expect("read attesting lawyer")
                .expect("attesting lawyer exists");
        assert_eq!(event.attesting_person_id, Some(lawyer.id));
    })
    .await;
}

#[tokio::test]
async fn reopening_clears_the_close_date() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let id = open_matter(&application_store, "reopen-me").await;

        close_active(&application_store, id, None).await.expect("close");
        let reopened = reopen(&application_store, id, None)
            .await
            .expect("reopen");

        assert_eq!(reopened.status, "open");
        assert!(
            reopened.closed_at.is_none(),
            "a reopened matter that kept its close date would be purged by retention while routed as open"
        );
        assert_invariant(&reopened);
    })
    .await;
}

/// Archiving after a close keeps the date retention already knows about,
/// rather than restarting the ten-year window.
#[tokio::test]
async fn archiving_a_closed_matter_preserves_its_original_close_date() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let id = open_matter(&application_store, "archive-closed").await;

        let closed = close_active(&application_store, id, None)
            .await
            .expect("close");
        let original = closed.closed_at.clone().expect("stamped");

        let archived = transition_project(&application_store, id, Transition::Archive, None)
            .await
            .expect("archive");
        assert_eq!(archived.status, "archived");
        assert_eq!(
            archived.closed_at.as_deref(),
            Some(original.as_str()),
            "archiving must not restart the retention window"
        );
        assert_invariant(&archived);
    })
    .await;
}

/// Archiving straight from `open` still has to satisfy the invariant, so
/// it stamps a close date on the way.
#[tokio::test]
async fn archiving_an_open_matter_stamps_a_close_date() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let id = open_matter(&application_store, "archive-open").await;

        let archived = transition_project(&application_store, id, Transition::Archive, None)
            .await
            .expect("archive");
        assert_eq!(archived.status, "archived");
        assert!(
            archived.closed_at.is_some(),
            "an archived matter with no close date is invisible to the retention purge"
        );
        assert_invariant(&archived);
    })
    .await;
}

/// Archived is terminal. Reopening one would resurrect a matter whose
/// retention clock is already running.
#[tokio::test]
async fn archived_is_terminal() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let id = open_matter(&application_store, "terminal").await;
        transition_project(&application_store, id, Transition::Archive, None)
            .await
            .expect("archive");

        for forbidden in [Transition::Reopen, Transition::Close] {
            let result = if forbidden == Transition::Reopen {
                reopen(&application_store, id, None).await
            } else {
                transition_project(&application_store, id, forbidden, None).await
            };
            let err = result.expect_err("archived refuses this transition");
            assert!(
                matches!(err, ProjectCommandError::Invalid(_)),
                "expected an Invalid transition, got {err:?}"
            );
        }

        let row = reload(&application_store, id).await;
        assert_eq!(row.status, "archived", "the refused calls changed nothing");
        assert_invariant(&row);
    })
    .await;
}

/// Re-applying a transition the matter already made is a no-op, so a
/// double-submitted lawyer form does not churn the row or restamp a date.
#[tokio::test]
async fn re_applying_a_transition_is_a_no_op() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let id = open_matter(&application_store, "idempotent").await;

        let first = close_active(&application_store, id, None)
            .await
            .expect("close");
        let stamp = first.closed_at.clone().expect("stamped");

        let second = transition_project_with_reason(
            &application_store,
            id,
            Transition::Close,
            Some(ClosureReason::EngagementCompleted),
            None,
        )
        .await
        .expect("close again");
        assert_eq!(
            second.closed_at.as_deref(),
            Some(stamp.as_str()),
            "a second close must not restamp the retention start"
        );

        // Re-archiving is the one no-op permitted out of the terminal state.
        transition_project(&application_store, id, Transition::Archive, None)
            .await
            .expect("archive");
        let again = transition_project(&application_store, id, Transition::Archive, None)
            .await
            .expect("re-archive is a no-op, not a failure");
        assert_eq!(again.status, "archived");
        assert_invariant(&again);
    })
    .await;
}

/// A full round trip: closing after a reopen starts a *fresh* retention
/// window, because the reopen cleared the old one.
#[tokio::test]
async fn closing_after_a_reopen_starts_a_fresh_retention_window() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let id = open_matter(&application_store, "round-trip").await;

        let first = close_active(&application_store, id, None).await.expect("close");
        let first_stamp = first.closed_at.clone().expect("stamped");

        let reopened = reopen(&application_store, id, None)
            .await
            .expect("reopen");
        // The load-bearing step: the reopen actually cleared the stamp, so
        // the next close cannot inherit it. Asserted directly rather than
        // inferred from two clock reads differing.
        assert!(
            reopened.closed_at.is_none(),
            "the reopen must clear the date, or the next close preserves the old window"
        );

        let second = transition_project_with_reason(
            &application_store,
            id,
            Transition::Close,
            Some(ClosureReason::EngagementCompleted),
            None,
        )
        .await
        .expect("close again");
        let second_stamp = second.closed_at.clone().expect("stamped");

        assert!(
            second_stamp >= first_stamp,
            "the fresh window cannot start before the original close: {second_stamp} < {first_stamp}"
        );
        assert_invariant(&second);
    })
    .await;
}

#[tokio::test]
async fn an_unknown_matter_is_not_found() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let err = transition_project(&application_store, Uuid::now_v7(), Transition::Close, None)
            .await
            .expect_err("unknown id");
        assert!(matches!(err, ProjectCommandError::NotFound), "{err:?}");
    })
    .await;
}

/// Every transition lands on the status it names — the mapping the API
/// verbs will key off.
#[test]
fn each_transition_names_its_target_status() {
    assert_eq!(Transition::Close.target_status(), "closed");
    assert_eq!(Transition::Reopen.target_status(), "open");
    assert_eq!(Transition::Archive.target_status(), "archived");
}
