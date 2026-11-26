//! Purchases against real PostgreSQL: one open invoice per Client and
//! Offering, durable before Xero is called, active only once paid.
use chrono::{TimeDelta, TimeZone, Utc};
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::PgPool;
use store::postgres::PostgresDb;
use store::purchases::{self, Cadence, NewPurchase, PurchaseError, PurchaseState};
use uuid::Uuid;

async fn database() -> Option<PostgresDb> {
    let port = store::test_support::postgres_test_port();
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        )
        .ssl_mode(PgSslMode::Disable);
    let admin = PgPool::connect_with(options.clone()).await.unwrap();
    let name = format!("purchase_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let pg = PostgresDb::connect(options.database(&name)).await.unwrap();
    pg.migrate_embedded(None).await.unwrap();
    Some(pg)
}

fn plan(person_id: Uuid) -> NewPurchase {
    NewPurchase {
        person_id,
        email: "client@example.com".into(),
        offering: "delete-your-data-monthly".into(),
        offering_name: "Delete Your Data, monthly".into(),
        cadence: Cadence::Month,
        amount_cents: 2_900,
        currency: "USD".into(),
        account_code: "200".into(),
    }
}

#[tokio::test]
async fn a_purchase_is_invoiced_once_and_active_only_after_payment() {
    let Some(pg) = database().await else { return };
    let now = Utc.with_ymd_and_hms(2026, 10, 20, 12, 0, 0).unwrap();
    let person = Uuid::new_v4();

    let first = purchases::open(&pg, &plan(person), now).await.unwrap();
    assert_eq!(first.state, PurchaseState::Invoicing);
    let again = purchases::open(&pg, &plan(person), now).await.unwrap();
    assert_eq!(again.id, first.id, "a retry finds the open Purchase");

    let invoiced = purchases::record_invoice(
        &pg,
        first.id,
        "contact",
        "inv-1",
        Some("https://in.xero.test/1"),
        now,
    )
    .await
    .unwrap();
    assert_eq!(invoiced.state, PurchaseState::Invoiced);
    assert_eq!(
        invoiced.invoice_url.as_deref(),
        Some("https://in.xero.test/1")
    );
    let replay = purchases::record_invoice(&pg, first.id, "contact", "inv-2", None, now)
        .await
        .unwrap();
    assert_eq!(
        replay.xero_invoice_id.as_deref(),
        Some("inv-1"),
        "the first invoice stands"
    );
    assert_eq!(
        purchases::open(&pg, &plan(person), now).await.unwrap().id,
        first.id
    );

    assert_eq!(purchases::invoiced(&pg, 10).await.unwrap().len(), 1);
    let later = now + TimeDelta::minutes(5);
    let active = purchases::activate(&pg, first.id, Some("rep-1"), later)
        .await
        .unwrap();
    assert_eq!(active.state, PurchaseState::Active);
    assert_eq!(active.activated_at, Some(later));
    assert_eq!(active.xero_repeating_invoice_id.as_deref(), Some("rep-1"));
    let twice = purchases::activate(&pg, first.id, None, later + TimeDelta::minutes(5))
        .await
        .unwrap();
    assert_eq!(twice.activated_at, Some(later), "activation is idempotent");
    assert!(purchases::invoiced(&pg, 10).await.unwrap().is_empty());

    let renewal = purchases::open(&pg, &plan(person), later).await.unwrap();
    assert_ne!(
        renewal.id, first.id,
        "a closed Purchase no longer blocks a new one"
    );
    assert_eq!(purchases::for_person(&pg, person).await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_complimentary_consultation_is_never_invoiced_and_prices_are_positive() {
    let Some(pg) = database().await else { return };
    let now = Utc.with_ymd_and_hms(2026, 10, 20, 12, 0, 0).unwrap();
    let person = Uuid::new_v4();
    let consultation = NewPurchase {
        offering: "consultation".into(),
        offering_name: "Consultation".into(),
        cadence: Cadence::Once,
        amount_cents: 0,
        ..plan(person)
    };
    assert!(matches!(
        purchases::open(&pg, &consultation, now).await,
        Err(PurchaseError::InvalidAmount)
    ));
    let free = purchases::record_complimentary(&pg, &consultation, now)
        .await
        .unwrap();
    assert_eq!(free.state, PurchaseState::Complimentary);
    assert_eq!(free.amount_cents, 0);
    assert!(free.xero_invoice_id.is_none());
    assert!(matches!(
        purchases::activate(&pg, free.id, None, now).await,
        Err(PurchaseError::NotFound)
    ));
    assert!(purchases::invoiced(&pg, 10).await.unwrap().is_empty());
}

/// The monthly credit reset reads its subscriber list through this one
/// query: every Purchase of the Same Day Legal offerings, in every state. State
/// filtering is the caller's decision — an `active` Purchase makes a
/// subscriber, while an `invoiced` one still names the contact an overage
/// invoice bills to.
#[tokio::test]
async fn for_offerings_lists_same_day_legal_purchases_in_every_state_and_no_others() {
    let Some(pg) = database().await else { return };
    let now = Utc.with_ymd_and_hms(2026, 10, 20, 12, 0, 0).unwrap();
    let monthly_subscriber = Uuid::new_v4();
    let annual_subscriber = Uuid::new_v4();
    let other = Uuid::new_v4();

    purchases::open(&pg, &plan(monthly_subscriber), now)
        .await
        .unwrap();
    let annual = NewPurchase {
        offering: "delete-your-data-annual".into(),
        offering_name: "Delete Your Data, annual".into(),
        cadence: Cadence::Year,
        ..plan(annual_subscriber)
    };
    let annual = purchases::open(&pg, &annual, now).await.unwrap();
    purchases::record_invoice(&pg, annual.id, "contact", "annual-invoice", None, now)
        .await
        .unwrap();
    purchases::activate(&pg, annual.id, None, now + TimeDelta::minutes(5))
        .await
        .unwrap();
    let other = NewPurchase {
        offering: "consultation".into(),
        offering_name: "Consultation".into(),
        ..plan(other)
    };
    purchases::record_complimentary(&pg, &other, now)
        .await
        .unwrap();

    let listed = purchases::for_offerings(&pg, &purchases::SAME_DAY_LEGAL_OFFERINGS)
        .await
        .unwrap();
    let listed_people: std::collections::BTreeSet<Uuid> =
        listed.iter().map(|p| p.person_id).collect();
    assert_eq!(
        listed_people,
        [monthly_subscriber, annual_subscriber]
            .into_iter()
            .collect(),
        "the two Same Day Legal subscribers, in both states, and no other offering"
    );
    assert!(listed
        .iter()
        .all(|p| purchases::SAME_DAY_LEGAL_OFFERINGS.contains(&p.offering.as_str())));
}

async fn pending_stripe(pg: &PostgresDb, now: chrono::DateTime<Utc>) -> purchases::Purchase {
    let row = purchases::open_stripe(
        pg,
        &plan(Uuid::new_v4()),
        now,
        purchases::StripeAdmission {
            firm_id: Uuid::new_v4(),
            account_id: "acct_synthetic",
            terms: "Accepted terms",
        },
    )
    .await
    .unwrap();
    purchases::record_stripe_checkout(
        pg,
        row.id,
        "cs_test_synthetic",
        "https://checkout.stripe.com/c/pay/synthetic",
        now,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn stripe_checkout_retains_consent_and_refuses_a_changed_offer() {
    let Some(pg) = database().await else { return };
    let now = Utc::now();
    let new = plan(Uuid::new_v4());
    let firm = Uuid::new_v4();
    let terms = "$29 each month until cancelled online before the next charge.";
    let row = purchases::open_stripe(
        &pg,
        &new,
        now,
        purchases::StripeAdmission {
            firm_id: firm,
            account_id: "acct_synthetic",
            terms,
        },
    )
    .await
    .unwrap();
    assert!(!row.is_active(now));
    let binding = row.stripe.as_ref().unwrap();
    assert_eq!(binding.renewal_terms, terms);
    assert_eq!(
        binding.consented_at.timestamp_micros(),
        now.timestamp_micros()
    );
    assert_eq!(binding.firm_id, firm);
    assert!(purchases::open_stripe(
        &pg,
        &new,
        now,
        purchases::StripeAdmission {
            firm_id: firm,
            account_id: "acct_synthetic",
            terms: "Changed terms"
        }
    )
    .await
    .is_err());
    let checkout = purchases::record_stripe_checkout(
        &pg,
        row.id,
        "cs_test_synthetic",
        "https://checkout.stripe.com/c/pay/synthetic",
        now,
    )
    .await
    .unwrap();
    assert_eq!(checkout.state, PurchaseState::Invoiced);
    assert!(!checkout.is_active(now));
    pg.pool().close().await;
}

#[tokio::test]
async fn verified_paid_time_and_cancellation_preserve_subscription_identity() {
    let Some(pg) = database().await else { return };
    let now = Utc.with_ymd_and_hms(2026, 11, 18, 12, 0, 0).unwrap();
    let row = pending_stripe(&pg, now).await;
    let end = Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap();
    let active = purchases::sync_stripe(
        &pg,
        row.id,
        purchases::StripeObservation {
            customer: "cus_synthetic",
            subscription: "sub_synthetic",
            paid_through: end,
            cancelling: false,
        },
        now,
    )
    .await
    .unwrap();
    assert!(active.is_active(end - TimeDelta::seconds(1)));
    assert!(!active.is_active(end));
    let attempted = now + TimeDelta::minutes(5);
    purchases::touch(&pg, row.id, attempted).await.unwrap();
    let refreshed_queue = purchases::payment_refresh_queue(&pg, 10).await.unwrap();
    assert_eq!(refreshed_queue.len(), 1);
    assert_eq!(refreshed_queue[0].id, row.id);
    assert_eq!(
        refreshed_queue[0].updated_at.timestamp_micros(),
        attempted.timestamp_micros()
    );
    assert!(purchases::sync_stripe(
        &pg,
        row.id,
        purchases::StripeObservation {
            customer: "cus_other",
            subscription: "sub_other",
            paid_through: end,
            cancelling: false,
        },
        now
    )
    .await
    .is_err());
    let cancelling = purchases::sync_stripe(
        &pg,
        row.id,
        purchases::StripeObservation {
            customer: "cus_synthetic",
            subscription: "sub_synthetic",
            paid_through: end,
            cancelling: true,
        },
        now,
    )
    .await
    .unwrap();
    assert!(cancelling.is_active(now));
    assert!(cancelling.is_active(end - TimeDelta::seconds(1)));
    assert!(!cancelling.is_active(end));
    assert!(cancelling.stripe.unwrap().cancel_at_period_end);
    let first = purchases::get(&pg, row.id)
        .await
        .unwrap()
        .renewal_cancelled_at;
    purchases::record_renewal_cancelled(&pg, row.id, now + TimeDelta::hours(1))
        .await
        .unwrap();
    assert_eq!(
        purchases::get(&pg, row.id)
            .await
            .unwrap()
            .renewal_cancelled_at,
        first
    );
    pg.pool().close().await;
}

#[tokio::test]
async fn an_overdue_subscription_keeps_its_reservation_until_renewal_stops() {
    let Some(pg) = database().await else { return };
    let now = Utc::now();
    let row = pending_stripe(&pg, now).await;
    let new = plan(row.person_id);
    let binding = row.stripe.as_ref().unwrap();
    let firm = binding.firm_id;
    let terms = binding.renewal_terms.as_str();
    let end = now;
    let overdue = purchases::sync_stripe(
        &pg,
        row.id,
        purchases::StripeObservation {
            customer: "cus_synthetic",
            subscription: "sub_synthetic",
            paid_through: end,
            cancelling: false,
        },
        end,
    )
    .await
    .unwrap();
    assert!(overdue.renewal_cancelled_at.is_none());
    let mut competing = new.clone();
    competing.offering = "annual".into();
    competing.cadence = Cadence::Year;
    assert!(purchases::open_stripe(
        &pg,
        &competing,
        end,
        purchases::StripeAdmission {
            firm_id: firm,
            account_id: "acct_synthetic",
            terms,
        }
    )
    .await
    .is_err());
    assert!(!overdue.is_active(now));
    pg.pool().close().await;
}

#[tokio::test]
async fn concurrent_monthly_and_yearly_stripe_checkouts_reserve_only_one_subscription() {
    let Some(pg) = database().await else { return };
    let now = Utc::now();
    let monthly = plan(Uuid::new_v4());
    let mut annual = plan(monthly.person_id);
    annual.offering = "delete-your-data-annual".into();
    annual.cadence = Cadence::Year;
    let firm = Uuid::new_v4();
    let (a, b) = tokio::join!(
        purchases::open_stripe(
            &pg,
            &monthly,
            now,
            purchases::StripeAdmission {
                firm_id: firm,
                account_id: "acct_synthetic",
                terms: "Monthly terms"
            }
        ),
        purchases::open_stripe(
            &pg,
            &annual,
            now,
            purchases::StripeAdmission {
                firm_id: firm,
                account_id: "acct_synthetic",
                terms: "Annual terms"
            }
        ),
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(
        purchases::for_person(&pg, monthly.person_id)
            .await
            .unwrap()
            .len(),
        1
    );
    pg.pool().close().await;
}

#[tokio::test]
async fn cancelling_a_xero_schedule_preserves_its_period_without_granting_access_forever() {
    let Some(pg) = database().await else { return };
    let now = Utc::now();
    let row = purchases::open(&pg, &plan(Uuid::new_v4()), now)
        .await
        .unwrap();
    purchases::record_invoice(
        &pg,
        row.id,
        "contact-synthetic",
        "invoice-synthetic",
        None,
        now,
    )
    .await
    .unwrap();
    purchases::activate(&pg, row.id, Some("repeating-synthetic"), now)
        .await
        .unwrap();
    let end = now + TimeDelta::days(7);
    purchases::record_xero_cancelled(&pg, row.id, end, now)
        .await
        .unwrap();
    let cancelled = purchases::get(&pg, row.id).await.unwrap();
    assert!(cancelled.is_active(end - TimeDelta::seconds(1)));
    assert!(!cancelled.is_active(end));
    assert!(cancelled.renewal_cancelled_at.is_some());
    purchases::record_xero_cancelled(&pg, row.id, end + TimeDelta::days(30), now)
        .await
        .unwrap();
    assert_eq!(
        purchases::get(&pg, row.id).await.unwrap().access_ends_at,
        cancelled.access_ends_at
    );
    pg.pool().close().await;
}

#[tokio::test]
async fn an_expired_unpaid_checkout_releases_only_its_own_reservation() {
    let Some(pg) = database().await else { return };
    let now = Utc::now();
    let new = plan(Uuid::new_v4());
    let firm = Uuid::new_v4();
    let admission = || purchases::StripeAdmission {
        firm_id: firm,
        account_id: "acct_synthetic",
        terms: "Monthly terms",
    };
    let first = purchases::open_stripe(&pg, &new, now, admission())
        .await
        .unwrap();
    purchases::record_stripe_checkout(
        &pg,
        first.id,
        "cs_test_expired",
        "https://checkout.stripe.com/c/pay/expired",
        now,
    )
    .await
    .unwrap();
    assert!(
        purchases::expire_stripe_checkout(&pg, first.id, "cs_test_other", now)
            .await
            .is_err()
    );
    let expired = purchases::expire_stripe_checkout(&pg, first.id, "cs_test_expired", now)
        .await
        .unwrap();
    assert_eq!(expired.state, PurchaseState::Expired);
    assert!(!expired.is_active(now));
    assert_eq!(expired.stripe.unwrap().renewal_terms, "Monthly terms");
    let retry = purchases::open_stripe(&pg, &new, now, admission())
        .await
        .unwrap();
    assert_ne!(first.id, retry.id);
    purchases::record_stripe_checkout(
        &pg,
        retry.id,
        "cs_test_retry",
        "https://checkout.stripe.com/c/pay/retry",
        now,
    )
    .await
    .unwrap();
    purchases::sync_stripe(
        &pg,
        retry.id,
        purchases::StripeObservation {
            customer: "cus_synthetic",
            subscription: "sub_synthetic",
            paid_through: now + TimeDelta::days(30),
            cancelling: false,
        },
        now,
    )
    .await
    .unwrap();
    assert!(
        purchases::expire_stripe_checkout(&pg, retry.id, "cs_test_retry", now)
            .await
            .is_err()
    );
    pg.pool().close().await;
}
