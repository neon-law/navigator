use super::*;
use crate::expunge_records::{self, NewExpunge, CATEGORY_PRIVILEGE};
use crate::postgres::fixtures::{activate_family, firm_for_notation, workflow_fixture};
use crate::postgres::fixtures::{connect, fresh_schema};
use crate::postgres::history::Family;

#[tokio::test]
async fn expunge_audit_uses_postgres_without_starting_an_engine() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    activate_family(&pg, Family::Satellites).await;
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    let project_id = Uuid::new_v4();
    let actor = Uuid::new_v4();
    with_test_db(pg.clone(), async {
        let id = expunge_records::record(
            &unavailable,
            &NewExpunge {
                project_id,
                path: "documents/synthetic.txt",
                category: CATEGORY_PRIVILEGE,
                authorized_by_person_id: actor,
                head_before: Some("before"),
                head_after: Some("after"),
                note: None,
            },
        )
        .await
        .expect("the PostgreSQL audit write needs no engine");
        let audit = expunge_records::by_id(&unavailable, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (audit.project_id, audit.authorized_by_person_id),
            (project_id, actor)
        );
        assert!(
            expunge_records::exists_for_project(&unavailable, project_id)
                .await
                .unwrap()
        );
        assert_eq!(
            expunge_records::for_project(&unavailable, project_id)
                .await
                .unwrap(),
            vec![audit]
        );
        assert!(
            !expunge_records::exists_for_project(&unavailable, Uuid::new_v4())
                .await
                .unwrap()
        );
        assert!(expunge_records::by_id(&unavailable, Uuid::new_v4())
            .await
            .unwrap()
            .is_none());
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn a_closed_postgres_pool_does_not_fall_back_to_the_other_client() {
    let pool = sqlx::postgres::PgPoolOptions::new().connect_lazy_with(
        sqlx::postgres::PgConnectOptions::new()
            .host("127.0.0.1")
            .database("smoke"),
    );
    pool.close().await;
    let pg = PostgresDb(pool);
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    with_test_db(pg, async {
        let error = expunge_records::for_project(&unavailable, Uuid::new_v4())
            .await
            .expect_err("a configured but closed pool must fail");
        assert!(matches!(
            error,
            expunge_records::ExpungeRecordError::Postgres(MatterAuthorityError::Family(
                crate::postgres::history::FamilyError::NativeOrigin(
                    crate::postgres::native_origin::OriginError::Storage
                )
            ))
        ));
    })
    .await;
}

#[tokio::test]
async fn expunge_resolution_is_atomic_and_keeps_the_other_project_pending() {
    use crate::expunge_requests::{self, NewExpungeRequest, STATUS_AUTHORIZED, STATUS_PENDING};
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    activate_family(&pg, Family::Satellites).await;
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    let project = Uuid::new_v4();
    let other = Uuid::new_v4();
    let actor = Uuid::new_v4();
    with_test_db(pg.clone(), async {
        let mut ids = Vec::new();
        for project_id in [project, other] {
            ids.push(
                expunge_requests::create(
                    &unavailable,
                    &NewExpungeRequest {
                        project_id,
                        asset_id: Uuid::new_v4(),
                        requested_by_person_id: actor,
                        note: Some("synthetic"),
                    },
                )
                .await
                .unwrap(),
            );
        }
        let audit = Uuid::new_v4();
        let won = expunge_requests::authorize(&unavailable, ids[0], actor, audit)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(won.status, STATUS_AUTHORIZED);
        assert_eq!(won.expunge_record_id, Some(audit));
        assert!(expunge_requests::deny(&unavailable, ids[0], Uuid::new_v4())
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            expunge_requests::by_id(&unavailable, ids[0]).await.unwrap(),
            Some(won)
        );
        let pending = expunge_requests::list_pending(&unavailable).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].project_id, other);
        assert_eq!(pending[0].status, STATUS_PENDING);
        assert!(expunge_requests::exists_for_project(&unavailable, project)
            .await
            .unwrap());
        assert_eq!(
            expunge_requests::pending_for_document(&unavailable, pending[0].asset_id)
                .await
                .unwrap(),
            Some(pending[0].clone())
        );
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn playbook_updates_are_scoped_and_duplicate_names_are_refused() {
    use crate::playbooks::{self, NewPlaybook, PlaybookError};
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    activate_family(&pg, Family::Satellites).await;
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    with_test_db(pg.clone(), async {
        let (_, notation, _) = workflow_fixture(&pg).await;
        let entity: Uuid = sqlx::query_scalar("SELECT p.entity_id FROM project p JOIN notation n ON n.project_id = p.id WHERE n.id = $1")
            .bind(notation).fetch_one(pg.pool()).await.unwrap();
        let new = NewPlaybook {
            entity_id: entity,
            name: "Synthetic",
            positions: &[],
        };
        let id = playbooks::create(&unavailable, &new).await.unwrap();
        assert!(matches!(
            playbooks::create(&unavailable, &new).await,
            Err(PlaybookError::DuplicateName(_))
        ));
        let (_, other_notation, _) = workflow_fixture(&pg).await;
        let other_entity: Uuid = sqlx::query_scalar("SELECT p.entity_id FROM project p JOIN notation n ON n.project_id = p.id WHERE n.id = $1")
            .bind(other_notation).fetch_one(pg.pool()).await.unwrap();
        let other = playbooks::create(
            &unavailable,
            &NewPlaybook {
                entity_id: other_entity,
                ..new
            },
        )
        .await
        .unwrap();
        let original = playbooks::by_id(&unavailable, other).await.unwrap();
        let positions = vec![playbooks::Position {
            topic: "Synthetic".into(),
            preferred: "A".into(),
            fallback: "B".into(),
            walkaway: "C".into(),
            severity: "low".into(),
        }];
        playbooks::update_positions(&unavailable, id, &positions)
            .await
            .unwrap();
        let rows = playbooks::for_entity(&unavailable, entity).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(playbooks::positions_of(rows.iter().find(|row| row.id == id).unwrap()).unwrap(), positions);
        assert_eq!(
            playbooks::by_id(&unavailable, other).await.unwrap(),
            original
        );
        assert_eq!(playbooks::all(&unavailable).await.unwrap().len(), 2);
        assert!(matches!(
            playbooks::update_positions(&unavailable, Uuid::new_v4(), &[]).await,
            Err(PlaybookError::NotFound(_))
        ));
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn git_tokens_store_hashes_and_refuse_expiry_and_revocation() {
    use crate::git_access_tokens::{self, SCOPE_READ};
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    activate_family(&pg, Family::Satellites).await;
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    with_test_db(pg.clone(), async {
        let now = chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let expires = now + chrono::Duration::minutes(10);
        let (person, _, _) = workflow_fixture(&pg).await;
        let row = git_access_tokens::mint(
            &unavailable,
            person,
            None,
            SCOPE_READ,
            "synthetic-token",
            expires,
        )
        .await
        .unwrap();
        assert_eq!(
            row.token_hash,
            git_access_tokens::hash_token("synthetic-token")
        );
        assert_ne!(row.token_hash, "synthetic-token");
        assert_eq!(
            git_access_tokens::validate(&unavailable, "synthetic-token", now)
                .await
                .unwrap()
                .unwrap()
                .id,
            row.id
        );
        assert!(
            git_access_tokens::validate(&unavailable, "synthetic-token", expires)
                .await
                .unwrap()
                .is_none()
        );
        assert!(git_access_tokens::validate(&unavailable, "wrong", now)
            .await
            .unwrap()
            .is_none());
        git_access_tokens::revoke(&unavailable, row.id)
            .await
            .unwrap();
        git_access_tokens::revoke(&unavailable, row.id)
            .await
            .unwrap();
        assert!(
            git_access_tokens::validate(&unavailable, "synthetic-token", now)
                .await
                .unwrap()
                .is_none()
        );
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn citation_pins_and_nullable_patches_preserve_other_fields() {
    use crate::authorities::{self, NewCitation};
    use serde_json::json;
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    activate_family(&pg, Family::Satellites).await;
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    with_test_db(pg.clone(), async {
        let use_id = Uuid::new_v4();
        let pin = json!({"page": 3, "rect": [0.1, 0.2, 0.3, 0.4], "extra": null});
        let row = authorities::cite(
            &unavailable,
            &NewCitation {
                authority_use_id: use_id,
                quote: "Synthetic",
                why: "Synthetic",
                source_pin: Some(pin.clone()),
                draft_pin: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(row.source_pin, Some(pin));
        assert_eq!(row.draft_pin, None);
        assert_eq!(
            authorities::citations_for_use(&unavailable, use_id)
                .await
                .unwrap(),
            vec![row.clone()]
        );
        let changed: Vec<authorities::Citation> = update(
            &pg,
            "citation",
            json!({"id": row.id}),
            json!({"source_pin": null}),
        )
        .await
        .unwrap();
        assert_eq!(changed[0].source_pin, Some(serde_json::Value::Null));
        assert_eq!(changed[0].quote, row.quote);
        assert_eq!(changed[0].inserted_at, row.inserted_at);
        let request = crate::expunge_requests::create(
            &unavailable,
            &crate::expunge_requests::NewExpungeRequest {
                project_id: Uuid::new_v4(),
                asset_id: Uuid::new_v4(),
                requested_by_person_id: Uuid::new_v4(),
                note: Some("Synthetic"),
            },
        )
        .await
        .unwrap();
        let patched: Vec<crate::expunge_requests::ExpungeRequest> = update(
            &pg,
            "expunge_request",
            json!({"id": request}),
            json!({"note": null}),
        )
        .await
        .unwrap();
        assert_eq!(patched[0].note, None);
        assert_eq!(patched[0].status, crate::expunge_requests::STATUS_PENDING);
        let null: bool =
            sqlx::query_scalar("SELECT note IS NULL FROM expunge_request WHERE id = $1")
                .bind(request)
                .fetch_one(pg.pool())
                .await
                .unwrap();
        assert!(null);
    })
    .await;
    pg.pool().close().await;
}

async fn assert_verification_staleness(
    unavailable: &crate::application_store::ApplicationStore,
    citation: Uuid,
) {
    use crate::verifications;
    use rules::citation::{Axis, AxisStatus};
    let verification = verifications::record(unavailable, citation, "revision-one", Uuid::new_v4())
        .await
        .unwrap();
    verifications::set_axis(
        unavailable,
        verification.id,
        Axis::Citation,
        AxisStatus::Verified,
    )
    .await
    .unwrap();
    let stale = verifications::stale_after_revision(unavailable, citation, "revision-two")
        .await
        .unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].axis, Axis::Citation);
    let rows = verifications::for_citation(unavailable, citation)
        .await
        .unwrap();
    assert_eq!(
        verifications::axis_status(&rows[0], Axis::Citation).unwrap(),
        AxisStatus::Stale
    );
    assert_eq!(
        verifications::axis_status(&rows[0], Axis::Quote).unwrap(),
        AxisStatus::Unverified
    );
    assert!(
        verifications::stale_after_revision(unavailable, citation, "revision-two")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        verifications::record(unavailable, citation, " ", Uuid::new_v4()).await,
        Err(verifications::VerificationError::MissingRevision)
    ));
    assert!(matches!(
        verifications::set_axis(
            unavailable,
            Uuid::new_v4(),
            Axis::Quote,
            AxisStatus::Verified
        )
        .await,
        Err(verifications::VerificationError::NotFound(_))
    ));
}

async fn assert_authority_deletion(
    unavailable: &crate::application_store::ApplicationStore,
    project: Uuid,
    other: Uuid,
    untouched: crate::authorities::AuthorityUse,
    citation: crate::authorities::Citation,
) {
    use crate::{authorities, verifications};
    authorities::delete_for_project(unavailable, project)
        .await
        .unwrap();
    assert!(authorities::uses_for_project(unavailable, project)
        .await
        .unwrap()
        .is_empty());
    assert!(
        authorities::citations_for_use(unavailable, citation.authority_use_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(verifications::for_citation(unavailable, citation.id)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        authorities::uses_for_project(unavailable, other)
            .await
            .unwrap(),
        vec![untouched]
    );
}

#[tokio::test]
async fn authority_visibility_verification_staleness_and_deletion_stay_project_scoped() {
    use crate::authorities;
    use rules::citation::Disposition;
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    activate_family(&pg, Family::Satellites).await;
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    Box::pin(crate::postgres::signatures::with_test_db(
        pg.clone(),
        with_test_db(pg.clone(), async {
            let project = Uuid::new_v4();
            let other = Uuid::new_v4();
            let authority = Uuid::new_v4();
            let visible = authorities::cite_in_matter(
                &unavailable,
                project,
                authority,
                "ours",
                Disposition::ReliedOn,
                None,
            )
            .await
            .unwrap();
            let changed = authorities::cite_in_matter(
                &unavailable,
                project,
                authority,
                "neutral",
                Disposition::ReliedOnWithQualification,
                Some("Synthetic"),
            )
            .await
            .unwrap();
            assert_eq!(
                (changed.id, changed.inserted_at),
                (visible.id, visible.inserted_at)
            );
            authorities::cite_in_matter(
                &unavailable,
                project,
                Uuid::new_v4(),
                "adverse",
                Disposition::ReviewedNotUsed,
                None,
            )
            .await
            .unwrap();
            let untouched = authorities::cite_in_matter(
                &unavailable,
                other,
                authority,
                "ours",
                Disposition::ReliedOn,
                None,
            )
            .await
            .unwrap();
            assert_eq!(
                authorities::uses_for_project(&unavailable, project)
                    .await
                    .unwrap()
                    .len(),
                2
            );
            assert_eq!(
                authorities::client_visible_uses(&unavailable, project)
                    .await
                    .unwrap(),
                vec![changed]
            );
            let citation = authorities::cite(
                &unavailable,
                &authorities::NewCitation {
                    authority_use_id: visible.id,
                    quote: "Synthetic",
                    why: "Synthetic",
                    source_pin: None,
                    draft_pin: None,
                },
            )
            .await
            .unwrap();
            assert_verification_staleness(&unavailable, citation.id).await;
            assert_authority_deletion(&unavailable, project, other, untouched, citation).await;
        }),
    ))
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn attestation_preserves_replay_identity_and_chain_outcomes() {
    use crate::attestations;
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    activate_family(&pg, Family::Satellites).await;
    let (_, notation, _) = workflow_fixture(&pg).await;
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    with_test_db(pg.clone(), async {
        let pending = attestations::NewAttestation {
            notation_id: notation,
            chain: "null",
            sha256: "synthetic",
            status: attestations::STATUS_PENDING,
            pda: None,
            tx_signature: None,
            firm_wallet: None,
            client_wallet: None,
            recorded_at: None,
        };
        let first = attestations::record(&unavailable, &pending).await.unwrap();
        let recorded = attestations::record(
            &unavailable,
            &attestations::NewAttestation {
                status: attestations::STATUS_RECORDED,
                recorded_at: Some("2026-11-17T00:00:00Z"),
                ..pending
            },
        )
        .await
        .unwrap();
        assert_eq!(
            (recorded.id, recorded.inserted_at),
            (first.id, first.inserted_at)
        );
        assert_eq!(recorded.id.get_version_num(), 4);
        assert_eq!(
            attestations::by_notation(&unavailable, notation)
                .await
                .unwrap(),
            Some(recorded)
        );
    })
    .await;
    pg.pool().close().await;
}

async fn assert_module_audits(
    pg: &PostgresDb,
    unavailable: &crate::application_store::ApplicationStore,
    project: Uuid,
    other: Uuid,
    person: Uuid,
) {
    use crate::project_modules::{self, Module};
    let first = project_modules::enable(unavailable, project, Module::Estate, Some(person))
        .await
        .unwrap();
    assert_eq!(
        project_modules::enable(unavailable, project, Module::Estate, Some(person))
            .await
            .unwrap(),
        first
    );
    assert_eq!(
        project_modules::list_for_project(unavailable, project)
            .await
            .unwrap(),
        vec![Module::Estate]
    );
    assert!(project_modules::list_for_project(unavailable, other)
        .await
        .unwrap()
        .is_empty());
    assert!(
        project_modules::disable(unavailable, project, Module::Estate, Some(person))
            .await
            .unwrap()
    );
    assert!(
        !project_modules::disable(unavailable, project, Module::Estate, Some(person))
            .await
            .unwrap()
    );
    let audit: Vec<String> =
        sqlx::query_scalar("SELECT action FROM relationship_log WHERE project_id = $1 ORDER BY id")
            .bind(project)
            .fetch_all(pg.pool())
            .await
            .unwrap();
    assert_eq!(audit, vec!["module_enabled", "module_disabled"]);
    assert!(
        project_modules::enable(unavailable, project, Module::Estate, Some(Uuid::new_v4()))
            .await
            .is_err()
    );
    assert!(
        project_modules::list_for_project(unavailable, project)
            .await
            .unwrap()
            .is_empty(),
        "a refused audit rolls the toggle back"
    );
}

#[tokio::test]
async fn captions_and_module_toggles_preserve_tenancy_and_atomic_audits() {
    use crate::matter_captions;
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    activate_family(&pg, Family::Satellites).await;
    let (person, notation, _) = workflow_fixture(&pg).await;
    let (_, project) = firm_for_notation(&pg, notation).await;
    let (_, other_notation, _) = workflow_fixture(&pg).await;
    let (other_firm, other) = firm_for_notation(&pg, other_notation).await;
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    with_test_db(pg.clone(), async {
        let matter = word::matter::Matter {
            case_number: "SYNTHETIC-1".into(),
            signed_on: Some("2026-11-17".into()),
            ..Default::default()
        };
        let first = matter_captions::replace_for_project(&unavailable, project, Some(&matter))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.matter, matter);
        assert_eq!(
            matter_captions::for_project(&unavailable, project)
                .await
                .unwrap(),
            Some(first)
        );
        matter_captions::replace_for_project(&unavailable, other, Some(&matter))
            .await
            .unwrap();
        assert!(
            sqlx::query("UPDATE matter_caption SET firm_id = $1 WHERE project_id = $2")
                .bind(other_firm)
                .bind(project)
                .execute(pg.pool())
                .await
                .is_err()
        );
        let changed = word::matter::Matter {
            case_number: "SYNTHETIC-2".into(),
            ..matter.clone()
        };
        matter_captions::replace_for_project(&unavailable, project, Some(&changed))
            .await
            .unwrap();
        assert_eq!(
            matter_captions::for_project(&unavailable, other)
                .await
                .unwrap()
                .unwrap()
                .matter,
            matter
        );
        assert!(
            matter_captions::replace_for_project(&unavailable, project, None)
                .await
                .unwrap()
                .is_none()
        );
        assert!(matter_captions::for_project(&unavailable, project)
            .await
            .unwrap()
            .is_none());
        assert_module_audits(&pg, &unavailable, project, other, person).await;
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn social_oauth_enforces_firm_authority_and_consumes_once_without_retaining_verifiers() {
    use crate::{persons::Role, social_oauth_attempts as oauth};
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    activate_family(&pg, Family::Satellites).await;
    let (person, notation, _) = workflow_fixture(&pg).await;
    let (firm, _) = firm_for_notation(&pg, notation).await;
    sqlx::query("INSERT INTO person_firm_role(person_id, firm_id, membership, is_dri) VALUES ($1, $2, 'admin', true)").bind(person).bind(firm).execute(pg.pool()).await.unwrap();
    sqlx::query(
        "INSERT INTO brand(name, brand_key, firm_id) VALUES ('Synthetic', 'synthetic-social', $1)",
    )
    .bind(firm)
    .execute(pg.pool())
    .await
    .unwrap();
    let (other_actor, other_notation, _) = workflow_fixture(&pg).await;
    let (other_firm, _) = firm_for_notation(&pg, other_notation).await;
    sqlx::query("INSERT INTO person_firm_role(person_id, firm_id, membership, is_dri) VALUES ($1, $2, 'admin', true)")
        .bind(other_actor).bind(other_firm).execute(pg.pool()).await.unwrap();
    sqlx::query("UPDATE person SET role = 'admin' WHERE id IN ($1, $2)")
        .bind(person)
        .bind(other_actor)
        .execute(pg.pool())
        .await
        .unwrap();
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    crate::postgres::tenancy::with_test_authority(pg.clone(), true, with_test_db(pg.clone(), async {
        let now = chrono::Utc::now();
        assert!(matches!(oauth::begin(&unavailable, Role::Admin, other_actor, "synthetic-social", "facebook", now).await, Err(oauth::SocialOAuthAttemptError::Denied)));
        assert!(matches!(oauth::begin(&unavailable, Role::Client, person, "synthetic-social", "facebook", now).await, Err(oauth::SocialOAuthAttemptError::Denied)));
        let attempt = oauth::begin(&unavailable, Role::Admin, person, "synthetic-social", "facebook", now).await.unwrap();
        oauth::bind_account(&unavailable, &attempt, "synthetic-account").await.unwrap();
        oauth::bind_instagram(&unavailable, &attempt, "synthetic-instagram").await.unwrap();
        let (left, right) = tokio::join!(oauth::consume(&unavailable, &attempt.state, now), oauth::consume(&unavailable, &attempt.state, now));
        assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
        let consumed = left.or(right).unwrap();
        assert_eq!(consumed.pkce_verifier, attempt.pkce_verifier);
        assert_eq!(consumed.provider_account_id.as_deref(), Some("synthetic-account"));
        assert_eq!(consumed.instagram_account_id.as_deref(), Some("synthetic-instagram"));
        assert!(matches!(oauth::consume(&unavailable, &attempt.state, now).await, Err(oauth::SocialOAuthAttemptError::InvalidState)));
        let stored: (String, Option<String>, Option<String>) = sqlx::query_as("SELECT pkce_verifier, provider_account_id, instagram_account_id FROM social_oauth_attempt").fetch_one(pg.pool()).await.unwrap();
        assert_eq!(stored, (String::new(), None, None));
        let expired = oauth::begin(&unavailable, Role::Admin, person, "synthetic-social", "facebook", now).await.unwrap();
        assert!(matches!(oauth::consume(&unavailable, &expired.state, now + chrono::Duration::minutes(11)).await, Err(oauth::SocialOAuthAttemptError::InvalidState)));
    })).await;
    pg.pool().close().await;
}

#[tokio::test]
async fn efiling_reports_keep_court_decisions_and_reject_cross_project_documents() {
    use crate::efiling_transactions::{
        self as filing, EfilingReport, STATUS_ACCEPTED, STATUS_SUBMITTED,
    };
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    activate_family(&pg, Family::Satellites).await;
    let (_, notation, _) = workflow_fixture(&pg).await;
    let (_, project) = firm_for_notation(&pg, notation).await;
    let (_, other_notation, _) = workflow_fixture(&pg).await;
    let (_, other) = firm_for_notation(&pg, other_notation).await;
    let asset = Uuid::new_v4();
    sqlx::query("INSERT INTO asset(id, storage_key, content_type, byte_size, sha256_hex, project_id) VALUES ($1, 'synthetic', 'application/pdf', 0, 'synthetic', $2)")
        .bind(asset).bind(other).execute(pg.pool()).await.unwrap();
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    with_test_db(pg.clone(), async {
        let report = EfilingReport {
            project_id: project,
            transaction_id: "SYNTHETIC-1".into(),
            status: STATUS_ACCEPTED.into(),
            filed_on: Some("2026-11-17".into()),
            ..Default::default()
        };
        let accepted = filing::record(&unavailable, &report).await.unwrap();
        let pending = filing::record(
            &unavailable,
            &EfilingReport {
                status: STATUS_SUBMITTED.into(),
                filed_on: None,
                ..report.clone()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            (pending.id, pending.status.as_str(), pending.filed_on),
            (accepted.id, STATUS_ACCEPTED, accepted.filed_on.clone())
        );
        assert!(filing::record(
            &unavailable,
            &EfilingReport {
                conformed_document_id: Some(asset),
                ..report.clone()
            }
        )
        .await
        .is_err());
        assert_eq!(filing::list(&unavailable, project).await.unwrap().len(), 1);
        assert!(filing::list(&unavailable, other).await.unwrap().is_empty());
        assert_eq!(
            filing::find(&unavailable, project, "SYNTHETIC-1")
                .await
                .unwrap()
                .unwrap()
                .id,
            accepted.id
        );
        assert!(filing::find(&unavailable, other, "SYNTHETIC-1")
            .await
            .unwrap()
            .is_none());
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn contract_review_patches_keep_findings_and_do_not_touch_another_review() {
    use crate::contract_reviews::{self as review, NewContractReview};
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    activate_family(&pg, Family::Satellites).await;
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    with_test_db(pg.clone(), async {
        let new = NewContractReview {
            notation_id: Uuid::new_v4(),
            playbook_id: Uuid::new_v4(),
            asset_id: None,
        };
        let id = review::create(&unavailable, &new).await.unwrap();
        let other = review::create(
            &unavailable,
            &NewContractReview {
                notation_id: Uuid::new_v4(),
                ..new
            },
        )
        .await
        .unwrap();
        let untouched = review::by_id(&unavailable, other).await.unwrap();
        let findings = vec![review::Finding {
            clause_ref: "Synthetic".into(),
            deviation: "Synthetic".into(),
            severity: "low".into(),
            suggested_redline: None,
            attorney_note: None,
            accepted: false,
        }];
        review::record_analysis(&unavailable, id, "Synthetic", &findings)
            .await
            .unwrap();
        review::update_risk_summary(&unavailable, id, "Edited")
            .await
            .unwrap();
        let row = review::by_id(&unavailable, id).await.unwrap().unwrap();
        assert_eq!(review::findings_of(&row).unwrap(), findings);
        assert_eq!(row.status, review::STATUS_ANALYZED);
        review::update_findings(&unavailable, id, &[])
            .await
            .unwrap();
        review::set_status(&unavailable, id, review::STATUS_APPROVED)
            .await
            .unwrap();
        assert_eq!(
            review::latest_for_notation(&unavailable, new.notation_id)
                .await
                .unwrap()
                .unwrap()
                .id,
            id
        );
        assert_eq!(review::by_id(&unavailable, other).await.unwrap(), untouched);
        assert!(matches!(
            review::set_status(&unavailable, Uuid::new_v4(), review::STATUS_APPROVED).await,
            Err(review::ContractReviewError::NotFound(_))
        ));
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn testimonial_consent_requires_client_dri_and_publication_requires_matter_lawyer() {
    use crate::{persons::Role, testimonials as testimonial};
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    crate::postgres::fixtures::activate_application(&pg).await;
    let (client, notation, _) = workflow_fixture(&pg).await;
    let (_, project) = firm_for_notation(&pg, notation).await;
    let (lawyer, _, _) = workflow_fixture(&pg).await;
    sqlx::query("UPDATE person SET role = 'lawyer' WHERE id = $1")
        .bind(lawyer)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO person_project_role(person_id, project_id, participation, is_client_dri) VALUES ($1, $2, 'client', true), ($3, $2, 'lawyer', false)")
        .bind(client).bind(project).bind(lawyer).execute(pg.pool()).await.unwrap();
    sqlx::query("UPDATE project SET brand = 'synthetic-brand' WHERE id = $1")
        .bind(project)
        .execute(pg.pool())
        .await
        .unwrap();
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    PostgresDb::with_test_db(
        pg.clone(),
        crate::postgres::tenancy::with_test_authority(
            pg.clone(),
            true,
            with_test_db(pg.clone(), async {
                let input = testimonial::TestimonialSubmission {
                    quote: "Synthetic",
                    attribution_label: Some("Synthetic".into()),
                    request_public: true,
                };
                assert!(matches!(
                    testimonial::save_for_client_dri(&unavailable, lawyer, project, &input).await,
                    Err(testimonial::TestimonialError::NotAuthorized)
                ));
                let row = testimonial::save_for_client_dri(&unavailable, client, project, &input)
                    .await
                    .unwrap();
                assert!(row.consented_at.is_some());
                assert!(row.published_at.is_none());
                assert!(testimonial::published_for_home(&unavailable, 10)
                    .await
                    .unwrap()
                    .is_empty());
                assert!(matches!(
                    testimonial::publish(&unavailable, Some(client), Role::Client, row.id).await,
                    Err(testimonial::TestimonialError::NotAuthorized)
                ));
                testimonial::publish(&unavailable, Some(lawyer), Role::Lawyer, row.id)
                    .await
                    .unwrap();
                assert_eq!(
                    testimonial::published_for_brand(&unavailable, "synthetic-brand", None)
                        .await
                        .unwrap()
                        .len(),
                    1
                );
                assert!(
                    testimonial::published_for_brand(&unavailable, "other-brand", None)
                        .await
                        .unwrap()
                        .is_empty()
                );
                testimonial::save_for_client_dri(
                    &unavailable,
                    client,
                    project,
                    &testimonial::TestimonialSubmission {
                        request_public: false,
                        ..input
                    },
                )
                .await
                .unwrap();
                assert!(testimonial::published_for_home(&unavailable, 10)
                    .await
                    .unwrap()
                    .is_empty());
                assert!(matches!(
                    testimonial::publish(&unavailable, Some(lawyer), Role::Lawyer, row.id).await,
                    Err(testimonial::TestimonialError::NotConsented)
                ));
            }),
        ),
    )
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn person_merge_rewires_authoritative_orphan_links_and_deduplicates_collisions() {
    use serde_json::json;
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    activate_family(&pg, Family::Satellites).await;
    let (drop_person, notation, _) = workflow_fixture(&pg).await;
    let (keep_person, _, _) = workflow_fixture(&pg).await;
    let (other_person, _, _) = workflow_fixture(&pg).await;
    let project: Uuid = sqlx::query_scalar("SELECT project_id FROM notation WHERE id = $1")
        .bind(notation)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    with_test_db(pg.clone(), async {
        let audit = expunge_records::record(&unavailable, &NewExpunge { project_id: Uuid::new_v4(), path: "synthetic", category: CATEGORY_PRIVILEGE,
            authorized_by_person_id: drop_person, head_before: None, head_after: None, note: None }).await.unwrap();
        let other_audit = expunge_records::record(&unavailable, &NewExpunge { project_id: Uuid::new_v4(), path: "other", category: CATEGORY_PRIVILEGE,
            authorized_by_person_id: other_person, head_before: None, head_after: None, note: None }).await.unwrap();
        let token = crate::git_access_tokens::mint(&unavailable, drop_person, None, "read", "synthetic-merge", chrono::Utc::now() + chrono::Duration::hours(1)).await.unwrap();
        for person in [keep_person, drop_person] {
            let now = chrono::Utc::now();
            upsert::<Value>(&pg, "testimonial", &json!({"id": Uuid::new_v4(), "project_id": project, "person_id": person,
                "quote": "Synthetic duplicate", "attribution_label": null, "consented_at": null, "published_at": null, "display_order": 0,
                "inserted_at": now, "updated_at": now})).await.unwrap();
        }
        // A nonexistent keeper violates the existing identity FKs; the whole merge rolls back.
        assert!(crate::postgres::people::merge_rows(&pg, Uuid::new_v4(), drop_person).await.is_err());
        assert_eq!(expunge_records::by_id(&unavailable, audit).await.unwrap().unwrap().authorized_by_person_id, drop_person);
        crate::postgres::people::merge_rows(&pg, keep_person, drop_person).await.unwrap();
        assert_eq!(expunge_records::by_id(&unavailable, audit).await.unwrap().unwrap().authorized_by_person_id, keep_person);
        assert_eq!(expunge_records::by_id(&unavailable, other_audit).await.unwrap().unwrap().authorized_by_person_id, other_person);
        let token_person: Uuid = sqlx::query_scalar("SELECT person_id FROM git_access_token WHERE id = $1").bind(token.id).fetch_one(pg.pool()).await.unwrap();
        assert_eq!(token_person, keep_person);
        let testimonials: Vec<Uuid> = sqlx::query_scalar("SELECT person_id FROM testimonial WHERE project_id = $1").bind(project).fetch_all(pg.pool()).await.unwrap();
        assert_eq!(testimonials, vec![keep_person]);
        let dropped: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM person WHERE id = $1)").bind(drop_person).fetch_one(pg.pool()).await.unwrap();
        assert!(!dropped);
        let respondent: Uuid = sqlx::query_scalar("SELECT person_id FROM notation WHERE id = $1").bind(notation).fetch_one(pg.pool()).await.unwrap();
        assert_eq!(respondent, keep_person);
    }).await;
    pg.pool().close().await;
}

#[tokio::test]
async fn matter_queries_refuse_identity_changes_and_unregistered_filters() {
    use serde_json::json;
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let row = Uuid::new_v4();
    assert!(matches!(
        update::<Value>(
            &pg,
            "expunge_record",
            json!({"id": row}),
            json!({"id": Uuid::new_v4()})
        )
        .await,
        Err(MatterAuthorityError::Unregistered { .. })
    ));
    assert!(matches!(
        select::<Value>(&pg, "expunge_record", json!({"unknown": "value"}), &[]).await,
        Err(MatterAuthorityError::Unregistered { .. })
    ));
    pg.pool().close().await;
}

#[tokio::test]
async fn provider_callbacks_are_idempotent_and_patch_only_their_request() {
    use crate::notarizations::{self, SignatureProvider};
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    activate_family(&pg, Family::Satellites).await;
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    crate::postgres::signatures::with_test_db(
        pg.clone(),
        with_test_db(pg.clone(), async {
            let notation = Uuid::new_v4();
            let provider = SignatureProvider::DocuSign;
            let row = notarizations::record_request(
                &unavailable,
                notation,
                provider,
                "synthetic-request",
            )
            .await
            .unwrap();
            assert_eq!(
                notarizations::record_request(
                    &unavailable,
                    notation,
                    provider,
                    "synthetic-request"
                )
                .await
                .unwrap(),
                row
            );
            let other = notarizations::record_request(
                &unavailable,
                Uuid::new_v4(),
                provider,
                "other-request",
            )
            .await
            .unwrap();
            let asset = Uuid::new_v4();
            notarizations::set_asset(&unavailable, row.id, asset)
                .await
                .unwrap();
            assert!(notarizations::stamp_notarized(
                &unavailable,
                provider,
                "synthetic-request",
                "2026-11-17T00:00:00Z"
            )
            .await
            .unwrap());
            assert!(!notarizations::stamp_notarized(
                &unavailable,
                provider,
                "absent",
                "2026-11-17T00:00:00Z"
            )
            .await
            .unwrap());
            let stored = notarizations::for_notation(&unavailable, notation)
                .await
                .unwrap();
            assert_eq!(stored.len(), 1);
            assert_eq!(stored[0].asset_id, Some(asset));
            assert!(stored[0].notarized_at.is_some());
            assert_eq!(
                notarizations::by_provider(&unavailable, provider, "other-request")
                    .await
                    .unwrap(),
                Some(other)
            );
        }),
    )
    .await;
    pg.pool().close().await;
}
