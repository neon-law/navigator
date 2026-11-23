//! Independent destination reader reconstructs a synthetic row into a separate PostgreSQL target.
use super::*;
use futures::TryStreamExt as _;

pub(super) async fn person_from_destination(
    writer: &crate::write_only::tests::CreateOnly,
    receipt: &LocalReceipt,
    options: sqlx::postgres::PgConnectOptions,
    person: uuid::Uuid,
) {
    let reader = crate::test_support::CollisionStorage::locked();
    let objects = writer.objects.lock().unwrap().clone();
    for (key, bytes) in objects {
        reader
            .put_immutable(&key, &bytes, "application/octet-stream")
            .await
            .unwrap();
    }
    let descriptor = reader.get(&receipt.inventory_key).await.unwrap();
    assert_eq!(
        crate::postgres::sha256_hex(&descriptor.bytes),
        receipt.inventory_sha256
    );
    let inventory: Inventory = serde_json::from_slice(&descriptor.bytes).unwrap();
    let mut files = Vec::new();
    for entry in &inventory.objects {
        let object = reader.get(&entry.key).await.unwrap();
        assert_eq!(object.bytes.len(), entry.bytes);
        assert_eq!(crate::postgres::sha256_hex(&object.bytes), entry.sha256);
        files.push((
            entry.key.clone(),
            object.bytes,
            crate::write_only::object_content_type(&entry.key).unwrap(),
        ));
    }
    crate::write_only::closure::validate(
        &inventory,
        &receipt.root_uri,
        &receipt.inventory_key,
        &files,
    )
    .unwrap();
    let io = iceberg::io::FileIO::new_with_memory();
    for (key, bytes, _) in files {
        io.new_output(format!("{}/{key}", receipt.root_uri))
            .unwrap()
            .write(bytes.into())
            .await
            .unwrap();
    }
    let metadata = inventory
        .metadata
        .iter()
        .find(|uri| uri.contains("/person/metadata/"))
        .unwrap();
    let table = iceberg::table::StaticTable::from_metadata_file(
        metadata,
        iceberg::TableIdent::from_strs(["restored", "person"]).unwrap(),
        io,
    )
    .await
    .unwrap();
    let tasks = table.scan().build().unwrap().plan_files().await.unwrap();
    let batches: Vec<_> = table
        .reader_builder()
        .build()
        .read(tasks)
        .unwrap()
        .stream()
        .try_collect()
        .await
        .unwrap();
    restore_person(batches, options, person).await;
}

async fn restore_person(
    batches: Vec<arrow_array58::RecordBatch>,
    options: sqlx::postgres::PgConnectOptions,
    person: uuid::Uuid,
) {
    let target = store::test_support::PostgresFixture::create(options)
        .await
        .unwrap();
    let mut restored = 0;
    for batch in batches {
        use arrow_array58::{
            Array as _, FixedSizeBinaryArray, StringArray, TimestampMicrosecondArray,
        };
        let ids = batch
            .column_by_name("id")
            .unwrap()
            .as_any()
            .downcast_ref::<FixedSizeBinaryArray>()
            .unwrap();
        let names = batch
            .column_by_name("name")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let emails = batch
            .column_by_name("email")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let roles = batch
            .column_by_name("role")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let inserted = batch
            .column_by_name("inserted_at")
            .unwrap()
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .unwrap();
        let updated = batch
            .column_by_name("updated_at")
            .unwrap()
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .unwrap();
        for row in 0..batch.num_rows() {
            let id = uuid::Uuid::from_slice(ids.value(row)).unwrap();
            assert!(!ids.is_null(row));
            sqlx::query("INSERT INTO person(id,name,email,role,inserted_at,updated_at) VALUES($1,$2,$3,$4,$5,$6)")
                .bind(id).bind(names.value(row)).bind(emails.value(row)).bind(roles.value(row))
                .bind(chrono::DateTime::from_timestamp_micros(inserted.value(row)).unwrap())
                .bind(chrono::DateTime::from_timestamp_micros(updated.value(row)).unwrap())
                .execute(target.postgres().pool()).await.unwrap();
            restored += 1;
        }
    }
    assert_eq!(restored, 1);
    let row: (String, String, String) =
        sqlx::query_as("SELECT name,email,role FROM person WHERE id=$1")
            .bind(person)
            .fetch_one(target.postgres().pool())
            .await
            .unwrap();
    assert_eq!(
        row,
        (
            "Synthetic archived person".into(),
            "archive-person@example.invalid".into(),
            "client".into()
        )
    );
    target.close().await.unwrap();
}
