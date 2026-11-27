# Review court dates

A lawyer confirms each court date in the Project's **Court date review** queue. A supervised clerk can prepare and
correct a proposal. Existing confirmed dates stay in effect while an amendment awaits review.

## Queue dates from the record

Save one submission or an array of submissions in a private JSON file. Give each proposal a stable UUID `request_id`.
Sending the same file again recovers the original proposals. Use `target_deadline_id` to amend an existing Deadline. The
following example uses synthetic identifiers and a placeholder digest.

```json
{
  "request_id": "00000000-0000-4000-8000-000000000001",
  "draft": {
    "kind": "signed_order",
    "title": "File response",
    "due_on": "2030-07-16",
    "evidence": [
      {
        "role": "signed_order",
        "id": "00000000-0000-4000-8000-000000000002",
        "sha256_hex": "<SHA-256 of the filed original>",
        "page": 1,
        "rects": [{ "x": 0.10, "y": 0.25, "width": 0.60, "height": 0.04 }],
        "forgejo": {
          "repository": "project",
          "commit": "0123456789abcdef0123456789abcdef01234567",
          "path": "blobs/orders/sample-order.pdf.yaml"
        }
      }
    ]
  }
}
```

```bash
navigator project deadline review queue --file /private/path/submissions.json
navigator project deadline review list
```

An express date needs the signed order alone. Cite the filed version's asset ID and the page stating the date. Highlight
coordinates range from zero to one, measured from the top left of the displayed page. A date without an explicit zone
inherits the matter zone. A hearing uses `due_at` with an offset, `category: "hearing"`, and optional
`hearing.duration_minutes` and `hearing.location`.

For a calculated date, use `kind: "calculated"` and cite both the triggering record and the governing Authority with
separate `trigger` and `authority` evidence entries. Use the filed Authority ID for the latter. Include its archived
digest, page and highlights. The `calculation` object carries `trigger`, `trigger_on`, `offset_days`, `counting_basis`
and `authority_kind`. Select `case_law`, `court_rule` or `statute` according to the filed source. Calendar-day offsets
must agree with the proposed date. A lawyer supplies a date that depends on court days or holidays.

### Trial series and depositions

For a trial, use `category: "trial_day"` and the first start in `due_at`. In the hearing details, supply
`duration_minutes`, `location`, and an `appearance` object with `kind: "trial_day"` and an `additional_starts` array of
explicit RFC 3339 start times in order. Run the queue command with `--preview` to see suggested preparation. Edit or
drop entries in `hearing.prep` before queuing the draft. Confirm all trial dates and their preparation together in Court
date review. To continue or cancel one day, target its Deadline ID and cite the filed order or stipulation in the
revised draft. Set `hearing.appearance.cancel` to `true` to cancel that day.

Create a deposition with `--category deposition --calendar-role firm --authority-kind internal`. Supply `--due-at`,
`--hearing-minutes`, `--hearing-location`, `--deponent-role`, and `--taken-by`. Review the proposed preparation and
rerun with `--confirm` to save. Supply an edited JSON array with `--prep-file`. An empty array drops all suggestions.
These suggestions are internal preparation dates. Use a cited court date proposal for a date fixed by a rule or order.

### Cite a file in Forgejo

The optional `forgejo` field identifies the file at a full commit SHA. Choose `project` for the matter repository or
`firm_library` for its owning [Firm Library](glossary/firm-library.md). Navigator checks that the file contains the
archived bytes or a document pointer to that exact version. The citation records the repository identity, commit, path,
archived version, page and highlights.

Reviewers can open the highlighted original from Navigator even if the repository is offline or the file changes later.
**Open in Forgejo** opens the verified file at the cited commit when it is available. Access to the archived page still
follows Project participation, Authority permissions and document retention.

## Review and publish

Open **Court date review** from the Project's Deadlines tab. Select a row to inspect the proposed date and its
highlighted source pages. Open a crop to see the entire page. The review screen also shows the calculation for a derived
date. A lawyer can confirm the displayed revision or return it with a reason. A supervised clerk can see returned work
in their Project overview.

Use J and K to move, R to open the record, A to open the Authority, and Escape to close the source. Command+Enter on
macOS or Ctrl+Enter confirms the selected revision. The question-mark shortcut lists the available keys.

To revise a returned row, save the corrected draft object in a private file and use its current revision.

```bash
navigator project deadline review correct <proposal-id> --revision <revision> --file /private/path/draft.json
```

DBOS publishes the confirmed revision to the stored Deadline. Calendar sync and ICS use that record. Confirmation
records the reviewing lawyer, time and exact evidence revision. A corrected date retains its Deadline identity in
calendar exports.

The existing `navigator project deadline propose` calculation command can preview a filed rule and its trigger. Use the
queue to publish a court date. Direct Deadline creation remains available for internal and contractual obligations.
