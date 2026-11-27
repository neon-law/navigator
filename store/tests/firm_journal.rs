use store::firm_journal::*;
use store::postgres::PostgresDb;
use uuid::Uuid;
#[path = "support/firm_journal.rs"]
mod journal_fixture;
use journal_fixture::entry;

async fn firm(db: &store::application_store::ApplicationStore) -> Uuid {
    let admin = store::persons::create(
        db,
        &store::persons::NewPerson::with_role(
            "Synthetic administrator",
            format!("admin-{}@example.com", Uuid::now_v7()),
            store::persons::Role::Admin,
        ),
    )
    .await
    .unwrap();
    store::firms::create(
        db,
        &store::firms::NewFirm {
            name: format!("Synthetic accounting firm {}", Uuid::now_v7()),
            status: "active".into(),
            entity_id: entity(db).await,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .unwrap()
    .id
}

async fn project_brand(db: &store::application_store::ApplicationStore, firm: Uuid) -> String {
    let admin = store::firms::memberships_for_firm(db, firm)
        .await
        .unwrap()
        .into_iter()
        .find(|membership| {
            membership.is_dri && membership.membership == store::firms::FirmMembership::Admin
        })
        .expect("the native Firm fixture has its appointed Admin DRI");
    let key = format!("synthetic-{}", Uuid::now_v7().simple());
    store::brands::create(
        db,
        store::persons::Role::Admin,
        Some(admin.person_id),
        &store::brands::NewBrand {
            name: format!("Synthetic Brand {key}"),
            key: key.clone(),
            firm_id: Some(firm),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    store::firms::attach_brand(db, store::persons::Role::Owner, None, firm, &key)
        .await
        .unwrap();
    key
}

async fn entity(db: &store::application_store::ApplicationStore) -> Uuid {
    let suffix = Uuid::now_v7();
    let jurisdiction = store::jurisdictions::create(
        db,
        &store::jurisdictions::NewJurisdiction::new(
            format!("Synthetic jurisdiction {suffix}"),
            suffix.to_string(),
            "state",
        ),
    )
    .await
    .unwrap();
    let kind = store::entity_types::create(db, &format!("Synthetic kind {suffix}"))
        .await
        .unwrap();
    store::entities::create(
        db,
        &store::entities::NewEntity {
            name: format!("Synthetic entity {suffix}"),
            entity_type_id: kind.id,
            jurisdiction_id: jurisdiction.id,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        },
    )
    .await
    .unwrap()
    .id
}

#[tokio::test]
async fn filed_receipts_post_one_confirmed_unbilled_cost_per_transaction() {
    use store::{documents, project_costs as costs};

    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let db = store::application_store::ApplicationStore::uninitialized();
        let firm = firm(&db).await;
        let actor = store::firms::memberships_for_firm(&db, firm).await.unwrap()[0].person_id;
        let project = store::projects::create(
            &db,
            &store::projects::NewProject {
                code: format!("expense-{}", Uuid::now_v7().simple()),
                name: "Synthetic expense Project".into(),
                status: "open".into(),
                entity_id: entity(&db).await,
                brand: project_brand(&db, firm).await,
                firm_id: Some(firm),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id;
        let tmp = tempfile::tempdir().unwrap();
        let storage: std::sync::Arc<dyn cloud::StorageService> = std::sync::Arc::new(
            cloud::FsStorage::new(tmp.path().to_path_buf())
                .await
                .unwrap(),
        );
        let source = costs::mercury_source_reference(Uuid::now_v7());
        let identity = documents::DocumentIdentity {
            metadata: Some(serde_json::json!({"receipt": {
                "amount_cents": 845,
                "currency": "USD",
                "payee": "Synthetic postal vendor",
                "category": "certified_mail_and_postage",
                "transaction_id": source,
            }})),
            ..Default::default()
        };
        let mut receipts = Vec::new();
        for filename in ["receipt.txt", "receipt-copy.txt"] {
            let filed = documents::ingest_bytes_as(
                &db,
                &storage,
                &documents::IngestArgs {
                    project_id: project,
                    source: "upload",
                    filename,
                    kind: "receipt",
                    content_type: "text/plain",
                    description: None,
                    secondary_storage_key: None,
                    visibility: documents::visibility::INTERNAL,
                },
                &identity,
                filename.as_bytes(),
            )
            .await
            .unwrap();
            receipts.push(filed.asset_id);
        }
        assert!(costs::list_for_project(&pg, project)
            .await
            .unwrap()
            .is_empty());

        let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        let post = |receipt| costs::create_from_receipt(&db, &pg, project, actor, receipt, date);
        let (first, second) = tokio::join!(post(receipts[0]), post(receipts[1]));
        let (first, created_first) = first.unwrap();
        let (second, created_second) = second.unwrap();
        assert_ne!(created_first, created_second);
        assert_eq!(first, second);
        assert_eq!(first.occurred_on, date);
        assert_eq!(first.payee, "Synthetic postal vendor");
        assert_eq!(first.description, "Certified mail and postage");
        assert_eq!(first.amount_cents, 845);
        assert_eq!(first.currency, "USD");
        assert_eq!(first.amount_status, "confirmed");
        assert_eq!(first.billing_status, "unbilled");
        assert_eq!(first.source_reference.as_deref(), Some(source.as_str()));
        assert_eq!(post(receipts[0]).await.unwrap(), (first.clone(), false));
        assert_eq!(costs::list_events(&pg, first.id).await.unwrap().len(), 1);
        assert_eq!(
            costs::list_for_project(&pg, project).await.unwrap(),
            [first]
        );
    })
    .await;
}

#[test]
fn exact_units_never_balance_unlike_assets_or_accept_overprecision() {
    assert_eq!(parse_decimal("0.000000000000000001", 18).unwrap(), "1");
    assert!(parse_decimal("1.001", 2).is_err());
    let mut e = entry(Uuid::now_v7(), "balance");
    e.postings[1].unit = Unit::fiat("EUR", 2);
    assert!(e.validate().is_err());
    e.postings[1].unit = Unit::fiat("USD", 2);
    e.postings[0].quantity = "100000000000000000000000000000000000000000".into();
    assert!(e.validate().is_err());
}
#[tokio::test]
async fn concurrent_duplicate_commands_survive_retry_and_refuse_changed_payload() {
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let db = store::application_store::ApplicationStore::uninitialized();
        let firm = firm(&db).await;
        let e = entry(firm, "manual:one");
        let (a, b) = tokio::join!(post(&db, &pg, &e), post(&db, &pg, &e));
        assert_eq!(a.unwrap(), b.unwrap());
        assert_eq!(entries(&pg, firm).await.unwrap().len(), 1);
        assert_eq!(post(&db, &pg, &e).await.unwrap(), e.id);
        let mut changed = e;
        changed.memo = "Different economic event".into();
        assert!(post(&db, &pg, &changed).await.is_err());
    })
    .await;
}

#[test]
fn allocations_are_partial_but_bounded_and_corrections_link_original() {
    let mut e = entry(Uuid::now_v7(), "split");
    let p1 = Uuid::now_v7();
    let p2 = Uuid::now_v7();
    e.postings[1].project_id = Some(p1);
    e.postings[1].quantity = "-4000".into();
    let mut second = e.postings[1].clone();
    second.project_id = Some(p2);
    second.quantity = "-6000".into();
    e.postings.push(second);
    assert!(e.validate().is_ok());
    e.postings[2].quantity = "-6001".into();
    assert!(e.validate().is_err());
    e.postings[2].quantity = "-6000".into();
    let reverse = e
        .reversal("reverse-one", Uuid::now_v7(), "2026-10-12T00:00:00Z")
        .unwrap();
    assert_eq!(reverse.id.get_version_num(), 4);
    assert_ne!(reverse.id, e.id);
    assert_eq!(reverse.reversal_of, Some(e.id));
    assert_eq!(reverse.postings[0].quantity, "-10000");
    assert!(reverse.validate().is_ok());
}
#[test]
fn pending_outages_incomplete_coverage_and_unvalued_assets_never_reconcile_as_zero() {
    let e = entry(Uuid::now_v7(), "check");
    let mut s = SourceSnapshot::synthetic("USD", 2, "10000");
    s.status = SourceStatus::Pending;
    assert!(!compare(&e, &s, true).reconciled());
    s.status = SourceStatus::Completed;
    assert!(!compare(&e, &s, false).reconciled());
    assert!(compare(&e, &s, true).reconciled());
    s.quantity = "9999".into();
    assert!(!compare(&e, &s, true).reconciled());
    assert_eq!(Valuation::Unknown.reporting_quantity(), None);
}

#[tokio::test]
async fn a_provider_movement_cannot_be_imported_again_under_another_command_key() {
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let db = store::application_store::ApplicationStore::uninitialized();
        let firm = firm(&db).await;
        let mut first = entry(firm, "source-one");
        first.sources = vec![SourceSnapshot::synthetic("USD", 2, "10000").source];
        post(&db, &pg, &first).await.unwrap();
        let mut duplicate = first.clone();
        duplicate.id = Uuid::now_v7();
        duplicate.key = "source-two".into();
        assert!(post(&db, &pg, &duplicate).await.is_err());
        let reversal = first
            .reversal("reverse-source", first.actor_id, "2026-10-12T00:00:00Z")
            .unwrap();
        post(&db, &pg, &reversal).await.unwrap();
        post(&db, &pg, &duplicate).await.unwrap();
        assert_eq!(entries(&pg, firm).await.unwrap().len(), 3);
    })
    .await;
}
#[tokio::test]
async fn firm_allocation_and_receipt_boundaries_are_checked_in_store_commands() {
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let db = store::application_store::ApplicationStore::uninitialized();
        let firm = firm(&db).await;
        let other = self::firm(&db).await;
        let project = store::projects::create(
            &db,
            &store::projects::NewProject {
                code: "foreign".into(),
                name: "Foreign synthetic project".into(),
                firm_id: Some(other),
                brand: project_brand(&db, other).await,
                entity_id: entity(&db).await,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut e = entry(firm, "foreign-allocation");
        e.postings[1].project_id = Some(project.id);
        assert!(post(&db, &pg, &e).await.is_err());
        e.postings[1].project_id = None;
        e.receipts = vec![Uuid::now_v7()];
        assert!(post(&db, &pg, &e).await.is_err());
    })
    .await;
}
#[test]
fn a_reversed_source_reconciles_only_after_the_linked_reversal_nets_its_bank_side_to_zero() {
    let e = entry(Uuid::now_v7(), "refund");
    let mut s = SourceSnapshot::synthetic("USD", 2, "10000");
    s.status = SourceStatus::Reversed;
    assert!(!compare(&e, &s, true).reconciled());
    let mut combined = e.clone();
    combined.postings.extend(
        e.reversal("reverse", e.actor_id, "2026-10-12T00:00:00Z")
            .unwrap()
            .postings,
    );
    assert!(compare(&combined, &s, true).reconciled());
}

#[test]
fn bank_and_xero_account_ids_can_evidence_one_navigator_account() {
    let firm = Uuid::now_v7();
    let mut event = entry(firm, "canonical-payment");
    event.postings[0].account = "firm-operating".into();
    let mut bank = SourceSnapshot::synthetic("USD", 2, "10000");
    bank.source.provider = "mercury".into();
    bank.source.account = "provider-account-a".into();
    bank.journal_account = Some("firm-operating".into());
    let mut xero = bank.clone();
    xero.source.provider = "xero".into();
    xero.source.account = "provider-account-b".into();
    event.sources = vec![bank.source.clone(), xero.source.clone()];
    assert!(compare(&event, &bank, true).reconciled());
    assert!(compare(&event, &xero, true).reconciled());
    xero.journal_account = Some("different-firm-account".into());
    assert!(!compare(&event, &xero, true).reconciled());
}

#[tokio::test]
async fn preview_runs_real_constraints_without_persisting_entries() {
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let db = store::application_store::ApplicationStore::uninitialized();
        let firm = firm(&db).await;
        let event = entry(firm, "preview-only");
        assert_eq!(preview(&db, &pg, &event).await.unwrap(), event.id);
        assert!(entries(&pg, firm).await.unwrap().is_empty());
        assert_eq!(post(&db, &pg, &event).await.unwrap(), event.id);
        assert_eq!(entries(&pg, firm).await.unwrap().len(), 1);
    })
    .await;
}

#[tokio::test]
async fn reconciliation_preview_missing_source_and_approved_retry_are_audited() {
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let db = store::application_store::ApplicationStore::uninitialized();
        let firm = firm(&db).await;
        let project = store::projects::create(
            &db,
            &store::projects::NewProject {
                code: format!("journal-{}", Uuid::now_v7().simple()),
                name: "Synthetic allocation".into(),
                status: "open".into(),
                entity_id: entity(&db).await,
                firm_id: Some(firm),
                brand: project_brand(&db, firm).await,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id;
        let tmp = tempfile::tempdir().unwrap();
        let storage: std::sync::Arc<dyn cloud::StorageService> = std::sync::Arc::new(
            cloud::FsStorage::new(tmp.path().to_path_buf())
                .await
                .unwrap(),
        );
        let bytes = b"synthetic source and complete account coverage evidence";
        let receipt = store::documents::ingest_bytes(
            &db,
            &storage,
            &store::documents::IngestArgs {
                project_id: project,
                source: "upload",
                filename: "synthetic-statement.json",
                kind: "unclassified",
                content_type: "application/json",
                description: None,
                secondary_storage_key: None,
                visibility: store::documents::visibility::INTERNAL,
            },
            bytes,
        )
        .await
        .unwrap();
        let mut snapshot = SourceSnapshot::synthetic("USD", 2, "10000");
        snapshot.digest = receipt.sha256_hex;
        snapshot.retrieved_at = chrono::Utc::now().to_rfc3339();
        snapshot.receipt_document_ids = vec![receipt.asset_id];
        let mut event = entry(firm, "repair");
        event.sources = vec![snapshot.source.clone()];
        event.receipts = vec![receipt.asset_id];
        let source_hash = store::documents::sha256_hex(
            serde_json::to_string(&snapshot.source).unwrap().as_bytes(),
        );
        event.key = format!("repair:{source_hash}:{}:posting", snapshot.revision);
        let mut request = ReconciliationRequest {
            signal_cursor: 0,
            id: Uuid::now_v7(),
            scope: ReconciliationScope {
                firm_id: firm,
                period_start: "2026-10-01T00:00:00Z".into(),
                period_end: "2026-11-01T00:00:00Z".into(),
                sources: event.sources.clone(),
                complete_statement_document_id: Some(receipt.asset_id),
            },
            mode: RepairMode::Preview,
            snapshots: vec![snapshot],
            proposed_entries: vec![event.clone()],
            approvals: vec![RepairApproval {
                actor_id: event.actor_id,
                approved_at: chrono::Utc::now().to_rfc3339(),
                command_digest: entry_digest(&event).unwrap(),
            }],
        };
        assert!(!reconcile(&db, &pg, &request).await.unwrap().reconciled());
        assert!(entries(&pg, firm).await.unwrap().is_empty());
        request.mode = RepairMode::ApprovedSafe;
        assert!(reconcile(&db, &pg, &request).await.unwrap().reconciled());
        assert!(reconcile(&db, &pg, &request).await.unwrap().reconciled());
        assert_eq!(entries(&pg, firm).await.unwrap().len(), 1);
        request.snapshots.clear();
        let audit = reconcile(&db, &pg, &request).await.unwrap();
        assert!(!audit.reconciled());
        assert!(audit.exceptions.iter().any(|e| e.contains("unavailable")));
        assert_eq!(audits(&pg, firm, Some(request.id)).await.unwrap().len(), 4);
        let audit_ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM journal_reconciliation WHERE firm_id=$1 AND job_id=$2",
        )
        .bind(firm)
        .bind(request.id)
        .fetch_all(pg.pool())
        .await
        .unwrap();
        assert!(audit_ids.iter().all(|id| id.get_version_num() == 4));
    })
    .await;
}

#[test]
fn costs_and_fees_have_independent_exact_balances() {
    let firm = Uuid::now_v7();
    let project = Uuid::now_v7();
    let mut cost = entry(firm, "cost");
    cost.postings[0].category = Category::AdvancedCost;
    cost.postings[0].project_id = Some(project);
    let mut repayment = entry(firm, "partial-repayment");
    repayment.postings[0].quantity = "6000".into();
    repayment.postings[1].quantity = "-6000".into();
    repayment.postings[1].category = Category::Reimbursement;
    repayment.postings[1].project_id = Some(project);
    let mut fee = entry(firm, "earned-fee");
    fee.postings[1].category = Category::EarnedFee;
    fee.postings[1].project_id = Some(project);
    let costs = cost_positions(&[cost, repayment, fee], Some(project)).unwrap();
    assert_eq!(costs.len(), 1);
    assert_eq!(
        (
            &*costs[0].incurred,
            &*costs[0].reimbursed,
            &*costs[0].outstanding
        ),
        ("10000", "6000", "4000")
    );
}

#[test]
fn property_award_is_not_receipt_and_conversion_keeps_assets_separate() {
    let firm = Uuid::now_v7();
    let asset = Uuid::now_v7();
    let mut award = entry(firm, "property-award");
    for posting in &mut award.postings {
        posting.unit = Unit::Noncash {
            property_id: asset,
            decimals: 0,
        };
    }
    award.postings[0].category = Category::Award;
    award.postings[1].category = Category::Custody;
    assert!(award.validate().is_ok());
    let mut observation = SourceSnapshot::synthetic("USD", 2, "10000");
    observation.unit = Unit::Noncash {
        property_id: asset,
        decimals: 0,
    };
    observation.facts = SourceFacts::Noncash {
        property_id: asset,
        description: "Synthetic property".into(),
        custody_reference: "synthetic-title".into(),
        received: false,
        valuation: Valuation::Unknown,
    };
    assert!(!compare(&award, &observation, true).reconciled());
    observation.facts = SourceFacts::Noncash {
        property_id: asset,
        description: "Synthetic property".into(),
        custody_reference: "synthetic-custody".into(),
        received: true,
        valuation: Valuation::Unknown,
    };
    assert!(compare(&award, &observation, true).reconciled());
    let mut sale = entry(firm, "property-sale");
    for posting in &mut sale.postings {
        posting.category = Category::Disposal;
    }
    let mut disposition = award.postings;
    for posting in &mut disposition {
        posting.quantity = (-quantity(&posting.quantity).unwrap()).to_string();
        posting.category = Category::Disposal;
    }
    sale.postings.extend(disposition);
    assert!(sale.validate().is_ok());
    assert_eq!(
        statement(&[sale], None)
            .unwrap()
            .iter()
            .map(|line| line.unit.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        2
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "synthetic integration scenario keeps setup and assertions together"
)]
async fn concurrent_repayments_cannot_exceed_reviewed_project_costs() {
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let db = store::application_store::ApplicationStore::uninitialized();
        let firm = firm(&db).await;
        let reviewer = store::persons::create(
            &db,
            &store::persons::NewPerson::with_role(
                "Synthetic reviewer",
                format!("reviewer-{}@example.com", Uuid::now_v7()),
                store::persons::Role::Owner,
            ),
        )
        .await
        .unwrap();
        let project = store::projects::create(
            &db,
            &store::projects::NewProject {
                code: format!("cost-{}", Uuid::now_v7().simple()),
                name: "Synthetic reviewed Project".into(),
                status: "open".into(),
                entity_id: entity(&db).await,
                firm_id: Some(firm),
                brand: project_brand(&db, firm).await,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id;
        let tmp = tempfile::tempdir().unwrap();
        let storage: std::sync::Arc<dyn cloud::StorageService> = std::sync::Arc::new(
            cloud::FsStorage::new(tmp.path().to_path_buf())
                .await
                .unwrap(),
        );
        let document = store::documents::ingest_bytes_as(
            &db,
            &storage,
            &store::documents::IngestArgs {
                project_id: project,
                source: "upload",
                filename: "signed-synthetic-engagement.pdf",
                kind: "unclassified",
                content_type: "application/pdf",
                description: None,
                secondary_storage_key: None,
                visibility: store::documents::visibility::INTERNAL,
            },
            &store::documents::DocumentIdentity {
                signing: Some(store::documents::Signing {
                    method: store::documents::SigningMethod::WetInk,
                    signed_on: chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
                }),
                ..Default::default()
            },
            b"synthetic executed engagement and reviewed cost terms",
        )
        .await
        .unwrap();
        let engagement = JournalLink {
            kind: LinkKind::Engagement,
            target_id: project,
            verified_by: reviewer.id,
            verified_at: chrono::Utc::now().to_rfc3339(),
            evidence_document_id: document.asset_id,
        };
        let mut cost = entry(firm, "reviewed-cost");
        cost.actor_id = reviewer.id;
        for posting in &mut cost.postings {
            posting.ownership = BeneficialOwnership::Firm;
        }
        cost.postings[0].account = "advanced-costs".into();
        cost.postings[0].category = Category::AdvancedCost;
        cost.postings[0].project_id = Some(project);
        cost.postings[1].account = "bank".into();
        cost.postings[1].category = Category::Cash;
        assert!(
            preview(&db, &pg, &cost).await.is_err(),
            "unsigned/unreviewed cost must fail"
        );
        cost.links.push(engagement.clone());
        cost.receipts.push(document.asset_id);
        post(&db, &pg, &cost).await.unwrap();
        let repayment = |key| {
            let mut event = entry(firm, key);
            event.actor_id = reviewer.id;
            event.receipts = vec![document.asset_id];
            event.links = vec![
                engagement.clone(),
                JournalLink {
                    kind: LinkKind::Repayment,
                    target_id: cost.id,
                    verified_by: reviewer.id,
                    verified_at: chrono::Utc::now().to_rfc3339(),
                    evidence_document_id: document.asset_id,
                },
            ];
            for posting in &mut event.postings {
                posting.ownership = BeneficialOwnership::Firm;
            }
            event.postings[0].quantity = "6000".into();
            event.postings[1].quantity = "-6000".into();
            event.postings[1].category = Category::Reimbursement;
            event.postings[1].account = "advanced-costs".into();
            event.postings[1].project_id = Some(project);
            event
        };
        let first = repayment("repay-one");
        let second = repayment("repay-two");
        let (a, b) = tokio::join!(post(&db, &pg, &first), post(&db, &pg, &second));
        assert_ne!(
            a.is_ok(),
            b.is_ok(),
            "only one competing reimbursement may commit"
        );
        let costs = cost_positions(&entries(&pg, firm).await.unwrap(), Some(project)).unwrap();
        assert_eq!(costs[0].outstanding, "4000");
    })
    .await;
}

#[test]
fn financial_revision_is_stable_across_evidence_refresh_but_not_amount_changes() {
    let snapshot = SourceSnapshot::synthetic("USD", 2, "123");
    let revision = source_revision(&snapshot).unwrap();
    let mut refreshed = snapshot.clone();
    refreshed.retrieved_at = chrono::Utc::now().to_rfc3339();
    refreshed.digest = "different-original-response-digest".into();
    refreshed.evidence_document_ids.push(Uuid::now_v7());
    assert_eq!(revision, source_revision(&refreshed).unwrap());
    refreshed.quantity = "124".into();
    assert_ne!(revision, source_revision(&refreshed).unwrap());
}

#[test]
fn crypto_transfer_requires_scoped_locator_finality_and_exact_separate_fees() {
    let mut e = entry(Uuid::now_v7(), "crypto-transfer");
    let unit = Unit::Crypto {
        network: "solana:devnet".into(),
        contract: "native".into(),
        decimals: 9,
    };
    for p in &mut e.postings {
        p.unit = unit.clone();
        p.category = Category::Recovery;
    }
    let source = SourceReference {
        provider: "solana".into(),
        scope: "solana:devnet".into(),
        account: "bank".into(),
        object_type: "transfer".into(),
        external_id: "synthetic-signature".into(),
        locator: Some("instruction:0".into()),
    };
    e.sources.push(source.clone());
    e.sources.push(SourceReference {
        object_type: "network_fee".into(),
        locator: None,
        ..source.clone()
    });
    let mut debit = e.postings[0].clone();
    debit.category = Category::Fee;
    debit.quantity = "-5000".into();
    let mut credit = debit.clone();
    credit.account = "network-fee-expense".into();
    credit.quantity = "5000".into();
    e.postings.extend([debit, credit]);
    let mut snapshot = SourceSnapshot::synthetic("USD", 2, "10000");
    snapshot.source = source;
    snapshot.unit = unit.clone();
    snapshot.facts = SourceFacts::Crypto {
        transaction_hash: "synthetic-signature".into(),
        transfer_locator: "instruction:0".into(),
        wallet: "bank".into(),
        finality_evidence_document_id: Uuid::now_v7(),
        finality: CryptoFinality::Confirmations {
            observed: 31,
            required: 32,
        },
        fee_unit: unit,
        fee_quantity: "5000".into(),
    };
    assert!(!compare(&e, &snapshot, true).reconciled());
    if let SourceFacts::Crypto { finality, .. } = &mut snapshot.facts {
        *finality = CryptoFinality::Finalized;
    }
    assert!(compare(&e, &snapshot, true).reconciled());
    let mut fee = SourceSnapshot::synthetic("USD", 2, "-5000");
    fee.source = e.sources[1].clone();
    fee.unit = snapshot.unit.clone();
    fee.facts = SourceFacts::NetworkFee {
        transaction_hash: "synthetic-signature".into(),
        wallet: "bank".into(),
        finality_evidence_document_id: Uuid::now_v7(),
        finality: CryptoFinality::Finalized,
    };
    assert!(
        compare(&e, &fee, true).reconciled(),
        "fee matching must exclude the transfer quantity"
    );
    snapshot.source.locator = Some("instruction:1".into());
    assert!(!compare(&e, &snapshot, true).reconciled());
    snapshot.source.locator = Some("instruction:0".into());
    e.sources.retain(|s| s.object_type != "network_fee");
    assert!(!compare(&e, &fee, true).reconciled());
    assert!(!CryptoFinality::Confirmations {
        observed: 100,
        required: 0
    }
    .satisfied());
}

#[tokio::test]
async fn chain_transfer_locators_are_distinct_but_transaction_fee_is_unique() {
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let db = store::application_store::ApplicationStore::uninitialized();
        let firm = firm(&db).await;
        let mut first = entry(firm, "chain:instruction:0");
        let unit = Unit::Crypto {
            network: "solana:devnet".into(),
            contract: "native".into(),
            decimals: 9,
        };
        for p in &mut first.postings {
            p.unit = unit.clone();
            p.category = Category::Recovery;
        }
        let source = SourceReference {
            provider: "solana".into(),
            scope: "solana:devnet".into(),
            account: "bank".into(),
            object_type: "transfer".into(),
            external_id: "synthetic-chain-signature".into(),
            locator: Some("instruction:0".into()),
        };
        let fee = SourceReference {
            object_type: "network_fee".into(),
            locator: None,
            ..source.clone()
        };
        first.sources = vec![source.clone(), fee.clone()];
        let mut debit = first.postings[0].clone();
        debit.category = Category::Fee;
        debit.quantity = "-5000".into();
        let mut credit = debit.clone();
        credit.account = "network-fee-expense".into();
        credit.quantity = "5000".into();
        first.postings.extend([debit, credit]);
        post(&db, &pg, &first).await.unwrap();
        let mut second = first.clone();
        second.id = Uuid::now_v7();
        second.key = "chain:instruction:1".into();
        second.postings.retain(|p| p.category != Category::Fee);
        second.sources = vec![SourceReference {
            locator: Some("instruction:1".into()),
            ..source
        }];
        post(&db, &pg, &second).await.unwrap();
        let mut duplicate_fee = second.clone();
        duplicate_fee.id = Uuid::now_v7();
        duplicate_fee.key = "chain:instruction:2".into();
        duplicate_fee.sources[0].locator = Some("instruction:2".into());
        duplicate_fee.postings.extend(
            first
                .postings
                .iter()
                .filter(|p| p.category == Category::Fee)
                .cloned(),
        );
        let mut absent_fee_source = duplicate_fee.clone();
        absent_fee_source.key = "chain:missing-fee-source".into();
        assert!(post(&db, &pg, &absent_fee_source).await.is_err());
        duplicate_fee.sources.push(fee);
        assert!(post(&db, &pg, &duplicate_fee).await.is_err());
        assert_eq!(entries(&pg, firm).await.unwrap().len(), 2);
    })
    .await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "synthetic integration scenario keeps setup and assertions together"
)]
async fn verified_signals_only_match_existing_firm_sources_and_are_idempotent() {
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let db = store::application_store::ApplicationStore::uninitialized();
        let firm = firm(&db).await;
        let mut e = entry(firm, "signal-source");
        let source = SourceReference {
            provider: "xero".into(),
            scope: "selected-tenant".into(),
            account: "bank".into(),
            object_type: "invoice".into(),
            external_id: Uuid::now_v7().to_string(),
            locator: None,
        };
        e.sources.push(source.clone());
        post(&db, &pg, &e).await.unwrap();
        let event = "a".repeat(64);
        let at = "2026-10-11T00:00:00Z";
        assert_eq!(
            signal(
                &pg,
                Uuid::now_v7(),
                "xero",
                &source.scope,
                "invoice",
                &source.external_id,
                &event,
                at
            )
            .await
            .unwrap(),
            0
        );
        assert_eq!(
            signal(
                &pg,
                firm,
                "xero",
                "other-tenant",
                "invoice",
                &source.external_id,
                &event,
                at
            )
            .await
            .unwrap(),
            0
        );
        assert_eq!(
            signal(
                &pg,
                firm,
                "xero",
                &source.scope,
                "invoice",
                "unlinked",
                &event,
                at
            )
            .await
            .unwrap(),
            0
        );
        assert_eq!(
            signal(
                &pg,
                firm,
                "xero",
                &source.scope,
                "invoice",
                &source.external_id,
                &event,
                at
            )
            .await
            .unwrap(),
            1
        );
        assert_eq!(
            signal(
                &pg,
                firm,
                "xero",
                &source.scope,
                "invoice",
                &source.external_id,
                &event,
                at
            )
            .await
            .unwrap(),
            0
        );
        let signals = signals(&pg, firm, 0).await.unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].source, source);
        assert!(store::firm_journal::signals(&pg, firm, signals[0].cursor)
            .await
            .unwrap()
            .is_empty());
        assert!(store::firm_journal::signals(&pg, Uuid::now_v7(), 0)
            .await
            .unwrap()
            .is_empty());
    })
    .await;
}

#[test]
fn crypto_conversion_preserves_acquisition_and_never_counts_proceeds_as_another_recovery() {
    let mut receipt = entry(Uuid::now_v7(), "crypto-receipt");
    let token = Unit::Crypto {
        network: "solana:devnet".into(),
        contract: "native".into(),
        decimals: 9,
    };
    for posting in &mut receipt.postings {
        posting.unit = token.clone();
        posting.category = Category::Recovery;
    }
    let mut conversion = receipt.clone();
    conversion.id = Uuid::now_v7();
    conversion.key = "crypto-conversion".into();
    for posting in &mut conversion.postings {
        posting.category = Category::Conversion;
        posting.quantity = (-quantity(&posting.quantity).unwrap()).to_string();
    }
    let mut fiat = entry(receipt.firm_id, "fiat-proceeds").postings;
    for posting in &mut fiat {
        posting.category = Category::Conversion;
    }
    conversion.postings.extend(fiat);
    assert!(conversion.validate().is_ok());
    let balances = statement(&[receipt, conversion], None).unwrap();
    assert!(balances
        .iter()
        .filter(|line| line.category == Category::Recovery)
        .all(|line| line.unit == token));
    assert!(balances
        .iter()
        .any(|line| line.category == Category::Conversion && line.unit == Unit::fiat("USD", 2)));
}

#[test]
fn invoice_installments_remain_distinct_and_a_payment_can_allocate_across_projects() {
    let firm = Uuid::now_v7();
    let invoice = Uuid::now_v7();
    let payments = [Uuid::now_v7(), Uuid::now_v7()];
    let mut e = entry(firm, "installment-one");
    e.postings[0].quantity = "6000".into();
    e.postings[1].quantity = "-3000".into();
    e.postings[1].project_id = Some(Uuid::now_v7());
    let mut allocation = e.postings[1].clone();
    allocation.project_id = Some(Uuid::now_v7());
    e.postings.push(allocation);
    let mut snapshot = SourceSnapshot::synthetic("USD", 2, "6000");
    snapshot.source.provider = "xero".into();
    snapshot.source.object_type = "payment".into();
    snapshot.source.external_id = payments[0].to_string();
    snapshot.facts = SourceFacts::Payment {
        credit_note_id: None,
        payment_id: payments[0],
        invoice_id: Some(invoice),
        invoice_number: Some("INV-SYNTHETIC".into()),
        original_currency: Some("USD".into()),
        original_quantity: "6000".into(),
        bank_quantity: "6000".into(),
        currency_rate: None,
        reconciled: Some(true),
    };
    assert!(e.validate().is_ok());
    assert!(compare(&e, &snapshot, true).reconciled());
    let mut second = e.clone();
    second.id = Uuid::now_v7();
    second.key = "installment-two".into();
    second.postings[0].quantity = "4000".into();
    second.postings[1].quantity = "-2000".into();
    second.postings[2].quantity = "-2000".into();
    snapshot.quantity = "4000".into();
    snapshot.source.external_id = payments[1].to_string();
    if let SourceFacts::Payment {
        payment_id,
        bank_quantity,
        original_quantity,
        ..
    } = &mut snapshot.facts
    {
        *payment_id = payments[1];
        *bank_quantity = "4000".into();
        *original_quantity = "4000".into();
    }
    assert!(compare(&second, &snapshot, true).reconciled());
    if let SourceFacts::Payment { payment_id, .. } = &mut snapshot.facts {
        *payment_id = invoice;
    }
    assert!(!compare(&second, &snapshot, true).reconciled());
}

#[tokio::test]
async fn postgres_journal_store_preserves_preview_idempotency_and_reversal_boundaries() {
    store::test_support::with_postgres(async {
        let db = store::application_store::ApplicationStore::uninitialized();
        let journal = JournalStore::from_env().await.unwrap();
        let firm = firm(&db).await;
        let e = entry(firm, "manual:postgres");
        assert_eq!(journal.preview(&db, &e).await.unwrap(), e.id);
        assert!(journal.entries(firm).await.unwrap().is_empty());
        let (a, b) = tokio::join!(journal.post(&db, &e), journal.post(&db, &e));
        assert_eq!(a.unwrap(), e.id);
        assert_eq!(b.unwrap(), e.id);
        assert_eq!(journal.entries(firm).await.unwrap(), vec![e.clone()]);
        let mut changed = e.clone();
        changed.memo = "Different economic event".into();
        assert!(matches!(
            journal.post(&db, &changed).await,
            Err(JournalError::Conflict)
        ));
        let reversal = e
            .reversal(
                "manual:postgres:reversal",
                e.actor_id,
                "2026-10-12T00:00:00Z",
            )
            .unwrap();
        journal.post(&db, &reversal).await.unwrap();
        let again = e
            .reversal("manual:postgres:again", e.actor_id, "2026-10-12T00:00:00Z")
            .unwrap();
        assert!(journal.post(&db, &again).await.is_err());
        assert_eq!(journal.entries(firm).await.unwrap(), vec![e, reversal]);
    })
    .await;
}
