---
kind: agreement
title: Nevada Single-Member LLC Operating Agreement
description: A member-managed Nevada LLC with one individual owner. Covers ownership, authority, and distributions.
start_path: /notations/nevada-single-member-llc-operating-agreement/start
respondent_type: person_and_entity
jurisdiction: NV
confidential: false
prompts:
  company: What is the company’s exact legal name on its Nevada articles of organization?
  member: Who is the sole individual member?
  effective_date: When does the member intend the agreement to take effect?
  contributions: List the money or property the member has contributed or agreed in writing to contribute.
audiences:
  effective_date: lawyer
  contributions: lawyer
questionnaire:
  BEGIN: { _: entity__company }
  entity__company: { _: person__member }
  person__member: { _: custom_datetime__effective_date }
  custom_datetime__effective_date: { _: custom_text__contributions }
  custom_text__contributions: { _: END }
  END: {}
workflow:
  BEGIN: { _: lawyer_review }
  lawyer_review: { approved: END, changes_requested: reask__draft }
  reask__draft: { resubmitted: lawyer_review }
  END: {}
---

# Operating Agreement of {{entity__company.name}}

{{person__member.name}} adopts this agreement as the sole member of {{entity__company.name}}, a Nevada limited-liability
company (the Company). It takes effect on {{custom_datetime__effective_date}} or, if later, when the Company's articles
of organization become effective. Nevada law governs this agreement.

## I. Ownership and contributions

The Member holds the entire membership interest. The Company's assets belong to the Company. The Member records initial
contributions below and will keep a record of later contributions. No additional contribution is due unless the Member
agrees to it in a signed writing.

{{custom_text__contributions}}

## II. Management

The Company is member-managed. The Member may conduct any lawful business permitted by its articles, open accounts,
enter contracts, and appoint agents for the Company. Each written delegation must identify the agent's authority. The
Member will sign for the Company in a representative capacity and document material decisions in its records.

## III. Accounts and distributions

The Member will keep the Company's money and records separate from personal funds, document transactions between the
Member and the Company, and arrange for required returns and filings. The Member receives the Company's profits and
bears its losses as the sole owner. The Member may authorize distributions to the Member after providing for Company
obligations and only to the extent permitted by Nevada law.

## IV. Changes in ownership and dissolution

The Member must document a transfer in writing. Before admitting an additional member, the incoming member and the
Member must adopt a written agreement that addresses their respective interests and management rights. A transfer of
economic rights alone does not authorize the transferee to manage the Company. If the Member dies or becomes
incapacitated, the Member's authorized representative may exercise the powers Nevada law permits.

The Member may dissolve the Company by written decision. On dissolution, the person winding up the Company must provide
for its liabilities before distributing remaining property to the person entitled to the membership interest, and
complete the required filings.

## V. Adoption and amendments

The Member may amend this agreement in a signed writing while the Company has one member. The Member will keep the
signed agreement and amendments with the Company's records.

Sole member: {{person__member.name}}

Signature: ______________________________

Date signed: ______________________________
