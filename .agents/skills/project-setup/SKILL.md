---
name: project-setup
description: Open a Project, generate its canonical repository shell, and set up its Project surfaces.
---

# Set up a Project

Find the term first with `navigator glossary list` and `navigator glossary show "<Term>"` before adding vocabulary. Use
the current `navigator` release. The scaffold embeds canonical files and skills from that release; do not copy them from
another checkout.

## Open and scaffold a new Project

From an empty destination, run:

```bash
navigator project scaffold --dir <directory> [--host <host>]
```

The command asks for the Project name and immutable code, client contact, Entity or client jurisdiction, then conflict
attestation. It uses the saved Navigator login as the lawyer DRI. Confirm the client and Entity details before
attesting. A code already in use is refused. For an individual client without an existing Entity, give the client's home
jurisdiction; Navigator validates it and creates a Human Entity there. Existing contacts and Entities are reused.

The command opens the Project through the authenticated deployment API, creates a missing client contact when needed,
and writes the canonical `navigator.yaml`, `AGENTS.md`, workflows, license, `CODEOWNERS`, and synced skills from this
CLI release. It initializes Git and runs the generated-project gate. It does not create application or notation-template
content. Cancel before answering yes to the conflict attestation to leave the Project and destination unwritten.

## Set up the Project surfaces

After the Project is open, run:

```bash
navigator project setup <code>
```

The caller needs the Project's assigned lawyer DRI authorization and a saved Navigator login. The command reconciles the
Drive, repository, Slack, Notion, and calendar surfaces and reports each resource separately. Re-running setup converges
after partial failures. A Project repository commits only source, so there is no Portal publisher to provision.

The `calendar` resource is the Project's shared Google Calendar on the Firm's Workspace, named for the Project code.
Setup creates it when none is recorded, records its id on the Project, and shares it with every firm-side participant,
never the client. Record the id in `navigator.yaml` with `navigator project set --from-live` (or `--calendar <id>`).
When the Project closes, the calendar becomes read-only for its participants and the booking link is cleared.

## Client booking link

The Calendar API cannot create appointment schedules, so the client booking link is one manual step per Project:

1. In Google Calendar on the web, create an appointment schedule for the lawyer, and choose the Project calendar as the
   calendar bookings are saved to.
2. Copy the schedule's booking page address.
3. Record it: `navigator project set --booking-url <address>`.

The matter page shows the link to the client as "Book a time". Clear it with `--booking-url ""`.

For the fleet, a repository adopts this skill and the other synced skills when it updates its gate to a release that
includes them. The repository gate requires synced skill files to match that release byte-for-byte.

## Matter folders

`navigator project sync` gives every Project checkout the same Git-ignored `documents/` tree, even when a folder is
empty:

| Folder | Holds | Default for `kind` |
| --- | --- | --- |
| `Client Production/` | What the client supplied | `exhibit` |
| `Correspondence (Served)/` | Letters served or received | `letter` |
| `Discovery (Served)/` | Discovery served or received | none: set `folder:` |
| `Docket Filings/` | Court paper and docket ingest | `pleading`, `filing` |
| `Court Rules/` | Local rules and standing orders | none |
| `Engagement/` | The letters that open and close the engagement | `onboarding` |
| `Work Product/` | Drafts, research, everything else | every other kind |

`Work Product/` holds the same seven fixed subfolders in every checkout, each suffixed so it reads unambiguously out of
context and never collides with a served twin (LAW-215): `Client Calls (Work Product)/` (`transcript`), `Correspondence
(Work Product)/` (letters drafted, not served), `Discovery (Work Product)/` (discovery in preparation), `Motions (Work
Product)/` (one folder per motion), `Pleadings (Work Product)/`, `Research (Work Product)/` (`memo`), and `Stipulations
(Work Product)/`.

Git tracks only the pointers under `blobs/`. To file a document, drop it into its matter folder and run `navigator
project sync`, or run `navigator site document upload --folder "<Folder>"` — a `Work Product` subfolder's name is its
whole path below `documents/`, such as `"Work Product/Research (Work Product)"`. `onboarding/`, `offboarding/`,
`invoices/`, `cases/`, and `rules/` stay beside the matter folders.

A checkout from before the matter folders moves on the next `navigator project sync`, or with `navigator project
repository migrate-documents`. A checkout still holding the bare `Discovery/` folder of the five-folder tree refiles
with `navigator project repository migrate-folders`. Neither move leaves a pointer's checksum or asset id changed.

## Verify

Run the read-only Project checks from the repository root:

```bash
navigator project doctor --project <code>
navigator project gate
```

`doctor` checks the local machine and Project coordinates. `gate` validates repository structure and generated files.
The Project code is immutable. Correct a mistaken code through the lifecycle and data-governance process. Do not rename
the repository or manifest independently.
