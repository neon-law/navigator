use super::*;
use crate::addresses::NewAddress;
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::test_support::mem_surreal;

type PersistedLetter = (
    Uuid,
    Uuid,
    Option<Uuid>,
    String,
    String,
    String,
    String,
    DateTime<Utc>,
    DateTime<Utc>,
);

#[test]
fn keys_name_rows_without_copying_mail_content() {
    let id = Uuid::nil();
    assert_eq!(MirrorKey::Mailroom(id).table(), "mailroom");
    assert_eq!(
        MirrorKey::Mailroom(id).to_string(),
        format!("mailroom:{id}")
    );
    assert_eq!(MirrorKey::Letter(id).table(), "letter");
    assert_eq!(MirrorKey::Letter(id).to_string(), format!("letter:{id}"));
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep the end-to-end source/reset/reseed lifecycle together.
async fn source_rows_round_trip_and_reconcile_by_their_existing_ids() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let address = crate::addresses::create(
        &surreal,
        &NewAddress {
            line1: "100 Synthetic Way".into(),
            city: "Reno".into(),
            region: "NV".into(),
            postal_code: "89501".into(),
            country: "US".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mailroom = crate::mailrooms::create(&surreal, "Synthetic HQ", address.id)
        .await
        .unwrap();
    let project_id = Uuid::new_v4(); // Deliberately dangling; source Letter permits it.
    let letter = crate::letters::record_with_id(
        &surreal,
        Uuid::new_v4(),
        &crate::letters::NewLetter {
            mailroom_id: mailroom.id,
            project_id: Some(project_id),
            direction: "outgoing".into(),
            sender: "Synthetic Sender".into(),
            recipient: "Synthetic Recipient".into(),
            summary: "Synthetic summary".into(),
        },
    )
    .await
    .unwrap();

    assert_eq!(
        mirror(&surreal, &pg, &MirrorKey::Mailroom(mailroom.id))
            .await
            .unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        mirror(&surreal, &pg, &MirrorKey::Letter(letter.id))
            .await
            .unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        mirror(&surreal, &pg, &MirrorKey::Letter(letter.id))
            .await
            .unwrap(),
        Mirrored::Unchanged
    );
    assert!(parity(&surreal, &pg).await.unwrap().is_clean());
    let persisted: PersistedLetter = sqlx::query_as(
        "SELECT id,mailroom_id,project_id,direction,sender,recipient,summary,inserted_at,updated_at FROM letter WHERE id=$1",
    ).bind(letter.id).fetch_one(&pg.0).await.unwrap();
    assert_eq!(
        persisted,
        (
            letter.id,
            mailroom.id,
            Some(project_id),
            "outgoing".into(),
            "Synthetic Sender".into(),
            "Synthetic Recipient".into(),
            "Synthetic summary".into(),
            super::super::micros(letter.inserted_at),
            super::super::micros(letter.updated_at)
        )
    );

    sqlx::query("DELETE FROM letter WHERE id=$1")
        .bind(letter.id)
        .execute(&pg.0)
        .await
        .unwrap();
    let drift = parity(&surreal, &pg).await.unwrap();
    assert!(drift
        .tables
        .iter()
        .any(|table| table.table == "letter"
            && table.divergent.contains(&MirrorKey::Letter(letter.id))));
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());

    surreal
        .query("DELETE $id")
        .bind(("id", crate::surreal::record_id("letter", letter.id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    surreal
        .query("DELETE $id")
        .bind(("id", crate::surreal::record_id("mailroom", mailroom.id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    let replacement = crate::mailrooms::create(&surreal, "Synthetic HQ", address.id)
        .await
        .unwrap();
    let replacement_letter = crate::letters::record_with_id(
        &surreal,
        Uuid::new_v4(),
        &crate::letters::NewLetter {
            mailroom_id: replacement.id,
            project_id: None,
            direction: "incoming".into(),
            sender: "Synthetic Replacement Sender".into(),
            recipient: "Synthetic Replacement Recipient".into(),
            summary: "Synthetic replacement summary".into(),
        },
    )
    .await
    .unwrap();
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM letter WHERE id=$1")
        .bind(letter.id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    let mailrooms: i64 = sqlx::query_scalar("SELECT count(*) FROM mailroom WHERE id=$1")
        .bind(mailroom.id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(mailrooms, 0);
    let replacement_row: i64 = sqlx::query_scalar("SELECT count(*) FROM mailroom WHERE id=$1")
        .bind(replacement.id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(replacement_row, 1);
    let replacement_letter_row: i64 = sqlx::query_scalar("SELECT count(*) FROM letter WHERE id=$1")
        .bind(replacement_letter.id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(replacement_letter_row, 1);

    let historical_letter_id = Uuid::new_v4();
    surreal
        .query("CREATE $id SET mailroom_id=$mailroom, direction='incoming', sender='Synthetic Historical Sender', recipient='Synthetic Recipient', summary='Synthetic historical row'")
        .bind(("id", crate::surreal::record_id("letter", historical_letter_id)))
        .bind(("mailroom", crate::surreal::record_id("mailroom", replacement.id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    mirror(&surreal, &pg, &MirrorKey::Letter(historical_letter_id))
        .await
        .unwrap();
    let historical_project: Option<Uuid> =
        sqlx::query_scalar("SELECT project_id FROM letter WHERE id=$1")
            .bind(historical_letter_id)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(historical_project, None);

    let dangling_mailroom_id = Uuid::new_v4();
    let dangling_letter_id = Uuid::new_v4();
    surreal
        .query("CREATE $id SET mailroom_id=$mailroom, direction='incoming', sender='Synthetic Sender', recipient='Synthetic Recipient', summary='Synthetic dangling row'")
        .bind(("id", crate::surreal::record_id("letter", dangling_letter_id)))
        .bind(("mailroom", crate::surreal::record_id("mailroom", dangling_mailroom_id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert!(matches!(
        mirror(&surreal, &pg, &MirrorKey::Letter(dangling_letter_id)).await,
        Err(MirrorError::Target(ref code)) if code == "23503"
    ));
    assert!(crate::letters::find_by_id(&surreal, dangling_letter_id)
        .await
        .unwrap()
        .is_some());
    assert!(parity(&surreal, &pg)
        .await
        .unwrap()
        .tables
        .iter()
        .any(|table| table.table == "letter"
            && table
                .divergent
                .contains(&MirrorKey::Letter(dangling_letter_id))));
    pg.0.close().await;
}
