# Propose a Deadline

`navigator project deadline propose` shows a cited, date-only proposal. It does not record a Deadline until the lawyer
passes `--confirm`. The command resolves a filed Authority's canonical citation through the authenticated API and
requires that Authority's archive to remain retrievable. This checks the filed record; the lawyer verifies that the rule
and its offset correctly state the source.

Save a private JSON rule outside Git. Its required fields are:

| Field | Meaning |
| --- | --- |
| `trigger` | Event whose date starts the count. |
| `offset_days` | Signed whole-day offset from the trigger. |
| `basis` | `calendar_days`, `court_days`, or `holidays`. |
| `authority_kind` | `statute`, `court_rule`, `court_order`, `contract`, or `internal`. |
| `authority_id` | Id of the already filed Authority. |

Calendar-day counting adds the signed offset without holiday, service-method, or time-of-day adjustments. Navigator
supplies no built-in legal rules. Court-day and holiday rules return `due_on: null` and `manual_due_on_required: true`.

```bash
navigator project deadline propose \
  --rule /private/path/rule.json \
  --trigger-on 2028-02-28 \
  --title "Synthetic response" \
  --replay-key synthetic-response
```

Output includes the proposal date, trigger, basis, citation, Authority id, and replay key.

Review the proposal against the filed source. Changing the trigger never changes a stored Deadline.

To record the reviewed date, repeat those inputs with `--confirm`, the Project, and `--due-at` containing the lawyer's
chosen time and explicit offset. Its local date must match the proposal. For a court-day or holiday basis, also supply
the lawyer-counted `--due-on YYYY-MM-DD`. Navigator never invents a due time or counts those rules.

Confirmation uses the existing Project Deadline API and its lawyer and Project access checks. A new row is internal,
uses calendar role `firm`, and records source `lawyer`.

The citation comes from the Authority record. The description retains the proposal inputs and Authority id for review.

A stable replay key identifies the obligation: keep it when correcting the trigger. Repeat confirmation intentionally
updates that row's reviewed date, title, authority, and proposal audit through the existing update route. Owner, lead
times, visibility, calendar role, source Document, source, and status stay unchanged. Existing description notes remain
beside the refreshed proposal audit. Cancelled rows must be reopened through the existing Deadline command before
confirmation. Use `navigator project deadline update` for owner, lead times, or visibility changes.

A successful confirmation prints the stored row. Read it back with `navigator project deadline list` and check the due
instant, citation, and replay key. A proposal does not schedule reminders or mirror a calendar.
