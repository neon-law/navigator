---
kind: filing
title: Nevada IOLTA Enrollment Form for Attorneys
respondent_type: person_and_entity
jurisdiction: NV
origin_url: https://nvbar.org/wp-content/uploads/NV-IOLTA-Enrollment-Form.Attys_.Update.2019v2.pdf
confidential: false
questionnaire:
  BEGIN:
    _: entity__trust_financial_institution
  entity__trust_financial_institution:
    _: person__authorized_signatory
  person__authorized_signatory:
    _: address__firm
  address__firm:
    _: custom_text__trust_account_title
  custom_text__trust_account_title:
    completed_by: signer
    _: custom_text__trust_account_number
  custom_text__trust_account_number:
    completed_by: signer
    _: END
  END: {}
prompts:
  trust_financial_institution: Which Nevada IOLTA-eligible financial institution holds the account?
  authorized_signatory: Who signs for the firm as the account's authorized signatory?
  firm: What is the firm's address?
  trust_account_title: The name of the trust account to be established as an IOLTA, written in at signing.
  trust_account_number: The account number, written in at signing.
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

Nevada IOLTA Enrollment Form for Attorneys, delivered to `{{entity__trust_financial_institution}}` by mail or fax.

The undersigned participates in the Nevada Interest on Lawyers Trust Account Program authorized by the Supreme Court of
Nevada. The general trust account `{{custom_text__trust_account_title}}`, number
`{{custom_text__trust_account_number}}`, both written in by the signatory and never stored in Navigator, is to be
established as an IOLTA in an interest-bearing NOW account. Interest is remitted monthly to the Nevada Bar Foundation,
the program's tax-exempt designee and third-party beneficiary, with an electronic IOLTA Remittance Report to the
Foundation. The undersigned authorizes the institution to release the account's name, number, interest earned, rate,
fees, and average balance to the Foundation.

Authorized signatory `{{person__authorized_signatory}}` for `{{entity_name}}`, `{{address__firm}}`.
