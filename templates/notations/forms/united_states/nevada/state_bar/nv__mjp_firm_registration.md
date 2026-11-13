---
kind: filing
title: Nevada RPC 7.5A Registration of Multijurisdictional Law Firms
respondent_type: entity
jurisdiction: NV
origin_url: https://nvbar.org/wp-content/uploads/Online-MJP-Application.pdf
confidential: false
questionnaire:
  BEGIN:
    _: custom_single_choice__application_type
  custom_single_choice__application_type:
    _: addresses__firm_offices
  addresses__firm_offices:
    _: people__firm_attorneys
  people__firm_attorneys:
    _: custom_text__attorney_licensure
  custom_text__attorney_licensure:
    _: custom_text__pending_discipline
  custom_text__pending_discipline:
    _: address__nevada_office
  address__nevada_office:
    _: custom_phone__nevada_office
  custom_phone__nevada_office:
    _: person__resident_member
  person__resident_member:
    _: address__resident_member_residence
  address__resident_member_residence:
    _: custom_phone__resident_member_residence
  custom_phone__resident_member_residence:
    _: entity__trust_financial_institution
  entity__trust_financial_institution:
    _: address__trust_financial_institution
  address__trust_financial_institution:
    _: person__bar_contact
  person__bar_contact:
    _: END
  END: {}
prompts:
  application_type: Is this a new firm registration or the annual renewal?
  firm_offices: >-
    List every office of the firm outside Nevada. Mark any office that is virtual or used only for meetings; the State
    Bar does not permit virtual multijurisdictional offices.
  firm_attorneys: List every attorney the firm employs, in any capacity, with their work address.
  attorney_licensure: >-
    For each attorney, list the jurisdictions where they are licensed and whether they are in good standing in each
    (Exhibit A).
  pending_discipline: >-
    Describe any pending disciplinary action or investigation against any attorney of the firm, or write "No pending
    disciplinary action or investigation" (Exhibit B).
  nevada_office: What is the address of the firm's permanent, physical office in Nevada?
  resident_member: >-
    Who is the resident member: an active Nevada Bar member in good standing, present at least 30 hours a week in the
    Nevada office, and the firm's agent for service of process?
  resident_member_residence: What is the resident member's home address?
  trust_financial_institution: >-
    Which State Bar of Nevada approved financial institution holds the firm's Nevada trust account?
  bar_contact: Whom may the State Bar contact with questions about this application?
choices:
  application_type:
    original: New firm registration
    renewal: Annual renewal
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

RPC 7.5A Original/Renewal Application for the registration of `{{entity_name}}` as a multijurisdictional law firm with
the State Bar of Nevada (`{{custom_single_choice__application_type}}`), filed with the $500 annual fee and proof that
the firm's registration with the Nevada Secretary of State is current.

1. The firm's offices outside Nevada are `{{addresses__firm_offices}}`.
2. Exhibit A names every attorney the firm employs, `{{people__firm_attorneys}}`, with their licensure and good
   standing: `{{custom_text__attorney_licensure}}`.
3. Exhibit B states any pending disciplinary action or investigation: `{{custom_text__pending_discipline}}`.
4. The firm's permanent Nevada office is at `{{address__nevada_office}}`, telephone
   `{{custom_phone__nevada_office}}`.
5. The resident member, who maintains a full-time presence of not less than 30 hours per week in that office and is
   the firm's agent for service of process in Nevada, is `{{person__resident_member}}`, residing at
   `{{address__resident_member_residence}}`, telephone `{{custom_phone__resident_member_residence}}`.
6. The firm certifies that it will maintain a permanent Nevada office with a resident member in good standing; will
   notify the State Bar within thirty days of any change in its resident member, status, or Nevada address; will
   disclose in writing to its Nevada clients which of its attorneys are not licensed in Nevada and what work they will
   perform, and will show that compliance on request; maintains its Nevada trust accounts under SCR 78.5 and SCR 217 at
   `{{entity__trust_financial_institution}}`, `{{address__trust_financial_institution}}`, with all funds from any Nevada
   matter held solely there (the account number is written in by the signing partner); and will comply with RPC 7.5 on
   firm names.
7. The firm will file a renewal and pay the $500 annual fee on or before each anniversary of its original approval.

The State Bar may contact `{{person__bar_contact}}` with questions. The Managing Partner or Principal Shareholder signs
in original ink and certifies that this application and its attachments are true and correct.
