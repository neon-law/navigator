//! The court seeds: the checked-in court-form registry as Rust data.
//!
//! `COURT_SEEDS` carries, per court and per form entry, the provenance a
//! filer needs: what the filing office calls the form, where the official
//! blank is published, the sha-256 pin of that blank, where in the
//! courthouse the filing goes, what it costs, and which fee-waiver form the
//! office accepts — plus the surveyed-but-unsheeted inventory in
//! `surveyed_forms`. This is the single source of truth; `store::courts`
//! validates it and the public catalog serves it, and
//! `cli/tests/court_registry.rs` fails CI when it and the forms shelf
//! drift apart.
//!
//! # No table, deliberately
//!
//! This is compile-time reference data, not seed state: fee data must not
//! become part of every environment's database, and no seed file exists for
//! it at all.
//!
//! # The join contract
//!
//! A `forms:` entry's `code` is a template code on the forms shelf
//! (`templates/notations/forms/…`): the same code the template's `form:`
//! frontmatter carries and, once the form is vendored, the `forms` crate's
//! `FormMeta.code`. The guard test pins all three together — every entry
//! resolves to a real template, and a vendored `FormMeta`'s URL, pin, and
//! fee fields must equal the registry row's. The reverse direction keeps
//! the shelf honest too: a template under a court directory with no
//! registry entry is an orphan, and orphans fail CI.
//!
//! # Provenance (populated 2026-10-31 from the four verified regional
//! surveys of 2026-10-30 — see the project artifacts
//! "California/Washington/Nevada/New York Court Self-Help Form Inventory")
//!
//! - Every `forms:` entry is a template-backed form (the 12 templates from
//!   the notation wave); everything else lives in `surveyed_forms:`, which
//!   is registry data only — the same provenance fields, no template join.
//! - Every sha256 was computed from the official blank downloaded
//!   2026-10-31 and is never the bytes themselves. New York blanks are
//!   pinned from Wayback-archived copies (nycourts.gov serves a Cloudflare
//!   bot challenge to plain clients); the live URLs are the ones the UCS
//!   pages link today.
//! - `fillability` is the survey's binary AcroForm check: `Fillable`
//!   (field dictionary carries widgets), `UnfillableSource` (0 fields,
//!   whatever the filename claims), or `Unverified` (not binary-checked).
//! - `fee_waiver_code` stays `None` throughout: the guard test resolves it
//!   to a forms-shelf template, and no fee-waiver template is vendored
//!   yet — the waiver form each office accepts is named in the fee note
//!   instead.
//! - CA Drupal JSON:API harvest: the root endpoint
//!   (https://selfhelp.courts.ca.gov/jsonapi) was live on 2026-10-31 but
//!   returns an empty default collection, and content-type probes beneath
//!   it serve HTML, not JSON:API envelopes — no form index to harvest; the
//!   2026-10-30 survey remains the freshest data for California.
//! - Explicitly unknown (survey-flagged, not guessed): the Washoe
//!   name-change filing fee; NY SE-3A small-estate affidavit fillability
//!   (page-claimed fillable, never binary-verified, no Wayback copy — so
//!   it carries no entry at all); the NY commercial-claims filing fee; the
//!   CA civil-harassment filing fee; WA statewide PO fees beyond the
//!   Pierce schedule's lines. Clark's "Fillable-Affidavit-of-Entitlement"
//!   is registered as an unfillable source.
//! - Agency forms (WA DOH birth-certificate sex-designation forms
//!   422-143/422-144 — flat; DOL 520043 — fillable; the NV DPBH
//!   vital-records affidavit route) are not court filings and are not
//!   registered here.
//! - Pierce County's name-change/small-claims petitions and the WA
//!   statewide PO 003A are delivered through portal pages with no stable
//!   static PDF captured; they are described in notes rather than
//!   registered.

use std::sync::LazyLock;

use crate::courts::{Court, CourtForm, CourtLevel, FilingFee, Fillability, SurveyedForm};

/// The court seeds, one [`Court`] per court or statewide form-set approver.
pub static COURT_SEEDS: LazyLock<Vec<Court>> = LazyLock::new(|| {
    vec![
    Court {
        key: "california_superior_court".into(),
        name: "Superior Court of California".into(),
        jurisdiction: "CA".into(),
        county: None,
        city: None,
        level: CourtLevel::Superior,
        forms: vec![
            CourtForm {
            code: "ca__name_change_petition".into(),
            title: "Petition for Change of Name".into(),
form_number: Some("NC-100".into()),
            source_url: "https://www.courts.ca.gov/documents/NC100.pdf".into(),
            sha256: "cab3f83b70ac2181342fcea77659d5eeeecaac57346f5e6f5d64267777e530cf".into(),
            filing_office: "Superior Court Clerk's Office in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: Some("First-paper filing fee varies by county (the published statewide range is $435–$450); fee-waiver paperwork is out of scope for this wave.".into()),
            fee_waiver_code: None,

            },
            CourtForm {
            code: "ca__name_change_attachment".into(),
            title: "Name and Information About the Person Whose Name is to be Changed".into(),
form_number: Some("NC-110".into()),
            source_url: "https://www.courts.ca.gov/documents/NC110.pdf".into(),
            sha256: "caee87d9bc922e5353b05ee91f39261c61f8b22781770d9e778d0d06e1fddd6b".into(),
            filing_office: "Filed with the NC-100 petition at the Superior Court Clerk's Office".into(),
            filing_fee: None,
            fee_note: None,
            fee_waiver_code: None,

            },
            CourtForm {
            code: "ca__name_change_order_to_show_cause".into(),
            title: "Order to Show Cause—Change of Name".into(),
form_number: Some("NC-120".into()),
            source_url: "https://www.courts.ca.gov/documents/NC120.pdf".into(),
            sha256: "aaf64476c94c5a1c1bf20eaea8303c889fbd0f2977f0fdcd840506db013c1339".into(),
            filing_office: "Filed with the NC-100 petition at the Superior Court Clerk's Office".into(),
            filing_fee: None,
            fee_note: None,
            fee_waiver_code: None,

            },
            CourtForm {
            code: "ca__name_change_decree".into(),
            title: "Decree Changing Name".into(),
form_number: Some("NC-130".into()),
            source_url: "https://www.courts.ca.gov/documents/NC130.pdf".into(),
            sha256: "0fea5cd809e737607c829cb03ba1d38d93405d729ee4eff808edeaf9187a1fce".into(),
            filing_office: "Entered by the Superior Court after the hearing".into(),
            filing_fee: None,
            fee_note: None,
            fee_waiver_code: None,

            },
            CourtForm {
            code: "ca__civil_case_cover_sheet".into(),
            title: "Civil Case Cover Sheet".into(),
form_number: Some("CM-010".into()),
            source_url: "https://www.courts.ca.gov/documents/CM010.pdf".into(),
            sha256: "2e97e4010e077b4d2da33847802806e33b7bee0a39f020fb7b3b6be644f37c55".into(),
            filing_office: "Filed with the NC-100 petition at the Superior Court Clerk's Office".into(),
            filing_fee: None,
            fee_note: None,
            fee_waiver_code: None,

            },
            CourtForm {
            code: "ca__gender_identity_name_change_petition".into(),
            title: "Petition for Change of Name to Conform to Gender Identity".into(),
form_number: Some("NC-200".into()),
            source_url: "https://www.courts.ca.gov/documents/NC200.pdf".into(),
            sha256: "75430d1d5cc37e45b9627376c75dbd2b0d47ed1285cb590ff29dbd93aef325bf".into(),
            filing_office: "Superior Court Clerk's Office in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: Some("First-paper filing fee varies by county (the published statewide range is $435–$450); fee-waiver paperwork is out of scope for this wave.".into()),
            fee_waiver_code: None,

            },
            CourtForm {
            code: "ca__gender_identity_name_change_order_to_show_cause".into(),
            title: "Order to Show Cause—Change of Name to Conform to Gender Identity".into(),
form_number: Some("NC-225".into()),
            source_url: "https://www.courts.ca.gov/documents/NC225.pdf".into(),
            sha256: "359fa6c36dc2290fc12325c2c20b939e51a2409c9b65786d2ecc91da80f726ff".into(),
            filing_office: "Filed with the NC-200 petition at the Superior Court Clerk's Office".into(),
            filing_fee: None,
            fee_note: None,
            fee_waiver_code: None,

            },
            CourtForm {
            code: "ca__gender_identity_name_change_decree".into(),
            title: "Decree Changing Name to Conform to Gender Identity".into(),
form_number: Some("NC-230".into()),
            source_url: "https://www.courts.ca.gov/documents/NC230.pdf".into(),
            sha256: "b9c31d8983721bd05fb1dd81a47b5e38ec490e6b1050b2d6c0f862ce5b64a135".into(),
            filing_office: "Entered by the Superior Court after the hearing".into(),
            filing_fee: None,
            fee_note: None,
            fee_waiver_code: None,

            },
            CourtForm {
            code: "ca__gender_recognition_petition".into(),
            title: "Petition for Recognition of Adult's Change of Gender and Sex Identifier, Name Change, and Issuance of New Certificates".into(),
form_number: Some("NC-300".into()),
            source_url: "https://www.courts.ca.gov/documents/NC300.pdf".into(),
            sha256: "2a1daab42173637b2a89ad704c1cff9345f34e578f47f115de09872efefd1da9".into(),
            filing_office: "Superior Court Clerk's Office in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: Some("First-paper filing fee varies by county (the published statewide range is $435–$450); the petition is confidential and fee-waiver paperwork is out of scope for this wave.".into()),
            fee_waiver_code: None,

            },
            CourtForm {
            code: "ca__gender_recognition_order".into(),
            title: "Order Recognizing Adult's Change of Gender and Sex Identifier, for Name Change and for Issuance of New Certificates".into(),
form_number: Some("NC-330".into()),
            source_url: "https://www.courts.ca.gov/documents/NC330.pdf".into(),
            sha256: "fd1d3e765ae0c77199453020e53b825ea311ab9423a4021b21f44c5d3a67f634".into(),
            filing_office: "Entered by the Superior Court after the hearing".into(),
            filing_fee: None,
            fee_note: None,
            fee_waiver_code: None,

            },
        ],
        surveyed_forms: vec![
            SurveyedForm {
            key: "nc100info".into(),
            title: "Instructions for Filing a Petition for Change of Name".into(),
form_number: Some("NC-100-INFO".into()),
            source_url: "https://www.courts.ca.gov/documents/NC100INFO.pdf".into(),
            sha256: "305137cb24c5838a153313d33d9af29621940c16b960b04dfbd178ea16ab4635".into(),
            filing_office: "Superior Court Clerk's Office in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: Some("n/a (information sheet). First-paper filing fee varies by county (the published statewide range is $435-$450); the accepted fee-waiver paperwork is FW-001 with the FW-003 order, which has no template code on the forms shelf yet.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "nc400info".into(),
            title: "Information Sheet for Name Change Proceedings Under Address Confidentiality Program (Safe at Home)".into(),
form_number: Some("NC-400-INFO".into()),
            source_url: "https://www.courts.ca.gov/documents/NC400INFO.pdf".into(),
            sha256: "000feab817ba5f76ccaf1faa28d9ce80af3a4e7d6d7aece3c3dbc5998ee5e42e".into(),
            filing_office: "Superior Court Clerk's Office in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: Some("n/a (information sheet).".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "nc200info".into(),
            title: "Instructions for the Petition for Change of Name to Conform to Gender Identity".into(),
form_number: Some("NC-200-INFO".into()),
            source_url: "https://www.courts.ca.gov/documents/NC200INFO.pdf".into(),
            sha256: "781f44e9ba20cc361111d810317188b42dd63e0276b0a01bb3c77000766c72bb".into(),
            filing_office: "Superior Court Clerk's Office in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: Some("n/a (information sheet). First-paper filing fee varies by county (the published statewide range is $435-$450); the accepted fee-waiver paperwork is FW-001 with the FW-003 order, which has no template code on the forms shelf yet.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "nc312".into(),
            title: "Marriage License and Certificate—Attachment".into(),
form_number: Some("NC-312".into()),
            source_url: "https://www.courts.ca.gov/documents/NC312.pdf".into(),
            sha256: "07f52c90bce99b8dad9a2d2cb419e0e3dc81b7d1f348dfcb2ebfbb1243bfce85".into(),
            filing_office: "Superior Court Clerk's Office in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: Some("First-paper filing fee varies by county (the published statewide range is $435-$450); the accepted fee-waiver paperwork is FW-001 with the FW-003 order, which has no template code on the forms shelf yet.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "nc325".into(),
            title: "Order to Show Cause—Issuance of New Marriage License and Certificate".into(),
form_number: Some("NC-325".into()),
            source_url: "https://www.courts.ca.gov/documents/NC325.pdf".into(),
            sha256: "99dd5332bcfaa4596bda59c39eeed7f55e7cff80a76a65f18b873a4c15831a5e".into(),
            filing_office: "Superior Court Clerk's Office in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: Some("First-paper filing fee varies by county (the published statewide range is $435-$450); the accepted fee-waiver paperwork is FW-001 with the FW-003 order, which has no template code on the forms shelf yet.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "nc500info".into(),
            title: "Information Sheet: Petition for Recognition of Minor's Change of Gender and Sex Identifier".into(),
form_number: Some("NC-500-INFO".into()),
            source_url: "https://www.courts.ca.gov/documents/NC500INFO.pdf".into(),
            sha256: "29c1e9d7a4ecc54c7af1700b30a966edc77e22cf0f9021aafbd05769cdcc11d8".into(),
            filing_office: "Superior Court Clerk's Office in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: Some("n/a (information sheet); NC-500-series petition PDFs follow the same documents/ URL pattern.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "dv100".into(),
            title: "Request for Domestic Violence Restraining Order".into(),
form_number: Some("DV-100".into()),
            source_url: "https://www.courts.ca.gov/documents/DV100.pdf".into(),
            sha256: "6ff055a34d989ec691fd9e6533e6a25aafa59342455438ebedd29ce0ed36a958".into(),
            filing_office: "Superior Court Clerk's Office or courthouse self-help center in the petitioner's county of residence".into(),
            filing_fee: Some(FilingFee {
                amount: "0.00".into(),
                currency: "USD".into(),
            }),
            fee_note: Some("There is no court fee to file for a domestic violence restraining order.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "dv109".into(),
            title: "Notice of Court Hearing".into(),
form_number: Some("DV-109".into()),
            source_url: "https://www.courts.ca.gov/documents/DV109.pdf".into(),
            sha256: "e32dd4fac6e53429ba2a1dbc750d0084fdd6f4f97b34b16cf90098e600c39569".into(),
            filing_office: "Superior Court Clerk's Office or courthouse self-help center in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "dv110".into(),
            title: "Temporary Restraining Order".into(),
form_number: Some("DV-110".into()),
            source_url: "https://www.courts.ca.gov/documents/DV110.pdf".into(),
            sha256: "55f28920e87d6e02511c3070f667cc8af24c35e8bcbd351c798aab5779df5967".into(),
            filing_office: "Superior Court Clerk's Office or courthouse self-help center in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "dv120".into(),
            title: "Response to Request for Domestic Violence Restraining Order".into(),
form_number: Some("DV-120".into()),
            source_url: "https://www.courts.ca.gov/documents/DV120.pdf".into(),
            sha256: "3728758b79486eb3aa35b6c84d0b0a4190f9ad2c38a9f3855c760d6e7f0252ec".into(),
            filing_office: "Superior Court Clerk's Office or courthouse self-help center in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "dv130".into(),
            title: "Restraining Order After Hearing".into(),
form_number: Some("DV-130".into()),
            source_url: "https://www.courts.ca.gov/documents/DV130.pdf".into(),
            sha256: "9d7e52d8a9b2a44cfb83aac85299273292bd6e206333bfca68c57a71a84faf64".into(),
            filing_office: "Superior Court Clerk's Office or courthouse self-help center in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "dv200".into(),
            title: "Confidential Contact Information (screening)".into(),
form_number: Some("DV-200".into()),
            source_url: "https://www.courts.ca.gov/documents/DV200.pdf".into(),
            sha256: "c02b4aa46365ff8724236e573b7c618cfe1060dd0775e905b772e865d160581d".into(),
            filing_office: "Superior Court Clerk's Office or courthouse self-help center in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "dv250".into(),
            title: "Proof of Service by Mail".into(),
form_number: Some("DV-250".into()),
            source_url: "https://www.courts.ca.gov/documents/DV250.pdf".into(),
            sha256: "6425637127086701a4e6b818063ae64926c3887e35720e1ef5c559f03b02f5a4".into(),
            filing_office: "Superior Court Clerk's Office or courthouse self-help center in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "dv300".into(),
            title: "Request to Change or End Restraining Order".into(),
form_number: Some("DV-300".into()),
            source_url: "https://www.courts.ca.gov/documents/DV300.pdf".into(),
            sha256: "f639648ec222fa14283d2f0bb4d0c46f6abac39a37802e0b2a4938a3c15481ab".into(),
            filing_office: "Superior Court Clerk's Office or courthouse self-help center in the petitioner's county of residence".into(),
            filing_fee: Some(FilingFee {
                amount: "0.00".into(),
                currency: "USD".into(),
            }),
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "dv305".into(),
            title: "Request to Change Child Custody and Visitation Orders".into(),
form_number: Some("DV-305".into()),
            source_url: "https://www.courts.ca.gov/documents/DV305.pdf".into(),
            sha256: "f685e88ee77c7fac66fa9f786523ea32b2066b18669c46bf60689bed315aa2b5".into(),
            filing_office: "Superior Court Clerk's Office or courthouse self-help center in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "dv310".into(),
            title: "Notice of Court Hearing and Temporary Order to Change or End Restraining Order".into(),
form_number: Some("DV-310".into()),
            source_url: "https://www.courts.ca.gov/documents/DV310.pdf".into(),
            sha256: "e9a29acb02496ae8753660a5467b11442e7ab6d267d2051e3d19d600a2d06ea0".into(),
            filing_office: "Superior Court Clerk's Office or courthouse self-help center in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "dv530info".into(),
            title: "How to Enforce Your Restraining Order".into(),
form_number: Some("DV-530-INFO".into()),
            source_url: "https://www.courts.ca.gov/documents/DV530INFO.pdf".into(),
            sha256: "98e7b4b2323a559dda41cbe7cd805a1f5fe369c3fc10d3d1c1bb0527934a1978".into(),
            filing_office: "Superior Court Clerk's Office or courthouse self-help center in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: Some("n/a (information sheet); fillability asserted on the self-help page but not binary-verified in the survey.".into()),
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "ser001".into(),
            title: "Request for Sheriff to Serve Court Papers".into(),
form_number: Some("SER-001".into()),
            source_url: "https://www.courts.ca.gov/documents/SER001.pdf".into(),
            sha256: "8197529a8646a662fbd7e89bdea1a06f61a9b9c4d75bfc8a4d8f795c62475893".into(),
            filing_office: "Superior Court Clerk's Office or courthouse self-help center in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: Some("The sheriff serves DV restraining-order papers free.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "ch100".into(),
            title: "Request for Civil Harassment Restraining Orders".into(),
form_number: Some("CH-100".into()),
            source_url: "https://www.courts.ca.gov/documents/CH100.pdf".into(),
            sha256: "4a2c52233d85489857cbafd4e66bd891e075a00060e0813701b34fdb11b804c8".into(),
            filing_office: "Superior Court Clerk's Office or courthouse self-help center in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: Some("Filing fee was not stated on any page surveyed (2026-10-30); any court fee can be waived with FW-001, which has no template code on the forms shelf yet.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "ch109".into(),
            title: "Notice of Court Hearing".into(),
form_number: Some("CH-109".into()),
            source_url: "https://www.courts.ca.gov/documents/CH109.pdf".into(),
            sha256: "8f02eb6d79fa00299a5a21850b691bca0ff20c23f15871c3857f48fbef13e4bd".into(),
            filing_office: "Superior Court Clerk's Office or courthouse self-help center in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "ch110".into(),
            title: "Temporary Restraining Order".into(),
form_number: Some("CH-110".into()),
            source_url: "https://www.courts.ca.gov/documents/CH110.pdf".into(),
            sha256: "d6ba3ae73c0c1bb91d1460beea46ec9694a7be7f68c692d50979fe1def460960".into(),
            filing_office: "Superior Court Clerk's Office or courthouse self-help center in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "ch120".into(),
            title: "Response to Request".into(),
form_number: Some("CH-120".into()),
            source_url: "https://www.courts.ca.gov/documents/CH120.pdf".into(),
            sha256: "c7d20724e7aa96d8190ad5f9eb6931331bc4993ccb8eae459dc01e297e16d5e0".into(),
            filing_office: "Superior Court Clerk's Office or courthouse self-help center in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "sc100".into(),
            title: "Plaintiff's Claim and ORDER to Go to Small Claims Court".into(),
form_number: Some("SC-100".into()),
            source_url: "https://www.courts.ca.gov/documents/SC100.pdf".into(),
            sha256: "ef3421b14ebf64dbf884566ff659b39776035ec5b6e6500be0af91e3cc15533c".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: Some("Filing fee is tiered by claim amount (Statewide Civil Fee Schedule eff. 1/1/2026): $30 up to $1,500; $50 for $1,501-$5,000; $75 for $5,001-$12,500; $100 for a 13th-or-later claim in 12 months; $75 notice of appeal. Jurisdiction: $12,500 individuals / $6,250 businesses.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "sc100a".into(),
            title: "Other Plaintiffs or Defendants (attachment)".into(),
form_number: Some("SC-100A".into()),
            source_url: "https://www.courts.ca.gov/documents/SC100A.pdf".into(),
            sha256: "c7cd9667cfa0508f62a34d4bc5f18be32d1ed30ae3447b9130903968403e38b5".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "sc101".into(),
            title: "Attorney Fee Dispute (After Arbitration) (attachment)".into(),
form_number: Some("SC-101".into()),
            source_url: "https://www.courts.ca.gov/documents/SC101.pdf".into(),
            sha256: "1d667a9a430019667bbe9c2024af0ebf867c9a2e26abaa15fff7a057a8239a0f".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "sc103".into(),
            title: "Fictitious Business Name".into(),
form_number: Some("SC-103".into()),
            source_url: "https://www.courts.ca.gov/documents/SC103.pdf".into(),
            sha256: "27ffd342b5869754b9484cb87ff632e4245e3f24b0b0326fba8d592790ba0759".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "sc104".into(),
            title: "Proof of Service (small claims)".into(),
form_number: Some("SC-104".into()),
            source_url: "https://www.courts.ca.gov/documents/SC104.pdf".into(),
            sha256: "d2e4b384f91948b71cc0d331f41133e2893bb552dadd761616844ffc41c1689e".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "sc105".into(),
            title: "Request for Court Order and Answer".into(),
form_number: Some("SC-105".into()),
            source_url: "https://www.courts.ca.gov/documents/SC105.pdf".into(),
            sha256: "ddc0593778b4b5df4384b4e05c1210d86bbc8bf1c5764d21404e87b20ee1749c".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "sc107".into(),
            title: "Small Claims Subpoena for Personal Appearance and Production of Documents at Trial or Hearing and Declaration".into(),
form_number: Some("SC-107".into()),
            source_url: "https://www.courts.ca.gov/documents/SC107.pdf".into(),
            sha256: "76b960d72aa64a8745239c0ae4bd4ac4a80b17309ad738c3b3388b1887cbbcf7".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "sc109".into(),
            title: "Authorization to Appear on Behalf of Party".into(),
form_number: Some("SC-109".into()),
            source_url: "https://www.courts.ca.gov/documents/SC109.pdf".into(),
            sha256: "425c4d735c8446f37b8f8a43c3dd44f1b756e7d98fcbcd9b629eb555bdd866f5".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "sc112a".into(),
            title: "Proof of Service by Mail (small claims)".into(),
form_number: Some("SC-112A".into()),
            source_url: "https://www.courts.ca.gov/documents/SC112A.pdf".into(),
            sha256: "c876c8350ec39496110a68ca605914648a537b765b45c7721b3c239f7d7f4b76".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "sc114".into(),
            title: "Request to Amend Party Name Before Hearing".into(),
form_number: Some("SC-114".into()),
            source_url: "https://www.courts.ca.gov/documents/SC114.pdf".into(),
            sha256: "7f35a9b5c57ed92b0c06c6fa9a6316938ce5392765bbd49b68b67bff09ca20a0".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "sc120".into(),
            title: "Defendant's Claim and ORDER to Go to Small Claims Court".into(),
form_number: Some("SC-120".into()),
            source_url: "https://www.courts.ca.gov/documents/SC120.pdf".into(),
            sha256: "f79851b27e429e10e533ad3d8d797ce49e8d78968df4f4653da4decf60d473d6".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "sc133".into(),
            title: "Judgment Debtor's Statement of Assets".into(),
form_number: Some("SC-133".into()),
            source_url: "https://www.courts.ca.gov/documents/SC133.pdf".into(),
            sha256: "51e3ad148ff50ab79e5ed09778b99c14f78cebbe2dd2ab4d4dc60183c32148eb".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "sc134".into(),
            title: "Order to Produce Statement of Assets and to Appear for Examination".into(),
form_number: Some("SC-134".into()),
            source_url: "https://www.courts.ca.gov/documents/SC134.pdf".into(),
            sha256: "1a5b731c21cdf0e7d73e8b3c6cb200d95131a586e1dd64eee1e4469cf8b624e7".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "sc135".into(),
            title: "Notice of Motion to Vacate Judgment and Declaration".into(),
form_number: Some("SC-135".into()),
            source_url: "https://www.courts.ca.gov/documents/SC135.pdf".into(),
            sha256: "d61295aeef4fd9c2581a23c888751ba17c95071b6520535a1b8b84e4d0d6df55".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "sc145".into(),
            title: "Request to Pay Judgment to Court".into(),
form_number: Some("SC-145".into()),
            source_url: "https://www.courts.ca.gov/documents/SC145.pdf".into(),
            sha256: "52c233bbd40cc649d33aede307a84fc3f3a933ee66c54c83e45c256cb89d9cbd".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "sc150".into(),
            title: "Request to Postpone Trial".into(),
form_number: Some("SC-150".into()),
            source_url: "https://www.courts.ca.gov/documents/SC150.pdf".into(),
            sha256: "adf8cf64a21d1d0202d8d028782b0dd4d2b6940d8e0d5dfa1243e87374f2db0c".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "sc220".into(),
            title: "Request to Make Payments".into(),
form_number: Some("SC-220".into()),
            source_url: "https://www.courts.ca.gov/documents/SC220.pdf".into(),
            sha256: "1d028f02c0ae44da5c4515ea27b0643d6874f567da14ff6f2c3e4b1984a3ad83".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "sc290".into(),
            title: "Acknowledgment of Satisfaction of Judgment".into(),
form_number: Some("SC-290".into()),
            source_url: "https://www.courts.ca.gov/documents/SC290.pdf".into(),
            sha256: "643103daa8dbe826cf4f2cd2458d314d96ed3a06f4766761ebd87e80858a0093".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "civ110".into(),
            title: "Request for Dismissal".into(),
form_number: Some("CIV-110".into()),
            source_url: "https://www.courts.ca.gov/documents/CIV110.pdf".into(),
            sha256: "95f8d4a98e5bf4d5383b62886413f54b1e8b9e59e4be1daa9d2e26662e64e774".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "subp025".into(),
            title: "Notice to Consumer or Employee and Objection".into(),
form_number: Some("SUBP-025".into()),
            source_url: "https://www.courts.ca.gov/documents/SUBP025.pdf".into(),
            sha256: "6d3c10db414dccdbfd738340320d0795ae34aa37b7e4e066dbf244e75e19061d".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "ej130".into(),
            title: "Writ of Execution".into(),
form_number: Some("EJ-130".into()),
            source_url: "https://www.courts.ca.gov/documents/EJ130.pdf".into(),
            sha256: "c90f4938097db8f56b602b8f8d3d21379beb56129f5dac2ad6b35b3029316859".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "ej001".into(),
            title: "Abstract of Judgment—Civil and Small Claims".into(),
form_number: Some("EJ-001".into()),
            source_url: "https://www.courts.ca.gov/documents/EJ001.pdf".into(),
            sha256: "375285721953da9883d2f7b6b4cefc19979d8dffafb035ce78a4e87b1a2d3c08".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "wg001".into(),
            title: "Application for Earnings Withholding Order (Wage Garnishment)".into(),
form_number: Some("WG-001".into()),
            source_url: "https://www.courts.ca.gov/documents/WG001.pdf".into(),
            sha256: "bc7037b78f10718be3fae7aacbb64359ff9ff54ed8e6d1b526548908ef2dd4e9".into(),
            filing_office: "Superior Court Clerk's Office, small claims division, in the plaintiff's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "de111".into(),
            title: "Petition for Probate".into(),
form_number: Some("DE-111".into()),
            source_url: "https://www.courts.ca.gov/documents/DE111.pdf".into(),
            sha256: "2a5759e5c7c0818003eaefc94dfade97625e7e3197675629fc00ae245fd59e14".into(),
            filing_office: "Superior Court Clerk's Office, probate division, in the decedent's county".into(),
            filing_fee: Some(FilingFee {
                amount: "435.00".into(),
                currency: "USD".into(),
            }),
            fee_note: Some("Probate first-paper fee of $435 under GC 70658(a) (county surcharges can push the total to $450); fee waiver FW-001 + FW-003 applies but has no template code on the forms shelf yet. The small-estate affidavit process (estates up to $184,500) uses no statewide court form.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "de121".into(),
            title: "Notice of Petition to Administer Estate".into(),
form_number: Some("DE-121".into()),
            source_url: "https://www.courts.ca.gov/documents/DE121.pdf".into(),
            sha256: "e03ae6f25fefac8838af6977c2dd70c6a61c365a78c7055d73ea43b81d4741f5".into(),
            filing_office: "Superior Court Clerk's Office, probate division, in the decedent's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "de140".into(),
            title: "Order for Probate".into(),
form_number: Some("DE-140".into()),
            source_url: "https://www.courts.ca.gov/documents/DE140.pdf".into(),
            sha256: "2f3faf014c87e7bc4dad4a9b7a6cbab90529519dbffdb9d0b2986024b87af3a3".into(),
            filing_office: "Superior Court Clerk's Office, probate division, in the decedent's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "de147".into(),
            title: "Duties and Liabilities of Personal Representative".into(),
form_number: Some("DE-147".into()),
            source_url: "https://www.courts.ca.gov/documents/DE147.pdf".into(),
            sha256: "4b8e4707f2cd2e0ff4d4d11891e6468dfc061e9f4cbb8408507a2c49ba848762".into(),
            filing_office: "Superior Court Clerk's Office, probate division, in the decedent's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "de160".into(),
            title: "Inventory and Appraisal".into(),
form_number: Some("DE-160".into()),
            source_url: "https://www.courts.ca.gov/documents/DE160.pdf".into(),
            sha256: "da0b0bb690bbfe15a9bc241c52d6baa878ac37f4797367f978d858df51105d53".into(),
            filing_office: "Superior Court Clerk's Office, probate division, in the decedent's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "de305".into(),
            title: "Affidavit re Real Property of Small Value".into(),
form_number: Some("DE-305".into()),
            source_url: "https://www.courts.ca.gov/documents/DE305.pdf".into(),
            sha256: "be6eec2553b3ba6e020ff0956c897217979770b486cce391c67a5902be5ca59e".into(),
            filing_office: "Superior Court Clerk's Office, probate division, in the decedent's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "mc030".into(),
            title: "Declaration".into(),
form_number: Some("MC-030".into()),
            source_url: "https://www.courts.ca.gov/documents/MC030.pdf".into(),
            sha256: "ed3f3ff115c7b6c5cfc6aa63939615f5dc1d7e44c31e4b7b5e7316a45ea2ff9a".into(),
            filing_office: "Superior Court Clerk's Office, probate division, in the decedent's county".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "fw001".into(),
            title: "Request to Waive Court Fees".into(),
form_number: Some("FW-001".into()),
            source_url: "https://www.courts.ca.gov/documents/FW001.pdf".into(),
            sha256: "8673409dc980bade5c3c534d0b6523e1abde4355f3fa72128dee85dc8ac3ce66".into(),
            filing_office: "Filed with the underlying case at the Superior Court Clerk's Office".into(),
            filing_fee: None,
            fee_note: Some("No filing fee; the request is confidential.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "fw001info".into(),
            title: "Information Sheet on Waiving Court Fees".into(),
form_number: Some("FW-001-INFO".into()),
            source_url: "https://www.courts.ca.gov/documents/FW001INFO.pdf".into(),
            sha256: "ee8d0eba1d96536d5d5c8d0f2eb048e9a1831a301b269720d7d452bafbec6f92".into(),
            filing_office: "Filed with the underlying case at the Superior Court Clerk's Office".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "fw002".into(),
            title: "Order on Court Fee Waiver (additional fees)".into(),
form_number: Some("FW-002".into()),
            source_url: "https://www.courts.ca.gov/documents/FW002.pdf".into(),
            sha256: "c448bcb1c544eaa917dee6434472a553b4cbce70774daa72e374c6f4bd83686d".into(),
            filing_office: "Filed with the underlying case at the Superior Court Clerk's Office".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "fw003".into(),
            title: "Order on Court Fee Waiver".into(),
form_number: Some("FW-003".into()),
            source_url: "https://www.courts.ca.gov/documents/FW003.pdf".into(),
            sha256: "1fead5e966ed70879852c081f57463a916bc2e2a76777c5959712d298e5a4928".into(),
            filing_office: "Filed with the underlying case at the Superior Court Clerk's Office".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
        ],
    },
    // No LA-specific fillable name-change blank exists; the statewide forms apply
    // and the LACIV 264 packet is an instructions sheet.
    Court {
        key: "los_angeles_superior_court".into(),
        name: "Los Angeles Superior Court".into(),
        jurisdiction: "CA".into(),
        county: Some("Los Angeles".into()),
        city: None,
        level: CourtLevel::Superior,
        forms: Vec::new(),
        surveyed_forms: vec![
            SurveyedForm {
            key: "laciv264".into(),
            title: "Name Change Packet (instructions and filing locations)".into(),
form_number: Some("LACIV 264".into()),
            source_url: "https://www.lacourt.org/division/civil/pdf/namechangelocationsandinstructions.pdf".into(),
            sha256: "40a9aacda2157a3b2d6d8431aed400b5b4a92c01201e8ac4b5a2d0bca3fb6886".into(),
            filing_office: "Los Angeles Superior Court district courthouse civil filing window (13+ locations listed in the packet)".into(),
            filing_fee: None,
            fee_note: Some("Not a fillable blank — a 2-page instructions/locations sheet covering adult name, adult name+gender, gender recognition, and minor's gender-recognition petitions. The operative blanks are the statewide Judicial Council forms. Statewide fees apply.".into()),
            fillability: Some(Fillability::UnfillableSource),

            },
        ],
    },
    // San Diego wraps the statewide forms in local packets; the statewide
    // Judicial Council blanks remain the operative filing targets.
    Court {
        key: "san_diego_superior_court".into(),
        name: "San Diego Superior Court".into(),
        jurisdiction: "CA".into(),
        county: Some("San Diego".into()),
        city: None,
        level: CourtLevel::Superior,
        forms: Vec::new(),
        surveyed_forms: vec![
            SurveyedForm {
            key: "sdpkt001".into(),
            title: "Change of Name Packet".into(),
form_number: Some("PKT-001".into()),
            source_url: "https://www.sdcourt.ca.gov/sites/default/files/sdcourt/generalinformation/forms/civilforms/pkt001.pdf".into(),
            sha256: "6314650140d92c1ae31dc406574aa0e3d2cff11414702b3bdf1e6ef90e1c159f".into(),
            filing_office: "North County Courthouse or Hall of Justice civil business office, depending on residence".into(),
            filing_fee: None,
            fee_note: Some("15-page fillable packet wrapping the statewide Judicial Council forms. Publication of the OSC runs once a week for four consecutive weeks.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "sdpkt033".into(),
            title: "Adult's Change of Gender and Sex Identifier, Name Change, and Issuance of New Certificates Packet".into(),
form_number: Some("PKT-033".into()),
            source_url: "https://www.sdcourt.ca.gov/sites/default/files/sdcourt/generalinformation/forms/civilforms/pkt033.pdf".into(),
            sha256: "a660610908922db73fc9c88e20af4db88d257587a0ec201305da39ef9fbd97e4".into(),
            filing_office: "North County Courthouse or Hall of Justice civil business office, depending on residence".into(),
            filing_fee: None,
            fee_note: Some("Packet cover is not fillable (0 AcroForm fields); its contents are the fillable JCC forms CM-010, NC-300-INFO, NC-300, NC-330. Statewide fees apply (ADM-001 fee schedule confirms $435 first paper).".into()),
            fillability: Some(Fillability::UnfillableSource),

            },
        ],
    },
    // The ACCESS Center (400 McAllister St, Room 509) helps fill RO forms (CH, DV,
    // EA, GV, WV). SF adopts the Statewide Civil Fee Schedule eff. 1/1/2026 as its
    // civil fee schedule, so the statewide $435/$450 figures and FW-001/FW-003
    // waiver pairing apply unchanged; the court publishes A-Z guide PDFs (name and
    // gender change, fee waiver) rather than local fillable blanks.
    Court {
        key: "san_francisco_superior_court".into(),
        name: "San Francisco Superior Court".into(),
        jurisdiction: "CA".into(),
        county: Some("San Francisco".into()),
        city: None,
        level: CourtLevel::Superior,
        forms: Vec::new(),
        surveyed_forms: Vec::new(),
    },
    // The Forms @ Home page routes self-represented litigants to Law Help
    // Interactive guided interviews (DVRO, CHRO, EARO, name change, parentage,
    // eviction, guardianship, guardianship fee waiver) — no local fillable
    // blanks of its own; the statewide forms apply.
    // Statewide form sets are registered under the court that approves them:
    // the Nevada Supreme Court's self-help forms are acceptable in any court
    // in Nevada, so the county-specific rows below carry only the forms and
    // fees unique to that county.
    Court {
        key: "alameda_superior_court".into(),
        name: "Alameda Superior Court".into(),
        jurisdiction: "CA".into(),
        county: Some("Alameda".into()),
        city: None,
        level: CourtLevel::Superior,
        forms: Vec::new(),
        surveyed_forms: Vec::new(),
    },
    Court {
        key: "nevada_supreme_court".into(),
        name: "Nevada Supreme Court".into(),
        jurisdiction: "NV".into(),
        county: None,
        city: None,
        level: CourtLevel::Supreme,
        forms: vec![
            CourtForm {
            code: "nv__adult_name_change_petition".into(),
            title: "Petition for Adult Name Change".into(),
form_number: None,
            source_url: "https://selfhelp.nvcourts.gov/images/name-change/name-adult-petition-pdf-fillable.pdf".into(),
            sha256: "1d949c984bc9a6779d8ea9b6a336da7ddfbf5051ee19d0ec90f7c202e467192f".into(),
            filing_office: "District Court Clerk's Office in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: Some("The filing fee varies by district court (for example, the Eighth Judicial District Court publishes $270.00); fee-waiver paperwork is out of scope for this wave.".into()),
            fee_waiver_code: None,

            },
            CourtForm {
            code: "nv__dv_protection_order_application".into(),
            title: "Application for Protection Order Against Domestic Violence".into(),
form_number: None,
            source_url: "https://selfhelp.nvcourts.gov/images/protection-orders/dv-app-pdf-fillable.pdf".into(),
            sha256: "0be2cdb927d377eda5e0f1e159239564973ef1b1a6a3f12b10e9dc4faa7cc2eb".into(),
            filing_office: "District Court Clerk's Office or the county protection-order office, in any Nevada county".into(),
            filing_fee: Some(FilingFee {
                amount: "0.00".into(),
                currency: "USD".into(),
            }),
            fee_note: None,
            fee_waiver_code: None,

            },
        ],
        surveyed_forms: vec![
            SurveyedForm {
            key: "FAMCOVER".into(),
            title: "Family Cover Sheet".into(),
form_number: None,
            source_url: "https://selfhelp.nvcourts.gov/images/misc/coversheet-family-pdf-fillable.pdf".into(),
            sha256: "96228b006e955c32212eb98a439d294a72a41a6505d32ac4c564b86eb4f5535d".into(),
            filing_office: "District Court Clerk's Office in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "ADULTNOTICE".into(),
            title: "Notice of Petition for Adult Name Change".into(),
form_number: None,
            source_url: "https://selfhelp.nvcourts.gov/images/name-change/name-adult-notice-pdf-fillable.pdf".into(),
            sha256: "a339ac110fefc41d5a8aa6b44718178a0b4c739372657af2690a163aea7c0ced".into(),
            filing_office: "District Court Clerk's Office in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: Some("Filing fee varies by district court; the statewide Application to Waive Filing Fee applies (registered below).".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "ADULTDISPO".into(),
            title: "Request for Summary Disposition & Declaration in Support (Adult Name Change)".into(),
form_number: None,
            source_url: "https://selfhelp.nvcourts.gov/images/name-change/name-adult-summary-dispo-pdf-fillable.pdf".into(),
            sha256: "324ed6314066676a827d0e20c8f1d56946a03de93bfbdb8965615f48e2938d2c".into(),
            filing_office: "District Court Clerk's Office in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "ADULTORDER".into(),
            title: "Order for Adult Name Change".into(),
form_number: None,
            source_url: "https://selfhelp.nvcourts.gov/images/name-change/name-adult-order-pdf-fillable.pdf".into(),
            sha256: "1c8c8e6080b42914f6636e5f026cd567a66013e36250e9f6d445ca61e9c431bc".into(),
            filing_office: "District Court Clerk's Office in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "CHILDPET".into(),
            title: "Petition for Child's Name Change".into(),
form_number: None,
            source_url: "https://selfhelp.nvcourts.gov/images/name-change/name-child-petition-pdf-fillable.pdf".into(),
            sha256: "6555a003d104f3efe5e4fd14ecb58020c913686cb556be1097df3a27c082b8c8".into(),
            filing_office: "District Court Clerk's Office in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: Some("Filing fee varies by district court; the statewide Application to Waive Filing Fee applies (registered below).".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "CHILDNOTICE".into(),
            title: "Notice of Petition for Child's Name Change".into(),
form_number: None,
            source_url: "https://selfhelp.nvcourts.gov/images/name-change/name-child-notice-pdf-fillable.pdf".into(),
            sha256: "2f47ecb63e95ae60a94358c6fb0558c17c2b748196ca1bb6b0796eedccb1573b".into(),
            filing_office: "District Court Clerk's Office in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "CHILDCONSENT".into(),
            title: "Child's Consent to Child's Name Change".into(),
form_number: None,
            source_url: "https://selfhelp.nvcourts.gov/images/name-change/name-child-consent-child-pdf-fillable.pdf".into(),
            sha256: "a0c0b934b5799af0c95bdd612137e353af0c60b91cb6f5ccdc9a2b37d4541798".into(),
            filing_office: "District Court Clerk's Office in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "CHILDSERVICE".into(),
            title: "Proof of Service on the Other Parent".into(),
form_number: None,
            source_url: "https://selfhelp.nvcourts.gov/images/name-change/name-child-service-pdf-fillable.pdf".into(),
            sha256: "f5e5c83b0dde76a8882f57d9ffdfc774f66c5ea3ec58491a28eecf8c9967d0e4".into(),
            filing_office: "District Court Clerk's Office in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "CHILDDISPO".into(),
            title: "Request for Summary Disposition & Declaration (Child)".into(),
form_number: None,
            source_url: "https://selfhelp.nvcourts.gov/images/name-change/name-child-summary-dispo-pdf-fillable.pdf".into(),
            sha256: "d3a7b4633610f6f36d0817c3b829be8d1b5e186c32e4717ce3e51f3ed173d01f".into(),
            filing_office: "District Court Clerk's Office in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "CHILDORDER".into(),
            title: "Order for Child's Name Change".into(),
form_number: None,
            source_url: "https://selfhelp.nvcourts.gov/images/name-change/name-child-order-pdf-fillable.pdf".into(),
            sha256: "41bb2256a47e751e7685255f5ce10a9649c74dd47361604894f55fa97d471807".into(),
            filing_office: "District Court Clerk's Office in the petitioner's county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "DVCONF".into(),
            title: "Confidential Information Sheet (Domestic Violence)".into(),
form_number: None,
            source_url: "https://selfhelp.nvcourts.gov/images/protection-orders/confidential-info-dv-pdf-fillable.pdf".into(),
            sha256: "ee5983550d91d1a7efc2358f5b158d8e1c7c76ad488b873dbbb87495ee42275c".into(),
            filing_office: "District Court Clerk's Office or the county protection-order office, in any Nevada county".into(),
            filing_fee: Some(FilingFee {
                amount: "0.00".into(),
                currency: "USD".into(),
            }),
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "DVUCCJEA".into(),
            title: "UCCJEA Declaration (Domestic Violence)".into(),
form_number: None,
            source_url: "https://selfhelp.nvcourts.gov/images/protection-orders/dv-uccjea-pdf-fillable.pdf".into(),
            sha256: "6558e218370c27a6bbfdbfe5d5bbbadbd67a5e36f91d335d4a49fff320f9632a".into(),
            filing_office: "District Court Clerk's Office or the county protection-order office, in any Nevada county".into(),
            filing_fee: Some(FilingFee {
                amount: "0.00".into(),
                currency: "USD".into(),
            }),
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "STALKAPP".into(),
            title: "Application for Protection (Stalking/Harassment/Sexual Assault/Harm to Minors)".into(),
form_number: None,
            source_url: "https://selfhelp.nvcourts.gov/images/protection-orders/stalking-harassment-sa-harm-app-pdf-fillable.pdf".into(),
            sha256: "8e501ff0d38de8865d307609c4f5a0195f353e66f40bf0a32e493ec942c6c1ea".into(),
            filing_office: "District Court Clerk's Office or the county protection-order office, in any Nevada county".into(),
            filing_fee: Some(FilingFee {
                amount: "0.00".into(),
                currency: "USD".into(),
            }),
            fee_note: Some("There is no fee to file protection-order forms; certified copies are also free.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "STALKCONF".into(),
            title: "Confidential Information Sheet (Stalking/Harassment/Sexual Assault/Harm to Minors)".into(),
form_number: None,
            source_url: "https://selfhelp.nvcourts.gov/images/protection-orders/confidential-info-other-pdf-fillable.pdf".into(),
            sha256: "76d8e990298807cc2614810d7ccef3ab677088e920132fe08e61f7f3c0512c07".into(),
            filing_office: "District Court Clerk's Office or the county protection-order office, in any Nevada county".into(),
            filing_fee: Some(FilingFee {
                amount: "0.00".into(),
                currency: "USD".into(),
            }),
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "WORKTPO".into(),
            title: "Application for Temporary Protection from Workplace Harassment".into(),
form_number: None,
            source_url: "https://selfhelp.nvcourts.gov/images/protection-orders/workplace-app-temp-pdf-fillable.pdf".into(),
            sha256: "5a6e6ee513751c2e6b012791a607b8b0252d1a5c79416ec7334ce2d278066d59".into(),
            filing_office: "District Court Clerk's Office or the county protection-order office, in any Nevada county".into(),
            filing_fee: Some(FilingFee {
                amount: "0.00".into(),
                currency: "USD".into(),
            }),
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "WORKEXT".into(),
            title: "Application for Extended Protection from Workplace Harassment".into(),
form_number: None,
            source_url: "https://selfhelp.nvcourts.gov/images/protection-orders/workplace-app-extended-pdf-fillable.pdf".into(),
            sha256: "88155bd9897b1efec9e4383b748cac118a5033a92d7a2b2ba5c742d7c9fbeafa".into(),
            filing_office: "District Court Clerk's Office or the county protection-order office, in any Nevada county".into(),
            filing_fee: Some(FilingFee {
                amount: "0.00".into(),
                currency: "USD".into(),
            }),
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "HRREG".into(),
            title: "Application for High-Risk Protection Order (family/household members)".into(),
form_number: None,
            source_url: "https://selfhelp.nvcourts.gov/images/protection-orders/high-risk-app-reg-pdf-fillable.pdf".into(),
            sha256: "b1418b110fc7f59947656c078948a459185d3003b669a1e176058a049b558c4f".into(),
            filing_office: "District Court Clerk's Office or the county protection-order office, in any Nevada county".into(),
            filing_fee: Some(FilingFee {
                amount: "0.00".into(),
                currency: "USD".into(),
            }),
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "HRLE".into(),
            title: "Application for High-Risk Protection Order (law enforcement)".into(),
form_number: None,
            source_url: "https://selfhelp.nvcourts.gov/images/protection-orders/high-risk-app-le-pdf-fillable.pdf".into(),
            sha256: "24847dc0b188fc5e0a3ca6adc72fd5a0afd72e810aa541a6b23817db82d7c4a2".into(),
            filing_office: "District Court Clerk's Office or the county protection-order office, in any Nevada county".into(),
            filing_fee: Some(FilingFee {
                amount: "0.00".into(),
                currency: "USD".into(),
            }),
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "FEEWAIVAPP".into(),
            title: "Application to Waive Filing Fee".into(),
form_number: None,
            source_url: "https://selfhelp.nvcourts.gov/images/misc/fee-waiver-application-pdf-fillable.pdf".into(),
            sha256: "36a9847de58c8903814add2c75080e3fe8dba359531a7d004f3f417a83d4fd8b".into(),
            filing_office: "Filed with the underlying case at the clerk's office".into(),
            filing_fee: None,
            fee_note: Some("No filing fee; the waiver lasts one year and covers case-startup and filing fees plus sheriff service.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "FEEWAIVORD".into(),
            title: "Order to Waive Filing Fee".into(),
form_number: None,
            source_url: "https://selfhelp.nvcourts.gov/images/misc/fee-waiver-order-pdf-fillable.pdf".into(),
            sha256: "0bed89fa93a8acb94ca12aef9202c8e2e7adda4891d11575af6f99375261fc9d".into(),
            filing_office: "Filed with the underlying case at the clerk's office".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
        ],
    },
    // Family Division (Family Law Self-Help Center) and Probate Division.
    // The statewide Nevada Supreme Court forms (registered above) are accepted
    // here; these rows carry only what is unique to Clark County. The adult
    // name-change fee is $270.00 (+$3.50 per eFileNV upload), per the Family
    // Law Self-Help Center. Clark's WordPress REST API is locked down (401),
    // so form indexes are HTML only.
    Court {
        key: "eighth_judicial_district_court".into(),
        name: "Eighth Judicial District Court".into(),
        jurisdiction: "NV".into(),
        county: Some("Clark".into()),
        city: None,
        level: CourtLevel::District,
        forms: Vec::new(),
        surveyed_forms: vec![
            SurveyedForm {
            key: "SETTINGSLIP".into(),
            title: "Setting Slip".into(),
form_number: None,
            source_url: "https://www.familylawselfhelpcenter.org/images/forms/misc/setting-slip-pdf-fillable.pdf".into(),
            sha256: "81a57f3513e0e70bcfdd3bc264a1c2f99fac87eed6d1367eedb11c3277e4f27a".into(),
            filing_office: "Family Division Clerk's Office, Eighth Judicial District Court".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "TPONONCONF".into(),
            title: "TPO Packet, Non-Confidential (Domestic Violence)".into(),
form_number: None,
            source_url: "https://www.familylawselfhelpcenter.org/images/forms/tpo/tpo-packet-nonconfidential-pdf-fillable.pdf".into(),
            sha256: "93e1c7f88afc00f27d999ed379775bcaeadab51f3186b85ed3aa9bea72c6b273".into(),
            filing_office: "Family Division Clerk's Office or the Protection Order Help Center, Family Division, 1 S. Sierra St equivalent — Las Vegas: 601 N. Pecos Rd".into(),
            filing_fee: None,
            fee_note: Some("Clark-wrapped copy of the statewide DV TPO packet; no filing fee.".into()),
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "TPOAPP".into(),
            title: "TPO Application (Domestic Violence)".into(),
form_number: None,
            source_url: "https://www.familylawselfhelpcenter.org/images/forms/tpo/tpo-app-pdf-fillable.pdf".into(),
            sha256: "c65fa55fd6d8f993a2e07cc73dd4375a23cc5c1dbb9737257726b57c363f62f4".into(),
            filing_office: "Family Division Clerk's Office or the Protection Order Help Center".into(),
            filing_fee: None,
            fee_note: Some("No filing fee.".into()),
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "TPOCONF".into(),
            title: "TPO Confidential Information (Domestic Violence)".into(),
form_number: None,
            source_url: "https://www.familylawselfhelpcenter.org/images/forms/tpo/tpo-conf-info-pdf-fillable.pdf".into(),
            sha256: "91d23516377bfd711a37c0b24b6c38273244ea961906d11852cc9bcc717e58d8".into(),
            filing_office: "Family Division Clerk's Office or the Protection Order Help Center".into(),
            filing_fee: None,
            fee_note: Some("No filing fee.".into()),
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "TPOCOVER".into(),
            title: "Family Court Cover Sheet (TPO)".into(),
form_number: None,
            source_url: "https://www.familylawselfhelpcenter.org/images/forms/tpo/family-court-cover-sheet-tpo-pdf-fillable.pdf".into(),
            sha256: "4a0fbbed4b6123bdddddc6037678649ab3ee732131419aa06dc7ed5c394e48ea".into(),
            filing_office: "Family Division Clerk's Office".into(),
            filing_fee: None,
            fee_note: Some("Required cover sheet for new Family Division cases (EDCR 7.20 requires a civil/family cover sheet at filing).".into()),
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "SETASIDEINT".into(),
            title: "Set-Aside Packet, Intestate (small-estate set-aside)".into(),
form_number: None,
            source_url: "https://www.civillawselfhelpcenter.org/images/Probate/2026-Probate-Forms/Set-Aside-Intestate/packet---set-aside-intestate.pdf".into(),
            sha256: "acedfd6f10178658bd447b0f44f58509b2ae16d0e5aa4fd362b83a8635526015".into(),
            filing_office: "Probate Division Clerk's Office, Eighth Judicial District Court".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "AFFENTITLE".into(),
            title: "Affidavit of Entitlement (small estate)".into(),
form_number: None,
            source_url: "https://www.civillawselfhelpcenter.org/images/Probate/2026-Probate-Forms/Affidavit-of-Entitlement/Fillable-Affidavit-of-Entitlement.pdf".into(),
            sha256: "c8a2c24907792360369e678759055a2b426f66b5ec6bbb01ce705302ea4e28f1".into(),
            filing_office: "Probate Division Clerk's Office, Eighth Judicial District Court".into(),
            filing_fee: None,
            fee_note: Some("The filename says 'Fillable' but the published PDF has zero AcroForm fields — registered as an unfillable source. A true small-estate self-help flow needs the field overlay or a re-authored blank.".into()),
            fillability: Some(Fillability::UnfillableSource),

            },
            SurveyedForm {
            key: "IFPPROBATE".into(),
            title: "Application to Proceed In Forma Pauperis and Order (Waiver of Costs and Fees under NRS 12.015)".into(),
form_number: None,
            source_url: "https://www.clarkcountycourts.us/res/clerk/civil-criminal-library//Probate/PDF/feewaiver_int.pdf".into(),
            sha256: "bed38ce7698461baf7bac31f524afb6c1a16cf4c8a10c5af6ddd89b58c58091c".into(),
            filing_office: "Clerk's Office, Eighth Judicial District Court".into(),
            filing_fee: None,
            fee_note: Some("No filing fee for the application itself; statutory fees are waived only by the IFP order, not at the counter.".into()),
            fillability: Some(Fillability::Fillable),

            },
        ],
    },
    // The Washoe NA adult name-change packet is Washoe-specific (not the
    // statewide form). The NA-2 publication-waiver/seal motion and NM-1..NM-4
    // minor packets live on the same forms tree with cache-busting URLs.
    // The statewide Nevada Supreme Court forms (registered above) are accepted
    // here; these rows carry only what is unique to Washoe County.
    Court {
        key: "second_judicial_district_court".into(),
        name: "Second Judicial District Court".into(),
        jurisdiction: "NV".into(),
        county: Some("Washoe".into()),
        city: None,
        level: CourtLevel::District,
        forms: vec![
            CourtForm {
            code: "nv__washoe_adult_name_change_petition".into(),
            title: "Petition for Adult Name Change (Washoe)".into(),
form_number: None,
            source_url: "https://www.washoecourts.com/Forms/Family/10.%20Name%20Changes/NA%20Adult%20Name%20Change/1.%20Petition%20for%20Adult%20Name%20Change.pdf".into(),
            sha256: "9a8ceaa3bd70b31300f259fb4fb4bf848adb5cbc75bc6e41f4b2a79d522af029".into(),
            filing_office: "Family Division Clerk's Office, Second Judicial District Court, 1 S. Sierra St, Reno".into(),
            filing_fee: None,
            fee_note: Some("The filing fee for a Washoe name change was NOT published on any page the survey fetched (2026-10-30) — treat the amount as unknown.".into()),
            fee_waiver_code: None,

            },
            CourtForm {
            code: "nv__washoe_fee_waiver_application".into(),
            title: "Application for Waiver of Fees and Costs (Washoe)".into(),
form_number: None,
            source_url: "https://www.washoecourts.com/Forms/Family/1.%20All%20Other%20Family%20Related%20Packets/F-6%20Application%20to%20Waive%20Fees%20and%20Costs/1.%20Application%20for%20Waiver%20of%20Fees%20and%20Costs.pdf".into(),
            sha256: "9b5223881ad34dc41dff0820b264010df986e911c3268118561078cbadf950fb".into(),
            filing_office: "Family Division Clerk's Office, Second Judicial District Court".into(),
            filing_fee: None,
            fee_note: Some("No filing fee for the application.".into()),
            fee_waiver_code: None,

            },
        ],
        surveyed_forms: vec![
            SurveyedForm {
            key: "WASHOENA".into(),
            title: "Petition for Adult Name Change (Washoe NA packet)".into(),
form_number: None,
            source_url: "https://www.washoecourts.com/Forms/Family/10.%20Name%20Changes/NA%20Adult%20Name%20Change/1.%20Petition%20for%20Adult%20Name%20Change.pdf".into(),
            sha256: "896d561aa43aed677344e6fbbf751b34902bc2988501e6630b0b5dc392ae9af8".into(),
            filing_office: "Family Division Clerk's Office, Second Judicial District Court, 1 S. Sierra St, Reno".into(),
            filing_fee: None,
            fee_note: Some("The filing fee for a Washoe name change was NOT published on any page the survey fetched (2026-10-30) — treat the amount as unknown. The NA-2 Ex Parte Motion to Waive Publication and Seal File (safety valve) and NM-1..NM-4 minor packets are on the same forms tree; their direct PDF URLs carry cache-busting query strings.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "F6FAM".into(),
            title: "Application for Waiver of Fees and Costs (F-6, Family)".into(),
form_number: Some("F-6".into()),
            source_url: "https://www.washoecourts.com/Forms/Family/1.%20All%20Other%20Family%20Related%20Packets/F-6%20Application%20to%20Waive%20Fees%20and%20Costs/1.%20Application%20for%20Waiver%20of%20Fees%20and%20Costs.pdf".into(),
            sha256: "e3e24ed4f4b7d6cb6e453e5dff8bc3340c2cc735ffab8f851dacbc3259e2c03e".into(),
            filing_office: "Family Division Clerk's Office, Second Judicial District Court".into(),
            filing_fee: None,
            fee_note: Some("No filing fee for the application; the F-6JP joint variant also exists.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "F6GJ".into(),
            title: "Application for Waiver of Fees and Costs (F-6, General Jurisdiction)".into(),
form_number: Some("F-6".into()),
            source_url: "https://www.washoecourts.com/Forms/GJ/16.%20F-6%20Application%20to%20Waive%20Fees%20and%20Costs/1.%20Application%20for%20Waiver%20of%20Fees%20and%20Costs.pdf".into(),
            sha256: "e3e24ed4f4b7d6cb6e453e5dff8bc3340c2cc735ffab8f851dacbc3259e2c03e".into(),
            filing_office: "Clerk's Office, Second Judicial District Court".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "WASHOEF14".into(),
            title: "Application for Protection Order Against Domestic Violence (F-14, no minor children)".into(),
form_number: Some("F-14".into()),
            source_url: "https://www.washoecourts.com/Forms/Family/17.%20Temporary%20Protection%20Orders/F-14%20Temporary%20Protection%20Order%20%28No%20Minor%20Children%29/2.%20Application%20for%20Protection%20Order%20Against%20Domestic%20Violence.pdf".into(),
            sha256: "73a099260a06197d32ffc07926beb5ac5bff1423c70f99052ac0d68000d7df86".into(),
            filing_office: "Protection Order Help Center, 3rd floor, Family Division, 1 S. Sierra St, Reno".into(),
            filing_fee: None,
            fee_note: Some("No filing fee.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "WASHOEAFFENT".into(),
            title: "Affidavit of Entitlement (Washoe)".into(),
form_number: None,
            source_url: "https://www.washoecourts.com/Forms/GJCommon/Affidavit%20of%20Entitlement.pdf".into(),
            sha256: "1cdd5ac6139b4427165ce077e41bbfafafcd50035ac4868950f2c0be438caa11".into(),
            filing_office: "Probate Division Clerk's Office, Second Judicial District Court".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
        ],
    },
    // Small claims forms are per-justice-court in Nevada; there is no
    // statewide small-claims set.
    Court {
        key: "las_vegas_justice_court".into(),
        name: "Las Vegas Justice Court".into(),
        jurisdiction: "NV".into(),
        county: Some("Clark".into()),
        city: Some("Las Vegas".into()),
        level: CourtLevel::Justice,
        forms: vec![
            CourtForm {
            code: "nv__las_vegas_small_claims_complaint".into(),
            title: "Complaint, Small Claims (Las Vegas)".into(),
form_number: None,
            source_url: "https://www.civillawselfhelpcenter.org/images/small-claims/complaints/complaint---small-claims.pdf".into(),
            sha256: "b9cecb40625f1156f0d540ebe618cc89587e42891f261d37cc0f3399c189fb09".into(),
            filing_office: "Las Vegas Justice Court clerk, small claims division".into(),
            filing_fee: None,
            fee_note: Some("Filing fee tiered by claim (NRS 4.060 schedule, eff. 2/8/2024): $66 up to $1,000; $86 for $1,000.01-$2,500; $106 for $2,500.01-$5,000; $146 for $5,000.01-$7,500; $196 for $7,500.01-$10,000. No fee may be charged to a defendant appearing. Waiver: the justice-court fee-waiver form or the statewide application.".into()),
            fee_waiver_code: None,

            },
            CourtForm {
            code: "nv__las_vegas_small_claims_counterclaim".into(),
            title: "Counterclaim, Small Claims (Las Vegas)".into(),
form_number: None,
            source_url: "https://www.civillawselfhelpcenter.org/images/small-claims/counterclaims/counterclaim---small-claims---las-vegas.pdf".into(),
            sha256: "1e52f29cdcc642d58c2d61fa47f16d1339dad88488c68b61732e565faba1d053".into(),
            filing_office: "Las Vegas Justice Court clerk, small claims division".into(),
            filing_fee: None,
            fee_note: Some("No separate filing fee for the counterclaim; it is filed in the same case.".into()),
            fee_waiver_code: None,

            },
        ],
        surveyed_forms: vec![
            SurveyedForm {
            key: "LVCSCOMPLAINT".into(),
            title: "Complaint — Small Claims (Las Vegas Township)".into(),
form_number: None,
            source_url: "https://www.civillawselfhelpcenter.org/images/small-claims/complaints/complaint---small-claims.pdf".into(),
            sha256: "44f5e7196a6e6f2d1e227ec3d43162345d2d343d77b5a9abc1dad79cf65df730".into(),
            filing_office: "Las Vegas Justice Court clerk, small claims division".into(),
            filing_fee: None,
            fee_note: Some("Filing fee tiered by claim (NRS 4.060 schedule, eff. 2/8/2024): $66 up to $1,000; $86 for $1,000.01-$2,500; $106 for $2,500.01-$5,000; $146 for $5,000.01-$7,500; $196 for $7,500.01-$10,000. No fee may be charged to a defendant appearing. Waiver: the justice-court fee-waiver form (below) or the statewide application.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "LVCSCOUNTER".into(),
            title: "Counterclaim — Small Claims (Las Vegas Township)".into(),
form_number: None,
            source_url: "https://www.civillawselfhelpcenter.org/images/small-claims/counterclaims/counterclaim---small-claims---las-vegas.pdf".into(),
            sha256: "53929d498f7bb2947fa5ab78fa28f629e5fb4d189bb42cad9d1b1841fbdd5ac4".into(),
            filing_office: "Las Vegas Justice Court clerk, small claims division".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "LVCSAPPEAL".into(),
            title: "Appeal of Small Claims Judgment".into(),
form_number: None,
            source_url: "https://www.civillawselfhelpcenter.org/images/small-claims/lasvegas/small-claims-appeal-fillable.pdf".into(),
            sha256: "39a818322e8a7f30e72efff19d7a19792535a720eabeb8fe5c71ed8f47b0eaf4".into(),
            filing_office: "Las Vegas Justice Court clerk, small claims division".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "JCFFEEWAIV".into(),
            title: "Fee Waiver — Justice Court".into(),
form_number: None,
            source_url: "https://www.civillawselfhelpcenter.org/images/fee-waivers/fee-waiver---justice-court.pdf".into(),
            sha256: "af39d7cb3087b38845344f1ab21e4a5707174b2bc35b55de3bf35e189bc9f27b".into(),
            filing_office: "Las Vegas Justice Court clerk".into(),
            filing_fee: None,
            fee_note: Some("No filing fee for the application. Henderson and North Las Vegas variants exist on the same self-help page.".into()),
            fillability: Some(Fillability::Fillable),

            },
        ],
    },
    Court {
        key: "reno_justice_court".into(),
        name: "Reno Justice Court".into(),
        jurisdiction: "NV".into(),
        county: Some("Washoe".into()),
        city: Some("Reno".into()),
        level: CourtLevel::Justice,
        forms: Vec::new(),
        surveyed_forms: vec![
            SurveyedForm {
            key: "RJCSCDECL".into(),
            title: "Declaration of Small Claims Complaint".into(),
form_number: None,
            source_url: "https://washoecounty.gov/rjc/files/forms/form_civil_sc-declaration_of_small_claims_complaint.pdf".into(),
            sha256: "347301146ed85d8992db7b431df347d4ad5710821c6e69799e406ed65a631084".into(),
            filing_office: "Reno Justice Court clerk, civil division".into(),
            filing_fee: None,
            fee_note: Some("Filing fee tiered by claim (NRS 4.060/73; 'Civil Fees as of 04-2026'): $66 up to $1,000; $86 to $2,500; $106 to $5,000; $146 to $7,500; $196 to $10,000; each includes the $21 NRS ch. 4 fee. Transfer to justice court $125; abstract of judgment $25.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "RJCSCANSWER".into(),
            title: "Small Claims Answer".into(),
form_number: None,
            source_url: "https://washoecounty.gov/rjc/files/forms/form_civil_sc-small_claims_answer.pdf".into(),
            sha256: "4086e444f319572120ac52bb3ac86b33583a2dd58f991ce2287f979eb381d51f".into(),
            filing_office: "Reno Justice Court clerk, civil division".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "RJCSCCOUNTER".into(),
            title: "Small Claims Counterclaim".into(),
form_number: None,
            source_url: "https://washoecounty.gov/rjc/files/forms/form_civil_sc-small_claims_counterclaim.pdf".into(),
            sha256: "8f197f77ccc6d2bee4f95d6e2e1677dae61d0f2baba6830e672f20016b856f0c".into(),
            filing_office: "Reno Justice Court clerk, civil division".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "RJCSCAPPEAL".into(),
            title: "Notice of Appeal (small claims)".into(),
form_number: None,
            source_url: "https://washoecounty.gov/rjc/files/forms/form_civil_sc_notice-appeal.pdf".into(),
            sha256: "319ec886af313f503a48f8aeb1c77380f92a6d97b9ea3f37efc0c8b6174e24a9".into(),
            filing_office: "Reno Justice Court clerk, civil division".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "RJCIFP".into(),
            title: "Motion to Proceed In Forma Pauperis".into(),
form_number: None,
            source_url: "https://washoecounty.gov/rjc/files/forms/form_civil_general_motion-proceed-in-forma-pauperis.pdf".into(),
            sha256: "3f3328cd1343f6aac1f09ba89be7b89ce07fefc361c5c5e4a68af92a02b8dd76".into(),
            filing_office: "Reno Justice Court clerk, civil division".into(),
            filing_fee: None,
            fee_note: Some("No filing fee for the motion.".into()),
            fillability: Some(Fillability::Fillable),

            },
        ],
    },
    // Statewide pattern-form library (courts.wa.gov/forms). The sampled pattern
    // PDFs are NOT AcroForm-fillable — a fill tool must overlay fields on the
    // flat PDFs or route through Guide & File; every form page also links an
    // official Word version. Name changes and wills/probate have NO statewide
    // pattern forms (county/local forms only; name changes may be filed in any
    // district court per RCW 4.24.130). Sex-designation changes run through
    // agency forms (DOH/DOL), not the courts.
    Court {
        key: "washington_supreme_court".into(),
        name: "Washington Supreme Court (statewide pattern forms)".into(),
        jurisdiction: "WA".into(),
        county: None,
        city: None,
        level: CourtLevel::Supreme,
        forms: Vec::new(),
        surveyed_forms: vec![
            SurveyedForm {
            key: "PO001".into(),
            title: "Petition for Protection Order".into(),
form_number: Some("PO 001".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/PO%20001%20Petition%20for%20Protection%20Order_06_2026.pdf".into(),
            sha256: "ffcf053c303e3019cf76236929f256a14b32398dbb88bdc3bd3a747484026d12".into(),
            filing_office: "Superior Court Clerk's Office or district court clerk (either accepts protection-order petitions); many counties also operate a dedicated protection-order office".into(),
            filing_fee: None,
            fee_note: Some("One unified petition covers all five RCW ch. 7.105 order types (DV, sexual assault, stalking, vulnerable adult, anti-harassment). The published PDF has zero AcroForm fields — overlay fields or route through the official Guide & File. Anti-harassment is the only type the petition flags '(fee may be required)' (Pierce: $53 Superior / $83 District); DV/SA/stalking petitions have no fee line in county schedules; vulnerable-adult filing is 'None (7.105)'. Fee waiver: PO 012/PO 014 (harassment) or GR 34.".into()),
            fillability: Some(Fillability::UnfillableSource),

            },
            SurveyedForm {
            key: "POI001".into(),
            title: "Instructions for Petition for Protection Order".into(),
form_number: Some("POi 001".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/POi%20001%20Instructions%20Petition%20for%20Protection%20Order_2026_01.pdf".into(),
            sha256: "4da46bc5c6e3489ff80bd7d77aa0769199babb05eb9db4f11d0d4d5cfa2d6d28".into(),
            filing_office: "Superior Court Clerk's Office or district court clerk (either accepts protection-order petitions); many counties also operate a dedicated protection-order office".into(),
            filing_fee: None,
            fee_note: Some("n/a (instructions).".into()),
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "PO003".into(),
            title: "Law Enforcement and Confidential Information (LECIF)".into(),
form_number: Some("PO 003".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/PO%20003%20Law%20Enforcement%20and%20Confidential%20Info_2026_03.pdf".into(),
            sha256: "0e97647445df9cff3c82ed79e4fad21f8faf3b44cab61c4918e36cc5bd97994f".into(),
            filing_office: "Superior Court Clerk's Office or district court clerk (either accepts protection-order petitions); many counties also operate a dedicated protection-order office".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "PO003R".into(),
            title: "LECIF — Restrained Person".into(),
form_number: Some("PO 003R".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/PO%20003R%20Law%20Enforcement%20and%20Confidential%20Info%20Restrained%20Person_2026_01.pdf".into(),
            sha256: "b6d5d4e39833347d28146b21e0557d22821024d2405c69abf5b31a3cb83689fa".into(),
            filing_office: "Superior Court Clerk's Office or district court clerk (either accepts protection-order petitions); many counties also operate a dedicated protection-order office".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "PO030".into(),
            title: "Temporary Protection Order and Hearing Notice".into(),
form_number: Some("PO 030".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/PO%20030%20Temporary%20Protection%20Order%20and%20Hearing%20Notice_07_2025.pdf".into(),
            sha256: "504584d1ad3f108d2d15f35861b9c8a8e22f75a9a563096aed312f3056a12b50".into(),
            filing_office: "Superior Court Clerk's Office or district court clerk (either accepts protection-order petitions); many counties also operate a dedicated protection-order office".into(),
            filing_fee: None,
            fee_note: Some("No filing fee for the petition types other than anti-harassment.".into()),
            fillability: Some(Fillability::UnfillableSource),

            },
            SurveyedForm {
            key: "PO040".into(),
            title: "Protection Order (final order)".into(),
form_number: Some("PO 040".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/PO%20040%20Protection%20Order_06_2026.pdf".into(),
            sha256: "925fc132d9f90d3b91a420b89051a8ea67c9b3e50620e78b672288782bd07b50".into(),
            filing_office: "Superior Court Clerk's Office or district court clerk (either accepts protection-order petitions); many counties also operate a dedicated protection-order office".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "PO018".into(),
            title: "Declaration".into(),
form_number: Some("PO 018".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/PO%20018%20Declaration_2022_07.pdf".into(),
            sha256: "27a93dd4552aa4ac429fcb16f1660ce66f509102deefa92f2ecd23c307c37d49".into(),
            filing_office: "Superior Court Clerk's Office or district court clerk (either accepts protection-order petitions); many counties also operate a dedicated protection-order office".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "PO010".into(),
            title: "Statement".into(),
form_number: Some("PO 010".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/PO%20010%20Statement_2022_07.pdf".into(),
            sha256: "b0fbcfccbb0eb7a93536f6ea7bc13fa22ab40f8cc901d2a6aed282d471ae76a8".into(),
            filing_office: "Superior Court Clerk's Office or district court clerk (either accepts protection-order petitions); many counties also operate a dedicated protection-order office".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "PO012".into(),
            title: "Motion and Declaration for Waiver of Filing Fees and Surcharges — Harassment".into(),
form_number: Some("PO 012".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/PO%20012%20Motion%20Filing%20Fee%20Waiver%20-%20Harassment_%202023_01.pdf".into(),
            sha256: "167a887eb729ff6817b9abb6a8299643d56fb8ab593ed2a765d7d400c38b06f0".into(),
            filing_office: "Superior Court Clerk's Office or district court clerk (either accepts protection-order petitions); many counties also operate a dedicated protection-order office".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::UnfillableSource),

            },
            SurveyedForm {
            key: "PO014".into(),
            title: "Order re Waiver of Filing Fees and Surcharges — Harassment".into(),
form_number: Some("PO 014".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/PO%20014%20Order%20re%20Filing%20Fee%20Waiver-Harassment_07_2025.pdf".into(),
            sha256: "b62058a005d98dee5321081dd3f9dff09e689b2107f448194464302c0c0dabed".into(),
            filing_office: "Superior Court Clerk's Office or district court clerk (either accepts protection-order petitions); many counties also operate a dedicated protection-order office".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "PO004".into(),
            title: "Proof of Service (RTS)".into(),
form_number: Some("PO 004".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/PO%20004%20Proof%20of%20Service_2026_01.pdf".into(),
            sha256: "e33553f8997b4f4d7294a6120e4ac97b1a25c7c6a2a9fd722ef15c97cc232b6d".into(),
            filing_office: "Superior Court Clerk's Office or district court clerk (either accepts protection-order petitions); many counties also operate a dedicated protection-order office".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "PO050".into(),
            title: "Motion to Renew Protection Order".into(),
form_number: Some("PO 050".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/PO%20050%20Motion%20for%20Renewal%20of%20Protection%20Order_07_2025.pdf".into(),
            sha256: "67168e5fb726d788709c8300ea1686325916e63d000ea200dfd26bca6c5045f8".into(),
            filing_office: "Superior Court Clerk's Office or district court clerk (either accepts protection-order petitions); many counties also operate a dedicated protection-order office".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "PO063".into(),
            title: "Motion to Modify or Terminate Protection Order".into(),
form_number: Some("PO 063".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/PO%20063%20Motion%20to%20Modify%20or%20Terminate_07_2025.pdf".into(),
            sha256: "76590871290b9255284a0e6d391838aae712a8c96894b1385fe34ef647b3501e".into(),
            filing_office: "Superior Court Clerk's Office or district court clerk (either accepts protection-order petitions); many counties also operate a dedicated protection-order office".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "WS001".into(),
            title: "Order to Surrender and Prohibit Weapons".into(),
form_number: Some("WS 001".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/WS%20001%20Order%20to%20Surrender%20and%20Prohibit%20Weapons_06_2026.pdf".into(),
            sha256: "3c0dc72dc887689b7b441f54b71ccba9e37f6bf30c2e1716bf4117ab446c91cc".into(),
            filing_office: "Superior Court Clerk's Office or district court clerk (either accepts protection-order petitions); many counties also operate a dedicated protection-order office".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "MISC050100".into(),
            title: "Notice of Small Claim".into(),
form_number: Some("MISC 05.0100".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/MISC%2005%200100%20Notice%20of%20Small%20Claim%202025%2001.pdf".into(),
            sha256: "ed0d0baf7eb073c2f8aa342852611dfee05dced4aacf82fd372f2199d9bc6418".into(),
            filing_office: "District Court clerk, small claims division".into(),
            filing_fee: None,
            fee_note: Some("Filed in district court (limits: $10,000 natural person / $5,000 entity). The statewide pattern PDF is not AcroForm-fillable; counties host local fillable copies (e.g. King County). Filing fee is set per county ($50 in King, Pierce, and Clark districts).".into()),
            fillability: Some(Fillability::UnfillableSource),

            },
            SurveyedForm {
            key: "MISC050200".into(),
            title: "Certificate of Service (small claims)".into(),
form_number: Some("MISC 05.0200".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/MISC%2005%200200%20SC%20-%20Proof%20of%20Service%202025%2001.pdf".into(),
            sha256: "1f4d38eff43a6cf7aa4408213aececcf59afd844a943591bba15a47f6f995f71".into(),
            filing_office: "District Court clerk, small claims division".into(),
            filing_fee: None,
            fee_note: Some("District Court clerk, small claims division".into()),
            fillability: Some(Fillability::UnfillableSource),

            },
            SurveyedForm {
            key: "MISC050300".into(),
            title: "Small Claims/Order of Dismissal/Continuance".into(),
form_number: Some("MISC 05.0300".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/MISC%2005%20300SC%20Order%20Dismiss%20Continue%202025%2001.pdf".into(),
            sha256: "2093636e319be9cae96261113f0dbd792afb4ca489c373a7ff4df787dcdcff86".into(),
            filing_office: "District Court clerk, small claims division".into(),
            filing_fee: None,
            fee_note: Some("District Court clerk, small claims division".into()),
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "MISC050500".into(),
            title: "Small Claims Judgment".into(),
form_number: Some("MISC 05.0500".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/MISC05_0500_SmallClaims%20Judgment%202025_01%2001.pdf".into(),
            sha256: "64b3c1bfa93338f1f37ced3385634f65ece84b452a750b8ff65706625679e692".into(),
            filing_office: "District Court clerk, small claims division".into(),
            filing_fee: None,
            fee_note: Some("District Court clerk, small claims division".into()),
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "GR340100".into(),
            title: "Motion and Declaration for Waiver of Civil Fees and Surcharges (MTMVF)".into(),
form_number: Some("WPF GR 34.0100".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/GR%2034_0100%20Party%20MTWVF%20Filing%20Fee%20Waiver_2019%2007(02).pdf".into(),
            sha256: "afc2bbc1c2751327b2d570e3d89f5acaa67298fa414ccfec8aea8477864bde03".into(),
            filing_office: "Filed with the underlying case at the clerk's office".into(),
            filing_fee: None,
            fee_note: Some("No filing fee for the waiver motion itself.".into()),
            fillability: Some(Fillability::UnfillableSource),

            },
            SurveyedForm {
            key: "GR340200".into(),
            title: "Motion and Declaration for Waiver of Civil Fees and Surcharges (QLSP Filing)".into(),
form_number: Some("WPF GR 34.0200".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/GR%2034_0200%20QLSP%20MTWVF%20Filing%20Fee%20Waiver_2021%2004.pdf".into(),
            sha256: "267fdcb9d92154e7988dffa504f068fd7fc3a96f7a26c4fc7f7988520ea6351e".into(),
            filing_office: "Filed with the underlying case at the clerk's office".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "GR340300".into(),
            title: "Financial Statement (attachment)".into(),
form_number: Some("WPF GR 34.0300".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/GR%2034_0300%20Financial%20Statement(02).pdf".into(),
            sha256: "712c44c1d8d92cc04ce5961b43904ed13f3ef155793f4837d430eb024fc93c49".into(),
            filing_office: "Filed with the underlying case at the clerk's office".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::UnfillableSource),

            },
            SurveyedForm {
            key: "GR340500".into(),
            title: "Order Re Waiver of Civil Fees and Surcharges".into(),
form_number: Some("WPF GR 34.0500".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/GR%2034_0500%20Order%20re%20Fee%20Waiver(02).pdf".into(),
            sha256: "4cb4325b314b6c2fd3552940571d1125bb13b5b431860f8e5001781657b6c647".into(),
            filing_office: "Filed with the underlying case at the clerk's office".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::UnfillableSource),

            },
        ],
    },
    // King County's own local forms ARE fillable AcroForms, unlike the
    // statewide pattern PDFs. eFiling portal: dcfiling.kingcounty.gov.
    Court {
        key: "king_county_district_court".into(),
        name: "King County District Court".into(),
        jurisdiction: "WA".into(),
        county: Some("King".into()),
        city: Some("Seattle".into()),
        level: CourtLevel::District,
        forms: vec![
            CourtForm {
            code: "wa__adult_name_change_petition".into(),
            title: "Petition for Name Change (Individual)".into(),
form_number: None,
            source_url: "https://cdn.kingcounty.gov/-/media/king-county/courts/district-court/forms_resources_library_docs/name-change-forms/petition-for-name-change-individual-july_2026.pdf".into(),
            sha256: "a4f86f65b2b30f5fa06284591617f27cee0a43ce334e188b7afb0f55ff72f04a".into(),
            filing_office: "King County District Court clerk".into(),
            filing_fee: Some(FilingFee {
                amount: "98.00".into(),
                currency: "USD".into(),
            }),
            fee_note: Some("Filing fee $98 per application (includes 1 certified copy; additional copies $5) plus a separate King County Recorder fee: $203.50 individual; $204.50 first family member + $203.50 each additional. Waiver: the statewide GR 34 set (pattern PDFs, unfillable) or the local motion filed in person. Sealed name changes (gender expression/identity, refugee/asylum, DV victim, emancipated minor) go to Superior Court instead.".into()),
            fee_waiver_code: None,

            },
            CourtForm {
            code: "wa__minor_name_change_petition".into(),
            title: "Petition for Minor Child Name Change".into(),
form_number: None,
            source_url: "https://cdn.kingcounty.gov/-/media/king-county/courts/district-court/forms_resources_library_docs/name-change-forms/petition-for-name-change-minor-october-2024.pdf".into(),
            sha256: "7d228ed4b18d377ee87a0cd07d81caaac1ec9338deddb51ef98318f325bab5ab".into(),
            filing_office: "King County District Court clerk".into(),
            filing_fee: Some(FilingFee {
                amount: "98.00".into(),
                currency: "USD".into(),
            }),
            fee_note: Some("Both parents' consent or service required; children 14-17 must attend the hearing. Fee as for the adult petition.".into()),
            fee_waiver_code: None,

            },
            CourtForm {
            code: "wa__small_claims_notice".into(),
            title: "Notice of Small Claim".into(),
form_number: None,
            source_url: "https://cdn.kingcounty.gov/-/media/king-county/courts/district-court/forms_resources_library_docs/small_claims_forms/forms_sc/notice-of-small-claim-september-2025.pdf".into(),
            sha256: "761f9e321b19237c890d31652f32beeed42a5f5323cb03d6b6246558912d7b96".into(),
            filing_office: "King County District Court clerk, small claims division".into(),
            filing_fee: Some(FilingFee {
                amount: "50.00".into(),
                currency: "USD".into(),
            }),
            fee_note: Some("Filing fee $50; limits $10,000 natural person / $5,000 other. Appeal to Superior: $280 cashier's check to the Superior Court Clerk + $40 district appeal-processing fee. A guided e-file interview generates this notice.".into()),
            fee_waiver_code: None,

            },
            CourtForm {
            code: "wa__small_claims_plaintiff_packet".into(),
            title: "Small Claims Packet — Plaintiff".into(),
form_number: None,
            source_url: "https://cdn.kingcounty.gov/-/media/king-county/courts/district-court/forms_resources_library_docs/small_claims_forms/info_instruct_small_claims/small_claims_plaintiff_packet-july-2026.pdf".into(),
            sha256: "74a11bf8a81c30a97f43c3abc7c71208fdb8812a0e4ab3e945e98bc479ba41c1".into(),
            filing_office: "King County District Court clerk, small claims division".into(),
            filing_fee: None,
            fee_note: Some("Instructions packet wrapping the claim process; the notice's filing fee applies.".into()),
            fee_waiver_code: None,

            },
        ],
        surveyed_forms: vec![
            SurveyedForm {
            key: "KCPETADULT".into(),
            title: "Petition for Name Change (Individual)".into(),
form_number: None,
            source_url: "https://cdn.kingcounty.gov/-/media/king-county/courts/district-court/forms_resources_library_docs/name-change-forms/petition-for-name-change-individual-july_2026.pdf".into(),
            sha256: "362c90497ebf8f978ea747a5ddb252b66e20211b74e109c3d0786943882680f4".into(),
            filing_office: "King County District Court clerk".into(),
            filing_fee: None,
            fee_note: Some("Filing fee $98 per application (includes 1 certified copy; additional copies $5) plus a separate King County Recorder fee: $203.50 individual; $204.50 first family member + $203.50 each additional. Waiver: the local GR 34 motion (below) or the statewide GR 34 set. Sealed name changes (gender expression/identity, refugee/asylum, DV victim, emancipated minor) go to Superior Court instead.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "KCPETMINOR".into(),
            title: "Petition for Minor Child Name Change".into(),
form_number: None,
            source_url: "https://cdn.kingcounty.gov/-/media/king-county/courts/district-court/forms_resources_library_docs/name-change-forms/petition-for-name-change-minor-october-2024.pdf".into(),
            sha256: "bab3b88de0aa8822f74584291d4f82bd6869c8b9da3c43c4cceb5ea760c18549".into(),
            filing_office: "King County District Court clerk".into(),
            filing_fee: None,
            fee_note: Some("Both parents' consent or service required; children 14-17 must attend the hearing. Fee as for the adult petition.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "KCGR34".into(),
            title: "Motion and Declaration for Waiver of Fees and Surcharges (King County local GR 34 version)".into(),
form_number: None,
            source_url: "https://cdn.kingcounty.gov/-/media/king-county/courts/district-court/forms_resources_library_docs/name-change-forms/motion_and_declaration_for_waiver_of_fees_and_surcharges_october-2025.pdf".into(),
            sha256: "688e31b40aa167fd2bca33c7f90d0e496654b1169b2a5ec1705edb42defff1c1".into(),
            filing_office: "King County District Court clerk".into(),
            filing_fee: None,
            fee_note: Some("No filing fee for the motion.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "KCSCNOTICE".into(),
            title: "Notice of Small Claim (King County local form)".into(),
form_number: None,
            source_url: "https://cdn.kingcounty.gov/-/media/king-county/courts/district-court/forms_resources_library_docs/small_claims_forms/forms_sc/notice-of-small-claim-september-2025.pdf".into(),
            sha256: "5bc191bc445d6da232815580bff270a7e344b523c6bf51c5b3346425f73c80f9".into(),
            filing_office: "King County District Court clerk, small claims division".into(),
            filing_fee: None,
            fee_note: Some("Filing fee $50; limits $10,000 natural person / $5,000 other. Appeal to Superior: $280 cashier's check to the Superior Court Clerk + $40 district appeal-processing fee. A guided e-file interview generates this notice.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "KCSCPACKET".into(),
            title: "Small Claims Packet — Plaintiff".into(),
form_number: None,
            source_url: "https://cdn.kingcounty.gov/-/media/king-county/courts/district-court/forms_resources_library_docs/small_claims_forms/info_instruct_small_claims/small_claims_plaintiff_packet-july-2026.pdf".into(),
            sha256: "041cda0ef94435f0e7a5a70cb69677bf68eeee51c2dc142541b422c29141f3a7".into(),
            filing_office: "King County District Court clerk, small claims division".into(),
            filing_fee: None,
            fee_note: Some("Instructions packet wrapping the claim process.".into()),
            fillability: Some(Fillability::Fillable),

            },
        ],
    },
    // Name changes auto-seal here under RCW 4.24.130(5) for gender identity,
    // DV, refugee/asylum, and emancipated-minor filings. Superior filing fee
    // $290.00 (civil action; fee schedule eff. 10/1/2026); statewide GR 34
    // applies for waivers. Protection orders: Superior or District Court.
    Court {
        key: "king_county_superior_court".into(),
        name: "King County Superior Court".into(),
        jurisdiction: "WA".into(),
        county: Some("King".into()),
        city: Some("Seattle".into()),
        level: CourtLevel::Superior,
        forms: Vec::new(),
        surveyed_forms: vec![
            SurveyedForm {
            key: "KCWILLCOVER".into(),
            title: "Will Repository Cover Sheet".into(),
form_number: None,
            source_url: "https://cdn.kingcounty.gov/-/media/king-county/depts/dja/forms/will-repository-coversheet-pdf.pdf".into(),
            sha256: "0f1106e8d783508d0fd91ed11cb801e02e320779e63268eff7937a7ef9b72b9e".into(),
            filing_office: "King County Superior Court Clerk's Office".into(),
            filing_fee: None,
            fee_note: Some("Will Repository $20.00; Will Filing only (no probate) $20.00; Probate Filing $290.00; Civil Action $290.00 (Superior Court and Clerk's fee schedule, eff. 10/1/2026). Fee waiver: statewide GR 34.".into()),
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "KCWILLWITHDRAW".into(),
            title: "Motion and Declaration to Withdraw Will from Repository".into(),
form_number: None,
            source_url: "https://cdn.kingcounty.gov/-/media/king-county/depts/dja/forms/motion-withdraw-will-pdf.pdf".into(),
            sha256: "38b428ecc6379c8b0460edc93699aa2020fbde93002aa4f3db1662b4077eb5e7".into(),
            filing_office: "King County Superior Court Clerk's Office".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Unverified),

            },
        ],
    },
    // Small claims: $50, initiated online or at kiosks; mandatory mediation
    // through the Dispute Resolution Center. District fee schedule eff.
    // 12/08/2025. Waiver request fillable online; statewide GR 34 elsewhere.
    Court {
        key: "pierce_county_district_court".into(),
        name: "Pierce County District Court".into(),
        jurisdiction: "WA".into(),
        county: Some("Pierce".into()),
        city: Some("Tacoma".into()),
        level: CourtLevel::District,
        forms: Vec::new(),
        surveyed_forms: vec![
            SurveyedForm {
            key: "PIERCENCINFO".into(),
            title: "Adult Name Change Information packet".into(),
form_number: None,
            source_url: "https://www.piercecountywa.gov/DocumentCenter/View/134269/NC_Name-Change-Information-Adult".into(),
            sha256: "8febe8c4d12333c83a9a28af450f03b9060e03614fc0aabe9cec6c3643815f20".into(),
            filing_office: "Pierce County District Court clerk, Room 239, County-City Building, 930 Tacoma Ave S".into(),
            filing_fee: None,
            fee_note: Some("Name-change fee $308.50 (each additional family member $218.50) per the District Court fee schedule eff. 12/08/2025. Petition completed online (district court portal) or via the county Find-a-Form page — no stable static petition PDF was captured by the survey (the site sits behind a bot challenge). Sealed name changes must be heard in Superior Court. Waiver: Financial Declaration/Motion and Order to Waive Fees fillable online; statewide GR 34 for Superior filings.".into()),
            fillability: Some(Fillability::Unverified),

            },
        ],
    },
    // Probate filing $290.00; small estate $20.00; will only $20.00; sealed
    // will repository $20.00 (Clerk fee schedule eff. 6/11/2026). Sealed name
    // changes must be heard here. Statewide GR 34 applies for waivers.
    Court {
        key: "pierce_county_superior_court".into(),
        name: "Pierce County Superior Court".into(),
        jurisdiction: "WA".into(),
        county: Some("Pierce".into()),
        city: Some("Tacoma".into()),
        level: CourtLevel::Superior,
        forms: Vec::new(),
        surveyed_forms: vec![
            SurveyedForm {
            key: "PIERCEPOPACKET".into(),
            title: "Petition and LECIF packet (protection order, county republished)".into(),
form_number: None,
            source_url: "https://www.piercecountywa.gov/DocumentCenter/View/115550/Petition-and-LECIF?bidId=".into(),
            sha256: "3c92ff61aea03a79065e4420d079f1c819974e7e05334a4fb53be65fd29b0218".into(),
            filing_office: "Civil Orders for Protection desk, County-City Building Room 110, 930 Tacoma Ave S".into(),
            filing_fee: None,
            fee_note: Some("County wrapper of the statewide PO 001 + PO 003/003A packet (RCW 7.105.115 Mandatory, 03/2026); deliver by 2:30 p.m. for same-day commissioner review; only one order type may be checked. Fees per the Clerk's fee schedule eff. 6/11/2026: anti-harassment petition $53.00; protection of vulnerable adults — no filing fee; DV/SA/stalking petitions have no fee line.".into()),
            fillability: Some(Fillability::Unverified),

            },
        ],
    },
    // Benton County Superior Court: the statewide pattern Declaration
    // (Optional Use) is the blank the guided client declaration follows;
    // it is filed in the pending Superior Court matter, so no separate
    // filing fee or fee-waiver form applies to it.
    Court {
        key: "benton_county_superior_court".into(),
        name: "Benton County Superior Court (Washington)".into(),
        jurisdiction: "WA".into(),
        county: Some("Benton".into()),
        city: None,
        level: CourtLevel::Superior,
        forms: vec![
            CourtForm {
            code: "wa__benton_county_declaration".into(),
            title: "Client Declaration — Benton County Superior Court".into(),
            form_number: Some("WPF DR 09.0100".into()),
            source_url: "https://www.courts.wa.gov/forms/documents/model.pdf".into(),
            sha256: "734d2b038130a9c0116f604e3895716f07cfc3ec809368fa892c6e8f50f24a78".into(),
            filing_office: "Benton County Superior Court clerk".into(),
            filing_fee: None,
            fee_note: None,
            fee_waiver_code: None,

            },
        ],
        surveyed_forms: Vec::new(),
    },
    // Name changes and small claims ($50; clerk provides the Notice of Small
    // Claim — no static PDF on the page). GR 34 fee waiver on request.
    Court {
        key: "clark_county_district_court".into(),
        name: "Clark County District Court (Washington)".into(),
        jurisdiction: "WA".into(),
        county: Some("Clark".into()),
        city: None,
        level: CourtLevel::District,
        forms: Vec::new(),
        surveyed_forms: vec![
            SurveyedForm {
            key: "CLARKWAPETADULT".into(),
            title: "Petition for Individual Adult Name Change".into(),
form_number: None,
            source_url: "https://clark.wa.gov/media/document/95066".into(),
            sha256: "e0c21d4ca3455ede2a4eeb4bc09d28906dc2382567fd8a14caddc0be07df4084".into(),
            filing_office: "Clark County District Court clerk, 1200 Franklin St, Vancouver".into(),
            filing_fee: None,
            fee_note: Some("Filing fees due at filing, non-refundable: $296.50 individual adult; $298.50 family or minor. Petitions filed in person; hearings virtual by default; additional certified copies $5. GR 34 fee waiver available upon request. Sealed name changes go to the Superior Court Clerk's Office.".into()),
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "CLARKWAPETMINOR".into(),
            title: "Petition for Minor Name Change".into(),
form_number: None,
            source_url: "https://clark.wa.gov/media/document/95061".into(),
            sha256: "310de65385cb2db2be8a094742d095d34c8e506c417fded0413ff2035a39ff94".into(),
            filing_office: "Clark County District Court clerk".into(),
            filing_fee: None,
            fee_note: Some("Fee $298.50 (family or minor).".into()),
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "CLARKWAPETFAMILY".into(),
            title: "Petition for Family Name Change".into(),
form_number: None,
            source_url: "https://clark.wa.gov/media/document/95056".into(),
            sha256: "d8c0e84c977df2becc4205aa0aae2d0ffd1baa46b437375b52be05057ba2491e".into(),
            filing_office: "Clark County District Court clerk".into(),
            filing_fee: None,
            fee_note: Some("Fee $298.50 (family or minor).".into()),
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "CLARKWACONFID".into(),
            title: "Confidential Identification Information Form (required for ALL name changes)".into(),
form_number: None,
            source_url: "https://clark.wa.gov/media/document/212476".into(),
            sha256: "eb9344b6eab13a5968e2d0391385c386006b7b56e617fa5869c8f66e6c18b7d5".into(),
            filing_office: "Clark County District Court clerk".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Unverified),

            },
            SurveyedForm {
            key: "CLARKWACONSENT".into(),
            title: "Consent of Natural Parent for Minor Name Change (notarized)".into(),
form_number: None,
            source_url: "https://clark.wa.gov/media/document/95076".into(),
            sha256: "f67a8574c15672f065f1a4200694868cd3f7c7ca96d63a1f3540759102d794c1".into(),
            filing_office: "Clark County District Court clerk".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Unverified),

            },
        ],
    },
    // Sealed name changes and probate/wills/trusts live with the Clerk; civil
    // filing $290.00 (Clerk fee schedule). Statewide GR 34 waiver forms apply.
    Court {
        key: "clark_county_superior_court".into(),
        name: "Clark County Superior Court (Washington)".into(),
        jurisdiction: "WA".into(),
        county: Some("Clark".into()),
        city: None,
        level: CourtLevel::Superior,
        forms: Vec::new(),
        surveyed_forms: vec![
            SurveyedForm {
            key: "CLARKWADVSEALED".into(),
            title: "Instructions for a DV Sealed Name Change Petition in Clark County Superior Court".into(),
form_number: None,
            source_url: "https://clark.wa.gov/sites/default/files/dept/files/law-library/brochures/DV-Sealed-Name-Change-Packet-Instructions_and_Forms.pdf".into(),
            sha256: "350610a4e4bbf188a737ba25fcb5ab2b0641e1df07534fc7fc5ae69aa5f90161".into(),
            filing_office: "Clark County Superior Court Clerk's Office, 1200 Franklin St, Vancouver".into(),
            filing_fee: None,
            fee_note: Some("Public Law Library of King County packet hosted by Clark County. Superior civil filing $290.00; small-estate affidavit filing $20.00 (blank forms $4.00); will-only filing $20.00 (Clerk fee schedule). GR 34 fee waiver forms available through the court administrator.".into()),
            fillability: Some(Fillability::Unverified),

            },
        ],
    },
    // The Unified Court System's name/sex-designation forms are the same
    // statewide whether filed in Supreme/County Court or NYC Civil Court;
    // this record carries the Supreme/County Court route (fees and RJI
    // note included). County Court accepts the same forms at the same fee.
    Court {
        key: "new_york_supreme_court".into(),
        name: "New York Supreme Court".into(),
        jurisdiction: "NY".into(),
        county: None,
        city: None,
        level: CourtLevel::Superior,
        forms: vec![
            CourtForm {
            code: "ny__nc1_adult_name_change_petition".into(),
            title: "Name Change and/or Sex Designation Change Petition for Individual Adult (18+)".into(),
form_number: None,
            source_url: "https://www.nycourts.gov/forms/Name-SexDesignationChange/AdultName-SexDesignationChangePetition-fillable.pdf".into(),
            sha256: "8c771a7ce619ebb9f717f613fb47ff7a8acfd2019980d4e757182afa0ee2174e".into(),
            filing_office: "Supreme Court County Clerk's Office in the county of residence".into(),
            filing_fee: Some(FilingFee {
                amount: "210.00".into(),
                currency: "USD".into(),
            }),
            fee_note: Some("One template serves both routes the form itself supports: NYC Civil Court at $65.00, or Supreme/County Court of the county of residence at $210.00 (the intake asks which court, confirmed by the lawyer at review). Fee waiver: poor person relief by motion with a sworn financial affidavit — no official statewide form. Registry sha256 pins the re-authored canonical blank; the official blank's pin lives in the NC1 surveyed_forms entry.".into()),
            fee_waiver_code: None,

            },
            CourtForm {
            code: "ny__nc3_lip_name_change_petition".into(),
            title: "Name Change and/or Sex Designation Change Petition for a Legally Incapacitated Person".into(),
form_number: None,
            source_url: "https://www.nycourts.gov/forms/Name-SexDesignationChange/LIPName-SexDesignationChangePetition-fillable.pdf".into(),
            sha256: "10a0ab457acbbb88b0439cf81699418fda7eaef776d23d96ebd47f1b4b8f11b9".into(),
            filing_office: "Supreme Court County Clerk's Office in the county of residence".into(),
            filing_fee: Some(FilingFee {
                amount: "210.00".into(),
                currency: "USD".into(),
            }),
            fee_note: Some("Brought by the person's guardian, committee, or parent as petitioner (the intake captures both names). Same dual-route fees as NC1: NYC Civil Court $65.00 or Supreme/County Court $210.00. Fee waiver: poor person relief by motion — no official statewide form. Registry sha256 pins the re-authored canonical blank; the official blank's pin lives in the NC3 surveyed_forms entry.".into()),
            fee_waiver_code: None,

            },
            CourtForm {
            code: "ny__nc2_minor_name_change_petition".into(),
            title: "Name Change and/or Sex Designation Change Petition for an Individual Minor (under 18)".into(),
form_number: None,
            source_url: "https://www.nycourts.gov/forms/Name-SexDesignationChange/MinorName-SexDesignationChangePetition-fillable.pdf".into(),
            sha256: "fc1aa34c334da4b45932de1ee0a582771fdcf29524eb690adde21de48a44d905".into(),
            filing_office: "Supreme Court County Clerk's Office in the county of residence".into(),
            filing_fee: Some(FilingFee {
                amount: "210.00".into(),
                currency: "USD".into(),
            }),
            fee_note: Some("Brought by a parent or legal guardian for a child under 18; a child 14 or older signs a notarized consent, and a non-petitioning parent or guardian consents or is given notice. Same dual-route fees as NC1: NYC Civil Court $65.00 or Supreme/County Court $210.00. Fee waiver: poor person relief by motion — no official statewide form. Registry sha256 pins the re-authored canonical blank; the official blank's pin lives in the NC2 surveyed_forms entry.".into()),
            fee_waiver_code: None,

            },
            CourtForm {
            code: "ny__nc4_family_name_change_petition".into(),
            title: "Family Name Change Petition".into(),
form_number: None,
            source_url: "https://www.nycourts.gov/FORMS/Name-SexDesignationChange/FamilyNameChangePetition-fillable.pdf".into(),
            sha256: "9beb5f0dc13a8798255ad20e5560afe88f3e02ed337f86aa853f00c4858f5ad9".into(),
            filing_office: "Supreme Court County Clerk's Office in the county of residence".into(),
            filing_fee: Some(FilingFee {
                amount: "210.00".into(),
                currency: "USD".into(),
            }),
            fee_note: Some("Family petition brought for more than one person at the same time (petitioners file together; NC5 supplies the additional petitioners' pages). Same dual-route fees as NC1: NYC Civil Court $65.00 or Supreme/County Court $210.00. Fee waiver: poor person relief by motion — no official statewide form. Registry sha256 pins the re-authored canonical blank; the official blank's pin lives in the NC4 surveyed_forms entry.".into()),
            fee_waiver_code: None,

            },
            CourtForm {
            code: "ny__nc5_family_name_change_supplement".into(),
            title: "Family Name Change Petition — Supplemental Form".into(),
form_number: None,
            source_url: "https://www.nycourts.gov/FORMS/Name-SexDesignationChange/SupplementalFamilyNameChangePetition-fillable.pdf".into(),
            sha256: "d625a28b846790890b86303a770867547635ec733c4a93c5fcdfeac856b24e68".into(),
            filing_office: "Supreme Court County Clerk's Office in the county of residence".into(),
            filing_fee: Some(FilingFee {
                amount: "210.00".into(),
                currency: "USD".into(),
            }),
            fee_note: Some("Supplemental pages filed with the NC4 family petition — additional petitioners and cross-references; no separate filing fee beyond the NC4 filing. Same dual-route fees as NC1: NYC Civil Court $65.00 or Supreme/County Court $210.00. Fee waiver: poor person relief by motion — no official statewide form. Registry sha256 pins the re-authored canonical blank; the official blank's pin lives in the NC5 surveyed_forms entry.".into()),
            fee_waiver_code: None,

            },
        ],
        surveyed_forms: vec![
            SurveyedForm {
            key: "NC1".into(),
            title: "Name Change and/or Sex Designation Change Petition for Individual Adult (18+)".into(),
form_number: Some("UCS-NC1".into()),
            source_url: "https://www.nycourts.gov/forms/Name-SexDesignationChange/AdultName-SexDesignationChangePetition-fillable.pdf".into(),
            sha256: "fc46bfdaa74da7b0c52d3f464d6ccc486725bdd823c2ded423e0b8e4a9685471".into(),
            filing_office: "Supreme Court County Clerk's Office in the county of residence".into(),
            filing_fee: Some(FilingFee {
                amount: "210.00".into(),
                currency: "USD".into(),
            }),
            fee_note: Some("Filed in Supreme Court or County Court of the county of residence (or any NYC Civil Court — see that court's record; Civil Court costs less). Supreme/County Court fee $210.00; a minor 14+ signs a notarized consent on the minor route. The same UCS-NC forms carry sex-designation changes (no separate petition form exists); the birth-certificate step runs through the NYC Health Attestation Form (born in NYC) or the NYS DOH Application for Correction of Certificate of Birth for Gender Designation (born in NYS outside NYC). Fee waiver: poor person relief by motion with a sworn financial affidavit — no official statewide form. sha256 pins a Wayback-archived copy of the blank (the live nycourts.gov URL serves a Cloudflare bot challenge to plain clients).".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "NCO1".into(),
            title: "Order Granting Name Change and/or Sex Designation Change, Individual Adult".into(),
form_number: Some("UCS-NCO1".into()),
            source_url: "https://www.nycourts.gov/forms/Name-SexDesignationChange/AdultName-SexDesignationChangeOrder-fillable.pdf".into(),
            sha256: "d769b61f6ed3fe567c66a041db1e623cdf22b4564403f18e023b96c88d2ec42b".into(),
            filing_office: "Supreme Court County Clerk's Office in the county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "NC2".into(),
            title: "Name Change and/or Sex Designation Change Petition for Individual Minor (under 18)".into(),
form_number: Some("UCS-NC2".into()),
            source_url: "https://www.nycourts.gov/forms/Name-SexDesignationChange/MinorName-SexDesignationChangePetition-fillable.pdf".into(),
            sha256: "9279ee869c3d52b5eb74550de3ee8fae914c093d189b612eae269b1669144939".into(),
            filing_office: "Supreme Court County Clerk's Office in the county of residence".into(),
            filing_fee: None,
            fee_note: Some("Minor petition; a minor 14 or older must sign a notarized consent and the other parent/guardian must consent or be noticed. Same $210.00 Supreme Court fee. sha256 pins a Wayback-archived copy (Cloudflare).".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "NCO2".into(),
            title: "Order Granting Name Change and/or Sex Designation Change, Individual Minor".into(),
form_number: Some("UCS-NCO2".into()),
            source_url: "https://www.nycourts.gov/forms/Name-SexDesignationChange/MinorName-SexDesignationChangeOrder-fillable.pdf".into(),
            sha256: "1f052a250dc379819ab05a6ce59651c2e49f552c9b13e50d48507095674bf470".into(),
            filing_office: "Supreme Court County Clerk's Office in the county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "NC3".into(),
            title: "Name Change and/or Sex Designation Change Petition for a Legally Incapacitated Person".into(),
form_number: Some("UCS-NC3".into()),
            source_url: "https://www.nycourts.gov/forms/Name-SexDesignationChange/LIPName-SexDesignationChangePetition-fillable.pdf".into(),
            sha256: "27f3d73ef1a157430df7a33e653e366cb5af855f51b61721e561a03016ad7147".into(),
            filing_office: "Supreme Court County Clerk's Office in the county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "NCO3".into(),
            title: "Order Granting Name Change and/or Sex Designation Change, Legally Incapacitated Person".into(),
form_number: Some("UCS-NCO3".into()),
            source_url: "https://www.nycourts.gov/forms/Name-SexDesignationChange/LIPName-SexDesignationChangeOrder-fillable.pdf".into(),
            sha256: "bb6eac4de2c0f59b5dba874bcaa2e390a87d71e79cd96fec33eb0866871e4a46".into(),
            filing_office: "Supreme Court County Clerk's Office in the county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "NC4".into(),
            title: "Family Name Change Petition".into(),
form_number: Some("UCS-NC4".into()),
            source_url: "https://www.nycourts.gov/FORMS/Name-SexDesignationChange/FamilyNameChangePetition-fillable.pdf".into(),
            sha256: "5f3e6b79d6d5e7f4b2314f1ac80b0f8e612eb0c0a8025ec77c04af69ca35373d".into(),
            filing_office: "Supreme Court County Clerk's Office in the county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "NC5".into(),
            title: "Family Name Change Petition — Supplemental Form".into(),
form_number: Some("UCS-NC5".into()),
            source_url: "https://www.nycourts.gov/FORMS/Name-SexDesignationChange/SupplementalFamilyNameChangePetition-fillable.pdf".into(),
            sha256: "835d178a5887efeb5c8f279eed8cfbea434fca331496b09bdeb5616a5974a97f".into(),
            filing_office: "Supreme Court County Clerk's Office in the county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "NCO4".into(),
            title: "Order Granting a Family Name Change".into(),
form_number: Some("UCS-NCO4".into()),
            source_url: "https://www.nycourts.gov/FORMS/Name-SexDesignationChange/FamilyNameChangeOrder-fillable.pdf".into(),
            sha256: "a9f0f4412aba4ed128ec9207dae94a3e48af1e585321aa39f8cee7ed0429a472".into(),
            filing_office: "Supreme Court County Clerk's Office in the county of residence".into(),
            filing_fee: None,
            fee_note: None,
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "UCS840".into(),
            title: "Request for Judicial Intervention".into(),
form_number: Some("UCS-840".into()),
            source_url: "https://www.nycourts.gov/LegacyPDFS/forms/rji/UCS-840-fillable.pdf".into(),
            sha256: "6314bde92766f82d9851b6ffcddb7017aa004c8e33529c024d75887e1031d71b".into(),
            filing_office: "Supreme Court County Clerk's Office in the county of residence".into(),
            filing_fee: None,
            fee_note: Some("Supreme Court only. The name-change self-help page states there is no fee to file this form in a name-change proceeding; the general fee schedule lists the RJI at $95 (CPLR 8020(a)). sha256 pins a Wayback-archived copy (Cloudflare).".into()),
            fillability: Some(Fillability::Fillable),

            },
        ],
    },
    // A NYC resident may bring a name or sex-designation change proceeding in
    // any county in the city here ($65.00 vs $210.00 in Supreme Court). The
    // Small Claims Part limit is $10,000 (individuals only; no e-filing).
    // Commercial claims sit in the same part; their filing fee is not stated
    // on the surveyed pages. Small-estate DIY (SE-3A) belongs to Surrogate's
    // Court, below.
    Court {
        key: "nyc_civil_court".into(),
        name: "New York City Civil Court".into(),
        jurisdiction: "NY".into(),
        county: None,
        city: Some("New York".into()),
        level: CourtLevel::City,
        forms: vec![
            CourtForm {
            code: "ny__civ_sc50_statement_of_claim".into(),
            title: "Statement of Claim (Small Claims)".into(),
form_number: None,
            source_url: "https://www.nycourts.gov/LegacyPDFS/courts/nyc/civil/forms/CIV-SC-50.pdf".into(),
            sha256: "2f1808d206ec948fb2f9b4ce8f5471eee1ca2762ea58089fa4bfec52e84ba070".into(),
            filing_office: "NYC Civil Court, Small Claims Part clerk".into(),
            filing_fee: None,
            fee_note: Some("Filing fee tiered by claim amount: $15.00 on claims up to $1,000.00, $20.00 above $1,000.00 (individuals only; the Small Claims Part limit is $10,000.00 and there is no e-filing). Waiver: poor person relief by motion with a sworn financial affidavit — no official statewide form. Registry sha256 pins the re-authored canonical blank; the official blank's pin lives in the CIVSC50 surveyed_forms entry.".into()),
            fee_waiver_code: None,

            },
        ],
        surveyed_forms: vec![
            SurveyedForm {
            key: "CIVNCADULT".into(),
            title: "Petition for Individual Adult for Leave to Change His/Her Name (NYC Civil Court format)".into(),
form_number: None,
            source_url: "https://www.nycourts.gov/courts/nyc/civil/forms/namechangeadult.pdf".into(),
            sha256: "fd1878e75a41d73d0977f984a68e094546f0a95f3323e184964f8bf5304807b9".into(),
            filing_office: "NYC Civil Court clerk, any county in the city".into(),
            filing_fee: Some(FilingFee {
                amount: "65.00".into(),
                currency: "USD".into(),
            }),
            fee_note: Some("NYC Civil Court fee $65.00 vs $210.00 in Supreme Court; a NYC resident may file in any county in the city. sha256 pins a Wayback-archived copy (Cloudflare).".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "CIVNCINSTR".into(),
            title: "Name Change Instructions (NYC Civil Court, non-fillable)".into(),
form_number: None,
            source_url: "https://www.nycourts.gov/courts/nyc/civil/forms/NameChangeInstructions.pdf".into(),
            sha256: "86e20e358b9635a44783ce7ffeb7f4d183dea1268552d0d44b99848c41ac1578".into(),
            filing_office: "NYC Civil Court clerk, any county in the city".into(),
            filing_fee: None,
            fee_note: Some("n/a (instructions). sha256 pins a Wayback-archived copy (Cloudflare).".into()),
            fillability: Some(Fillability::UnfillableSource),

            },
            SurveyedForm {
            key: "CIVSC50".into(),
            title: "Statement of Claim (Small Claims)".into(),
form_number: Some("CIV-SC-50".into()),
            source_url: "https://www.nycourts.gov/LegacyPDFS/courts/nyc/civil/forms/CIV-SC-50.pdf".into(),
            sha256: "348e3117b09df5d583b3011331abed4628a38d2eab327edf8bbabb4cd1cfbe39".into(),
            filing_office: "NYC Civil Court, Small Claims Part clerk".into(),
            filing_fee: Some(FilingFee {
                amount: "15.00".into(),
                currency: "USD".into(),
            }),
            fee_note: Some("Filing fee $15.00 for claims up to $1,000; $20.00 above $1,000. Limit $10,000 in NYC; individuals only (corporations cannot sue). Cannot be filed electronically. sha256 pins a Wayback-archived copy (Cloudflare).".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "CIVSC70".into(),
            title: "Statement of Claim (Commercial Claims)".into(),
form_number: Some("CIV-SC-70".into()),
            source_url: "https://www.nycourts.gov/LegacyPDFS/courts/nyc/civil/forms/CIV-SC-70.pdf".into(),
            sha256: "e520362b579495e989717fbe3739c97701725b592aff2d9a6c32c1ce943f9ee1".into(),
            filing_office: "NYC Civil Court, Commercial Claims Part clerk".into(),
            filing_fee: None,
            fee_note: Some("The surveyed pages state no separate commercial-claims filing fee — amount unknown. sha256 pins a Wayback-archived copy (Cloudflare).".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "CIVGP67A".into(),
            title: "Notice of Appeal (small claims; cannot be used after arbitration)".into(),
form_number: Some("CIV-GP-67A".into()),
            source_url: "https://www.nycourts.gov/LegacyPDFS/courts/nyc/smallclaims/forms/noticeofappeal.pdf".into(),
            sha256: "a3c57081d61b976d366811fb5d3e12aa832c98f5227a0380835f66c6af3716db".into(),
            filing_office: "NYC Civil Court, Small Claims Part clerk".into(),
            filing_fee: None,
            fee_note: Some("sha256 pins a Wayback-archived copy (Cloudflare).".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "SCFEEWAIV".into(),
            title: "Fee Waiver Form (small claims / commercial claims)".into(),
form_number: None,
            source_url: "https://www.nycourts.gov/LegacyPDFS/courts/nyc/civil/forms/FeeWaiver.pdf".into(),
            sha256: "837763d46b405fe0f153547338b9988229ff48fc9d0d0805ee77bb231ab26748".into(),
            filing_office: "NYC Civil Court, Small Claims Part clerk".into(),
            filing_fee: None,
            fee_note: Some("No filing fee for the application. sha256 pins a Wayback-archived copy (Cloudflare).".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "POORPERSON".into(),
            title: "Proceed as a Poor Person and to Waive Court Fees".into(),
form_number: None,
            source_url: "https://nycourts.gov/courts/nyc/civil/forms/PoorPersonsRelief.pdf".into(),
            sha256: "5f2aadc58c58933e84ef57a846df52b0d8c1c01a278219e7570ea5ef8c8dff95".into(),
            filing_office: "NYC Civil Court clerk".into(),
            filing_fee: None,
            fee_note: Some("General civil fee-waiver application. sha256 pins a Wayback-archived copy (Cloudflare).".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "UCS124".into(),
            title: "Demand Letter (consumer credit transaction cases)".into(),
form_number: Some("UCS-124".into()),
            source_url: "https://www.nycourts.gov/LegacyPDFS/courts/nyc/civil/pdfs/UCS124_DmdLtr.pdf".into(),
            sha256: "f4b311d543a6c22820f71c0a6a1c1573ecdf13912d171e784c1b3fb428231dd0".into(),
            filing_office: "Sent by the claimant before filing; not a court filing".into(),
            filing_fee: None,
            fee_note: Some("n/a — evidence attachment, not a filing form. sha256 pins a Wayback-archived copy (Cloudflare).".into()),
            fillability: Some(Fillability::UnfillableSource),

            },
        ],
    },
    // The probate self-help path for small estates (personal property up to
    // $50,000) is the official Small Estate Affidavit DIY program (LawHelp
    // Interactive). The paper SE-3A blank (SEAFF09.pdf) is live and its text
    // was extracted in the survey, but its fillability could NOT be verified
    // (Cloudflare + no Wayback binary), so it carries no entry here rather
    // than a guessed one. Probate fees vary by estate under SCPA 2402.
    Court {
        key: "new_york_surrogates_court".into(),
        name: "New York Surrogate's Court".into(),
        jurisdiction: "NY".into(),
        county: None,
        city: None,
        level: CourtLevel::Surrogate,
        forms: vec![
            CourtForm {
            code: "ny__probate_petition".into(),
            title: "Petition for Probate and Letters Testamentary / Trusteeship".into(),
form_number: None,
            source_url: "https://www.nycourts.gov/LegacyPDFS/FORMS/surrogates/pdfs/Probate.pdf".into(),
            sha256: "41e297f070ae213b0e4ae5f2306936df65d9e8b9aa0693a21ca677d61210d286".into(),
            filing_office: "Surrogate's Court of the county where the decedent lived".into(),
            filing_fee: None,
            fee_note: Some("Filing fees vary by proceeding and estate size under SCPA 2402; the UCS does not publish the table on its fee page. Fee waiver: poor person relief by motion (no official form). Registry sha256 pins the re-authored canonical blank; the official blank's pin lives in the PROBATE surveyed_forms entry.".into()),
            fee_waiver_code: None,

            },
        ],
        surveyed_forms: vec![
            SurveyedForm {
            key: "PROBATE".into(),
            title: "Petition for Probate and Letters Testamentary / Trusteeship".into(),
form_number: None,
            source_url: "https://www.nycourts.gov/LegacyPDFS/FORMS/surrogates/pdfs/Probate.pdf".into(),
            sha256: "8b31d7d781309658a9522e6893a69985c07dc486a1f1ad3d5efab6d721bd0b97".into(),
            filing_office: "Surrogate's Court of the county where the decedent lived".into(),
            filing_fee: None,
            fee_note: Some("Filing fees vary by proceeding and estate size under SCPA 2402; the UCS does not publish the table on its fee page. Fee waiver: poor person relief by motion (no official form). sha256 pins a Wayback-archived copy of the blank (the live nycourts.gov URL serves a Cloudflare bot challenge). The flagship self-help packet: 22 pages, 776 AcroForm fields.".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "ANCPROB".into(),
            title: "Petition for Ancillary Probate".into(),
form_number: None,
            source_url: "https://www.nycourts.gov/LegacyPDFS/FORMS/surrogates/pdfs/ancprob.pdf".into(),
            sha256: "4da52253c839f5e417eaaf90446c4ddd56fe426c69336598bdb55352cd48f864".into(),
            filing_office: "Surrogate's Court of the county where the decedent lived".into(),
            filing_fee: None,
            fee_note: Some("Filing fees vary by proceeding and estate size under SCPA 2402; the UCS does not publish the table on its fee page. Fee waiver: poor person relief by motion (no official form). sha256 pins a Wayback-archived copy of the blank (the live nycourts.gov URL serves a Cloudflare bot challenge).".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "PROBCTA".into(),
            title: "Petition for Letters of Administration c.t.a. (after Probate)".into(),
form_number: None,
            source_url: "https://www.nycourts.gov/LegacyPDFS/FORMS/surrogates/pdfs/probcta.pdf".into(),
            sha256: "8d1c21f6213b700a2b5ff2b0e3b34bae8d6d3e29a4e9cb831a7662e0071b48c0".into(),
            filing_office: "Surrogate's Court of the county where the decedent lived".into(),
            filing_fee: None,
            fee_note: Some("Filing fees vary by proceeding and estate size under SCPA 2402; the UCS does not publish the table on its fee page. Fee waiver: poor person relief by motion (no official form). sha256 pins a Wayback-archived copy of the blank (the live nycourts.gov URL serves a Cloudflare bot challenge).".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "SUCTEST".into(),
            title: "Petition for Successor Letters Testamentary".into(),
form_number: None,
            source_url: "https://www.nycourts.gov/LegacyPDFS/FORMS/surrogates/pdfs/PDF%20Fillable%20SuccessorTest.pdf".into(),
            sha256: "994abda0aff238567d845910ee07ca5eb02f42046fa80c818444b616e0089144".into(),
            filing_office: "Surrogate's Court of the county where the decedent lived".into(),
            filing_fee: None,
            fee_note: Some("Filing fees vary by proceeding and estate size under SCPA 2402; the UCS does not publish the table on its fee page. Fee waiver: poor person relief by motion (no official form). sha256 pins a Wayback-archived copy of the blank (the live nycourts.gov URL serves a Cloudflare bot challenge).".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "ADM".into(),
            title: "Administration Proceeding, Petition for Letters of Administration / Limited Administration".into(),
form_number: None,
            source_url: "https://www.nycourts.gov/LegacyPDFS/FORMS/surrogates/pdfs/ADM.pdf".into(),
            sha256: "234f4b95a22c31c3c01de48600356541a224b4b954ac6580044a8770d7a1759e".into(),
            filing_office: "Surrogate's Court of the county where the decedent lived".into(),
            filing_fee: None,
            fee_note: Some("Filing fees vary by proceeding and estate size under SCPA 2402; the UCS does not publish the table on its fee page. Fee waiver: poor person relief by motion (no official form). sha256 pins a Wayback-archived copy of the blank (the live nycourts.gov URL serves a Cloudflare bot challenge).".into()),
            fillability: Some(Fillability::Fillable),

            },
            SurveyedForm {
            key: "ADMDBN".into(),
            title: "Petition for Letters of Administration d.b.n. (SCPA 1007)".into(),
form_number: None,
            source_url: "https://www.nycourts.gov/LegacyPDFS/FORMS/surrogates/pdfs/ADMdbn.pdf".into(),
            sha256: "61d53e62b012dc98058cae69a4a8e0840abc044d470c0a1483e439e2020c8acd".into(),
            filing_office: "Surrogate's Court of the county where the decedent lived".into(),
            filing_fee: None,
            fee_note: Some("Filing fees vary by proceeding and estate size under SCPA 2402; the UCS does not publish the table on its fee page. Fee waiver: poor person relief by motion (no official form). sha256 pins a Wayback-archived copy of the blank (the live nycourts.gov URL serves a Cloudflare bot challenge).".into()),
            fillability: Some(Fillability::UnfillableSource),

            },
            SurveyedForm {
            key: "SURRP14".into(),
            title: "Petition for Successor Letters Testamentary (legacy version)".into(),
form_number: Some("Surrogate-P14".into()),
            source_url: "https://www.nycourts.gov/LegacyPDFS/FORMS/surrogates/pdfs/Surrogate-P14.pdf".into(),
            sha256: "d774e5e614aa96c85a3f008d3dc9a34835684ed33a51c351242fe35f9f941136".into(),
            filing_office: "Surrogate's Court of the county where the decedent lived".into(),
            filing_fee: None,
            fee_note: Some("Filing fees vary by proceeding and estate size under SCPA 2402; the UCS does not publish the table on its fee page. Fee waiver: poor person relief by motion (no official form). sha256 pins a Wayback-archived copy of the blank (the live nycourts.gov URL serves a Cloudflare bot challenge).".into()),
            fillability: Some(Fillability::UnfillableSource),

            },
            SurveyedForm {
            key: "SURRP15".into(),
            title: "Renunciation of Successor Letters Testamentary and Waiver of Process (Individual)".into(),
form_number: Some("Surrogate-P15".into()),
            source_url: "https://www.nycourts.gov/LegacyPDFS/FORMS/surrogates/pdfs/Surrogate-P15.pdf".into(),
            sha256: "04102d6e7c075f9782ff784cf54665acd981dd9be96e6db1375edfe2a73a1f79".into(),
            filing_office: "Surrogate's Court of the county where the decedent lived".into(),
            filing_fee: None,
            fee_note: Some("Filing fees vary by proceeding and estate size under SCPA 2402; the UCS does not publish the table on its fee page. Fee waiver: poor person relief by motion (no official form). sha256 pins a Wayback-archived copy of the blank (the live nycourts.gov URL serves a Cloudflare bot challenge).".into()),
            fillability: Some(Fillability::UnfillableSource),

            },
            SurveyedForm {
            key: "SURRP16".into(),
            title: "Renunciation of Successor Letters Testamentary and Waiver of Process (Corporation)".into(),
form_number: Some("Surrogate-P16".into()),
            source_url: "https://www.nycourts.gov/LegacyPDFS/FORMS/surrogates/pdfs/Surrogate-P16.pdf".into(),
            sha256: "444f121b3a88ebb8059b9a7e750054b971a8d03bbb64a4d8b469ff25f2d75fa6".into(),
            filing_office: "Surrogate's Court of the county where the decedent lived".into(),
            filing_fee: None,
            fee_note: Some("Filing fees vary by proceeding and estate size under SCPA 2402; the UCS does not publish the table on its fee page. Fee waiver: poor person relief by motion (no official form). sha256 pins a Wayback-archived copy of the blank (the live nycourts.gov URL serves a Cloudflare bot challenge).".into()),
            fillability: Some(Fillability::UnfillableSource),

            },
            SurveyedForm {
            key: "SURRP17".into(),
            title: "Notice of Petition for Appointment of Successor Executor".into(),
form_number: Some("Surrogate-P17".into()),
            source_url: "https://www.nycourts.gov/LegacyPDFS/FORMS/surrogates/pdfs/Surrogate-P17.pdf".into(),
            sha256: "bbc8c2f09f0c94878ffbd138fc565a7b56d4dd541a1b0e5906f79bae0027a82d".into(),
            filing_office: "Surrogate's Court of the county where the decedent lived".into(),
            filing_fee: None,
            fee_note: Some("Filing fees vary by proceeding and estate size under SCPA 2402; the UCS does not publish the table on its fee page. Fee waiver: poor person relief by motion (no official form). sha256 pins a Wayback-archived copy of the blank (the live nycourts.gov URL serves a Cloudflare bot challenge).".into()),
            fillability: Some(Fillability::UnfillableSource),

            },
        ],
    },
    // No filing fees in Family Court. The family offense petition — the
    // highest-volume DV self-help form in NY — is print-and-write, not an
    // AcroForm; the order forms are court-issued. No DIY program exists for
    // it (verified against the Family Court DIY page).
    Court {
        key: "new_york_family_court".into(),
        name: "New York Family Court".into(),
        jurisdiction: "NY".into(),
        county: None,
        city: None,
        level: CourtLevel::Family,
        forms: Vec::new(),
        surveyed_forms: vec![
            SurveyedForm {
            key: "FC82".into(),
            title: "Family Offense Petition (FCA §§812, 818, 821)".into(),
form_number: Some("UCS-FC8-2".into()),
            source_url: "https://www.nycourts.gov/LegacyPDFS/FORMS/familycourt/pdfs/8-2.pdf".into(),
            sha256: "59aac9f005deecfdcc56e8c44a04f50b1a12c0d4599195f34fbeedfde1e6eb01".into(),
            filing_office: "Family Court clerk, any county".into(),
            filing_fee: Some(FilingFee {
                amount: "0.00".into(),
                currency: "USD".into(),
            }),
            fee_note: Some("It is free to file in Family Court. The published petition has zero AcroForm fields — print-and-write; filers can also complete it with help from court personnel, and no DIY program exists for family offense petitions. The order forms (8-3 through 8-7) are court-issued, not litigant-fillable. sha256 pins a Wayback-archived copy (Cloudflare).".into()),
            fillability: Some(Fillability::UnfillableSource),

            },
        ],
    },
]
});

#[cfg(test)]
mod tests {
    use crate::courts::validate;

    use super::COURT_SEEDS;

    #[test]
    fn every_seed_entry_is_well_formed() {
        validate(&COURT_SEEDS)
            .expect("the shipped court seeds validate against the registry schema");
    }

    /// Every surveyed entry states how its blank's fillability was verified
    /// — unknowns are explicit, never guessed away.
    #[test]
    fn surveyed_entries_carry_a_verified_fillability() {
        for court in COURT_SEEDS.iter() {
            for surveyed in &court.surveyed_forms {
                assert!(
                    surveyed.fillability.is_some(),
                    "court `{}` surveyed form `{}` carries no fillability",
                    court.key,
                    surveyed.key
                );
            }
        }
    }

    /// The provenance discipline the surveys committed to: no waiver
    /// template is vendored yet, so no entry names one.
    #[test]
    fn fee_waiver_codes_stay_null_until_a_waiver_template_is_vendored() {
        for court in COURT_SEEDS.iter() {
            for form in &court.forms {
                assert!(
                    form.fee_waiver_code.is_none(),
                    "court `{}` form `{}` names a fee-waiver template; the \
                     provenance note says none is vendored",
                    court.key,
                    form.code
                );
            }
        }
    }

    /// The inventory the four verified regional surveys captured, plus the
    /// Benton County Superior Court survey: 22 court records, 28
    /// template-backed form entries, 167 surveyed-only entries.
    #[test]
    fn the_seed_inventory_is_what_the_surveys_captured() {
        let forms: usize = COURT_SEEDS.iter().map(|court| court.forms.len()).sum();
        let surveyed: usize = COURT_SEEDS
            .iter()
            .map(|court| court.surveyed_forms.len())
            .sum();
        assert_eq!(COURT_SEEDS.len(), 22, "court records");
        assert_eq!(forms, 28, "template-backed form entries");
        assert_eq!(surveyed, 167, "surveyed-only entries");
    }
}
