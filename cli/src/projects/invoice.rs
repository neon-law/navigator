//! `navigator project invoice`: a matter's invoices from the CLI (LAW-506,
//! LAW-507). `create` composes a draft Stripe invoice the lawyer reviews;
//! nothing reaches the client until `send`. `record` and `payment` keep
//! money collected outside Stripe on the same ledger.
use std::fmt::Write;
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use chrono::NaiveDate;
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{legal_plans, remote};

#[derive(Subcommand)]
pub enum InvoiceCmd {
    /// Compose a draft Stripe invoice for review. It is not sent.
    Create(DraftArgs),
    /// List the matter's invoices.
    List,
    /// Finalize a reviewed draft and send it to the client.
    Send { id: Uuid },
    /// Void a sent invoice.
    Void {
        id: Uuid,
        #[arg(long)]
        reason: String,
    },
    /// Mark a sent invoice uncollectible.
    Uncollectible {
        id: Uuid,
        #[arg(long)]
        reason: String,
    },
    /// Record a payment received outside Stripe, in full or in part.
    Payment {
        id: Uuid,
        /// The amount, such as 250.00.
        #[arg(long)]
        amount: String,
        #[arg(long)]
        received_on: NaiveDate,
        #[arg(long, value_enum)]
        method: Method,
        /// The check number or wire reference.
        #[arg(long)]
        reference: Option<String>,
        #[arg(long)]
        reason: String,
    },
    /// Record an invoice collected outside Stripe.
    Record {
        #[arg(long, value_enum)]
        method: Method,
        /// The check number, wire reference, or receipt number.
        #[arg(long)]
        external_id: String,
        #[arg(long)]
        amount: String,
        #[arg(long)]
        issued_on: NaiveDate,
        /// Whether the money is already in.
        #[arg(long)]
        paid: bool,
        #[arg(long)]
        memo: Option<String>,
    },
}

#[derive(Args)]
pub struct DraftArgs {
    /// A line as `<description>=<price>`, or `<description>=<price>x<quantity>`. Repeat it.
    #[arg(long = "line", required = true)]
    lines: Vec<String>,
    /// Discount one line: `<line number>:<10% or 25.00>:<reason>`.
    #[arg(long)]
    line_discount: Vec<String>,
    /// Write a line off to $0, showing its value and the waiver: `<line number>:<reason>`.
    #[arg(long)]
    write_off: Vec<String>,
    /// Discount the invoice: `10%` or `25.00`. Needs --discount-reason.
    #[arg(long, requires = "discount_reason")]
    discount: Option<String>,
    #[arg(long, requires = "discount")]
    discount_reason: Option<String>,
    /// A Stripe promotion code, such as a marketing-agreement discount.
    #[arg(long)]
    coupon: Option<String>,
    /// Credit a payment already received and accounted for. Needs --credit-reason.
    #[arg(long, requires = "credit_reason")]
    credit: Option<String>,
    #[arg(long, requires = "credit")]
    credit_reason: Option<String>,
    #[arg(long, value_enum, default_value_t = Terms::OnReceipt, conflicts_with = "due_on")]
    terms: Terms,
    /// A due date (YYYY-MM-DD) in place of --terms.
    #[arg(long)]
    due_on: Option<NaiveDate>,
    /// The memo the client sees.
    #[arg(long)]
    memo: Option<String>,
    /// A retainer or deposit: unearned money, which always collects into trust.
    #[arg(long)]
    advance: bool,
    /// Where the payment lands. Operating needs the responsible lawyer and --reason.
    #[arg(long, value_enum, default_value_t = Account::Trust)]
    account: Account,
    /// Why this invoice collects into Operating; kept with the approval.
    #[arg(long)]
    reason: Option<String>,
    /// A stable id for this request. Repeating it returns the first draft.
    #[arg(long)]
    request_id: Option<Uuid>,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Terms {
    OnReceipt,
    #[value(name = "net-15")]
    Net15,
    #[value(name = "net-30")]
    Net30,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Account {
    Trust,
    Operating,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Method {
    InPerson,
    Check,
    Wire,
    Other,
}

impl Method {
    const fn as_str(self) -> &'static str {
        match self {
            Self::InPerson => "in_person",
            Self::Check => "check",
            Self::Wire => "wire",
            Self::Other => "other",
        }
    }
}

/// Dollars as typed (`1,500`, `$25.50`) in cents.
fn cents(text: &str) -> Result<i64> {
    let cleaned: String = text
        .trim()
        .trim_start_matches('$')
        .chars()
        .filter(|c| *c != ',')
        .collect();
    let (whole, fraction) = cleaned.split_once('.').unwrap_or((&cleaned, ""));
    let valid = !whole.is_empty()
        && whole.bytes().all(|b| b.is_ascii_digit())
        && fraction.len() <= 2
        && fraction.bytes().all(|b| b.is_ascii_digit());
    if !valid {
        return Err(anyhow!("`{text}` is not an amount like 25.00"));
    }
    let whole: i64 = whole.parse().context("amount too large")?;
    let fraction: i64 = format!("{fraction:0<2}").parse().unwrap_or(0);
    whole
        .checked_mul(100)
        .and_then(|value| value.checked_add(fraction))
        .ok_or_else(|| anyhow!("amount too large"))
}

/// `10%` (basis points) or a dollar amount.
fn discount(text: &str, reason: &str) -> Result<Value> {
    Ok(match text.trim().strip_suffix('%') {
        Some(percent) => {
            let bps = cents(percent).context("a percent like 10% or 12.5%")?;
            json!({ "percent_bps": bps, "reason": reason })
        }
        None => json!({ "amount_cents": cents(text)?, "reason": reason }),
    })
}

fn line(text: &str) -> Result<Value> {
    let (description, price) = text
        .rsplit_once('=')
        .ok_or_else(|| anyhow!("a line is `<description>=<price>`, got `{text}`"))?;
    let (price, quantity) = match price.split_once('x') {
        Some((price, quantity)) => (price, quantity.trim().parse::<u32>().context("quantity")?),
        None => (price, 1),
    };
    Ok(json!({
        "description": description.trim(), "quantity": quantity, "unit_amount_cents": cents(price)?,
    }))
}

/// The 0-based index of the 1-based line a `--line-discount` or
/// `--write-off` names, and the rest of the flag.
fn numbered(text: &str, count: usize) -> Result<(usize, &str)> {
    let (number, rest) = text
        .split_once(':')
        .ok_or_else(|| anyhow!("name the line number first, like `2:...`"))?;
    let number: usize = number.trim().parse().context("a line number")?;
    if number == 0 || number > count {
        return Err(anyhow!(
            "there is no line {number}; the invoice has {count}"
        ));
    }
    Ok((number - 1, rest))
}

impl DraftArgs {
    /// The draft request body: lines with their discounts and write-offs,
    /// terms, and the destination.
    fn draft(&self) -> Result<Value> {
        let Self {
            lines,
            line_discount,
            write_off,
            discount: whole,
            discount_reason,
            coupon,
            credit,
            credit_reason,
            terms,
            due_on,
            memo,
            advance,
            account,
            reason,
            request_id,
        } = self;
        let mut items = lines
            .iter()
            .map(|text| line(text))
            .collect::<Result<Vec<_>>>()?;
        for text in line_discount {
            let (index, rest) = numbered(text, items.len())?;
            let (amount, why) = rest
                .split_once(':')
                .ok_or_else(|| anyhow!("a line discount is `<line>:<10% or 25.00>:<reason>`"))?;
            items[index]["discount"] = discount(amount, why)?;
        }
        for text in write_off {
            let (index, why) = numbered(text, items.len())?;
            items[index]["discount"] = json!({ "percent_bps": 10_000, "reason": why });
        }
        let operating = matches!(account, Account::Operating);
        if operating != reason.is_some() {
            return Err(anyhow!("--account operating and --reason go together"));
        }
        Ok(json!({
                "request_id": request_id.unwrap_or_else(Uuid::now_v7),
                "lines": items,
                "terms": match terms { Terms::OnReceipt => "on_receipt", Terms::Net15 => "net_15", Terms::Net30 => "net_30" },
                "due_on": due_on,
                "memo": memo,
                "discount": whole.as_deref().zip(discount_reason.as_deref()).map(|(amount, why)| discount(amount, why)).transpose()?,
                "coupon": coupon,
                "credit": credit.as_deref().zip(credit_reason.as_deref()).map(|(amount, why)| {
                    Ok::<_, anyhow::Error>(json!({ "amount_cents": cents(amount)?, "reason": why }))
                }).transpose()?,
                "advance": advance,
                "account": if operating { "operating" } else { "trust" },
                "operating_reason": reason,
        }))
    }
}

impl InvoiceCmd {
    fn request(&self) -> Result<(reqwest::Method, String, Option<Value>)> {
        use reqwest::Method as Http;
        Ok(match self {
            Self::Create(draft) => (Http::POST, "invoices/drafts".into(), Some(draft.draft()?)),
            Self::List => (Http::GET, "invoices".into(), None),
            Self::Send { id } => (Http::POST, format!("invoices/{id}/send"), None),
            Self::Void { id, reason } => (
                Http::POST,
                format!("invoices/{id}/void"),
                Some(json!({ "reason": reason })),
            ),
            Self::Uncollectible { id, reason } => (
                Http::POST,
                format!("invoices/{id}/uncollectible"),
                Some(json!({ "reason": reason })),
            ),
            Self::Payment {
                id,
                amount,
                received_on,
                method,
                reference,
                reason,
            } => (
                Http::POST,
                format!("invoices/{id}/payments"),
                Some(json!({
                    "amount_cents": cents(amount)?, "received_on": received_on,
                    "source": method.as_str(), "external_id": reference, "reason": reason,
                })),
            ),
            Self::Record {
                method,
                external_id,
                amount,
                issued_on,
                paid,
                memo,
            } => (
                Http::POST,
                "invoices/records".into(),
                Some(json!({
                    "source": method.as_str(), "external_id": external_id,
                    "amount_cents": cents(amount)?, "issued_on": issued_on,
                    "status": if *paid { "paid" } else { "sent" }, "memo": memo,
                    "client_visible": true,
                })),
            ),
        })
    }
}

/// Run one `navigator project invoice` verb against the Project's site.
pub async fn run(
    host: Option<&str>,
    project: Option<&str>,
    json: bool,
    action: InvoiceCmd,
) -> ExitCode {
    let result = async {
        let (route, token, _) = legal_plans::project_route(host, project).await?;
        let value = send(&route, &token, &action).await?;
        Ok(if json {
            serde_json::to_string_pretty(&value)?
        } else {
            render(&value)?
        })
    }
    .await;
    crate::billing::finish("project invoice", result)
}

async fn send(route: &str, token: &str, action: &InvoiceCmd) -> Result<Value> {
    let (method, path, body) = action.request()?;
    let client = remote::authenticated_client_builder(route)?.build()?;
    let mut request = client
        .request(method, format!("{route}/{path}"))
        .bearer_auth(token)
        .timeout(std::time::Duration::from_secs(90));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.context("send the invoice request")?;
    let status = response.status();
    let text = response.text().await.context("read the invoice response")?;
    if !status.is_success() {
        let reason = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|body| {
                body["error"]
                    .as_str()
                    .filter(|error| *error != "invoice_generation_refused")
                    .or_else(|| body["reason"].as_str())
                    .map(str::to_owned)
            })
            .unwrap_or(text);
        return Err(anyhow!("refused ({status}): {reason}"));
    }
    serde_json::from_str(&text).context("parse the invoice response")
}

fn dollars(cents: &Value) -> String {
    let cents = cents.as_i64().unwrap_or_default();
    let sign = if cents < 0 { "-" } else { "" };
    format!("{sign}${}.{:02}", cents.abs() / 100, cents.abs() % 100)
}

fn invoice_line(text: &mut String, invoice: &Value) -> std::fmt::Result {
    let paid = invoice["amount_paid_cents"].as_i64().unwrap_or_default();
    let amount = invoice["amount_cents"].as_i64().unwrap_or_default();
    writeln!(
        text,
        "{} · {} · {} · {} · balance {} · {} {}",
        invoice["id"].as_str().unwrap_or_default(),
        invoice["status"].as_str().unwrap_or_default(),
        invoice["source"].as_str().unwrap_or_default(),
        dollars(&invoice["amount_cents"]),
        dollars(&json!((amount - paid).max(0))),
        invoice["destination"].as_str().unwrap_or("recorded"),
        invoice["external_id"].as_str().unwrap_or_default(),
    )
}

fn render(value: &Value) -> Result<String> {
    let mut text = String::new();
    if let Some(rows) = value.as_array() {
        if rows.is_empty() {
            text.push_str("No invoices\n");
        }
        for row in rows {
            invoice_line(&mut text, row)?;
        }
        return Ok(text);
    }
    let invoice = value.get("invoice").unwrap_or(value);
    invoice_line(&mut text, invoice)?;
    for line in value["line_items"].as_array().into_iter().flatten() {
        writeln!(
            text,
            "  {:>12}  {}",
            dollars(&line["unit_amount_cents"]),
            line["description"].as_str().unwrap_or_default()
        )?;
    }
    if let Some(coupon) = value["coupon"].as_str() {
        writeln!(text, "  coupon {coupon}, applied by Stripe")?;
    }
    if invoice["status"] == "draft" {
        text.push_str("Draft only: review it, then `navigator project invoice send <id>`.\n");
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Invoice {
        #[command(subcommand)]
        action: InvoiceCmd,
    }

    fn body(args: &[&str]) -> Result<Value> {
        let action =
            Invoice::try_parse_from(std::iter::once("invoice").chain(args.iter().copied()))
                .unwrap()
                .action;
        Ok(action.request()?.2.unwrap())
    }

    #[test]
    fn amounts_parse_as_typed() {
        assert_eq!(cents("1,500").unwrap(), 150_000);
        assert_eq!(cents("$25.5").unwrap(), 2550);
        assert_eq!(cents("0.07").unwrap(), 7);
        assert!(cents("12.345").is_err());
        assert!(cents("-5").is_err());
    }

    #[test]
    fn create_carries_lines_discounts_write_offs_and_credits() {
        let sent = body(&[
            "create",
            "--line",
            "Retainer=1,500",
            "--line",
            "Prep=50x2",
            "--line",
            "Call=200",
            "--line-discount",
            "2:10%:Returning client",
            "--write-off",
            "3:Courtesy",
            "--discount",
            "25",
            "--discount-reason",
            "Referral",
            "--credit",
            "100",
            "--credit-reason",
            "Deposit",
            "--terms",
            "net-30",
            "--coupon",
            "SPRING",
        ])
        .unwrap();
        assert_eq!(
            sent["lines"][0],
            json!({"description": "Retainer", "quantity": 1, "unit_amount_cents": 150_000})
        );
        assert_eq!(sent["lines"][1]["quantity"], 2);
        assert_eq!(
            sent["lines"][1]["discount"],
            json!({"percent_bps": 1000, "reason": "Returning client"})
        );
        assert_eq!(
            sent["lines"][2]["discount"],
            json!({"percent_bps": 10_000, "reason": "Courtesy"})
        );
        assert_eq!(
            sent["discount"],
            json!({"amount_cents": 2500, "reason": "Referral"})
        );
        assert_eq!(
            sent["credit"],
            json!({"amount_cents": 10_000, "reason": "Deposit"})
        );
        assert_eq!(
            (sent["terms"].as_str(), sent["account"].as_str()),
            (Some("net_30"), Some("trust"))
        );
        assert!(body(&["create", "--line", "A=1", "--write-off", "2:x"]).is_err());
        assert!(body(&["create", "--line", "A=1", "--account", "operating"]).is_err());
        let operating = body(&[
            "create",
            "--line",
            "A=1",
            "--account",
            "operating",
            "--reason",
            "Earned",
        ])
        .unwrap();
        assert_eq!(operating["operating_reason"], "Earned");
    }
}
