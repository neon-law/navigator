//! Fill round-trip through the storage seam — the pipeline a notation
//! runs in production, sourced the way production sources it.
//!
//! The canonical blanks live only in the public assets bucket, so these
//! tests stage a synthetic blank per vendored form (built from its own
//! field-layer mirror, so every mapped field shape exists) in a
//! provider-neutral store, then run the full production pipeline
//! against the `cloud::StorageService` seam: pull → verify the sha-256
//! pin → resolve the map → `pdf::fill_acroform` → `pdf::flatten`.
//! Whether the *bucket's* bytes are the
//! pinned canonical blanks is `navigator forms sync`'s verify half —
//! network truth, checked at vendor time, not here.

use std::collections::BTreeMap;

use cloud::StorageService;

async fn test_storage() -> (cloud::FsStorage, tempfile::TempDir) {
    let root = tempfile::TempDir::new().expect("temporary storage root");
    let storage = cloud::FsStorage::new(root.path())
        .await
        .expect("filesystem storage");
    (storage, root)
}

/// A synthetic, genuinely fillable blank for `form_code`. A re-authored
/// form gets one widget per `.fields` manifest name — a radio group
/// (options from the sibling notation's `choices:`) for
/// `custom_single_choice__*` names, a text field for everything else,
/// `unmapped__*` included (those fields exist on the real blank too;
/// the resolver just never fills them).
fn synthetic_blank(form_code: &str, object_path: &str) -> Vec<u8> {
    let manifest = forms::manifest(form_code).expect("form has a manifest");
    let choices = template_choices(object_path);
    let specs: Vec<pdf::FieldSpec> = manifest
        .iter()
        .map(|name| {
            let role = name.strip_prefix("custom_single_choice__");
            match role.and_then(|r| choices.get(r)) {
                Some(options) => pdf::FieldSpec::Radio {
                    name: (*name).to_string(),
                    options: options.clone(),
                },
                None => pdf::FieldSpec::Text {
                    name: (*name).to_string(),
                },
            }
        })
        .collect();
    pdf::blank_acroform_with(&specs)
}

/// The sibling notation's `choices:` options — the on-state
/// vocabulary a re-authored radio group carries, keyed by the custom
/// question's `__<key>` and read from its top-level `choices`.
fn template_choices(object_path: &str) -> BTreeMap<String, Vec<String>> {
    #[derive(serde::Deserialize)]
    struct Fm {
        #[serde(default)]
        choices: BTreeMap<String, BTreeMap<String, String>>,
    }

    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("templates")
        .join("notations")
        .join(object_path.replace(".pdf", ".md"));
    let contents = std::fs::read_to_string(&path).expect("sibling notation");
    let fm = contents
        .strip_prefix("---\n")
        .and_then(|rest| rest.find("\n---").map(|end| &rest[..end]))
        .expect("frontmatter");
    let fm: Fm = serde_yaml::from_str(fm).expect("frontmatter parses");
    fm.choices
        .into_iter()
        .filter(|(_, q)| !q.is_empty())
        .map(|(role, q)| (role, q.into_keys().collect()))
        .collect()
}

fn two_people() -> String {
    r#"[
        {"name": "Aries Client", "street": "1 Main St", "city": "Las Vegas",
         "state": "NV", "zip": "89101", "country": "USA", "title": "President"},
        {"name": "Libra Partner", "street": "2 Side St", "city": "Reno",
         "state": "NV", "zip": "89501", "country": "USA", "title": "Secretary"}
    ]"#
    .to_string()
}

fn sample_answers(form_code: &str) -> BTreeMap<String, String> {
    let pairs: Vec<(&str, String)> = match form_code {
        // Re-authored: the keys are the canonical state paths the /T
        // names carry.
        "nv__llc_formation" => vec![
            ("entity__company.name", "Neon Demo LLC".to_string()),
            (
                "person__registered_agent.name",
                "Neon Law Services".to_string(),
            ),
            (
                "custom_single_choice__management_structure",
                "members".to_string(),
            ),
            ("people__managing_members", two_people()),
        ],
        "nv__profit_corp_formation" => vec![
            ("entity__company.name", "Neon Demo Corp".to_string()),
            (
                "person__registered_agent.name",
                "Neon Law Services".to_string(),
            ),
            ("custom_text__shares_authorized", "1000".to_string()),
            ("custom_text__par_value", "0.01".to_string()),
            ("people__directors", two_people()),
            ("people__corporate_officers", two_people()),
        ],
        "nv__business_trust_formation" => vec![
            ("entity__company.name", "Neon Demo Trust".to_string()),
            (
                "person__registered_agent.name",
                "Neon Law Services".to_string(),
            ),
            ("people__trustees", two_people()),
        ],
        "us__naturalization" => vec![
            ("custom_datetime__date_of_birth", "1990-04-12".to_string()),
            ("country__of_birth.name", "Mexico".to_string()),
            ("country__of_citizenship.name", "Mexico".to_string()),
            ("custom_datetime__lpr_since", "2019-03-01".to_string()),
            ("custom_phone__daytime_phone", "702-555-0100".to_string()),
            (
                "custom_single_choice__eligibility_basis",
                "five_year".to_string(),
            ),
            (
                "custom_single_choice__marital_status",
                "married".to_string(),
            ),
            ("person__client.email", "maria@example.com".to_string()),
            // Structured legal name (#311) — accented parts also exercise
            // the WinAnsi overlay encoding on the flatten path.
            ("person__client.family", "Santos Gómez".to_string()),
            ("person__client.given", "María".to_string()),
            ("person__client.middle", "Elena".to_string()),
        ],
        other => court_form_answers(other),
    };
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// First-wave court-form packets share answer fixtures per packet family:
/// every form in a packet fills from the same petition, so one family of
/// facts covers the packet (the minimization the questionnaires promise).
fn court_form_answers(form_code: &str) -> Vec<(&'static str, String)> {
    match form_code {
        "ca__name_change_petition"
        | "ca__name_change_attachment"
        | "ca__name_change_order_to_show_cause"
        | "ca__name_change_decree"
        | "ca__civil_case_cover_sheet" => ca_name_change_answers(form_code),
        "ca__gender_identity_name_change_petition"
        | "ca__gender_identity_name_change_order_to_show_cause"
        | "ca__gender_identity_name_change_decree"
        | "ca__gender_recognition_petition"
        | "ca__gender_recognition_order" => ca_gender_marker_answers(form_code),
        "nv__adult_name_change_petition" | "nv__dv_protection_order_application" => {
            nv_answers(form_code)
        }
        "nv__las_vegas_small_claims_complaint"
        | "nv__las_vegas_small_claims_counterclaim"
        | "nv__washoe_adult_name_change_petition"
        | "nv__washoe_fee_waiver_application"
        | "ny__nc1_adult_name_change_petition"
        | "ny__nc2_minor_name_change_petition"
        | "ny__nc3_lip_name_change_petition"
        | "ny__nc4_family_name_change_petition"
        | "ny__nc5_family_name_change_supplement"
        | "ny__civ_sc50_statement_of_claim"
        | "wa__adult_name_change_petition"
        | "wa__minor_name_change_petition"
        | "wa__small_claims_notice"
        | "wa__small_claims_plaintiff_packet" => wave_two_answers(form_code),
        other => panic!("no sample answers for `{other}`"),
    }
}

/// California name-change packet (NC-100 family): the petition, its
/// attachment, the order to show cause, and the decree share answers.
fn ca_name_change_answers(form_code: &str) -> Vec<(&'static str, String)> {
    match form_code {
        "ca__name_change_petition"
        | "ca__name_change_order_to_show_cause"
        | "ca__name_change_decree" => vec![
            ("person__client.name", "Jordan Kim Blake".to_string()),
            ("person__new_name.name", "Jordan Kim Rivera".to_string()),
            ("custom_text__county_of_residence", "Sacramento".to_string()),
        ],
        "ca__name_change_attachment" => vec![
            ("person__client.name", "Jordan Kim Blake".to_string()),
            ("person__new_name.name", "Jordan Kim Rivera".to_string()),
            (
                "custom_single_choice__current_sex_designation",
                "female".to_string(),
            ),
            ("custom_datetime__date_of_birth", "1990-04-12".to_string()),
            (
                "custom_text__place_of_birth",
                "Fresno, California".to_string(),
            ),
            (
                "custom_text__reason_for_change",
                "The new name reflects the petitioner's family ties.".to_string(),
            ),
        ],
        "ca__civil_case_cover_sheet" => vec![
            ("person__client.name", "Jordan Kim Blake".to_string()),
            ("custom_text__county_of_residence", "Sacramento".to_string()),
        ],
        other => panic!("no sample answers for `{other}`"),
    }
}

/// California gender-identity family: the NC-200 packet (petition, order
/// to show cause, decree) and the recognition petition NC-300 with its
/// order NC-330.
fn ca_gender_marker_answers(form_code: &str) -> Vec<(&'static str, String)> {
    match form_code {
        "ca__gender_identity_name_change_petition" => vec![
            ("person__client.name", "Casey Finley Harper".to_string()),
            ("person__new_name.name", "Casey Finley Santos".to_string()),
            (
                "custom_single_choice__current_sex_designation",
                "female".to_string(),
            ),
            ("custom_datetime__date_of_birth", "1988-09-30".to_string()),
            (
                "custom_text__place_of_birth",
                "San Diego, California".to_string(),
            ),
            (
                "custom_text__reason_for_change",
                "The new name conforms to the petitioner's gender identity.".to_string(),
            ),
        ],
        "ca__gender_identity_name_change_order_to_show_cause"
        | "ca__gender_identity_name_change_decree" => vec![
            ("person__client.name", "Casey Finley Harper".to_string()),
            ("person__new_name.name", "Casey Finley Santos".to_string()),
            ("custom_text__county_of_residence", "Fresno".to_string()),
        ],
        "ca__gender_recognition_petition" => vec![
            ("person__client.name", "Casey Finley Harper".to_string()),
            (
                "custom_single_choice__sex_identifier",
                "nonbinary".to_string(),
            ),
            (
                "custom_single_choice__new_certificates",
                "birth_certificate".to_string(),
            ),
            ("person__new_name.name", "Casey Finley Santos".to_string()),
            ("custom_datetime__date_of_birth", "1988-09-30".to_string()),
            (
                "custom_text__place_of_birth",
                "San Diego, California".to_string(),
            ),
            (
                "custom_single_choice__current_sex_designation",
                "male".to_string(),
            ),
        ],
        "ca__gender_recognition_order" => vec![
            ("person__client.name", "Casey Finley Harper".to_string()),
            ("custom_single_choice__sex_identifier", "male".to_string()),
            ("person__new_name.name", "Casey Finley Santos".to_string()),
            (
                "custom_single_choice__new_certificates",
                "birth_certificate".to_string(),
            ),
        ],
        other => panic!("no sample answers for `{other}`"),
    }
}

/// Nevada Supreme Court self-help packets: the statewide adult name-change
/// petition and the domestic-violence protection-order application.
fn nv_answers(form_code: &str) -> Vec<(&'static str, String)> {
    match form_code {
        "nv__adult_name_change_petition" => vec![
            ("person__client.name", "Alex Kim Marlowe".to_string()),
            ("person__client.given", "Alex".to_string()),
            ("person__client.middle", "Kim".to_string()),
            ("person__client.family", "Marlowe".to_string()),
            // The new legal name is a person answer: the map fills its
            // given/middle/family parts into the SEQ001 slots.
            ("person__new_name.given", "Alex".to_string()),
            ("person__new_name.middle", "Kim".to_string()),
            ("person__new_name.family", "Quill".to_string()),
            ("custom_datetime__date_of_birth", "1992-02-17".to_string()),
            ("custom_text__birthplace_city", "Elko".to_string()),
            ("jurisdiction__birthplace.name", "Nevada".to_string()),
            ("custom_text__county_of_residence", "Elko".to_string()),
            (
                "custom_text__reason_for_change",
                "The petitioner is changing their surname after marriage.".to_string(),
            ),
            ("custom_yes_no__felony_conviction", "no".to_string()),
        ],
        "nv__dv_protection_order_application" => vec![
            ("person__client.name", "Cora Blair Stubbs".to_string()),
            ("person__client.given", "Cora".to_string()),
            ("person__client.middle", "Blair".to_string()),
            ("person__client.family", "Stubbs".to_string()),
            ("person__respondent.name", "Odile Quill Prine".to_string()),
            ("person__respondent.given", "Odile".to_string()),
            ("person__respondent.middle", "Quill".to_string()),
            ("person__respondent.family", "Prine".to_string()),
            ("custom_single_choice__relationship", "dating".to_string()),
            ("custom_yes_no__living_together", "yes".to_string()),
            ("custom_text__incident_date", "October 12, 2026".to_string()),
            (
                "custom_text__what_happened",
                "The adverse party pushed the applicant and broke their phone.".to_string(),
            ),
            ("custom_yes_no__weapon_involved", "no".to_string()),
            ("custom_yes_no__incident_reported", "no".to_string()),
            (
                "custom_yes_no__emergency_residence_stay_away",
                "yes".to_string(),
            ),
            ("custom_text__county_of_residence", "Washoe".to_string()),
        ],
        other => panic!("no sample answers for `{other}`"),
    }
}

/// One provider-neutral store, every vendored form, the whole pipeline: stage the
/// blank, pull it back through the trait, verify its pin, resolve the
/// map, fill, read every resolved value back, then flatten and confirm
/// nothing interactive survives while the filled text does.
#[tokio::test]
async fn every_packet_round_trips_from_the_storage_seam() {
    let (storage, _root) = test_storage().await;
    let storage: &dyn StorageService = &storage;

    for form in forms::registry().expect("registry loads") {
        let staged = synthetic_blank(form.code, form.object_path);
        storage
            .put(form.object_path, &staged, "application/pdf")
            .await
            .expect("stage blank in the bucket");
        let pin = forms::sha256_hex(&staged);

        // Pull through the seam and verify before filling — the exact
        // gate `portal::retainer_walk::acroform_payload` runs.
        let blank = storage.get(form.object_path).await.expect("pull blank");
        forms::verify_sha256(&pin, &blank.bytes).expect("staged bytes verify against their pin");

        // The exact resolution entry `portal::retainer_walk::acroform_payload`
        // calls — map-driven or manifest-driven per form.
        let resolved = forms::fill_values(form.code, &sample_answers(form.code))
            .expect("answers resolve")
            .expect("every vendored form fills");
        assert!(
            !resolved.is_empty(),
            "{}: sample answers resolved to nothing",
            form.code
        );

        let filled = pdf::fill_acroform(&blank.bytes, &resolved).expect("fill succeeds");
        let read_back = pdf::read_field_values(&filled).expect("filled packet re-parses");
        for (name, want) in &resolved {
            assert_eq!(
                read_back.get(name),
                Some(want),
                "{}: `{name}` did not round-trip",
                form.code
            );
        }

        // Flatten freezes what lawyer approved: no interactive field or
        // widget survives, and the filled text is static page content.
        let flat = pdf::flatten(&filled).expect("flatten succeeds");
        assert!(
            pdf::field_names(&flat).expect("field names").is_empty(),
            "{}: flattened packet still exposes interactive fields",
            form.code
        );
        assert_eq!(
            pdf::widget_annotation_count(&flat).expect("widget count"),
            0,
            "{}: flattened packet still carries widget annotations",
            form.code
        );
        let text = pdf::page_text(&flat).expect("extract flattened page text");
        for value in flattened_text_smoke_values(form.code) {
            assert!(
                text.contains(value),
                "{}: flattened packet lost `{value}`",
                form.code
            );
        }
    }
}

/// Wave-two court-form packets: King County (WA), Washoe and Las Vegas
/// (NV), and the New York UCS routes. Each form fills from its own
/// questionnaire, so the arms are per form rather than per family.
fn wave_two_answers(form_code: &str) -> Vec<(&'static str, String)> {
    match form_code {
        "nv__las_vegas_small_claims_complaint" => vec![
            (
                "custom_phone__defendant_contact",
                "702-555-0100".to_string(),
            ),
            (
                "custom_text__defendant_caption",
                "Sample entry for defendant_caption".to_string(),
            ),
            (
                "custom_text__plaintiff_caption",
                "Sample entry for plaintiff_caption".to_string(),
            ),
            (
                "custom_text__reason_owed",
                "Sample entry for reason_owed".to_string(),
            ),
            ("custom_usd__amount_owed", "2500".to_string()),
            ("person__client.email", "jordan@example.com".to_string()),
            ("person__client.name", "Jordan Avery Blake".to_string()),
            ("person__client.phone", "702-555-0100".to_string()),
        ],
        "nv__las_vegas_small_claims_counterclaim" => vec![
            (
                "custom_text__counterdefendant_caption",
                "Sample entry for counterdefendant_caption".to_string(),
            ),
            (
                "custom_text__counterplaintiff_caption",
                "Sample entry for counterplaintiff_caption".to_string(),
            ),
            (
                "custom_text__reason_owed",
                "Sample entry for reason_owed".to_string(),
            ),
            (
                "custom_text__reason_owed_2",
                "Sample entry for reason_owed_2".to_string(),
            ),
            (
                "custom_text__reason_owed_3",
                "Sample entry for reason_owed_3".to_string(),
            ),
            (
                "custom_text__reason_owed_4",
                "Sample entry for reason_owed_4".to_string(),
            ),
            (
                "custom_text__reason_owed_5",
                "Sample entry for reason_owed_5".to_string(),
            ),
            (
                "custom_text__reason_owed_6",
                "Sample entry for reason_owed_6".to_string(),
            ),
            ("custom_usd__amount_owed", "2500".to_string()),
            ("person__client.email", "jordan@example.com".to_string()),
            ("person__client.name", "Jordan Avery Blake".to_string()),
            ("person__client.phone", "702-555-0100".to_string()),
        ],
        "nv__washoe_adult_name_change_petition" => vec![
            (
                "custom_datetime__county_residency_since",
                "1990-04-12".to_string(),
            ),
            ("custom_datetime__date_of_birth", "1990-04-12".to_string()),
            ("custom_text__birthplace_city", "Reno".to_string()),
            (
                "custom_text__felony_details",
                "Sample entry for felony_details".to_string(),
            ),
            (
                "custom_text__reason_for_change",
                "The new name reflects the petitioner's family ties.".to_string(),
            ),
            ("custom_yes_no__criminal_record", "yes".to_string()),
            ("custom_yes_no__felony_conviction", "yes".to_string()),
            ("jurisdiction__birthplace.name", "Nevada".to_string()),
            ("person__client.family", "Blake".to_string()),
            ("person__client.given", "Jordan".to_string()),
            ("person__client.middle", "Avery".to_string()),
            ("person__client.name", "Jordan Avery Blake".to_string()),
            ("person__new_name.family", "Blake".to_string()),
            ("person__new_name.given", "Jordan".to_string()),
            ("person__new_name.middle", "Avery".to_string()),
        ],
        "nv__washoe_fee_waiver_application" => vec![
            (
                "custom_text__account_one",
                "Sample entry for account_one".to_string(),
            ),
            (
                "custom_text__account_two",
                "Sample entry for account_two".to_string(),
            ),
            (
                "custom_text__additional_information",
                "Sample entry for additional_information".to_string(),
            ),
            (
                "custom_text__expense_other_label",
                "Sample entry for expense_other_label".to_string(),
            ),
            (
                "custom_text__hours_per_week",
                "Sample entry for hours_per_week".to_string(),
            ),
            (
                "custom_text__household_age_1",
                "Sample entry for household_age_1".to_string(),
            ),
            (
                "custom_text__household_age_2",
                "Sample entry for household_age_2".to_string(),
            ),
            (
                "custom_text__household_age_3",
                "Sample entry for household_age_3".to_string(),
            ),
            (
                "custom_text__household_age_4",
                "Sample entry for household_age_4".to_string(),
            ),
            (
                "custom_text__household_age_5",
                "Sample entry for household_age_5".to_string(),
            ),
            (
                "custom_text__household_age_6",
                "Sample entry for household_age_6".to_string(),
            ),
            (
                "custom_text__household_age_7",
                "Sample entry for household_age_7".to_string(),
            ),
            (
                "custom_text__household_person_1",
                "Sample entry for household_person_1".to_string(),
            ),
            (
                "custom_text__household_person_2",
                "Sample entry for household_person_2".to_string(),
            ),
            (
                "custom_text__household_person_3",
                "Sample entry for household_person_3".to_string(),
            ),
            (
                "custom_text__household_person_4",
                "Sample entry for household_person_4".to_string(),
            ),
            (
                "custom_text__household_person_5",
                "Sample entry for household_person_5".to_string(),
            ),
            (
                "custom_text__household_person_6",
                "Sample entry for household_person_6".to_string(),
            ),
            (
                "custom_text__household_person_7",
                "Sample entry for household_person_7".to_string(),
            ),
            (
                "custom_text__household_relationship_1",
                "Sample entry for household_relationship_1".to_string(),
            ),
            (
                "custom_text__household_relationship_2",
                "Sample entry for household_relationship_2".to_string(),
            ),
            (
                "custom_text__household_relationship_3",
                "Sample entry for household_relationship_3".to_string(),
            ),
            (
                "custom_text__household_relationship_4",
                "Sample entry for household_relationship_4".to_string(),
            ),
            (
                "custom_text__household_relationship_5",
                "Sample entry for household_relationship_5".to_string(),
            ),
            (
                "custom_text__household_relationship_6",
                "Sample entry for household_relationship_6".to_string(),
            ),
            (
                "custom_text__household_relationship_7",
                "Sample entry for household_relationship_7".to_string(),
            ),
            (
                "custom_text__no_income_explanation",
                "Sample entry for no_income_explanation".to_string(),
            ),
            (
                "custom_text__other_benefits_program",
                "Sample entry for other_benefits_program".to_string(),
            ),
            (
                "custom_text__real_estate_address",
                "Sample entry for real_estate_address".to_string(),
            ),
            (
                "custom_text__vehicle_one",
                "Sample entry for vehicle_one".to_string(),
            ),
            (
                "custom_text__vehicle_two",
                "Sample entry for vehicle_two".to_string(),
            ),
            ("custom_usd__account_one_owed", "2500".to_string()),
            ("custom_usd__account_one_worth", "2500".to_string()),
            ("custom_usd__account_two_owed", "2500".to_string()),
            ("custom_usd__account_two_worth", "2500".to_string()),
            ("custom_usd__cash", "2500".to_string()),
            ("custom_usd__commissions_monthly", "2500".to_string()),
            ("custom_usd__expense_childcare", "2500".to_string()),
            ("custom_usd__expense_food", "2500".to_string()),
            ("custom_usd__expense_insurance", "2500".to_string()),
            ("custom_usd__expense_medical", "2500".to_string()),
            ("custom_usd__expense_other", "2500".to_string()),
            ("custom_usd__expense_rent", "2500".to_string()),
            ("custom_usd__expense_transportation", "2500".to_string()),
            ("custom_usd__expense_utilities", "2500".to_string()),
            ("custom_usd__expenses_total", "2500".to_string()),
            ("custom_usd__hourly_wage", "2500".to_string()),
            ("custom_usd__household_contribution_1", "2500".to_string()),
            ("custom_usd__household_contribution_2", "2500".to_string()),
            ("custom_usd__household_contribution_3", "2500".to_string()),
            ("custom_usd__household_contribution_4", "2500".to_string()),
            ("custom_usd__household_contribution_5", "2500".to_string()),
            ("custom_usd__household_contribution_6", "2500".to_string()),
            ("custom_usd__household_contribution_7", "2500".to_string()),
            ("custom_usd__other_income_monthly", "2500".to_string()),
            ("custom_usd__pension_monthly", "2500".to_string()),
            ("custom_usd__real_estate_owed", "2500".to_string()),
            ("custom_usd__real_estate_worth", "2500".to_string()),
            ("custom_usd__salary_per_month", "2500".to_string()),
            ("custom_usd__salary_per_year", "2500".to_string()),
            ("custom_usd__support_monthly", "2500".to_string()),
            ("custom_usd__unemployment_weekly", "2500".to_string()),
            ("custom_usd__vehicle_one_owed", "2500".to_string()),
            ("custom_usd__vehicle_one_worth", "2500".to_string()),
            ("custom_usd__vehicle_two_owed", "2500".to_string()),
            ("custom_usd__vehicle_two_worth", "2500".to_string()),
            ("custom_usd__veterans_or_ss_monthly", "2500".to_string()),
            ("custom_yes_no__benefit_food_stamps", "yes".to_string()),
            ("custom_yes_no__benefit_legal_services", "yes".to_string()),
            ("custom_yes_no__benefit_medicaid", "yes".to_string()),
            ("custom_yes_no__benefit_other", "yes".to_string()),
            ("custom_yes_no__benefit_ssi", "yes".to_string()),
            (
                "custom_yes_no__benefit_subsidized_housing",
                "yes".to_string(),
            ),
            ("custom_yes_no__benefit_tanf", "yes".to_string()),
            ("custom_yes_no__income_benefits", "yes".to_string()),
            ("custom_yes_no__income_commissions", "yes".to_string()),
            ("custom_yes_no__income_employment", "yes".to_string()),
            ("custom_yes_no__income_other", "yes".to_string()),
            ("custom_yes_no__income_pension", "yes".to_string()),
            ("custom_yes_no__income_salary", "yes".to_string()),
            ("custom_yes_no__income_support", "yes".to_string()),
            ("custom_yes_no__income_unemployment", "yes".to_string()),
            ("custom_yes_no__no_income", "yes".to_string()),
            ("custom_yes_no__no_real_estate", "yes".to_string()),
            ("custom_yes_no__no_vehicle", "yes".to_string()),
            ("person__client.name", "Jordan Avery Blake".to_string()),
            (
                "person__opposing_party.name",
                "Jordan Avery Blake".to_string(),
            ),
        ],
        "ny__nc1_adult_name_change_petition" => vec![
            ("custom_datetime__date_of_birth", "1990-04-12".to_string()),
            ("custom_single_choice__bankruptcy", "No".to_string()),
            ("custom_single_choice__child_support", "No".to_string()),
            (
                "custom_single_choice__child_support_current",
                "No".to_string(),
            ),
            ("custom_single_choice__children_under_21", "No".to_string()),
            ("custom_single_choice__convicted", "No".to_string()),
            ("custom_single_choice__currently_married", "No".to_string()),
            ("custom_single_choice__documents_attached", "No".to_string()),
            (
                "custom_single_choice__filing_county",
                "New York".to_string(),
            ),
            (
                "custom_single_choice__filing_court",
                "Supreme Court".to_string(),
            ),
            ("custom_single_choice__judgments_liens", "No".to_string()),
            ("custom_single_choice__name_change", "Yes".to_string()),
            ("custom_single_choice__new_sex_designation", "X".to_string()),
            ("custom_single_choice__party_to_action", "No".to_string()),
            ("custom_single_choice__previously_married", "No".to_string()),
            ("custom_single_choice__prior_name_change", "No".to_string()),
            (
                "custom_single_choice__prior_sex_designation_change",
                "No".to_string(),
            ),
            ("custom_single_choice__sealing_requested", "No".to_string()),
            (
                "custom_single_choice__sex_designation_change",
                "Yes".to_string(),
            ),
            (
                "custom_single_choice__sex_designation_reasons_given",
                "No".to_string(),
            ),
            ("custom_single_choice__spousal_support", "No".to_string()),
            (
                "custom_single_choice__spousal_support_current",
                "No".to_string(),
            ),
            (
                "custom_text__child_support_court",
                "Sample entry for child_support_court".to_string(),
            ),
            (
                "custom_text__current_address",
                "Sample entry for current_address".to_string(),
            ),
            (
                "custom_text__financial_matters_details",
                "Sample entry for financial_matters_details".to_string(),
            ),
            (
                "custom_text__place_of_birth",
                "Sample entry for place_of_birth".to_string(),
            ),
            (
                "custom_text__prior_petition_details",
                "Sample entry for prior_petition_details".to_string(),
            ),
            (
                "custom_text__prior_sex_designation_details",
                "Sample entry for prior_sex_designation_details".to_string(),
            ),
            (
                "custom_text__reason_for_change",
                "The new name reflects the petitioner's family ties.".to_string(),
            ),
            (
                "custom_text__sealing_details",
                "Sample entry for sealing_details".to_string(),
            ),
            (
                "custom_text__sex_designation_reasons",
                "Sample entry for sex_designation_reasons".to_string(),
            ),
            (
                "custom_text__spousal_support_court",
                "Sample entry for spousal_support_court".to_string(),
            ),
            (
                "custom_text__support_collections_unit",
                "Sample entry for support_collections_unit".to_string(),
            ),
            (
                "custom_text__supporting_documents_list",
                "Sample entry for supporting_documents_list".to_string(),
            ),
            ("custom_usd__child_support_arrears", "2500".to_string()),
            ("custom_usd__spousal_support_arrears", "2500".to_string()),
            ("person__client.name", "Jordan Avery Blake".to_string()),
            ("person__new_name.name", "Jordan Avery Blake".to_string()),
        ],
        "ny__nc2_minor_name_change_petition" => vec![
            ("custom_datetime__date_of_birth", "1990-04-12".to_string()),
            ("custom_single_choice__documents_attached", "No".to_string()),
            (
                "custom_single_choice__filing_county",
                "New York".to_string(),
            ),
            (
                "custom_single_choice__filing_court",
                "Supreme Court".to_string(),
            ),
            ("custom_single_choice__minor_bankruptcy", "No".to_string()),
            (
                "custom_single_choice__minor_child_support",
                "No".to_string(),
            ),
            (
                "custom_single_choice__minor_child_support_current",
                "No".to_string(),
            ),
            (
                "custom_single_choice__minor_children_under_21",
                "No".to_string(),
            ),
            ("custom_single_choice__minor_convicted", "No".to_string()),
            (
                "custom_single_choice__minor_currently_married",
                "No".to_string(),
            ),
            (
                "custom_single_choice__minor_judgments_liens",
                "No".to_string(),
            ),
            (
                "custom_single_choice__minor_new_sex_designation",
                "X".to_string(),
            ),
            (
                "custom_single_choice__minor_party_to_action",
                "No".to_string(),
            ),
            (
                "custom_single_choice__minor_previously_married",
                "No".to_string(),
            ),
            (
                "custom_single_choice__minor_prior_sex_designation",
                "No".to_string(),
            ),
            (
                "custom_single_choice__minor_sex_designation_reasons_given",
                "No".to_string(),
            ),
            (
                "custom_single_choice__minor_spousal_support",
                "No".to_string(),
            ),
            (
                "custom_single_choice__minor_spousal_support_current",
                "No".to_string(),
            ),
            ("custom_single_choice__name_change", "Yes".to_string()),
            (
                "custom_single_choice__petitioner_bankruptcy",
                "No".to_string(),
            ),
            (
                "custom_single_choice__petitioner_child_support",
                "No".to_string(),
            ),
            (
                "custom_single_choice__petitioner_child_support_current",
                "No".to_string(),
            ),
            (
                "custom_single_choice__petitioner_children_under_21",
                "No".to_string(),
            ),
            (
                "custom_single_choice__petitioner_convicted",
                "No".to_string(),
            ),
            (
                "custom_single_choice__petitioner_currently_married",
                "No".to_string(),
            ),
            (
                "custom_single_choice__petitioner_judgments_liens",
                "No".to_string(),
            ),
            (
                "custom_single_choice__petitioner_party_to_action",
                "No".to_string(),
            ),
            (
                "custom_single_choice__petitioner_previously_married",
                "No".to_string(),
            ),
            (
                "custom_single_choice__petitioner_spousal_support",
                "No".to_string(),
            ),
            (
                "custom_single_choice__petitioner_spousal_support_current",
                "No".to_string(),
            ),
            ("custom_single_choice__prior_name_change", "No".to_string()),
            (
                "custom_single_choice__relationship",
                "Parent(s)".to_string(),
            ),
            ("custom_single_choice__sealing_requested", "No".to_string()),
            (
                "custom_single_choice__sex_designation_change",
                "Yes".to_string(),
            ),
            (
                "custom_text__minor_child_support_court",
                "Sample entry for minor_child_support_court".to_string(),
            ),
            (
                "custom_text__minor_current_address",
                "Sample entry for minor_current_address".to_string(),
            ),
            (
                "custom_text__minor_financial_details",
                "Sample entry for minor_financial_details".to_string(),
            ),
            (
                "custom_text__minor_prior_sex_designation_details",
                "Sample entry for minor_prior_sex_designation_details".to_string(),
            ),
            (
                "custom_text__minor_sex_designation_reasons",
                "Sample entry for minor_sex_designation_reasons".to_string(),
            ),
            (
                "custom_text__minor_spousal_support_court",
                "Sample entry for minor_spousal_support_court".to_string(),
            ),
            (
                "custom_text__minor_support_collections_unit",
                "Sample entry for minor_support_collections_unit".to_string(),
            ),
            (
                "custom_text__other_relation",
                "Sample entry for other_relation".to_string(),
            ),
            (
                "custom_text__petitioner_address",
                "Sample entry for petitioner_address".to_string(),
            ),
            (
                "custom_text__petitioner_financial_details",
                "Sample entry for petitioner_financial_details".to_string(),
            ),
            (
                "custom_text__petitioner_spousal_support_court",
                "Sample entry for petitioner_spousal_support_court".to_string(),
            ),
            (
                "custom_text__petitioner_support_collections_unit",
                "Sample entry for petitioner_support_collections_unit".to_string(),
            ),
            (
                "custom_text__place_of_birth",
                "Sample entry for place_of_birth".to_string(),
            ),
            (
                "custom_text__prior_petition_details",
                "Sample entry for prior_petition_details".to_string(),
            ),
            (
                "custom_text__reason_for_change",
                "The new name reflects the petitioner's family ties.".to_string(),
            ),
            (
                "custom_text__sealing_details",
                "Sample entry for sealing_details".to_string(),
            ),
            (
                "custom_text__supporting_documents_list",
                "Sample entry for supporting_documents_list".to_string(),
            ),
            (
                "custom_usd__minor_child_support_arrears",
                "2500".to_string(),
            ),
            (
                "custom_usd__minor_spousal_support_arrears",
                "2500".to_string(),
            ),
            (
                "custom_usd__petitioner_child_support_arrears",
                "2500".to_string(),
            ),
            (
                "custom_usd__petitioner_spousal_support_arrears",
                "2500".to_string(),
            ),
            ("people__consenting_parents", two_people().to_string()),
            ("person__minor.name", "Jordan Avery Blake".to_string()),
            (
                "person__minor_new_name.name",
                "Jordan Avery Blake".to_string(),
            ),
            ("person__petitioner.name", "Jordan Avery Blake".to_string()),
        ],
        "ny__nc3_lip_name_change_petition" => vec![
            ("custom_datetime__date_of_birth", "1990-04-12".to_string()),
            ("custom_single_choice__bankruptcy", "No".to_string()),
            ("custom_single_choice__child_support", "No".to_string()),
            (
                "custom_single_choice__child_support_current",
                "No".to_string(),
            ),
            ("custom_single_choice__convicted", "No".to_string()),
            ("custom_single_choice__currently_married", "No".to_string()),
            ("custom_single_choice__documents_attached", "No".to_string()),
            (
                "custom_single_choice__filing_county",
                "New York".to_string(),
            ),
            (
                "custom_single_choice__filing_court",
                "Supreme Court".to_string(),
            ),
            ("custom_single_choice__judgments_liens", "No".to_string()),
            ("custom_single_choice__name_change", "Yes".to_string()),
            ("custom_single_choice__new_sex_designation", "X".to_string()),
            ("custom_single_choice__party_to_action", "No".to_string()),
            ("custom_single_choice__previously_married", "No".to_string()),
            ("custom_single_choice__prior_name_change", "No".to_string()),
            (
                "custom_single_choice__sex_designation_change",
                "Yes".to_string(),
            ),
            (
                "custom_single_choice__sex_designation_reasons_given",
                "No".to_string(),
            ),
            ("custom_single_choice__spousal_support", "No".to_string()),
            (
                "custom_single_choice__spousal_support_current",
                "No".to_string(),
            ),
            (
                "custom_text__child_support_court",
                "Sample entry for child_support_court".to_string(),
            ),
            (
                "custom_text__current_address",
                "Sample entry for current_address".to_string(),
            ),
            (
                "custom_text__financial_matters_details",
                "Sample entry for financial_matters_details".to_string(),
            ),
            (
                "custom_text__place_of_birth",
                "Sample entry for place_of_birth".to_string(),
            ),
            (
                "custom_text__prior_petition_details",
                "Sample entry for prior_petition_details".to_string(),
            ),
            (
                "custom_text__prior_sex_designation_details",
                "Sample entry for prior_sex_designation_details".to_string(),
            ),
            (
                "custom_text__reason_for_change",
                "The new name reflects the petitioner's family ties.".to_string(),
            ),
            (
                "custom_text__sex_designation_reasons",
                "Sample entry for sex_designation_reasons".to_string(),
            ),
            (
                "custom_text__spousal_support_court",
                "Sample entry for spousal_support_court".to_string(),
            ),
            (
                "custom_text__support_collections_unit",
                "Sample entry for support_collections_unit".to_string(),
            ),
            (
                "custom_text__supporting_documents_list",
                "Sample entry for supporting_documents_list".to_string(),
            ),
            ("custom_usd__child_support_arrears", "2500".to_string()),
            ("custom_usd__spousal_support_arrears", "2500".to_string()),
            ("person__client.name", "Jordan Avery Blake".to_string()),
            ("person__new_name.name", "Jordan Avery Blake".to_string()),
            ("person__petitioner.name", "Jordan Avery Blake".to_string()),
        ],
        "ny__nc4_family_name_change_petition" => vec![
            ("custom_datetime__address_since", "1990-04-12".to_string()),
            ("custom_single_choice__bankruptcy", "No".to_string()),
            ("custom_single_choice__child_support", "No".to_string()),
            ("custom_single_choice__convicted", "No".to_string()),
            (
                "custom_single_choice__filing_county",
                "New York".to_string(),
            ),
            (
                "custom_single_choice__filing_court",
                "Supreme Court".to_string(),
            ),
            ("custom_single_choice__judgments_liens", "No".to_string()),
            ("custom_single_choice__party_to_action", "No".to_string()),
            ("custom_single_choice__prior_name_change", "No".to_string()),
            ("custom_single_choice__sealing_requested", "No".to_string()),
            ("custom_single_choice__spousal_support", "No".to_string()),
            (
                "custom_text__current_address",
                "Sample entry for current_address".to_string(),
            ),
            (
                "custom_text__parent_1_address",
                "Sample entry for parent_1_address".to_string(),
            ),
            (
                "custom_text__parent_1_consent_children",
                "Sample entry for parent_1_consent_children".to_string(),
            ),
            (
                "custom_text__parent_1_consent_new_names",
                "Sample entry for parent_1_consent_new_names".to_string(),
            ),
            (
                "custom_text__parent_1_name",
                "Sample entry for parent_1_name".to_string(),
            ),
            (
                "custom_text__parent_2_address",
                "Sample entry for parent_2_address".to_string(),
            ),
            (
                "custom_text__parent_2_consent_children",
                "Sample entry for parent_2_consent_children".to_string(),
            ),
            (
                "custom_text__parent_2_consent_new_names",
                "Sample entry for parent_2_consent_new_names".to_string(),
            ),
            (
                "custom_text__parent_2_name",
                "Sample entry for parent_2_name".to_string(),
            ),
            (
                "custom_text__prior_petition_details",
                "Sample entry for prior_petition_details".to_string(),
            ),
            (
                "custom_text__reason_for_change",
                "The new name reflects the petitioner's family ties.".to_string(),
            ),
            (
                "custom_text__sealing_details",
                "Sample entry for sealing_details".to_string(),
            ),
            ("people__co_petitioners", two_people().to_string()),
            ("people__family_members", two_people().to_string()),
            ("person__client.name", "Jordan Avery Blake".to_string()),
            (
                "person__new_family_name.name",
                "Jordan Avery Blake".to_string(),
            ),
        ],
        "ny__nc5_family_name_change_supplement" => vec![
            (
                "custom_single_choice__child_support_current",
                "No".to_string(),
            ),
            ("custom_single_choice__children_under_21", "No".to_string()),
            ("custom_single_choice__currently_married", "No".to_string()),
            (
                "custom_single_choice__filing_county",
                "New York".to_string(),
            ),
            (
                "custom_single_choice__filing_court",
                "Supreme Court".to_string(),
            ),
            ("custom_single_choice__member_bankruptcy", "No".to_string()),
            (
                "custom_single_choice__member_child_support",
                "No".to_string(),
            ),
            ("custom_single_choice__member_convicted", "No".to_string()),
            (
                "custom_single_choice__member_judgments_liens",
                "No".to_string(),
            ),
            (
                "custom_single_choice__member_party_to_action",
                "No".to_string(),
            ),
            (
                "custom_single_choice__member_role",
                "The petitioner".to_string(),
            ),
            (
                "custom_single_choice__member_spousal_support",
                "No".to_string(),
            ),
            ("custom_single_choice__previously_married", "No".to_string()),
            (
                "custom_single_choice__spousal_support_current",
                "No".to_string(),
            ),
            (
                "custom_text__child_support_collection_unit",
                "Sample entry for child_support_collection_unit".to_string(),
            ),
            (
                "custom_text__child_support_order_court",
                "Sample entry for child_support_order_court".to_string(),
            ),
            (
                "custom_text__court_of_conviction",
                "Sample entry for court_of_conviction".to_string(),
            ),
            (
                "custom_text__crime_of_conviction",
                "Sample entry for crime_of_conviction".to_string(),
            ),
            (
                "custom_text__member_disclosure_details",
                "Sample entry for member_disclosure_details".to_string(),
            ),
            (
                "custom_text__spousal_support_order_court",
                "Sample entry for spousal_support_order_court".to_string(),
            ),
            ("custom_usd__child_support_arrears", "2500".to_string()),
            ("custom_usd__spousal_support_arrears", "2500".to_string()),
            ("person__client.name", "Jordan Avery Blake".to_string()),
            ("person__member.name", "Jordan Avery Blake".to_string()),
        ],
        "ny__civ_sc50_statement_of_claim" => vec![
            (
                "custom_datetime__claim_occurrence_date",
                "1990-04-12".to_string(),
            ),
            (
                "custom_phone__defendant_contact",
                "702-555-0100".to_string(),
            ),
            (
                "custom_single_choice__primary_claim_reason",
                "automobile".to_string(),
            ),
            (
                "custom_text__auto_accident_place",
                "Sample entry for auto_accident_place".to_string(),
            ),
            (
                "custom_text__claim_other_description",
                "The sample narrative for this answer.".to_string(),
            ),
            (
                "custom_text__identifying_numbers",
                "Sample entry for identifying_numbers".to_string(),
            ),
            ("custom_usd__claim_amount", "2500".to_string()),
            ("people__claimants", two_people().to_string()),
            ("people__defendants", two_people().to_string()),
            ("person__client.email", "jordan@example.com".to_string()),
            ("person__client.family", "Blake".to_string()),
            ("person__client.given", "Jordan".to_string()),
            ("person__client.middle", "Avery".to_string()),
            ("person__client.phone", "702-555-0100".to_string()),
            ("person__defendant.family", "Blake".to_string()),
            ("person__defendant.given", "Jordan".to_string()),
            ("person__defendant.middle", "Avery".to_string()),
        ],
        "wa__adult_name_change_petition" => vec![
            (
                "custom_text__reason_for_change",
                "The new name reflects the petitioner's family ties.".to_string(),
            ),
            ("custom_yes_no__doc_jurisdiction", "yes".to_string()),
            ("custom_yes_no__not_detrimental", "yes".to_string()),
            ("custom_yes_no__open_protection_order", "yes".to_string()),
            ("custom_yes_no__prior_name_change", "yes".to_string()),
            (
                "custom_yes_no__sex_offender_registration",
                "yes".to_string(),
            ),
            ("person__client.family", "Blake".to_string()),
            ("person__client.given", "Jordan".to_string()),
            ("person__client.middle", "Avery".to_string()),
            ("person__client.name", "Jordan Avery Blake".to_string()),
            ("person__new_name.family", "Blake".to_string()),
            ("person__new_name.given", "Jordan".to_string()),
            ("person__new_name.middle", "Avery".to_string()),
        ],
        "wa__minor_name_change_petition" => vec![
            ("custom_datetime__date_of_birth", "1990-04-12".to_string()),
            (
                "custom_text__best_interest_reasons",
                "Sample entry for best_interest_reasons".to_string(),
            ),
            (
                "custom_text__prior_name_change_details",
                "Sample entry for prior_name_change_details".to_string(),
            ),
            ("custom_yes_no__prior_name_change", "yes".to_string()),
            (
                "custom_yes_no__sex_offender_registration",
                "yes".to_string(),
            ),
            ("people__parents", two_people().to_string()),
            ("person__client.name", "Jordan Avery Blake".to_string()),
        ],
        "wa__small_claims_notice" => vec![
            ("custom_datetime__amount_due_date", "1990-04-12".to_string()),
            (
                "custom_datetime__auto_accident_date",
                "1990-04-12".to_string(),
            ),
            (
                "custom_text__claim_facts",
                "Sample entry for claim_facts".to_string(),
            ),
            (
                "custom_text__claim_other_description",
                "The sample narrative for this answer.".to_string(),
            ),
            ("custom_usd__claim_amount", "2500".to_string()),
            ("custom_yes_no__claim_auto_damages", "yes".to_string()),
            ("custom_yes_no__claim_faulty_workmanship", "yes".to_string()),
            ("custom_yes_no__claim_loan", "yes".to_string()),
            ("custom_yes_no__claim_merchandise", "yes".to_string()),
            ("custom_yes_no__claim_other", "yes".to_string()),
            ("custom_yes_no__claim_property_damage", "yes".to_string()),
            ("custom_yes_no__claim_rent", "yes".to_string()),
            ("custom_yes_no__claim_return_of_deposit", "yes".to_string()),
            ("custom_yes_no__claim_wages", "yes".to_string()),
            ("people__defendants", two_people().to_string()),
            ("people__plaintiffs", two_people().to_string()),
            ("person__client.email", "jordan@example.com".to_string()),
            ("person__client.name", "Jordan Avery Blake".to_string()),
            ("person__client.phone", "702-555-0100".to_string()),
        ],
        "wa__small_claims_plaintiff_packet" => vec![
            ("custom_usd__claim_amount", "2500".to_string()),
            ("custom_yes_no__claim_auto_damages", "yes".to_string()),
            ("custom_yes_no__claim_faulty_workmanship", "yes".to_string()),
            ("custom_yes_no__claim_loan", "yes".to_string()),
            ("custom_yes_no__claim_merchandise", "yes".to_string()),
            ("custom_yes_no__claim_other", "yes".to_string()),
            ("custom_yes_no__claim_return_of_deposit", "yes".to_string()),
            ("custom_yes_no__claim_wages", "yes".to_string()),
            ("people__defendants", two_people().to_string()),
            ("people__plaintiffs", two_people().to_string()),
            ("person__client.email", "jordan@example.com".to_string()),
            ("person__client.name", "Jordan Avery Blake".to_string()),
            ("person__client.phone", "702-555-0100".to_string()),
        ],
        "ny__probate_petition" => vec![
            ("custom_datetime__codicil_date", "2010-03-02".to_string()),
            ("custom_datetime__codicil_date_2", "2015-09-14".to_string()),
            (
                "custom_datetime__decedent_date_of_death",
                "2024-11-05".to_string(),
            ),
            ("custom_datetime__petition_date", "2026-10-31".to_string()),
            ("custom_datetime__will_date", "2001-06-15".to_string()),
            (
                "custom_single_choice__distributee_class_aunts_uncles",
                "No survivors in this class".to_string(),
            ),
            (
                "custom_single_choice__distributee_class_children",
                "Two".to_string(),
            ),
            (
                "custom_single_choice__distributee_class_cousins_once_removed",
                "No survivors in this class".to_string(),
            ),
            (
                "custom_single_choice__distributee_class_grandparents",
                "No survivors in this class".to_string(),
            ),
            (
                "custom_single_choice__distributee_class_parents",
                "No survivors in this class".to_string(),
            ),
            (
                "custom_single_choice__distributee_class_siblings",
                "No survivors in this class".to_string(),
            ),
            (
                "custom_single_choice__distributee_class_spouse",
                "One".to_string(),
            ),
            (
                "custom_single_choice__letters_requested",
                "Letters Testamentary".to_string(),
            ),
            (
                "custom_text__codicil_witnesses",
                "Morgan Riley Reese".to_string(),
            ),
            (
                "custom_text__codicil_witnesses_2",
                "Morgan Riley Reese".to_string(),
            ),
            ("custom_text__decedent_aka", "Jordan A. Blake".to_string()),
            (
                "custom_text__decedent_citizenship",
                "United States".to_string(),
            ),
            ("custom_text__decedent_county", "Queens".to_string()),
            (
                "custom_text__decedent_place_of_death",
                "Queens, New York".to_string(),
            ),
            ("custom_text__filing_county", "Queens".to_string()),
            (
                "custom_text__letters_to_name",
                "Jordan Avery Blake".to_string(),
            ),
            ("custom_text__no_later_instruments", "NONE".to_string()),
            ("custom_text__other_petition_details", "".to_string()),
            ("custom_text__other_testamentary_assets", "NONE".to_string()),
            (
                "custom_text__petitioner_citizenship",
                "United States".to_string(),
            ),
            ("custom_text__petitioner_mailing_address", "".to_string()),
            (
                "custom_text__will_witnesses",
                "Casey Devon Ellis, Morgan Riley Reese".to_string(),
            ),
            ("custom_usd__gross_rents_18_months", "2500".to_string()),
            (
                "custom_usd__improved_real_property_less_than",
                "2500".to_string(),
            ),
            (
                "custom_usd__improved_real_property_value",
                "2500".to_string(),
            ),
            (
                "custom_usd__unimproved_real_property_less_than",
                "2500".to_string(),
            ),
            (
                "custom_usd__unimproved_real_property_value",
                "2500".to_string(),
            ),
            ("custom_yes_no__executor_is_attorney", "no".to_string()),
            (
                "custom_yes_no__executor_is_convicted_felon",
                "no".to_string(),
            ),
            ("custom_yes_no__executor_is_draftsperson", "no".to_string()),
            (
                "custom_yes_no__petitioner_is_named_executor",
                "yes".to_string(),
            ),
            ("person__client.city", "Queens".to_string()),
            ("person__client.name", "Jordan Avery Blake".to_string()),
            ("person__client.state", "NY".to_string()),
            ("person__client.street", "1 Main St".to_string()),
            ("person__client.zip", "11101".to_string()),
            ("person__decedent.city", "Queens".to_string()),
            ("person__decedent.name", "Jordan Avery Blake".to_string()),
            ("person__decedent.state", "NY".to_string()),
            ("person__decedent.street", "2 Side St".to_string()),
        ],
        other => panic!("no sample answers for `{other}`"),
    }
}

fn flattened_text_smoke_values(form_code: &str) -> &'static [&'static str] {
    match form_code {
        "nv__llc_formation" | "nv__profit_corp_formation" | "nv__business_trust_formation" => {
            &["Neon Law Services", "Aries Client"]
        }
        "us__naturalization" => &["1990-04-12", "maria@example.com", "Santos Gómez", "María"],
        "ca__name_change_petition"
        | "ca__name_change_order_to_show_cause"
        | "ca__name_change_decree" => &["Jordan Kim Blake", "Jordan Kim Rivera", "Sacramento"],
        "ca__name_change_attachment" => &[
            "Jordan Kim Blake",
            "Jordan Kim Rivera",
            "1990-04-12",
            "Fresno, California",
        ],
        "ca__civil_case_cover_sheet" => &["Jordan Kim Blake", "Sacramento"],
        "ca__gender_identity_name_change_petition" | "ca__gender_recognition_petition" => &[
            "Casey Finley Harper",
            "Casey Finley Santos",
            "1988-09-30",
            "San Diego, California",
        ],
        "ca__gender_identity_name_change_order_to_show_cause"
        | "ca__gender_identity_name_change_decree" => {
            &["Casey Finley Harper", "Casey Finley Santos", "Fresno"]
        }
        "ca__gender_recognition_order" => &["Casey Finley Harper", "Casey Finley Santos"],
        "nv__adult_name_change_petition" => {
            &["Alex Kim Marlowe", "Marlowe", "Quill", "1992-02-17", "Elko"]
        }
        "nv__dv_protection_order_application" => {
            &["Cora Blair Stubbs", "Odile Quill Prine", "October 12, 2026"]
        }
        "nv__las_vegas_small_claims_complaint" => &["Sample entry for defendant_caption"],
        "nv__las_vegas_small_claims_counterclaim" => &["Sample entry for counterdefendant_caption"],
        "nv__washoe_adult_name_change_petition" => &["Sample entry for birthplace_city"],
        "nv__washoe_fee_waiver_application" => &["Sample entry for account_one"],
        "ny__nc1_adult_name_change_petition" => &["Sample entry for child_support_court"],
        "ny__nc2_minor_name_change_petition" => &["Sample entry for minor_child_support_court"],
        "ny__nc3_lip_name_change_petition" => &["Sample entry for child_support_court"],
        "ny__nc4_family_name_change_petition" => &["Sample entry for current_address"],
        "ny__nc5_family_name_change_supplement" => {
            &["Sample entry for child_support_collection_unit"]
        }
        "ny__civ_sc50_statement_of_claim" => &["Sample entry for auto_accident_place"],
        "wa__adult_name_change_petition" => {
            &["The new name reflects the petitioner's family ties."]
        }
        "wa__minor_name_change_petition" => &["Sample entry for best_interest_reasons"],
        "wa__small_claims_notice" => &["Sample entry for claim_facts"],
        "wa__small_claims_plaintiff_packet" => &["Aries Client"],
        other => panic!("no flattened text smoke values for `{other}`"),
    }
}

/// The pin is the gate: bytes that don't match it must never be filled,
/// and a blank missing from the bucket is a loud error, not a fallback.
#[tokio::test]
async fn tampered_or_missing_blanks_fail_loudly_before_any_fill() {
    let (storage, _root) = test_storage().await;
    let storage: &dyn StorageService = &storage;
    let form = forms::get("nv__llc_formation")
        .expect("registry loads")
        .expect("LLC packet vendored");

    // Missing object: the pull itself errors.
    let err = storage.get(form.object_path).await.unwrap_err();
    assert!(
        matches!(err, cloud::StorageError::NotFound(_)),
        "missing blank must surface as an error, got {err:?}"
    );

    // Staged bytes that fail the pin: verification refuses them.
    let staged = synthetic_blank(form.code, form.object_path);
    let pin = forms::sha256_hex(&staged);
    storage
        .put(form.object_path, b"%PDF-1.5 re-vendored", "application/pdf")
        .await
        .expect("stage tampered bytes");
    let pulled = storage.get(form.object_path).await.expect("pull");
    let err = forms::verify_sha256(&pin, &pulled.bytes).unwrap_err();
    assert_eq!(err.pinned, pin);
    assert_ne!(err.actual, err.pinned);
}

/// Resolution behavior that needs no bytes at all: filled slots and
/// their printed titles follow the people-list rows. A slot's title is
/// the member's own `title` row part — a person fact entered at intake,
/// not a value derived from the management-structure choice — and a row
/// the respondent never gave fills nothing.
#[test]
fn single_member_llc_leaves_empty_slots_and_their_titles_blank() {
    let one_person = r#"[{"name": "Pisces Founder", "title": "Managing Member",
        "street": "9 Quiet Rd", "city": "Henderson", "state": "NV",
        "zip": "89002", "country": "USA"}]"#;
    let answers: BTreeMap<String, String> = [
        ("entity__company.name", "Solo Founder LLC".to_string()),
        (
            "person__registered_agent.name",
            "Neon Law Services".to_string(),
        ),
        (
            "custom_single_choice__management_structure",
            "members".to_string(),
        ),
        ("people__managing_members", one_person.to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let resolved = forms::fill_values("nv__llc_formation", &answers)
        .unwrap()
        .unwrap();
    assert_eq!(
        resolved["people__managing_members.0.title"],
        "Managing Member"
    );
    assert!(
        !resolved.contains_key("people__managing_members.1.title"),
        "an empty officer slot must not carry a printed title"
    );
    assert!(!resolved.contains_key("people__managing_members.1.name"));
}

/// Business trusts print `Trustee` in the title cells for each trustee
/// row. Existing answers may predate the explicit `title` row part, so
/// the form fill seam supplies the required filing label when a trustee
/// row exists.
#[test]
fn business_trust_trustee_rows_default_missing_titles() {
    let trustees = r#"[
        {"name": "Aries Client", "street": "1 Main St", "city": "Las Vegas",
         "state": "NV", "zip": "89101", "country": "USA"},
        {"name": "Libra Partner", "street": "2 Side St", "city": "Reno",
         "state": "NV", "zip": "89501", "country": "USA"}
    ]"#;
    let answers: BTreeMap<String, String> = [
        ("entity__company.name", "Neon Demo Trust".to_string()),
        (
            "person__registered_agent.name",
            "Neon Law Services".to_string(),
        ),
        ("people__trustees", trustees.to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let resolved = forms::fill_values("nv__business_trust_formation", &answers)
        .unwrap()
        .unwrap();
    assert_eq!(resolved["people__trustees.0.title"], "Trustee");
    assert_eq!(resolved["people__trustees.1.title"], "Trustee");
    assert!(!resolved.contains_key("people__trustees.2.title"));
}

/// The fill itself still rejects bad inputs loudly — a wrong radio
/// state and a misspelled field name are `pdf` errors, not silent
/// blanks. (Field-shape truth for the canonical blanks is `navigator
/// forms sync`/`fields` territory; the shapes here come from the
/// manifest + the notation's choices.)
#[test]
fn wrong_states_and_misspelled_names_are_loud_fill_errors() {
    let blank = synthetic_blank(
        "nv__llc_formation",
        "forms/united_states/nevada/state/nv__llc_formation.pdf",
    );
    let fill = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    };
    let err = pdf::fill_acroform(
        &blank,
        &fill(&[("custom_single_choice__management_structure", "Yes")]),
    )
    .unwrap_err();
    match err {
        pdf::PdfError::InvalidChoice { field, allowed, .. } => {
            assert_eq!(field, "custom_single_choice__management_structure");
            assert_eq!(allowed, vec!["managers", "members"]);
        }
        other => panic!("expected InvalidChoice, got {other:?}"),
    }
    let err =
        pdf::fill_acroform(&blank, &fill(&[("Name of Entity", "Neon Demo LLC")])).unwrap_err();
    assert!(matches!(err, pdf::PdfError::UnmatchedField(name) if name == "Name of Entity"));
}
