//! Cross-crate reconciliation for the court-form registry:
//! `store/seeds/Court.yaml` ↔ the forms shelf ↔ the vendored `FormMeta`
//! registry. Mirrors `public_notations_catalog_matches_legal_templates`:
//! the tracked registry and the tracked templates must stay in lockstep —
//! every registry entry resolves to a real template, and no court-form
//! template is an orphan. No official blank is ever checked in: the
//! registry pins blanks by sha-256 where the office publishes them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use store::courts::{Court, CourtForm};

/// Filing offices that are agencies rather than courts. A form under one
/// of these directories is an agency form and never appears in the court
/// registry; any other office directory is a court key and must be
/// registered under `store/seeds/Court.yaml`.
const AGENCY_OFFICES: &[&str] = &["irs", "state", "uscis"];

/// One template on the forms shelf.
#[derive(Debug)]
struct ShelfTemplate {
    /// The template code: the file stem, mirrored by the template's
    /// `form:` frontmatter when it carries one.
    code: String,
    /// The `jurisdiction:` frontmatter value.
    jurisdiction: String,
    /// The office directory the template lives under — an
    /// [`AGENCY_OFFICES`] member or a registered court key.
    office: String,
    /// Path relative to the repository root, for failure messages.
    display: String,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli crate lives below the repo root")
        .to_path_buf()
}

fn collect_templates(dir: &Path, root: &Path, out: &mut Vec<ShelfTemplate>) {
    for entry in std::fs::read_dir(dir).expect("read forms-shelf directory") {
        let path = entry.expect("read forms-shelf entry").path();
        if path.is_dir() {
            collect_templates(&path, root, out);
        } else if path.extension().is_some_and(|ext| ext == "md")
            && path.file_name().is_none_or(|name| name != "README.md")
        {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            let contents = std::fs::read_to_string(&path).expect("read forms-shelf template");
            // forms/<country>/<jurisdiction>/<office>/<code>.md — the office
            // decides whether the template is an agency form or a court form.
            let office = path
                .parent()
                .and_then(Path::file_name)
                .expect("template has a parent directory")
                .to_string_lossy()
                .to_string();
            out.push(ShelfTemplate {
                code: path
                    .file_stem()
                    .expect("template has a stem")
                    .to_string_lossy()
                    .to_string(),
                jurisdiction: jurisdiction_from_frontmatter(&contents)
                    .unwrap_or_else(|| {
                        panic!(
                            "{} is missing `jurisdiction:` frontmatter",
                            relative.display()
                        )
                    })
                    .to_string(),
                office,
                display: relative.display().to_string(),
            });
        }
    }
}

fn jurisdiction_from_frontmatter(contents: &str) -> Option<&str> {
    let mut lines = contents.lines();
    if lines.next() != Some("---") {
        return None;
    }
    for line in lines {
        if line == "---" {
            return None;
        }
        if let Some(value) = line.strip_prefix("jurisdiction:") {
            return Some(value.trim());
        }
    }
    None
}

/// The registry as parsed by its typed module — the same parse the
/// compile-time consumers run.
fn court_registry() -> Vec<Court> {
    store::courts::embedded().expect("shipped court registry parses and validates")
}

fn forms_shelf() -> Vec<ShelfTemplate> {
    let root = repo_root();
    let mut templates = Vec::new();
    collect_templates(
        &root.join("templates/notations/forms"),
        &root,
        &mut templates,
    );
    templates
}

#[test]
fn every_registry_entry_resolves_to_a_real_template() {
    let courts = court_registry();
    let shelf_templates = forms_shelf();
    let shelf: BTreeMap<String, &ShelfTemplate> = shelf_templates
        .iter()
        .map(|t| (t.code.clone(), t))
        .collect();

    for court in &courts {
        for form in &court.forms {
            let template = shelf.get(&form.code).unwrap_or_else(|| {
                panic!(
                    "court `{}` registers form `{code}`, but no template \
                     `{code}.md` exists on the forms shelf \
                     (templates/notations/forms)",
                    court.key,
                    code = form.code
                )
            });
            assert_eq!(
                template.jurisdiction, court.jurisdiction,
                "{}, registered under court `{}` (jurisdiction {}): \
                 template frontmatter says jurisdiction `{}`",
                form.code, court.key, court.jurisdiction, template.jurisdiction
            );
            assert_eq!(
                template.office, court.key,
                "{}: the template must live under its own court's template \
                 directory (templates/notations/forms/…/{})",
                form.code, court.key
            );
            assert!(
                form.source_url.starts_with("https://"),
                "{}: source_url must be https",
                form.code
            );
            assert_eq!(
                form.sha256.len(),
                64,
                "{}: sha256 must pin the official blank where it is \
                 published — never check the blank itself in",
                form.code
            );
            if let Some(waiver) = &form.fee_waiver_code {
                assert!(
                    shelf.contains_key(waiver),
                    "{}: fee_waiver_code `{waiver}` must also resolve to a \
                     forms-shelf template",
                    form.code
                );
            }
        }
    }
}

#[test]
fn every_court_template_is_registered() {
    let courts = court_registry();
    let registered: BTreeMap<String, Vec<String>> = courts
        .iter()
        .map(|court| {
            (
                court.key.clone(),
                court.forms.iter().map(|f| f.code.clone()).collect(),
            )
        })
        .collect();
    let registered_codes: Vec<&String> =
        registered.values().flat_map(|codes| codes.iter()).collect();

    for template in forms_shelf() {
        if AGENCY_OFFICES.contains(&template.office.as_str()) {
            assert!(
                !registered_codes.contains(&&template.code),
                "{}: `{}` is an agency form — it is never a court-registry \
                 entry",
                template.display,
                template.code
            );
        } else {
            let codes = registered.get(&template.office).unwrap_or_else(|| {
                panic!(
                    "{}: template directory `{}` is neither an agency office \
                     nor a registered court key — add the court to \
                     store/seeds/Court.yaml or move the template",
                    template.display, template.office
                )
            });
            assert!(
                codes.contains(&template.code),
                "{}: court template `{}` under `{}` has no registry entry — \
                 register it under court `{}` in store/seeds/Court.yaml",
                template.display,
                template.code,
                template.office,
                template.office
            );
        }
    }
}

#[test]
fn vendored_formmeta_agrees_with_the_registry() {
    let courts = court_registry();
    let by_code: BTreeMap<String, (&Court, &CourtForm)> = courts
        .iter()
        .flat_map(|court| {
            court
                .forms
                .iter()
                .map(move |form| (form.code.clone(), (court, form)))
        })
        .collect();

    for form in forms::registry().expect("vendored form registry loads") {
        // An agency office decides its own fees; only court filings carry
        // registry rows.
        let office = form
            .object_path
            .trim_start_matches("forms/")
            .split('/')
            .nth(2)
            .expect("object paths sit two directories deep")
            .to_string();
        if AGENCY_OFFICES.contains(&office.as_str()) {
            continue;
        }
        let row = by_code.get(form.code).unwrap_or_else(|| {
            panic!(
                "`{}` is vendored as a court form but has no entry in \
                 store/seeds/Court.yaml under court `{office}`",
                form.code
            )
        });
        let (court, registry_form) = *row;
        assert_eq!(
            form.origin_url, registry_form.source_url,
            "`{}` under `{}`: the vendored origin_url must match the \
             registry row",
            form.code, court.key
        );
        assert_eq!(
            form.pinned_sha256(),
            registry_form.sha256,
            "`{}` under `{}`: the vendored pin must match the registry \
             row's sha256 of the official blank",
            form.code,
            court.key
        );
        match (&form.filing_fee, &registry_form.filing_fee) {
            (Some(vendored), Some(published)) => {
                assert_eq!(
                    (vendored.amount, vendored.currency),
                    (published.amount.as_str(), published.currency.as_str()),
                    "`{}` under `{}`: the vendored filing fee must match the \
                     registry row",
                    form.code,
                    court.key
                );
            }
            (None, None) => {}
            (vendored, published) => panic!(
                "`{}` under `{}`: filing fee disagreement — vendored \
                 {vendored:?} vs registry row {published:?}",
                form.code, court.key
            ),
        }
        assert_eq!(
            form.fee_waiver_code,
            registry_form.fee_waiver_code.as_deref(),
            "`{}` under `{}`: the vendored fee-waiver code must match the \
             registry row",
            form.code,
            court.key
        );
    }
}
