//! The sandbox transcript's owner-scope and retention rules.
//!
//! The transcript is the subscriber's own workspace content: every read
//! is scoped to the owner, the four glossary turn kinds round-trip
//! through the engine, and the purge seam removes one subscriber's whole
//! chat in one statement — the property the retention policy will stand
//! on. The order tests pin what the session loop relies on: turns come
//! back in the order they were appended, with no sequence column to race.

use store::assistant_turns::{self, NewTurn, TurnKind};
use store::surreal::test_support::mem;
use uuid::Uuid;

fn turn(person_id: Uuid, kind: TurnKind, content: &str) -> NewTurn {
    NewTurn {
        person_id,
        kind,
        content: content.to_string(),
        tool_name: None,
        input_tokens: None,
        output_tokens: None,
    }
}

#[tokio::test]
async fn append_stores_the_turn_and_history_returns_it() {
    let db = mem().await;
    let who = Uuid::now_v7();

    let stored =
        assistant_turns::append(&db, turn(who, TurnKind::Prompt, "What is a quiet title?"))
            .await
            .expect("the prompt stored");

    assert_eq!(stored.person_id, who);
    assert_eq!(stored.kind, TurnKind::Prompt);
    assert_eq!(stored.content, "What is a quiet title?");

    let history = assistant_turns::history(&db, who).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, stored.id, "history returns the same row");
    assert_eq!(history[0].content, "What is a quiet title?");
}

#[tokio::test]
async fn history_is_chronological_across_appends() {
    let db = mem().await;
    let who = Uuid::now_v7();

    assistant_turns::append(&db, turn(who, TurnKind::Prompt, "first"))
        .await
        .unwrap();
    assistant_turns::append(&db, turn(who, TurnKind::Reply, "second"))
        .await
        .unwrap();
    assistant_turns::append(&db, turn(who, TurnKind::Prompt, "third"))
        .await
        .unwrap();

    let history = assistant_turns::history(&db, who).await.unwrap();
    let contents: Vec<&str> = history.iter().map(|t| t.content.as_str()).collect();
    assert_eq!(contents, ["first", "second", "third"]);
}

#[tokio::test]
async fn history_is_scoped_to_the_owner() {
    let db = mem().await;
    let pat = Uuid::now_v7();
    let sam = Uuid::now_v7();

    assistant_turns::append(&db, turn(pat, TurnKind::Prompt, "pat asked"))
        .await
        .unwrap();
    assistant_turns::append(&db, turn(sam, TurnKind::Prompt, "sam asked"))
        .await
        .unwrap();

    let pat_history = assistant_turns::history(&db, pat).await.unwrap();
    assert_eq!(pat_history.len(), 1);
    assert_eq!(pat_history[0].content, "pat asked");

    let sam_history = assistant_turns::history(&db, sam).await.unwrap();
    assert_eq!(sam_history.len(), 1);
    assert_eq!(sam_history[0].content, "sam asked");
}

#[tokio::test]
async fn all_four_kinds_round_trip_with_tool_fields_and_token_counts() {
    let db = mem().await;
    let who = Uuid::now_v7();

    assistant_turns::append(
        &db,
        NewTurn {
            person_id: who,
            kind: TurnKind::Reply,
            content: "Here is what I found.".into(),
            tool_name: None,
            input_tokens: Some(1200),
            output_tokens: Some(300),
        },
    )
    .await
    .unwrap();
    assistant_turns::append(
        &db,
        NewTurn {
            person_id: who,
            kind: TurnKind::ToolCall,
            content: r#"{"query":"quiet title nevada"}"#.into(),
            tool_name: Some("search_case_law".into()),
            input_tokens: None,
            output_tokens: None,
        },
    )
    .await
    .unwrap();
    assistant_turns::append(
        &db,
        NewTurn {
            person_id: who,
            kind: TurnKind::ToolResult,
            content: r#"{"results":[]}"#.into(),
            tool_name: Some("search_case_law".into()),
            input_tokens: None,
            output_tokens: None,
        },
    )
    .await
    .unwrap();

    let history = assistant_turns::history(&db, who).await.unwrap();
    assert_eq!(history.len(), 3);

    assert_eq!(history[0].kind, TurnKind::Reply);
    assert_eq!(history[0].input_tokens, Some(1200));
    assert_eq!(history[0].output_tokens, Some(300));

    assert_eq!(history[1].kind, TurnKind::ToolCall);
    assert_eq!(history[1].tool_name.as_deref(), Some("search_case_law"));

    assert_eq!(history[2].kind, TurnKind::ToolResult);
    assert_eq!(history[2].tool_name.as_deref(), Some("search_case_law"));
}

#[tokio::test]
async fn purge_removes_only_that_subscribers_turns_and_repeats_as_zero() {
    let db = mem().await;
    let pat = Uuid::now_v7();
    let sam = Uuid::now_v7();

    assistant_turns::append(&db, turn(pat, TurnKind::Prompt, "p1"))
        .await
        .unwrap();
    assistant_turns::append(&db, turn(pat, TurnKind::Reply, "p2"))
        .await
        .unwrap();
    assistant_turns::append(&db, turn(sam, TurnKind::Prompt, "s1"))
        .await
        .unwrap();

    let removed = assistant_turns::purge(&db, pat).await.unwrap();
    assert_eq!(removed, 2);
    assert!(assistant_turns::history(&db, pat).await.unwrap().is_empty());
    assert_eq!(
        assistant_turns::history(&db, sam).await.unwrap().len(),
        1,
        "the purge is scoped to one owner"
    );

    let again = assistant_turns::purge(&db, pat).await.unwrap();
    assert_eq!(again, 0, "purging an already-empty chat removes nothing");
}

/// The schema carries the kind guard, not just the Rust enum: a writer
/// that bypassed this module cannot store a kind the glossary does not
/// define.
#[tokio::test]
async fn the_schema_refuses_a_kind_the_glossary_does_not_define() {
    let db = mem().await;
    let who = Uuid::now_v7();

    let result = db
        .query("CREATE assistant_turn SET person_id = $person_id, kind = 'nonsense', content = ''")
        .bind(("person_id", store::surreal::record_id("person", who)))
        .await;

    assert!(result.is_err(), "the schema must refuse an undefined kind");
}
