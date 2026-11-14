use super::postgres;
use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema};

async fn database() -> Option<PostgresDb> {
    let (options, database) = fresh_schema().await?;
    Some(connect(&options, &database).await)
}

/// A synthetic Person row for a transcript's owner.
pub(crate) async fn person(pg: &PostgresDb) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO person (id, name, email) VALUES ($1, 'Synthetic Person', $2)")
        .bind(id)
        .bind(format!("{id}@example.com"))
        .execute(&pg.0)
        .await
        .unwrap();
    id
}

fn new_turn(person_id: Uuid, kind: TurnKind, content: &str) -> NewTurn {
    NewTurn {
        person_id,
        kind,
        content: content.to_string(),
        tool_name: (kind == TurnKind::ToolCall).then(|| "search_case_law".to_string()),
        input_tokens: Some(3),
        output_tokens: None,
    }
}

#[tokio::test]
async fn history_is_the_owners_turns_in_append_order_and_purge_removes_them() {
    let Some(pg) = database().await else { return };
    let owner = person(&pg).await;
    let stranger = person(&pg).await;
    let mut appended = Vec::new();
    for (kind, content) in [
        (TurnKind::Prompt, "Is my lease assignable?"),
        (TurnKind::ToolCall, "{\"query\":\"lease\"}"),
        (TurnKind::ToolResult, "{\"cases\":[]}"),
        (TurnKind::Reply, "Generally, yes."),
    ] {
        appended.push(
            postgres::append(&pg, &new_turn(owner, kind, content))
                .await
                .unwrap(),
        );
    }
    postgres::append(&pg, &new_turn(stranger, TurnKind::Prompt, "other"))
        .await
        .unwrap();

    assert_eq!(postgres::history(&pg, owner).await.unwrap(), appended);
    assert_eq!(appended[1].tool_name.as_deref(), Some("search_case_law"));
    assert_eq!(appended[0].input_tokens, Some(3));

    assert_eq!(postgres::purge(&pg, owner).await.unwrap(), 4);
    assert!(postgres::history(&pg, owner).await.unwrap().is_empty());
    assert_eq!(postgres::history(&pg, stranger).await.unwrap().len(), 1);
    assert_eq!(postgres::purge(&pg, owner).await.unwrap(), 0);
    pg.0.close().await;
}

#[tokio::test]
async fn surreal_turns_copy_once_with_their_identity() {
    let Some(pg) = database().await else { return };
    let surreal = crate::test_support::mem_surreal().await;
    let owner = person(&pg).await;
    let turn = append(&surreal, new_turn(owner, TurnKind::Prompt, "earlier"))
        .await
        .unwrap();

    let copied = postgres::copy_from_surreal(&surreal, &pg).await.unwrap();
    assert_eq!((copied.applied, copied.failures.len()), (1, 0));
    assert_eq!(
        postgres::copy_from_surreal(&surreal, &pg)
            .await
            .unwrap()
            .applied,
        0
    );
    let copied_turn = postgres::history(&pg, owner).await.unwrap();
    assert_eq!(copied_turn.len(), 1);
    assert_eq!(copied_turn[0].id, turn.id);
    assert_eq!(copied_turn[0].content, turn.content);

    // A turn whose owner PostgreSQL does not hold is refused by id, never
    // by content.
    let orphan = append(
        &surreal,
        new_turn(Uuid::now_v7(), TurnKind::Prompt, "Synthetic secret"),
    )
    .await
    .unwrap();
    let refused = postgres::copy_from_surreal(&surreal, &pg).await.unwrap();
    assert_eq!(refused.applied, 0);
    assert_eq!(refused.failures.len(), 1);
    let (table, id, reason) = &refused.failures[0];
    assert_eq!(*table, TABLE);
    assert_eq!(id.as_deref(), Some(orphan.id.to_string().as_str()));
    assert!(reason.contains("23503"), "{reason}");
    assert!(!reason.contains("Synthetic secret"));
    pg.0.close().await;
}
