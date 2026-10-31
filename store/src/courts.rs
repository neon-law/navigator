//! The court-form registry — reference data for court self-help forms and
//! the offices that accept them.
//!
//! `store/seeds/Court.yaml` models two things: the courts (name, jurisdiction
//! code, county/city, level) and, nested under each court, the per-form
//! entries keyed to forms-shelf template codes (official form number, title,
//! source URL, sha-256 of the official blank, filing office, filing fee, and
//! the fee-waiver form the office accepts).
//!
//! # No table, deliberately
//!
//! Unlike the other canonical seeds this file is not written to a database:
//! it is compile-time reference data, served by the public catalog and
//! reconciled by the cross-crate guard test (`cli/tests/court_registry.rs`),
//! because fee data must not become part of every environment's seed state.
//! `store/tests/seed_boundary.rs` still classifies the file in the canonical
//! layer, and [`parse`] is strict — `deny_unknown_fields` on every record —
//! so a schema drift fails tests rather than a catalog page later.
//!
//! # The join contract
//!
//! A form entry's `code` is a template code on the forms shelf
//! (`templates/notations/forms/…`): the same code the template's `form:`
//! frontmatter carries and, once the form is vendored, the `forms` crate's
//! `FormMeta.code`. The guard test pins all three together — every entry
//! resolves to a real template, and a vendored `FormMeta`'s URL, pin, and
//! fee fields must equal the registry row's. The reverse direction keeps
//! the shelf honest too: a template under a court directory with no
//! registry entry is an orphan, and orphans fail CI.
//!
//! The blank itself is never checked in: `sha256` pins the official blank
//! where the court publishes it, exactly like the vendored `.sha256` files.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

/// The registry YAML embedded at compile time — the file this module types.
/// Public so cross-crate tests reconcile the exact bytes the binary ships.
pub const COURT_SEED_YAML: &str = include_str!("../seeds/Court.yaml");

/// One court, or the court that approves a statewide form set. A
/// county-specific court names its county; a city or township court names
/// its city too.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Court {
    /// Stable slug (`eighth_judicial_district_court`). It is also the
    /// template directory name for the court's own forms on the forms
    /// shelf — `templates/notations/forms/united_states/nevada/<key>/`.
    pub key: String,
    /// Display name (`Eighth Judicial District Court`).
    pub name: String,
    /// Jurisdiction code from `store/seeds/Jurisdiction.yaml` (`NV`, `CA`).
    pub jurisdiction: String,
    /// The county the court serves, when it is county-specific.
    pub county: Option<String>,
    /// The city, for a city or township court.
    pub city: Option<String>,
    pub level: CourtLevel,
    /// The forms this court's filing offices accept.
    pub forms: Vec<CourtForm>,
}

/// The class of court a form is filed in. Closed: a new class is an edit
/// here plus a seed row, reviewed together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CourtLevel {
    /// A court of last resort. A statewide self-help form set is registered
    /// under the court that approves it — the forms file in any court of
    /// the state, so the county rows carry only what is unique to them.
    Supreme,
    /// An intermediate appellate court.
    Appellate,
    /// A superior or supreme trial court of general jurisdiction
    /// (California, Washington, New York).
    Superior,
    /// A state judicial-district or county district court
    /// (Nevada, Washington).
    District,
    /// A justice or district court of limited civil jurisdiction.
    Justice,
    /// A city municipal court.
    Municipal,
    /// A probate court for decedents' estates (New York).
    Surrogate,
    /// A city court outside the municipal-court name (New York).
    City,
}

/// One form a court's filing office accepts, keyed to a forms-shelf
/// template code.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CourtForm {
    /// Forms-shelf template code (`nv__adult_name_change_petition`) — the
    /// join key to the template's `form:` frontmatter and, once the form is
    /// vendored, the `forms` crate's `FormMeta.code`.
    pub code: String,
    /// Human title, matching the template card.
    pub title: String,
    /// The form number the filing office publishes, when it publishes one
    /// (a packet code such as `F-6` counts).
    pub form_number: Option<String>,
    /// Official page where the blank is published.
    pub source_url: String,
    /// Lowercase sha-256 hex digest of the official blank PDF — never the
    /// bytes themselves. A court blank is not checked in, here or anywhere
    /// else in the tree; once the form is vendored this is the pin the
    /// `forms` registry compiles in.
    pub sha256: String,
    /// The office within the court that accepts the filing
    /// (`Family Division Clerk's Office`).
    pub filing_office: String,
    /// The filing fee, when the office publishes a flat one.
    pub filing_fee: Option<FilingFee>,
    /// Where the published schedule is tiered or varies by case value, the
    /// prose the catalog shows instead of a flat amount.
    pub fee_note: Option<String>,
    /// Template code of the fee-waiver form this office accepts.
    pub fee_waiver_code: Option<String>,
}

/// A filing fee as the filing office publishes it: a decimal amount and an
/// ISO 4217 currency code.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilingFee {
    /// Decimal amount (`"270.00"`, `"0.00"`).
    pub amount: String,
    /// ISO 4217 alphabetic currency code (`USD`).
    pub currency: String,
}

/// Errors reading or validating the registry.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RegistryError {
    #[error("parse court registry: {0}")]
    Parse(#[from] serde_yaml::Error),
    #[error("court registry lookup_fields must be exactly [key], got {0:?}")]
    UnexpectedLookupFields(Vec<String>),
    #[error("court key `{0}` is registered twice")]
    DuplicateCourt(String),
    #[error("form code `{0}` is registered twice (under {1} and {2})")]
    DuplicateFormCode(String, String, String),
    #[error("court `{0}`: jurisdiction code `{1}` is not seeded in store/seeds/Jurisdiction.yaml")]
    UnknownJurisdiction(String, String),
    #[error("court `{0}` form `{1}`: source_url must start with https://")]
    InsecureSourceUrl(String, String),
    #[error("court `{0}` form `{1}`: sha256 must be 64 lowercase hex characters")]
    BadSha256(String, String),
    #[error("court `{0}` form `{1}`: filing fee amount `{2}` is not a decimal amount")]
    BadFeeAmount(String, String, String),
    #[error(
        "court `{0}` form `{1}`: filing fee currency `{2}` is not a three-letter ISO 4217 code"
    )]
    BadFeeCurrency(String, String, String),
    #[error("court `{0}` form `{1}`: fee_waiver_code names the form itself")]
    SelfReferentialWaiver(String, String),
    #[error("court `{0}` form `{1}`: field `{2}` must not be blank")]
    BlankField(String, String, &'static str),
}

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    lookup_fields: Vec<String>,
    records: Vec<T>,
}

/// Parse and validate a registry document — the full seed-shaped envelope,
/// strict about unknown fields, duplicate keys, and unseeded jurisdictions.
///
/// # Errors
///
/// [`RegistryError`] naming the court or form the document goes wrong on.
pub fn parse(yaml: &str) -> Result<Vec<Court>, RegistryError> {
    let envelope: Envelope<Court> = serde_yaml::from_str(yaml)?;
    if envelope.lookup_fields != vec!["key"] {
        return Err(RegistryError::UnexpectedLookupFields(
            envelope.lookup_fields,
        ));
    }
    validate(&envelope.records)?;
    Ok(envelope.records)
}

/// The embedded registry: parse and validate [`COURT_SEED_YAML`].
///
/// # Errors
///
/// [`RegistryError`] when the shipped file fails its own schema.
pub fn embedded() -> Result<Vec<Court>, RegistryError> {
    parse(COURT_SEED_YAML)
}

/// The jurisdiction codes `store/seeds/Jurisdiction.yaml` seeds.
fn seeded_jurisdiction_codes() -> BTreeSet<String> {
    #[derive(Deserialize)]
    struct Seed {
        records: Vec<Record>,
    }
    #[derive(Deserialize)]
    struct Record {
        code: String,
    }
    serde_yaml::from_str::<Seed>(crate::seed::JURISDICTION_SEED_YAML)
        .expect("Jurisdiction.yaml parses against its seed shape")
        .records
        .into_iter()
        .map(|record| record.code)
        .collect()
}

fn validate(courts: &[Court]) -> Result<(), RegistryError> {
    let jurisdiction_codes = seeded_jurisdiction_codes();
    let mut court_keys = BTreeSet::new();
    let mut form_courts: BTreeMap<&str, &str> = BTreeMap::new();
    for court in courts {
        if !court_keys.insert(court.key.as_str()) {
            return Err(RegistryError::DuplicateCourt(court.key.clone()));
        }
        if !jurisdiction_codes.contains(court.jurisdiction.as_str()) {
            return Err(RegistryError::UnknownJurisdiction(
                court.key.clone(),
                court.jurisdiction.clone(),
            ));
        }
        for form in &court.forms {
            if let Some(previous) = form_courts.get(form.code.as_str()) {
                return Err(RegistryError::DuplicateFormCode(
                    form.code.clone(),
                    (*previous).to_string(),
                    court.key.clone(),
                ));
            }
            form_courts.insert(&form.code, &court.key);
            validate_form(court, form)?;
        }
    }
    Ok(())
}

fn validate_form(court: &Court, form: &CourtForm) -> Result<(), RegistryError> {
    let court_key = court.key.as_str();
    let code = form.code.as_str();
    if form.title.trim().is_empty() {
        return Err(RegistryError::BlankField(
            court_key.to_string(),
            code.to_string(),
            "title",
        ));
    }
    if form.filing_office.trim().is_empty() {
        return Err(RegistryError::BlankField(
            court_key.to_string(),
            code.to_string(),
            "filing_office",
        ));
    }
    if !form.source_url.starts_with("https://") {
        return Err(RegistryError::InsecureSourceUrl(
            court_key.to_string(),
            code.to_string(),
        ));
    }
    if !is_sha256_pin(&form.sha256) {
        return Err(RegistryError::BadSha256(
            court_key.to_string(),
            code.to_string(),
        ));
    }
    if let Some(fee) = &form.filing_fee {
        if !is_iso_4217(&fee.currency) {
            return Err(RegistryError::BadFeeCurrency(
                court_key.to_string(),
                code.to_string(),
                fee.currency.clone(),
            ));
        }
        if !is_decimal_amount(&fee.amount) {
            return Err(RegistryError::BadFeeAmount(
                court_key.to_string(),
                code.to_string(),
                fee.amount.clone(),
            ));
        }
    }
    if form.fee_waiver_code.as_deref() == Some(code) {
        return Err(RegistryError::SelfReferentialWaiver(
            court_key.to_string(),
            code.to_string(),
        ));
    }
    Ok(())
}

/// The encoding the `.sha256` pin files carry: 64 lowercase hex characters.
fn is_sha256_pin(value: &str) -> bool {
    value.len() == 64
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f'))
}

fn is_iso_4217(currency: &str) -> bool {
    currency.len() == 3 && currency.chars().all(|c| c.is_ascii_uppercase())
}

/// A decimal amount: digits with at most one fractional separator, neither
/// side empty. Money is never a float; the string is the display form.
fn is_decimal_amount(amount: &str) -> bool {
    let (whole, fraction) = match amount.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (amount, None),
    };
    let whole = !whole.is_empty() && whole.chars().all(|c| c.is_ascii_digit());
    let fraction = fraction.is_none_or(|f| !f.is_empty() && f.chars().all(|c| c.is_ascii_digit()));
    whole && fraction
}

#[cfg(test)]
mod tests {
    use super::{embedded, is_decimal_amount, is_sha256_pin, parse, RegistryError};

    /// Wrap one or more full court records (already YAML-indented) in the
    /// seed envelope, so each fixture reads as the document it parses.
    fn document(records: &str) -> String {
        format!("lookup_fields:\n  - key\nrecords:\n{records}")
    }

    const VALID_SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn adult_name_change(code: &str) -> String {
        format!(
            "      - code: {code}
        title: Petition for Adult Name Change
        source_url: https://selfhelp.nvcourts.gov/images/name-change/name-adult-petition-pdf-fillable.pdf
        sha256: {VALID_SHA256}
        filing_office: Family Division Clerk's Office"
        )
    }

    #[test]
    fn embedded_registry_parses_and_validates() {
        let courts = embedded().expect("shipped court registry must parse and validate");
        assert!(!courts.is_empty(), "registry lists no courts");
        for court in &courts {
            assert!(!court.key.is_empty());
            assert!(!court.name.is_empty());
        }
    }

    #[test]
    fn a_valid_form_entry_round_trips() {
        let document = document(&format!(
            "  - key: eighth_judicial_district_court
    name: Eighth Judicial District Court
    jurisdiction: NV
    county: Clark
    level: district
    forms:
      - code: nv__adult_name_change_petition
        title: Petition for Adult Name Change
        form_number: null
        source_url: https://selfhelp.nvcourts.gov/images/name-change/name-adult-petition-pdf-fillable.pdf
        sha256: {VALID_SHA256}
        filing_office: Family Division Clerk's Office
        filing_fee:
          amount: \"270.00\"
          currency: USD
        fee_note: null
        fee_waiver_code: nv__fee_waiver_application"
        ));
        let courts = parse(&document).expect("a fully specified form entry parses");
        let form = &courts[0].forms[0];
        assert_eq!(form.filing_fee.as_ref().expect("fee").currency, "USD");
        assert_eq!(
            form.fee_waiver_code.as_deref(),
            Some("nv__fee_waiver_application")
        );
    }

    #[test]
    fn duplicate_court_keys_are_refused() {
        let court = "  - key: nevada_supreme_court
    name: Nevada Supreme Court
    jurisdiction: NV
    level: supreme
    forms: []";
        let err = parse(&document(&format!("{court}\n{court}")))
            .expect_err("a duplicate court key must fail");
        assert!(matches!(err, RegistryError::DuplicateCourt(key) if key == "nevada_supreme_court"));
    }

    #[test]
    fn duplicate_form_codes_across_courts_are_refused() {
        let document = document(&format!(
            "  - key: nevada_supreme_court
    name: Nevada Supreme Court
    jurisdiction: NV
    level: supreme
    forms:
{}
  - key: eighth_judicial_district_court
    name: Eighth Judicial District Court
    jurisdiction: NV
    level: district
    forms:
{}",
            adult_name_change("nv__fee_waiver_application"),
            adult_name_change("nv__fee_waiver_application"),
        ));
        let err = parse(&document).expect_err("the same code under two courts must fail");
        assert!(matches!(err, RegistryError::DuplicateFormCode(..)));
    }

    #[test]
    fn unseeded_jurisdiction_codes_are_refused() {
        let document = document(
            "  - key: ontario_court_of_justice
    name: Ontario Court of Justice
    jurisdiction: ON
    level: justice
    forms: []",
        );
        let err = parse(&document).expect_err("an unseeded jurisdiction code must fail");
        assert!(
            matches!(err, RegistryError::UnknownJurisdiction(_, code) if code == "ON"),
            "a jurisdiction outside store/seeds/Jurisdiction.yaml must be refused"
        );
    }

    #[test]
    fn insecure_source_urls_and_bad_pins_are_refused() {
        let record = |url: &str, sha: &str| {
            document(&format!(
                "  - key: eighth_judicial_district_court
    name: Eighth Judicial District Court
    jurisdiction: NV
    level: district
    forms:
      - code: nv__adult_name_change_petition
        title: Petition for Adult Name Change
        source_url: {url}
        sha256: {sha}
        filing_office: Family Division Clerk's Office"
            ))
        };
        let err = parse(&record("http://selfhelp.nvcourts.gov/x.pdf", VALID_SHA256))
            .expect_err("a non-https source_url must fail");
        assert!(matches!(err, RegistryError::InsecureSourceUrl(..)));
        let err = parse(&record("https://selfhelp.nvcourts.gov/x.pdf", "DEADBEEF"))
            .expect_err("a non-hex sha256 must fail");
        assert!(matches!(err, RegistryError::BadSha256(..)));
    }

    #[test]
    fn malformed_fees_are_refused() {
        let record = |amount: &str, currency: &str| {
            document(&format!(
                "  - key: eighth_judicial_district_court
    name: Eighth Judicial District Court
    jurisdiction: NV
    level: district
    forms:
      - code: nv__adult_name_change_petition
        title: Petition for Adult Name Change
        source_url: https://selfhelp.nvcourts.gov/x.pdf
        sha256: {VALID_SHA256}
        filing_office: Family Division Clerk's Office
        filing_fee:
          amount: \"{amount}\"
          currency: {currency}"
            ))
        };
        let err = parse(&record("270", "dollars")).expect_err("a non-ISO currency must fail");
        assert!(matches!(err, RegistryError::BadFeeCurrency(..)));
        let err =
            parse(&record("twenty-seven", "USD")).expect_err("a non-decimal amount must fail");
        assert!(matches!(err, RegistryError::BadFeeAmount(..)));
    }

    #[test]
    fn a_self_referential_fee_waiver_is_refused() {
        let document = document(&format!(
            "  - key: eighth_judicial_district_court
    name: Eighth Judicial District Court
    jurisdiction: NV
    level: district
    forms:
      - code: nv__fee_waiver_application
        title: Application to Waive Filing Fee
        source_url: https://selfhelp.nvcourts.gov/x.pdf
        sha256: {VALID_SHA256}
        filing_office: Clerk's Office
        fee_waiver_code: nv__fee_waiver_application"
        ));
        let err = parse(&document).expect_err("a form cannot waive its own fee");
        assert!(matches!(err, RegistryError::SelfReferentialWaiver(..)));
    }

    #[test]
    fn pin_and_amount_shapes() {
        assert!(is_sha256_pin(VALID_SHA256));
        assert!(!is_sha256_pin("DEADBEEF"));
        assert!(!is_sha256_pin(&VALID_SHA256.to_ascii_uppercase()));
        for amount in ["270.00", "0", "0.00"] {
            assert!(is_decimal_amount(amount), "{amount} is a decimal amount");
        }
        for amount in ["", ".50", "270.", "twenty", "-4"] {
            assert!(
                !is_decimal_amount(amount),
                "{amount} is not a decimal amount"
            );
        }
    }
}
