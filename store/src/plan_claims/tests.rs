use super::*;

fn date(value: &str) -> NaiveDate {
    value.parse().unwrap()
}

fn claim(service: Service, authorization: Option<Authorization>) -> Work {
    Work {
        service,
        benefit: "SYNTHETIC".into(),
        performed_on: date("2026-11-01"),
        attorney_minutes: Some(90),
        time_entry_ids: Vec::new(),
        authorization,
    }
}

#[test]
fn legal_ease_cap_excludes_pending_minutes_and_arag_requires_its_own_service_path() {
    let work = claim(
        Service::LegalEasePaymentAuthorization,
        Some(Authorization::CappedHours {
            reference: "sample-authorization".into(),
            rate_cents_per_hour: 12_000,
            covered_minutes: 60,
            pending_minutes: 30,
        }),
    );
    assert_eq!(work.estimate().unwrap(), Some(12_000));
    assert!(work.validate("ARAG", date("2026-11-21"), false).is_err());
    assert!(work
        .validate("LegalEASE", date("2026-11-21"), false)
        .is_ok());
    assert_eq!(
        claim(Service::AragAdviceOnly, None).estimate().unwrap(),
        None
    );
    let fixed = claim(
        Service::LegalEaseLegalGuard,
        Some(Authorization::FixedFee {
            reference: "sample-written-fixed-fee".into(),
            cents: 9_500,
        }),
    );
    assert_eq!(fixed.estimate().unwrap(), Some(9_500));
    assert!(fixed
        .validate("Legal Ease", date("2026-11-21"), false)
        .is_ok());
    assert!(fixed
        .validate("Legal Ease ARAG", date("2026-11-21"), false)
        .is_err());
}

#[test]
fn approved_outstanding_uses_net_cash_and_denial_preserves_the_partial_payment() {
    let mut claim = ClaimState::new(claim(
        Service::AragAdviceOnly,
        Some(Authorization::FixedFee {
            reference: "sample-benefit".into(),
            cents: 12_000,
        }),
    ));
    let now = date("2026-11-21");
    claim.apply(now, &Event::Prepared).unwrap();
    claim
        .apply(
            now,
            &Event::Submission {
                provider_reference: Some("sample-claim".into()),
                sent_message_id: None,
                claimed_cents: None,
            },
        )
        .unwrap();
    assert_eq!(claim.claimed_cents, None);
    assert_eq!(claim.outstanding(), None);
    assert_eq!(claim.net_cents, 0);
    claim
        .apply(
            now,
            &Event::Approval {
                reference: "sample-approval".into(),
                cents: 10_000,
            },
        )
        .unwrap();
    claim
        .apply(
            now,
            &Event::Payment {
                reference: "sample-payment".into(),
                cents: 5_000,
            },
        )
        .unwrap();
    claim
        .apply(
            now,
            &Event::Reversal {
                reference: "sample-refund".into(),
                cents: 1_000,
            },
        )
        .unwrap();
    assert_eq!(claim.outstanding(), Some(6_000));
    assert_eq!(claim.net_cents, 4_000);
    claim
        .apply(
            now,
            &Event::Denial {
                reference: "sample-denial".into(),
            },
        )
        .unwrap();
    assert_eq!(claim.outstanding(), None);
    assert_eq!(claim.net_cents, 4_000);
    claim
        .apply(
            now,
            &Event::Correction {
                provider_reference: "sample-correction".into(),
                claimed_cents: Some(6_000),
            },
        )
        .unwrap();
    assert_eq!(claim.net_cents, 4_000);
    assert_eq!(claim.outstanding(), None);
}

#[test]
fn the_rate_includes_unpaid_work_and_reports_cash_with_unknown_time_separately() {
    let now = date("2026-11-21");
    let row = |minutes, net_cents| {
        let mut work = claim(Service::AragRepresentation, None);
        work.attorney_minutes = minutes;
        let mut state = ClaimState::new(work);
        state.net_cents = net_cents;
        ReviewRow {
            id: Uuid::new_v4(),
            code: "sample-matter".into(),
            plan: "ARAG".into(),
            state: Json(state),
            recorded_this_week: true,
            net_collected_this_week_cents: net_cents,
            closed_on: None,
            filing_rule: None,
        }
    };
    let worklist = summarize(
        now,
        vec![row(Some(60), 12_000), row(Some(60), 0), row(None, 500)],
        true,
    )
    .unwrap();
    let plan = &worklist.plans["ARAG"];
    assert_eq!(plan.attorney_minutes, 120);
    assert_eq!(plan.net_collected_cents, 12_500);
    assert_eq!(plan.net_collected_without_attorney_time_cents, 500);
    assert_eq!(plan.net_collected_per_attorney_hour_cents, Some(6_000));
    assert_eq!(plan.net_collected_this_week_cents, 12_500);
    assert_eq!(plan.claims_missing_attorney_time, 1);
}

#[test]
fn a_receipt_establishes_submission_without_pricing_missing_coverage() {
    let on = date("2026-11-21");
    let mut state = ClaimState::new(claim(Service::AragAdviceOnly, None));
    assert!(state.apply(on, &Event::Prepared).is_err());
    state
        .apply(
            on,
            &Event::Submission {
                provider_reference: Some("sample-claim".into()),
                sent_message_id: None,
                claimed_cents: None,
            },
        )
        .unwrap();
    assert_eq!(state.stage, Stage::Submitted);
    assert_eq!(state.claimed_cents, None);
    assert_eq!(state.net_cents, 0);
    state
        .apply(
            on,
            &Event::Payment {
                reference: "example-payment".into(),
                cents: 2_000,
            },
        )
        .unwrap();
    state
        .apply(
            on,
            &Event::Reversal {
                reference: "example-reversal".into(),
                cents: 2_000,
            },
        )
        .unwrap();
    assert_eq!(state.net_cents, 0);
}

#[test]
fn filing_review_groups_verified_unfiled_work_by_carrier() {
    let today = date("2026-11-21");
    let fixed = Some(Authorization::FixedFee {
        reference: "sample-benefit".into(),
        cents: 8_050,
    });
    let arag = claim(Service::AragAdviceOnly, fixed);
    let legal_ease = claim(
        Service::LegalEasePaymentAuthorization,
        Some(Authorization::CappedHours {
            reference: "sample-approval".into(),
            rate_cents_per_hour: 12_000,
            covered_minutes: 60,
            pending_minutes: 30,
        }),
    );
    let row = |plan: &str, work, stage| {
        let mut state = ClaimState::new(work);
        state.stage = stage;
        ReviewRow {
            id: Uuid::new_v4(),
            code: "sample-matter".into(),
            plan: plan.into(),
            state: Json(state),
            recorded_this_week: true,
            net_collected_this_week_cents: 0,
            closed_on: None,
            filing_rule: None,
        }
    };
    let report = summarize(
        today,
        vec![
            row("ARAG Group Legal", arag.clone(), Stage::Completed),
            row("ARAG", arag.clone(), Stage::Prepared),
            row("ARAG", arag, Stage::Submitted),
            row("Legal Ease", legal_ease, Stage::Prepared),
            row(
                "Legal Ease",
                claim(Service::LegalEaseLegalGuard, None),
                Stage::Completed,
            ),
        ],
        false,
    )
    .unwrap();
    let json = serde_json::to_value(report).unwrap();
    let totals = |count, cents| {
        serde_json::json!({
            "eligible_unsubmitted_count": count, "eligible_unsubmitted_cents": cents
        })
    };
    assert_eq!(json["filing_review"]["ARAG"], totals(2, 16_100));
    assert_eq!(json["filing_review"]["LegalEASE"], totals(1, 12_000));
    assert_eq!(json["plans"]["Legal Ease"]["needs_verification_count"], 1);
    assert_eq!(
        json["plans"]["ARAG Group Legal"]["eligible_unsubmitted_cents"],
        8_050
    );
    let empty = serde_json::to_value(summarize(today, vec![], false).unwrap()).unwrap();
    assert_eq!(empty["filing_review"]["ARAG"], totals(0, 0));
    assert_eq!(empty["filing_review"]["LegalEASE"], totals(0, 0));
}

#[test]
fn configured_claim_uses_the_benefit_trigger_and_filing_window() {
    let mut work = claim(
        Service::LegalEasePaymentAuthorization,
        Some(Authorization::CappedHours {
            reference: "synthetic-authorization".into(),
            rate_cents_per_hour: 100,
            covered_minutes: 120,
            pending_minutes: 15,
        }),
    );
    let mut rule = FilingRule {
        benefit_code: work.benefit.clone(),
        effective_on: date("2026-10-01"),
        trigger: FilingTrigger::HoursExhaustedOrMatterClosed,
        filing_window_days: Some(30),
        filing_destination: "https://plan.example/claims".into(),
        filer: "Handling lawyer".into(),
    };
    assert_eq!(
        work.filing(Some(&rule), None, date("2026-11-02"))
            .unwrap()
            .status,
        FilingStatus::Accruing
    );
    let closed = Some(date("2026-11-03"));
    let filing = work
        .filing(Some(&rule), closed, date("2026-11-03"))
        .unwrap();
    assert_eq!(filing.status, FilingStatus::Ready);
    assert_eq!(filing.due_on, Some(date("2026-12-03")));
    assert_eq!(
        work.filing(Some(&rule), closed, date("2026-12-04"))
            .unwrap()
            .status,
        FilingStatus::Expired
    );
    work.attorney_minutes = Some(120);
    assert_eq!(
        work.filing(Some(&rule), closed, date("2026-12-01"))
            .unwrap()
            .due_on,
        Some(date("2026-12-01"))
    );
    rule.trigger = FilingTrigger::ServiceCompleted;
    work.authorization = None;
    assert_eq!(
        work.filing(Some(&rule), None, date("2026-11-02"))
            .unwrap()
            .status,
        FilingStatus::VerifyTerms
    );
}

#[test]
fn manual_service_requires_a_configured_benefit_without_overriding_named_carrier_paths() {
    let mut work = claim(Service::Manual, None);
    let today = date("2026-11-02");
    assert!(work.validate("Example Plan", today, false).is_err());
    assert!(work.validate("Example Plan", today, true).is_ok());
    assert!(work.validate("ARAG", today, true).is_err());
    work.service = Service::AragRepresentation;
    assert!(work.validate("Example Plan", today, true).is_err());
}

#[test]
fn an_email_filed_claim_is_submitted_by_its_sent_message_and_takes_a_reference_later() {
    let mut state = ClaimState::new(claim(
        Service::LegalEaseLegalGuard,
        Some(Authorization::FixedFee {
            reference: "sample-benefit".into(),
            cents: 12_000,
        }),
    ));
    let on = date("2026-11-21");
    state.apply(on, &Event::Prepared).unwrap();
    let packet = Event::PacketFiled {
        asset_id: Uuid::nil(),
        sha256: "0".repeat(64),
    };
    state.apply(on, &packet).unwrap();
    assert_eq!(state.packet_asset_id, Some(Uuid::nil()));
    assert!(matches!(
        Event::Submission {
            provider_reference: None,
            sent_message_id: None,
            claimed_cents: None,
        }
        .reference(),
        Err(ClaimError::Invalid(_))
    ));
    let emailed = Event::Submission {
        provider_reference: None,
        sent_message_id: Some("18c0ffee".into()),
        claimed_cents: Some(12_000),
    };
    assert_eq!(
        emailed.reference().unwrap(),
        Some(("claim", "message:18c0ffee".to_owned()))
    );
    state.apply(on, &emailed).unwrap();
    let billing = state.billing(Uuid::nil(), "synthetic").unwrap();
    assert_eq!(billing.status, "submitted");
    assert_eq!(
        billing.next_action.as_deref(),
        Some("Check provider decision or payment")
    );
    assert_eq!(state.provider_reference, None);
    assert_eq!(state.sent_message_id.as_deref(), Some("18c0ffee"));
    let reference = Event::ProviderReference {
        provider_reference: "sample-claim".into(),
    };
    state.apply(on, &reference).unwrap();
    assert_eq!(state.provider_reference.as_deref(), Some("sample-claim"));
    assert!(
        state.apply(on, &reference).is_err(),
        "one reference per claim"
    );
    state
        .apply(
            on,
            &Event::Denial {
                reference: "sample-denial".into(),
            },
        )
        .unwrap();
    assert!(
        state.apply(on, &packet).is_err(),
        "no packet after a denial"
    );
    // Older rows without the new fields still read.
    let stored: ClaimState = serde_json::from_value(serde_json::json!({
        "work": serde_json::to_value(&state.work).unwrap(), "stage": "submitted",
        "provider_reference": "older", "submitted_on": "2026-11-21", "claimed_cents": null,
        "approved_cents": null, "net_cents": 0,
    }))
    .unwrap();
    assert_eq!(stored.sent_message_id, None);
    let older: Event = serde_json::from_value(serde_json::json!({
        "type": "submission", "provider_reference": "older", "claimed_cents": null,
    }))
    .unwrap();
    assert!(matches!(
        older,
        Event::Submission {
            provider_reference: Some(_),
            ..
        }
    ));
}
