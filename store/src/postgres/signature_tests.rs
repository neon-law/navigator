//! The provider-envelope authority's tests, against the PostgreSQL fixture.
//! Each test fails when `NAVIGATOR_TEST_POSTGRES_PORT` is unset.
use super::*;
use crate::notarizations;
use crate::postgres_mirror::tests::{connect, fresh_schema, notation_fixture};
use crate::signatures::{
    by_provider, completed_for_notation, completed_for_notations, for_notation,
    latest_for_notation, record_request, record_signer_completion, stamp_declined, stamp_expired,
    stamp_signed, stamp_voided, SignatureError, SignatureProvider, SignatureState,
};
use crate::surreal::SurrealDb;
use crate::verifications::{self, axis_status, stale_after_revision};
use rules::citation::{Axis, AxisStatus};
use uuid::Uuid;

async fn database() -> Option<PostgresDb> {
    let (options, database) = fresh_schema().await?;
    Some(connect(&options, &database).await)
}

/// The engine the SurrealDB fallback would need. Every dispatched path in
/// this family reads and writes PostgreSQL only, so the handle stays
/// uninitialized: touching it would panic, which is the point.
fn surreal() -> SurrealDb {
    SurrealDb::uninitialized()
}

#[tokio::test]
async fn signature_record_request_is_idempotent_in_postgres() {
    let Some((pg, _, notation_id)) = notation_fixture().await else {
        return;
    };
    let db = surreal();

    with_test_db(pg, async {
        let first = record_request(&db, notation_id, SignatureProvider::DocuSign, "env-1")
            .await
            .unwrap();
        assert_eq!(first.state, SignatureState::Requested);
        let again = record_request(&db, notation_id, SignatureProvider::DocuSign, "env-1")
            .await
            .unwrap();
        assert_eq!(first.id, again.id);

        let latest = latest_for_notation(&db, notation_id)
            .await
            .unwrap()
            .expect("the envelope exists");
        assert_eq!(latest.id, first.id);
        let all = for_notation(&db, notation_id).await.unwrap();
        assert_eq!(all.len(), 1);

        assert!(completed_for_notation(&db, notation_id)
            .await
            .unwrap()
            .is_none());
        let batch = completed_for_notations(&db, &[notation_id, Uuid::new_v4()])
            .await
            .unwrap();
        assert!(batch.is_empty());
    })
    .await;
}

#[tokio::test]
async fn signature_stamp_signed_marks_completion_and_is_a_no_op_for_unknown_envelopes() {
    let Some((pg, _, notation_id)) = notation_fixture().await else {
        return;
    };
    let db = surreal();

    with_test_db(pg, async {
        record_request(&db, notation_id, SignatureProvider::DocuSign, "env-1")
            .await
            .unwrap();
        assert!(!stamp_signed(
            &db,
            SignatureProvider::DocuSign,
            "unknown",
            "2026-06-30T00:00:00Z"
        )
        .await
        .unwrap());
        assert!(stamp_signed(
            &db,
            SignatureProvider::DocuSign,
            "env-1",
            "2026-06-30T00:00:00Z"
        )
        .await
        .unwrap());
        assert!(stamp_signed(
            &db,
            SignatureProvider::DocuSign,
            "env-1",
            "2026-06-30T00:00:01Z"
        )
        .await
        .unwrap());
        let after = by_provider(&db, SignatureProvider::DocuSign, "env-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.signed_at.as_deref(), Some("2026-06-30T00:00:00Z"));
        assert_eq!(after.state, SignatureState::Completed);
    })
    .await;
}

#[tokio::test]
async fn signer_completion_records_each_role_once_while_the_envelope_is_live() {
    let Some((pg, _, notation_id)) = notation_fixture().await else {
        return;
    };
    let db = surreal();

    with_test_db(pg, async {
        assert!(
            !record_signer_completion(&db, SignatureProvider::DocuSign, "env-1", "client")
                .await
                .unwrap(),
            "an unknown envelope records nothing"
        );
        record_request(&db, notation_id, SignatureProvider::DocuSign, "env-1")
            .await
            .unwrap();

        for _ in 0..2 {
            assert!(
                record_signer_completion(&db, SignatureProvider::DocuSign, "env-1", "client")
                    .await
                    .unwrap()
            );
        }
        let live = latest_for_notation(&db, notation_id)
            .await
            .unwrap()
            .expect("the envelope exists");
        assert_eq!(live.state, SignatureState::Requested);
        assert_eq!(live.completed_signer_roles, vec!["client".to_string()]);

        stamp_voided(&db, SignatureProvider::DocuSign, "env-1")
            .await
            .unwrap();
        assert!(
            !record_signer_completion(&db, SignatureProvider::DocuSign, "env-1", "firm")
                .await
                .unwrap(),
            "a terminal envelope takes no further signer completions"
        );
        let voided = latest_for_notation(&db, notation_id)
            .await
            .unwrap()
            .expect("the envelope exists");
        assert_eq!(voided.completed_signer_roles, vec!["client".to_string()]);
    })
    .await;
}

#[tokio::test]
async fn terminal_stamps_are_readable_but_not_completed_and_monotonic() {
    let Some((pg, _, notation_id)) = notation_fixture().await else {
        return;
    };
    let db = surreal();

    with_test_db(pg, async {
        for (provider_id, state) in [
            ("declined", SignatureState::Declined),
            ("voided", SignatureState::Voided),
            ("expired", SignatureState::Expired),
        ] {
            record_request(&db, notation_id, SignatureProvider::DocuSign, provider_id)
                .await
                .unwrap();
            let stamped = match state {
                SignatureState::Declined => {
                    stamp_declined(&db, SignatureProvider::DocuSign, provider_id).await
                }
                SignatureState::Voided => {
                    stamp_voided(&db, SignatureProvider::DocuSign, provider_id).await
                }
                SignatureState::Expired => {
                    stamp_expired(&db, SignatureProvider::DocuSign, provider_id).await
                }
                _ => unreachable!("the loop only stamps terminal states"),
            };
            assert!(stamped.unwrap());
            assert_eq!(
                by_provider(&db, SignatureProvider::DocuSign, provider_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .state,
                state
            );
            assert!(completed_for_notation(&db, notation_id)
                .await
                .unwrap()
                .is_none());
            // Terminal states are monotonic: nothing re-opens them.
            let re_stamped = match state {
                SignatureState::Declined => {
                    stamp_declined(&db, SignatureProvider::DocuSign, provider_id).await
                }
                SignatureState::Voided => {
                    stamp_voided(&db, SignatureProvider::DocuSign, provider_id).await
                }
                SignatureState::Expired => {
                    stamp_expired(&db, SignatureProvider::DocuSign, provider_id).await
                }
                _ => unreachable!("the loop only stamps terminal states"),
            };
            assert!(!re_stamped.unwrap());
        }
    })
    .await;
}

#[tokio::test]
async fn a_provider_timestamp_that_does_not_parse_is_refused() {
    let Some((pg, _, notation_id)) = notation_fixture().await else {
        return;
    };
    let db = surreal();

    with_test_db(pg, async {
        record_request(&db, notation_id, SignatureProvider::DocuSign, "env-1")
            .await
            .unwrap();
        let error = stamp_signed(&db, SignatureProvider::DocuSign, "env-1", "not a timestamp")
            .await
            .unwrap_err();
        assert!(
            matches!(error, SignatureError::InvalidTimestamp(_)),
            "{error:?}"
        );
    })
    .await;
}

#[tokio::test]
async fn verification_records_unverified_axes_and_transitions_stale_in_postgres() {
    let Some(pg) = database().await else { return };
    let db = surreal();

    with_test_db(pg, async {
        let citation_id = Uuid::new_v4();
        let person_id = Uuid::new_v4();

        let v = verifications::record(&db, citation_id, "revision-one", person_id)
            .await
            .unwrap();
        for axis in Axis::ALL {
            assert_eq!(
                axis_status(&v, *axis).unwrap(),
                AxisStatus::Unverified,
                "{} must not seed as passing",
                axis.as_str()
            );
        }

        verifications::set_axis(&db, v.id, Axis::Citation, AxisStatus::Verified)
            .await
            .unwrap();
        verifications::set_axis(&db, v.id, Axis::Quote, AxisStatus::Verified)
            .await
            .unwrap();
        verifications::set_axis(&db, v.id, Axis::Proposition, AxisStatus::Rejected)
            .await
            .unwrap();

        let transitions = stale_after_revision(&db, citation_id, "revision-two")
            .await
            .unwrap();
        assert_eq!(transitions.len(), 3);
        for t in &transitions {
            assert_eq!(t.verification_id, v.id);
            assert_eq!(t.from_revision_sha, "revision-one");
        }

        let after = verifications::for_citation(&db, citation_id).await.unwrap();
        for axis in Axis::ALL {
            assert_eq!(axis_status(&after[0], *axis).unwrap(), AxisStatus::Stale);
        }

        // A verification of the current revision is untouched.
        let current = verifications::record(&db, citation_id, "revision-two", person_id)
            .await
            .unwrap();
        verifications::set_axis(&db, current.id, Axis::Quote, AxisStatus::Verified)
            .await
            .unwrap();
        assert!(stale_after_revision(&db, citation_id, "revision-two")
            .await
            .unwrap()
            .is_empty());
    })
    .await;
}

#[tokio::test]
async fn notarization_records_idempotently_stamps_and_names_its_asset_in_postgres() {
    let Some((pg, _, notation_id)) = notation_fixture().await else {
        return;
    };
    let db = surreal();

    with_test_db(pg, async {
        let first =
            notarizations::record_request(&db, notation_id, SignatureProvider::DocuSign, "nz-1")
                .await
                .unwrap();
        let again =
            notarizations::record_request(&db, notation_id, SignatureProvider::DocuSign, "nz-1")
                .await
                .unwrap();
        assert_eq!(first.id, again.id);

        assert!(notarizations::stamp_notarized(
            &db,
            SignatureProvider::DocuSign,
            "nz-1",
            "2026-06-30T00:00:00Z"
        )
        .await
        .unwrap());
        let after = notarizations::by_provider(&db, SignatureProvider::DocuSign, "nz-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.notarized_at.as_deref(), Some("2026-06-30T00:00:00Z"));

        let all = notarizations::for_notation(&db, notation_id).await.unwrap();
        assert_eq!(all.len(), 1);

        let asset_id = Uuid::new_v4();
        notarizations::set_asset(&db, first.id, asset_id)
            .await
            .unwrap();
        let named = notarizations::by_provider(&db, SignatureProvider::DocuSign, "nz-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(named.asset_id, Some(asset_id));
    })
    .await;
}

#[tokio::test]
async fn a_notarization_timestamp_that_does_not_parse_is_refused() {
    let Some((pg, _, notation_id)) = notation_fixture().await else {
        return;
    };
    let db = surreal();

    with_test_db(pg, async {
        notarizations::record_request(&db, notation_id, SignatureProvider::DocuSign, "nz-1")
            .await
            .unwrap();
        let error = notarizations::stamp_notarized(
            &db,
            SignatureProvider::DocuSign,
            "nz-1",
            "not a timestamp",
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, notarizations::NotarizationError::InvalidTimestamp(_)),
            "{error:?}"
        );
    })
    .await;
}

#[tokio::test]
async fn provider_envelope_replay_cannot_cross_project_notations() {
    let Some((pg, original_project, original)) = notation_fixture().await else {
        return;
    };
    let db = surreal();
    let other_project = Uuid::new_v4();
    let other_project_notation = Uuid::new_v4();
    sqlx::query("INSERT INTO project(id,code,name,status,entity_id) SELECT $1,$2,'Synthetic Other','open',entity_id FROM project WHERE id=$3")
        .bind(other_project).bind(other_project.to_string()).bind(original_project).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO notation(id,template_id,person_id,project_id,state) SELECT $1,template_id,person_id,$2,'BEGIN' FROM notation WHERE id=$3")
        .bind(other_project_notation).bind(other_project).bind(original).execute(pg.pool()).await.unwrap();
    with_test_db(pg, async {
        let first = record_request(
            &db,
            original,
            SignatureProvider::DocuSign,
            "shared-envelope",
        )
        .await
        .unwrap();
        let replay = record_request(
            &db,
            other_project_notation,
            SignatureProvider::DocuSign,
            "shared-envelope",
        )
        .await
        .unwrap_err();
        assert!(matches!(replay, SignatureError::WriteReturnedNothing));
        assert_eq!(
            by_provider(&db, SignatureProvider::DocuSign, "shared-envelope")
                .await
                .unwrap(),
            Some(first)
        );
        assert!(for_notation(&db, other_project_notation)
            .await
            .unwrap()
            .is_empty());

        let first = notarizations::record_request(
            &db,
            original,
            SignatureProvider::DocuSign,
            "shared-notarization",
        )
        .await
        .unwrap();
        let replay = notarizations::record_request(
            &db,
            other_project_notation,
            SignatureProvider::DocuSign,
            "shared-notarization",
        )
        .await
        .unwrap_err();
        assert!(matches!(
            replay,
            notarizations::NotarizationError::WriteReturnedNothing
        ));
        assert_eq!(
            notarizations::by_provider(&db, SignatureProvider::DocuSign, "shared-notarization")
                .await
                .unwrap(),
            Some(first)
        );
        assert!(notarizations::for_notation(&db, other_project_notation)
            .await
            .unwrap()
            .is_empty());
    })
    .await;
}
