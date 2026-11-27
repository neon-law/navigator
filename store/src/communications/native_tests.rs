//! Native logged-call mapping and client-visibility regression (LAW-424).
use super::*;

#[tokio::test]
async fn a_logged_call_carries_its_direction_and_role_at_the_top_level() {
    crate::test_support::with_postgres(async {
        let pg = crate::postgres::PostgresDb::application().await.unwrap();
        let (_, notation_id, _) = crate::postgres::fixtures::workflow_fixture(&pg).await;
        let project_id: uuid::Uuid =
            sqlx::query_scalar("SELECT project_id FROM notation WHERE id=$1")
                .bind(notation_id)
                .fetch_one(pg.pool())
                .await
                .unwrap();
        let application_store = crate::application_store::ApplicationStore::uninitialized();

        let call = record_call(
            &application_store,
            &NewCall {
                project_id,
                counterparty: "client@example.com".into(),
                body: "Discussed the engagement.".into(),
                at: "2026-10-09T15:00:00-07:00".into(),
                call: CallDetails {
                    direction: "inbound".into(),
                    counterparty_role: "client".into(),
                    duration_minutes: Some(20),
                    recorded: "none".into(),
                    meet_and_confer: None,
                    client_visible: false,
                },
                agreements: vec![],
                ..NewCall::default()
            },
        )
        .await
        .expect("log the call");
        assert_eq!(call.direction, "inbound");
        assert_eq!(call.counterparty_role.as_deref(), Some("client"));
        assert_eq!(call.call.as_ref().unwrap().direction, "inbound");

        // The firm kept the note internal, so the client lens hides it.
        assert!(for_project_client_visible(&application_store, project_id)
            .await
            .unwrap()
            .is_empty());
        let persisted = for_project(&application_store, project_id).await.unwrap();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].id, call.id);
        assert_eq!(persisted[0].direction, "inbound");
        assert_eq!(persisted[0].counterparty_role.as_deref(), Some("client"));
        assert_eq!(persisted[0].call.as_ref().unwrap().direction, "inbound");
    })
    .await;
}
