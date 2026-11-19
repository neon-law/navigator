use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema};

#[tokio::test]
async fn captured_mail_preserves_native_drift_and_missing_source_history() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let source = crate::surreal::test_support::mem().await;
    let address = Uuid::now_v7();
    let mailroom = Uuid::now_v7();
    let letter = Uuid::now_v7();
    source.query("CREATE $address SET line1='100 Synthetic Way',city='Reno',region='NV',postal_code='89501',country='US'; CREATE $mailroom SET name='Synthetic captured mailroom',address_id=$address; CREATE $letter SET mailroom_id=$mailroom,direction='incoming',sender='Synthetic sender',recipient='Synthetic recipient',summary='Captured history'")
        .bind(("address", crate::surreal::record_id("address", address)))
        .bind(("mailroom", crate::surreal::record_id("mailroom", mailroom)))
        .bind(("letter", crate::surreal::record_id("letter", letter)))
        .await.unwrap().check().unwrap();
    super::super::during_initial_copy(async {
        for key in [MirrorKey::Mailroom(mailroom), MirrorKey::Letter(letter)] {
            assert_eq!(mirror(&source, &pg, &key).await.unwrap(), Mirrored::Written);
            assert_eq!(
                mirror(&source, &pg, &key).await.unwrap(),
                Mirrored::Unchanged
            );
        }
        assert!(parity(&source, &pg).await.unwrap().is_clean());
        sqlx::query("DELETE FROM letter WHERE id=$1")
            .bind(letter)
            .execute(pg.pool())
            .await
            .unwrap();
        assert!(reconcile(&source, &pg).await.unwrap().is_clean());
        sqlx::query("UPDATE letter SET summary='Native retained history' WHERE id=$1")
            .bind(letter)
            .execute(pg.pool())
            .await
            .unwrap();
        assert_eq!(
            mirror(&source, &pg, &MirrorKey::Letter(letter))
                .await
                .unwrap(),
            Mirrored::Unchanged
        );
        assert!(!reconcile(&source, &pg).await.unwrap().is_clean());
        source
            .query("DELETE $letter; DELETE $mailroom")
            .bind(("letter", crate::surreal::record_id("letter", letter)))
            .bind(("mailroom", crate::surreal::record_id("mailroom", mailroom)))
            .await
            .unwrap()
            .check()
            .unwrap();
        assert!(!reconcile(&source, &pg).await.unwrap().is_clean());
        let retained: (String, Option<Uuid>) =
            sqlx::query_as("SELECT summary,project_id FROM letter WHERE id=$1")
                .bind(letter)
                .fetch_one(pg.pool())
                .await
                .unwrap();
        assert_eq!(retained, ("Native retained history".into(), None));
        let parents: i64 = sqlx::query_scalar("SELECT count(*) FROM mailroom WHERE id=$1")
            .bind(mailroom)
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert_eq!(parents, 1);
    })
    .await;
    pg.pool().close().await;
}
