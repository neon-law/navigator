//! The nightly trust mirror, run under the `ReconcileInvoices` workflow id.
//! It reads the Firm's pooled IOLTA accounts and their deposits, refunds,
//! and withdrawals from the books and posts them onto each matter's trust
//! ledger. Invoices are not mirrored: Stripe issues them, and the Matter
//! Invoice ledger (`store::matter_invoices`) records them. A withdrawal line
//! settles a ledger invoice.
//!
//! The `billing-reconcile-trigger` `CronJob` starts one invocation per day
//! (keyed on the UTC date, so a same-day re-fire is a no-op); Restate owns
//! the retry schedule. Identical split to the canary/archives triggers.
//!
//! [`reconcile_once`] is provider-agnostic so it unit-tests against the
//! [`billing::StubBillingProvider`] + a test database without a worker.

use std::fmt::Write as _;

use billing::BillingProvider;

use serde::{Deserialize, Serialize};
use store::application_store::ApplicationStore;
use workflows::SlackBot;

/// Request body for `ReconcileInvoices::run`. Empty — the trigger only
/// starts the workflow — but kept as a struct so fields can be threaded
/// later without changing the handler signature.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct ReconcileRequest {}

/// What a reconcile run touched, surfaced as the invocation output.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ReconcileReport {
    /// Xero bank accounts whose name declared a state (`IOLTA NV …`) and
    /// whose pooled balance was mirrored onto `iolta_account`.
    pub iolta_accounts: usize,
    /// Listed bank accounts that declared no state, or a state this
    /// deployment does not carry. No row is written — the firm's operating
    /// and payroll accounts land here, and so does a typo.
    pub iolta_unscoped: usize,
    /// Client deposits into trust posted onto a matter's ledger this run. A
    /// deposit already mirrored on an earlier night is not counted again.
    pub trust_deposits: usize,
    /// Refunds of unearned funds posted onto a matter's ledger this run.
    pub trust_refunds: usize,
    /// Trust transactions this run did not post: no `Matter` reference, a
    /// matter that does not sit on the account the money moved through, or a
    /// matter with no notation to anchor a posting to.
    pub trust_unscoped: usize,
    /// Pooled withdrawals mirrored this run: one bank transfer out of a
    /// state's IOLTA account, split across the invoices it settles, with one
    /// earned draw posted per matter.
    pub iolta_withdrawals: usize,
    /// Pooled withdrawals refused whole and left for a human — lines that do
    /// not sum to the transfer, a line naming an unmirrored invoice, a
    /// matter on another state's pool, or a matter drawn beyond what it
    /// holds. Nothing of a refused withdrawal is written.
    pub iolta_withdrawals_refused: usize,
    /// What actually moved in trust tonight, per matter. Firm-internal:
    /// Project codes and cents only, never a client's name, email, or
    /// portal URL (`#finance` copy rules, ENG-790).
    #[serde(default)]
    pub trust_lines: Vec<TrustLine>,
}

/// One trust movement this run posted, as `#finance` reads it out.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct TrustLine {
    /// The matter's Project code — the firm-internal name for it. Never the
    /// client's own name.
    pub project_code: String,
    /// `deposit`, `refund`, or `draw`.
    pub kind: String,
    pub cents: i64,
}

/// Mirror the pooled trust accounts and their movements. Provider-agnostic;
/// unit-tested against the stub.
///
/// # Errors
///
/// Propagates any database or billing-provider error.
pub async fn reconcile_once(
    provider: &dyn BillingProvider,
    application_store: &ApplicationStore,
) -> anyhow::Result<ReconcileReport> {
    let (iolta_accounts, iolta_unscoped) =
        mirror_iolta_accounts(provider, application_store).await?;
    let trust = mirror_trust_movements(provider, application_store).await?;
    Ok(ReconcileReport {
        iolta_accounts,
        iolta_unscoped,
        trust_deposits: trust.deposits,
        trust_refunds: trust.refunds,
        trust_unscoped: trust.unscoped,
        iolta_withdrawals: trust.withdrawals,
        iolta_withdrawals_refused: trust.withdrawals_refused,
        trust_lines: trust.lines,
    })
}

/// What one pass over the trust transactions posted.
#[derive(Debug, Default)]
struct TrustMirrorTally {
    deposits: usize,
    refunds: usize,
    unscoped: usize,
    withdrawals: usize,
    withdrawals_refused: usize,
    lines: Vec<TrustLine>,
}

/// What one pooled withdrawal did.
enum WithdrawalOutcome {
    /// Carries what each matter drew, so the nightly notice can name it.
    Applied { drawn: Vec<(uuid::Uuid, i64)> },
    /// Mirrored on an earlier night; nothing written.
    AlreadyApplied,
    /// Refused whole, with the reason a human needs to reconcile it in Xero.
    Refused(String),
}

/// Mirror one pooled withdrawal and its allocations.
///
/// A refusal is a *reported* outcome rather than an error that abandons the
/// night: one bookkeeping mistake in one transfer must not stop the rest of
/// the run. Only a database or ledger failure propagates.
async fn apply_withdrawal(
    application_store: &ApplicationStore,
    transaction: &billing::TrustBankTransaction,
) -> anyhow::Result<WithdrawalOutcome> {
    let input = store::iolta_withdrawals::WithdrawalInput {
        xero_transaction_id: transaction.transaction_id.clone(),
        xero_account_id: transaction.account_id.clone(),
        total_cents: transaction.amount_cents,
        currency: transaction.currency.clone(),
        occurred_at: transaction.occurred_at,
        lines: transaction
            .line_items
            .iter()
            .filter_map(|line| {
                line.invoice_reference.as_ref().map(|reference| {
                    store::iolta_withdrawals::AllocationInput {
                        invoice_reference: reference.clone(),
                        amount_cents: line.amount_cents,
                    }
                })
            })
            .collect(),
    };
    match store::iolta_withdrawals::apply(application_store, &input).await {
        Ok(store::iolta_withdrawals::Applied::Posted { projects }) => {
            let mut drawn = Vec::with_capacity(projects.len());
            for project_id in projects {
                let cents = store::iolta_withdrawals::for_project(application_store, project_id)
                    .await?
                    .into_iter()
                    .filter(|line| line.withdrawal_id == transaction.transaction_id)
                    .map(|line| line.amount_cents)
                    .sum();
                drawn.push((project_id, cents));
            }
            Ok(WithdrawalOutcome::Applied { drawn })
        }
        Ok(store::iolta_withdrawals::Applied::AlreadyApplied) => {
            Ok(WithdrawalOutcome::AlreadyApplied)
        }
        Err(refusal) => Ok(WithdrawalOutcome::Refused(refusal.to_string())),
    }
}

/// Whether a spend settles invoices — the pooled withdrawal — rather than
/// refunding a client. The lines say which: an allocation line names the
/// invoice it settles, a refund's lines name none.
fn settles_invoices(transaction: &billing::TrustBankTransaction) -> bool {
    transaction
        .line_items
        .iter()
        .any(|line| line.invoice_reference.is_some())
}

/// Mirror client deposits into trust, and refunds out of it, onto each
/// matter's `store::trust` ledger.
///
/// Every posting is refused unless it holds together three ways: the
/// transaction's `Matter <code>` reference resolves to a live Project, that
/// Project's governing jurisdiction has a mirrored pooled account, and the
/// money actually moved through *that* account. A Nevada deposit landing
/// against a California-governed matter is a bookkeeping error, so it is
/// counted and left for a human rather than posted somewhere plausible.
///
/// Idempotent on the Xero transaction id: the same night's read, re-run,
/// posts nothing.
async fn mirror_trust_movements(
    provider: &dyn BillingProvider,
    application_store: &ApplicationStore,
) -> anyhow::Result<TrustMirrorTally> {
    let listed = provider.list_trust_transactions().await?;
    let mut tally = TrustMirrorTally::default();
    for transaction in listed {
        if matches!(transaction.kind, billing::TrustTransactionKind::Spend)
            && settles_invoices(&transaction)
        {
            match apply_withdrawal(application_store, &transaction).await? {
                WithdrawalOutcome::Applied { drawn } => {
                    tally.withdrawals += 1;
                    for (project_id, cents) in drawn {
                        tally
                            .lines
                            .push(trust_line(application_store, project_id, "draw", cents).await?);
                    }
                }
                WithdrawalOutcome::Refused(reason) => {
                    tracing::warn!(
                        transaction_id = %transaction.transaction_id,
                        reason = %reason,
                        "IOLTA withdrawal refused; nothing written"
                    );
                    tally.withdrawals_refused += 1;
                }
                WithdrawalOutcome::AlreadyApplied => {}
            }
            continue;
        }
        let Some(project_id) = store::matter_invoices::resolve_project_scope(
            application_store,
            &transaction.reference,
        )
        .await?
        else {
            tally.unscoped += 1;
            continue;
        };
        let Some(account) =
            store::iolta_accounts::for_project(application_store, project_id).await?
        else {
            tally.unscoped += 1;
            continue;
        };
        if account.xero_account_id != transaction.account_id {
            tally.unscoped += 1;
            continue;
        }

        let occurred_at = transaction.occurred_at.to_rfc3339();
        let movement = match transaction.kind {
            billing::TrustTransactionKind::Receive => store::trust::Movement::deposit(
                project_id,
                transaction.currency.clone(),
                format_cents(transaction.amount_cents),
                transaction.amount_cents,
                occurred_at,
            ),
            billing::TrustTransactionKind::Spend => {
                store::trust::Movement::refund(project_id, transaction.amount_cents, occurred_at)
            }
        }
        .with_external_ref(transaction.transaction_id.clone());

        match store::trust::record_project_movement(application_store, project_id, &movement)
            .await
            .map_err(anyhow::Error::msg)?
        {
            store::trust::Recorded::Posted => {
                let kind = match transaction.kind {
                    billing::TrustTransactionKind::Receive => {
                        tally.deposits += 1;
                        "deposit"
                    }
                    billing::TrustTransactionKind::Spend => {
                        tally.refunds += 1;
                        "refund"
                    }
                };
                tally.lines.push(
                    trust_line(
                        application_store,
                        project_id,
                        kind,
                        transaction.amount_cents,
                    )
                    .await?,
                );
            }
            // Already on the ledger from an earlier night, or the matter has
            // no notation to anchor a posting to. Neither is a new fact.
            store::trust::Recorded::AlreadyRecorded => {}
            store::trust::Recorded::NoAnchor => tally.unscoped += 1,
        }
    }
    Ok(tally)
}

/// Name one posted movement for the nightly `#finance` notice.
///
/// The matter is named by its **Project code**, the firm's own handle for it
/// — never by the client's name, which `#finance` has no reason to carry. A
/// matter whose row has gone reads as `unknown`, so a report line is never
/// dropped silently.
async fn trust_line(
    application_store: &ApplicationStore,
    project_id: uuid::Uuid,
    kind: &str,
    cents: i64,
) -> anyhow::Result<TrustLine> {
    let project_code = store::projects::find_by_id(application_store, project_id)
        .await?
        .map_or_else(|| "unknown".to_string(), |project| project.code);
    Ok(TrustLine {
        project_code,
        kind: kind.to_string(),
        cents,
    })
}

/// Render minor units as the decimal string `store::trust::Movement` keeps
/// its native amount in — never a float in the money path.
fn format_cents(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.unsigned_abs();
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

/// Refresh the mirrored balance of each state's pooled IOLTA account.
///
/// Read-only in both directions: Xero lists its bank accounts, Navigator
/// keeps the one row per state that `store::iolta_accounts` allows, and
/// nothing is ever written back to Xero. An account whose name does not
/// declare a state (`IOLTA NV — Trust`) is counted as unscoped rather than
/// attached to one — the firm's operating account is in the same list.
///
/// A refused write for one account does not abandon the rest: a second Xero
/// account claiming a state that is already mirrored is reported as unscoped
/// and the run continues, because one bookkeeping mistake should not stop
/// every other state's balance from refreshing.
async fn mirror_iolta_accounts(
    provider: &dyn BillingProvider,
    application_store: &ApplicationStore,
) -> anyhow::Result<(usize, usize)> {
    let listed = provider.list_bank_accounts().await?;
    let mirrored_at = chrono::Utc::now();
    let mut mirrored = 0;
    let mut unscoped = 0;
    for account in listed {
        let Some(jurisdiction_id) =
            store::iolta_accounts::resolve_jurisdiction_scope(application_store, &account.name)
                .await?
        else {
            unscoped += 1;
            continue;
        };
        let upsert = store::iolta_accounts::UpsertIoltaAccount {
            jurisdiction_id,
            xero_account_id: account.account_id.clone(),
            xero_account_code: account.code.clone(),
            name: account.name.clone(),
            currency: account.currency.clone(),
            balance_cents: account.balance_cents,
            mirrored_at,
        };
        match store::iolta_accounts::upsert(application_store, &upsert).await {
            Ok(_) => mirrored += 1,
            Err(store::iolta_accounts::IoltaAccountError::JurisdictionTaken { .. }) => {
                unscoped += 1;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok((mirrored, unscoped))
}

/// Post one night's report to `#finance`, and nowhere else.
///
/// Split out of the handler so the destination is testable without a Restate
/// context: the channel a firm-finance notice lands in is exactly the sort of
/// thing that should fail a test rather than a reader's trust.
///
/// # Errors
///
/// Propagates the Slack client's own error.
pub async fn post_finance_notice(
    slack: &dyn SlackBot,
    finance_channel: &str,
    report: &ReconcileReport,
    simulated: bool,
) -> Result<(), workflows::SlackBotError> {
    slack
        .post_message(finance_channel, &finance_message(report, simulated))
        .await
}

/// What the configured Slack App posts to `#finance` after a nightly run.
///
/// Firm-internal by construction: it names **Project codes** and **cents**,
/// which the owner allowed for this channel, and nothing else about a
/// client. No client name, no email address, no portal link. Unscoped trust
/// movements are a **count**, never the names behind them.
///
/// Staging disclosure is the Slack adapter's last-line `from Staging` mark,
/// not a rewrite of this body. `simulated` is accepted so callers share one
/// flag with the rest of the reconcile path.
#[must_use]
pub fn finance_message(report: &ReconcileReport, _simulated: bool) -> String {
    let mut body = String::from("Trust mirror, nightly run.");
    let _ = write!(
        body,
        "\nTrust: {} deposit(s), {} refund(s), {} withdrawal(s) across {} pooled account(s).",
        report.trust_deposits,
        report.trust_refunds,
        report.iolta_withdrawals,
        report.iolta_accounts
    );
    if report.trust_unscoped > 0
        || report.iolta_withdrawals_refused > 0
        || report.iolta_unscoped > 0
    {
        let _ = write!(
            body,
            "\nNeeds a look: {} trust movement(s) unscoped, {} withdrawal(s) refused, \
             {} bank account(s) unscoped.",
            report.trust_unscoped, report.iolta_withdrawals_refused, report.iolta_unscoped
        );
    }
    for line in &report.trust_lines {
        let _ = write!(
            body,
            "\n  {} {} {}",
            line.project_code,
            line.kind,
            format_usd(line.cents)
        );
    }
    body
}

/// Render minor units as a dollar amount for the `#finance` post. Money
/// never passes through a float.
fn format_usd(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.unsigned_abs();
    let digits = (abs / 100).to_string();
    let mut grouped = String::new();
    for (index, ch) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let grouped: String = grouped.chars().rev().collect();
    format!("{sign}${grouped}.{:02}", abs % 100)
}

#[cfg(test)]
mod tests {

    /// The report a nightly run hands `finance_message`, with only the
    /// fields a given assertion cares about set.
    fn report() -> super::ReconcileReport {
        super::ReconcileReport {
            iolta_accounts: 2,
            iolta_unscoped: 0,
            trust_deposits: 1,
            trust_refunds: 0,
            trust_unscoped: 0,
            iolta_withdrawals: 1,
            iolta_withdrawals_refused: 0,
            trust_lines: vec![
                super::TrustLine {
                    project_code: "acme-formation".into(),
                    kind: "deposit".into(),
                    cents: 500_000,
                },
                super::TrustLine {
                    project_code: "libra-estate".into(),
                    kind: "draw".into(),
                    cents: 123_456,
                },
            ],
        }
    }

    /// The owner allowed Project codes and cents in `#finance`; everything
    /// that identifies a *client* stays out.
    #[test]
    fn the_finance_notice_names_codes_and_cents_and_nothing_client_facing() {
        let body = super::finance_message(&report(), false);

        assert!(!body.contains("Invoices"), "invoices are not mirrored");
        assert!(body.contains("1 deposit(s), 0 refund(s), 1 withdrawal(s)"),);
        assert!(body.contains("acme-formation deposit $5,000.00"));
        assert!(body.contains("libra-estate draw $1,234.56"));

        for forbidden in ["@", "linear.app", "http", "/app/projects"] {
            assert!(
                !body.contains(forbidden),
                "the #finance notice must not carry {forbidden:?}"
            );
        }
    }

    /// Unscoped and refused trust work is a count, never the names behind it.
    #[test]
    fn unscoped_and_refused_work_is_reported_as_counts() {
        let mut report = report();
        report.trust_unscoped = 2;
        report.iolta_withdrawals_refused = 1;
        report.iolta_unscoped = 3;

        let body = super::finance_message(&report, false);
        assert!(body.contains(
            "2 trust movement(s) unscoped, 1 withdrawal(s) refused, 3 bank account(s) unscoped"
        ),);
    }

    /// A quiet night still says so, without a "needs a look" line.
    #[test]
    fn a_quiet_night_reports_zeroes_and_flags_nothing() {
        let body = super::finance_message(&super::ReconcileReport::default(), false);
        assert!(body.contains("0 deposit(s)"));
        assert!(!body.contains("Needs a look"));
    }

    /// The notice lands in `#finance` and nowhere else — one post, to the
    /// finance channel id, never to `#general` or a Project's own channel.
    #[tokio::test]
    async fn the_notice_lands_in_the_finance_channel_and_nowhere_else() {
        let bot = workflows::CapturingSlackBot::new();
        super::post_finance_notice(&bot, "C012FINANCE", &report(), false)
            .await
            .unwrap();

        let posted = bot.posted_messages();
        assert_eq!(posted.len(), 1, "one post per run: {posted:?}");
        assert_eq!(posted[0].0, "C012FINANCE");
        assert!(
            posted[0].1.contains("Trust mirror, nightly run."),
            "{posted:?}"
        );
        assert!(
            bot.created_channels().is_empty(),
            "the notice creates no channel"
        );
    }

    #[test]
    fn finance_copy_does_not_rewrite_for_simulated_matters() {
        let simulated = super::finance_message(&report(), true);
        let production = super::finance_message(&report(), false);
        assert_eq!(simulated, production);
        assert!(
            !production.contains("staging"),
            "the body must not carry a staging parenthetical; the adapter marks staging"
        );
    }
}
