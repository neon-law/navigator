# Xero and Stripe billing — setup, invoice flow, and production cutover

**Stripe issues every invoice; Xero holds the Firm's books.** Lawyers agree a matter's price with the client. Navigator
drafts the matter's invoice in Stripe, from approved billing work or from the lines a lawyer writes, and the lawyer
reviews and sends it. Each invoice lands on the matter's [Matter Invoice](glossary/matter-invoice.md) ledger. Xero keeps
the accounting: the nightly mirror reads its IOLTA bank accounts and transactions, and the billing canary finds or
creates a Xero contact. **One custom connection per organisation** keeps test activity off the live ledger. The
client-trust side is [`trust-accounting.md`](trust-accounting.md).

Local sandbox values use a gitignored `.env`; production values live in that deployment's `secrets.enc.yaml` per
[`deployment-secrets.md`](deployment-secrets.md). The environment-variable convention is in
[`third-party-integrations.md`](third-party-integrations.md). This page is the Xero specifics.

The billing seam lives in [`billing/src/lib.rs`](../billing/src/lib.rs) (the `BillingProvider` trait + the
`XeroBillingProvider` and `StubBillingProvider` impls), client-credentials auth in
[`billing/src/xero_auth.rs`](../billing/src/xero_auth.rs), and the worker-side nightly reconcile in
[`billing-workflows`](../billing-workflows/). The crate is re-exported as `portal::billing` so the app and tests share
one trait. An unconfigured vendor falls back to the in-process stub, so a fresh checkout boots and self-tests without a
Xero account.

## Stripe draft routing

A Project's Jujutsu repository declares its governing jurisdiction as `project.jurisdiction: NV` in `navigator.yaml`.
Run `navigator project sync` to project that code to the Project before generating an invoice. An absent declaration
leaves the current Project jurisdiction in place.

Each payment destination belongs to one Firm and one state. Before enabling collection, verify the payout bank, client
ownership, and how the processor handles fees and chargebacks. Keep the bank verification in the private Firm record.
Configure `NAVIGATOR_STRIPE_TRUST_ROUTES` in that deployment with a JSON array. Each entry has the Firm UUID in `firm`,
the two-letter `state`, and the verified Stripe account ID in `account`. Account IDs must be distinct across
destinations. A Nevada route may list `fallback_states`: states whose matters collect into the Nevada IOLTA while they
have no trust account of their own, listed only once the Firm has confirmed that state's rules allow it. A matter with
no route is refused: a missing route never selects Operating. The matter's responsible lawyer may approve the Firm's
Operating account for one invoice of earned work with a reason (`--account operating --reason`), never for an advance;
the approval is retained as an [Invoice Destination Approval](glossary/invoice-destination-approval.md). A configured
trust destination with an unavailable credential blocks the draft.

Connected accounts use the Firm's platform credential (`STRIPE_OPERATING_APP_KEY`) with `STRIPE_DEFAULT` enabled. Every
customer, draft, and invoice item request targets the selected connected account. Trust drafts require an account
distinct from the platform. A dedicated Nevada credential uses `dedicated_nevada: true` in its route, `STRIPE_NEVADA`,
and the Firm's `STRIPE_NEVADA_APP_KEY`. Its authenticated account ID must match the registered destination. Staging
accepts sandbox credentials. Production accepts live credentials.

On GKE, the deployment-owned `navigator-stripe-routing` ConfigMap supplies these settings to the web issuer. Set
`operating` and `nevada` to `1` to enable their respective routes. Omit a switch to disable it. The `trust-routes` entry
contains the Firm and state destinations described above. Restart the web deployment after changing this ConfigMap.
Store each account's provider key through Firm integration secrets.

Navigator creates an unsent draft on the authenticated Stripe account and reserves the underlying work for review. The
draft retains the account and invoice identity so a retry cannot silently move to a different account. The billed
Person's Stripe Customer is saved per account and reused. The lawyer reviews the draft and runs `navigator project
invoice send <id>`; a zero-balance invoice is finalized as a record and never sent. `void` and `uncollectible` take a
reason, and `payment` records a full or partial payment received outside Stripe. Navigator does not calculate a card
surcharge.

```bash
navigator project invoice create --line "Retainer=1,500" --advance --terms net-15
navigator project invoice create --line "Consultation=500" --write-off 1:Courtesy --credit 100 --credit-reason "Deposit received"
navigator project invoice send <id>
navigator project invoice record --method check --external-id 1001 --amount 250 --issued-on 2026-11-27 --paid
```

Recurring Offerings use a separate hosted Checkout path with renewal consent and online cancellation. Its activation,
fee-handling prerequisites, and verification are in [privacy and recurring purchases](privacy-and-purchases.md).

## One connection per organisation (the contrast with DocuSign)

DocuSign promotes **one app** across environments — at Go-Live the integration key is *copied*, so demo and production
share a GUID. **Xero is the opposite.** A Xero *custom connection* is a machine-to-machine app bound to exactly **one
organisation**, so each environment needs its **own** connection:

| Environment | Connection | Organisation | Ledger weight | Cost |
| --- | --- | --- | --- | --- |
| dev / CI | sandbox custom connection | the free **demo company** | none — resets periodically | free |
| production | live custom connection | the firm's real **organisation** | real receivables | $5/mo USD |

Both use the same `XERO_*` variable names — the env *file* is the namespace (`.env` = sandbox, `.env.production` =
live), so no code branches on environment. The connected org is fixed per connection and sent as the `Xero-Tenant-Id`
header on every Accounting API call.

### Testing never touches the live ledger

The demo company is the one free, non-binding target: invoices and contacts created there carry no receivable weight and
are self-cleaning (the demo org resets periodically). That is why dev, CI, and the live grounding test all point at the
demo company — a leaked sandbox secret cannot raise a real invoice against a real client.

## Money collected outside Stripe

A check, a wire, or cash is recorded on the same ledger with `navigator project invoice record`, naming its method and
its check number, wire reference, or receipt number. Invoices Xero issued before it was retired are kept on the ledger
as history with source `xero`; a document pointer names its invoice by `stripe_invoice_id`, or by `source` and
`external_id`.

**A trust receipt is not an invoice.** A client's deposit into the firm's IOLTA account is a bank transaction on a
pooled trust account. What connects the two is the **withdrawal**: when the firm draws earned fees out of trust, its
allocation lines name the Matter Invoices it settles, and each matter's share posts against that matter's trust ledger.
See [`trust-accounting.md`](trust-accounting.md).

## A Client's self-serve Purchase is collected through Stripe

A signed-in Client may buy an [Offering](glossary/offering.md) at its published price from `/purchases`: a consultation,
or a Delete Your Data plan billed monthly or yearly. That [Purchase](glossary/purchase.md) belongs to no matter and
never reaches Xero; it lives in the PostgreSQL `purchase` table and collects into the Firm's Stripe Operating account
(`STRIPE_DEFAULT` and the Firm's Operating key). Whether Purchases stay outside trust-first collection is open in
LAW-362.

1. `portal::purchases::buy` reserves the Purchase on the Firm's Stripe account, then creates, finalizes, and sends a
   Stripe invoice due in 14 days. Every write carries the Purchase id as its `Idempotency-Key`; an interrupted attempt
   older than Stripe's 24-hour key window needs reconciliation instead of a second invoice. The Client pays on the
   hosted invoice page.
2. `portal::purchases::activate_if_paid` turns the Purchase active once Stripe reports the invoice `paid` in full.
3. A monthly or yearly plan uses Stripe Checkout and a Stripe subscription; see [privacy and recurring
   purchases](privacy-and-purchases.md).

The DBOS `PurchaseActivation` schedule runs step 2 every five minutes; the purchase page also runs it for the viewer's
open invoices. The page refuses to sell a consultation unless the deployment enables the Operating Stripe route.

The monthly Legal Sandbox reset bills each subscriber's closed-month overage the same way: one sent Stripe invoice on
the account and Customer of their Stripe subscription. A subscriber whose plan predates Stripe has no Stripe account to
bill and is reported as skipped.

A Purchase Xero invoiced before Stripe keeps its Xero invoice and repeating-invoice ids as read-only history. An unpaid
or unanswered Xero Purchase is expired; a plan that renews through a Xero repeating invoice is cancelled by the Firm in
Xero, not online.

## Where the price comes from: the matter, agreed per client

There is no catalog and no published price for matters. Every engagement is bespoke: lawyers agree the figure with the
client for that matter, and the invoice Navigator drafts is what was billed. The pooled IOLTA account a matter's client
funds sit in is derived from the matter's jurisdiction, as described in [`trust-accounting.md`](trust-accounting.md).

The live sandbox test exercises the Xero connection against the demo company:

```bash
set -a; source .env; set +a
NAVIGATOR_RUN_LIVE_SANDBOX=1 cargo test -p server --test xero_sandbox -- --nocapture
```

It reads the CI `XERO_SANDBOX_*` names first, each falling back to the canonical `XERO_*` name, so a local `.env` drives
it without separate sandbox vars.

## Nightly ingest and paid-status reconciliation

The nightly `ReconcileInvoices` workflow (worker-side, in [`billing-workflows`](../billing-workflows/)) refreshes the
IOLTA side: the pooled bank account mirrored for each state, the client deposits and refunds that moved through it, and
any withdrawal that settles invoices. Its report counts what it posted and what it refused — a deposit that does not sit
on its matter's pool, a withdrawal whose lines do not add up. This workflow only ever *reads* from Xero. Like every
workflow, it is hosted by `workflows-service` — no per-workflow worker pod.

### Slack reports the nightly run

After the mirror settles, the same run posts its report to Slack **`#finance`** as a second journaled step, so a retry
of the post never re-runs the reconcile. It uses the same Slack App installation and Web API bot as `#general`, not a
second credential or an incoming webhook. Invite that app to the channel and set `SLACK_FINANCE_CHANNEL_ID`; an unset
value fails the run as a **terminal error** rather than reconciling with nowhere to report it.

The app's visible display name is managed in the Slack workspace, separately from its installed identifiers and the
deployment credential. New installations should use **Navigator** as the display name. Verify the actual workspace
display name before enabling live reports; changing it is an external Slack administration step, not a Navigator code
change.

The copy is firm-internal: trust counts and one line per matter that moved, naming the **Project code** and the
**cents**. Never a client's name, email address, or portal link. A simulated-matters deployment folds in the staging
disclosure, the same signal `#general` gets.

## Production cutover

1. Create the **live** custom connection (separate from the demo one) authorised against the firm's real organisation.
2. Put its `XERO_CLIENT_ID` / `XERO_CLIENT_SECRET` / `XERO_TENANT_ID` in the production deployment's
   `secrets.enc.yaml` (applied to Secret Manager and projected into the `navigator-web-secrets` Secret), never in
   plaintext source.
3. Confirm the live org grants the same `accounting.contacts accounting.settings.read accounting.transactions.read`
   scopes.
4. Verify that `ReconcileInvoices` mirrors the live IOLTA bank accounts and reports to `#finance`.

## Related

- [`trust-accounting.md`](trust-accounting.md) — the client-trust (IOLTA) side of this integration.
- [`third-party-integrations.md`](third-party-integrations.md) — the per-environment vendor-account convention and the
  full integration catalog.
- [`docusign-esignature.md`](docusign-esignature.md) — the sibling e-signature integration (one app, two environments).
  [`deployment-secrets.md`](deployment-secrets.md) — how production secrets are rendered.

## Staging time from Jujutsu

In a private Project checkout, add a final trailer to the change description. Give each block of actual work its own
UUID. This example records twelve minutes. Anyone who can read the repository can read the trailer.

```text
Update the template

Billing-Time: 0.2 2026-11-20 00000000-0000-4000-8000-000000000001
```

`navigator jj push` reads the outgoing Jujutsu changes and stages the signed-in author's annotated time for review. To
stage your time before pushing, run `navigator jj stage-time`, optionally with `--revision`. Your Jujutsu author email
must match your Navigator sign-in email. The checkout's `navigator.yaml` identifies the Project and site. Each work
block retains its original change ID and UUID through a rebase. A retry with changed hours, date, timekeeper, or change
identity requires review.

On the Project's Finances tab, review the time against the engagement and check for overlap with manual or timer
entries. Enter the client-facing description and approve the time, record it at no charge, or deny it with an internal
reason. An active contingency term permits hourly approval only at its agreed hybrid rate. Record eligible court fees as
receipt-backed Project Costs. Review the signed agreement for when each reimbursement is due.

The invoice preview contains approved billable work and confirmed costs. A lawyer reviews its lines and destination
before generating the batch. Review an unsent Stripe draft in the mapped account before release. To apply an advance,
verify the client's cleared and undisputed funds for this matter, the earned fee or incurred expense, and any required
notice. Allocate the verified transfer to the invoice and reconcile it in the Firm journal. Request only the remainder
from the client.
