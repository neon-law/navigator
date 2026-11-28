# Xero billing — setup, invoice flow, and production cutover

**Xero holds the accounting ledger.** Lawyers agree a matter's price with the client. Navigator can issue a Xero invoice
from approved billing work or prepare a Stripe draft for the Firm's configured payment destination. This page covers the
**receivables** side — how a Xero invoice reaches the portal and how its paid-status is reconciled back — and how **one
custom connection per organisation** keeps test activity off the live ledger. The **client-trust** side of the same
integration, where Xero is equally the books, is [`trust-accounting.md`](trust-accounting.md).

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
destinations. Without a matching state route, Navigator prepares an unsent draft on the Firm's configured operating
account for review. If neither destination is configured, the Xero receivable workflow remains available. A configured
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
billing batch retains the account and invoice identity so a retry cannot silently move to a different account. A Stripe
draft does not create a Xero receivable or settle time and expenses. The Stripe path does not collect payments or
calculate a card surcharge.

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

## Who raises Xero invoices

Lawyers agree the price with the client and can raise an **`ACCREC`** (accounts-receivable) invoice in Xero directly or
generate it from the Project's Finances tab. Navigator composes the invoice from the agreed fee or recorded billable
work and confirmed costs.

What Navigator does hold is a **read-only mirror**: the `xero_invoice` table carries the Xero `InvoiceID`, reference,
amount, dates, and paid-status for **every** invoice tagged to a matter. The row is keyed on the Xero invoice id;
several invoices may share one `project_id`. Lawyers set Xero `Reference` to `Matter <project uuid>` or `Matter <project
code>`. The nightly ingest lists `ACCREC` invoices, upserts each that resolves to a live Project, and skips DRAFT,
DELETED, and any invoice whose reference does not match a Project (counted as unscoped — no row). That mirror backs the
per-project invoice list in the portal and the Firm trailing-30-day graphs, so nobody has to open Xero to see whether a
matter is paid. Navigator **never holds client funds, card data, or bank credentials** — Xero reconciles against the
firm's bank itself. The integration boundary is the Xero Accounting API and nothing beyond it.

**A trust receipt is not an invoice.** A client's deposit into the firm's IOLTA account is a bank transaction on a
pooled trust account, not an `ACCREC` invoice, and the ingest never writes one into `xero_invoice`. What connects the
two is the **withdrawal**: when the firm draws earned fees out of trust, its allocation lines name the invoices it
settles, and each matter's share posts against that matter's trust ledger. See
[`trust-accounting.md`](trust-accounting.md).

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
the account and Customer of their Stripe subscription. A subscriber whose plan predates Stripe has no Stripe account
to bill and is reported as skipped.

A Purchase Xero invoiced before Stripe keeps its Xero invoice and repeating-invoice ids as read-only history. An unpaid
or unanswered Xero Purchase is expired; a plan that renews through a Xero repeating invoice is cancelled by the Firm in
Xero, not online.

## Where the price comes from: the matter, agreed per client

There is no catalog and no published price. Every engagement is bespoke: lawyers agree the figure with the client for
that matter and raise the invoice in Xero directly. What was actually billed is the Xero invoice, mirrored read-only
into `xero_invoice` — nothing in Navigator computes, quotes, or anchors a price.

An invoice line carries the firm-wide default account code. Tagging invoices to a project is the `Reference` convention
above; the pooled IOLTA account a matter's client funds sit in is derived from the matter's jurisdiction instead, and is
described in [`trust-accounting.md`](trust-accounting.md).

## Authentication: client-credentials grant (preferred)

A custom connection authenticates with the OAuth 2.0 **client-credentials** grant — no user, no redirect, no consent
ceremony. [`XeroClientCredentials`](../billing/src/xero_auth.rs) mints a short-lived Accounting API token and refreshes
it itself, so there is no 30-minute token to rotate by hand. Set the client-credentials pair to activate it:

- `XERO_CLIENT_ID`, `XERO_CLIENT_SECRET` — the custom connection's credentials (the secret is shown **once** at
  creation).
- `XERO_TENANT_ID` — the connected org's GUID (`Xero-Tenant-Id` header). Optional for the live test, which can
  auto-discover it from the `/connections` endpoint since a custom connection binds to one org.
- `XERO_SCOPE` — optional; defaults to `accounting.contacts accounting.invoices accounting.settings.read
  accounting.transactions.read`. The two `.read` scopes are what the IOLTA mirror needs to read the pooled trust bank
  accounts and the transactions that move through them — and no more, so a leaked token cannot move client funds.

A static `XERO_ACCESS_TOKEN` is accepted as a fallback for a quick local smoke test, but Xero expires it in ~30 minutes
and it is ignored when the client-credentials pair is set. The real provider activates when `XERO_TENANT_ID` is present
together with **either** the client-credentials pair **or** a static access token; otherwise `web` uses the stub.

## Sandbox setup (one-time) — sign up and create the custom connection

1. **Create a Xero developer account.** Sign up free at [developer.xero.com](https://developer.xero.com/) and sign in to
   **My Apps**.
2. **Have a demo company.** From your Xero account, enable the **Demo Company** (My Xero → "Try the demo company"). It
   is free, pre-populated, and resets periodically — the right target for dev and CI.
3. **Create a custom connection.** In My Apps → **New app** → choose **Custom connection** (the machine-to-machine,
   client-credentials app type). Name it (e.g. `Neon Law Navigator (demo)`), and add the **integrator** email that will
   authorise it.
4. **Select scopes.** Grant exactly `accounting.contacts`, `accounting.invoices`, `accounting.settings.read`, and
   `accounting.transactions.read` — the last two for the IOLTA mirror, both read-only. A custom connection offers only
   granular scopes — the legacy parent `accounting.transactions` is **not** offered, and requesting it fails token
   minting with `invalid_scope`.
5. **Authorise the connection against the demo company.** The integrator opens the authorisation link Xero generates and
   connects it to the **demo company** org. This binds the connection to that one organisation.
6. **Copy the credentials.** From the connection's Configuration, copy the **Client ID** → `XERO_CLIENT_ID` and generate
   the **Client Secret** (shown once) → `XERO_CLIENT_SECRET`. Set `XERO_TENANT_ID` to the demo org's GUID (or leave it
   unset locally and let the live test discover it).

For production, repeat steps 3–6 with a **separate** custom connection authorised against the firm's **live**
organisation (a paid $5/mo single-org app), and put those credentials in the production deployment's `secrets.enc.yaml`.

## Running the live test (grounding)

The live test in [`server/tests/xero_sandbox.rs`](../server/tests/xero_sandbox.rs) mints a real client-credentials token
against the demo-company connection and drives `ensure_contact` twice with the same unique name — the first call
**creates** the contact, the second must **find** it and return the *same* `ContactID`. This is the only test that
catches a regression in our understanding of Xero's API (a wrong `where` predicate, a bad scope, a rejected payload). It
self-skips green when no creds are present and runs only under the explicit `NAVIGATOR_RUN_LIVE_SANDBOX=1` opt-in, so it
never fires on an ambient-credentials `cargo test`:

```bash
set -a; source .env; set +a
NAVIGATOR_RUN_LIVE_SANDBOX=1 cargo test -p server --test xero_sandbox -- --nocapture
```

It reads the CI `XERO_SANDBOX_*` names first, each falling back to the canonical `XERO_*` name, so a local `.env` drives
it without separate sandbox vars.

## Nightly ingest and paid-status reconciliation

The invoice is raised in Xero. The nightly `ReconcileInvoices` workflow (worker-side, in
[`billing-workflows`](../billing-workflows/)) first **lists** `ACCREC` invoices and upserts each Project-scoped row into
`xero_invoice`, folding `Status` and `AmountPaid` from that list so a first-night Paid invoice already shows Paid. It
then calls `get_invoice` for each mirrored invoice still open (`AUTHORISED`, not `PAID`/`VOIDED`) and folds Xero's
paid-status into the mirror. The portal never calls Xero live.

The same nightly run then refreshes the IOLTA side: the pooled bank account mirrored for each state, the client deposits
and refunds that moved through it, and any withdrawal that settles invoices. Its report counts what it posted and what
it refused — an invoice with no matter reference, a deposit that does not sit on its matter's pool, a withdrawal whose
lines do not add up. This workflow only ever *reads* from Xero. Like every workflow, it is hosted by `workflows-service`
— no per-workflow worker pod.

### Slack reports the nightly run

After the mirror settles, the same run posts its report to Slack **`#finance`** as a second journaled step, so a retry
of the post never re-runs the reconcile. It uses the same Slack App installation and Web API bot as `#general`, not a
second credential or an incoming webhook. Invite that app to the channel and set `SLACK_FINANCE_CHANNEL_ID`; an unset
value fails the run as a **terminal error** rather than reconciling with nowhere to report it.

The app's visible display name is managed in the Slack workspace, separately from its installed identifiers and the
deployment credential. New installations should use **Navigator** as the display name. Verify the actual workspace
display name before enabling live reports; changing it is an external Slack administration step, not a Navigator code
change.

The copy is firm-internal: invoice counts, trust counts, and one line per matter that moved, naming the **Project code**
and the **cents**. Never a client's name, email address, or portal link. Invoices whose Xero `Reference` names no matter
are a **count** — dumping the Xero contact names behind them would put client identities in a channel that has no need
for them. A simulated-matters deployment folds in the staging disclosure, the same signal `#general` gets.

## Production cutover

1. Create the **live** custom connection (separate from the demo one) authorised against the firm's real organisation.
2. Put its `XERO_CLIENT_ID` / `XERO_CLIENT_SECRET` / `XERO_TENANT_ID` in the production deployment's
   `secrets.enc.yaml` (applied to Secret Manager and projected into the `navigator-web-secrets` Secret), never in
   plaintext source.
3. Confirm the live org grants the same `accounting.contacts accounting.invoices` scopes.
4. Verify with one real invoice raised in the live org, `Reference` set to `Matter <project code>`, that
   `ReconcileInvoices` ingests it onto the portal list and mirrors its paid-status.

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
