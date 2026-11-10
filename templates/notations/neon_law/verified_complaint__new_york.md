---
kind: pleading
title: Verified Complaint
respondent_type: person
jurisdiction: NY
confidential: true
prompts:
  index_number: What index number has the clerk assigned?
  marriage_date: On what date were you married?
  place_of_marriage: Where were you married (city and state)?
  county_of_residence: In which county will the action be filed?
  prior_proceedings: Were there prior matrimonial proceedings between the spouses?
  minor_children: Are there minor children of the marriage?
  verification_date: On what date will the plaintiff verify the complaint?
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
    _: custom_text__prior_proceedings
  custom_text__prior_proceedings:
    _: custom_yes_no__minor_children
  custom_yes_no__minor_children:
    _: schedule__children
  schedule__children:
    _: custom_datetime__verification_date
    columns:
      - name: child
        label: Child's name
        type: text
      - name: birth_date
        label: Date of birth
        type: date
  custom_datetime__verification_date: { _: END }
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

## VERIFIED COMPLAINT

{{person__client.name}}, Plaintiff,

against {{person__spouse.name}}, Defendant.

**Index No.** {{custom_text__index_number}}

Plaintiff, by the Firm as counsel, complaining of the Defendant, alleges:

## 1. The parties

1. Plaintiff resides in the State of New York.
2. Defendant resides in the State of New York.
3. The marriage occurred on {{custom_datetime__marriage_date}}.
4. It took place at {{custom_text__place_of_marriage}}. Neither party is a member of the armed forces.

## 2. Venue

5. The action is brought in the County of {{custom_text__county_of_residence}}, a county in which a party resided at
   the time the action was commenced and in which either party has resided for the period the venue rules require.

## 3. The ground

6. No prior action for divorce, separation, or annulment between the parties is pending or was decided, except as the
   answer to the last question records: {{custom_text__prior_proceedings}}
7. The relationship between the parties has broken down irretrievably for a period of six months or more. This
   statement is made subject to the penalties of perjury under the law of New York.

## 4. The children

8. {{custom_yes_no__minor_children}} — minor children are issue of the marriage:

{{#for child in schedule__children}}

- {{child.child}}, born {{child.birth_date}}.

{{/for}}

The parties' parenting and child-support terms are stated in their settlement agreement, which this action asks the
court to incorporate without merging.

## 5. The relief demanded

9. Wherefore, Plaintiff demands judgment dissolving the marriage between the parties, directing the certificate of
   dissolution the law requires, incorporating the parties' settlement agreement without merging it into the judgment,
   and granting the Firm such other and further relief as the court finds just.

Dated: {{custom_datetime__verification_date}}

The lawyer of record confirms the verification block, the service address, and the ground statement against the matter
record before filing.
