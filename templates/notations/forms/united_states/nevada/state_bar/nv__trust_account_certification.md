---
kind: filing
title: Nevada SCR 78 and SCR 217 Trust Account Certification of Compliance and Consent
respondent_type: person_and_entity
jurisdiction: NV
origin_url: https://nvbar.org/wp-content/uploads/Trust-IOLTA-Accounts-1.pdf
confidential: false
questionnaire:
  BEGIN:
    _: person__lawyer
  person__lawyer:
    _: custom_text__bar_number
  custom_text__bar_number:
    completed_by: signer
    _: custom_single_choice__trust_account_status
  custom_single_choice__trust_account_status:
    _: entity__trust_financial_institution
  entity__trust_financial_institution:
    _: address__trust_financial_institution
  address__trust_financial_institution:
    _: custom_text__trust_account_title
  custom_text__trust_account_title:
    completed_by: signer
    _: custom_text__trust_account_number
  custom_text__trust_account_number:
    completed_by: signer
    _: END
  END: {}
prompts:
  lawyer: Which State Bar of Nevada member is certifying?
  bar_number: The member's Nevada bar number, written in at signing.
  trust_account_status: Which statement describes the member's Nevada trust accounts?
  trust_financial_institution: Which Nevada financial institution holds the trust account?
  trust_account_title: The account name exactly as it appears on the bank statement, written in at signing.
  trust_account_number: The account number, without the routing number, written in at signing.
choices:
  trust_account_status:
    exempt: Exempt under SCR 78; handles no client or third-party funds in Nevada
    iolta: Maintains an IOLTA account created under SCR 217
    iolta_distance_exempt: >-
      Maintains an IOLTA account exempt from participating-bank requirements; no office within 20 miles of an approved
      institution
    scr_78_nominal_exempt: Maintains an SCR 78 account exempt from IOLTA; handles no nominal or short-term funds
workflow:
  BEGIN:
    _: lawyer_review
  lawyer_review:
    _: member_signatures
  member_signatures:
    _: mailroom_send
  mailroom_send:
    _: END
  END: {}
---

SCR 78 and SCR 217 Acknowledgement and Certification of Compliance and Consent, Trust Account Reporting, for
`{{person__lawyer}}`, State Bar of Nevada member `{{custom_text__bar_number}}`. Every active member acknowledges
compliance with and consents to SCR 78 annually, and files an updated certificate when a trust account changes.

The member certifies: `{{custom_single_choice__trust_account_status}}`. The Nevada trust account maintained by the
member or the member's firm, `{{entity_name}}`, is held at `{{entity__trust_financial_institution}}`,
`{{address__trust_financial_institution}}`, under the account name `{{custom_text__trust_account_title}}` and account
number `{{custom_text__trust_account_number}}`, both written in by the member at signing and never stored in Navigator.
Only Nevada trust accounts are reported here.

The member certifies that the statements SCR 78 and SCR 217 require are true and complete.
