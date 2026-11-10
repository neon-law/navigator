---
jurisdiction: NV
practice_area: document_review
name: Nevada Document Review Playbook
version: "1"
notations:
  - document_review_memo__nevada
assumptions:
  - plan.target_minutes.document_review
---

# Nevada Document Review Playbook

This Kit governs one document review for one Person's plan matter under Nevada law. The Firm reads the document the
Person received, records what it actually says, and answers in writing. Defined terms carry their glossary meanings: the
client is a Person, the matter a Project, a work product a Notation, a docketed date a Deadline, and the Firm is the
Firm.

The Kit carries no Firm numbers. Where the playbook plans effort, it renders the Firm's own Assumption for the key
declared above. The field stays blank until the Firm sets a value in the Firm Assumptions catalog.

## Intake

The Firm asks the Person six questions and records the answers before it reads anything into the record.

1. What is the document, and who sent it?
2. On what date did the Person receive it?
3. In the Person's own words, what does it say or ask for?
4. What does it demand of the Person, and by what date? "Nothing" is an answer.
5. Does a legal plan cover this Project?
6. What does the Person want — advice, negotiation, an answer to a demand, or a decision on signing?

## Deadlines

The Firm records two Deadlines at intake, before analysis. Navigator stores a date; it never derives one. The lawyer
computes each window from the intake answers and the plan agreement and records the instant in America/Los_Angeles.

| Deadline | Window | Authority | Role | Category |
| --- | --- | --- | --- | --- |
| Review turnaround | The plan agreement's review window | Plan agreement | firm | internal |
| Person's response due | The date the document names, if any | The reviewed document | firm | internal |

The command records one row; the Firm runs it once per row:

```bash navigator project deadline create
  --title "Plan review turnaround"
  --due-at <RFC 3339 instant>
  --authority-kind contract
  --authority "Legal plan agreement"
  --calendar-role firm
  --category internal
  --source lawyer ```

A demand's date the Person disputes, or an amended document, changes the rows, not the Kit: the lawyer updates the row
in place with `navigator project deadline update`.

## Deliverables

The Kit scaffolds one Notation onto the Project.

1. **Document review memo.** `document_review_memo__nevada`. The Firm plans {{ plan.target_minutes.document_review }}
   attorney minutes for the review, tests the document's conduct against NRS 598.0915's deceptive-trade-practice
   provisions and the limitations period that fits, and reserves the recommendation for the lawyer.

When the Person wants a written answer sent, the letters kit for this jurisdiction scaffolds `plan_letter__nevada` onto
the same Project.

## Scope limits

- The Kit covers one document for one Person. A second document is a second review, on its own Project or an amended
  plan, and the Firm reopens intake when it learns of one.
- The review answers in writing. Negotiating revisions, contesting a demand in court, and defending a lawsuit are new
  engagements the plan does not cover.
- The Firm confirms every limitation period against primary sources at review and records what it confirms.

## References

- [Kit](../../docs/glossary/kit.md) — what a Kit is and how a Project pins one.
- [Notation](../../docs/glossary/notation.md) — what the Firm scaffolds and renders.
- [Notation authoring](../../docs/notation-authoring.md) — the rules a bundled template must pass.
- [Public contributor safety](../../docs/public-contributor-safety.md) — what never enters a template.
