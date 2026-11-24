---
name: today
description: Give a read-only brief of today's Navigator deadlines, calendar, and priorities. Use for /today.
---

# Today

Use the lawyer's local date and IANA time zone. Honor the requested Project or host scope. For a workspace-wide brief,
use the production hosts identified in the workspace instructions or top-level Project manifests. Include staging only
when requested. Read under the lawyer's existing Navigator session for each host.

## Read the agenda

Check the installed command's help before choosing flags.

- For each in-scope host, run `navigator calendar --date <YYYY-MM-DD> --timezone <zone> --host <host> --json` when
  supported. Read `entries` and `overdue`, and check Deadline and calendar coverage independently.
- Add `--project <code>` for a single Project. For that scope or a CLI without the dated agenda, run
  `navigator project deadline brief --due-by <YYYY-MM-DD> --timezone <zone> --host <host> --json`.
- For a workspace-wide brief on older releases, use `navigator calendar events` with `--host`, `--start` and `--end`
  when supported. Pass consecutive local midnights in RFC 3339 for the bounds.
- Keep successful results when another read fails. A nonzero exit can come with useful JSON and incomplete coverage.
  Report unreadable hosts, Projects, and calendars. Say a category is empty only for the scope that was read.
- Deduplicate Deadline IDs across hosts and preserve the original Project, Deadline and source links. Distinct rows
  with the same title remain distinct. Collapse repeated calendar events when their event ID and start time match.

## Present the brief

Lead with hearings and the Firm's court, statutory, and contract work due today or still open past the recorded time.
Flag unverified dates, placeholder times, and missing authority when relevant. Use recorded times and sources. Leave
legal date calculations and confirmation to the lawyer. Label opponent obligations so the lawyer can see what to watch.

Then list today's calendar events and internal work, followed by older open items. Group repeated operational work, such
as release follow-ups, with a count and keep individual record links available. Show the date, time zone and coverage.
Keep the brief in the lawyer's session.

Keep `/today` read-only. The Gmail MCP connector is the only path for Gmail mailbox operations. Agents never send email.
Every composed email remains an unsent draft for the lawyer to send personally.
