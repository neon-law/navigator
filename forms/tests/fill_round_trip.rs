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
