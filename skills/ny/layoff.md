---
jurisdiction: NY
practice_area: layoff
name: New York Layoff Playbook
version: "1"
notations:
  - severance_review_memo__new_york
  - non_compete_analysis__new_york
  - severance_negotiation_letter__new_york
  - unemployment_appeal_letter__new_york
  - layoff_next_steps__new_york
assumptions:
  - plan.target_minutes.severance_review
---

# New York Layoff Playbook

This Kit governs one Person's separation from an Employer under New York law. The Firm reviews the separation agreement,
analyzes any restrictive covenant, negotiates in writing, and protects the Person's unemployment benefits. Defined terms
carry their glossary meanings: the client is a Person, the matter a Project, a work product a Notation, a docketed date
a Deadline, and the Firm is the Firm.

The Kit carries no Firm numbers. Where the playbook plans effort, it renders the Firm's own Assumption for the key
declared above. The field stays blank until the Firm sets a value in the Firm Assumptions catalog.

## Intake

The Firm asks the Person seven questions and records the answers before it analyzes anything.

1. What is the Employer's legal name?
2. What is the Person's last day of employment?
3. What did the Employer hand over — the separation agreement, any form release, the WARN notice, the benefits
   notice, the final pay statement?
4. By what date does the Employer require the signed agreement?
5. Is the Person forty years of age or older? The Older Workers Benefit Protection Act windows turn on the answer.
6. Does a legal plan cover this Project?
7. What does the Person want — negotiation, acceptance, a contest, or advice only?

## Deadlines

The Firm records three Deadlines at intake, before analysis. Navigator stores a date; it never derives one. The lawyer
computes each window from the intake answers and records the instant in America/New_York.

| Deadline | Window | Authority | Role | Category |
| --- | --- | --- | --- | --- |
| Consideration closes | 21 days; 45 under a group program | 29 U.S.C. § 626(f)(1)(F) | firm | internal |
| Revocation closes | 7 days after signing | 29 U.S.C. § 626(f)(1)(G) | firm | internal |
| Unemployment hearing | 30 days from the mailing or delivery | N.Y. Labor Law § 620(1)(a) | firm | court |

The consideration window runs from the final offer; the unemployment window runs from the mailing or delivery of the
determination. The command records one row; the Firm runs it once per row:

```bash
navigator project deadline create \
  --title "Consideration period closes" \
  --due-at <RFC 3339 instant> \
  --authority-kind statute \
  --authority "29 U.S.C. § 626(f)(1)(F)(i)" \
  --calendar-role firm \
  --category internal \
  --source lawyer
```

An extended or withdrawn offer changes the rows, not the Kit: the lawyer updates the row in place with `navigator
project deadline update`. A referee's decision after a hearing opens a shorter appeal to the Unemployment Insurance
Appeal Board under Labor Law § 621, and the Firm records that row only when the decision exists.

## Deliverables

The Kit scaffolds five Notations onto the Project.

1. **Severance review memo.** `severance_review_memo__new_york`. The Firm plans
   {{ plan.target_minutes.severance_review }} attorney minutes for the review.
2. **Non-compete analysis.** `non_compete_analysis__new_york`. The Firm tests the covenant's time, area, and
   purpose against New York's reasonableness standard before the Person signs anything.
3. **Negotiation letter.** `severance_negotiation_letter__new_york`. The Firm states its requested revisions and a
   response date in one letter.
4. **Unemployment claim and appeal.** `unemployment_appeal_letter__new_york`. The Firm answers an adverse
   determination with a timely hearing request.
5. **Next-step one-pager.** `layoff_next_steps__new_york`. It tells the Person what happens next and points at the
   Firm's LLC formation kit when the plan is self-employment.

## Scope limits

- The Kit covers one separation for one Person. A group program changes the consideration window and adds the
  Employer's disclosure duties; the Firm reopens intake when it learns of one.
- The Firm negotiates in writing and files administrative claims. A lawsuit is a new engagement.
- The Firm confirms every window against primary sources at review and records what it confirms.

## References

- [Kit](../../docs/glossary/kit.md) — what a Kit is and how a Project pins one.
- [Notation](../../docs/glossary/notation.md) — what the Firm scaffolds and renders.
- [Notation authoring](../../docs/notation-authoring.md) — the rules a bundled template must pass.
- [Public contributor safety](../../docs/public-contributor-safety.md) — what never enters a template.
