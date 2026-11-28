---
kind: pleading
title: Civil Complaint (California)
caption_title: COMPLAINT
footer_title: COMPLAINT
jurisdiction: CA
respondent_type: person
confidential: true
prompts:
  pleading_venue_facts: What facts establish jurisdiction and venue?
  pleading_allegations: What material facts support the complaint?
  pleading_cause_of_action: What cause of action is asserted, by whom, and against whom?
  pleading_supporting_allegations: What allegations support this cause of action?
  pleading_relief: What relief does the plaintiff request?
audiences:
  pleading_venue_facts: lawyer
  pleading_allegations: lawyer
  pleading_cause_of_action: lawyer
  pleading_supporting_allegations: lawyer
  pleading_relief: lawyer
questionnaire:
  BEGIN:
    _: custom_text__pleading_venue_facts
  custom_text__pleading_venue_facts:
    _: custom_text__pleading_allegations
  custom_text__pleading_allegations:
    _: custom_text__pleading_cause_of_action
  custom_text__pleading_cause_of_action:
    _: custom_text__pleading_supporting_allegations
  custom_text__pleading_supporting_allegations:
    _: custom_text__pleading_relief
  custom_text__pleading_relief:
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

# COMPLAINT

## 1. Jurisdiction and venue

{{custom_text__pleading_venue_facts}}

## 2. General allegations

{{custom_text__pleading_allegations}}

## 3. Cause of action

{{custom_text__pleading_cause_of_action}}

{{custom_text__pleading_supporting_allegations}}

## 4. Prayer for relief

Plaintiff requests the following relief:

{{custom_text__pleading_relief}}

<!-- keep -->

<!-- counsel-signature -->

<!-- /keep -->
