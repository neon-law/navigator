---
name: bulk-contact-upload
description: >
  Turn a raw list of organizations and the people who work at them into a valid Neon Law Navigator bulk-import payload,
  then load it through the `bulk_import` MCP tool, which calls the shared `import` engine. Use this when someone hands
  you a contact list (names, emails, titles, phone numbers, org names) and wants those people and their organizations
  created as `persons` and `entities` with the links between them. Trigger on "bulk import these contacts", "load this
  list of people and orgs", or "create these as clients", or when Navigator MCP is asked to add several people at once.
  Skip for a single person (use `create_person`) or for opening a matter or Project (the per-engagement onboarding
  flow).
---

# Bulk contact upload

Produce a **version-1 bulk-import payload** from the raw list and load it. The contract is in
[`docs/bulk-contact-import.md`](../../../docs/bulk-contact-import.md); read it if anything here is ambiguous.

## 1. Emit the payload

Output one JSON object with `organizations` and `people`. Wire each person to their organization with a stable,
human-readable kebab-case `key`; keys live only in the file.

```json
{
  "version": 1,
  "source": "<short provenance slug, e.g. partner-outreach-2026-06>",
  "organizations": [
    {
      "key": "ejp",
      "name": "Example Justice Project",
      "entity_type": "501(c)(3) Non-Profit",
      "jurisdiction": "WA",
      "phone": "206-555-0142",
      "url": "https://justice.example"
    }
  ],
  "people": [
    {
      "key": "ada-counsel",
      "name": "Ada Counsel",
      "email": "acounsel@justice.example",
      "title": "Executive Director",
      "phone": "206-555-0142",
      "organization": "ejp"
    }
  ]
}
```

## 2. Fill it in

- **One organization row per distinct organization.** People at the same org share one `organizations` entry and
  reference it by `key`.
- **`email` is required and unique** across the payload; it is the dedupe key. Drop or flag any contact without one.
- **`entity_type`** is almost always `501(c)(3) Non-Profit` for legal-aid and nonprofit orgs. For a company, use the
  matching type name (e.g. `Multi Member LLC`). The type must already exist as reference data; never invent one.
- **`jurisdiction`** is the two-letter code of the org's home or incorporation state. Prefer an explicit signal;
  otherwise infer it from the address or area code (206 → WA, 612 → MN, 312 → IL, 646 → NY).
- **`url`** is the organization's canonical `https://` website. Derive it from the email domain when not given
  (`acounsel@justice.example` → `https://justice.example`). Emit it clean: no tracking query params, no trailing slash.
- **`title`** and **`phone`** are optional. A shared switchboard number may appear as both the org `phone` and each
  person's `phone`.
- **Leave `entity_role` off** unless told otherwise; it defaults to `client_contact`.

## 3. Load it

Call the Navigator MCP tool `bulk_import` with the payload as the arguments. Lawyer or admin only; a client-tier or
anonymous caller is refused.

## 4. Read the report

The report lists each row as `created`, `updated`, `unchanged`, or `failed`. Re-running the same payload is safe and
reports `unchanged`. A `failed` row carries a reason, commonly an unknown `entity_type` or `jurisdiction` that needs
seeding first. Fix it and re-run; only the failed rows change.

Importing is a pure data upsert with no outbound side effects. Opening a matter, sending welcome emails, and
provisioning repositories are separate steps in the per-engagement onboarding flow.
