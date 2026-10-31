---
kind: filing
title: Petition for Probate and Letters Testamentary (New York)
respondent_type: person
jurisdiction: NY
origin_url: https://www.nycourts.gov/LegacyPDFS/FORMS/surrogates/pdfs/Probate.pdf
confidential: true
output: form
form: ny__surrogates_probate_petition
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: custom_text__petitioner_mailing_address
  custom_text__petitioner_mailing_address:
    _: custom_text__petitioner_citizenship
  custom_text__petitioner_citizenship:
    _: custom_yes_no__petitioner_is_named_executor
  custom_yes_no__petitioner_is_named_executor:
    _: custom_yes_no__executor_is_attorney
  custom_yes_no__executor_is_attorney:
    _: custom_yes_no__executor_is_draftsperson
  custom_yes_no__executor_is_draftsperson:
    _: custom_yes_no__executor_is_convicted_felon
  custom_yes_no__executor_is_convicted_felon:
    _: person__decedent
  person__decedent:
    _: custom_text__decedent_county
  custom_text__decedent_county:
    _: custom_text__decedent_aka
  custom_text__decedent_aka:
    _: custom_datetime__decedent_date_of_death
  custom_datetime__decedent_date_of_death:
    _: custom_text__decedent_place_of_death
  custom_text__decedent_place_of_death:
    _: custom_text__decedent_citizenship
  custom_text__decedent_citizenship:
    _: custom_text__filing_county
  custom_text__filing_county:
    _: custom_datetime__will_date
  custom_datetime__will_date:
    _: custom_text__will_witnesses
  custom_text__will_witnesses:
    _: custom_datetime__codicil_date
  custom_datetime__codicil_date:
    _: custom_text__codicil_witnesses
  custom_text__codicil_witnesses:
    _: custom_datetime__codicil_date_2
  custom_datetime__codicil_date_2:
    _: custom_text__codicil_witnesses_2
  custom_text__codicil_witnesses_2:
    _: custom_text__no_later_instruments
  custom_text__no_later_instruments:
    _: custom_single_choice__distributee_class_spouse
  custom_single_choice__distributee_class_spouse:
    _: custom_single_choice__distributee_class_children
  custom_single_choice__distributee_class_children:
    _: custom_single_choice__distributee_class_parents
  custom_single_choice__distributee_class_parents:
    _: custom_single_choice__distributee_class_siblings
  custom_single_choice__distributee_class_siblings:
    _: custom_single_choice__distributee_class_grandparents
  custom_single_choice__distributee_class_grandparents:
    _: custom_single_choice__distributee_class_aunts_uncles
  custom_single_choice__distributee_class_aunts_uncles:
    _: custom_single_choice__distributee_class_cousins_once_removed
  custom_single_choice__distributee_class_cousins_once_removed:
    _: custom_usd__improved_real_property_value
  custom_usd__improved_real_property_value:
    _: custom_usd__improved_real_property_less_than
  custom_usd__improved_real_property_less_than:
    _: custom_usd__unimproved_real_property_value
  custom_usd__unimproved_real_property_value:
    _: custom_usd__unimproved_real_property_less_than
  custom_usd__unimproved_real_property_less_than:
    _: custom_usd__gross_rents_18_months
  custom_usd__gross_rents_18_months:
    _: custom_text__other_testamentary_assets
  custom_text__other_testamentary_assets:
    _: custom_text__other_petition_details
  custom_text__other_petition_details:
    _: custom_single_choice__letters_requested
  custom_single_choice__letters_requested:
    _: custom_text__letters_to_name
  custom_text__letters_to_name:
    _: custom_datetime__petition_date
  custom_datetime__petition_date:
    _: END
  END: {}
choices:
  distributee_class_spouse:
    "No survivors in this class": "No"
    "Mark only — survivors in a later class": "X"
    "One": "1"
    "Two": "2"
    "Three": "3"
    "Four": "4"
    "Five": "5"
    "Six": "6"
    "Seven": "7"
    "Eight": "8"
    "Nine": "9"
  distributee_class_children:
    "No survivors in this class": "No"
    "Mark only — survivors in a later class": "X"
    "One": "1"
    "Two": "2"
    "Three": "3"
    "Four": "4"
    "Five": "5"
    "Six": "6"
    "Seven": "7"
    "Eight": "8"
    "Nine": "9"
  distributee_class_parents:
    "No survivors in this class": "No"
    "Mark only — survivors in a later class": "X"
    "One": "1"
    "Two": "2"
    "Three": "3"
    "Four": "4"
    "Five": "5"
    "Six": "6"
    "Seven": "7"
    "Eight": "8"
    "Nine": "9"
  distributee_class_siblings:
    "No survivors in this class": "No"
    "Mark only — survivors in a later class": "X"
    "One": "1"
    "Two": "2"
    "Three": "3"
    "Four": "4"
    "Five": "5"
    "Six": "6"
    "Seven": "7"
    "Eight": "8"
    "Nine": "9"
  distributee_class_grandparents:
    "No survivors in this class": "No"
    "Mark only — survivors in a later class": "X"
    "One": "1"
    "Two": "2"
    "Three": "3"
    "Four": "4"
    "Five": "5"
    "Six": "6"
    "Seven": "7"
    "Eight": "8"
    "Nine": "9"
  distributee_class_aunts_uncles:
    "No survivors in this class": "No"
    "Mark only — survivors in a later class": "X"
    "One": "1"
    "Two": "2"
    "Three": "3"
    "Four": "4"
    "Five": "5"
    "Six": "6"
    "Seven": "7"
    "Eight": "8"
    "Nine": "9"
  distributee_class_cousins_once_removed:
    "No survivors in this class": "No"
    "Mark only — survivors in a later class": "X"
    "One": "1"
    "Two": "2"
    "Three": "3"
    "Four": "4"
    "Five": "5"
    "Six": "6"
    "Seven": "7"
    "Eight": "8"
    "Nine": "9"
  letters_requested:
    "Letters Testamentary": testamentary
    "Letters of Trusteeship": trusteeship
    "Letters of Administration c.t.a.": administration_cta
prompts:
  client_name: >-
    What is your full legal name? (Paragraph 1 of the petition opens with the
    petitioner's name, citizenship, and interest in the estate.)
  petitioner_mailing_address: >-
    Your mailing address, if it is different from your home address. (Leave
    blank if the court should use your home address.)
  petitioner_citizenship: >-
    Of what country are you a citizen? (The petition states the petitioner's
    citizenship.)
  petitioner_is_named_executor: >-
    Are you the executor named in the decedent's will? (If your interest is
    something other than named executor, tell your attorney — the petition's
    "other interest" box is completed at review.)
  executor_is_attorney: >-
    Is the proposed executor an attorney? (A sole executor-attorney must
    comply with 22 NYCRR 207.16(e).)
  executor_is_draftsperson: >-
    Is the proposed executor the attorney who drafted the will, or an
    affiliated attorney or employee? (SCPA 2307-a disclosures apply then.)
  executor_is_convicted_felon: >-
    Has the proposed executor been convicted of a felony? (SCPA 707 bars
    letters then; a Certificate of Relief from Civil Disabilities is attached
    if there is one, and the answer is reviewed.)
  decedent_name: >-
    The decedent's full legal name, as the will's caption reads it.
  decedent_county: >-
    In which county did the decedent live? (The domicile county; the petition
    is filed in that county's Surrogate's Court.)
  decedent_aka: >-
    Another name the decedent used, if the will or the records use more than
    one. (Leave blank if there is none.)
  decedent_date_of_death: When did the decedent die?
  decedent_place_of_death: Where did the decedent die? (The city and state.)
  decedent_citizenship: Of what country was the decedent a citizen?
  filing_county: >-
    Which Surrogate's Court will this be filed in? (The county where the
    decedent lived; the caption reads "To the Surrogate's Court, County
    of".)
  will_date: What is the date of the will?
  will_witnesses: >-
    The names of all witnesses to the will. (Paragraph 3 identifies each
    instrument by date and witnesses.)
  codicil_date: The date of a first codicil, if there is one. (Leave blank if
    the will has no codicil.)
  codicil_witnesses: The names of all witnesses to that first codicil.
  codicil_date_2: The date of a second codicil, if there is one.
  codicil_witnesses_2: The names of all witnesses to that second codicil.
  no_later_instruments: >-
    Is there any later will, codicil, or other testamentary instrument?
    (Paragraph 4 swears there is none except as listed — enter "NONE" or
    describe the later instrument.)
  distributee_class_spouse: >-
    How many surviving spouses? (Paragraph 5 classifies the distributees:
    give the number of survivors in the class, "No survivors in this class"
    for every earlier class with no one in it, and "Mark only — survivors in
    a later class" for every class after the first one with survivors.)
  distributee_class_children: >-
    How many surviving children (or issue of predeceased children)? Include
    marital, nonmarital, and adopted children.
  distributee_class_parents: How many surviving parents?
  distributee_class_siblings: >-
    How many surviving sisters and brothers (or their issue)? Count whole or
    half blood.
  distributee_class_grandparents: How many surviving grandparents?
  distributee_class_aunts_uncles: >-
    How many surviving aunts and uncles (or children of predeceased ones)?
  distributee_class_cousins_once_removed: >-
    How many surviving first cousins once removed?
  improved_real_property_value: >-
    The estimated value of improved real property in New York State.
    (Paragraph 9's asset estimate; the figures are estimates and the court
    confirms them.)
  improved_real_property_less_than: The upper end of that estimate.
  unimproved_real_property_value: >-
    The estimated value of unimproved real property in New York State.
  unimproved_real_property_less_than: The upper end of that estimate.
  gross_rents_18_months: The estimated gross rents for an 18-month period.
  other_testamentary_assets: >-
    Other testamentary assets or causes of action in New York State, if any.
    (Paragraph 9(b) swears there are none except as listed — enter "NONE" or
    describe them.)
  other_petition_details: >-
    Any other petition for probate or administration already filed elsewhere?
    (Paragraph 10 swears none has been filed except as listed — leave blank
    if there is none.)
  letters_requested: >-
    Which letters should issue? (The prayer asks for the letters that match
    the proceeding; the attorney confirms the relief requested.)
  letters_to_name: >-
    To whom should the letters issue? (The nominated executor's name, as the
    prayer's "Letters Testamentary to" line reads it.)
  petition_date: >-
    The date the petition is signed. (The attorney reviews the dating before
    filing.)
workflow:
  BEGIN:
    intake_submitted: intake_persisted__client
  intake_persisted__client:
    petition_rendered: lawyer_review
  lawyer_review:
    approved: generate_pdf__ny__surrogates_probate_petition_pdf
    rejected: END
  generate_pdf__ny__surrogates_probate_petition_pdf:
    pdf_persisted: filing__new_york_surrogates_court
  filing__new_york_surrogates_court:
    filed: END
  END: {}
---

# Petition for Probate and Letters Testamentary (Surrogate's Court, New York)

This workflow fills the New York Surrogate's Court's Petition for Probate and
Letters Testamentary / Trusteeship — the 22-page flagship self-help packet the
Surrogate's Court forms page publishes as a fillable PDF (776 AcroForm fields).
It asks the Surrogate's Court of the county where the decedent lived to admit
the will to probate and issue letters to the executor. Filing fees vary by
proceeding and estate size under SCPA 2402 (the registry's fee_note; no flat
fee is encoded). The workflow runs every filing through lawyer review before
the PDF is generated and filed.

## Who this petition serves

An executor named in a will (or another interested petitioner) asks the court
to probate the will. The petition identifies the petitioner and their interest
(paragraph 1), the decedent and the will (paragraphs 2 and 3), the distributees
the EPTL 4-1.1 classes classify (paragraph 5), the New York assets (paragraph
9), and the letters requested (the prayer). The original will, a certified
death certificate, and the petition are filed together.

## What the intake asks

The questionnaire gathers, in grouped sections that follow the petition's own
paragraphs: the petitioner's identity, mailing address, citizenship, and
interest; the three proposed-executor disclosures (attorney, attorney-
draftsperson, convicted felon — each an is / is-not pair the form prints);
the decedent's identity, domicile, death, and citizenship; the filing county
(the caption and the verification designation repeat it); the will's date and
witnesses with up to two codicils; the paragraph 4 "no later instruments"
sworn line; the seven paragraph 5 distributee-class counts (the form's own
dropdown vocabulary — counts, "No", or the later-class "X" mark); the
paragraph 9 asset estimates; the paragraph 10 exceptions line; and the
prayer's letters request, nominee, and date.

This intake deliberately exceeds the usual five-to-eight question range: the
probate packet is the survey's flagship exception, and the brief for it
directs grouped intake sections with the minimization target not applying.
Thirty-plus states cover the whole client-answerable petition core, organized
by the packet's own paragraph numbers.

## What the lawyer or clerk completes at review

The transform records, but does not answer, the fields that need a human:

- the clerk's stamp block (fees paid, certificates, bond fee, receipt and file
  numbers) — the court's own entries;
- the caption continuations (the second probate-proceeding line and the extra
  "a/k/a" lines) — skip-when-empty duplicates the lawyer trims at review;
- the paragraph 6 distributee tables and the paragraph 7 beneficiary tables,
  including the persons-under-disability rows — who must receive notice is
  the legally sensitive core of a probate petition, and the row cells cannot
  carry the structured people-row parts (one combined address cell, a custom
  interest column), so the tables are completed from the lawyer's distributee
  workpaper at review;
- the paragraph 8 confidential-relationship disclosure — a lawyer
  determination under SCPA 2307-a;
- the prayer's secondary lines (the second letters-testamentary nominee, the
  trusteeship and administration c.t.a. nominee lines with their f/b/o
  beneficiaries) — the letters actually requested are the lawyer's call once
  the distributee work is done;
- the verification, oath, and designation page and the attorney block — sworn
  before the notary and signed at review, with the notary completing the
  dates; and
- the signatures throughout — wet-signed at the review appointment.
