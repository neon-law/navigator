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

/// Every list-shaped notation read compares equal once the mirror has
/// copied the rows, and a row the mirror has not copied is a mismatch.
#[tokio::test]
async fn the_notation_list_shapes_shadow_a_match_after_the_mirror() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let client = person(&surreal, Role::Client).await;
    let project_a = matter(&surreal, &pg, "Synthetic Shadow Matter A").await;
    let project_b = matter(&surreal, &pg, "Synthetic Shadow Matter B").await;
    let template = draft(&surreal).await;
    let a = open(&surreal, template, client, project_a).await;
    let b = open(&surreal, template, client, project_b).await;
    assert_template_pin_repair(&surreal, &pg, template).await;
    mirrored(&surreal, &pg, &MirrorKey::Notation(a)).await;
    mirrored(&surreal, &pg, &MirrorKey::Notation(b)).await;
    let row_a = crate::notations::find_by_id(&surreal, a)
        .await
        .unwrap()
        .unwrap();
    let row_b = crate::notations::find_by_id(&surreal, b)
        .await
        .unwrap()
        .unwrap();

    for (shape, source) in [
        (NotationShape::ByProject(project_a), vec![row_a.clone()]),
        (
            NotationShape::ByProjects(vec![project_a, project_b]),
            vec![row_b.clone(), row_a.clone()],
        ),
        (
            NotationShape::ByPerson(client),
            vec![row_a.clone(), row_b.clone()],
        ),
        (NotationShape::All, vec![row_a.clone(), row_b.clone()]),
        (
            NotationShape::ByProjectTemplatePerson {
                project_id: project_a,
                template_id: template,
                person_id: client,
            },
            vec![row_a.clone()],
        ),
        (
            // The probe reads one row or none; a triple naming no
            // notation compares two empty answers.
            NotationShape::ByProjectTemplatePerson {
                project_id: project_a,
                template_id: Uuid::now_v7(),
                person_id: client,
            },
            Vec::new(),
        ),
    ] {
        let label = format!("{shape:?}");
        let comparison = shadow_notation_rows(&pg, shape, &source).await.unwrap();
        assert!(comparison.equal, "shape {label} diverges");
    }

    // The delete guard's boolean agrees for a Project whose notation the
    // mirror holds and for one it holds nothing for.
    assert!(
        shadow_notation_exists(&pg, project_a, true)
            .await
            .unwrap()
            .equal
    );
    assert!(
        shadow_notation_exists(&pg, Uuid::now_v7(), false)
            .await
            .unwrap()
            .equal
    );

    // A row SurrealDB still holds but the mirror has not copied yet —
    // the window the shadow exists to catch — is a mismatch, and the
    // exists probe reports the mirrored truth.
    open(&surreal, template, client, project_a).await;
    let rows = crate::notations::list_by_project(&surreal, project_a)
        .await
        .unwrap();
    let comparison = shadow_notation_rows(&pg, NotationShape::ByProject(project_a), &rows)
        .await
        .unwrap();
    assert!(!comparison.equal);
    // The mirrored row keeps the boolean probe in agreement: Surreal's
    // answer (true) matches PostgreSQL's mirrored truth (true) even
    // while the row shapes diverge.
    assert!(
        shadow_notation_exists(&pg, project_a, true)
            .await
            .unwrap()
            .equal
    );
    // A Project whose only notation is not yet mirrored drifts the
    // other way: SurrealDB answers true where PostgreSQL holds nothing.
    let project_c = matter(&surreal, &pg, "Synthetic Shadow Matter C").await;
    open(&surreal, template, client, project_c).await;
    assert!(
        !shadow_notation_exists(&pg, project_c, true)
            .await
            .unwrap()
            .equal
    );
    pg.0.close().await;
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
    crate::questions::create(
        surreal,
        &crate::questions::NewQuestion::new(
            format!("synthetic_{}", Uuid::now_v7().simple()),
            "Synthetic?",
            "string",
        ),
    )
    .await
    .unwrap()
    .id
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

#[tokio::test]
async fn a_shadowed_notation_read_matches_its_mirror_and_flags_drift() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let template = draft(&surreal).await;
    let client = person(&surreal, Role::Client).await;
    let project = matter(&surreal, &pg, "Synthetic Shadow Matter").await;
    let notation = open(&surreal, template, client, project).await;
    mirrored(&surreal, &pg, &MirrorKey::Notation(notation)).await;
    let source = crate::notations::find_by_id(&surreal, notation)
        .await
        .unwrap();
    let read = shadow_notation(&pg, notation, source.as_ref())
        .await
        .unwrap();
    assert_eq!((read.id, read.equal), (Some(notation), true));
    sqlx::query("UPDATE notation SET state = 'END' WHERE id = $1")
        .bind(notation)
        .execute(&pg.0)
        .await
        .unwrap();
    assert!(
        !shadow_notation(&pg, notation, source.as_ref())
            .await
            .unwrap()
            .equal
    );
}
