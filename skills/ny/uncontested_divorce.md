---
jurisdiction: NY
practice_area: uncontested_divorce
name: New York Uncontested Divorce Playbook
version: "1"
notations:
  - divorce_filing_checklist__new_york
  - summons_with_notice__new_york
  - verified_complaint__new_york
  - affidavit_of_defendant__new_york
  - settlement_agreement__new_york
  - findings_of_fact__new_york
  - judgment_of_divorce__new_york
assumptions:
  - plan.target_minutes.uncontested_divorce
---

# New York Uncontested Divorce Playbook

This Kit governs one uncontested divorce for one couple who agree under New York law — "a brighter way through divorce
for couples who agree." The Firm represents the couple jointly only where joint representation is permitted, after
conflict review and informed written consent; each spouse understands the choices and the documents before signing.
Defined terms carry their glossary meanings: the client is a Person, the matter a Project, a work product a Notation, a
docketed date a Deadline, and the Firm is the Firm.

The Kit carries no Firm numbers. The one-time fee for a mutual split lives in the Firm's plan fee schedule and the
written retainer, never in the Kit. Where the playbook plans effort, it renders the Firm's own Assumption for the key
declared above; the field stays blank until the Firm sets a value in the Firm Assumptions catalog.

## Intake

The Firm asks the couple seven questions and records the answers before it drafts anything.

1. Where are you, what have you agreed on, and has either of you started a case? (No case yet is the normal answer.)
2. When and where were you married?
3. Where does each spouse live? The venue county is a county where a party resides.
4. Are there minor children of the marriage? If so, who are they?
5. Have you agreed on the property, the debts, and any support? What is still open?
6. Will one spouse pay maintenance to the other?
7. Does a legal plan cover this Project?

If agreement changes — if a conflict prevents the Firm from continuing for both spouses, the Firm stops the joint work
and explains the need for separate counsel; it never takes one spouse's side against the other in this engagement.

## Deadlines

The Firm records four Deadlines at intake, before drafting. Navigator stores a date; it never derives one. The lawyer
computes each window from the intake answers and the rules below and records the instant in America/New_York.

| Deadline | Window | Authority | Role | Category |
| --- | --- | --- | --- | --- |
| Service completed | 120 days after the summons is filed | CPLR § 306-b | firm | internal |
| Defendant's affidavit returned | The appearance window after personal service | CPLR § 320 | firm | internal |
| Findings and judgment submitted | The plan agreement's turnaround | Plan agreement | firm | internal |
| Certificate of dissolution requested | With the judgment submission | Court rule | firm | internal |

The command records one row; the Firm runs it once per row:

```bash navigator project deadline create
  --title "Service completed"
  --due-at <RFC 3339 instant>
  --authority-kind statute
  --authority "CPLR 306-b"
  --calendar-role firm
  --category internal
  --source lawyer ```

A service date the spouses dispute, or an amended pleading, changes the rows, not the Kit: the lawyer updates the row
in place with `navigator project deadline update`.

## Deliverables

The Kit scaffolds seven Notations onto the Project — the practice's own papers, carried into the Kit so the public
pages and the matter run from one set of documents.

1. **Filing checklist.** `divorce_filing_checklist__new_york`. The Firm's ordered list of the filing steps, from the
   index number to the certificate of dissolution, with the Deadlines above marked.
2. **Summons with notice.** `summons_with_notice__new_york`. The first filing route: a summons that states the nature
   of the action without a complaint.
3. **Verified complaint.** `verified_complaint__new_york`. The second filing route: the complaint verified by the
   plaintiff-spouse, stating the residence, the ground, the children, and the prior proceedings.
4. **Affidavit of defendant.** `affidavit_of_defendant__new_york`. The defendant-spouse's signed acknowledgment that
   service was received and the action is not contested.
5. **Settlement agreement.** `settlement_agreement__new_york`. The couple's agreed terms assembled from the Clause
   library — property and debts, maintenance, parenting and child support — with the schedules the intake answers
   fill.
6. **Findings of fact and conclusions of law.** `findings_of_fact__new_york`. The findings supporting the judgment,
   reciting the marriage, the residence, the ground, and the agreement.
7. **Judgment of divorce.** `judgment_of_divorce__new_york`. The decree the court signs, incorporating the agreement
   without merging it and directing the certificate of dissolution.

The Firm plans {{ plan.target_minutes.uncontested_divorce }} attorney minutes for the engagement's plan work. The
ground is the no-fault ground: the relationship has broken down irretrievably for a period of six months or more, and
the ancillary issues are resolved by the agreement. The lawyer verifies every statutory reference against primary
sources at drafting and records what it confirms.

## Scope limits

- The Kit covers one couple who agree. A contested proceeding is outside the joint engagement — the Firm stops the
  joint work and explains the need for separate counsel.
- The Kit does not include a name change. A judgment that restores a former name is a separate drafting decision the
  lawyer makes outside this Kit.
- The Kit does not set fees. The fee lives in the plan fee schedule; the Kit renders no Firm number.
- The court controls its schedule and decides whether to grant the divorce. The Firm's papers ask; they do not promise.

## References

- [Kit](../../docs/glossary/kit.md) — what a Kit is and how a Project pins one.
- [Notation](../../docs/glossary/notation.md) — what the Firm scaffolds and renders.
- [Clause](../../docs/glossary/clause.md) — how the settlement agreement assembles its wording.
- [Notation authoring](../../docs/notation-authoring.md) — the rules a bundled template must pass.
- [Public contributor safety](../../docs/public-contributor-safety.md) — what never enters a template.
