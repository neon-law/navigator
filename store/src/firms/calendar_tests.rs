use super::calendar::calendar_owners_with_bootstrap;
use super::{calendar_owner, set_calendar_owner, with_google_calendar};
use crate::{application_store::ApplicationStore, persons::Role};
use uuid::Uuid;

#[tokio::test]
async fn calendar_owner_bootstrap_is_scoped_and_saved_opt_out_wins() {
    crate::test_support::with_postgres(async {
        let source = ApplicationStore::uninitialized();
        let db = crate::postgres::PostgresDb::application().await.unwrap();
        let (_, anchor_firm) = crate::test_support::seed_calendar_project().await;
        let other = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO firm (name, slug, status) VALUES ('Other Calendar Firm', 'other-calendar-firm', 'active')
             RETURNING id",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        let name = crate::seed::FIRM_ENTITY_NAME;
        sqlx::query("UPDATE entity SET firm_anchor_key = $2 WHERE id = (SELECT entity_id FROM firm WHERE id = $1)")
            .bind(anchor_firm)
            .bind(crate::entity_commands::firm_anchor_key(name, name))
            .execute(db.pool()).await.unwrap();

        let rows = |bootstrap: &str| {
            let mut query = calendar_owners_with_bootstrap(Some(bootstrap.to_owned()));
            let db = db.clone();
            async move {
                query.push("SELECT firm_id, owner FROM calendar_owners WHERE owner IS NOT NULL ORDER BY firm_id")
                    .build_query_as::<(Uuid, String)>()
                    .fetch_all(db.pool()).await.unwrap()
            }
        };
        assert!(rows("  ").await.is_empty());
        assert_eq!(rows("bootstrap@calendar.test").await, [(anchor_firm, "bootstrap@calendar.test".into())]);
        set_calendar_owner(&source, Role::Owner, None, other, "other@calendar.test").await.unwrap();
        set_calendar_owner(&source, Role::Owner, None, anchor_firm, "saved@calendar.test").await.unwrap();
        assert_eq!(calendar_owner(anchor_firm).await.unwrap().as_deref(), Some("saved@calendar.test"));
        assert!(rows("bootstrap@calendar.test").await.contains(&(anchor_firm, "saved@calendar.test".into())));
        assert_eq!(with_google_calendar(vec![other, Uuid::nil(), anchor_firm, other]).await.unwrap(), [other, anchor_firm, other]);
        set_calendar_owner(&source, Role::Owner, None, anchor_firm, "").await.unwrap();
        assert_eq!(rows("bootstrap@calendar.test").await, [(other, "other@calendar.test".into())]);
    }).await;
}
