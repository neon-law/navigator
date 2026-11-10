---
jurisdiction: WA
practice_area: letters
name: Washington Letters Playbook
version: "1"
notations:
  - plan_letter__washington
assumptions:
  - plan.target_minutes.letter
---

# Washington Letters Playbook

This Kit governs the letters the Firm writes on one Person's behalf under Washington law as plan work. The Firm reviews
the subject, drafts the letter, and records the dates it names. Defined terms carry their glossary meanings: the client
is a Person, the matter a Project, a work product a Notation, a docketed date a Deadline, and the Firm is the Firm.

The Kit carries no Firm numbers. Where the playbook plans effort, it renders the Firm's own Assumption for the key
declared above. The field stays blank until the Firm sets a value in the Firm Assumptions catalog.

## Intake

The Firm asks the Person six questions and records the answers before it drafts anything.

1. What is the letter about, in one clause?
2. Who receives it — the legal name and the address of the other party?
3. What has the Person already put in writing to the other party?
4. By what date should the other party respond?
5. Does a legal plan cover this Project?
6. What does the Person want the letter to accomplish?

## Deadlines

The Firm records two Deadlines when the letter goes out. Navigator stores a date; it never derives one. The lawyer picks
each date and records the instant in America/Los_Angeles.

| Deadline | Window | Authority | Role | Category |
| --- | --- | --- | --- | --- |
| Response date passes | The date the letter names | The letter itself | firm | internal |
| Follow-up letter | The date the Firm picks if no response arrives | Plan agreement | firm | internal |

The command records one row; the Firm runs it once per row:

```bash navigator project deadline create
  --title "Response date passes"
  --due-at <RFC 3339 instant>
  --authority-kind internal
  --authority "The letter's stated response date"
  --calendar-role firm
  --category internal
  --source lawyer ```

A response that arrives, or a letter the Person revises, changes the rows, not the Kit: the lawyer completes or updates
the row in place with `navigator project deadline complete` or `navigator project deadline update`.

## Deliverables

The Kit scaffolds one Notation onto the Project.

1. **Plan letter.** `plan_letter__washington`. The Firm plans {{ plan.target_minutes.letter }} attorney minutes for the
   draft. The letter states the Person's ask, grounds it in the Consumer Protection Act, RCW 19.86, where the conduct
   fits, sets the response date, and waives nothing.

The letter assumes a completed document review. When no review is on the Project yet, the document-review kit for this
jurisdiction scaffolds `document_review_memo__washington` first.

## Scope limits

- The Kit covers correspondence. A court filing, an administrative claim, and a letter the recipient must answer under a
  statute's own procedure are new engagements the plan does not cover.
- The Firm prepares the letter for the lawyer's review; no letter leaves the Firm unsigned.
- The Firm confirms every citation against primary sources at drafting and records what it confirms.

## References

- [Kit](../../docs/glossary/kit.md) — what a Kit is and how a Project pins one.
- [Notation](../../docs/glossary/notation.md) — what the Firm scaffolds and renders.
- [Notation authoring](../../docs/notation-authoring.md) — the rules a bundled template must pass.
- [Public contributor safety](../../docs/public-contributor-safety.md) — what never enters a template.
