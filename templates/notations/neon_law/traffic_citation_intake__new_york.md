---
kind: memo
title: Traffic Citation Intake (New York)
respondent_type: person
jurisdiction: NY
confidential: true
prompts:
  county: In which county was the citation issued?
  traffic_court: Which court is printed on the citation? A town, village, or city court, or the DMV TVB.
  traffic_citation_number: What is the citation number?
  citation_date: On what date was the citation issued?
  traffic_violation: What violation is charged? Include the speed and the posted limit if they are written on it.
  response_deadline: What is the court date or the deadline to respond?
  case_status: What has the driver done about the citation so far?
  accident: Was there an accident or an injury?
  other_citations: Was any other citation issued at the same stop?
  commercial_license: Is the driver's license a commercial license (CDL)?
  traffic_prior_record: List any tickets, deferrals, or traffic school in the last five years, or write "none".
  driving_for_work: Does the driver's employer or insurer check the driving record?
  goal: What outcome matters most?
choices:
  county:
    albany: Albany County
    allegany: Allegany County
    bronx: Bronx County (the Bronx)
    broome: Broome County
    cattaraugus: Cattaraugus County
    cayuga: Cayuga County
    chautauqua: Chautauqua County
    chemung: Chemung County
    chenango: Chenango County
    clinton: Clinton County
    columbia: Columbia County
    cortland: Cortland County
    delaware: Delaware County
    dutchess: Dutchess County
    erie: Erie County
    essex: Essex County
    franklin: Franklin County
    fulton: Fulton County
    genesee: Genesee County
    greene: Greene County
    hamilton: Hamilton County
    herkimer: Herkimer County
    jefferson: Jefferson County
    kings: Kings County (Brooklyn)
    lewis: Lewis County
    livingston: Livingston County
    madison: Madison County
    monroe: Monroe County
    montgomery: Montgomery County
    nassau: Nassau County
    new_york: New York County (Manhattan)
    niagara: Niagara County
    oneida: Oneida County
    onondaga: Onondaga County
    ontario: Ontario County
    orange: Orange County
    orleans: Orleans County
    oswego: Oswego County
    otsego: Otsego County
    putnam: Putnam County
    queens: Queens County
    rensselaer: Rensselaer County
    richmond: Richmond County (Staten Island)
    rockland: Rockland County
    st_lawrence: St. Lawrence County
    saratoga: Saratoga County
    schenectady: Schenectady County
    schoharie: Schoharie County
    schuyler: Schuyler County
    seneca: Seneca County
    steuben: Steuben County
    suffolk: Suffolk County
    sullivan: Sullivan County
    tioga: Tioga County
    tompkins: Tompkins County
    ulster: Ulster County
    warren: Warren County
    washington: Washington County
    wayne: Wayne County
    westchester: Westchester County
    wyoming: Wyoming County
    yates: Yates County
  case_status:
    not_responded: Not responded yet
    paid: Paid the fine
    pleaded_guilty: Pleaded guilty or no contest
    pleaded_not_guilty: Pleaded not guilty
    traffic_school: Asked for traffic school
    other: Something else
  goal:
    no_points: No points on the driving record
    keep_off_record: Keep the citation off the record entirely
    reduce_fine: A lower fine
    dismissal: Dismissal
    not_sure: Not sure yet
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: custom_single_choice__county
  custom_single_choice__county:
    _: custom_text__traffic_court
  custom_text__traffic_court:
    _: custom_text__traffic_citation_number
  custom_text__traffic_citation_number:
    _: custom_datetime__citation_date
  custom_datetime__citation_date:
    _: custom_text__traffic_violation
  custom_text__traffic_violation:
    _: custom_datetime__response_deadline
  custom_datetime__response_deadline:
    _: custom_single_choice__case_status
  custom_single_choice__case_status:
    _: custom_yes_no__accident
  custom_yes_no__accident:
    _: custom_yes_no__other_citations
  custom_yes_no__other_citations:
    _: jurisdiction__license_issuer
  jurisdiction__license_issuer:
    _: custom_yes_no__commercial_license
  custom_yes_no__commercial_license:
    _: custom_text__traffic_prior_record
  custom_text__traffic_prior_record:
    _: custom_yes_no__driving_for_work
  custom_yes_no__driving_for_work:
    _: custom_single_choice__goal
  custom_single_choice__goal:
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

# Traffic citation intake

**Driver.** {{person__client.name}}

**County.** {{custom_single_choice__county}}

**Court.** {{custom_text__traffic_court}}

**Citation number.** {{custom_text__traffic_citation_number}}

**Issued.** {{custom_datetime__citation_date}}

**Violation charged.** {{custom_text__traffic_violation}}

**Court date or response deadline.** {{custom_datetime__response_deadline}}

**What the driver has done so far.** {{custom_single_choice__case_status}}

**Accident or injury.** {{custom_yes_no__accident}}

**Other citations at the same stop.** {{custom_yes_no__other_citations}}

**License issued by.** {{jurisdiction__license_issuer.name}}

**Commercial license.** {{custom_yes_no__commercial_license}}

**Record in the last five years.** {{custom_text__traffic_prior_record}}

**Employer or insurer checks the record.** {{custom_yes_no__driving_for_work}}

**Objective.** {{custom_single_choice__goal}}

## Documents to collect

1. A photo of both sides of the citation.
2. Every notice or letter the court has sent.
3. The driver's record from the licensing state, when the driver can get it.
4. A photo of the driver's license.

## When a call is needed

The Firm can usually take the matter on these answers alone. The reviewing lawyer calls the driver first when any of
these is true:

1. There was an accident or an injury.
2. The driver holds a commercial license.
3. The charge is a misdemeanor or a criminal offense rather than an infraction.
4. The license was issued outside New York.
5. The court date or deadline is less than seven days away.
6. The person who wrote to the Firm is not the driver.

## Short answer

Reserved for the reviewing lawyer. The lawyer confirms against primary sources the court's options for this citation,
such as a deferral, traffic school, or an amendment to a non-moving violation, and the points each outcome carries,
before advising the driver.
