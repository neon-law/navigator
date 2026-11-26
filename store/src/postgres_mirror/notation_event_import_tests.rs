use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema};

#[tokio::test]
async fn captured_event_import_preserves_actor_template_and_replay() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (actor, notation, _) = crate::postgres::fixtures::workflow_fixture(&pg).await;
    let template: Uuid = sqlx::query_scalar("SELECT template_id FROM notation WHERE id=$1")
        .bind(notation)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    let source = crate::test_support::mem_surreal().await;
    let event = Uuid::now_v7();
    let record = crate::notation_events::TransitionRecord {
        notation_id: notation,
        acting_person_id: Some(actor),
        machine_kind: "questionnaire",
        from_state: "BEGIN",
        to_state: "END",
        condition: "_",
        payload_json: None,
        recorded_at: "2026-01-02T00:00:00+00:00",
    };
    assert!(
        super::super::from_source(crate::notation_events::append_event(&source, record))
            .await
            .is_err()
    );
    source.query("CREATE $id SET notation_id=$notation, acting_person_id=$actor, template_version_id=$template, machine_kind='questionnaire', from_state='BEGIN', to_state='END', condition='_', recorded_at='2026-01-02T00:00:00+00:00'")
        .bind(("id", record_id("notation_event", event)))
        .bind(("notation", record_id("notation", notation)))
        .bind(("actor", record_id("person", actor)))
        .bind(("template", record_id("template", template)))
        .await.unwrap().check().unwrap();
    let key = MirrorKey::NotationEvent(event);
    super::super::during_initial_copy(async {
        assert_eq!(
            super::super::mirror(&source, &pg, &key).await.unwrap(),
            Mirrored::Written
        );
        assert_eq!(
            super::super::mirror(&source, &pg, &key).await.unwrap(),
            Mirrored::Unchanged
        );
    })
    .await;
    let held: (Uuid, Uuid, Uuid, String) = sqlx::query_as("SELECT notation_id, acting_person_id, template_version_id, to_state FROM notation_event WHERE id=$1")
        .bind(event).fetch_one(pg.pool()).await.unwrap();
    assert_eq!(held, (notation, actor, template, "END".into()));
    pg.pool().close().await;
}
