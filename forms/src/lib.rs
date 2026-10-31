//! Vendored government forms — the metadata registry behind
//! `templates/notations/forms/`.
//!
//! The blank PDF bytes live **only** in the public assets bucket, at
//! each form's `object_path`. The repository keeps the diffable text:
//! the sibling markdown template (the catalog card), the re-authored
//! blank's `.fields` manifest — its `/T` names *are* question paths, so
//! [`fill_values`] fills with no map at all — and a `.sha256` pin of the
//! canonical blank. A `<code>.fields.toml` map is the one-time input
//! `navigator forms re-author` consumes (see [`fieldmap`]); it is
//! deleted once the blank is re-authored. The fill path pulls the blank
//! through `cloud::StorageService` and must verify it against the pin
//! before filling — a mismatch or a missing object is a loud failure,
//! never a fallback.

pub mod fieldmap;
pub mod notation;
pub mod reauthor;

pub use fieldmap::{parse_field_map, FieldMap, FieldMapError, FieldRule};
pub use reauthor::{manifest, resolve_reauthored, ReauthorPlanError, UNMAPPED_PREFIX};

/// Fill values for one form through its re-authored `.fields` manifest —
/// each `/T` name *is* its data path. `Ok(None)` when the form carries no
/// manifest — a reference document nothing fills.
///
/// # Errors
///
/// [`FieldMapError`] from manifest resolution.
pub fn fill_values(
    form_code: &str,
    answers: &std::collections::BTreeMap<String, String>,
) -> Result<Option<std::collections::BTreeMap<String, String>>, FieldMapError> {
    match reauthor::manifest(form_code) {
        Some(names) => {
            let names: Vec<String> = names.into_iter().map(ToString::to_string).collect();
            Ok(Some(reauthor::resolve_reauthored_with_title_defaults(
                &names,
                answers,
                title_defaults(form_code),
            )?))
        }
        None => Ok(None),
    }
}

fn title_defaults(form_code: &str) -> &'static [(&'static str, &'static str)] {
    match form_code {
        "nv__business_trust_formation" => &[("people__trustees", "Trustee")],
        _ => &[],
    }
}

/// A filing fee as the filing office publishes it: a decimal amount and an
/// ISO 4217 currency code. `&'static str` fields keep a fee expressible in
/// the compiled-in [`BUNDLED`] registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilingFee {
    /// Decimal amount (`"270.00"`, `"0.00"`).
    pub amount: &'static str,
    /// ISO 4217 alphabetic currency code (`USD`).
    pub currency: &'static str,
}

/// Metadata for one vendored government form. Carries no bytes: the
/// blank itself lives in the assets bucket at
/// [`object_path`](Self::object_path), pinned by
/// [`pinned_sha256`](Self::pinned_sha256).
#[derive(Debug, Clone)]
pub struct FormMeta {
    /// Stable form/template code. For forms, this is jurisdiction-first:
    /// `nv__llc_formation`, `us__form_990`, etc.
    pub code: &'static str,
    /// Jurisdiction code from `store/seeds/Jurisdiction.yaml`.
    pub jurisdiction: &'static str,
    /// Human title from the sibling markdown template.
    pub title: &'static str,
    /// Canonical government page where the blank can be obtained.
    pub origin_url: &'static str,
    /// Path of the blank in the public assets bucket. With `templates/`
    /// prepended it is also the repo path of the untracked working copy
    /// `navigator forms sync` uploads.
    pub object_path: &'static str,
    /// Raw contents of the sibling `.sha256` pin file — the sha-256 hex
    /// digest of the canonical blank. Public so a test harness can stage
    /// a synthetic blank under its own pin; production code reads
    /// [`pinned_sha256`](Self::pinned_sha256).
    pub sha256_pin: &'static str,
    /// The filing fee the office charges for this form, when it publishes
    /// a flat one. The court-form registry (`store/seeds/Court.yaml`) is
    /// the curated source of fee data; the cross-crate guard test pins a
    /// vendored form's fee to its registry row.
    pub filing_fee: Option<FilingFee>,
    /// Template code of the fee-waiver form the filing office accepts,
    /// when one exists. Must resolve to a real forms-shelf template.
    pub fee_waiver_code: Option<&'static str>,
}

impl FormMeta {
    /// The pinned sha-256 hex digest of the canonical blank.
    #[must_use]
    pub fn pinned_sha256(&self) -> &'static str {
        self.sha256_pin.trim()
    }

    /// Verify `bytes` against this form's pinned digest.
    ///
    /// # Errors
    ///
    /// [`IntegrityError`] when the digest does not match — the bytes are
    /// not the blank the repository pinned and must not be filled.
    pub fn verify(&self, bytes: &[u8]) -> Result<(), IntegrityError> {
        verify_sha256(self.pinned_sha256(), bytes)
    }
}

/// Errors loading the bundled registry.
#[derive(Debug, thiserror::Error)]
pub enum FormsError {
    #[error("forms registry unavailable")]
    Unavailable,
}

/// A blank's bytes do not match the repository's `.sha256` pin — the
/// bucket object was re-vendored (or tampered with) without updating
/// the pin. The fill path must stop here, loudly.
#[derive(Debug, thiserror::Error)]
#[error("blank form bytes do not match the pinned sha256 (pinned {pinned}, got {actual})")]
pub struct IntegrityError {
    pub pinned: String,
    pub actual: String,
}

/// The lowercase sha-256 hex digest of `bytes` — the encoding the
/// `.sha256` pin files carry.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Verify `bytes` against a pinned sha-256 hex digest.
///
/// # Errors
///
/// [`IntegrityError`] when the digest does not match.
pub fn verify_sha256(pinned: &str, bytes: &[u8]) -> Result<(), IntegrityError> {
    let actual = sha256_hex(bytes);
    if actual == pinned.trim() {
        Ok(())
    } else {
        Err(IntegrityError {
            pinned: pinned.trim().to_string(),
            actual,
        })
    }
}

const NV_SOS_FORMS_URL: &str =
    "https://www.nvsos.gov/businesses/commercial-recordings/forms-fees/all-business-forms";
const USCIS_N400_URL: &str = "https://www.uscis.gov/n-400";

const BUNDLED: &[FormMeta] = &[
    FormMeta {
        code: "us__naturalization",
        jurisdiction: "US",
        title: "Application for Naturalization -- Form N-400",
        origin_url: USCIS_N400_URL,
        object_path: "forms/united_states/federal/uscis/us__naturalization.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/federal/uscis/us__naturalization.sha256"
        ),
        filing_fee: None,
        fee_waiver_code: None,
    },
    FormMeta {
        code: "nv__llc_formation",
        jurisdiction: "NV",
        title: "Nevada LLC Formation",
        origin_url: NV_SOS_FORMS_URL,
        object_path: "forms/united_states/nevada/state/nv__llc_formation.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/nevada/state/nv__llc_formation.sha256"
        ),
        filing_fee: None,
        fee_waiver_code: None,
    },
    FormMeta {
        code: "nv__profit_corp_formation",
        jurisdiction: "NV",
        title: "Nevada Profit Corporation Formation",
        origin_url: NV_SOS_FORMS_URL,
        object_path: "forms/united_states/nevada/state/nv__profit_corp_formation.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/nevada/state/nv__profit_corp_formation.sha256"
        ),
        filing_fee: None,
        fee_waiver_code: None,
    },
    FormMeta {
        code: "nv__business_trust_formation",
        jurisdiction: "NV",
        title: "Nevada Business Trust Formation",
        origin_url: NV_SOS_FORMS_URL,
        object_path: "forms/united_states/nevada/state/nv__business_trust_formation.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/nevada/state/nv__business_trust_formation.sha256"
        ),
        filing_fee: None,
        fee_waiver_code: None,
    },
    // First-wave court forms: statewide Judicial Council blanks (California)
    // and Nevada Supreme Court self-help blanks, registered under their
    // filing courts in `store/seeds/Court.yaml`.
    FormMeta {
        code: "ca__name_change_petition",
        jurisdiction: "CA",
        title: "Petition for Change of Name",
        origin_url: "https://www.courts.ca.gov/documents/NC100.pdf",
        object_path: "forms/united_states/california/california_superior_court/ca__name_change_petition.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/california/california_superior_court/ca__name_change_petition.sha256"
        ),
        filing_fee: None,
        fee_waiver_code: None,
    },
    FormMeta {
        code: "ca__name_change_attachment",
        jurisdiction: "CA",
        title: "Name and Information About the Person Whose Name is to be Changed",
        origin_url: "https://www.courts.ca.gov/documents/NC110.pdf",
        object_path: "forms/united_states/california/california_superior_court/ca__name_change_attachment.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/california/california_superior_court/ca__name_change_attachment.sha256"
        ),
        filing_fee: None,
        fee_waiver_code: None,
    },
    FormMeta {
        code: "ca__name_change_order_to_show_cause",
        jurisdiction: "CA",
        title: "Order to Show Cause—Change of Name",
        origin_url: "https://www.courts.ca.gov/documents/NC120.pdf",
        object_path: "forms/united_states/california/california_superior_court/ca__name_change_order_to_show_cause.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/california/california_superior_court/ca__name_change_order_to_show_cause.sha256"
        ),
        filing_fee: None,
        fee_waiver_code: None,
    },
    FormMeta {
        code: "ca__name_change_decree",
        jurisdiction: "CA",
        title: "Decree Changing Name",
        origin_url: "https://www.courts.ca.gov/documents/NC130.pdf",
        object_path: "forms/united_states/california/california_superior_court/ca__name_change_decree.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/california/california_superior_court/ca__name_change_decree.sha256"
        ),
        filing_fee: None,
        fee_waiver_code: None,
    },
    FormMeta {
        code: "ca__civil_case_cover_sheet",
        jurisdiction: "CA",
        title: "Civil Case Cover Sheet",
        origin_url: "https://www.courts.ca.gov/documents/CM010.pdf",
        object_path: "forms/united_states/california/california_superior_court/ca__civil_case_cover_sheet.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/california/california_superior_court/ca__civil_case_cover_sheet.sha256"
        ),
        filing_fee: None,
        fee_waiver_code: None,
    },
    FormMeta {
        code: "ca__gender_identity_name_change_petition",
        jurisdiction: "CA",
        title: "Petition for Change of Name to Conform to Gender Identity",
        origin_url: "https://www.courts.ca.gov/documents/NC200.pdf",
        object_path: "forms/united_states/california/california_superior_court/ca__gender_identity_name_change_petition.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/california/california_superior_court/ca__gender_identity_name_change_petition.sha256"
        ),
        filing_fee: None,
        fee_waiver_code: None,
    },
    FormMeta {
        code: "ca__gender_identity_name_change_order_to_show_cause",
        jurisdiction: "CA",
        title: "Order to Show Cause—Change of Name to Conform to Gender Identity",
        origin_url: "https://www.courts.ca.gov/documents/NC225.pdf",
        object_path: "forms/united_states/california/california_superior_court/ca__gender_identity_name_change_order_to_show_cause.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/california/california_superior_court/ca__gender_identity_name_change_order_to_show_cause.sha256"
        ),
        filing_fee: None,
        fee_waiver_code: None,
    },
    FormMeta {
        code: "ca__gender_identity_name_change_decree",
        jurisdiction: "CA",
        title: "Decree Changing Name to Conform to Gender Identity",
        origin_url: "https://www.courts.ca.gov/documents/NC230.pdf",
        object_path: "forms/united_states/california/california_superior_court/ca__gender_identity_name_change_decree.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/california/california_superior_court/ca__gender_identity_name_change_decree.sha256"
        ),
        filing_fee: None,
        fee_waiver_code: None,
    },
    FormMeta {
        code: "ca__gender_recognition_petition",
        jurisdiction: "CA",
        title: "Petition for Recognition of Adult's Change of Gender and Sex Identifier, Name Change, and Issuance of New Certificates",
        origin_url: "https://www.courts.ca.gov/documents/NC300.pdf",
        object_path: "forms/united_states/california/california_superior_court/ca__gender_recognition_petition.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/california/california_superior_court/ca__gender_recognition_petition.sha256"
        ),
        filing_fee: None,
        fee_waiver_code: None,
    },
    FormMeta {
        code: "ca__gender_recognition_order",
        jurisdiction: "CA",
        title: "Order Recognizing Adult's Change of Gender and Sex Identifier, for Name Change and for Issuance of New Certificates",
        origin_url: "https://www.courts.ca.gov/documents/NC330.pdf",
        object_path: "forms/united_states/california/california_superior_court/ca__gender_recognition_order.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/california/california_superior_court/ca__gender_recognition_order.sha256"
        ),
        filing_fee: None,
        fee_waiver_code: None,
    },
    FormMeta {
        code: "nv__adult_name_change_petition",
        jurisdiction: "NV",
        title: "Petition for Adult Name Change",
        origin_url: "https://selfhelp.nvcourts.gov/images/name-change/name-adult-petition-pdf-fillable.pdf",
        object_path: "forms/united_states/nevada/nevada_supreme_court/nv__adult_name_change_petition.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/nevada/nevada_supreme_court/nv__adult_name_change_petition.sha256"
        ),
        filing_fee: None,
        fee_waiver_code: None,
    },
    FormMeta {
        code: "nv__dv_protection_order_application",
        jurisdiction: "NV",
        title: "Application for Protection Order Against Domestic Violence",
        origin_url: "https://selfhelp.nvcourts.gov/images/protection-orders/dv-app-pdf-fillable.pdf",
        object_path: "forms/united_states/nevada/nevada_supreme_court/nv__dv_protection_order_application.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/nevada/nevada_supreme_court/nv__dv_protection_order_application.sha256"
        ),
        filing_fee: Some(FilingFee {
            amount: "0.00",
            currency: "USD",
        }),
        fee_waiver_code: None,
    },
    FormMeta {
        code: "wa__adult_name_change_petition",
        jurisdiction: "WA",
        title: "Petition for Name Change (Individual)",
        origin_url: "https://cdn.kingcounty.gov/-/media/king-county/courts/district-court/forms_resources_library_docs/name-change-forms/petition-for-name-change-individual-july_2026.pdf",
        object_path: "forms/united_states/washington/king_county_district_court/wa__adult_name_change_petition.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/washington/king_county_district_court/wa__adult_name_change_petition.sha256"
        ),
        filing_fee: Some(FilingFee {
            amount: "98.00",
            currency: "USD",
        }),
        fee_waiver_code: None,
    },
    FormMeta {
        code: "wa__minor_name_change_petition",
        jurisdiction: "WA",
        title: "Petition for Minor Child Name Change",
        origin_url: "https://cdn.kingcounty.gov/-/media/king-county/courts/district-court/forms_resources_library_docs/name-change-forms/petition-for-name-change-minor-october-2024.pdf",
        object_path: "forms/united_states/washington/king_county_district_court/wa__minor_name_change_petition.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/washington/king_county_district_court/wa__minor_name_change_petition.sha256"
        ),
        filing_fee: Some(FilingFee {
            amount: "98.00",
            currency: "USD",
        }),
        fee_waiver_code: None,
    },
    FormMeta {
        code: "wa__small_claims_notice",
        jurisdiction: "WA",
        title: "Notice of Small Claim",
        origin_url: "https://cdn.kingcounty.gov/-/media/king-county/courts/district-court/forms_resources_library_docs/small_claims_forms/forms_sc/notice-of-small-claim-september-2025.pdf",
        object_path: "forms/united_states/washington/king_county_district_court/wa__small_claims_notice.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/washington/king_county_district_court/wa__small_claims_notice.sha256"
        ),
        filing_fee: Some(FilingFee {
            amount: "50.00",
            currency: "USD",
        }),
        fee_waiver_code: None,
    },
    FormMeta {
        code: "wa__small_claims_plaintiff_packet",
        jurisdiction: "WA",
        title: "Small Claims Packet — Plaintiff",
        origin_url: "https://cdn.kingcounty.gov/-/media/king-county/courts/district-court/forms_resources_library_docs/small_claims_forms/info_instruct_small_claims/small_claims_plaintiff_packet-july-2026.pdf",
        object_path: "forms/united_states/washington/king_county_district_court/wa__small_claims_plaintiff_packet.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/washington/king_county_district_court/wa__small_claims_plaintiff_packet.sha256"
        ),
        filing_fee: None,
        fee_waiver_code: None,
    },

    FormMeta {
        code: "nv__washoe_adult_name_change_petition",
        jurisdiction: "NV",
        title: "Petition for Adult Name Change (Washoe)",
        origin_url: "https://www.washoecourts.com/Forms/Family/10.%20Name%20Changes/NA%20Adult%20Name%20Change/1.%20Petition%20for%20Adult%20Name%20Change.pdf",
        object_path: "forms/united_states/nevada/second_judicial_district_court/nv__washoe_adult_name_change_petition.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/nevada/second_judicial_district_court/nv__washoe_adult_name_change_petition.sha256"
        ),
        filing_fee: None,
        fee_waiver_code: None,
    },
    FormMeta {
        code: "nv__washoe_fee_waiver_application",
        jurisdiction: "NV",
        title: "Application for Waiver of Fees and Costs (Washoe)",
        origin_url: "https://www.washoecourts.com/Forms/Family/1.%20All%20Other%20Family%20Related%20Packets/F-6%20Application%20to%20Waive%20Fees%20and%20Costs/1.%20Application%20for%20Waiver%20of%20Fees%20and%20Costs.pdf",
        object_path: "forms/united_states/nevada/second_judicial_district_court/nv__washoe_fee_waiver_application.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/nevada/second_judicial_district_court/nv__washoe_fee_waiver_application.sha256"
        ),
        filing_fee: None,
        fee_waiver_code: None,
    },

    FormMeta {
        code: "nv__las_vegas_small_claims_complaint",
        jurisdiction: "NV",
        title: "Complaint, Small Claims (Las Vegas)",
        origin_url: "https://www.civillawselfhelpcenter.org/images/small-claims/complaints/complaint---small-claims.pdf",
        object_path: "forms/united_states/nevada/las_vegas_justice_court/nv__las_vegas_small_claims_complaint.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/nevada/las_vegas_justice_court/nv__las_vegas_small_claims_complaint.sha256"
        ),
        // The complaint's fee is tiered by claim amount ($66-$196); no
        // single honest figure fits FormMeta, so the registry fee_note
        // carries the schedule.
        filing_fee: None,
        fee_waiver_code: None,
    },
    FormMeta {
        code: "nv__las_vegas_small_claims_counterclaim",
        jurisdiction: "NV",
        title: "Counterclaim, Small Claims (Las Vegas)",
        origin_url: "https://www.civillawselfhelpcenter.org/images/small-claims/counterclaims/counterclaim---small-claims---las-vegas.pdf",
        object_path: "forms/united_states/nevada/las_vegas_justice_court/nv__las_vegas_small_claims_counterclaim.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/nevada/las_vegas_justice_court/nv__las_vegas_small_claims_counterclaim.sha256"
        ),
        filing_fee: None,
        fee_waiver_code: None,
    },
    FormMeta {
        code: "ny__nc1_adult_name_change_petition",
        jurisdiction: "NY",
        title: "Name Change and/or Sex Designation Change Petition for Individual Adult (18+)",
        origin_url: "https://www.nycourts.gov/forms/Name-SexDesignationChange/AdultName-SexDesignationChangePetition-fillable.pdf",
        object_path: "forms/united_states/new_york/new_york_supreme_court/ny__nc1_adult_name_change_petition.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/new_york/new_york_supreme_court/ny__nc1_adult_name_change_petition.sha256"
        ),
        filing_fee: Some(FilingFee {
            amount: "210.00",
            currency: "USD",
        }),
        fee_waiver_code: None,
    },
    FormMeta {
        code: "ny__nc3_lip_name_change_petition",
        jurisdiction: "NY",
        title: "Name Change and/or Sex Designation Change Petition for a Legally Incapacitated Person",
        origin_url: "https://www.nycourts.gov/forms/Name-SexDesignationChange/LIPName-SexDesignationChangePetition-fillable.pdf",
        object_path: "forms/united_states/new_york/new_york_supreme_court/ny__nc3_lip_name_change_petition.pdf",
        sha256_pin: include_str!(
            "../../templates/notations/forms/united_states/new_york/new_york_supreme_court/ny__nc3_lip_name_change_petition.sha256"
        ),
        filing_fee: Some(FilingFee {
            amount: "210.00",
            currency: "USD",
        }),
        fee_waiver_code: None,
    },
];

/// Return the bundled form registry.
///
/// # Errors
///
/// This currently cannot fail; the `Result` keeps the public seam stable
/// for callers that already propagate registry errors.
pub fn registry() -> Result<Vec<FormMeta>, FormsError> {
    Ok(BUNDLED.to_vec())
}

/// Look up one vendored form by its stable `code`.
///
/// # Errors
///
/// Propagates [`registry`] errors; `Ok(None)` when the code is unknown.
pub fn get(code: &str) -> Result<Option<FormMeta>, FormsError> {
    Ok(registry()?.into_iter().find(|f| f.code == code))
}

#[cfg(test)]
mod tests {
    use super::{fill_values, get, registry, sha256_hex, verify_sha256};

    #[test]
    fn fill_values_is_none_for_a_form_with_no_manifest() {
        let answers = std::collections::BTreeMap::new();
        assert!(fill_values("not_a_vendored_form", &answers)
            .expect("no error")
            .is_none());
    }

    #[test]
    fn registry_pins_every_blank() {
        let forms = registry().expect("registry loads");
        assert!(!forms.is_empty(), "registry is empty");
        for form in &forms {
            let pin = form.pinned_sha256();
            assert_eq!(pin.len(), 64, "{}: pin is not a sha-256 digest", form.code);
            assert!(
                pin.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{}: pin is not lowercase hex",
                form.code
            );
            assert!(std::path::Path::new(form.object_path)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf")));
            assert!(form.object_path.starts_with("forms/united_states/"));
        }
    }

    #[test]
    fn get_finds_known_and_misses_unknown() {
        assert!(get("nv__llc_formation").expect("registry loads").is_some());
        assert!(get("nv__annual_list").expect("registry loads").is_none());
    }

    #[test]
    fn sha_verification_accepts_the_pinned_bytes_and_rejects_others() {
        let bytes = b"%PDF-1.5 canonical blank";
        let pin = sha256_hex(bytes);
        verify_sha256(&pin, bytes).expect("matching bytes verify");
        // A trailing newline in the pin file is tolerated.
        verify_sha256(&format!("{pin}\n"), bytes).expect("pin file newline is trimmed");
        let err = verify_sha256(&pin, b"%PDF-1.5 re-vendored blank").unwrap_err();
        assert_eq!(err.pinned, pin);
        assert_ne!(err.actual, err.pinned);
    }
}
