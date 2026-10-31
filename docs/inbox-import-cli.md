# `navigator inbox import` — bring a lawyer's own correspondence into the Firm Inbox

Navigator's Inbox already holds what providers push to the firm and what Navigator itself
sends. What it has never held is the lawyer's own half of the exchange: mail from their native
mailbox and their LinkedIn messages. `navigator inbox import` closes that gap — it hands a
correspondence payload to the firm deployment's import door, which parses it, threads it,
deduplicates it, and files any attachments, without sending anything to anyone.

Two sources ship today:

- `--source=email` — one RFC 5322 message per `.eml` file (or a directory of them via `--dir`).
  Attachments found in the message are filed as document revisions with full provenance.
- `--source=linkedin` — the `messages.csv` from a LinkedIn data export ("Settings → Get a copy
  of your data"). A LinkedIn export carries no attachments.

## Usage

```console
# One court notice from the lawyer's own mailbox, attributed to a matter at import:
$ navigator inbox import --source=email --mailbox=nick@neonlaw.com \
    --file=notice.eml --project=24-1043

# A whole thread directory:
$ navigator inbox import --source=email --mailbox=nick@neonlaw.com --dir=./thread

# A LinkedIn data export:
$ navigator inbox import --source=linkedin --mailbox=linkedin.com/in/nickshook \
    --file=messages.csv
```

Replaying the same payload — the same `.eml`, the same export `messages.csv` — reports the
existing conversation and message ids as deduped and exits 0. Re-running a bulk import is
always safe.

`--dry-run` prints the exact request JSON for each payload and sends nothing; it needs no
login. `--host` selects the deployment exactly as every other server verb does; `--project`
is the matter code the imported conversations are attributed to at import.

## What the server does with an import

The CLI is a thin client over `POST /app/api/inbox/import` — the same bearer session
`navigator site login` stores, the same lawyer-tier gates as the portal. The door's adapter
per source parses the payload, archives the verbatim bytes in object storage, and walks each
parsed message through the Inbox's postgres ingest: dedup on
`(firm, channel, provider_account, provider_message_id)`, threading by conversation, and
project attribution when `--project` names a matter. A message whose sender equals the
importing mailbox ingests as the lawyer's own outbound mail.

## No send verb

`navigator inbox import` is ingest-only. There is no `navigator inbox send` and no outbound
path of any kind in the import surface, matching `navigator mail`'s documented draft-only
posture: the lawyer keeps sending from their own client; Navigator keeps the record.

## Errors worth knowing

- An unknown `--source` fails immediately, naming the two supported values.
- A payload that cannot be read or is empty fails before anything is sent.
- If the request is sent and the outcome is unknown (a dropped connection mid-import), the
  error says so and names the reconcile step: re-run the same command — a replayed payload is
  deduped to the existing rows, so nothing duplicates.
