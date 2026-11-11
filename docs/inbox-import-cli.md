# `navigator-deploy inbox import`

Import exported email and LinkedIn messages into a Firm's Inbox. The server retains the original payload, parses
Messages, threads and deduplicates them, and files attachments when a Project is supplied.

The command is ingest-only: there is no `navigator-deploy inbox send` command. `--dry-run` needs no login.

Log in with `navigator site login --host https://your-deployment.example` before transferring imports.

A successful import exits 0, including a fully deduplicated replay. Retrying is a reconcile step: existing ids are
reused and any missing attachment filing or attribution is completed.

Supported sources:

- `--source=email`: one RFC 5322 message per `.eml` file, or a directory of files with `--dir`.
- `--source=linkedin`: `messages.csv` from a LinkedIn data export.

```bash
# Import one message and attribute its Conversation to a Project.
navigator-deploy inbox import --source=email --mailbox=lawyer@neonlaw.com \
  --file=notice.eml --project=sample-litigation

# Import a directory of messages.
navigator-deploy inbox import --source=email --mailbox=lawyer@neonlaw.com --dir=./thread

# Import a LinkedIn export using the exporting account's profile URL.
navigator-deploy inbox import --source=linkedin --mailbox='https://www.linkedin.com/in/your-profile' \
  --file=messages.csv
```

Re-run the same command after an interrupted request. Existing Message and Conversation ids are reused; retries finish
missing attachment filings and attribution. The archive key stays the same for identical bytes in the same Firm and
mailbox. Original attachment names remain on the Message; filed document identities preserve extensions or use `.bin`
when the provider supplied none.

`--dry-run` prints the complete request JSON, including the base64 payload, and makes no request or login lookup.
`--host` selects the deployment's stored session. `--project` supplies a visible Project code for attribution.

Remote deployments require HTTPS. The command refuses redirects to keep credentials and payloads on the selected
deployment; HTTP is allowed for a local loopback fixture.

## Server requirements

The command calls `POST /app/api/inbox/import` with a lawyer-tier session. Writing imports requires the PostgreSQL Inbox
authority; see [PostgreSQL tooling](postgres-tooling.md). A server dry run can parse and preview without that authority.

The deduplication key is `(firm, channel, provider_account, provider_message_id)`.

Mail whose sender matches the importing mailbox becomes an outbound Message authored by the importing lawyer. Historical
delivery is recorded as `received`, so imports do not queue a provider send.

Without a Project, attachment bytes remain in the original archived payload; a later import with `--project` files them
through the shared document pipeline.

## Errors

- Unsupported sources, unreadable or empty files, invalid payloads, and missing dates fail clearly.
- A dropped connection can leave a partially completed import. Re-run the same command to reconcile it; Message ids and
  attachment filings remain stable.
