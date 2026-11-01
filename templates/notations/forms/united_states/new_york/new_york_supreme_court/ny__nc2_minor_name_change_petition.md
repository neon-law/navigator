---
kind: filing
title: Name Change and/or Sex Designation Change Petition for an Individual Minor (New York)
respondent_type: person
jurisdiction: NY
origin_url: https://www.nycourts.gov/forms/Name-SexDesignationChange/MinorName-SexDesignationChangePetition-fillable.pdf
confidential: true
output: form
form: ny__nc2_minor_name_change_petition
questionnaire:
  BEGIN:
    _: person__minor
  person__minor:
    _: person__petitioner
  person__petitioner:
    _: person__minor_new_name
  person__minor_new_name:
    _: custom_text__place_of_birth
  custom_text__place_of_birth:
    _: custom_single_choice__filing_court
  custom_single_choice__filing_court:
    _: custom_single_choice__filing_county
  custom_single_choice__filing_county:
    _: custom_single_choice__relationship
  custom_single_choice__relationship:
    _: custom_text__other_relation
  custom_text__other_relation:
    _: custom_single_choice__name_change
  custom_single_choice__name_change:
    _: custom_single_choice__sex_designation_change
  custom_single_choice__sex_designation_change:
    _: custom_text__reason_for_change
  custom_text__reason_for_change:
    _: custom_single_choice__prior_name_change
  custom_single_choice__prior_name_change:
    _: custom_text__prior_petition_details
  custom_text__prior_petition_details:
    _: custom_single_choice__minor_convicted
  custom_single_choice__minor_convicted:
    _: custom_text__minor_felony_details
  custom_text__minor_felony_details:
    _: custom_single_choice__minor_bankruptcy
  custom_single_choice__minor_bankruptcy:
    _: custom_single_choice__minor_judgments_liens
  custom_single_choice__minor_judgments_liens:
    _: custom_single_choice__minor_party_to_action
  custom_single_choice__minor_party_to_action:
    _: custom_text__minor_financial_details
  custom_text__minor_financial_details:
    _: custom_single_choice__minor_currently_married
  custom_single_choice__minor_currently_married:
    _: custom_single_choice__minor_previously_married
  custom_single_choice__minor_previously_married:
    _: custom_single_choice__minor_children_under_21
  custom_single_choice__minor_children_under_21:
    _: custom_single_choice__minor_child_support
  custom_single_choice__minor_child_support:
    _: custom_single_choice__minor_child_support_current
  custom_single_choice__minor_child_support_current:
    _: custom_usd__minor_child_support_arrears
  custom_usd__minor_child_support_arrears:
    _: custom_text__minor_child_support_court
  custom_text__minor_child_support_court:
    _: custom_text__minor_support_collections_unit
  custom_text__minor_support_collections_unit:
    _: custom_single_choice__minor_spousal_support
  custom_single_choice__minor_spousal_support:
    _: custom_single_choice__minor_spousal_support_current
  custom_single_choice__minor_spousal_support_current:
    _: custom_usd__minor_spousal_support_arrears
  custom_usd__minor_spousal_support_arrears:
    _: custom_text__minor_spousal_support_court
  custom_text__minor_spousal_support_court:
    _: custom_text__petitioner_address
  custom_text__petitioner_address:
    _: custom_single_choice__petitioner_convicted
  custom_single_choice__petitioner_convicted:
    _: custom_text__petitioner_felony_details
  custom_text__petitioner_felony_details:
    _: custom_single_choice__petitioner_bankruptcy
  custom_single_choice__petitioner_bankruptcy:
    _: custom_single_choice__petitioner_judgments_liens
  custom_single_choice__petitioner_judgments_liens:
    _: custom_single_choice__petitioner_party_to_action
  custom_single_choice__petitioner_party_to_action:
    _: custom_text__petitioner_financial_details
  custom_text__petitioner_financial_details:
    _: custom_single_choice__petitioner_currently_married
  custom_single_choice__petitioner_currently_married:
    _: custom_single_choice__petitioner_previously_married
  custom_single_choice__petitioner_previously_married:
    _: custom_single_choice__petitioner_children_under_21
  custom_single_choice__petitioner_children_under_21:
    _: custom_single_choice__petitioner_child_support
  custom_single_choice__petitioner_child_support:
    _: custom_single_choice__petitioner_child_support_current
  custom_single_choice__petitioner_child_support_current:
    _: custom_usd__petitioner_child_support_arrears
  custom_usd__petitioner_child_support_arrears:
    _: custom_text__petitioner_child_support_court
  custom_text__petitioner_child_support_court:
    _: custom_text__petitioner_support_collections_unit
  custom_text__petitioner_support_collections_unit:
    _: custom_single_choice__petitioner_spousal_support
  custom_single_choice__petitioner_spousal_support:
    _: custom_single_choice__petitioner_spousal_support_current
  custom_single_choice__petitioner_spousal_support_current:
    _: custom_usd__petitioner_spousal_support_arrears
  custom_usd__petitioner_spousal_support_arrears:
    _: custom_text__petitioner_spousal_support_court
  custom_text__petitioner_spousal_support_court:
    _: custom_single_choice__minor_new_sex_designation
  custom_single_choice__minor_new_sex_designation:
    _: custom_single_choice__minor_prior_sex_designation
  custom_single_choice__minor_prior_sex_designation:
    _: custom_text__minor_prior_sex_designation_details
  custom_text__minor_prior_sex_designation_details:
    _: custom_single_choice__minor_sex_designation_reasons_given
  custom_single_choice__minor_sex_designation_reasons_given:
    _: custom_text__minor_sex_designation_reasons
  custom_text__minor_sex_designation_reasons:
    _: custom_datetime__date_of_birth
  custom_datetime__date_of_birth:
    _: custom_text__minor_current_address
  custom_text__minor_current_address:
    _: custom_single_choice__sealing_requested
  custom_single_choice__sealing_requested:
    _: custom_text__sealing_details
  custom_text__sealing_details:
    _: custom_single_choice__documents_attached
  custom_single_choice__documents_attached:
    _: custom_text__supporting_documents_list
  custom_text__supporting_documents_list:
    _: people__consenting_parents
  people__consenting_parents:
    _: END
  END: {}
prompts:
  person__minor: The child whose name (or sex designation) this petition would change.
  person__petitioner: The parent or legal guardian bringing this petition for the child.
  person__minor_new_name: >-
    The child's complete requested new name, exactly as it should read on the order. Leave blank if you are not asking
    for a name change.
  filing_court: >-
    Which court will this be filed in? If the child lives in New York City, any NYC Civil Court accepts this petition
    and costs less ($65.00 instead of $210.00).
  place_of_birth: >-
    The full address of the place where the child was born — street, city or town, state, ZIP, country.
  filing_county: The county where that court sits.
  relationship: What is your relationship to the child?
  other_relation: If you chose Other, what is your relationship to the child?
  name_change: Are you asking the court to change the child's name?
  sex_designation_change: Are you asking the court to change the child's sex designation?
  reason_for_change: >-
    Why are you asking to change the child's name? Tell the story plainly; the judge reads this.
  prior_name_change: Has a name change petition ever been filed for the child in any court?
  prior_petition_details: If yes, give the court, the date filed, the case number if known, and the result.
  minor_convicted: Has the child ever been convicted of a crime?
  minor_felony_details: If yes, the court and the crime. The attorney confirms the exact wording at review.
  minor_bankruptcy: Has the child ever filed for bankruptcy?
  minor_judgments_liens: Are there any judgments or liens of record against the child?
  minor_party_to_action: Is the child a party to any lawsuit or proceeding?
  minor_financial_details: >-
    If yes to any of the three, give the court where it was filed, the date, and the case number if known.
  minor_currently_married: Is the child currently married?
  minor_previously_married: Has the child been previously married?
  minor_children_under_21: Does the child have any children under 21?
  minor_child_support: Does the child have to pay child support?
  minor_child_support_current: If yes, are the payments satisfied and up to date?
  minor_child_support_arrears: If not, how much is owed?
  minor_child_support_court: The court that issued the child support order, including the county.
  minor_support_collections_unit: The Child Support Collections Unit handling the payments, if any.
  minor_spousal_support: Is the child responsible for spousal support?
  minor_spousal_support_current: If yes, are the payments satisfied and up to date?
  minor_spousal_support_arrears: If not, how much is owed?
  minor_spousal_support_court: The court that issued the spousal support order, including the county.
  petitioner_address: Your full address — street, city or town, state, ZIP, country.
  petitioner_convicted: Have you ever been convicted of a crime?
  petitioner_felony_details: >-
    If yes, the court and the crime. The attorney confirms the exact wording at review.
  petitioner_bankruptcy: Have you ever filed for bankruptcy?
  petitioner_judgments_liens: Are there any judgments or liens of record against you?
  petitioner_party_to_action: Are you a party to any lawsuit or proceeding?
  petitioner_financial_details: >-
    If yes to any of the three, give the court where it was filed, the date, and the case number if known.
  petitioner_currently_married: Are you currently married?
  petitioner_previously_married: Have you been previously married?
  petitioner_children_under_21: Do you have any children under 21?
  petitioner_child_support: Do you have to pay any child support?
  petitioner_child_support_current: If yes, are the payments satisfied and up to date?
  petitioner_child_support_arrears: If not, how much is owed?
  petitioner_child_support_court: The court that issued your child support order, including the county.
  petitioner_support_collections_unit: The Child Support Collections Unit handling your payments, if any.
  petitioner_spousal_support: Are you responsible for spousal support?
  petitioner_spousal_support_current: If yes, are the payments satisfied and up to date?
  petitioner_spousal_support_arrears: If not, how much is owed?
  petitioner_spousal_support_court: The court that issued your spousal support order, including the county.
  minor_new_sex_designation: >-
    The child's requested new sex designation. Leave blank if you are not asking for a sex designation change.
  minor_prior_sex_designation: >-
    Has a sex designation change petition ever been filed for the child in any court?
  minor_prior_sex_designation_details: >-
    If yes, give the court, the date filed, the case number if known, and the result.
  minor_sex_designation_reasons_given: >-
    Do you want to give the court your reasons for changing the child's sex designation? This is optional and not
    required for the order.
  minor_sex_designation_reasons: If yes, the reasons.
  date_of_birth: The child's date of birth.
  minor_current_address: >-
    The child's full current address — street, city or town, state, ZIP, country. The court may ask for proof of this
    address when you file.
  sealing_requested: >-
    Should this court record be sealed for the child's personal safety? While the case is pending, the clerk temporarily
    seals it anyway.
  sealing_details: If yes, explain why you believe sealing is needed.
  documents_attached: Are you attaching any supporting documents or additional pages?
  supporting_documents_list: If yes, list the name or a description of each document.
  people__consenting_parents: >-
    Each parent or legal guardian who is NOT signing this petition must consent. Add each one whose consent is required.
    If every parent or guardian is signing as a petitioner, skip this. (A child aged 14 or older signs their own consent
    section in person before a notary.)
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
  relationship:
    Parent(s): "0"
    Guardian(s): "1"
    Other: "2"
  name_change:
    "Yes": Name Change
  sex_designation_change:
    "Yes": Sex Designation Change
  prior_name_change:
    "Yes": "0"
    "No": "1"
  minor_convicted:
    "Yes": "0"
    "No": "1"
  minor_bankruptcy:
    "Yes": "0"
    "No": "1"
  minor_judgments_liens:
    "Yes": "0"
    "No": "1"
  minor_party_to_action:
    "Yes": "0"
    "No": "1"
  minor_currently_married:
    "Yes": "0"
    "No": "1"
  minor_previously_married:
    "Yes": "0"
    "No": "1"
  minor_children_under_21:
    "Yes": "0"
    "No": "1"
  minor_child_support:
    "Yes": "0"
    "No": "1"
  minor_child_support_current:
    "Yes": "0"
    "No": "1"
  minor_spousal_support:
    "Yes": "1"
    "No": "0"
  minor_spousal_support_current:
    "Yes": "0"
    "No": "1"
  minor_new_sex_designation:
    Male: Male
    Female: Female
    X: X
  minor_prior_sex_designation:
    "Yes": "0"
    "No": "1"
  minor_sex_designation_reasons_given:
    "Yes": "0"
    "No": "1"
  petitioner_convicted:
    "Yes": "0"
    "No": "1"
  petitioner_bankruptcy:
    "Yes": "0"
    "No": "1"
  petitioner_judgments_liens:
    "Yes": "0"
    "No": "1"
  petitioner_party_to_action:
    "Yes": "0"
    "No": "1"
  petitioner_currently_married:
    "Yes": "0"
    "No": "1"
  petitioner_previously_married:
    "Yes": "0"
    "No": "1"
  petitioner_children_under_21:
    "Yes": "0"
    "No": "1"
  petitioner_child_support:
    "Yes": "1"
    "No": "0"
  petitioner_child_support_current:
    "Yes": "0"
    "No": "1"
  petitioner_spousal_support:
    "Yes": "0"
    "No": "1"
  petitioner_spousal_support_current:
    "Yes": "0"
    "No": "1"
  sealing_requested:
    "Yes": "Yes"
    "No": "No"
  documents_attached:
    "Yes": "0"
    "No": "1"

workflow:
  BEGIN:
    intake_submitted: intake_persisted__client
  intake_persisted__client:
    petition_rendered: lawyer_review
  lawyer_review:
    approved: generate_pdf__ny__nc2_minor_name_change_petition_pdf
    rejected: END
  generate_pdf__ny__nc2_minor_name_change_petition_pdf:
    pdf_persisted: filing__new_york_supreme_court
  filing__new_york_supreme_court:
    filed: END
  END: {}

---

# Name Change and/or Sex Designation Change Petition for an Individual Minor (UCS-NC2, New York)

This workflow fills the New York Unified Court System's UCS-NC2 (06/2025) petition to change a minor child's name and/or
sex designation. A parent or legal guardian brings it in the Supreme Court or County Court of the county where the child
lives — or, if the child lives in New York City, in any NYC Civil Court, which costs $65.00 instead of $210.00. The same
statewide form serves both routes.

{{person__minor}} is the child; {{person__petitioner}} is the parent or legal guardian bringing the petition. The
petition asks whether you want a name change, a sex designation change, or both, and then asks a block of background
questions about the child and, separately, about you. Answers you give here are painted onto the official form; anything
the form's own widgets cannot honestly express is left for the attorney to complete with you at review.

## What the petition needs from you

- The child's present name, date of birth, place of birth (with its full address),
  and current address — you must attach proof of birth (a certified birth document or its equivalent) to the petition.
- The requested new name and/or new sex designation (male, female, or X).
- The reasons for the change, told plainly, and whether any earlier petition was
  ever filed for the child.
- Background questions for both the child and you: criminal convictions,
  bankruptcies, judgments or liens, other lawsuits, marriages, children under 21, and any child or spousal support
  obligations with their courts and arrears.
- Whether the record should be sealed for the child's personal safety, and any
  documents you are attaching.

If the child is fourteen or older, the child must sign a notarized consent (Section E of the form) in person before a
notary; that section is completed at signature time. If the child has a parent or legal guardian who is not signing the
petition, that person must consent (Section D) or be given notice — the workflow collects each consenting parent or
guardian and fills their consent recital with the child's names.

## Fees and fee waiver

Filing costs $210.00 in Supreme or County Court, or $65.00 in NYC Civil Court. If you cannot afford the fee, you may
apply for poor person relief (CPLR 1101) by motion with a sworn financial affidavit — there is no official statewide
form, and different judges may require different information, so the attorney prepares that application separately if
you want one.

## What happens after intake

The attorney reviews every answer against the form before filing: the conviction detail boxes, the caption index number,
the child's age, and the verification date are completed at the review appointment; the consents (Section D for each
non-petitioning parent or guardian, Section E for a child aged 14 or older) and the notarizations are signed in person.
The petition, the proof of birth, and any attachments are filed with the clerk of the court you chose.

Note: this petition runs long because the form itself asks the same background block twice — once about the child and
once about you. The questionnaire follows the form's structure; no questions beyond the form's own are asked.
