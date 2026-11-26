use super::*;
use crate::notations::NewNotation;
use crate::persons::{NewPerson, Role};
use crate::postgres_mirror::tests::{connect, fresh_schema, mirror_identity};
use crate::test_support::mem_surreal;

async fn mirrored(surreal: &SurrealDb, pg: &PostgresDb, key: &MirrorKey) -> Mirrored {
    super::super::during_initial_copy(super::super::mirror(surreal, pg, key))
        .await
        .unwrap()
}

async fn assert_template_pin_repair(surreal: &SurrealDb, pg: &PostgresDb, template: Uuid) {
    super::super::templates::mirror(surreal, pg, template)
        .await
        .unwrap();
    assert_eq!(
        template_pin_parity(surreal, pg).await.unwrap().differences,
        0
    );
    let digest = "a".repeat(64);
    surreal
        .query("UPDATE $id SET body_sha256=$digest")
        .bind(("id", record_id("template", template)))
        .bind(("digest", digest.clone()))
        .await
        .unwrap()
        .check()
        .unwrap();
    let mismatch = template_pin_parity(surreal, pg).await.unwrap();
    assert_eq!((mismatch.compared, mismatch.differences), (1, 1));
    sqlx::query("UPDATE template SET body_sha256=$2 WHERE id=$1")
        .bind(template)
        .bind(digest)
        .execute(pg.pool())
        .await
        .unwrap();
    assert_eq!(
        template_pin_parity(surreal, pg).await.unwrap().differences,
        0
    );
}

#[tokio::test]
async fn captured_notation_template_pins_detect_drift_until_target_repair() {
    super::super::from_source(super::super::during_initial_copy(async {
        let Some((options, database)) = fresh_schema().await else {
            return;
        };
        let pg = connect(&options, &database).await;
        let source = mem_surreal().await;
        let client = person(&source, Role::Client).await;
        let project = matter(&source, &pg, "Synthetic Captured Pins").await;
        let template = Uuid::now_v7();
        let code = format!("captured-pin-{template}");
        source.query("CREATE $id SET code=$code,title='Captured',respondent_type='person',is_current=true,current_key=$key")
            .bind(("id", record_id("template", template)))
            .bind(("code", code.clone()))
            .bind(("key",crate::templates::current_key_in(crate::templates::Scope::Shared,&code)))
            .await.unwrap().check().unwrap();
        open(&source, template, client, project).await;
        assert_template_pin_repair(&source, &pg, template).await;
        pg.pool().close().await;
    })).await;
}

#[tokio::test]
async fn captured_notation_clause_import_replays_without_duplicate_rows() {
    super::super::from_source(super::super::during_initial_copy(async {
        let Some((options, database)) = fresh_schema().await else {
            return;
        };
        let pg = connect(&options, &database).await;
        let source = mem_surreal().await;
        let client = person(&source, Role::Client).await;
        let project = matter(&source, &pg, "Synthetic clause import").await;
        let template = Uuid::now_v7();
        let code = format!("captured-clause-{template}");
        source.query("CREATE $id SET code=$code,title='Captured',respondent_type='person',is_current=true,current_key=$key")
            .bind(("id", record_id("template", template)))
            .bind(("code", code.clone()))
            .bind(("key", crate::templates::current_key_in(crate::templates::Scope::Shared, &code)))
            .await.unwrap().check().unwrap();
        let notation = open(&source, template, client, project).await;
        assert!(crate::notation_clauses::append(&source, notation, "Source write refused", None)
            .await
            .is_err());
        let clause = Uuid::now_v7();
        source.query("CREATE $id SET notation_id=$notation, position=0, body_markdown='Synthetic captured clause'")
            .bind(("id", record_id("notation_clause", clause)))
            .bind(("notation", record_id("notation", notation)))
            .await.unwrap().check().unwrap();
        let key = MirrorKey::NotationClause(clause);
        assert_eq!(super::super::mirror(&source, &pg, &key).await.unwrap(), Mirrored::Written);
        assert_eq!(super::super::mirror(&source, &pg, &key).await.unwrap(), Mirrored::Unchanged);
        let row: (i64, String) = sqlx::query_as("SELECT count(*), min(body_markdown) FROM notation_clause WHERE id=$1")
            .bind(clause).fetch_one(pg.pool()).await.unwrap();
        assert_eq!(row, (1, "Synthetic captured clause".into()));
        pg.pool().close().await;
    })).await;
}

async fn person(surreal: &SurrealDb, role: Role) -> Uuid {
    crate::persons::create(
        surreal,
        &NewPerson::with_role(
            "Synthetic Person",
            format!("workflow-{}@example.com", Uuid::now_v7()),
            role,
        ),
    )
    .await
    .unwrap()
    .id
}

/// A Project whose Entity PostgreSQL already holds.
async fn matter(surreal: &SurrealDb, pg: &PostgresDb, name: &str) -> Uuid {
    let project = crate::test_support::seed_project_surreal(surreal, name).await;
    let entity = crate::projects::find_by_id(surreal, project)
        .await
        .unwrap()
        .unwrap()
        .entity_id;
    mirror_identity(pg, &[entity], &[]).await;
    project
}

async fn draft(surreal: &SurrealDb) -> Uuid {
    crate::templates::save_version(
        surreal,
        None,
        &format!("onboarding__synthetic-{}", Uuid::now_v7()),
        crate::templates::Version {
            title: "Synthetic Draft".into(),
            respondent_type: "person".into(),
            asset_id: None,
            form_code: None,
            kind: None,
            source_commit_sha: None,
        },
    )
    .await
    .unwrap()
    .into_model()
    .id
}

async fn open(surreal: &SurrealDb, template: Uuid, client: Uuid, project: Uuid) -> Uuid {
    crate::notations::create(
        surreal,
        &NewNotation::new(template, client, project, "BEGIN")
            .with_questionnaire_snapshot(serde_json::json!({"states": ["BEGIN", "END"]})),
    )
    .await
    .unwrap()
    .id
}

async fn question(surreal: &SurrealDb) -> Uuid {
    let id = Uuid::now_v7();
    surreal
        .query("CREATE $id SET code=$code,prompt='Synthetic?',answer_type='string',audience='both'")
        .bind(("id", record_id("question", id)))
        .bind(("code", format!("synthetic_{}", id.simple())))
        .await
        .unwrap()
        .check()
        .unwrap();
    id
}

async fn answer_at(
    surreal: &SurrealDb,
    id: Uuid,
    question: Uuid,
    person: Uuid,
    notation: Uuid,
    inserted_at: &str,
) {
    surreal
        .query(format!(
            "CREATE $id SET question_id = $question, person_id = $person, \
             notation_id = $notation, state_name = 'client_name', value = {{ value: 'Synthetic' }}, \
             source = 'lawyer', inserted_at = d'{inserted_at}', updated_at = d'{inserted_at}'"
        ))
        .bind(("id", record_id(crate::answers::TABLE, id)))
        .bind(("question", record_id("question", question)))
        .bind(("person", record_id("person", person)))
        .bind(("notation", record_id("notation", notation)))
        .await
        .unwrap()
        .check()
        .unwrap();
}

#[tokio::test]
async fn answer_mirror_preserves_microsecond_chronology_and_uuid_ties() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let client = person(&surreal, Role::Client).await;
    let project = matter(&surreal, &pg, "Synthetic Answer Chronology").await;
    let notation = open(&surreal, draft(&surreal).await, client, project).await;
    mirrored(&surreal, &pg, &MirrorKey::Notation(notation)).await;
    let question = question(&surreal).await;

    let older_v4 = Uuid::parse_str("f0000000-0000-4000-8000-000000000001").unwrap();
    let newer_v7 = Uuid::parse_str("00000000-0001-7000-8000-000000000001").unwrap();
    let tie_earlier_v7 = Uuid::parse_str("00000000-0002-7000-8000-000000000001").unwrap();
    let tie_later_v4 = Uuid::parse_str("f0000000-0000-4000-8000-000000000002").unwrap();
    let fixtures = [
        (newer_v7, "2026-01-02T00:00:00.000002Z"),
        (older_v4, "2026-01-01T00:00:00.000001Z"),
        (tie_later_v4, "2026-01-03T00:00:00.123456900Z"),
        (tie_earlier_v7, "2026-01-03T00:00:00.123456100Z"),
    ];
    for (id, inserted_at) in fixtures {
        answer_at(&surreal, id, question, client, notation, inserted_at).await;
        mirrored(&surreal, &pg, &MirrorKey::Answer(id)).await;
    }

    let expected = [older_v4, newer_v7, tie_earlier_v7, tie_later_v4];
    let source =
        crate::answers::for_question_and_person(&surreal, question, client, Some("client_name"))
            .await
            .unwrap();
    let source_ids: Vec<_> = source.iter().map(|answer| answer.id).collect();
    assert_eq!(source_ids, expected);
    let target_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM answer WHERE notation_id = $1 ORDER BY inserted_at, id")
            .bind(notation)
            .fetch_all(&pg.0)
            .await
            .unwrap();
    assert_eq!(target_ids, expected);
    assert_eq!(
        crate::answers::latest_for_state(&surreal, question, "client_name", client, notation)
            .await
            .unwrap()
            .unwrap()
            .id,
        *expected.last().unwrap()
    );
    let mirrored_latest: Uuid = sqlx::query_scalar(
        "SELECT id FROM answer WHERE notation_id = $1 AND question_id = $2 AND person_id = $3 \
         AND state_name = 'client_name' ORDER BY inserted_at DESC, id DESC LIMIT 1",
    )
    .bind(notation)
    .bind(question)
    .bind(client)
    .fetch_one(&pg.0)
    .await
    .unwrap();
    assert_eq!(mirrored_latest, *expected.last().unwrap());
    let report = parity(&surreal, &pg).await.unwrap();
    assert!(report.is_clean(), "{report:?}");
    pg.0.close().await;
}

async fn source_reset_fixture(pg: &PostgresDb) -> (SurrealDb, [Uuid; 2]) {
    let surreal = mem_surreal().await;
    let client = person(&surreal, Role::Client).await;
    let project = matter(&surreal, pg, "Synthetic reset census").await;
    let template = draft(&surreal).await;
    let first = open(&surreal, template, client, project).await;
    let second = open(&surreal, template, client, project).await;
    (surreal, [first, second])
}

async fn source_reset_asset(surreal: &SurrealDb, notation: Uuid) -> Uuid {
    let asset = crate::test_support::seed_body_asset(surreal, b"synthetic reset fact").await;
    surreal
        .query("UPDATE $id SET storage_key=$key")
        .bind(("id", record_id("asset", asset)))
        .bind(("key", format!("notations/{notation}/synthetic.txt")))
        .await
        .unwrap()
        .check()
        .unwrap();
    asset
}

#[tokio::test]
async fn source_only_notation_reset_census_precedes_parent_copy() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (surreal, ids) = source_reset_fixture(&pg).await;
    let asset = source_reset_asset(&surreal, ids[0]).await;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM notation")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "the captured Notes genuinely exist only in Source"
    );
    let (differences, proof) = reset_fact_differences(&surreal, &pg).await.unwrap();
    assert_eq!(proof.compared, ids.len());
    assert_eq!(proof.retained_native_protections, 0);
    assert_eq!(
        differences,
        vec![ids[0]],
        "Source protection must be imported"
    );

    mirrored(&surreal, &pg, &MirrorKey::Notation(ids[0])).await;
    mirrored(&surreal, &pg, &MirrorKey::Asset(asset)).await;
    let (differences, proof) = reset_fact_differences(&surreal, &pg).await.unwrap();
    assert!(differences.is_empty());
    assert_eq!(
        proof.compared, 2,
        "a still-missing unprotected parent is counted"
    );
    mirrored(&surreal, &pg, &MirrorKey::Notation(ids[1])).await;
    let (differences, proof) = reset_fact_differences(&surreal, &pg).await.unwrap();
    assert!(differences.is_empty());
    assert_eq!((proof.compared, proof.retained_native_protections), (2, 0));
    let original = super::super::from_source(crate::notations::find_by_id(&surreal, ids[0]))
        .await
        .unwrap()
        .unwrap();
    for _ in 0..255 {
        open(
            &surreal,
            original.template_id,
            original.person_id,
            original.project_id,
        )
        .await;
    }
    let (differences, proof) = reset_fact_differences(&surreal, &pg).await.unwrap();
    assert!(differences.is_empty());
    assert_eq!(
        proof.compared, 257,
        "the census crosses its bounded page boundary"
    );
    pg.pool().close().await;
}

#[tokio::test]
async fn native_reset_protection_does_not_mask_source_or_pg_only_drift() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (surreal, ids) = source_reset_fixture(&pg).await;
    for id in ids {
        mirrored(&surreal, &pg, &MirrorKey::Notation(id)).await;
    }
    sqlx::query("UPDATE template SET kind='onboarding' WHERE id=(SELECT template_id FROM notation WHERE id=$1)")
        .bind(ids[0]).execute(pg.pool()).await.unwrap();
    let envelope = Uuid::new_v4();
    sqlx::query("INSERT INTO signature(id,notation_id,provider,provider_id,state,signed_at,inserted_at,updated_at) VALUES($1,$2,'docusign',$3,'completed',now(),now(),now())")
        .bind(Uuid::new_v4()).bind(ids[0]).bind(envelope.to_string()).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO asset(id,storage_key,content_type,byte_size,sha256_hex,kind,project_id,metadata,inserted_at,updated_at) SELECT $1,$2,'application/pdf',1,$3,'onboarding',project_id,$4,now(),now() FROM notation WHERE id=$5")
        .bind(Uuid::new_v4()).bind(format!("notations/{}/signed.pdf",ids[0])).bind("a".repeat(64))
        .bind(serde_json::json!({"docusign_envelope_id":envelope.to_string(),"verified_onboarding_envelope_id":envelope.to_string()}))
        .bind(ids[0]).execute(pg.pool()).await.unwrap();
    let (differences, proof) = reset_fact_differences(&surreal, &pg).await.unwrap();
    assert!(differences.is_empty());
    assert_eq!((proof.compared, proof.retained_native_protections), (2, 1));

    let unknown = Uuid::new_v4();
    sqlx::query("INSERT INTO asset(id,storage_key,content_type,byte_size,sha256_hex,kind,project_id,metadata,inserted_at,updated_at) SELECT $1,$2,'application/pdf',1,$3,'onboarding',project_id,$4,now(),now() FROM notation WHERE id=$5")
        .bind(unknown).bind(format!("notations/{}/unknown.pdf",ids[1])).bind("b".repeat(64))
        .bind(serde_json::json!({"docusign_envelope_id":envelope.to_string(),"verified_onboarding_envelope_id":envelope.to_string()}))
        .bind(ids[1]).execute(pg.pool()).await.unwrap();
    let (differences, proof) = reset_fact_differences(&surreal, &pg).await.unwrap();
    assert_eq!(
        differences,
        vec![ids[1]],
        "one Note cannot borrow another Note's native evidence"
    );
    assert_eq!(proof.retained_native_protections, 1);
    sqlx::query("DELETE FROM asset WHERE id=$1")
        .bind(unknown)
        .execute(pg.pool())
        .await
        .unwrap();

    source_reset_asset(&surreal, ids[1]).await;
    let (differences, proof) = reset_fact_differences(&surreal, &pg).await.unwrap();
    assert_eq!(
        differences,
        vec![ids[1]],
        "native protection cannot hide missing Source protection"
    );
    assert_eq!((proof.compared, proof.retained_native_protections), (2, 1));
    let (_, extra, _) = crate::postgres::fixtures::workflow_fixture(&pg).await;
    let (_, proof) = reset_fact_differences(&surreal, &pg).await.unwrap();
    assert_eq!(
        proof.compared, 2,
        "PG-only Notes do not change the captured Source census"
    );
    let source_notes = super::super::from_source(crate::notations::list_all(&surreal))
        .await
        .unwrap();
    let notes = compare(
        "notation",
        &images(
            &source_notes,
            |row| MirrorKey::Notation(row.id),
            |row| Ok(NotationImage::from_source(row)),
        ),
        &target(
            &pg,
            NOTATION_SELECT.as_str(),
            NotationImage::from_row,
            |row| MirrorKey::Notation(row.id),
        )
        .await
        .unwrap(),
    );
    assert_eq!((notes.source_rows, notes.target_rows), (2, 3));
    assert!(
        notes.divergent.contains(&MirrorKey::Notation(extra)),
        "strict table parity refuses PG-only history"
    );
    pg.pool().close().await;
}
