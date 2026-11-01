---
kind: filing
title: Family Name Change Petition (New York)
respondent_type: person
jurisdiction: NY
origin_url: https://www.nycourts.gov/FORMS/Name-SexDesignationChange/FamilyNameChangePetition-fillable.pdf
confidential: true
output: form
form: ny__nc4_family_name_change_petition
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: people__family_members
  people__family_members:
    _: person__new_family_name
  person__new_family_name:
    _: custom_single_choice__filing_court
  custom_single_choice__filing_court:
    _: custom_single_choice__filing_county
  custom_single_choice__filing_county:
    _: people__co_petitioners
  people__co_petitioners:
    _: custom_single_choice__convicted
  custom_single_choice__convicted:
    _: custom_single_choice__bankruptcy
  custom_single_choice__bankruptcy:
    _: custom_single_choice__judgments_liens
  custom_single_choice__judgments_liens:
    _: custom_single_choice__party_to_action
  custom_single_choice__party_to_action:
    _: custom_single_choice__child_support
  custom_single_choice__child_support:
    _: custom_single_choice__spousal_support
  custom_single_choice__spousal_support:
    _: custom_text__reason_for_change
  custom_text__reason_for_change:
    _: custom_single_choice__prior_name_change
  custom_single_choice__prior_name_change:
    _: custom_text__prior_petition_details
  custom_text__prior_petition_details:
    _: custom_text__current_address
  custom_text__current_address:
    _: custom_datetime__address_since
  custom_datetime__address_since:
    _: custom_single_choice__sealing_requested
  custom_single_choice__sealing_requested:
    _: custom_text__sealing_details
  custom_text__sealing_details:
    _: person__parent_1
  person__parent_1:
    _: custom_text__parent_1_address
  custom_text__parent_1_address:
    _: custom_text__parent_1_consent_children
  custom_text__parent_1_consent_children:
    _: custom_text__parent_1_consent_new_names
  custom_text__parent_1_consent_new_names:
    _: person__parent_2
  person__parent_2:
    _: custom_text__parent_2_address
  custom_text__parent_2_address:
    _: custom_text__parent_2_consent_children
  custom_text__parent_2_consent_children:
    _: custom_text__parent_2_consent_new_names
  custom_text__parent_2_consent_new_names:
    _: END
  END: {}
choices:
  filing_court:
    Supreme Court: Supreme
    County Court: County
    New York City Civil Court: New York City Civil
  filing_county:
    Albany: Albany
    Allegany: Allegany
    Bronx: Bronx
    Broome: Broome
    Cattaraugus: Cattaraugus
    Cayuga: Cayuga
    Chautauqua: Chautauqua
    Chemung: Chemung
    Chenango: Chenango
    Clinton: Clinton
    Columbia: Columbia
    Cortland: Cortland
    Delaware: Delaware
    Dutchess: Dutchess
    Erie: Erie
    Essex: Essex
    Franklin: Franklin
    Fulton: Fulton
    Genesee: Genesee
    Greene: Greene
    Hamilton: Hamilton
    Herkimer: Herkimer
    Jefferson: Jefferson
    Kings: Kings
    Lewis: Lewis
    Livingston: Livingston
    Madison: Madison
    Monroe: Monroe
    Montgomery: Montgomery
    Nassau: Nassau
    New York: New York
    Niagara: Niagara
    Oneida: Oneida
    Onondaga: Onondaga
    Ontario: Ontario
    Orange: Orange
    Orleans: Orleans
    Oswego: Oswego
    Otsego: Otsego
    Putnam: Putnam
    Queens: Queens
    Rensselaer: Rensselaer
    Richmond: Richmond
    Rockland: Rockland
    Saratoga: Saratoga
    Schenectady: Schenectady
    Schoharie: Schoharie
    Schuyler: Schuyler
    Seneca: Seneca
    St. Lawrence: St. Lawrence
    Steuben: Steuben
    Suffolk: Suffolk
    Sullivan: Sullivan
    Tioga: Tioga
    Tompkins: Tompkins
    Ulster: Ulster
    Warren: Warren
    Washington: Washington
    Wayne: Wayne
    Westchester: Westchester
    Wyoming: Wyoming
    Yates: Yates
  convicted:
    Yes: "0"
    "No": "1"
  bankruptcy:
    Yes: "0"
    "No": "1"
  judgments_liens:
    Yes: "0"
    "No": "1"
  party_to_action:
    Yes: "0"
    "No": "1"
  child_support:
    Yes: "0"
    "No": "1"
  spousal_support:
    Yes: "0"
    "No": "1"
  prior_name_change:
    Yes: "8"
  sealing_requested:
    Yes: "0"
    "No": "1"
prompts:
  client_name: >-
    What is your full legal name as it appears now? (It opens the petition caption and identifies you to the court.)
  family_members_name: >-
    List every family member included in this petition — the petitioner, any co-petitioners, and each minor under 18.
    (List minors in the order they appear; the consent pages follow that order. Proof of birth is required for each
    member and is attached to the petition.)
  person__new_family_name: >-
    What new family (last) name are you asking the court to order for the whole household?
  filing_court: >-
    Which court will the petition be filed in? (In New York City the Civil Court route costs $65.00 and the Supreme
    Court route costs $210.00; outside the city the Supreme or County Court of your county hears it. The attorney
    confirms your venue before filing.)
  filing_county: The county where you will file. (For NYC residents, your
    borough's county.)
  people__co_petitioners: >-
    Any co-petitioners — other adult family members joining the petition — by full name. (Leave blank if you are the
    only petitioner.)
  convicted: >-
    Does any family member listed above have a criminal conviction (misdemeanor or felony)? (If yes, that member files a
    supplement — UCS-NC5 — with this petition.)
  bankruptcy: >-
    Has any family member had a bankruptcy adjudication against them? (If yes, that member files a supplement — UCS-NC5
    — with this petition.)
  judgments_liens: >-
    Are there any judgments or liens filed against any family member? (If yes, that member files a supplement — UCS-NC5
    — with this petition.)
  party_to_action: >-
    Is any family member a party in a pending court case? (If yes, that member files a supplement — UCS-NC5 — with this
    petition.)
  child_support: >-
    Does any family member have child support obligations? (If yes, that member files a supplement — UCS-NC5 — with this
    petition.)
  spousal_support: >-
    Does any family member have spousal support obligations? (If yes, that member files a supplement — UCS-NC5 — with
    this petition.)
  reason_for_change: >-
    In your own words, why does the family want its name changed? (The petition tells the judge this under oath.)
  prior_name_change: Have you ever filed a name change petition in any court?
  prior_petition_details: >-
    If you answered yes: where you filed it, when, the case number if you know it, and how it came out. (Leave blank if
    you answered no.)
  current_address: Your current address as the form prints it — street
    address, city or town, state, zip, and country. (The consent pages reuse
    this address.)
  address_since: When did you move to your current address? (Month, day, and
    year.)
  sealing_requested: >-
    Should this court record be sealed for personal safety reasons? (The clerk already keeps the case private until the
    judge decides; sealing makes it inaccessible after that.)
  sealing_details: >-
    If you asked for sealing: why, in your own words. (Leave blank if you did not.)
  parent_1: >-
    If a parent or legal guardian is NOT included as a petitioner above: that person's full name, as the consent page
    swears it. (Leave the parent-consent questions blank when every parent or guardian is already a petitioner — the law
    skips their consent then.)
  parent_1_address: The non-petitioning parent's or guardian's address.
  parent_1_consent_children: >-
    The children's present name(s) as that parent's consent page reads them.
  parent_1_consent_new_names: The children's proposed name(s) for that consent
    page.
  parent_2: >-
    A second non-petitioning parent or guardian, if there is one. (Leave blank when there is none.)
  parent_2_address: The second non-petitioning parent's or guardian's
    address.
  parent_2_consent_children: The children's present name(s) for that second
    consent page.
  parent_2_consent_new_names: The children's proposed name(s) for that second
    consent page.
workflow:
  BEGIN:
    intake_submitted: intake_persisted__client
  intake_persisted__client:
    petition_rendered: lawyer_review
  lawyer_review:
    approved: generate_pdf__ny__nc4_family_name_change_petition_pdf
    rejected: END
  generate_pdf__ny__nc4_family_name_change_petition_pdf:
    pdf_persisted: filing__new_york_supreme_court
  filing__new_york_supreme_court:
    filed: END
  END: {}
---

# Family Name Change Petition (UCS-NC4, New York)

This workflow fills the New York Unified Court System's UCS-NC4 (06/2025) Family Name Change Petition — one petition
that changes the last name of an entire household at once: the petitioner, any co-petitioners, and the minors under 18.
A parent or guardian files it in the Supreme Court or County Court of the county of residence, or, in New York City, in
the Civil Court (the $65.00 route against the Supreme Court's $210.00; see the registry's fee_note). The workflow runs
every filing through lawyer review before the PDF is generated and filed.

## Who this petition serves

One petition, one fee, one filing for a whole family. The form's own note says the petition must list the petitioner,
any co-petitioners, and all minors under 18; each member's proposed name is the same new family name, which the client
states once. Family members 14 or older consent to their own name change; a non-petitioning parent or guardian must
consent for the minors. The petition carries proof of birth for every member (a certified birth certificate or its
equivalents); the intake does not transcribe those documents, and the clerk copies the birth data from the attached
proof.

## What the intake asks

The questionnaire gathers, in order: the petitioner's identity; the family members by name; the new family name; the
filing court and county; any co-petitioners; the six household disclosures the form asks (criminal conviction,
bankruptcy, judgments or liens, pending cases, child support, spousal support — a yes sends that member's UCS-NC5
supplement with the petition); the reasons for the change; any prior name-change petition; the household address and
when it became current; whether the record should be sealed; and the non-petitioning parent or guardian consents.

The form is a family-scale petition and its consent pages add whole sections, so the intake runs past the usual
five-to-eight question range by design; every parent-consent question is skipped when all of a minor's parents or
guardians already join the petition (the form itself says the consent section is skipped then).

## What the lawyer or clerk completes at review

The transform records, but does not answer, the fields that need a human:

- each member's place of birth, date of birth, and age — copied from the
  attached proof of birth at filing (the petition attaches that proof for every member);
- the "parent or legal guardian" selection box on each consent page — the
  two checkbox exports are not machine-readable, so the lawyer checks the right box at review;
- every signature and the notary dates on the consent, minor-consent, and
  verification pages — sworn and signed at review;
- the verification page's print-name lines for co-petitioners — transcribed
  from the caption at review;
- the index number — the clerk's caption stamp;
- the three "attachments" checkboxes on the reasons, prior-petition, and
  sealing sections — the lawyer decides at review what rides with the petition.
