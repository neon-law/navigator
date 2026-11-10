---
kind: filing
title: Findings of Fact and Conclusions of Law
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
  minor_children: Are there minor children of the marriage?
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
    _: custom_yes_no__minor_children
  custom_yes_no__minor_children:
    _: schedule__children
  schedule__children:
    _: custom_yes_no__maintenance_applies
    columns:
      - name: child
        label: Child's name
        type: text
      - name: birth_date
        label: Date of birth
        type: date
  custom_yes_no__maintenance_applies:
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

## FINDINGS OF FACT AND CONCLUSIONS OF LAW

{{person__client.name}}, Plaintiff,

against {{person__spouse.name}}, Defendant.

**Index No.** {{custom_text__index_number}}

The court, on the pleadings, the papers, and the plaintiff's sworn statement, makes the following findings:

## Findings of fact

1. The parties married on {{custom_datetime__marriage_date}}.
2. The marriage took place at {{custom_text__place_of_marriage}}.
3. Plaintiff has resided in the State of New York for the period the Domestic Relations Law's residence requirements
   state, continuously, before bringing this action.
4. Defendant was served on {{custom_datetime__served_on}} and signed the affidavit of defendant. Defendant does not
   appear and does not contest the action.
5. The relationship between the parties has broken down irretrievably for a period of six months or more. The
   plaintiff states this under the penalties of perjury, and the defendant does not dispute it.
6. Minor children are issue of the marriage: {{custom_yes_no__minor_children}}

{{#for child in schedule__children}}

- {{child.child}}, born {{child.birth_date}}.

{{/for}}

7. The parties executed a settlement agreement resolving the property, the debts, the support, and the parenting of
   their children, if any. The agreement provides:

{{#if custom_yes_no__maintenance_applies=true}}

One spouse pays maintenance to the other as the agreement's schedule records.

{{else if custom_yes_no__maintenance_applies=false}}

Each spouse waives maintenance.

{{/if}}

## Conclusions of law

1. The court has jurisdiction over the parties and the subject of the action, and venue lies in the county above.
2. The plaintiff has met the residence requirements the Domestic Relations Law states.
3. The no-fault ground is established: the relationship has broken down irretrievably for a period of six months or
   more, and the issues the court may reach on the papers are resolved by the parties' agreement or outside the action.
4. The settlement agreement is not contested. It survives as a contract and is incorporated, without being merged,
   into the judgment the court will enter.

The lawyer of record confirms each finding against the matter record and the primary sources before submission.
