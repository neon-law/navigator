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
fn explicit_mapping_preserves_both_directions_without_source_content_or_fake_twilio_identity() {
    let bytes=csv("C,I,+15555550456,2026-10-30T18:00:00-04:00,361,call-one,private-content\nS,O,+15555550789,2026-10-30T22:01:00Z,,text-one,private-content");
    let rows = parse(&mapping(), "+15555550123", &bytes).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].metadata.duration_seconds, Some(361));
    assert_eq!(rows[0].metadata.occurred_at, "2026-10-30T22:00:00.000000Z");
    assert_eq!(rows[1].metadata.direction, Direction::Outbound);
    assert_eq!(rows[1].metadata.duration_seconds, None);
    assert!(rows
        .iter()
        .all(|row| row.provider_message_id.starts_with("google_fi:v1:id:")));
    assert!(rows.iter().all(|row| !row.body.contains("private-content")));
    assert_eq!(metadata(&rows[0].body), Some(rows[0].metadata.clone()));
    let account = account("+15555550123").unwrap();
    let ingest = rows[1].ingest(Uuid::now_v7(), "example", &account);
    assert!(ingest.attachments.is_empty());
    assert!(ingest.raw_storage_key.is_none());
    assert!(ingest.provider_account.starts_with("google_fi:"));
}

#[test]
fn units_are_exact_and_missing_duration_is_not_zero() {
    assert_eq!(seconds("0.1", DurationUnit::Minutes), Ok(Some(6)));
    assert!(seconds("0.01", DurationUnit::Minutes).is_err());
    assert!(seconds("-1", DurationUnit::Seconds).is_err());
    assert!(seconds("9223372036854775808", DurationUnit::Seconds).is_err());
    assert_eq!(seconds("", DurationUnit::Seconds), Ok(None));
    assert_eq!(seconds("0", DurationUnit::Seconds), Ok(Some(0)));
    assert!(parse(
        &mapping(),
        "+15555550123",
        &csv("S,O,+15555550456,2026-10-30T18:00:00Z,1,sms-id,ignored")
    )
    .is_err());
}

#[test]
fn timezone_is_explicit_and_dst_gaps_and_folds_are_refused() {
    let mut mapped = mapping();
    mapped.timezone = Zone::Iana("America/Los_Angeles".into());
    assert!(matches!(
        parse(
            &mapped,
            "+15555550123",
            &csv("C,I,+15555550456,2026-10-30T18:00:00Z,1,one,ignored")
        ),
        Err(FiImportError::Mapping(_))
    ));
    mapped.timestamp_format = "%Y-%m-%d %H:%M:%S".into();
    for at in ["2026-03-08 02:30:00", "2026-11-01 01:30:00"] {
        assert!(parse(
            &mapped,
            "+15555550123",
            &csv(&format!("C,I,+15555550456,{at},1,one,ignored"))
        )
        .is_err());
    }
    let accepted = parse(
        &mapped,
        "+15555550123",
        &csv("C,I,+15555550456,2026-10-30 18:00:00,1,one,ignored"),
    )
    .unwrap();
    assert_eq!(
        accepted[0].metadata.occurred_at,
        "2026-10-31T01:00:00.000000Z"
    );
    mapped.timezone = Zone::FixedOffset("+02:00".into());
    let accepted = parse(
        &mapped,
        "+15555550123",
        &csv("C,I,+15555550456,2026-10-30 18:00:00,1,one,ignored"),
    )
    .unwrap();
    assert_eq!(
        accepted[0].metadata.occurred_at,
        "2026-10-30T16:00:00.000000Z"
    );
}

#[test]
fn identity_survives_order_changes_and_normalization_and_ambiguous_twins_are_reported() {
    let first = "C,I,+15555550456,2026-10-30T18:00:00Z,6,a,ignored";
    let second = "C,O,+15555550789,2026-10-30T18:01:00Z,7,b,ignored";
    let original = parse(
        &mapping(),
        "+15555550123",
        &csv(&format!("{first}\n{second}")),
    )
    .unwrap();
    let reversed = parse(
        &mapping(),
        "+15555550123",
        &csv(&format!("{second}\n{first}")),
    )
    .unwrap();
    assert_eq!(
        original[0].provider_message_id,
        reversed[1].provider_message_id
    );
    let mut mapped = mapping();
    mapped.columns.identity = None;
    let a = parse(&mapped, "+15555550123", &csv(first)).unwrap();
    mapped.duration_unit = DurationUnit::Minutes;
    let b = parse(
        &mapped,
        "+15555550123",
        &csv("C,I,+15555550456,2026-10-30T14:00:00-04:00,0.1,other,ignored"),
    )
    .unwrap();
    assert_eq!(a[0].provider_message_id, b[0].provider_message_id);
    mapped.duration_unit = DurationUnit::Seconds;
    assert_eq!(
        parse(&mapped, "+15555550123", &csv(&format!("{first}\n{first}"))).unwrap_err(),
        FiImportError::Duplicate {
            first: 2,
            second: 3
        }
    );
    // Different real native IDs can legitimately have identical facts.
    let twins = parse(
        &mapping(),
        "+15555550123",
        &csv(&format!("{first}\n{}", first.replace(",a,", ",b,"))),
    )
    .unwrap();
    assert_ne!(twins[0].provider_message_id, twins[1].provider_message_id);
}

#[test]
fn duplicate_headers_unknown_values_and_limits_fail_before_any_ingestion() {
    let duplicated =
        b"type,TYPE,way,phone,at,length,id\nC,C,I,+15555550456,2026-10-30T18:00:00Z,1,one\n";
    assert_eq!(
        parse(&mapping(), "+15555550123", duplicated).unwrap_err(),
        FiImportError::Headers
    );
    assert!(parse(
        &mapping(),
        "+15555550123",
        &csv("UNKNOWN,I,+15555550456,2026-10-30T18:00:00Z,1,one,ignored")
    )
    .is_err());
    assert_eq!(
        parse(&mapping(), "+15555550123", &vec![0; MAX_CSV_BYTES + 1]).unwrap_err(),
        FiImportError::Limit
    );
    let mut bytes = b"type,way,phone,at,length,id,content\n".to_vec();
    for index in 0..=MAX_ROWS {
        bytes.extend_from_slice(
            format!("C,I,+15555550456,2026-10-30T18:00:00Z,1,{index},ignored\n").as_bytes(),
        );
    }
    assert_eq!(
        parse(&mapping(), "+15555550123", &bytes).unwrap_err(),
        FiImportError::Limit
    );
}

#[test]
fn split_timestamp_columns_and_absent_duration_are_explicit() {
    let mut mapped = mapping();
    mapped.columns.timestamp = vec!["day".into(), "clock".into()];
    mapped.timestamp_format = "%Y-%m-%d %H:%M:%S".into();
    mapped.timezone = Zone::Iana("UTC".into());
    mapped.columns.duration = None;
    let rows = parse(
        &mapped,
        "+15555550123",
        b"type,way,phone,day,clock,id\nC,I,+15555550456,2026-10-30,18:00:00,one\n",
    )
    .unwrap();
    assert_eq!(rows[0].metadata.duration_seconds, None);
    assert_eq!(rows[0].metadata.occurred_at, "2026-10-30T18:00:00.000000Z");
}

#[test]
fn timestamp_precision_matches_postgresql_without_silent_truncation() {
    let accepted = parse(
        &mapping(),
        "+15555550123",
        &csv("C,I,+15555550456,2026-10-30T18:00:00.123456-04:00,1,one,ignored"),
    )
    .unwrap();
    assert_eq!(
        accepted[0].metadata.occurred_at,
        "2026-10-30T22:00:00.123456Z"
    );
    assert!(parse(
        &mapping(),
        "+15555550123",
        &csv("C,I,+15555550456,2026-10-30T18:00:00.123456789-04:00,1,one,ignored")
    )
    .is_err());
}
