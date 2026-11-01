//! Temporary local vendoring helper (untracked; delete before commit).
//! Mirrors `cli/src/forms_sync.rs::run_reauthor` byte for byte but reads
//! the blank from the working tree instead of the assets bucket: loads the
//! `<code>.fields.toml` map beside the blank, runs the pure transform
//! (`strip_static_xfa` → `field_names` → `plan` → `reauthor`), writes the
//! re-authored working copy and `.fields` manifest, prints the pin, and
//! fills a QA sample.
//!
//! Usage: `cargo run -p forms --example vendor_local -- <code>`

use std::path::PathBuf;

type BoxErr = Box<dyn std::error::Error>;

fn main() -> Result<(), BoxErr> {
    let code = std::env::args()
        .nth(1)
        .ok_or("usage: vendor_local <form-code>")?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("forms lives one level below the repository root")
        .to_path_buf();
    let object_path = object_path_for(&code)?;
    let blank_path = root.join("templates").join("notations").join(&object_path);

    // The transform overwrites `blank_path` with the canonical copy, so
    // every run must transform from a pristine source snapshot instead of
    // re-reading its own output.
    std::fs::create_dir_all("/tmp/vendor")?;
    let official_copy = PathBuf::from("/tmp/vendor").join(format!("{code}-official.pdf"));
    if !official_copy.exists() {
        std::fs::copy(&blank_path, &official_copy)
            .map_err(|e| -> BoxErr { format!("seed {}: {e}", official_copy.display()).into() })?;
    }
    let raw = std::fs::read(&official_copy)
        .map_err(|e| -> BoxErr { format!("read {}: {e}", official_copy.display()).into() })?;

    let map_path = blank_path.with_extension("fields.toml");
    let Ok(map_raw) = std::fs::read_to_string(&map_path) else {
        // Names-only mode: no map yet — print authoritative /T names.
        let blank = pdf::strip_static_xfa(&raw)?;
        let names = pdf::field_names(&blank)?;
        println!("official blank carries {} fields", names.len());
        for name in &names {
            println!("  {name}");
        }
        return Ok(());
    };
    let map = forms::parse_field_map(&map_raw)?;

    let template_path = blank_path.with_extension("md");
    let template_raw = std::fs::read_to_string(&template_path)?;
    let states = questionnaire_states(&template_raw)?;
    println!("template states: {states:?}");

    let blank = pdf::strip_static_xfa(&raw)?;
    let names = pdf::field_names(&blank)?;
    println!("blank carries {} fields", names.len());
    let plan = forms::reauthor::plan(&map, &names, &states)?;
    let spec = pdf::ReauthorSpec {
        renames: plan.renames,
        radios: plan
            .radios
            .into_iter()
            .map(|(name, members)| pdf::RadioMergeSpec {
                name,
                members: members
                    .into_iter()
                    .map(|member| pdf::RadioMergeMember {
                        field: member.field,
                        source_export: member.source_export,
                        final_export: member.final_export,
                    })
                    .collect(),
            })
            .collect(),
        literals: plan.literals,
    };
    let reauthored = pdf::reauthor(&blank, &spec)?;
    println!(
        "re-authored {} fields",
        pdf::field_names(&reauthored)?.len()
    );

    std::fs::write(&blank_path, &reauthored)
        .map_err(|e| -> BoxErr { format!("write {}: {e}", blank_path.display()).into() })?;
    let mut manifest = pdf::field_names(&reauthored)?;
    manifest.sort();
    let manifest_path = blank_path.with_extension("fields");
    std::fs::write(&manifest_path, manifest.join("\n") + "\n")
        .map_err(|e| -> BoxErr { format!("write {}: {e}", manifest_path.display()).into() })?;
    println!("working copy:   {}", blank_path.display());
    println!(
        "manifest:       {} ({} fields)",
        manifest_path.display(),
        manifest.len()
    );

    let digest = forms::sha256_hex(&reauthored);
    println!("pin:            {digest}");

    fill_sample(
        &code,
        &reauthored,
        &manifest,
        &choice_first_exports(&template_raw)?,
    )?;
    Ok(())
}

/// The sibling notation's declared questionnaire states — copied from
/// `cli/src/forms_sync.rs::questionnaire_states`.
fn questionnaire_states(contents: &str) -> Result<Vec<String>, BoxErr> {
    #[derive(serde::Deserialize)]
    struct Notation {
        questionnaire: std::collections::BTreeMap<String, serde_yaml::Value>,
    }
    let fm = contents
        .strip_prefix("---\n")
        .and_then(|rest| rest.find("\n---").map(|end| &rest[..end]))
        .ok_or("no `---` frontmatter block")?;
    let notation: Notation = serde_yaml::from_str(fm)?;
    Ok(notation
        .questionnaire
        .into_keys()
        .filter(|s| s != "BEGIN" && s != "END")
        .collect())
}

/// The template's `choices:` sets, keyed by state tail, mapped to each
/// set's first (key, export) pair — the sample fill picks the key so
/// radio and dropdown fields are exercised with values the widgets
/// actually accept.
fn choice_first_exports(
    contents: &str,
) -> Result<std::collections::BTreeMap<String, (String, String)>, BoxErr> {
    let fm = contents
        .strip_prefix("---\n")
        .and_then(|rest| rest.find("\n---").map(|end| &rest[..end]))
        .ok_or("no `---` frontmatter block")?;
    let mut out = std::collections::BTreeMap::new();
    let mut current: Option<String> = None;
    let mut in_choices = false;
    for line in fm.lines() {
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim_end();
        if indent == 0 {
            // Only the top-level `choices:` section carries key/export
            // pairs; the state listing and other frontmatter sections reuse
            // the same indent shapes with different meanings.
            in_choices = trimmed == "choices:";
            current = None;
        } else if in_choices && indent == 2 && trimmed.ends_with(':') && !trimmed.contains(": ") {
            current = Some(trimmed.trim().trim_end_matches(':').to_string());
        } else if in_choices && indent >= 4 {
            if let (Some(state), Some((key, export))) =
                (current.as_deref(), trimmed.trim_start().split_once(": "))
            {
                out.entry(state.to_string()).or_insert_with(|| {
                    (
                        key.trim().trim_matches('"').to_string(),
                        export.trim().trim_matches('"').to_string(),
                    )
                });
            }
        }
    }
    Ok(out)
}

fn fill_sample(
    code: &str,
    blank: &[u8],
    names: &[String],
    choices: &std::collections::BTreeMap<String, (String, String)>,
) -> Result<(), BoxErr> {
    std::fs::create_dir_all("/tmp/qa")?;
    let mut values = std::collections::BTreeMap::new();
    for name in names {
        if name.starts_with("unmapped__") {
            continue;
        }
        let tail = name.split_once("__").map(|(_, tail)| tail.to_string());
        values.insert(
            name.clone(),
            match name.split_once("__").map(|(ty, _)| ty) {
                Some("custom_yes_no") => "yes".to_string(),
                Some("custom_single_choice") => tail
                    .as_deref()
                    .and_then(|tail| choices.get(tail))
                    .map_or_else(|| format!("SAMPLE {name}"), |(key, _)| key.clone()),
                Some("person" | "people") => {
                    let tail = name.rsplit('.').next().unwrap_or("name");
                    match tail {
                        "given" => "Jamie".to_string(),
                        "middle" => "Q".to_string(),
                        "family" => "Rivera".to_string(),
                        "email" => "jamie@example.com".to_string(),
                        _ => "Jamie Q Rivera".to_string(),
                    }
                }
                _ => format!("SAMPLE {name}"),
            },
        );
    }
    // Kid-ful radio states fill by widget export; checkbox groups synthesized
    // from `checked_when` rules fill by questionnaire key. The fill rejects
    // the wrong kind of value one state at a time — swap each offender to its
    // export and retry until the fill is clean.
    let mut filled = pdf::fill_acroform(blank, &values);
    for _ in 0..32 {
        let Err(pdf::PdfError::InvalidChoice { field, .. }) = &filled else {
            break;
        };
        let Some((_, (_, export))) = choices.iter().find(|(state, _)| {
            field
                .strip_prefix("custom_single_choice__")
                .is_some_and(|tail| tail == state.as_str())
        }) else {
            break;
        };
        let Some(name) = names.iter().find(|n| n.ends_with(field)) else {
            break;
        };
        values.insert(name.clone(), export.clone());
        filled = pdf::fill_acroform(blank, &values);
    }
    let filled = filled?;
    if std::env::var("VENDOR_KEEP_UNFLATTENED").is_ok() {
        std::fs::write(format!("/tmp/qa/{code}-unflattened.pdf"), &filled)?;
        println!("unflattened copy: /tmp/qa/{code}-unflattened.pdf");
    }
    std::fs::write(format!("/tmp/qa/{code}-filled.pdf"), pdf::flatten(&filled)?)?;
    println!("filled sample: /tmp/qa/{code}-filled.pdf");
    Ok(())
}

fn object_path_for(code: &str) -> Result<String, BoxErr> {
    // Mirrors the FormMeta object_path registered for the code.
    let path: &str = match code {
        "wa__adult_name_change_petition" => {
            "forms/united_states/washington/king_county_district_court/wa__adult_name_change_petition.pdf"
        }
        "wa__minor_name_change_petition" => {
            "forms/united_states/washington/king_county_district_court/wa__minor_name_change_petition.pdf"
        }
        "wa__small_claims_notice" => {
            "forms/united_states/washington/king_county_district_court/wa__small_claims_notice.pdf"
        }
        "wa__small_claims_plaintiff_packet" => {
            "forms/united_states/washington/king_county_district_court/wa__small_claims_plaintiff_packet.pdf"
        }
        "nv__washoe_adult_name_change_petition" => {
            "forms/united_states/nevada/second_judicial_district_court/nv__washoe_adult_name_change_petition.pdf"
        }
        "nv__washoe_fee_waiver_application" => {
            "forms/united_states/nevada/second_judicial_district_court/nv__washoe_fee_waiver_application.pdf"
        }
        "nv__las_vegas_small_claims_complaint" => {
            "forms/united_states/nevada/las_vegas_justice_court/nv__las_vegas_small_claims_complaint.pdf"
        }
        "nv__las_vegas_small_claims_counterclaim" => {
            "forms/united_states/nevada/las_vegas_justice_court/nv__las_vegas_small_claims_counterclaim.pdf"
        }
        "ny__nc1_adult_name_change_petition" => {
            "forms/united_states/new_york/new_york_supreme_court/ny__nc1_adult_name_change_petition.pdf"
        }
        "ny__nc2_minor_name_change_petition" => {
            "forms/united_states/new_york/new_york_supreme_court/ny__nc2_minor_name_change_petition.pdf"
        }
        "ny__nc3_lip_name_change_petition" => {
            "forms/united_states/new_york/new_york_supreme_court/ny__nc3_lip_name_change_petition.pdf"
        }
        "ny__nc4_family_name_change_petition" => {
            "forms/united_states/new_york/new_york_supreme_court/ny__nc4_family_name_change_petition.pdf"
        }
        "ny__nc5_family_name_change_supplement" => {
            "forms/united_states/new_york/new_york_supreme_court/ny__nc5_family_name_change_supplement.pdf"
        }
        "ny__civ_sc50_statement_of_claim" => {
            "forms/united_states/new_york/nyc_civil_court/ny__civ_sc50_statement_of_claim.pdf"
        }
        "ny__probate_petition" => {
            "forms/united_states/new_york/new_york_surrogates_court/ny__probate_petition.pdf"
        }
        other => return Err(format!("unknown example code {other}").into()),
    };
    Ok(path.to_string())
}
