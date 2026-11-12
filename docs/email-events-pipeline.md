# Email events pipeline — SendGrid delivery stream to BigQuery

Two halves of the outbound-email audit picture, joined in BigQuery.

- **Request side** — `workflows::email::LoggingEmail` writes one `sent_email` row per attempt, from both `web` and the
  `workflows-service` worker. A SendGrid `202` proves the message was *accepted*, not *delivered*. Each row carries
  `sg_message_id` (SendGrid's `X-Message-Id`, the join key to the delivery stream), `template_slug` and the addressee's
  `person_id`. See [the request-side store](#the-request-side-store) below.
- **Delivery side** — SendGrid's Event Webhook POSTs lifecycle events (`processed`, `delivered`, `open`, `click`,
  `bounce`, `dropped`, `deferred`, `spam_report`, `unsubscribe`). The `portal::email_events` handler lands each POST as
  one Snappy Parquet object on GCS — the same Parquet-on-GCS shape the [`archives`](../cloud/README.md) snapshot uses,
  so BigQuery reads it through an external table.

Inbound Parse is a separate callback adapter. In raw mode, `portal::inbound_email` extracts MIME attachments from the
RFC 5322 `email` field, scans each exactly once through the configured cluster-private ClamAV service, and only then
permits clean bytes into lawyer notifications or matter documents. SendGrid spam scoring is not used as a malware
verdict.

## How the join works

At send time `SendGridEmail::build_request_body` stamps top-level `custom_args` — `template_slug` and `person_id` (see
`OutboundEmail::with_person`). SendGrid echoes those keys on every lifecycle event for that message, so the delivery
rows carry `template_slug` / `person_id` directly and the analytics join needs no address parsing:

```text
sent_email.sg_message_id ── 1:N ── email_events.sg_message_id
sent_email.person_id     ─────────  persons.id
email_events.person_id   ─────────  persons.id
```

The admin "Send welcome" path sets `person_id`; workflow-driven sends carry `template_slug` only until `EmailPayload`
threads a person id (a follow-up).

## The request-side store

PostgreSQL is the system of record for `sent_email` whenever `NAVIGATOR_POSTGRES_HOST` is set (`store::sent_emails`); a
configured but unreachable server fails the audit insert, never falls back. Without a host (a bare dev run, the
in-memory tests) the same calls use SurrealDB. Historical SurrealDB rows reach PostgreSQL through `hudson ops postgres
mirror reconcile`, which inserts only what PostgreSQL lacks.

The table is **append-only in the database**. A trigger refuses every `UPDATE` and `TRUNCATE`, and refuses a `DELETE`
unless all three hold: the session set `navigator.sent_email_purge`, the row is more than a year old, and its UTC day is
in the `sent_email_export` ledger. Only the retention purge sets that flag. The application role keeps its ordinary
grants, so the trigger is the guarantee.

PostgreSQL keeps one year. The daily [Sent Email Export](glossary/sent-email-export.md) (DBOS workflow
`SentEmailExport`, `archives::sent_email_export`, 02:41 UTC in `web`'s DBOS executor) runs three steps:

1. **Find pending days.** These are closed UTC days with rows but no ledger entry, or with more rows than the ledger
   counted (a late insert).
2. **Export each day.** The day's rows become one Snappy Parquet file. The file is committed to an Iceberg v2 table
   partitioned by `day(sent_at)`, and the day goes into the ledger with its row count and the SHA-256 of its row ids.
3. **Purge.** Each exported day more than a year old is removed, but only after the day still matches its ledger row in
   count and digest. Rows a `project_service` cites as evidence of service stay. The ledger stamps `purged_at`, so a
   purged day is never exported again.

```text
gs://<NAVIGATOR_ARCHIVES_BUCKET>/email/sent_email/
  metadata/version-hint.text, v<N>.metadata.json
  metadata/snap-<snapshot-id>-<attempt>.avro             # manifest list: every day so far
  metadata/day-<YYYY-MM-DD>-<sha256>-<attempt>-m0.avro   # one manifest per day
  data/sent_at_day=<YYYY-MM-DD>/part-<sha256>-<attempt>.parquet
```

Every key is new to its attempt, so the export never replaces an object. On a retention-locked bucket each write is
create-only and `version-hint.text` is never written; readers take the highest `v<N>.metadata.json`. A re-run is
idempotent. An unchanged day matches its ledger digest and writes nothing. A day that gained a row is written again
under its new digest, and the new snapshot drops that day's old manifest (an Iceberg overwrite). A crash between the
Iceberg commit and the ledger write finds the day already in the current snapshot and records the ledger without
committing twice. Every snapshot carries earlier days forward, so the current snapshot holds the whole trail after
PostgreSQL has purged a day. The table lives apart from the full-table `postgres/navigator/sent_email` coverage lineage
and is never pruned.

Enable it on `web`, beside `NAVIGATOR_DBOS_ENABLED=true`, with `NAVIGATOR_DBOS_SENT_EMAIL_EXPORT_ENABLED=true` (only
`true` opts in; any other non-empty value fails the boot). It runs in `web` because that is the process `ops ship` gives
the `navigator_app` PostgreSQL login and the DBOS login, and it keeps the export off Restate. It also needs
`NAVIGATOR_ARCHIVES_BUCKET` and write access to that bucket for `web`'s service account. An enabled schedule fails the
boot without PostgreSQL or the bucket. Until the flag is on, nothing is exported, so nothing is ever purged.

A `workflows-service` worker with no PostgreSQL login, as the GKE example ships it, journals its sends to SurrealDB.
They reach PostgreSQL, and then the export, only when `hudson ops postgres mirror reconcile` or its recurring Job runs.
Giving the worker the `navigator_app` login writes them to PostgreSQL directly.

Register the table in BigQuery once, as a BigLake Iceberg table over the current metadata file:

```sql
CREATE EXTERNAL TABLE `YOUR_PROJECT_ID.navigator_bi.sent_email`
WITH CONNECTION `us-west4.exports`
OPTIONS (
  format = 'ICEBERG',
  uris = ['gs://YOUR_ARCHIVES_BUCKET/email/sent_email/metadata/v<N>.metadata.json']
);
```

A BigQuery Iceberg external table reads the snapshot that metadata file names, so re-point `uris` (or use a BigLake
metastore) to see later days.

## Object layout

One POST becomes one object:

```text
email-events/data/dt=<YYYY-MM-DD>/<sha256(body)>.parquet
```

- `dt=<date>` is a Hive-style partition (date of the first event in the batch) so a BigQuery external table prunes by
  day.
- The filename is the SHA-256 of the raw request body, so SendGrid's at-least-once retries (it re-POSTs the identical
  body on any non-2xx for 24h) overwrite the same object — file-level idempotency with no dedupe table. `sg_event_id` is
  unique per event for row-level dedupe at query time.

Columns (all nullable `Utf8` except `event_unix_ts` which is `Int64`): `sg_event_id`, `sg_message_id`, `event`, `email`,
`template_slug`, `person_id`, `url`, `reason`, `status`, `timestamp_utc`, `event_unix_ts`, `raw_json`. `raw_json` keeps
the whole event so a field we don't model yet is never lost.

The events land in `web`'s configured storage bucket under the `email-events/` prefix. A deployer who wants delivery
analytics on a separate lifecycle (and IAM) can point a dedicated bucket — e.g. `YOUR_PROJECT_ID-events` — at the prefix
instead; nothing in the handler hard-codes a bucket.

## Operator setup (one-time, machine-bound)

These run on the operator's machine against the live project — `web` issues no BigQuery DDL itself.

### 1. Configure the SendGrid Event Webhook

Point SendGrid's Event Webhook at the path-secret URL and store the secret so prod boot accepts it:

```bash
# Pick a long random token; SendGrid posts to this exact URL.
kubectl -n navigator create secret generic navigator-web-secrets \
  --from-literal=SENDGRID_EVENTS_SECRET="$(openssl rand -hex 24)" \
  --dry-run=client -o yaml | kubectl apply -f -
# In the SendGrid console, set the Event Webhook POST URL to:
#   https://www.your-domain.example/webhook/email-events/<that-token>
```

The path is `/webhook/email-events/`, not `/api/email-events/`. A URL under `/api/` returns 404 for every event, and
SendGrid retries and then drops the events.

`SENDGRID_EVENTS_SECRET` is a deployment boot invariant (`portal::config::enforce_deployment_invariants`), so a deploy
without it fails fast rather than serving an unauthenticated endpoint. The ECDSA "Signed Event Webhook" is stronger: it
authenticates the payload, not just the URL.

### 2. Create the BigQuery external table

Reuse the `navigator_bi` dataset and connection from the archives bootstrap (see
[`cloud/README.md`](../cloud/README.md)), then:

```sql
CREATE EXTERNAL TABLE `YOUR_PROJECT_ID.navigator_bi.email_events`
WITH CONNECTION `us-west4.exports`
OPTIONS (
  format = 'PARQUET',
  uris = ['gs://YOUR_PROJECT_ID-assets/email-events/data/*'],
  hive_partition_uri_prefix = 'gs://YOUR_PROJECT_ID-assets/email-events/data',
  require_hive_partition_filter = false
);
```

Substitute the bucket `web` actually writes to (its `NAVIGATOR_GCS_BUCKET` / storage backend). New partitions show up on
the next query — BigLake external tables re-scan their `uris` glob, so there is no refresh step.

## Updating the pinned SendGrid OpenAPI adapter

The vendored SendGrid contract and its adapter template are pinned together with the generated adapter. After
intentionally updating either vendor input, run the regeneration command from a source checkout:

```text
cargo run -p hudson -- dev sendgrid-openapi --regenerate
```

The first run calculates the specification and deterministic rendered-adapter hashes before it writes anything. If
either pin is stale, it leaves `workflows/src/email/sendgrid_openapi.rs` unchanged and prints the exact
`EXPECTED_SHA256` and `EXPECTED_ADAPTER_SHA256` replacements for `deploy/src/sendgrid_openapi.rs`. Apply both
replacements, then rerun the same command; it writes the adapter and verifies the complete pinned set. Commit the
updated vendor input, both pin constants, and generated adapter together.

## Analytics

Per-template delivery funnel, joined to the request side:

```sql
SELECT
  e.template_slug,
  COUNT(DISTINCT e.sg_message_id)                          AS messages,
  COUNTIF(e.event = 'delivered')                           AS delivered,
  COUNTIF(e.event = 'open')                                AS opened,
  COUNTIF(e.event = 'click')                               AS clicked,
  COUNTIF(e.event IN ('bounce', 'dropped', 'spam_report')) AS problems
FROM `YOUR_PROJECT_ID.navigator_bi.email_events` AS e
WHERE e.dt >= '2026-05-01'
GROUP BY e.template_slug
ORDER BY messages DESC;
```

## Non-goals

- **Live dashboards.** The lake is for analytics; the `sent_email` table (admin `/app/admin/email-log`) stays the
  operational request-side view for its year in PostgreSQL.
- **Replacing `sent_email`.** PostgreSQL holds the request side for a year and the Sent Email Export's Iceberg table
  keeps it after that. Parquet-on-GCS holds the delivery side, and BigQuery joins them.
- **Iceberg metadata for the delivery side.** `email-events/` stays a Parquet external table; only the request side has
  authored Iceberg metadata.
