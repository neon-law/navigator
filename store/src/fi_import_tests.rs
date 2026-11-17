use super::*;

fn mapping() -> Mapping {
    serde_json::from_value(serde_json::json!({
        "columns":{"kind":"type","direction":"way","counterparty":"phone","timestamp":["at"],"duration":"length","identity":"id","status":null},
        "kinds":{"C":"call","S":"sms"},"directions":{"I":"inbound","O":"outbound"},
        "timestamp_format":"%+","timestamp_separator":" ","timezone":{"mode":"embedded_offset"},"duration_unit":"seconds"
    })).unwrap()
}

fn csv(row: &str) -> Vec<u8> {
    format!("type,way,phone,at,length,id,content\n{row}\n").into_bytes()
}

#[test]
fn fi_adapter_keeps_content_and_attachments_out_of_metadata_ingestion() {
    let bytes=csv("C,I,+15555550456,2026-10-30T18:00:00-04:00,361,call-one,private-content\nS,O,+15555550789,2026-10-30T22:01:00Z,,text-one,private-content");
    let rows = parse(&mapping(), "+15555550123", &bytes).unwrap();
    let account = account("+15555550123").unwrap();
    let ingest = rows[1].ingest(Uuid::now_v7(), "example", &account);
    assert!(ingest.attachments.is_empty());
    assert!(ingest.raw_storage_key.is_none());
    assert!(ingest.provider_account.starts_with("google_fi:"));
}
