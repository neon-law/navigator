# Storage models and writes

PostgreSQL is the application storage authority. Name tables from the [glossary](glossary/README.md), use relational
constraints, and add ordered migrations under `store/postgres/migrations/`. The consolidated model is
[`store/postgres/schema.sql`](../store/postgres/schema.sql); SQLx owns the migration ledger. Apply migrations with
`navigator-migrate` before serving requests. [PostgreSQL tooling](postgres-tooling.md) covers the driver and runner.

Write through the domain accessor. Ordinary application reads and writes require PostgreSQL configuration and completed
historical enrollment. Missing configuration, unavailable PostgreSQL, or invalid history returns an error.

## Historical admission

`PostgresDb::application` checks the completed global import and every current scoped activation. A database with tables
but no valid handoff cannot serve application requests. Each scope names an exact versioned set:

| Scope | Enrollment |
| --- | --- |
| `satellites:1` | Thirteen matter satellite tables |
| `notation:1` | Eight Notation, Answer, Asset, and document tables |
| `identity:1` | Nine Person, Entity, mailbox, and identity-detail tables |
| `tenancy:1` | Six Firm, Brand, Project, and membership tables |
| `application:1` | Sixteen Template, communication, Deadline, page, secret, audit, and Journal tables |

The registry in `store::postgres::history` owns the table sets. Existing completed scopes retain their sealed receipts
and checkpoints. A later enrollment does not reinterpret an earlier receipt as proof of newly switched tables. Import
preserves native rows and refuses unresolved references or divergent overlap; activation requires the registered scope's
complete proof. See the [scoped enrollment procedure](postgres-tooling.md#application-admission).

Explicit migrations, imports, diagnostics, and raw recovery connect through `PostgresDb::from_env` before application
admission is possible. That operator connection does not activate a database. Source reads belong to the explicit
importer context, with verified capture and the required source quiescence; matching counts alone do not prove parity.
Activated PostgreSQL rows cannot be overwritten or deleted by replaying an older source.

## People

`store::persons` uses `store::postgres::people`. Person and mailbox claims commit together, aliases retain claim
ownership, and a merge moves PostgreSQL references transactionally. Cap Table Holding and Fiduciary Role writes lock
their typed Person or Entity in the same transaction as their row write. Person merges retarget financial Person links
before deleting the duplicate; a unique collision refuses the transaction, and same-UUID Entity links remain distinct. A
Person named as an immutable Billing Annotation author or reviewer, or as a Legal Plan Claim event actor, cannot be
merged away. The merge returns a specific refusal and retains both identities, participation, and evidence; it preserves
billing attribution and claim collection history. Other eligible application Person merges remain PostgreSQL-only.
Identity detail uses `store::postgres::identity`: addresses, credentials, email tokens, entities, delegates, external
identities, and phones. The people directory reads the `person_directory` and `firm_visible_person` views.

## Tenancy

`store::postgres::tenancy` owns Firms, Brands, Firm Brands, Firm memberships, Projects, and Project participation.
Identity and tenancy require their exact scoped handoffs; application writes create no SurrealDB compatibility copy. An
Attorney Calendar (`person_calendar`) is PostgreSQL-native. A lawyer-tier sign-in records the Workspace address as
primary when the Person exists and has no primary calendar.

The Project list and single-code integration lookup read the live `project_list` view. It joins each Person’s
participation to the Project. The caller filters by Person and participation side before returning Project rows.

## Relationship graph

PostgreSQL holds `entity_role`, `relationship`, `project_reference`, `disclosure`, and the append-only
`relationship_log`. Edges are projected into `navigator_graph`, and conflict checks traverse Apache AGE. Relationship
Log entries remain distinct from graph edges; their actor and Project references must exist before an append. Explicit
historical import preserves native graph rows. Follow the [AGE graph runbook](age-graph-runbook.md) for extension setup,
migrations, backfill, and parity.

## Project lifecycle

PostgreSQL commits each [Project Status Transition](glossary/project-status-transition.md) with the Project's current
status and retention start. Historical enrollment checks the transition chain and terminal projection while preserving
native lifecycle decisions and unrelated Project fields, without inferring timestamp witnesses. Lifecycle commands
return PostgreSQL failures and create no SurrealDB projection.

## Matter records

Notation history, Answers, version pins, Assets, and document revisions use their PostgreSQL accessors. Document writes
retain object-before-row ordering: object storage accepts the bytes before PostgreSQL records their identity, evidence,
and visibility. Template versions referenced by Notations, events, and packages must exist before their constraints can
be applied; never invent a version or delete a pin to make migration pass.

`store::postgres::matter` owns statutory deadlines, Notation drafts, estate assets, cap-table holdings, fiduciary roles,
Git repositories and access tokens, expunge records and requests, playbooks, contract reviews, authority uses,
citations, attestations, Project modules, testimonials, and social OAuth attempts. `store::postgres::captions` owns
Matter Caption replacement with the Project/Firm constraint. Matter Pages and Notation document comments are
PostgreSQL-native and preserve their Project, Asset, and Notation boundaries.

Atomic patches preserve unrelated columns, JSONB values retain JSON null, and capability toggles commit with their
Relationship Log audit. OAuth callback consumption returns its verifier once and erases it in the same statement. Person
merges account for orphan-tolerant links through canonical record metadata as well as catalog foreign keys.

## Catalogs

Shared Authorities, Entity Types, Glossary Terms, Jurisdictions, Notation packages, rates, and Questions use
`store::postgres::catalog`. These retain their global-import contract within application admission. Versioned Templates
use `store::postgres::templates` and the Application handoff; their bodies remain in Assets.

## Signatures and verifications

`store::postgres::signatures` owns Signature envelopes, Notarizations, and citation Verifications. Provider request
indexes resolve replay races, and independent verification axes retain the draft revision they attest to. Existing
Project, Notation, Template, and encrypted Firm connection records must be present before execution can be verified.

## Communications and email

Communications, email conversations, receipts, deliveries, and delivery attempts use PostgreSQL. Unique indexes decide
replay races, and a delivery reservation commits with its attempt. Parent Project, Person, Firm, Notation, and Asset
rows must exist. Retention refuses to delete a Communication still named by an Inbox Message or Notation source.

Inbox conversations, Messages, assignments, ingress, attachments, and triage use PostgreSQL. The Inbox sorts email
threads by the latest recorded send time, falling back to the Conversation update time. Attachment links keep provider
order and names; object bytes are read only after Project and Message access checks.

Mailrooms, Letters, Leads and consent history, Project Deadlines, Project Services, and summary-sender approval history
join Templates, pages, comments, encrypted secrets, Sent Email, and the Firm journal in Application enrollment. Sent
Email handoff distinguishes retained source rows, native-only audit history, and proven retention purges. Import and
purge use the same UTC-day lock so an older source cannot resurrect a purged day. Journal handoff checks immutable
headers, postings, references, causal history, and source containment without rewriting native history. See the [Firm
journal](firm-journal.md).

## Brand and social content

Posts, Blog Videos, Social Handles, Social Publications, Social Accounts, and account selections use
`store::postgres::social`. Brand and slug, object-key, publication, and account indexes decide races. Brand, Firm, and
Person parents must exist in PostgreSQL; source import cannot overwrite their native authority.

## Encrypted configuration

Firm integration secrets preserve encrypted versions and their authenticated context. Deployment Secret Changes retain
the exact source timestamp used by encryption as well as their PostgreSQL representation. Migration does not decrypt and
re-encrypt history to align timestamps. [Tasks](glossary/task.md), [Case Stages](glossary/case-stage.md), and
[Purchases](glossary/purchase.md) also have PostgreSQL-native models.

## PostgreSQL recovery

Physical base backups, WAL archives, and [Iceberg logical exports](iceberg-archive.md) have separate recovery evidence.
Prove restoration with their respective drills. [Project recovery archives](project-recovery-archives.md) cover the
Project repository and document parts of the recovery contract.
