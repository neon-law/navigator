use super::*;
use crate::answers::NewAnswer;
use crate::notation_events::{TransitionRecord, MACHINE_QUESTIONNAIRE, MACHINE_WORKFLOW};
use crate::notations::NewNotation;
use crate::persons::{NewPerson, Role};
use crate::postgres_mirror::tests::{connect, fresh_schema, mirror_identity};
use crate::test_support::mem_surreal;

async fn mirrored(surreal: &SurrealDb, pg: &PostgresDb, key: &MirrorKey) -> Mirrored {
    super::super::mirror(surreal, pg, key).await.unwrap()
}

#[tokio::test]
async fn notation_clauses_keep_author_and_render_order_across_edits() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let client = person(&surreal, Role::Client).await;
    let project = matter(&surreal, &pg, "Synthetic Clause Matter").await;
    let notation = open(&surreal, draft(&surreal).await, client, project).await;
    mirrored(&surreal, &pg, &MirrorKey::Notation(notation)).await;

    let lawyer = person(&surreal, Role::Lawyer).await;
    let (first, second) = append_and_check_notation_clauses(&surreal, &pg, notation, lawyer).await;
    edit_and_check_notation_clauses(&surreal, &pg, first, second).await;
    reconcile_deleted_notation_clause(&surreal, &pg, notation, first, second).await;
    delete_notation_and_check_clause_cascade(&surreal, &pg, notation).await;
    pg.0.close().await;
}

async fn append_and_check_notation_clauses(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    notation: Uuid,
    lawyer: Uuid,
) -> (Uuid, Uuid) {
    let first =
        crate::notation_clauses::append(surreal, notation, "First synthetic clause.", Some(lawyer))
            .await
            .unwrap();
    let second = crate::notation_clauses::append(surreal, notation, "Second clause.", None)
        .await
        .unwrap();
    for id in [first, second] {
        mirrored(surreal, pg, &MirrorKey::NotationClause(id)).await;
    }
    let ordered: Vec<(Uuid, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT id, body_markdown, authored_by_person_id FROM notation_clause \
         WHERE notation_id = $1 ORDER BY position, id",
    )
    .bind(notation)
    .fetch_all(&pg.0)
    .await
    .unwrap();
    assert_eq!(
        ordered,
        [
            (first, "First synthetic clause.".into(), Some(lawyer)),
            (second, "Second clause.".into(), None)
        ]
    );
    (first, second)
}

async fn edit_and_check_notation_clauses(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    first: Uuid,
    second: Uuid,
) {
    assert!(crate::notation_clauses::move_clause(surreal, second, true)
        .await
        .unwrap());
    for id in [first, second] {
        mirrored(surreal, pg, &MirrorKey::NotationClause(id)).await;
    }
    crate::notation_clauses::update_body(surreal, first, "Updated synthetic clause.")
        .await
        .unwrap();
    mirrored(surreal, pg, &MirrorKey::NotationClause(first)).await;
    let report = super::super::notation_clauses::parity(surreal, pg)
        .await
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
}

async fn reconcile_deleted_notation_clause(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    notation: Uuid,
    first: Uuid,
    second: Uuid,
) {
    assert!(crate::notation_clauses::delete(surreal, first)
        .await
        .unwrap());
    let drift = super::super::notation_clauses::parity(surreal, pg)
        .await
        .unwrap();
    assert_eq!(
        drift.tables[0].divergent,
        [MirrorKey::NotationClause(first)]
    );
    let repaired = super::super::notation_clauses::reconcile(surreal, pg)
        .await
        .unwrap();
    assert!(repaired.is_clean(), "{repaired:?}");
    assert!(sqlx::query_scalar::<_, bool>(
        "SELECT NOT EXISTS (SELECT 1 FROM notation_clause WHERE id = $1)",
    )
    .bind(first)
    .fetch_one(&pg.0)
    .await
    .unwrap());
    let remaining: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, body_markdown FROM notation_clause WHERE notation_id = $1 ORDER BY position, id",
    )
    .bind(notation)
    .fetch_all(&pg.0)
    .await
    .unwrap();
    assert_eq!(remaining, [(second, "Second clause.".into())]);
}

async fn delete_notation_and_check_clause_cascade(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    notation: Uuid,
) {
    assert_eq!(
        crate::notation_reset::delete_draft(surreal, notation)
            .await
            .unwrap(),
        crate::notation_reset::ResetOutcome::Deleted
    );
    assert_eq!(
        mirrored(surreal, pg, &MirrorKey::Notation(notation)).await,
        Mirrored::Deleted
    );
    let parent_removed = super::super::notation_clauses::parity(surreal, pg)
        .await
        .unwrap();
    assert!(parent_removed.is_clean(), "{parent_removed:?}");
    let remaining_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM notation_clause WHERE notation_id = $1")
            .bind(notation)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(remaining_count, 0);
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

async fn transition(surreal: &SurrealDb, notation: Uuid, kind: &str, from: &str, to: &str) -> Uuid {
    crate::notation_events::append_event(
        surreal,
        TransitionRecord {
            notation_id: notation,
            acting_person_id: None,
            machine_kind: kind,
            from_state: from,
            to_state: to,
            condition: "_",
            payload_json: None,
            recorded_at: "2026-01-02T00:00:00+00:00",
        },
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

fn rows(report: &ParityReport) -> Vec<usize> {
    report
        .tables
        .iter()
        .map(|table| table.target_rows)
        .collect()
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

#[test]
fn workflow_keys_name_rows_without_values() {
    let id = Uuid::nil();
    assert_eq!(MirrorKey::Notation(id).table(), "notation");
    assert_eq!(MirrorKey::NotationEvent(id).table(), "notation_event");
    assert_eq!(MirrorKey::Answer(id).table(), "answer");
    assert_eq!(
        MirrorKey::Answer(id).to_string(),
        "answer:00000000-0000-0000-0000-000000000000"
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one questionnaire's lifecycle reads in order"
)]
async fn notations_events_and_answers_converge_with_order_and_provenance() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let template = draft(&surreal).await;
    let client = person(&surreal, Role::Client).await;
    let lawyer = person(&surreal, Role::Lawyer).await;
    let ours = matter(&surreal, &pg, "Synthetic Matter").await;
    let theirs = matter(&surreal, &pg, "Other Synthetic Matter").await;

    // A Notation brings its Project and respondent with it.
    let notation = open(&surreal, template, client, ours).await;
    let other = open(&surreal, template, client, theirs).await;
    for id in [notation, other] {
        let key = MirrorKey::Notation(id);
        assert_eq!(mirrored(&surreal, &pg, &key).await, Mirrored::Written);
        assert_eq!(mirrored(&surreal, &pg, &key).await, Mirrored::Unchanged);
    }
    // Notation isolation: each Project reads only its own.
    let held: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM notation WHERE project_id = $1")
        .bind(ours)
        .fetch_all(&pg.0)
        .await
        .unwrap();
    assert_eq!(held, [notation]);
    crate::notations::update_state(&surreal, notation, "lawyer_review")
        .await
        .unwrap();
    assert_eq!(
        mirrored(&surreal, &pg, &MirrorKey::Notation(notation)).await,
        Mirrored::Written
    );

    // The journal: the newest event per (notation, machine kind) is the
    // current state in both stores.
    let mut events = Vec::new();
    for (kind, from, to) in [
        (MACHINE_QUESTIONNAIRE, "BEGIN", "client_name"),
        (MACHINE_QUESTIONNAIRE, "client_name", "END"),
        (MACHINE_WORKFLOW, "BEGIN", "lawyer_review"),
    ] {
        events.push(transition(&surreal, notation, kind, from, to).await);
    }
    for event in &events {
        let key = MirrorKey::NotationEvent(*event);
        assert_eq!(mirrored(&surreal, &pg, &key).await, Mirrored::Written);
        assert_eq!(mirrored(&surreal, &pg, &key).await, Mirrored::Unchanged);
    }
    for kind in [MACHINE_QUESTIONNAIRE, MACHINE_WORKFLOW] {
        let latest: String = sqlx::query_scalar(
            "SELECT to_state FROM notation_event WHERE notation_id = $1 AND machine_kind = $2 \
             ORDER BY inserted_at DESC, id DESC LIMIT 1",
        )
        .bind(notation)
        .bind(kind)
        .fetch_one(&pg.0)
        .await
        .unwrap();
        let source = crate::notation_events::latest_for_kind(&surreal, notation, kind)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(latest, source.to_state);
    }

    // Append-only corrections: the client answers, the lawyer corrects on
    // the client's behalf. Both rows stay; the newest wins; respondent and
    // author stay distinct.
    // An answer brings its Question with it.
    let question = question(&surreal).await;
    let mut answers = Vec::new();
    for (value, source, author) in [
        ("Synthetic Name", "client", client),
        ("Synthetic Name Corrected", "lawyer", lawyer),
    ] {
        let answer = crate::answers::record(
            &surreal,
            &NewAnswer::new(question, client, crate::answers::primitive(value))
                .in_notation(notation, "client_name")
                .authored_by(source, Some(author)),
        )
        .await
        .unwrap();
        answers.push(answer.id);
        let key = MirrorKey::Answer(answer.id);
        assert_eq!(mirrored(&surreal, &pg, &key).await, Mirrored::Written);
        assert_eq!(mirrored(&surreal, &pg, &key).await, Mirrored::Unchanged);
    }
    let latest: (Uuid, Uuid, Option<Uuid>, String) = sqlx::query_as(
        "SELECT id, person_id, authored_by_person_id, source FROM answer \
         WHERE question_id = $1 AND state_name = $2 AND person_id = $3 AND notation_id = $4 \
         ORDER BY inserted_at DESC, id DESC LIMIT 1",
    )
    .bind(question)
    .bind("client_name")
    .bind(client)
    .bind(notation)
    .fetch_one(&pg.0)
    .await
    .unwrap();
    let source =
        crate::answers::latest_for_state(&surreal, question, "client_name", client, notation)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        latest,
        (source.id, client, Some(lawyer), "lawyer".to_string())
    );
    let report = parity(&surreal, &pg).await.unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(rows(&report), [2, 3, 2]);

    // A reset draft takes its events and answers with it.
    let reset = open(&surreal, template, client, ours).await;
    let reset_event = transition(
        &surreal,
        reset,
        MACHINE_QUESTIONNAIRE,
        "BEGIN",
        "client_name",
    )
    .await;
    let reset_answer = crate::answers::record(
        &surreal,
        &NewAnswer::new(question, client, crate::answers::primitive("Draft"))
            .in_notation(reset, "client_name"),
    )
    .await
    .unwrap()
    .id;
    for key in [
        MirrorKey::Notation(reset),
        MirrorKey::NotationEvent(reset_event),
        MirrorKey::Answer(reset_answer),
    ] {
        mirrored(&surreal, &pg, &key).await;
    }
    assert_eq!(
        crate::notation_reset::delete_draft(&surreal, reset)
            .await
            .unwrap(),
        crate::notation_reset::ResetOutcome::Deleted
    );
    let key = MirrorKey::Notation(reset);
    assert_eq!(mirrored(&surreal, &pg, &key).await, Mirrored::Deleted);
    assert_eq!(mirrored(&surreal, &pg, &key).await, Mirrored::Unchanged);
    let report = parity(&surreal, &pg).await.unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(rows(&report), [2, 3, 2]);
}

#[tokio::test]
async fn parity_names_divergent_workflow_rows_and_reconcile_repairs_them() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let template = draft(&surreal).await;
    let client = person(&surreal, Role::Client).await;
    let project = matter(&surreal, &pg, "Synthetic Matter").await;
    let notation = open(&surreal, template, client, project).await;
    mirrored(&surreal, &pg, &MirrorKey::Notation(notation)).await;
    // Never mirrored: missing.
    let event = transition(&surreal, notation, MACHINE_QUESTIONNAIRE, "BEGIN", "END").await;
    let answer = crate::answers::record(
        &surreal,
        &NewAnswer::new(
            question(&surreal).await,
            client,
            crate::answers::primitive("Synthetic"),
        )
        .in_notation(notation, "client_name"),
    )
    .await
    .unwrap()
    .id;
    // Drifted in PostgreSQL: differing.
    sqlx::query("UPDATE notation SET state = 'END' WHERE id = $1")
        .bind(notation)
        .execute(&pg.0)
        .await
        .unwrap();
    // Held only by PostgreSQL: extra.
    let stale = Uuid::new_v4();
    let stale_question = question(&surreal).await;
    super::super::mirror(&surreal, &pg, &MirrorKey::Question(stale_question))
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO answer (id, question_id, person_id, notation_id, value) \
         VALUES ($1, $2, $3, $4, '\"Stale\"')",
    )
    .bind(stale)
    .bind(stale_question)
    .bind(client)
    .bind(notation)
    .execute(&pg.0)
    .await
    .unwrap();

    let report = parity(&surreal, &pg).await.unwrap();
    assert_eq!(report.tables[0].divergent, [MirrorKey::Notation(notation)]);
    assert_eq!(
        report.tables[1].divergent,
        [MirrorKey::NotationEvent(event)]
    );
    let mut expected = vec![MirrorKey::Answer(answer), MirrorKey::Answer(stale)];
    expected.sort();
    assert_eq!(report.tables[2].divergent, expected);
    // The report carries keys and counts, never an answer's value.
    assert!(!format!("{report:?}").contains("Synthetic"));

    let repaired = reconcile(&surreal, &pg).await.unwrap();
    assert!(repaired.is_clean(), "{repaired:?}");
    assert_eq!(rows(&repaired), [1, 1, 1]);
    let replay = reconcile(&surreal, &pg).await.unwrap();
    assert_eq!(replay, repaired);
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one version-pin lifecycle includes retirement and constraint checks"
)]
async fn workflow_template_versions_are_mirrored_and_retained() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let template = draft(&surreal).await;
    let event_template = draft(&surreal).await;
    let client = person(&surreal, Role::Client).await;
    let project = matter(&surreal, &pg, "Synthetic Matter").await;
    let notation = open(&surreal, template, client, project).await;
    mirrored(&surreal, &pg, &MirrorKey::Notation(notation)).await;
    let event = transition(&surreal, notation, MACHINE_WORKFLOW, "BEGIN", "END").await;
    surreal
        .query("UPDATE $id SET template_version_id = $template")
        .bind(("id", crate::surreal::record_id("notation_event", event)))
        .bind((
            "template",
            crate::surreal::record_id("template", event_template),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();
    mirrored(&surreal, &pg, &MirrorKey::NotationEvent(event)).await;

    let original = crate::templates::find_by_id(&surreal, template)
        .await
        .unwrap()
        .unwrap();
    let replacement = crate::templates::save_version(
        &surreal,
        None,
        &original.code,
        crate::templates::Version {
            title: "Synthetic Revised Draft".into(),
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
    .id;
    mirrored(&surreal, &pg, &MirrorKey::Template(template)).await;
    mirrored(&surreal, &pg, &MirrorKey::Template(replacement)).await;
    let pinned: Uuid = sqlx::query_scalar("SELECT template_id FROM notation WHERE id = $1")
        .bind(notation)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(
        pinned, template,
        "a new current version cannot move an existing pin"
    );
    let current: bool = sqlx::query_scalar("SELECT is_current FROM template WHERE id = $1")
        .bind(template)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert!(!current, "the pinned version is retained after retirement");

    let versions: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM template ORDER BY id")
        .fetch_all(&pg.0)
        .await
        .unwrap();
    let mut expected = vec![template, event_template, replacement];
    expected.sort();
    assert_eq!(
        versions, expected,
        "both exact version parents are mirrored"
    );
    for query in [
        "UPDATE notation SET template_id = $1 WHERE id = $2",
        "UPDATE notation_event SET template_version_id = $1 WHERE id = $2",
    ] {
        let row = if query.contains("notation_event") {
            event
        } else {
            notation
        };
        let error = sqlx::query(query)
            .bind(Uuid::new_v4())
            .bind(row)
            .execute(&pg.0)
            .await
            .unwrap_err();
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some("23503")
        );
    }
    for version in [template, event_template] {
        let error = sqlx::query("DELETE FROM template WHERE id = $1")
            .bind(version)
            .execute(&pg.0)
            .await
            .unwrap_err();
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some("23503")
        );
    }
    assert!(parity(&surreal, &pg).await.unwrap().is_clean());
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
