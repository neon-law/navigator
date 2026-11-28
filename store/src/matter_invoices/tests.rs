use super::*;

#[test]
fn a_payment_reference_names_its_project_by_id_or_code() {
    let id = Uuid::new_v4();
    assert_eq!(
        project_scope_from_reference(&format!("Matter {id}")),
        Some(ProjectScopeRef::Id(id))
    );
    assert_eq!(
        project_scope_from_reference(" Matter sample-estate "),
        Some(ProjectScopeRef::Code("sample-estate".into()))
    );
    for unscoped in ["", "Matter ", "Invoice sample-estate", "Matter not a code!"] {
        assert_eq!(project_scope_from_reference(unscoped), None, "{unscoped}");
    }
}

#[test]
fn only_an_open_invoice_has_a_balance_due() {
    let invoice = |status, paid| MatterInvoice {
        id: Uuid::nil(),
        project_id: Uuid::nil(),
        source: Source::Check,
        external_id: Some("1001".into()),
        reference: "Check 1001".into(),
        status,
        amount_cents: 10_000,
        amount_paid_cents: paid,
        currency: "USD".into(),
        issued_on: None,
        due_on: None,
        payer: None,
        memo: None,
        status_reason: None,
        client_visible: false,
        stripe_account_id: None,
        destination: None,
        billing_batch_id: None,
        inserted_at: Utc::now(),
        updated_at: Utc::now(),
    };
    assert_eq!(invoice(Status::Sent, 2_500).balance_due_cents(), 7_500);
    assert_eq!(invoice(Status::Sent, 12_000).balance_due_cents(), 0);
    for closed in [Status::Paid, Status::Void, Status::Uncollectible, Status::Pending] {
        assert_eq!(invoice(closed, 0).balance_due_cents(), 0, "{closed:?}");
    }
    assert!(Status::Sent.is_open() && !Status::Draft.is_open());
    assert_eq!(Source::InPerson.as_str(), "in_person");
}
