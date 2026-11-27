//! Hosted recurring purchases. Card details stay at Stripe; every mutation is
//! bound to the signed-in Person's Purchase and the Firm's stored account.

use billing::stripe::{StripeInvoiceClient, Subscription, SubscriptionCheckout};
use billing::BillingError;
use chrono::{DateTime, TimeDelta, Utc};
use store::application_store::ApplicationStore;
use store::firm_secrets::{IntegrationProvider, IntegrationSecretKind, ResolvedCredential};
use store::postgres::PostgresDb;
use store::purchases::{self, Purchase, StripeAdmission, StripeObservation};
use uuid::Uuid;

fn refused() -> BillingError {
    BillingError::Provider("subscription billing unavailable or requires reconciliation".into())
}

fn checkout_enabled(activation: Option<&str>) -> bool {
    activation == Some("1")
}

pub(crate) fn enabled() -> bool {
    checkout_enabled(std::env::var("STRIPE_SUBSCRIPTIONS").ok().as_deref())
        && store::firm_secrets::stripe_billing_enabled(IntegrationSecretKind::StripeOperatingAppKey)
}

async fn credential(
    application_store: &ApplicationStore,
    firm: Uuid,
) -> Result<ResolvedCredential, BillingError> {
    let kms = crate::integrations::runtime_kms_from_env().await;
    store::firm_secrets::resolve_firm_credential(
        application_store,
        firm,
        IntegrationProvider::Stripe,
        IntegrationSecretKind::StripeOperatingAppKey,
        kms.as_ref(),
    )
    .await
    .map_err(|_| refused())
}

fn client(key: &ResolvedCredential) -> Result<StripeInvoiceClient<'_>, BillingError> {
    let mode = store::firm_secrets::stripe_mode().map_err(|_| refused())?;
    StripeInvoiceClient::new(
        key.expose_for_provider_call(),
        mode == store::firm_secrets::Mode::Live,
    )
}

async fn current_firm(application_store: &ApplicationStore) -> Result<Uuid, BillingError> {
    store::firms::firm_id_for_brand_key(application_store, views::brand::brand_key().as_str())
        .await
        .map_err(|_| refused())?
        .ok_or_else(refused)
}

pub(crate) fn unsettled_renewal(purchase: &Purchase) -> bool {
    purchase.state == purchases::PurchaseState::Active
        && purchase.renewal_cancelled_at.is_none()
        && purchase
            .stripe
            .as_ref()
            .is_some_and(|s| s.subscription_id.is_some())
}

fn conflicts_with_checkout(purchase: &Purchase, now: DateTime<Utc>) -> bool {
    purchase.cadence != purchases::Cadence::Once
        && (purchase.is_active(now)
            || unsettled_renewal(purchase)
            || (purchase.stripe.is_none()
                && matches!(
                    purchase.state,
                    purchases::PurchaseState::Invoicing | purchases::PurchaseState::Invoiced
                )))
}

pub(crate) async fn checkout(
    application_store: &ApplicationStore,
    pg: &PostgresDb,
    new: &purchases::NewPurchase,
    terms: &str,
) -> Result<String, BillingError> {
    if !enabled() {
        return Err(refused());
    }
    let firm = current_firm(application_store).await?;
    let key = credential(application_store, firm).await?;
    let stripe = client(&key)?;
    let account = stripe.account_id().await?;
    let now = Utc::now();
    // A new plan cannot charge while an existing recurring period still
    // grants access, even after renewal was stopped. This prevents overlap.
    let own = purchases::for_person(pg, new.person_id)
        .await
        .map_err(|_| refused())?;
    if own.iter().any(|p| conflicts_with_checkout(p, now)) {
        return Err(refused());
    }
    let purchase = purchases::open_stripe(
        pg,
        new,
        now,
        StripeAdmission {
            firm_id: firm,
            account_id: &account,
            terms,
        },
    )
    .await
    .map_err(|_| refused())?;
    let binding = purchase.stripe.as_ref().ok_or_else(refused)?;
    if binding.firm_id != firm || binding.account_id != account {
        return Err(refused());
    }
    if let Some(id) = &binding.checkout_session_id {
        let session = stripe.checkout_session(id).await?;
        return session
            .payment_url()
            .filter(|_| session.status == "open")
            .map(str::to_owned)
            .ok_or_else(refused);
    }
    // Stripe retains idempotency keys for at least 24 hours. An uncertain
    // first attempt older than that window requires reconciliation, never a charge retry.
    if now >= purchase.inserted_at + TimeDelta::hours(23) {
        return Err(refused());
    }
    let session = stripe
        .create_subscription_checkout(&SubscriptionCheckout {
            purchase_id: purchase.id,
            email: &purchase.email,
            name: &purchase.offering_name,
            amount_cents: purchase.amount_cents,
            interval: purchase.cadence.as_str(),
            terms: &binding.renewal_terms,
            site_url: views::brand::base_url(),
        })
        .await?;
    let url = session.payment_url().ok_or_else(refused)?;
    purchases::record_stripe_checkout(pg, purchase.id, &session.id, url, now)
        .await
        .map_err(|_| refused())?;
    Ok(url.to_owned())
}

fn observation<'a>(
    purchase: &Purchase,
    subscription: &'a Subscription,
    now: DateTime<Utc>,
) -> Result<StripeObservation<'a>, BillingError> {
    let binding = purchase.stripe.as_ref().ok_or_else(refused)?;
    let [item] = subscription.items.data.as_slice() else {
        return Err(refused());
    };
    if item.price.unit_amount != Some(purchase.amount_cents)
        || item.price.currency != "usd"
        || item.price.recurring.interval != purchase.cadence.as_str()
        || item.price.recurring.interval_count != 1
        || binding
            .customer_id
            .as_ref()
            .is_some_and(|id| id != &subscription.customer)
        || binding
            .subscription_id
            .as_ref()
            .is_some_and(|id| id != &subscription.id)
    {
        return Err(refused());
    }
    let end = DateTime::from_timestamp(item.current_period_end, 0).ok_or_else(refused)?;
    // No free trial or unpaid invoice grants access. A terminal provider state
    // revokes immediately; past-due keeps only the already confirmed period.
    let paid_through = match subscription.status.as_str() {
        "active" => end,
        "past_due" => binding.period_end.unwrap_or(now).min(end),
        _ => now,
    };
    Ok(StripeObservation {
        customer: &subscription.customer,
        subscription: &subscription.id,
        paid_through,
        cancelling: subscription.cancel_at_period_end
            || matches!(
                subscription.status.as_str(),
                "canceled" | "incomplete_expired"
            ),
    })
}

fn confirmed_checkout<'a>(
    purchase: &Purchase,
    session: &'a billing::stripe::CheckoutSession,
) -> Result<(&'a str, &'a str), BillingError> {
    if session.client_reference_id != purchase.id.to_string()
        || session.status != "complete"
        || session.payment_status != "paid"
        || session
            .consent
            .as_ref()
            .and_then(|consent| consent.terms_of_service.as_deref())
            != Some("accepted")
    {
        return Err(refused());
    }
    Ok((
        session.subscription.as_deref().ok_or_else(refused)?,
        session.customer.as_deref().ok_or_else(refused)?,
    ))
}

pub(crate) async fn sync(
    application_store: &ApplicationStore,
    pg: &PostgresDb,
    purchase: &Purchase,
) -> Result<Purchase, BillingError> {
    let binding = purchase.stripe.as_ref().ok_or_else(refused)?;
    let key = credential(application_store, binding.firm_id).await?;
    let stripe = client(&key)?;
    if stripe.account_id().await? != binding.account_id {
        return Err(refused());
    }
    let session = stripe
        .checkout_session(binding.checkout_session_id.as_deref().ok_or_else(refused)?)
        .await?;
    if session.status == "expired" && session.client_reference_id == purchase.id.to_string() {
        return purchases::expire_stripe_checkout(pg, purchase.id, &session.id, Utc::now())
            .await
            .map_err(|_| refused());
    }
    let (subscription_id, customer_id) = confirmed_checkout(purchase, &session)?;
    let sub = stripe.subscription(subscription_id).await?;
    if sub.customer != customer_id {
        return Err(refused());
    }
    purchases::sync_stripe(
        pg,
        purchase.id,
        observation(purchase, &sub, Utc::now())?,
        Utc::now(),
    )
    .await
    .map_err(|_| refused())
}

fn cancellation_observation<'a>(
    purchase: &Purchase,
    subscription: &'a Subscription,
    now: DateTime<Utc>,
) -> Result<StripeObservation<'a>, BillingError> {
    let binding = purchase.stripe.as_ref().ok_or_else(refused)?;
    if binding.customer_id.as_deref() != Some(subscription.customer.as_str())
        || binding.subscription_id.as_deref() != Some(subscription.id.as_str())
        || !subscription.cancel_at_period_end
    {
        return Err(refused());
    }
    let end = binding.period_end.ok_or_else(refused)?;
    Ok(StripeObservation {
        customer: &subscription.customer,
        subscription: &subscription.id,
        paid_through: if subscription.status == "canceled" {
            end.min(now)
        } else {
            end
        },
        cancelling: true,
    })
}

pub(crate) async fn cancel(
    application_store: &ApplicationStore,
    pg: &PostgresDb,
    purchase: &Purchase,
) -> Result<(), BillingError> {
    let binding = purchase.stripe.as_ref().ok_or_else(refused)?;
    if current_firm(application_store).await? != binding.firm_id {
        return Err(refused());
    }
    let key = credential(application_store, binding.firm_id).await?;
    let stripe = client(&key)?;
    if stripe.account_id().await? != binding.account_id {
        return Err(refused());
    }
    let sub = stripe
        .cancel_subscription(
            purchase.id,
            binding.subscription_id.as_deref().ok_or_else(refused)?,
        )
        .await?;
    purchases::sync_stripe(
        pg,
        purchase.id,
        cancellation_observation(purchase, &sub, Utc::now())?,
        Utc::now(),
    )
    .await
    .map_err(|_| refused())?;
    purchases::record_renewal_cancelled(pg, purchase.id, Utc::now())
        .await
        .map_err(|_| refused())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use purchases::{Cadence, PurchaseState, StripePurchase};

    #[test]
    fn draft_billing_configuration_does_not_implicitly_enable_recurring_charges() {
        for activation in [None, Some(""), Some("0"), Some("false"), Some("true")] {
            assert!(!checkout_enabled(activation));
        }
        assert!(checkout_enabled(Some("1")));
    }

    fn purchase(now: DateTime<Utc>) -> Purchase {
        Purchase {
            id: Uuid::now_v7(),
            person_id: Uuid::now_v7(),
            email: "client@example.com".into(),
            offering: "monthly".into(),
            offering_name: "Plan".into(),
            cadence: Cadence::Month,
            amount_cents: 2900,
            currency: "USD".into(),
            account_code: "200".into(),
            state: PurchaseState::Active,
            xero_contact_id: None,
            xero_invoice_id: None,
            invoice_url: None,
            xero_repeating_invoice_id: None,
            inserted_at: now,
            updated_at: now,
            activated_at: Some(now),
            renewal_cancelled_at: None,
            access_ends_at: None,
            stripe: Some(StripePurchase {
                firm_id: Uuid::now_v7(),
                account_id: "acct_synthetic".into(),
                checkout_session_id: Some("cs_test_synthetic".into()),
                customer_id: Some("cus_synthetic".into()),
                subscription_id: Some("sub_synthetic".into()),
                period_end: Some(now + TimeDelta::days(10)),
                cancel_at_period_end: false,
                renewal_terms: "Terms".into(),
                consented_at: now,
            }),
        }
    }

    fn subscription(now: DateTime<Utc>) -> Subscription {
        serde_json::from_value(serde_json::json!({"id":"sub_synthetic", "livemode":false, "customer":"cus_synthetic", "status":"active",
            "cancel_at_period_end":false,"items":{"data":[{"current_period_end":(now+TimeDelta::days(30)).timestamp(),
            "price":{"unit_amount":2900,"currency":"usd","recurring":{"interval":"month","interval_count":1}}}]}})).unwrap()
    }

    #[test]
    fn pending_legacy_and_expired_but_renewable_plans_block_a_second_checkout() {
        let now = Utc::now();
        let mut existing = purchase(now);
        existing.stripe.as_mut().unwrap().period_end = Some(now - TimeDelta::days(1));
        assert!(!existing.is_active(now));
        assert!(conflicts_with_checkout(&existing, now));
        existing.renewal_cancelled_at = Some(now);
        assert!(!conflicts_with_checkout(&existing, now));
        existing.stripe = None;
        existing.renewal_cancelled_at = None;
        existing.state = PurchaseState::Invoiced;
        assert!(conflicts_with_checkout(&existing, now));
        existing.cadence = Cadence::Once;
        assert!(!conflicts_with_checkout(&existing, now));
    }

    #[test]
    fn an_unpaid_unconsented_or_foreign_checkout_cannot_activate_a_purchase() {
        let purchase = purchase(Utc::now());
        let response = serde_json::json!({"id":"cs_test_synthetic", "livemode":false, "status":"complete", "payment_status":"paid",
            "client_reference_id":purchase.id.to_string(), "customer":"cus_synthetic", "subscription":"sub_synthetic", "url":null,
            "consent":{"terms_of_service":"accepted"}});
        let session = serde_json::from_value(response.clone()).unwrap();
        assert_eq!(
            confirmed_checkout(&purchase, &session).unwrap(),
            ("sub_synthetic", "cus_synthetic")
        );
        for (field, value) in [
            ("payment_status", serde_json::json!("unpaid")),
            ("status", serde_json::json!("open")),
            (
                "client_reference_id",
                serde_json::json!(Uuid::now_v7().to_string()),
            ),
            ("consent", serde_json::Value::Null),
            ("customer", serde_json::Value::Null),
            ("subscription", serde_json::Value::Null),
        ] {
            let mut invalid = response.clone();
            invalid[field] = value;
            assert!(
                confirmed_checkout(&purchase, &serde_json::from_value(invalid).unwrap()).is_err()
            );
        }
    }

    #[test]
    fn a_price_change_does_not_trap_a_purchaser_in_renewal_or_extend_paid_time() {
        let now = Utc::now();
        let purchase = purchase(now);
        let mut sub = subscription(now);
        sub.cancel_at_period_end = true;
        sub.items.data[0].price.unit_amount = Some(9900);
        let result = cancellation_observation(&purchase, &sub, now).unwrap();
        assert_eq!(result.paid_through, now + TimeDelta::days(10));
        sub.customer = "cus_other".into();
        assert!(cancellation_observation(&purchase, &sub, now).is_err());
    }

    #[test]
    fn only_the_bound_paid_subscription_can_extend_access() {
        let now = Utc::now();
        let purchase = purchase(now);
        let mut subscription = subscription(now);
        let active = observation(&purchase, &subscription, now).unwrap();
        assert!(active.paid_through > now + TimeDelta::days(29));
        subscription.cancel_at_period_end = true;
        assert!(
            observation(&purchase, &subscription, now)
                .unwrap()
                .paid_through
                > now + TimeDelta::days(29)
        );
        subscription.cancel_at_period_end = false;
        subscription.status = "past_due".into();
        assert!(
            !observation(&purchase, &subscription, now)
                .unwrap()
                .cancelling
        );
        assert_eq!(
            observation(&purchase, &subscription, now)
                .unwrap()
                .paid_through,
            now + TimeDelta::days(10)
        );
        subscription.status = "canceled".into();
        assert!(
            observation(&purchase, &subscription, now)
                .unwrap()
                .cancelling
        );
        assert_eq!(
            observation(&purchase, &subscription, now)
                .unwrap()
                .paid_through,
            now
        );
        subscription.status = "active".into();
        subscription.customer = "cus_other".into();
        assert!(observation(&purchase, &subscription, now).is_err());
        subscription.customer = "cus_synthetic".into();
        subscription.items.data[0].price.unit_amount = Some(29_000);
        assert!(observation(&purchase, &subscription, now).is_err());
        subscription.items.data[0].price.unit_amount = Some(2900);
        subscription.items.data[0].price.recurring.interval = "year".into();
        assert!(observation(&purchase, &subscription, now).is_err());
    }
}
