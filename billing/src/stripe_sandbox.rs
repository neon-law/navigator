//! Two-account provider contract. A Stripe test clock owns each run’s billing objects.
use super::*;
use crate::InvoiceLine;
use anyhow::{anyhow, ensure, Context, Result};
use serde_json::Value;

const NEVADA: &str = "STRIPE_SANDBOX_NEVADA_SECRET_KEY";
const OPERATING: &str = "STRIPE_SANDBOX_OPERATING_SECRET_KEY";

fn sandbox_keys(
    nevada: Option<String>,
    operating: Option<String>,
) -> Result<Option<(String, String)>> {
    for key in [&nevada, &operating].into_iter().flatten() {
        ensure!(
            key.strip_prefix("sk_test_")
                .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric())),
            "sandbox invoice tests require sk_test_ secret keys"
        );
    }
    Ok(nevada.zip(operating))
}

#[test]
fn every_supplied_key_is_checked_even_when_the_other_is_missing() {
    for invalid in [
        "sk_live_example",
        "rk_test_example",
        "pk_test_example",
        "sk_test_",
        "",
    ] {
        assert!(sandbox_keys(Some(invalid.into()), None).is_err());
        assert!(sandbox_keys(Some("sk_test_example".into()), Some(invalid.into())).is_err());
    }
    assert!(sandbox_keys(None, Some("sk_test_example".into()))
        .unwrap()
        .is_none());
}

async fn post(
    client: &StripeInvoiceClient<'_>,
    path: &str,
    run: Uuid,
    step: &str,
    fields: &[(&str, &str)],
) -> Result<Value> {
    client
        .call(Method::POST, path, Some(&format!("{run}/{step}")), fields)
        .await
        .with_context(|| format!("Stripe sandbox {step}"))
}

fn id(object: &Value) -> Result<&str> {
    object["id"]
        .as_str()
        .context("Stripe object missing its ID")
}

async fn exercise(
    client: &StripeInvoiceClient<'_>,
    other: &StripeInvoiceClient<'_>,
    run: Uuid,
    clock: &str,
    coupon: Option<&str>,
) -> Result<()> {
    let tag = run.to_string();
    let customer = post(
        client,
        "customers",
        run,
        "customer",
        &[
            ("test_clock", clock),
            ("email", "billing@example.invalid"),
            ("metadata[navigator_batch_id]", &tag),
        ],
    )
    .await?;
    let customer = id(&customer)?;
    let due = chrono::Utc::now() + chrono::TimeDelta::days(15);
    let request = InvoiceRequest {
        contact_id: None,
        contact_name: "Sandbox customer".into(),
        contact_email: "billing@example.invalid".into(),
        reference: "Sandbox invoice".into(),
        issued_date: None,
        due_date: Some(due.date_naive()),
        line_items: vec![InvoiceLine {
            description: "Sandbox service".into(),
            quantity: 1,
            unit_amount_cents: 1000,
            account_code: "200".into(),
        }],
    };
    let invoice = client
        .create_draft_for_customer(run, &request, customer, &due.timestamp().to_string())
        .await?;
    let path = format!("invoices/{}", invoice.0);
    let draft: Value = client.call(Method::GET, &path, None, &[]).await?;
    ensure!(
        draft["status"] == "draft"
            && draft["metadata"]["navigator_batch_id"] == tag
            && draft["customer"] == customer,
        "draft ownership or status mismatch"
    );
    let wrong = other
        .client
        .get(other.base.join(&path)?)
        .bearer_auth(other.key)
        .send()
        .await?;
    ensure!(
        wrong.status() == reqwest::StatusCode::NOT_FOUND,
        "invoice must be absent from the other account"
    );
    let line = &draft["lines"]["data"][0];
    let item = line["parent"]["invoice_item_details"]["invoice_item"]
        .as_str()
        .context("invoice item missing")?;
    let mut fields = vec![("metadata[navigator_batch_id]", tag.as_str())];
    if let Some(coupon) = coupon {
        fields.push(("discounts[0][coupon]", coupon));
    } else {
        post(
            client,
            &format!("customers/{customer}/balance_transactions"),
            run,
            "credit",
            &[
                ("amount", "-300"),
                ("currency", "usd"),
                ("description", "Prior payment"),
                ("metadata[navigator_batch_id]", &tag),
            ],
        )
        .await?;
    }
    post(
        client,
        &format!("invoiceitems/{item}"),
        run,
        "update-line",
        &fields,
    )
    .await?;
    let final_invoice = post(
        client,
        &format!("{path}/finalize"),
        run,
        "finalize",
        &[("auto_advance", "false")],
    )
    .await?;
    if coupon.is_some() {
        ensure!(
            final_invoice["lines"]["data"][0]["discount_amounts"][0]["amount"] == 1000,
            "coupon must waive the full line"
        );
        ensure!(
            final_invoice["total"] == 0
                && final_invoice["amount_due"] == 0
                && final_invoice["status"] == "paid",
            "zero-balance invoice must settle at finalization"
        );
    } else {
        ensure!(
            final_invoice["total"] == 1000
                && final_invoice["starting_balance"] == -300
                && final_invoice["amount_due"] == 700
                && final_invoice["status"] == "open",
            "prior payment must reduce the balance due"
        );
        let voided = post(client, &format!("{path}/void"), run, "void", &[]).await?;
        ensure!(voided["status"] == "void", "invoice must be void");
    }
    Ok(())
}

async fn remove(client: &StripeInvoiceClient<'_>, path: &str) -> Result<()> {
    let deleted: Value = client.call(Method::DELETE, path, None, &[]).await?;
    ensure!(
        deleted["deleted"] == true,
        "Stripe did not acknowledge sandbox cleanup"
    );
    Ok(())
}

async fn scenario(
    client: &StripeInvoiceClient<'_>,
    other: &StripeInvoiceClient<'_>,
    use_coupon: bool,
) -> Result<()> {
    let run = Uuid::now_v7();
    let clock = post(
        client,
        "test_helpers/test_clocks",
        run,
        "clock",
        &[
            ("name", &run.to_string()),
            ("frozen_time", &chrono::Utc::now().timestamp().to_string()),
        ],
    )
    .await?;
    let clock = id(&clock)?;
    let mut coupon = None;
    let result = async {
        if use_coupon {
            coupon = Some(
                post(
                    client,
                    "coupons",
                    run,
                    "coupon",
                    &[
                        ("duration", "once"),
                        ("percent_off", "100"),
                        ("metadata[navigator_batch_id]", &run.to_string()),
                    ],
                )
                .await?,
            );
        }
        exercise(
            client,
            other,
            run,
            clock,
            coupon.as_ref().map(id).transpose()?,
        )
        .await
    }
    .await;
    // Both cleanup attempts run even when a provider call or assertion failed.
    let clock_cleanup = remove(client, &format!("test_helpers/test_clocks/{clock}")).await;
    if let Some(coupon) = coupon {
        remove(client, &format!("coupons/{}", id(&coupon)?))
            .await
            .context("remove sandbox coupon")?;
    }
    clock_cleanup.context("remove sandbox clock and its customer and invoices")?;
    result
}

#[tokio::test]
async fn two_account_invoice_lifecycle() -> Result<()> {
    fn read(name: &str) -> Result<Option<String>> {
        std::env::var_os(name)
            .map(|s| {
                s.into_string()
                    .map_err(|_| anyhow!("invalid sandbox key encoding"))
            })
            .transpose()
    }
    let Some((nevada, operating)) = sandbox_keys(read(NEVADA)?, read(OPERATING)?)? else {
        eprintln!("skipped Stripe sandbox lifecycle: both account keys are required");
        return Ok(());
    };
    let nevada = StripeInvoiceClient::new(&nevada, false)?;
    let operating = StripeInvoiceClient::new(&operating, false)?;
    ensure!(
        nevada.account_id().await? != operating.account_id().await?,
        "configure two distinct sandbox accounts"
    );
    scenario(&nevada, &operating, false)
        .await
        .context("Nevada sandbox")?;
    scenario(&operating, &nevada, true)
        .await
        .context("operating sandbox")
}
