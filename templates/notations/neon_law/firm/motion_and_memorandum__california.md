---
kind: pleading
title: Motion and Supporting Memorandum (California)
caption_title: NOTICE OF MOTION AND MOTION; MEMORANDUM OF POINTS AND AUTHORITIES
footer_title: MOTION AND SUPPORTING MEMORANDUM
short_caption: true
jurisdiction: CA
respondent_type: person
confidential: true
prompts:
  motion_hearing_details: At what date, time, and location will the motion be heard?
  motion_moving_party: Which parties are bringing the motion, as they appear in the caption?
  motion_addressed_to: Which parties are addressed by the motion, as they appear in the caption?
  motion_order_requested: What order does the moving party request?
  motion_grounds: What grounds support the motion?
  motion_facts: What material facts and evidence support the motion?
  motion_argument: What law and reasoning support the requested order?
audiences:
  motion_hearing_details: lawyer
  motion_moving_party: lawyer
  motion_addressed_to: lawyer
  motion_order_requested: lawyer
  motion_grounds: lawyer
  motion_facts: lawyer
  motion_argument: lawyer
questionnaire:
  BEGIN:
    _: custom_text__motion_hearing_details
  custom_text__motion_hearing_details:
    _: custom_text__motion_moving_party
  custom_text__motion_moving_party:
    _: custom_text__motion_addressed_to
  custom_text__motion_addressed_to:
    _: custom_text__motion_order_requested
  custom_text__motion_order_requested:
    _: custom_text__motion_grounds
  custom_text__motion_grounds:
    _: custom_text__motion_facts
  custom_text__motion_facts:
    _: custom_text__motion_argument
  custom_text__motion_argument:
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

# NOTICE OF MOTION AND MOTION

## 1. Notice of motion and motion

To {{custom_text__motion_addressed_to}} and their counsel of record:

At {{custom_text__motion_hearing_details}}, {{custom_text__motion_moving_party}} will move on the following grounds:

{{custom_text__motion_grounds}}

The moving party requests this order:

{{custom_text__motion_order_requested}}

## 2. Memorandum of points and authorities

### A. Statement of facts

{{custom_text__motion_facts}}

### B. Argument

{{custom_text__motion_argument}}

### C. Conclusion

For these reasons, the moving party requests the order identified in the notice of motion.

<!-- keep -->

<!-- counsel-signature -->

<!-- /keep -->
