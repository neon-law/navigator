---
kind: memo
title: Uncontested Divorce Filing Checklist
respondent_type: person
jurisdiction: NY
confidential: true
prompts:
  marriage_date: When and where were you married?
  county_of_residence: In which county will the action be filed?
  minor_children: Are there minor children of the marriage?
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: person__spouse
  person__spouse:
    _: custom_datetime__marriage_date
  custom_datetime__marriage_date:
    _: custom_text__county_of_residence
  custom_text__county_of_residence:
    _: custom_yes_no__minor_children
  custom_yes_no__minor_children:
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

# Uncontested divorce filing checklist

**Plaintiff.** {{person__client.name}}

**Defendant.** {{person__spouse.name}}

**Married.** {{custom_datetime__marriage_date}}

**Filing county.** {{custom_text__county_of_residence}}

**Minor children.** {{custom_yes_no__minor_children}}

The Firm works these steps in order for one couple who agree. The court controls its schedule and decides whether to
grant the divorce; each step asks, it does not promise. The lawyer marks each step as the record confirms it.

1. **Purchase the index number.** The clerk assigns the index number the caption prints from filing forward.
2. **File the summons** — with notice alone, or with the verified complaint — and pay the fee.
3. **Serve the defendant** within the service window the rule clock records, and have the defendant sign the affidavit
   of defendant.
4. **Execute the settlement agreement** once every term is agreed and both spouses have signed.
5. **Submit the findings of fact and conclusions of law** and the proposed judgment with the index number in the
   caption.
6. **Request the certificate of dissolution** with the judgment submission.
7. **Note the entry of the judgment** and deliver the signed copy to each spouse.

A step that changes — service by a method other than personal delivery, an amended pleading — updates the rule-clock
rows on the matter, not this checklist.
