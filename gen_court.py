#!/usr/bin/env python3
"""Generate store/seeds/Court.yaml from the four verified surveys.

sha256 pins come from the blanks downloaded 2026-10-31 (all_shas.txt).
The blank PDFs themselves are never checked in.
"""
import re

SHAS = {}
for line in open('/home/user/work/blanks/all_shas.txt'):
    parts = line.split()
    if len(parts) >= 4 and parts[0] == 'OK':
        SHAS[parts[2].upper()] = parts[3]

def sha(k):
    return SHAS[k.upper()]

USD = "USD"
def fee(a): return {"amount": a, "currency": USD}

def sf(key, title, num, url, fill, office, fee=None, note=None):
    out = [f"      - key: {key}",
           f"        title: {title}"]
    out.append(f"        form_number: {num if num else 'null'}")
    out.append(f"        source_url: {url}")
    out.append(f"        sha256: {sha(key.upper())}")
    out.append(f"        filing_office: {office}")
    if fee:
        out.append(f"        filing_fee:\n          amount: \"{fee}\"\n          currency: {USD}")
    if note:
        out.append(f"        fee_note: {note}")
    out.append(f"        fillability: {fill}")
    return "\n".join(out)

FILL = {"fill": "fillable", "flat": "unfillable_source", "unv": "unverified"}

# ---------------- California ----------------
CA_URL = lambda n: f"https://www.courts.ca.gov/documents/{n}.pdf"
ca_statewide_note = ('First-paper filing fee varies by county (the published statewide range is '
                     '$435-$450); the accepted fee-waiver paperwork is FW-001 with the FW-003 order, '
                     'which has no template code on the forms shelf yet.')

ca = []
# Name change info + gender forms
ca.append(sf("nc100info", "Instructions for Filing a Petition for Change of Name", "NC-100-INFO", CA_URL("NC100INFO"), FILL["fill"], "Superior Court Clerk's Office in the petitioner's county of residence", note="n/a (information sheet). " + ca_statewide_note))
ca.append(sf("nc400info", "Information Sheet for Name Change Proceedings Under Address Confidentiality Program (Safe at Home)", "NC-400-INFO", CA_URL("NC400INFO"), FILL["fill"], "Superior Court Clerk's Office in the petitioner's county of residence", note="n/a (information sheet)."))
ca.append(sf("nc200info", "Instructions for the Petition for Change of Name to Conform to Gender Identity", "NC-200-INFO", CA_URL("NC200INFO"), FILL["fill"], "Superior Court Clerk's Office in the petitioner's county of residence", note="n/a (information sheet). " + ca_statewide_note))
ca.append(sf("nc312", "Marriage License and Certificate—Attachment", "NC-312", CA_URL("NC312"), FILL["fill"], "Superior Court Clerk's Office in the petitioner's county of residence", note=ca_statewide_note))
ca.append(sf("nc325", "Order to Show Cause—Issuance of New Marriage License and Certificate", "NC-325", CA_URL("NC325"), FILL["fill"], "Superior Court Clerk's Office in the petitioner's county of residence", note=ca_statewide_note))
ca.append(sf("nc500info", "Information Sheet: Petition for Recognition of Minor's Change of Gender and Sex Identifier", "NC-500-INFO", CA_URL("NC500INFO"), FILL["fill"], "Superior Court Clerk's Office in the petitioner's county of residence", note="n/a (information sheet); NC-500-series petition PDFs follow the same documents/ URL pattern."))
# DV packet
DV_OFFICE = "Superior Court Clerk's Office or courthouse self-help center in the petitioner's county of residence"
dv_forms = [
    ("dv100", "Request for Domestic Violence Restraining Order", "DV-100", "DV100"),
    ("dv109", "Notice of Court Hearing", "DV-109", "DV109"),
    ("dv110", "Temporary Restraining Order", "DV-110", "DV110"),
    ("dv120", "Response to Request for Domestic Violence Restraining Order", "DV-120", "DV120"),
    ("dv130", "Restraining Order After Hearing", "DV-130", "DV130"),
    ("dv200", "Confidential Contact Information (screening)", "DV-200", "DV200"),
    ("dv250", "Proof of Service by Mail", "DV-250", "DV250"),
    ("dv300", "Request to Change or End Restraining Order", "DV-300", "DV300"),
    ("dv305", "Request to Change Child Custody and Visitation Orders", "DV-305", "DV305"),
    ("dv310", "Notice of Court Hearing and Temporary Order to Change or End Restraining Order", "DV-310", "DV310"),
]
for k, t, n, f in dv_forms:
    ca.append(sf(k, t, n, CA_URL(f), FILL["fill"], DV_OFFICE, fee="0.00" if k in ("dv100", "dv300") else None,
                 note="There is no court fee to file for a domestic violence restraining order." if k == "dv100" else None))
ca.append(sf("dv530info", "How to Enforce Your Restraining Order", "DV-530-INFO", CA_URL("DV530INFO"), FILL["unv"], DV_OFFICE, note="n/a (information sheet); fillability asserted on the self-help page but not binary-verified in the survey."))
ca.append(sf("ser001", "Request for Sheriff to Serve Court Papers", "SER-001", CA_URL("SER001"), FILL["fill"], DV_OFFICE, note="The sheriff serves DV restraining-order papers free."))
# CHRO
ch_note = "Filing fee was not stated on any page surveyed (2026-10-30); any court fee can be waived with FW-001, which has no template code on the forms shelf yet."
for k, t, n, f in [("ch100", "Request for Civil Harassment Restraining Orders", "CH-100", "CH100"),
                   ("ch109", "Notice of Court Hearing", "CH-109", "CH109"),
                   ("ch110", "Temporary Restraining Order", "CH-110", "CH110"),
                   ("ch120", "Response to Request", "CH-120", "CH120")]:
    ca.append(sf(k, t, n, CA_URL(f), FILL["fill"], DV_OFFICE, note=ch_note if k == "ch100" else None))
# Small claims
SC_OFFICE = "Superior Court Clerk's Office, small claims division, in the plaintiff's county"
sc_note = ('Filing fee is tiered by claim amount (Statewide Civil Fee Schedule eff. 1/1/2026): $30 up to $1,500; '
           '$50 for $1,501-$5,000; $75 for $5,001-$12,500; $100 for a 13th-or-later claim in 12 months; '
           '$75 notice of appeal. Jurisdiction: $12,500 individuals / $6,250 businesses.')
sc_forms = [
    ("sc100", "Plaintiff's Claim and ORDER to Go to Small Claims Court", "SC-100", "SC100"),
    ("sc100a", "Other Plaintiffs or Defendants (attachment)", "SC-100A", "SC100A"),
    ("sc101", "Attorney Fee Dispute (After Arbitration) (attachment)", "SC-101", "SC101"),
    ("sc103", "Fictitious Business Name", "SC-103", "SC103"),
    ("sc104", "Proof of Service (small claims)", "SC-104", "SC104"),
    ("sc105", "Request for Court Order and Answer", "SC-105", "SC105"),
    ("sc107", "Small Claims Subpoena for Personal Appearance and Production of Documents at Trial or Hearing and Declaration", "SC-107", "SC107"),
    ("sc109", "Authorization to Appear on Behalf of Party", "SC-109", "SC109"),
    ("sc112a", "Proof of Service by Mail (small claims)", "SC-112A", "SC112A"),
    ("sc114", "Request to Amend Party Name Before Hearing", "SC-114", "SC114"),
    ("sc120", "Defendant's Claim and ORDER to Go to Small Claims Court", "SC-120", "SC120"),
    ("sc133", "Judgment Debtor's Statement of Assets", "SC-133", "SC133"),
    ("sc134", "Order to Produce Statement of Assets and to Appear for Examination", "SC-134", "SC134"),
    ("sc135", "Notice of Motion to Vacate Judgment and Declaration", "SC-135", "SC135"),
    ("sc145", "Request to Pay Judgment to Court", "SC-145", "SC145"),
    ("sc150", "Request to Postpone Trial", "SC-150", "SC150"),
    ("sc220", "Request to Make Payments", "SC-220", "SC220"),
    ("sc290", "Acknowledgment of Satisfaction of Judgment", "SC-290", "SC290"),
    ("civ110", "Request for Dismissal", "CIV-110", "CIV110"),
    ("subp025", "Notice to Consumer or Employee and Objection", "SUBP-025", "SUBP025"),
    ("ej130", "Writ of Execution", "EJ-130", "EJ130"),
    ("ej001", "Abstract of Judgment—Civil and Small Claims", "EJ-001", "EJ001"),
    ("wg001", "Application for Earnings Withholding Order (Wage Garnishment)", "WG-001", "WG001"),
]
for k, t, n, f in sc_forms:
    ca.append(sf(k, t, n, CA_URL(f), FILL["fill"], SC_OFFICE, note=sc_note if k == "sc100" else None))
# Probate
for k, t, n, f, fl in [("de111", "Petition for Probate", "DE-111", "DE111", FILL["fill"]),
                       ("de121", "Notice of Petition to Administer Estate", "DE-121", "DE121", FILL["fill"]),
                       ("de140", "Order for Probate", "DE-140", "DE140", FILL["fill"]),
                       ("de147", "Duties and Liabilities of Personal Representative", "DE-147", "DE147", FILL["fill"]),
                       ("de160", "Inventory and Appraisal", "DE-160", "DE160", FILL["fill"]),
                       ("de305", "Affidavit re Real Property of Small Value", "DE-305", "DE305", FILL["fill"]),
                       ("mc030", "Declaration", "MC-030", "MC030", FILL["fill"])]:
    ca.append(sf(k, t, n, CA_URL(f), fl, "Superior Court Clerk's Office, probate division, in the decedent's county",
                 fee="435.00" if k == "de111" else None,
                 note=("Probate first-paper fee of $435 under GC 70658(a) (county surcharges can push the total to $450); "
                       "fee waiver FW-001 + FW-003 applies but has no template code on the forms shelf yet. "
                       "The small-estate affidavit process (estates up to $184,500) uses no statewide court form.") if k == "de111" else None))
# Fee waivers
for k, t, n, f in [("fw001", "Request to Waive Court Fees", "FW-001", "FW001"),
                   ("fw001info", "Information Sheet on Waiving Court Fees", "FW-001-INFO", "FW001INFO"),
                   ("fw002", "Order on Court Fee Waiver (additional fees)", "FW-002", "FW002"),
                   ("fw003", "Order on Court Fee Waiver", "FW-003", "FW003")]:
    ca.append(sf(k, t, n, CA_URL(f), FILL["fill"], "Filed with the underlying case at the Superior Court Clerk's Office",
                 note="No filing fee; the request is confidential." if k == "fw001" else None))

CA_STATEWIDE = "\n".join(ca)

LA = "\n".join([
    sf("laciv264", "Name Change Packet (instructions and filing locations)", "LACIV 264",
       "https://www.lacourt.org/division/civil/pdf/namechangelocationsandinstructions.pdf",
       FILL["flat"], "Los Angeles Superior Court district courthouse civil filing window (13+ locations listed in the packet)",
       note="Not a fillable blank — a 2-page instructions/locations sheet covering adult name, adult name+gender, gender recognition, and minor's gender-recognition petitions. The operative blanks are the statewide Judicial Council forms. Statewide fees apply."),
])
SD = "\n".join([
    sf("sdpkt001", "Change of Name Packet", "PKT-001",
       "https://www.sdcourt.ca.gov/sites/default/files/sdcourt/generalinformation/forms/civilforms/pkt001.pdf",
       FILL["fill"], "North County Courthouse or Hall of Justice civil business office, depending on residence",
       note="15-page fillable packet wrapping the statewide Judicial Council forms. Publication of the OSC runs once a week for four consecutive weeks."),
    sf("sdpkt033", "Adult's Change of Gender and Sex Identifier, Name Change, and Issuance of New Certificates Packet", "PKT-033",
       "https://www.sdcourt.ca.gov/sites/default/files/sdcourt/generalinformation/forms/civilforms/pkt033.pdf",
       FILL["flat"], "North County Courthouse or Hall of Justice civil business office, depending on residence",
       note="Packet cover is not fillable (0 AcroForm fields); its contents are the fillable JCC forms CM-010, NC-300-INFO, NC-300, NC-330. Statewide fees apply (ADM-001 fee schedule confirms $435 first paper)."),
])
NV_STATEWIDE = []
NV_OFFICE = "District Court Clerk's Office in the petitioner's county of residence"
for k, t, n, u, fl in [
    ("FAMCOVER", "Family Cover Sheet", None, "https://selfhelp.nvcourts.gov/images/misc/coversheet-family-pdf-fillable.pdf", FILL["fill"]),
    ("ADULTNOTICE", "Notice of Petition for Adult Name Change", None, "https://selfhelp.nvcourts.gov/images/name-change/name-adult-notice-pdf-fillable.pdf", FILL["fill"]),
    ("ADULTDISPO", "Request for Summary Disposition & Declaration in Support (Adult Name Change)", None, "https://selfhelp.nvcourts.gov/images/name-change/name-adult-summary-dispo-pdf-fillable.pdf", FILL["fill"]),
    ("ADULTORDER", "Order for Adult Name Change", None, "https://selfhelp.nvcourts.gov/images/name-change/name-adult-order-pdf-fillable.pdf", FILL["fill"]),
    ("CHILDPET", "Petition for Child's Name Change", None, "https://selfhelp.nvcourts.gov/images/name-change/name-child-petition-pdf-fillable.pdf", FILL["fill"]),
    ("CHILDNOTICE", "Notice of Petition for Child's Name Change", None, "https://selfhelp.nvcourts.gov/images/name-change/name-child-notice-pdf-fillable.pdf", FILL["fill"]),
    ("CHILDCONSENT", "Child's Consent to Child's Name Change", None, "https://selfhelp.nvcourts.gov/images/name-change/name-child-consent-child-pdf-fillable.pdf", FILL["fill"]),
    ("CHILDSERVICE", "Proof of Service on the Other Parent", None, "https://selfhelp.nvcourts.gov/images/name-change/name-child-service-pdf-fillable.pdf", FILL["fill"]),
    ("CHILDDISPO", "Request for Summary Disposition & Declaration (Child)", None, "https://selfhelp.nvcourts.gov/images/name-change/name-child-summary-dispo-pdf-fillable.pdf", FILL["fill"]),
    ("CHILDORDER", "Order for Child's Name Change", None, "https://selfhelp.nvcourts.gov/images/name-change/name-child-order-pdf-fillable.pdf", FILL["fill"]),
]:
    NV_STATEWIDE.append(sf(k, t, n, u, fl, NV_OFFICE,
        note="Filing fee varies by district court; the statewide Application to Waive Filing Fee applies (registered below)." if k in ("CHILDPET", "ADULTNOTICE") else None))
NV_P0 = "District Court Clerk's Office or the county protection-order office, in any Nevada county"
for k, t, u in [
    ("DVCONF", "Confidential Information Sheet (Domestic Violence)", "https://selfhelp.nvcourts.gov/images/protection-orders/confidential-info-dv-pdf-fillable.pdf"),
    ("DVUCCJEA", "UCCJEA Declaration (Domestic Violence)", "https://selfhelp.nvcourts.gov/images/protection-orders/dv-uccjea-pdf-fillable.pdf"),
    ("STALKAPP", "Application for Protection (Stalking/Harassment/Sexual Assault/Harm to Minors)", "https://selfhelp.nvcourts.gov/images/protection-orders/stalking-harassment-sa-harm-app-pdf-fillable.pdf"),
    ("STALKCONF", "Confidential Information Sheet (Stalking/Harassment/Sexual Assault/Harm to Minors)", "https://selfhelp.nvcourts.gov/images/protection-orders/confidential-info-other-pdf-fillable.pdf"),
    ("WORKTPO", "Application for Temporary Protection from Workplace Harassment", "https://selfhelp.nvcourts.gov/images/protection-orders/workplace-app-temp-pdf-fillable.pdf"),
    ("WORKEXT", "Application for Extended Protection from Workplace Harassment", "https://selfhelp.nvcourts.gov/images/protection-orders/workplace-app-extended-pdf-fillable.pdf"),
    ("HRREG", "Application for High-Risk Protection Order (family/household members)", "https://selfhelp.nvcourts.gov/images/protection-orders/high-risk-app-reg-pdf-fillable.pdf"),
    ("HRLE", "Application for High-Risk Protection Order (law enforcement)", "https://selfhelp.nvcourts.gov/images/protection-orders/high-risk-app-le-pdf-fillable.pdf"),
]:
    NV_STATEWIDE.append(sf(k, t, None, u, FILL["fill"], NV_P0, fee="0.00",
        note="There is no fee to file protection-order forms; certified copies are also free." if k == "STALKAPP" else None))
for k, t, u in [("FEEWAIVAPP", "Application to Waive Filing Fee", "https://selfhelp.nvcourts.gov/images/misc/fee-waiver-application-pdf-fillable.pdf"),
                ("FEEWAIVORD", "Order to Waive Filing Fee", "https://selfhelp.nvcourts.gov/images/misc/fee-waiver-order-pdf-fillable.pdf")]:
    NV_STATEWIDE.append(sf(k, t, None, u, FILL["fill"], "Filed with the underlying case at the clerk's office",
        note="No filing fee; the waiver lasts one year and covers case-startup and filing fees plus sheriff service." if k == "FEEWAIVAPP" else None))
NV_STATEWIDE = "\n".join(NV_STATEWIDE)

EJDC = "\n".join([
    sf("SETTINGSLIP", "Setting Slip", None, "https://www.familylawselfhelpcenter.org/images/forms/misc/setting-slip-pdf-fillable.pdf", FILL["fill"], "Family Division Clerk's Office, Eighth Judicial District Court"),
    sf("TPONONCONF", "TPO Packet, Non-Confidential (Domestic Violence)", None, "https://www.familylawselfhelpcenter.org/images/forms/tpo/tpo-packet-nonconfidential-pdf-fillable.pdf", FILL["unv"], "Family Division Clerk's Office or the Protection Order Help Center, Family Division, 1 S. Sierra St equivalent — Las Vegas: 601 N. Pecos Rd", note="Clark-wrapped copy of the statewide DV TPO packet; no filing fee."),
    sf("TPOAPP", "TPO Application (Domestic Violence)", None, "https://www.familylawselfhelpcenter.org/images/forms/tpo/tpo-app-pdf-fillable.pdf", FILL["unv"], "Family Division Clerk's Office or the Protection Order Help Center", note="No filing fee."),
    sf("TPOCONF", "TPO Confidential Information (Domestic Violence)", None, "https://www.familylawselfhelpcenter.org/images/forms/tpo/tpo-conf-info-pdf-fillable.pdf", FILL["unv"], "Family Division Clerk's Office or the Protection Order Help Center", note="No filing fee."),
    sf("TPOCOVER", "Family Court Cover Sheet (TPO)", None, "https://www.familylawselfhelpcenter.org/images/forms/tpo/family-court-cover-sheet-tpo-pdf-fillable.pdf", FILL["unv"], "Family Division Clerk's Office", note="Required cover sheet for new Family Division cases (EDCR 7.20 requires a civil/family cover sheet at filing)."),
    sf("SETASIDEINT", "Set-Aside Packet, Intestate (small-estate set-aside)", None, "https://www.civillawselfhelpcenter.org/images/Probate/2026-Probate-Forms/Set-Aside-Intestate/packet---set-aside-intestate.pdf", FILL["fill"], "Probate Division Clerk's Office, Eighth Judicial District Court"),
    sf("AFFENTITLE", "Affidavit of Entitlement (small estate)", None, "https://www.civillawselfhelpcenter.org/images/Probate/2026-Probate-Forms/Affidavit-of-Entitlement/Fillable-Affidavit-of-Entitlement.pdf", FILL["flat"], "Probate Division Clerk's Office, Eighth Judicial District Court",
       note="The filename says 'Fillable' but the published PDF has zero AcroForm fields — registered as an unfillable source. A true small-estate self-help flow needs the field overlay or a re-authored blank."),
    sf("IFPPROBATE", "Application to Proceed In Forma Pauperis and Order (Waiver of Costs and Fees under NRS 12.015)", None, "https://www.clarkcountycourts.us/res/clerk/civil-criminal-library//Probate/PDF/feewaiver_int.pdf", FILL["fill"], "Clerk's Office, Eighth Judicial District Court", note="No filing fee for the application itself; statutory fees are waived only by the IFP order, not at the counter."),
])

LVJC = "\n".join([
    sf("LVCSCOMPLAINT", "Complaint — Small Claims (Las Vegas Township)", None, "https://www.civillawselfhelpcenter.org/images/small-claims/complaints/complaint---small-claims.pdf", FILL["fill"], "Las Vegas Justice Court clerk, small claims division",
       note="Filing fee tiered by claim (NRS 4.060 schedule, eff. 2/8/2024): $66 up to $1,000; $86 for $1,000.01-$2,500; $106 for $2,500.01-$5,000; $146 for $5,000.01-$7,500; $196 for $7,500.01-$10,000. No fee may be charged to a defendant appearing. Waiver: the justice-court fee-waiver form (below) or the statewide application."),
    sf("LVCSCOUNTER", "Counterclaim — Small Claims (Las Vegas Township)", None, "https://www.civillawselfhelpcenter.org/images/small-claims/counterclaims/counterclaim---small-claims---las-vegas.pdf", FILL["fill"], "Las Vegas Justice Court clerk, small claims division"),
    sf("LVCSAPPEAL", "Appeal of Small Claims Judgment", None, "https://www.civillawselfhelpcenter.org/images/small-claims/lasvegas/small-claims-appeal-fillable.pdf", FILL["fill"], "Las Vegas Justice Court clerk, small claims division"),
    sf("JCFFEEWAIV", "Fee Waiver — Justice Court", None, "https://www.civillawselfhelpcenter.org/images/fee-waivers/fee-waiver---justice-court.pdf", FILL["fill"], "Las Vegas Justice Court clerk", note="No filing fee for the application. Henderson and North Las Vegas variants exist on the same self-help page."),
])

WASHOE_SJD = "\n".join([
    sf("WASHOENA", "Petition for Adult Name Change (Washoe NA packet)", None, "https://www.washoecourts.com/Forms/Family/10.%20Name%20Changes/NA%20Adult%20Name%20Change/1.%20Petition%20for%20Adult%20Name%20Change.pdf", FILL["fill"], "Family Division Clerk's Office, Second Judicial District Court, 1 S. Sierra St, Reno",
       note="The filing fee for a Washoe name change was NOT published on any page the survey fetched (2026-10-30) — treat the amount as unknown. The NA-2 Ex Parte Motion to Waive Publication and Seal File (safety valve) and NM-1..NM-4 minor packets are on the same forms tree; their direct PDF URLs carry cache-busting query strings."),
    sf("F6FAM", "Application for Waiver of Fees and Costs (F-6, Family)", "F-6", "https://www.washoecourts.com/Forms/Family/1.%20All%20Other%20Family%20Related%20Packets/F-6%20Application%20to%20Waive%20Fees%20and%20Costs/1.%20Application%20for%20Waiver%20of%20Fees%20and%20Costs.pdf", FILL["fill"], "Family Division Clerk's Office, Second Judicial District Court", note="No filing fee for the application; the F-6JP joint variant also exists."),
    sf("F6GJ", "Application for Waiver of Fees and Costs (F-6, General Jurisdiction)", "F-6", "https://www.washoecourts.com/Forms/GJ/16.%20F-6%20Application%20to%20Waive%20Fees%20and%20Costs/1.%20Application%20for%20Waiver%20of%20Fees%20and%20Costs.pdf", FILL["fill"], "Clerk's Office, Second Judicial District Court"),
    sf("WASHOEF14", "Application for Protection Order Against Domestic Violence (F-14, no minor children)", "F-14", "https://www.washoecourts.com/Forms/Family/17.%20Temporary%20Protection%20Orders/F-14%20Temporary%20Protection%20Order%20%28No%20Minor%20Children%29/2.%20Application%20for%20Protection%20Order%20Against%20Domestic%20Violence.pdf", FILL["fill"], "Protection Order Help Center, 3rd floor, Family Division, 1 S. Sierra St, Reno", note="No filing fee."),
    sf("WASHOEAFFENT", "Affidavit of Entitlement (Washoe)", None, "https://www.washoecourts.com/Forms/GJCommon/Affidavit%20of%20Entitlement.pdf", FILL["fill"], "Probate Division Clerk's Office, Second Judicial District Court"),
])

RJC = "\n".join([
    sf("RJCSCDECL", "Declaration of Small Claims Complaint", None, "https://washoecounty.gov/rjc/files/forms/form_civil_sc-declaration_of_small_claims_complaint.pdf", FILL["fill"], "Reno Justice Court clerk, civil division",
       note="Filing fee tiered by claim (NRS 4.060/73; 'Civil Fees as of 04-2026'): $66 up to $1,000; $86 to $2,500; $106 to $5,000; $146 to $7,500; $196 to $10,000; each includes the $21 NRS ch. 4 fee. Transfer to justice court $125; abstract of judgment $25."),
    sf("RJCSCANSWER", "Small Claims Answer", None, "https://washoecounty.gov/rjc/files/forms/form_civil_sc-small_claims_answer.pdf", FILL["fill"], "Reno Justice Court clerk, civil division"),
    sf("RJCSCCOUNTER", "Small Claims Counterclaim", None, "https://washoecounty.gov/rjc/files/forms/form_civil_sc-small_claims_counterclaim.pdf", FILL["fill"], "Reno Justice Court clerk, civil division"),
    sf("RJCSCAPPEAL", "Notice of Appeal (small claims)", None, "https://washoecounty.gov/rjc/files/forms/form_civil_sc_notice-appeal.pdf", FILL["fill"], "Reno Justice Court clerk, civil division"),
    sf("RJCIFP", "Motion to Proceed In Forma Pauperis", None, "https://washoecounty.gov/rjc/files/forms/form_civil_general_motion-proceed-in-forma-pauperis.pdf", FILL["fill"], "Reno Justice Court clerk, civil division", note="No filing fee for the motion."),
])

# ---------------- Washington ----------------
WA_URL = "https://www.courts.wa.gov/forms/documents/"
wa_statewide = []
PO_OFF = "Superior Court Clerk's Office or district court clerk (either accepts protection-order petitions); many counties also staff a protection-order office"
wa_statewide.append(sf("PO001", "Petition for Protection Order", "PO 001", WA_URL + "PO%20001%20Petition%20for%20Protection%20Order_06_2026.pdf", FILL["flat"], PO_OFF,
    note="One unified petition covers all five RCW ch. 7.105 order types (DV, sexual assault, stalking, vulnerable adult, anti-harassment). The published PDF has zero AcroForm fields — overlay fields or route through the official Guide & File. Anti-harassment is the only type the petition flags '(fee may be required)' (Pierce: $53 Superior / $83 District); DV/SA/stalking petitions have no fee line in county schedules; vulnerable-adult filing is 'None (7.105)'. Fee waiver: PO 012/PO 014 (harassment) or GR 34."))
wa_statewide.append(sf("POI001", "Instructions for Petition for Protection Order", "POi 001", WA_URL + "POi%20001%20Instructions%20Petition%20for%20Protection%20Order_2026_01.pdf", FILL["unv"], PO_OFF, note="n/a (instructions)."))
for k, t, n, u, fl in [
    ("PO003", "Law Enforcement and Confidential Information (LECIF)", "PO 003", "PO%20003%20Law%20Enforcement%20and%20Confidential%20Info_2026_03.pdf", FILL["unv"]),
    ("PO003R", "LECIF — Restrained Person", "PO 003R", "PO%20003R%20Law%20Enforcement%20and%20Confidential%20Info%20Restrained%20Person_2026_01.pdf", FILL["unv"]),
    ("PO030", "Temporary Protection Order and Hearing Notice", "PO 030", "PO%20030%20Temporary%20Protection%20Order%20and%20Hearing%20Notice_07_2025.pdf", FILL["flat"]),
    ("PO040", "Protection Order (final order)", "PO 040", "PO%20040%20Protection%20Order_06_2026.pdf", FILL["unv"]),
    ("PO018", "Declaration", "PO 018", "PO%20018%20Declaration_2022_07.pdf", FILL["unv"]),
    ("PO010", "Statement", "PO 010", "PO%20010%20Statement_2022_07.pdf", FILL["unv"]),
    ("PO012", "Motion and Declaration for Waiver of Filing Fees and Surcharges — Harassment", "PO 012", "PO%20012%20Motion%20Filing%20Fee%20Waiver%20-%20Harassment_%202023_01.pdf", FILL["flat"]),
    ("PO014", "Order re Waiver of Filing Fees and Surcharges — Harassment", "PO 014", "PO%20014%20Order%20re%20Filing%20Fee%20Waiver-Harassment_07_2025.pdf", FILL["unv"]),
    ("PO004", "Proof of Service (RTS)", "PO 004", "PO%20004%20Proof%20of%20Service_2026_01.pdf", FILL["unv"]),
    ("PO050", "Motion to Renew Protection Order", "PO 050", "PO%20050%20Motion%20for%20Renewal%20of%20Protection%20Order_07_2025.pdf", FILL["unv"]),
    ("PO063", "Motion to Modify or Terminate Protection Order", "PO 063", "PO%20063%20Motion%20to%20Modify%20or%20Terminate_07_2025.pdf", FILL["unv"]),
    ("WS001", "Order to Surrender and Prohibit Weapons", "WS 001", "WS%20001%20Order%20to%20Surrender%20and%20Prohibit%20Weapons_06_2026.pdf", FILL["unv"]),
]:
    wa_statewide.append(sf(k, t, n, WA_URL + u, fl, PO_OFF, note="No filing fee for the petition types other than anti-harassment." if k == "PO030" else None))
SC_OFF_WA = "District Court clerk, small claims division"
for k, t, n, u, fl, note in [
    ("MISC050100", "Notice of Small Claim", "MISC 05.0100", "MISC%2005%200100%20Notice%20of%20Small%20Claim%202025%2001.pdf", FILL["flat"],
     "Filed in district court (limits: $10,000 natural person / $5,000 entity). The statewide pattern PDF is not AcroForm-fillable; counties host local fillable copies (e.g. King County). Filing fee is set per county ($50 in King, Pierce, and Clark districts)."),
    ("MISC050200", "Certificate of Service (small claims)", "MISC 05.0200", "MISC%2005%200200%20SC%20-%20Proof%20of%20Service%202025%2001.pdf", FILL["flat"], SC_OFF_WA),
    ("MISC050300", "Small Claims/Order of Dismissal/Continuance", "MISC 05.0300", "MISC%2005%20300SC%20Order%20Dismiss%20Continue%202025%2001.pdf", FILL["unv"], SC_OFF_WA),
    ("MISC050500", "Small Claims Judgment", "MISC 05.0500", "MISC05_0500_SmallClaims%20Judgment%202025_01%2001.pdf", FILL["unv"], SC_OFF_WA),
]:
    wa_statewide.append(sf(k, t, n, WA_URL + u, fl, SC_OFF_WA, note=note))
GR_OFF = "Filed with the underlying case at the clerk's office"
for k, t, n, u, fl, note in [
    ("GR340100", "Motion and Declaration for Waiver of Civil Fees and Surcharges (MTMVF)", "WPF GR 34.0100", "GR%2034_0100%20Party%20MTWVF%20Filing%20Fee%20Waiver_2019%2007(02).pdf", FILL["flat"], GR_OFF),
    ("GR340200", "Motion and Declaration for Waiver of Civil Fees and Surcharges (QLSP Filing)", "WPF GR 34.0200", "GR%2034_0200%20QLSP%20MTWVF%20Filing%20Fee%20Waiver_2021%2004.pdf", FILL["unv"], GR_OFF),
    ("GR340300", "Financial Statement (attachment)", "WPF GR 34.0300", "GR%2034_0300%20Financial%20Statement(02).pdf", FILL["flat"], GR_OFF),
    ("GR340500", "Order Re Waiver of Civil Fees and Surcharges", "WPF GR 34.0500", "GR%2034_0500%20Order%20re%20Fee%20Waiver(02).pdf", FILL["flat"], GR_OFF),
]:
    wa_statewide.append(sf(k, t, n, WA_URL + u, fl, GR_OFF, note="No filing fee for the waiver motion itself." if k == "GR340100" else None))
WA_STATEWIDE = "\n".join(wa_statewide)

KC_DC = "\n".join([
    sf("KCPETADULT", "Petition for Name Change (Individual)", None, "https://cdn.kingcounty.gov/-/media/king-county/courts/district-court/forms_resources_library_docs/name-change-forms/petition-for-name-change-individual-july_2026.pdf", FILL["fill"], "King County District Court clerk",
       note="Filing fee $98 per application (includes 1 certified copy; additional copies $5) plus a separate King County Recorder fee: $203.50 individual; $204.50 first family member + $203.50 each additional. Waiver: the local GR 34 motion (below) or the statewide GR 34 set. Sealed name changes (gender expression/identity, refugee/asylum, DV victim, emancipated minor) go to Superior Court instead."),
    sf("KCPETMINOR", "Petition for Minor Child Name Change", None, "https://cdn.kingcounty.gov/-/media/king-county/courts/district-court/forms_resources_library_docs/name-change-forms/petition-for-name-change-minor-october-2024.pdf", FILL["fill"], "King County District Court clerk",
       note="Both parents' consent or service required; children 14-17 must attend the hearing. Fee as for the adult petition."),
    sf("KCGR34", "Motion and Declaration for Waiver of Fees and Surcharges (King County local GR 34 version)", None, "https://cdn.kingcounty.gov/-/media/king-county/courts/district-court/forms_resources_library_docs/name-change-forms/motion_and_declaration_for_waiver_of_fees_and_surcharges_october-2025.pdf", FILL["fill"], "King County District Court clerk", note="No filing fee for the motion."),
    sf("KCSCNOTICE", "Notice of Small Claim (King County local form)", None, "https://cdn.kingcounty.gov/-/media/king-county/courts/district-court/forms_resources_library_docs/small_claims_forms/forms_sc/notice-of-small-claim-september-2025.pdf", FILL["fill"], "King County District Court clerk, small claims division",
       note="Filing fee $50; limits $10,000 natural person / $5,000 other. Appeal to Superior: $280 cashier's check to the Superior Court Clerk + $40 district appeal-processing fee. A guided e-file interview generates this notice."),
    sf("KCSCPACKET", "Small Claims Packet — Plaintiff", None, "https://cdn.kingcounty.gov/-/media/king-county/courts/district-court/forms_resources_library_docs/small_claims_forms/info_instruct_small_claims/small_claims_plaintiff_packet-july-2026.pdf", FILL["fill"], "King County District Court clerk, small claims division", note="Instructions packet wrapping the claim process."),
])
KC_SUP = "\n".join([
    sf("KCWILLCOVER", "Will Repository Cover Sheet", None, "https://cdn.kingcounty.gov/-/media/king-county/depts/dja/forms/will-repository-coversheet-pdf.pdf", FILL["unv"], "King County Superior Court Clerk's Office",
       note="Will Repository $20.00; Will Filing only (no probate) $20.00; Probate Filing $290.00; Civil Action $290.00 (Superior Court and Clerk's fee schedule, eff. 10/1/2026). Fee waiver: statewide GR 34."),
    sf("KCWILLWITHDRAW", "Motion and Declaration to Withdraw Will from Repository", None, "https://cdn.kingcounty.gov/-/media/king-county/depts/dja/forms/motion-withdraw-will-pdf.pdf", FILL["unv"], "King County Superior Court Clerk's Office"),
])
PIERCE_DC = "\n".join([
    sf("PIERCENCINFO", "Adult Name Change Information packet", None, "https://www.piercecountywa.gov/DocumentCenter/View/134269/NC_Name-Change-Information-Adult", FILL["unv"], "Pierce County District Court clerk, Room 239, County-City Building, 930 Tacoma Ave S",
       note="Name-change fee $308.50 (each additional family member $218.50) per the District Court fee schedule eff. 12/08/2025. Petition completed online (district court portal) or via the county Find-a-Form page — no stable static petition PDF was captured by the survey (the site sits behind a bot challenge). Sealed name changes must be heard in Superior Court. Waiver: Financial Declaration/Motion and Order to Waive Fees fillable online; statewide GR 34 for Superior filings."),
])
PIERCE_SUP = "\n".join([
    sf("PIERCEPOPACKET", "Petition and LECIF packet (protection order, county republished)", None, "https://www.piercecountywa.gov/DocumentCenter/View/115550/Petition-and-LECIF?bidId=", FILL["unv"], "Civil Orders for Protection desk, County-City Building Room 110, 930 Tacoma Ave S",
       note="County wrapper of the statewide PO 001 + PO 003/003A packet (RCW 7.105.115 Mandatory, 03/2026); deliver by 2:30 p.m. for same-day commissioner review; only one order type may be checked. Fees per the Clerk's fee schedule eff. 6/11/2026: anti-harassment petition $53.00; protection of vulnerable adults — no filing fee; DV/SA/stalking petitions have no fee line."),
])
CLARK_WA_DC = "\n".join([
    sf("CLARKWAPETADULT", "Petition for Individual Adult Name Change", None, "https://clark.wa.gov/media/document/95066", FILL["unv"], "Clark County District Court clerk, 1200 Franklin St, Vancouver",
       note="Filing fees due at filing, non-refundable: $296.50 individual adult; $298.50 family or minor. Petitions filed in person; hearings virtual by default; additional certified copies $5. GR 34 fee waiver available upon request. Sealed name changes go to the Superior Court Clerk's Office."),
    sf("CLARKWAPETMINOR", "Petition for Minor Name Change", None, "https://clark.wa.gov/media/document/95061", FILL["unv"], "Clark County District Court clerk", note="Fee $298.50 (family or minor)."),
    sf("CLARKWAPETFAMILY", "Petition for Family Name Change", None, "https://clark.wa.gov/media/document/95056", FILL["unv"], "Clark County District Court clerk", note="Fee $298.50 (family or minor)."),
    sf("CLARKWACONFID", "Confidential Identification Information Form (required for ALL name changes)", None, "https://clark.wa.gov/media/document/212476", FILL["unv"], "Clark County District Court clerk"),
    sf("CLARKWACONSENT", "Consent of Natural Parent for Minor Name Change (notarized)", None, "https://clark.wa.gov/media/document/95076", FILL["unv"], "Clark County District Court clerk"),
])
CLARK_WA_SUP = "\n".join([
    sf("CLARKWADVSEALED", "Instructions for a DV Sealed Name Change Petition in Clark County Superior Court", None, "https://clark.wa.gov/sites/default/files/dept/files/law-library/brochures/DV-Sealed-Name-Change-Packet-Instructions_and_Forms.pdf", FILL["unv"], "Clark County Superior Court Clerk's Office, 1200 Franklin St, Vancouver",
       note="Public Law Library of King County packet hosted by Clark County. Superior civil filing $290.00; small-estate affidavit filing $20.00 (blank forms $4.00); will-only filing $20.00 (Clerk fee schedule). GR 34 fee waiver forms available through the court administrator."),
])

# ---------------- New York ----------------
NY_SUP = []
NC_BASE = "https://www.nycourts.gov/forms/Name-SexDesignationChange/"
LEG = "https://www.nycourts.gov/LegacyPDFS/"
for k, t, n, u in [
    ("NC1", "Name Change and/or Sex Designation Change Petition for Individual Adult (18+)", "UCS-NC1", NC_BASE + "AdultName-SexDesignationChangePetition-fillable.pdf"),
    ("NCO1", "Order Granting Name Change and/or Sex Designation Change, Individual Adult", "UCS-NCO1", NC_BASE + "AdultName-SexDesignationChangeOrder-fillable.pdf"),
    ("NC2", "Name Change and/or Sex Designation Change Petition for Individual Minor (under 18)", "UCS-NC2", NC_BASE + "MinorName-SexDesignationChangePetition-fillable.pdf"),
    ("NCO2", "Order Granting Name Change and/or Sex Designation Change, Individual Minor", "UCS-NCO2", NC_BASE + "MinorName-SexDesignationChangeOrder-fillable.pdf"),
    ("NC3", "Name Change and/or Sex Designation Change Petition for a Legally Incapacitated Person", "UCS-NC3", NC_BASE + "LIPName-SexDesignationChangePetition-fillable.pdf"),
    ("NCO3", "Order Granting Name Change and/or Sex Designation Change, Legally Incapacitated Person", "UCS-NCO3", NC_BASE + "LIPName-SexDesignationChangeOrder-fillable.pdf"),
    ("NC4", "Family Name Change Petition", "UCS-NC4", "https://www.nycourts.gov/FORMS/Name-SexDesignationChange/FamilyNameChangePetition-fillable.pdf"),
    ("NC5", "Family Name Change Petition — Supplemental Form", "UCS-NC5", "https://www.nycourts.gov/FORMS/Name-SexDesignationChange/SupplementalFamilyNameChangePetition-fillable.pdf"),
    ("NCO4", "Order Granting a Family Name Change", "UCS-NCO4", "https://www.nycourts.gov/FORMS/Name-SexDesignationChange/FamilyNameChangeOrder-fillable.pdf"),
    ("UCS840", "Request for Judicial Intervention", "UCS-840", LEG + "forms/rji/UCS-840-fillable.pdf"),
]:
    note = None
    if k == "NC1":
        note = ("Filed in Supreme Court or County Court of the county of residence (or any NYC Civil Court — see that court's record; Civil Court costs less). "
                "Supreme/County Court fee $210.00; a minor 14+ signs a notarized consent on the minor route. "
                "The same UCS-NC forms carry sex-designation changes (no separate petition form exists); the birth-certificate step runs through the NYC Health Attestation Form (born in NYC) or the NYS DOH Application for Correction of Certificate of Birth for Gender Designation (born in NYS outside NYC). "
                "Fee waiver: poor person relief by motion with a sworn financial affidavit — no official statewide form. "
                "sha256 pins a Wayback-archived copy of the blank (the live nycourts.gov URL serves a Cloudflare bot challenge to plain clients).")
    elif k == "NC2":
        note = "Minor petition; a minor 14 or older must sign a notarized consent and the other parent/guardian must consent or be noticed. Same $210.00 Supreme Court fee. sha256 pins a Wayback-archived copy (Cloudflare)."
    elif k == "UCS840":
        note = "Supreme Court only. The name-change self-help page states there is no fee to file this form in a name-change proceeding; the general fee schedule lists the RJI at $95 (CPLR 8020(a)). sha256 pins a Wayback-archived copy (Cloudflare)."
    NY_SUP.append(sf(k, t, n, u, FILL["fill"], "Supreme Court County Clerk's Office in the county of residence", fee="210.00" if k == "NC1" else None, note=note))
NY_SUPREME = "\n".join(NY_SUP)

CIV_OFF = "NYC Civil Court clerk, any county in the city"
NYC_CIVIL = "\n".join([
    sf("CIVNCADULT", "Petition for Individual Adult for Leave to Change His/Her Name (NYC Civil Court format)", None, "https://www.nycourts.gov/courts/nyc/civil/forms/namechangeadult.pdf", FILL["fill"], CIV_OFF,
       fee="65.00", note="NYC Civil Court fee $65.00 vs $210.00 in Supreme Court; a NYC resident may file in any county in the city. sha256 pins a Wayback-archived copy (Cloudflare)."),
    sf("CIVNCINSTR", "Name Change Instructions (NYC Civil Court, non-fillable)", None, "https://www.nycourts.gov/courts/nyc/civil/forms/NameChangeInstructions.pdf", FILL["flat"], CIV_OFF, note="n/a (instructions). sha256 pins a Wayback-archived copy (Cloudflare)."),
    sf("CIVSC50", "Statement of Claim (Small Claims)", "CIV-SC-50", LEG + "courts/nyc/civil/forms/CIV-SC-50.pdf", FILL["fill"], "NYC Civil Court, Small Claims Part clerk",
       fee="15.00", note="Filing fee $15.00 for claims up to $1,000; $20.00 above $1,000. Limit $10,000 in NYC; individuals only (corporations cannot sue). Cannot be filed electronically. sha256 pins a Wayback-archived copy (Cloudflare)."),
    sf("CIVSC70", "Statement of Claim (Commercial Claims)", "CIV-SC-70", LEG + "courts/nyc/civil/forms/CIV-SC-70.pdf", FILL["fill"], "NYC Civil Court, Commercial Claims Part clerk",
       note="The surveyed pages state no separate commercial-claims filing fee — amount unknown. sha256 pins a Wayback-archived copy (Cloudflare)."),
    sf("CIVGP67A", "Notice of Appeal (small claims; cannot be used after arbitration)", "CIV-GP-67A", LEG + "courts/nyc/smallclaims/forms/noticeofappeal.pdf", FILL["fill"], "NYC Civil Court, Small Claims Part clerk", note="sha256 pins a Wayback-archived copy (Cloudflare)."),
    sf("SCFEEWAIV", "Fee Waiver Form (small claims / commercial claims)", None, LEG + "courts/nyc/civil/forms/FeeWaiver.pdf", FILL["fill"], "NYC Civil Court, Small Claims Part clerk", note="No filing fee for the application. sha256 pins a Wayback-archived copy (Cloudflare)."),
    sf("POORPERSON", "Proceed as a Poor Person and to Waive Court Fees", None, "https://nycourts.gov/courts/nyc/civil/forms/PoorPersonsRelief.pdf", FILL["fill"], "NYC Civil Court clerk", note="General civil fee-waiver application. sha256 pins a Wayback-archived copy (Cloudflare)."),
    sf("UCS124", "Demand Letter (consumer credit transaction cases)", "UCS-124", LEG + "courts/nyc/civil/pdfs/UCS124_DmdLtr.pdf", FILL["flat"], "Sent by the claimant before filing; not a court filing", note="n/a — evidence attachment, not a filing form. sha256 pins a Wayback-archived copy (Cloudflare)."),
])
SURR_OFF = "Surrogate's Court of the county where the decedent lived"
SURR_NOTE = ("Filing fees vary by proceeding and estate size under SCPA 2402; the UCS does not publish the table on its fee page. "
             "Fee waiver: poor person relief by motion (no official form). "
             "sha256 pins a Wayback-archived copy of the blank (the live nycourts.gov URL serves a Cloudflare bot challenge).")
NY_SURR = "\n".join([
    sf("PROBATE", "Petition for Probate and Letters Testamentary / Trusteeship", None, LEG + "FORMS/surrogates/pdfs/Probate.pdf", FILL["fill"], SURR_OFF, note=SURR_NOTE + " The flagship self-help packet: 22 pages, 776 AcroForm fields."),
    sf("ANCPROB", "Petition for Ancillary Probate", None, LEG + "FORMS/surrogates/pdfs/ancprob.pdf", FILL["fill"], SURR_OFF, note=SURR_NOTE),
    sf("PROBCTA", "Petition for Letters of Administration c.t.a. (after Probate)", None, LEG + "FORMS/surrogates/pdfs/probcta.pdf", FILL["fill"], SURR_OFF, note=SURR_NOTE),
    sf("SUCTEST", "Petition for Successor Letters Testamentary", None, LEG + "FORMS/surrogates/pdfs/PDF%20Fillable%20SuccessorTest.pdf", FILL["fill"], SURR_OFF, note=SURR_NOTE),
    sf("ADM", "Administration Proceeding, Petition for Letters of Administration / Limited Administration", None, LEG + "FORMS/surrogates/pdfs/ADM.pdf", FILL["fill"], SURR_OFF, note=SURR_NOTE),
    sf("ADMDBN", "Petition for Letters of Administration d.b.n. (SCPA 1007)", None, LEG + "FORMS/surrogates/pdfs/ADMdbn.pdf", FILL["flat"], SURR_OFF, note=SURR_NOTE),
    sf("SURRP14", "Petition for Successor Letters Testamentary (legacy version)", "Surrogate-P14", LEG + "FORMS/surrogates/pdfs/Surrogate-P14.pdf", FILL["flat"], SURR_OFF, note=SURR_NOTE),
    sf("SURRP15", "Renunciation of Successor Letters Testamentary and Waiver of Process (Individual)", "Surrogate-P15", LEG + "FORMS/surrogates/pdfs/Surrogate-P15.pdf", FILL["flat"], SURR_OFF, note=SURR_NOTE),
    sf("SURRP16", "Renunciation of Successor Letters Testamentary and Waiver of Process (Corporation)", "Surrogate-P16", LEG + "FORMS/surrogates/pdfs/Surrogate-P16.pdf", FILL["flat"], SURR_OFF, note=SURR_NOTE),
    sf("SURRP17", "Notice of Petition for Appointment of Successor Executor", "Surrogate-P17", LEG + "FORMS/surrogates/pdfs/Surrogate-P17.pdf", FILL["flat"], SURR_OFF, note=SURR_NOTE),
])
NY_FAMILY = "\n".join([
    sf("FC82", "Family Offense Petition (FCA §§812, 818, 821)", "UCS-FC8-2", LEG + "FORMS/familycourt/pdfs/8-2.pdf", FILL["flat"], "Family Court clerk, any county",
       fee="0.00", note=("It is free to file in Family Court. The published petition has zero AcroForm fields — print-and-write; "
                         "filers can also complete it with court staff, and no DIY program exists for family offense petitions. "
                         "The order forms (8-3 through 8-7) are court-issued, not litigant-fillable. "
                         "sha256 pins a Wayback-archived copy (Cloudflare).")),
])

# ---------------- assemble document ----------------
def court(key, name, jur, level, forms, surveyed, county=None, city=None, extra=""):
    lines = [f"  - key: {key}", f"    name: {name}", f"    jurisdiction: {jur}"]
    if county: lines.append(f"    county: {county}")
    if city: lines.append(f"    city: {city}")
    lines.append(f"    level: {level}")
    lines.append(extra.rstrip() if extra else "    forms: []")
    if surveyed:
        lines.append("    surveyed_forms:")
        lines.append(surveyed)
    return "\n".join(lines)

HEADER = """lookup_fields:
  - key
# The court-form registry: the courts whose self-help forms Navigator
# carries, and the per-form entries keyed to forms-shelf template codes.
#
# Each record is a court; each entry in its `forms:` list registers one
# template under `templates/notations/forms/` with the provenance a filer
# needs: what the filing office calls the form, where the official blank is
# published, the sha-256 pin of that blank, where in the courthouse the
# filing goes, what it costs, and which fee-waiver form the office accepts.
#
# The blank itself is never checked in — `sha256` pins it where the court
# publishes it, exactly like the vendored forms' `.sha256` files. A form
# entry may only reference a template that exists on the forms shelf, and a
# template under a court directory must have an entry here;
# `cli/tests/court_registry.rs` fails CI on either drift.
#
# This file is canonical reference data consumed at compile time
# (`store::courts`), not a database table — the seed boundary test classifies
# it in the canonical layer.
#
# ---------------------------------------------------------------------
# Provenance (populated 2026-10-31 from the four verified regional surveys
# of 2026-10-30 — see the project artifacts "California/Washington/Nevada/
# New York Court Self-Help Form Inventory"):
#
# - Every `forms:` entry is a template-backed form (the 12 templates from
#   the notation wave); everything else lives in `surveyed_forms:`, which
#   is registry data only — the same provenance fields, no template join.
# - Every sha256 was computed from the official blank downloaded 2026-10-31
#   and is never the bytes themselves. New York blanks are pinned from
#   Wayback-archived copies (nycourts.gov serves a Cloudflare bot challenge
#   to plain clients); the live URLs are the ones the UCS pages link today.
# - `fillability` is the survey's binary AcroForm check: `fillable` (field
#   dictionary carries widgets), `unfillable_source` (0 fields, whatever
#   the filename claims), or `unverified` (not binary-checked).
# - `fee_waiver_code` stays null throughout: the guard test resolves it to
#   a forms-shelf template, and no fee-waiver template is vendored yet —
#   the waiver form each office accepts is named in the fee note instead.
# - CA Drupal JSON:API harvest: the root endpoint
#   (https://selfhelp.courts.ca.gov/jsonapi) was live on 2026-10-31 but
#   returns an empty default collection, and content-type probes beneath
#   it serve HTML, not JSON:API envelopes — no form index to harvest; the
#   2026-10-30 survey remains the freshest data for California.
# - Explicitly unknown (survey-flagged, not guessed): the Washoe name-change
#   filing fee; NY SE-3A small-estate affidavit fillability (page-claimed
#   fillable, never binary-verified, no Wayback copy — so it carries no
#   entry at all); the NY commercial-claims filing fee; the CA civil-
#   harassment filing fee; WA statewide PO fees beyond the Pierce schedule's
#   lines. Clark's "Fillable-Affidavit-of-Entitlement" is registered as an
#   unfillable source.
# - Agency forms (WA DOH birth-certificate sex-designation forms 422-143/
#   422-144 — flat; DOL 520043 — fillable; the NV DPBH vital-records
#   affidavit route) are not court filings and are not registered here.
# - Pierce County's name-change/small-claims petitions and the WA statewide
#   PO 003A are delivered through portal pages with no stable static PDF
#   captured; they are described in notes rather than registered.
records:
"""

# splice existing template-backed forms lists verbatim
main_yaml = open('/home/user/work/court-registry/store/seeds/Court.yaml').read()
def extract(record_key):
    lines = main_yaml.split('\n')
    out, in_block = [], False
    for i, l in enumerate(lines):
        if l == '  - key: ' + record_key:
            in_block = True
            continue
        if in_block:
            if l.startswith('  - key: '):
                break
            out.append(l)
    text = '\n'.join(out)
    idx = text.index('    forms:\n')
    body = text[idx + len('    forms:\n'):]
    while body.endswith('\n'):
        body = body[:-1]
    # forms list ends where indent drops below 6 spaces
    collected = []
    for l in body.split('\n'):
        if l.startswith('      ') or not l.strip():
            collected.append(l)
        else:
            break
    return '    forms:\n' + '\n'.join(collected).rstrip()

EXISTING_CA_FORMS = extract("california_superior_court")
EXISTING_NV_FORMS = extract("nevada_supreme_court")

records = []
# --- California (existing statewide record: keep forms, add surveyed) ---
records.append(court("california_superior_court", "Superior Court of California", "CA", "superior", None,
    CA_STATEWIDE, extra=EXISTING_CA_FORMS))
records.append(court("los_angeles_superior_court", "Los Angeles Superior Court", "CA", "superior",
    None, LA, county="Los Angeles",
    extra="    # No LA-specific fillable name-change blank exists; the statewide forms apply\n    # and the LACIV 264 packet is an instructions sheet.\n    forms: []"))
records.append(court("san_diego_superior_court", "San Diego Superior Court", "CA", "superior",
    None, SD, county="San Diego",
    extra="    # San Diego wraps the statewide forms in local packets; the statewide\n    # Judicial Council blanks remain the operative filing targets.\n    forms: []"))
records.append(court("san_francisco_superior_court", "San Francisco Superior Court", "CA", "superior",
    None, "", county="San Francisco",
    extra="    # The ACCESS Center (400 McAllister St, Room 509) helps fill RO forms (CH, DV,\n"
          "    # EA, GV, WV). SF adopts the Statewide Civil Fee Schedule eff. 1/1/2026 as its\n"
          "    # civil fee schedule, so the statewide $435/$450 figures and FW-001/FW-003\n"
          "    # waiver pairing apply unchanged; the court publishes A-Z guide PDFs (name and\n"
          "    # gender change, fee waiver) rather than local fillable blanks.\n    forms: []"))
records.append(court("alameda_superior_court", "Alameda Superior Court", "CA", "superior",
    None, "", county="Alameda",
    extra="    # The Forms @ Home page routes self-represented litigants to Law Help\n"
          "    # Interactive guided interviews (DVRO, CHRO, EARO, name change, parentage,\n"
          "    # eviction, guardianship, guardianship fee waiver) — no local fillable\n"
          "    # blanks of its own; the statewide forms apply.\n    forms: []"))
# --- Nevada ---
records.append(court("nevada_supreme_court", "Nevada Supreme Court", "NV", "supreme", None,
    NV_STATEWIDE, extra=EXISTING_NV_FORMS))
records.append(court("eighth_judicial_district_court", "Eighth Judicial District Court", "NV", "district",
    None, EJDC, county="Clark",
    extra="    # Family Division (Family Law Self-Help Center) and Probate Division.\n"
          "    # The statewide Nevada Supreme Court forms (registered above) are accepted\n"
          "    # here; these rows carry only what is unique to Clark County. The adult\n"
          "    # name-change fee is $270.00 (+$3.50 per eFileNV upload), per the Family\n"
          "    # Law Self-Help Center. Clark's WordPress REST API is locked down (401),\n"
          "    # so form indexes are HTML only.\n    forms: []"))
records.append(court("second_judicial_district_court", "Second Judicial District Court", "NV", "district",
    None, WASHOE_SJD, county="Washoe",
    extra="    # The Washoe NA adult name-change packet is Washoe-specific (not the\n"
          "    # statewide form). The NA-2 publication-waiver/seal motion and NM-1..NM-4\n"
          "    # minor packets live on the same forms tree with cache-busting URLs.\n"
          "    # The statewide Nevada Supreme Court forms (registered above) are accepted\n"
          "    # here; these rows carry only what is unique to Washoe County.\n    forms: []"))
records.append(court("las_vegas_justice_court", "Las Vegas Justice Court", "NV", "justice",
    None, LVJC, county="Clark", city="Las Vegas",
    extra="    # Small claims forms are per-justice-court in Nevada; there is no\n"
          "    # statewide small-claims set.\n    forms: []"))
records.append(court("reno_justice_court", "Reno Justice Court", "NV", "justice",
    None, RJC, county="Washoe", city="Reno", extra="    forms: []"))
# --- Washington ---
records.append(court("washington_supreme_court", "Washington Supreme Court (statewide pattern forms)", "WA", "supreme",
    None, WA_STATEWIDE,
    extra="    # Statewide pattern-form library (courts.wa.gov/forms). The sampled pattern\n"
          "    # PDFs are NOT AcroForm-fillable — a fill tool must overlay fields on the\n"
          "    # flat PDFs or route through Guide & File; every form page also links an\n"
          "    # official Word version. Name changes and wills/probate have NO statewide\n"
          "    # pattern forms (county/local forms only; name changes may be filed in any\n"
          "    # district court per RCW 4.24.130). Sex-designation changes run through\n"
          "    # agency forms (DOH/DOL), not the courts.\n    forms: []"))
records.append(court("king_county_district_court", "King County District Court", "WA", "district",
    None, KC_DC, county="King", city="Seattle",
    extra="    # King County's own local forms ARE fillable AcroForms, unlike the\n"
          "    # statewide pattern PDFs. eFiling portal: dcfiling.kingcounty.gov.\n    forms: []"))
records.append(court("king_county_superior_court", "King County Superior Court", "WA", "superior",
    None, KC_SUP, county="King", city="Seattle",
    extra="    # Name changes auto-seal here under RCW 4.24.130(5) for gender identity,\n"
          "    # DV, refugee/asylum, and emancipated-minor filings. Superior filing fee\n"
          "    # $290.00 (civil action; fee schedule eff. 10/1/2026); statewide GR 34\n"
          "    # applies for waivers. Protection orders: Superior or District Court.\n    forms: []"))
records.append(court("pierce_county_district_court", "Pierce County District Court", "WA", "district",
    None, PIERCE_DC, county="Pierce", city="Tacoma",
    extra="    # Small claims: $50, initiated online or at kiosks; mandatory mediation\n"
          "    # through the Dispute Resolution Center. District fee schedule eff.\n"
          "    # 12/08/2025. Waiver request fillable online; statewide GR 34 elsewhere.\n    forms: []"))
records.append(court("pierce_county_superior_court", "Pierce County Superior Court", "WA", "superior",
    None, PIERCE_SUP, county="Pierce", city="Tacoma",
    extra="    # Probate filing $290.00; small estate $20.00; will only $20.00; sealed\n"
          "    # will repository $20.00 (Clerk fee schedule eff. 6/11/2026). Sealed name\n"
          "    # changes must be heard here. Statewide GR 34 applies for waivers.\n    forms: []"))
records.append(court("clark_county_district_court", "Clark County District Court (Washington)", "WA", "district",
    None, CLARK_WA_DC, county="Clark",
    extra="    # Name changes and small claims ($50; clerk provides the Notice of Small\n"
          "    # Claim — no static PDF on the page). GR 34 fee waiver on request.\n    forms: []"))
records.append(court("clark_county_superior_court", "Clark County Superior Court (Washington)", "WA", "superior",
    None, CLARK_WA_SUP, county="Clark",
    extra="    # Sealed name changes and probate/wills/trusts live with the Clerk; civil\n"
          "    # filing $290.00 (Clerk fee schedule). Statewide GR 34 waiver forms apply.\n    forms: []"))
# --- New York ---
records.append(court("new_york_supreme_court", "New York Supreme Court", "NY", "superior",
    None, NY_SUPREME,
    extra="    # The Unified Court System's name/sex-designation forms are the same\n"
          "    # statewide whether filed in Supreme/County Court or NYC Civil Court;\n"
          "    # this record carries the Supreme/County Court route (fees and RJI\n"
          "    # note included). County Court accepts the same forms at the same fee.\n    forms: []"))
records.append(court("nyc_civil_court", "New York City Civil Court", "NY", "city",
    None, NYC_CIVIL, city="New York",
    extra="    # A NYC resident may bring a name or sex-designation change proceeding in\n"
          "    # any county in the city here ($65.00 vs $210.00 in Supreme Court). The\n"
          "    # Small Claims Part limit is $10,000 (individuals only; no e-filing).\n"
          "    # Commercial claims sit in the same part; their filing fee is not stated\n"
          "    # on the surveyed pages. Small-estate DIY (SE-3A) belongs to Surrogate's\n"
          "    # Court, below.\n    forms: []"))
records.append(court("new_york_surrogates_court", "New York Surrogate's Court", "NY", "surrogate",
    None, NY_SURR,
    extra="    # The probate self-help path for small estates (personal property up to\n"
          "    # $50,000) is the official Small Estate Affidavit DIY program (LawHelp\n"
          "    # Interactive). The paper SE-3A blank (SEAFF09.pdf) is live and its text\n"
          "    # was extracted in the survey, but its fillability could NOT be verified\n"
          "    # (Cloudflare + no Wayback binary), so it carries no entry here rather\n"
          "    # than a guessed one. Probate fees vary by estate under SCPA 2402.\n    forms: []"))
records.append(court("new_york_family_court", "New York Family Court", "NY", "family",
    None, NY_FAMILY,
    extra="    # No filing fees in Family Court. The family offense petition — the\n"
          "    # highest-volume DV self-help form in NY — is print-and-write, not an\n"
          "    # AcroForm; the order forms are court-issued. No DIY program exists for\n"
          "    # it (verified against the Family Court DIY page).\n    forms: []"))

out = HEADER + "\n".join([
    court("california_superior_court", "Superior Court of California", "CA", "superior", None, CA_STATEWIDE, extra=EXISTING_CA_FORMS),
    court("los_angeles_superior_court", "Los Angeles Superior Court", "CA", "superior", None, LA, county="Los Angeles",
        extra="    # No LA-specific fillable name-change blank exists; the statewide forms apply\n    # and the LACIV 264 packet is an instructions sheet.\n    forms: []"),
    court("san_diego_superior_court", "San Diego Superior Court", "CA", "superior", None, SD, county="San Diego",
        extra="    # San Diego wraps the statewide forms in local packets; the statewide\n    # Judicial Council blanks remain the operative filing targets.\n    forms: []"),
    court("san_francisco_superior_court", "San Francisco Superior Court", "CA", "superior", None, "", county="San Francisco",
        extra="    # The ACCESS Center (400 McAllister St, Room 509) helps fill RO forms (CH, DV,\n    # EA, GV, WV). SF adopts the Statewide Civil Fee Schedule eff. 1/1/2026 as its\n    # civil fee schedule, so the statewide $435/$450 figures and FW-001/FW-003\n    # waiver pairing apply unchanged; the court publishes A-Z guide PDFs (name and\n    # gender change, fee waiver) rather than local fillable blanks.\n    forms: []"),
    court("alameda_superior_court", "Alameda Superior Court", "CA", "superior", None, "", county="Alameda",
        extra="    # The Forms @ Home page routes self-represented litigants to Law Help\n    # Interactive guided interviews (DVRO, CHRO, EARO, name change, parentage,\n    # eviction, guardianship, guardianship fee waiver) — no local fillable\n    # blanks of its own; the statewide forms apply.\n    forms: []"),
    court("nevada_supreme_court", "Nevada Supreme Court", "NV", "supreme", None, NV_STATEWIDE, extra=EXISTING_NV_FORMS),
    court("eighth_judicial_district_court", "Eighth Judicial District Court", "NV", "district", None, EJDC, county="Clark",
        extra="    # Family Division (Family Law Self-Help Center) and Probate Division.\n    # The statewide Nevada Supreme Court forms (registered above) are accepted\n    # here; these rows carry only what is unique to Clark County. The adult\n    # name-change fee is $270.00 (+$3.50 per eFileNV upload), per the Family\n    # Law Self-Help Center. Clark's WordPress REST API is locked down (401),\n    # so form indexes are HTML only.\n    forms: []"),
    court("second_judicial_district_court", "Second Judicial District Court", "NV", "district", None, WASHOE_SJD, county="Washoe",
        extra="    # The Washoe NA adult name-change packet is Washoe-specific (not the\n    # statewide form). The NA-2 publication-waiver/seal motion and NM-1..NM-4\n    # minor packets live on the same forms tree with cache-busting URLs.\n    # The statewide Nevada Supreme Court forms (registered above) are accepted\n    # here; these rows carry only what is unique to Washoe County.\n    forms: []"),
    court("las_vegas_justice_court", "Las Vegas Justice Court", "NV", "justice", None, LVJC, county="Clark", city="Las Vegas",
        extra="    # Small claims forms are per-justice-court in Nevada; there is no\n    # statewide small-claims set.\n    forms: []"),
    court("reno_justice_court", "Reno Justice Court", "NV", "justice", None, RJC, county="Washoe", city="Reno"),
    court("washington_supreme_court", "Washington Supreme Court (statewide pattern forms)", "WA", "supreme", None, WA_STATEWIDE,
        extra="    # Statewide pattern-form library (courts.wa.gov/forms). The sampled pattern\n    # PDFs are NOT AcroForm-fillable — a fill tool must overlay fields on the\n    # flat PDFs or route through Guide & File; every form page also links an\n    # official Word version. Name changes and wills/probate have NO statewide\n    # pattern forms (county/local forms only; name changes may be filed in any\n    # district court per RCW 4.24.130). Sex-designation changes run through\n    # agency forms (DOH/DOL), not the courts.\n    forms: []"),
    court("king_county_district_court", "King County District Court", "WA", "district", None, KC_DC, county="King", city="Seattle",
        extra="    # King County's own local forms ARE fillable AcroForms, unlike the\n    # statewide pattern PDFs. eFiling portal: dcfiling.kingcounty.gov.\n    forms: []"),
    court("king_county_superior_court", "King County Superior Court", "WA", "superior", None, KC_SUP, county="King", city="Seattle",
        extra="    # Name changes auto-seal here under RCW 4.24.130(5) for gender identity,\n    # DV, refugee/asylum, and emancipated-minor filings. Superior filing fee\n    # $290.00 (civil action; fee schedule eff. 10/1/2026); statewide GR 34\n    # applies for waivers. Protection orders: Superior or District Court.\n    forms: []"),
    court("pierce_county_district_court", "Pierce County District Court", "WA", "district", None, PIERCE_DC, county="Pierce", city="Tacoma",
        extra="    # Small claims: $50, initiated online or at kiosks; mandatory mediation\n    # through the Dispute Resolution Center. District fee schedule eff.\n    # 12/08/2025. Waiver request fillable online; statewide GR 34 elsewhere.\n    forms: []"),
    court("pierce_county_superior_court", "Pierce County Superior Court", "WA", "superior", None, PIERCE_SUP, county="Pierce", city="Tacoma",
        extra="    # Probate filing $290.00; small estate $20.00; will only $20.00; sealed\n    # will repository $20.00 (Clerk fee schedule eff. 6/11/2026). Sealed name\n    # changes must be heard here. Statewide GR 34 applies for waivers.\n    forms: []"),
    court("clark_county_district_court", "Clark County District Court (Washington)", "WA", "district", None, CLARK_WA_DC, county="Clark",
        extra="    # Name changes and small claims ($50; clerk provides the Notice of Small\n    # Claim — no static PDF on the page). GR 34 fee waiver on request.\n    forms: []"),
    court("clark_county_superior_court", "Clark County Superior Court (Washington)", "WA", "superior", None, CLARK_WA_SUP, county="Clark",
        extra="    # Sealed name changes and probate/wills/trusts live with the Clerk; civil\n    # filing $290.00 (Clerk fee schedule). Statewide GR 34 waiver forms apply.\n    forms: []"),
    court("new_york_supreme_court", "New York Supreme Court", "NY", "superior", None, NY_SUPREME,
        extra="    # The Unified Court System's name/sex-designation forms are the same\n    # statewide whether filed in Supreme/County Court or NYC Civil Court;\n    # this record carries the Supreme/County Court route (fees and RJI\n    # note included). County Court accepts the same forms at the same fee.\n    forms: []"),
    court("nyc_civil_court", "New York City Civil Court", "NY", "city", None, NYC_CIVIL, city="New York",
        extra="    # A NYC resident may bring a name or sex-designation change proceeding in\n    # any county in the city here ($65.00 vs $210.00 in Supreme Court). The\n    # Small Claims Part limit is $10,000 (individuals only; no e-filing).\n    # Commercial claims sit in the same part; their filing fee is not stated\n    # on the surveyed pages. Small-estate DIY (SE-3A) belongs to Surrogate's\n    # Court, below.\n    forms: []"),
    court("new_york_surrogates_court", "New York Surrogate's Court", "NY", "surrogate", None, NY_SURR,
        extra="    # The probate self-help path for small estates (personal property up to\n    # $50,000) is the official Small Estate Affidavit DIY program (LawHelp\n    # Interactive). The paper SE-3A blank (SEAFF09.pdf) is live and its text\n    # was extracted in the survey, but its fillability could NOT be verified\n    # (Cloudflare + no Wayback binary), so it carries no entry here rather\n    # than a guessed one. Probate fees vary by estate under SCPA 2402.\n    forms: []"),
    court("new_york_family_court", "New York Family Court", "NY", "family", None, NY_FAMILY,
        extra="    # No filing fees in Family Court. The family offense petition — the\n    # highest-volume DV self-help form in NY — is print-and-write, not an\n    # AcroForm; the order forms are court-issued. No DIY program exists for\n    # it (verified against the Family Court DIY page).\n    forms: []"),
])
# append trailing newline
# restore provenance comments that lived with the original records
ca_comments = """  # California's Judicial Council statewide name-change and gender-recognition
  # blanks are accepted in every county's superior court; the first-paper
  # filing fee varies by county, so those rows carry a fee note rather than a
  # single amount.
"""
nv_comments = """  # Statewide form sets are registered under the court that approves them:
  # the Nevada Supreme Court's self-help forms are acceptable in any court
  # in Nevada, so the county-specific rows below carry only the forms and
  # fees unique to that county.
"""
forms_comments = """    # One entry per form this court accepts, keyed to the template code:
    #
    # forms:
    #   - code: ca__name_change_petition
    #     title: Petition for Change of Name
    #     form_number: NC-100          # official form number, when the office publishes one
    #     source_url: https://www.courts.ca.gov/documents/NC100.pdf
    #     sha256: 0000000000000000000000000000000000000000000000000000000000000000
    #     filing_office: Superior Court Clerk's Office in the petitioner's county of residence
    #     filing_fee:                  # flat fee; omit when the office publishes none
    #       amount: "435.00"
    #       currency: USD
    #     fee_note: null               # tiered or county-varying schedules go here as prose
    #     fee_waiver_code: ca__fee_waiver_request
"""
out = out.replace("  - key: california_superior_court", ca_comments + "  - key: california_superior_court", 1)
out = out.replace("  - key: nevada_supreme_court", nv_comments + "  - key: nevada_supreme_court", 1)
out = out.replace(ca_comments + "  - key: california_superior_court\n    name: Superior Court of California\n    jurisdiction: CA\n    level: superior\n    forms:",
                  ca_comments + "  - key: california_superior_court\n    name: Superior Court of California\n    jurisdiction: CA\n    level: superior\n" + forms_comments + "    forms:", 1)
nv_forms_comments = """    # One entry per form this court accepts, keyed to the template code:
    #
    # forms:
    #   - code: nv__adult_name_change_petition
    #     title: Petition for Adult Name Change
    #     form_number: null            # official form number, when the office publishes one
    #     source_url: https://selfhelp.nvcourts.gov/images/name-change/name-adult-petition-pdf-fillable.pdf
    #     sha256: 0000000000000000000000000000000000000000000000000000000000000000
    #     filing_office: Family Division Clerk's Office
    #     filing_fee:                  # flat fee; omit when the office publishes none
    #       amount: "270.00"
    #       currency: USD
    #     fee_note: null               # tiered or county-varying schedules go here as prose
    #     fee_waiver_code: nv__fee_waiver_application
"""
out = out.replace(nv_comments + "  - key: nevada_supreme_court\n    name: Nevada Supreme Court\n    jurisdiction: NV\n    level: supreme\n    forms:",
                  nv_comments + "  - key: nevada_supreme_court\n    name: Nevada Supreme Court\n    jurisdiction: NV\n    level: supreme\n" + nv_forms_comments + "    forms:", 1)
import re as _re
_lines = []
for _line in out.rstrip().splitlines():
    _pat = '^(' + chr(92) + 's*(?:- )?[A-Za-z_][A-Za-z0-9_]*: )(.*)$'
    _m = _re.match(_pat, _line)
    if _m and ': ' in _m.group(2) and not _re.match('^["' + chr(39) + chr(92) + '[{>|&*]', _m.group(2)):
        _line = _m.group(1) + '"' + _m.group(2).replace(chr(92), chr(92)+chr(92)).replace('"', chr(92)+'"') + '"'
    _lines.append(_line)
open('/home/user/work/court-registry/store/seeds/Court.yaml', 'w').write(chr(10).join(_lines) + chr(10))
print("written")
