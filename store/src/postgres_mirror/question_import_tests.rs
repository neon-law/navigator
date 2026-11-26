use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema};

#[tokio::test]
async fn captured_question_import_preserves_audience_and_replay() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let source = crate::test_support::mem_surreal().await;
    let question = Uuid::now_v7();
    let input = crate::questions::NewQuestion::new("refused", "Synthetic?", "string");
    assert!(
        super::super::from_source(crate::questions::create(&source, &input))
            .await
            .is_err()
    );
    assert!(super::super::from_source(crate::questions::set_audience(
        &source, "refused", "lawyer"
    ))
    .await
    .is_err());
    source.query("CREATE $id SET code='captured_question',prompt='Synthetic?',answer_type='string',audience='lawyer'")
        .bind(("id", record_id("question", question)))
        .await.unwrap().check().unwrap();
    let key = MirrorKey::Question(question);
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
    let held: (String, String) = sqlx::query_as("SELECT code, audience FROM question WHERE id=$1")
        .bind(question)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(held, ("captured_question".into(), "lawyer".into()));
    pg.pool().close().await;
}
