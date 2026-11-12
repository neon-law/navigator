# Import Google Fi history

`hudson inbox import-fi` imports call and SMS metadata into the Firm Inbox. It preserves historical inbound and outbound
directions. A CSV direction does not establish who made a call or wrote a text, so historical human authors stay unset.
It sends nothing, imports no message content or recordings, creates no billing, and archives no raw CSV. Fi
conversations remain read-only for external replies; authorized people can leave internal notes and link them to a
Project.

Download metadata through [Google Fi history](https://support.google.com/fi/answer/6380147?hl=en). The importer uses an
explicit mapping rather than assuming Google's export headers. Choose the columns and value labels actually present in
your file. Preview is the default and checks authorization, parsing and existing source identities without writes.

```bash
hudson inbox import-fi --file history.csv --mapping mapping.json \
  --account "$FI_ACCOUNT" --brand example
```

After reviewing the preview, repeat with `--apply`. Add `--project <code>` to link new history to an authorized
same-Firm Project. The import requires PostgreSQL Inbox authority and the existing lawyer-tier import session. Use
`navigator site login` when the CLI has no session. The CSV cannot choose your role, Firm or authorization.

## Mapping

This example uses **fictional headers and values**, not a published Google schema:

```json
{
  "columns": {
    "kind": "type",
    "direction": "way",
    "counterparty": "phone",
    "timestamp": ["at"],
    "duration": "length",
    "identity": "id",
    "status": null
  },
  "kinds": {"C": "call", "S": "sms"},
  "directions": {"I": "inbound", "O": "outbound"},
  "timestamp_format": "%+",
  "timestamp_separator": " ",
  "timezone": {"mode": "embedded_offset"},
  "duration_unit": "seconds"
}
```

The kind and direction columns may be the same column when a source label encodes both. Every source label needs an
explicit value-map entry. Optional `statuses` map source labels to `completed`, `missed` or `unknown`; set
`columns.status` only when using that map. A missing status stays unrecorded. No content column is accepted. Unmapped
columns are ignored and never persisted, archived or logged.

The importing account must be E.164. Counterparties must be phone-number metadata: digits with optional leading `+` and
ordinary spacing, parentheses, dots or dashes. Formatting is removed for stable identity; no country code is guessed for
local numbers. Unknown caller names and arbitrary text fail validation rather than becoming an address.

`duration_unit` is required: `seconds` or `minutes`. Decimal minutes convert exactly to whole seconds. Negative,
overflowing and fractional-second results are rejected. Set `columns.duration` to null when the file has no duration
column. A blank duration stays unrecorded; zero remains zero. SMS duration must be blank. Nothing infers answered status
or billed tenths from a duration.

For offset-bearing timestamps, use `embedded_offset` and a format that reads the offset, such as `%+` for RFC 3339. For
naive local timestamps, choose an explicit zone and format:

```json
{
  "timestamp_format": "%Y-%m-%d %H:%M:%S",
  "timestamp_separator": " ",
  "timezone": {"mode": "iana", "value": "America/New_York"}
}
```

A fixed-offset source may use `{"mode":"fixed_offset","value":"+02:00"}`. Never use a fixed offset to approximate a
source that changes for daylight saving. Two timestamp columns are joined using `timestamp_separator` before parsing.
Ambiguous or nonexistent daylight-saving times are refused; use offset-bearing values to identify the actual instant.
The host timezone is never consulted. Timestamps must be representable as whole microseconds in PostgreSQL.
Sub-microsecond values and leap seconds are rejected rather than silently truncated or reinterpreted.

## Replays and duplicates

Map `columns.identity` to a native stable event ID when one exists. Otherwise set it to null: identity uses the
canonical metadata, independent of file order, filename and import time. Reordered and overlapping exports dedup. Two
no-ID rows with identical canonical facts in one file are indistinguishable and reject the file with their row numbers.
Distinct real events with identical metadata need distinct native IDs. An export without native identity cannot
distinguish a new real event from an existing event with exactly the same facts.

A repeated native ID with changed metadata conflicts, including concurrent imports and attempts to change call/SMS kind.
Existing records are never overwritten. Fi identities and accounts have their own namespace and never become Twilio
CallSids. Dedup applies within the Fi source; the importer never guesses that a Fi record and a Twilio record are the
same event from matching phone numbers or times. Historical Messages have delivery state `received` even when their
direction is `outbound`, so they cannot enter the delivery queue.

The entire file is parsed and checked before writes. Limits are 4 MiB of CSV, 10,000 rows, 64 unique headers and 64 KiB
of mapping. Duplicate headers, including case variants, are rejected. Preview reports new, existing and conflicting
rows. Apply rechecks the stored winner; a preview is not a reservation.

A storage or attribution failure can interrupt an applied batch. The response lists confirmed Message and Conversation
IDs and the failing row. That row may already have committed if its acknowledgment was lost. Rerun the unchanged batch
with the same account and Project to reconcile it. Already accepted identities dedup; a changed payload does not
silently replace history. A busy concurrent Fi import also asks for an unchanged retry. There is no whole-file
transaction across Inbox and Project projection.

A Fi import adds no BilledCall rows or invoices. Unassigned calls appear in the parked queue, and Brand totals include
imported metadata. Known duration remains metadata; billed units stay absent. Project attribution changes scope, not
historical facts.
