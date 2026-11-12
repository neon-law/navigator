---
kind: filing
title: Summons With Notice
respondent_type: person
jurisdiction: NY
confidential: true
prompts:
  index_number: What index number has the clerk assigned?
  county_of_residence: In which county will the action be filed?
  marriage_date: On what date were you married?
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: person__spouse
  person__spouse:
    _: custom_text__county_of_residence
  custom_text__county_of_residence:
    _: custom_text__index_number
  custom_text__index_number:
    _: custom_datetime__marriage_date
  custom_datetime__marriage_date:
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

## SUMMONS WITH NOTICE — Action for divorce

**Plaintiff.** {{person__client.name}}

**Defendant.** {{person__spouse.name}}

**Index No.** {{custom_text__index_number}}

**Date of marriage.** {{custom_datetime__marriage_date}}

## Nature of action

This is an action for divorce, brought under the Domestic Relations Law. The ground asserted is that the relationship
between the parties has broken down irretrievably for a period of six months or more, as stated in the complaint or in
the plaintiff's sworn statement, and the issues the court may reach on the papers are resolved by the parties'
settlement agreement or outside the action.

## Notice

**TO THE DEFENDANT:** You are hereby summoned to respond within twenty days after service of this summons, or within
thirty days after service is complete by any other method the Civil Practice Law and Rules allow. You are not required
to appear in court in person. If you fail to respond, judgment may be entered against you for the relief demanded in the
notice.

The lawyer of record confirms, before filing, that the notice states only the relief the engagement covers and that the
service method matches the window the Firm recorded on the rule clock.
