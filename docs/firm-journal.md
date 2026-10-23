# Firm journal

The Firm owns its authoritative journal. When `NAVIGATOR_POSTGRES_HOST` is set, PostgreSQL stores immutable Journal
Entries, postings, structured source references, evidence snapshots, and reconciliation audits. Without it, Journal
Entries live in SurrealDB's `journal_entry` table, and `list`, `statement`, `preview`, `post`, and `migration-triage`
work unchanged. Reconciliation, audits, and signals require PostgreSQL; without it they answer `503
journal_requires_postgresql`. The web server logs the selected backend once at boot. Project allocations identify which
matter benefits from an entry. A Project statement includes only those allocations.

Each Journal Entry has a Navigator UUID and command key. Bank and accounting provider IDs remain source references,
scoped by provider, tenant, account, object type, external ID, and optional transfer locator. Xero invoice UUIDs,
payment UUIDs, and display invoice numbers remain distinct. A reviewed `journal_account` maps provider account IDs to
the Navigator account used by postings; the external account identity remains in the source reference. One payment can
allocate across several Projects.

## Accounts and quantities

Keep IOLTA and operating accounts separate. Each posting declares Firm or client beneficial ownership and any
restrictions, independently of asset type. Legacy unspecified ownership remains explicit; new commands require reviewed
ownership. Bank of Montreal IOLTA accounts use provider-neutral entries and private statement evidence. Mercury
operating-account transactions can be fetched by explicit account and transaction UUID. No adapter scans a bank account
or initiates a payment.

Quantities are integer strings with an explicit asset unit and precision. Fiat, network-qualified crypto tokens, and
noncash assets balance separately. Unknown valuations remain unknown. Statements report incurred, reimbursed, and
outstanding costs separately from fees. A property award is an entitlement; receipt requires custody evidence. Crypto
evidence includes transfer locator, transaction hash, wallet reference, finality Document, and exact network fees. No
private keys or seed phrases belong in journal input. Crypto reconciliation requires an explicit finalized or
confirmation-count policy. Network fees use a distinct `network_fee` source reference so multiple transfers in one
transaction cannot charge the same fee twice.

Corrections append balanced entries or exact linked reversals. Reimbursable costs require Project allocation and signed
engagement evidence. Reimbursements identify the original cost and cannot exceed its outstanding quantity. Earned fees
remain a separate category.

## Commands

Authenticate to Navigator, then use the Firm UUID:

```bash
navigator journal --firm "$FIRM_ID" list
navigator journal --firm "$FIRM_ID" migration-triage
navigator journal --firm "$FIRM_ID" preview entry.json
navigator journal --firm "$FIRM_ID" post entry.json
navigator journal --firm "$FIRM_ID" statement --project "$PROJECT_ID"
navigator journal --firm "$FIRM_ID" mercury \
  --project "$PROJECT_ID" --account "$ACCOUNT_ID" --transaction "$TRANSACTION_ID"
navigator journal --firm "$FIRM_ID" reconcile reconciliation.json
navigator journal --firm "$FIRM_ID" audits --job "$JOB_ID"
```

`preview` runs the same posting, tenancy, evidence, and database constraints in a transaction that rolls back. `post`
validates the whole balanced command. Retrying an identical key returns its existing identity; changed content is
refused. Statements retain separate account, category, and asset balances and never imply complete IOLTA coverage.
Original source JSON and receipts are private Documents; Project repositories hold Document pointers only. Keep journal
command files and provider IDs out of Project Git; evidence filenames use content digests rather than bank IDs.

Reconciliation supports preview, approved safe repair, and reviewed correction. Approvals bind to exact command digests.
Repair keys include scoped source identity, revision, and repair type. Pending sources, missing evidence, unavailable
sources, and incomplete statement coverage prevent reconciliation. Provider requests are read-only.

`--refresh-project` re-fetches only the source references declared in a reconciliation file. Add `--interval-seconds`
with at least 60 seconds to poll those records. The Project must belong to the Firm. Xero reads use Firm-owned custom
connection credentials and tenant identity, with granular invoice, credit note, payment, bank transaction, settings, and
attachment read scopes. Original bank-currency quantities govern bank reconciliation; invoice currency and rates remain
evidence.

## Journal store

Both stores run the same command checks: balance, ownership, evidence, repayment, history, and idempotent command keys.
The SurrealDB store writes the canonical serialized entry with its insertion timestamp and serializes commands within
one process; PostgreSQL adds the Firm row lock across replicas. A configured but unreachable PostgreSQL fails the
request rather than falling back to SurrealDB.

## PostgreSQL startup

Configure discrete `NAVIGATOR_POSTGRES_HOST`, `PORT`, `USER`, `PASSWORD`, `DATABASE`, `SSLMODE`, and optional
`ROOT_CERT` values. Production connections require verified TLS and a dedicated application role. Apply the canonical
PostgreSQL schema with the operator connection before serving requests. Ordinary journal requests perform no DDL. The
temporary startup importer copies existing Surreal Journal Entries before serving, preserving IDs and all recorded
timestamps, skipping identical rows, and refusing conflicts. Missing insertion timestamps require triage and remain in
SurrealDB until original timestamp evidence is established. Occurrence time never substitutes for insertion time. The
importer never deletes the source. Other application tables still use SurrealDB.

## Linked source signals and monitoring

Configure the Firm's encrypted `xero_webhook_key` for `POST /webhook/journal/xero/{firm}`. The handler verifies HMAC-
SHA256 against the original request bytes before accepting an event. Invoice and Credit Note events wake only already-
linked sources; event URLs are never followed and events never authorize a financial write. Signed empty handshake
requests return success. Unsupported event categories do not expand the job's scope.

Polling resumes from the latest PostgreSQL audit cursor and checks signals every five seconds between scheduled runs.
Missed events and objects without a supported webhook are covered by the regular polling interval. Use `--summary` for
scheduled logs that expose counts and health without source IDs, amounts, or memos. Monitor a specific job with
`navigator journal --firm "$FIRM_ID" status --job "$JOB_ID" --stale-after-seconds 600`; missing, stale, or unresolved
reconciliation exits with failure. Credentials and reconciliation command files belong in private mounted storage.

The Solana adapter reads one explicitly selected signature at finalized commitment. Transfer locators identify a parsed
top-level or inner instruction. Native SOL and classic SPL Token quantities retain their original precision; unsupported
instructions require reviewed evidence. A failed transaction cannot become a successful transfer. Network fees have
their own source reference and are recorded once per transaction, even when it contains multiple transfers. A wallet
that did not pay the network fee cannot claim it. No adapter signs or broadcasts a blockchain transaction.
