---
kind: filing
title: Judgment of Divorce
respondent_type: person
jurisdiction: NY
confidential: true
prompts:
  index_number: What index number has the clerk assigned?
  marriage_date: On what date were you married?
  place_of_marriage: Where were you married (city and state)?
  county_of_residence: In which county will the action be filed?
  served_on: On what date was the defendant served?
  maintenance_applies: Will one spouse pay maintenance to the other?
  entry_date: On what date should the proposed judgment be dated?
  presiding_justice: Who is the presiding justice for the proposed judgment?
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: person__spouse
  person__spouse:
    _: custom_datetime__marriage_date
  custom_datetime__marriage_date:
    _: custom_text__place_of_marriage
  custom_text__place_of_marriage:
    _: custom_text__county_of_residence
  custom_text__county_of_residence:
    _: custom_text__index_number
  custom_text__index_number:
    _: custom_datetime__served_on
  custom_datetime__served_on:
    _: custom_yes_no__maintenance_applies
  custom_yes_no__maintenance_applies:
    _: custom_datetime__entry_date
  custom_datetime__entry_date:
    _: custom_text__presiding_justice
  custom_text__presiding_justice:
    _: END
  END: {}
workflow:
  BEGIN:
    _: lawyer_review
  lawyer_review:
    approved: END
    changes_requested: reask__client
  reask__client:
    intake_resubmitted: lawyer_review
  END: {}
---

# Supreme Court of the State of New York

**County of {{custom_text__county_of_residence}}**

## JUDGMENT OF DIVORCE

{{person__client.name}}, Plaintiff,

against {{person__spouse.name}}, Defendant.

**Index No.** {{custom_text__index_number}}

Upon the summons and the verified complaint, or the summons with notice; the plaintiff's sworn statement of the
irretrievable breakdown of the relationship; the affidavit of defendant dated after service on
{{custom_datetime__served_on}}; and the parties' settlement agreement —

## It is ordered and adjudged that

1. The parties married on {{custom_datetime__marriage_date}}.
2. The marriage took place at {{custom_text__place_of_marriage}}. The court has jurisdiction over the parties and
   the subject of the action, and venue lies in this county.
3. The relationship between the parties has broken down irretrievably for a period of six months or more. The ground
   for divorce under the no-fault provision of the Domestic Relations Law is established.
4. The marriage between the parties is dissolved.
5. The parties' settlement agreement is incorporated, without being merged, into this judgment. Its terms survive as
   a contract between the parties and may be enforced as the law allows.

{{#if custom_yes_no__maintenance_applies=true}}

6. Maintenance runs as the settlement agreement's schedule records.

{{else if custom_yes_no__maintenance_applies=false}}

6. Each spouse waives maintenance.

{{/if}}

7. The parties' minor children, if any, remain the joint custodial children of the parties, and the parenting and
   child-support terms the settlement agreement records govern.
8. The certificate of dissolution the law requires is issued with the entry of this judgment.
9. This judgment is entered on default of the defendant's appearance, on the defendant's own affidavit consenting to
   entry on the papers.

Dated: {{custom_datetime__entry_date}}

Justice of the Supreme Court: {{custom_text__presiding_justice}}

The lawyer of record confirms the recitals and the ordering clauses against the entered papers before submission.
