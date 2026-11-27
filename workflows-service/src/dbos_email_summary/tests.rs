use super::{enabled, workflow_id, EmailSummaryError, ENABLED_ENV, WORKFLOW};

#[test]
fn every_failure_names_its_step_without_message_content() {
    assert_eq!(
        EmailSummaryError::Receipt.to_string(),
        "the email receipt could not be loaded"
    );
    assert_eq!(
        EmailSummaryError::Config.to_string(),
        "summary run configuration does not match receipt"
    );
    assert_eq!(
        EmailSummaryError::SenderGate("store".into()).to_string(),
        "the sender gate step failed: store"
    );
    assert_eq!(
        EmailSummaryError::Provider("vertex".into()).to_string(),
        "the summary provider step failed: vertex"
    );
    assert_eq!(
        EmailSummaryError::Delivery("slack".into()).to_string(),
        "the summary delivery failed: slack"
    );
    let journaled = serde_json::to_string(&EmailSummaryError::Provider("x".into())).unwrap();
    let replayed: EmailSummaryError = serde_json::from_str(&journaled).unwrap();
    assert!(matches!(replayed, EmailSummaryError::Provider(reason) if reason == "x"));
}

#[test]
fn registration_is_opt_in_and_rejects_invalid_values() {
    assert!(!enabled(|_| None).unwrap());
    assert!(!enabled(|key| (key == ENABLED_ENV).then(|| "false".to_owned())).unwrap());
    assert!(enabled(|key| (key == ENABLED_ENV).then(|| "true".to_owned())).unwrap());
    assert!(enabled(|key| (key == ENABLED_ENV).then(|| "yes".to_owned())).is_err());
}

#[test]
fn every_delivery_of_one_receipt_joins_one_workflow_id() {
    let receipt_id = uuid::Uuid::now_v7();
    assert_eq!(
        workflow_id(receipt_id),
        format!("EmailSummary:{receipt_id}")
    );
    assert_eq!(workflow_id(receipt_id), workflow_id(receipt_id));
    assert_ne!(workflow_id(uuid::Uuid::now_v7()), workflow_id(receipt_id));
    assert_eq!(WORKFLOW, "EmailSummary");
}
