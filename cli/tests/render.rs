//! End-to-end tests for `navigator notation pdf <file> --out <pdf>` and
//! `navigator notation word <file> --out <docx>`. Each test writes a
//! notation fixture to a tempdir, invokes the real binary, and checks the
//! produced document (or the refusal).

use std::fs;
use std::process::Command;

use assert_cmd::cargo::cargo_bin;
use tempfile::TempDir;

/// A minimal notation template that passes the full validation gate:
/// `kind:` (the declared classifier) / `title` / `respondent_type` /
/// `confidential`, a `questionnaire:` + `workflow:` with the
/// required lawyer review, and a clean Markdown body. `output:` is
/// `letter`; callers can override on the CLI.
const VALID: &str = "\
---
kind: letter
title: Test Demand
respondent_type: entity
confidential: true
output: letter
questionnaire:
  BEGIN:
    _: END
  END: {}
workflow:
  BEGIN:
    intake_submitted: lawyer_review
  lawyer_review:
    approved: END
    rejected: END
  END: {}
---

# Demand

Pay the sum of `{{amount}}` to **NEON LAW** without delay.

- First point
- Second point
";

/// Same fixture as `VALID` but with no `output:` declared at all — the
/// regression case: a `kind: letter` template must still render on
/// letterhead by default, derived from `Kind::default_output`.
const VALID_NO_OUTPUT: &str = "\
---
kind: letter
title: Test Demand
respondent_type: entity
confidential: true
questionnaire:
  BEGIN:
    _: END
  END: {}
workflow:
  BEGIN:
    intake_submitted: lawyer_review
  lawyer_review:
    approved: END
    rejected: END
  END: {}
---

# Demand

Pay the sum of `{{amount}}` to **NEON LAW** without delay.

- First point
- Second point
";

/// Same shape, `kind: will` — a kind whose default is `plain`, so this
/// proves the derivation does not blanket every notation kind in
/// letterhead. A `will` is an outlined kind, so the body carries the `#`
/// title and the numbered `##` section `N123` requires of one; without
/// them validation refuses the template and nothing renders.
const VALID_WILL_NO_OUTPUT: &str = "\
---
kind: will
title: Test Will
respondent_type: person
confidential: true
questionnaire:
  BEGIN:
    _: END
  END: {}
workflow:
  BEGIN:
    intake_submitted: lawyer_review
  lawyer_review:
    approved: END
    rejected: END
  END: {}
---

# Last Will and Testament

## I. Revocation

I hereby revoke all prior wills.
";

const VALID_TYPED: &str = "\
---
kind: letter
title: Typed Demand
respondent_type: entity
confidential: true
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: people__members
  people__members:
    _: END
  END: {}
workflow:
  BEGIN:
    intake_submitted: lawyer_review
  lawyer_review:
    approved: END
    rejected: END
  END: {}
---

Client: {{person__client.name}}

Members:
{{#for m in people__members}}- {{m.name}} from {{m.city}}
{{/for}}
";

fn write(dir: &TempDir, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, body).expect("write fixture");
    path
}

fn render(subcommand: &str, args: &[&std::ffi::OsStr]) -> std::process::Output {
    Command::new(cargo_bin("navigator"))
        .args(["notation", subcommand])
        .args(args)
        .output()
        .expect("run navigator notation pdf/word")
}

fn render_pdf(args: &[&std::ffi::OsStr]) -> std::process::Output {
    render("pdf", args)
}

fn render_word(args: &[&std::ffi::OsStr]) -> std::process::Output {
    render("word", args)
}

#[test]
fn renders_a_letter_pdf_from_a_valid_template() {
    let work = TempDir::new().unwrap();
    let src = write(&work, "demand.md", VALID);
    let out = work.path().join("demand.pdf");
    let result = render_pdf(&[
        "--preview".as_ref(),
        src.as_os_str(),
        "--out".as_ref(),
        out.as_os_str(),
    ]);
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let bytes = fs::read(&out).expect("pdf written");
    assert_eq!(&bytes[..4], b"%PDF", "output is not a PDF");
}

#[test]
fn renders_a_word_document_from_a_valid_template() {
    let work = TempDir::new().unwrap();
    let src = write(&work, "demand.md", VALID);
    let out = work.path().join("demand.docx");
    let result = render_word(&[
        "--preview".as_ref(),
        src.as_os_str(),
        "--out".as_ref(),
        out.as_os_str(),
    ]);
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let bytes = fs::read(&out).expect("DOCX written");
    assert_eq!(
        &bytes[..4],
        b"PK\x03\x04",
        "output is not an Open XML package"
    );
}

#[test]
fn refuses_an_output_extension_the_command_does_not_support() {
    let work = TempDir::new().unwrap();
    let src = write(&work, "demand.md", VALID);

    let txt_out = work.path().join("demand.txt");
    let result = render_pdf(&[src.as_os_str(), "--out".as_ref(), txt_out.as_os_str()]);
    assert!(
        !result.status.success(),
        "an unsupported output extension must be refused"
    );
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("output extension must be `.pdf`"),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!txt_out.exists(), "a refused render must not write a file");

    // Each command takes only its own extension — `pdf` will not write a
    // `.docx` and `word` will not write a `.pdf`, so a typo'd `--out` is
    // caught rather than silently written under the wrong format.
    let docx_out = work.path().join("demand.docx");
    let result = render_pdf(&[src.as_os_str(), "--out".as_ref(), docx_out.as_os_str()]);
    assert!(
        !result.status.success(),
        "`pdf` must refuse a `.docx` target"
    );
    assert!(!docx_out.exists());

    let pdf_out = work.path().join("demand.pdf");
    let result = render_word(&[src.as_os_str(), "--out".as_ref(), pdf_out.as_os_str()]);
    assert!(
        !result.status.success(),
        "`word` must refuse a `.pdf` target"
    );
    assert!(!pdf_out.exists());
}

#[test]
fn repeated_notation_renders_are_identical_and_valid_pdfs() {
    let work = TempDir::new().unwrap();
    let src = write(&work, "demand.md", VALID);
    let first_out = work.path().join("first.pdf");
    let second_out = work.path().join("second.pdf");

    for out in [&first_out, &second_out] {
        let result = render_pdf(&[
            "--preview".as_ref(),
            src.as_os_str(),
            "--out".as_ref(),
            out.as_os_str(),
        ]);
        assert!(
            result.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    let first = fs::read(&first_out).expect("first PDF written");
    let second = fs::read(&second_out).expect("second PDF written");
    assert_eq!(
        first, second,
        "repeated renders must be byte-for-byte equal"
    );
    pdf::validate_pdf(&first).expect("the repeated render must remain a valid PDF");
}

#[test]
fn frontmatter_output_selects_the_frame_and_letterhead_is_larger_than_plain() {
    // `output:` is the template's own deliberate override, and the only
    // one: a `kind: will` template that declares none renders plain, an
    // `output: letter` one renders on letterhead, and the frame is read
    // from the document either way.
    let work = TempDir::new().unwrap();

    let letter_src = write(&work, "demand.md", VALID);
    let letter_out = work.path().join("letter.pdf");
    let letter = render_pdf(&[
        "--preview".as_ref(),
        letter_src.as_os_str(),
        "--out".as_ref(),
        letter_out.as_ref(),
    ]);
    assert!(
        letter.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&letter.stderr)
    );

    let plain_src = write(&work, "will.md", VALID_WILL_NO_OUTPUT);
    let plain_out = work.path().join("plain.pdf");
    let plain = render_pdf(&[
        "--preview".as_ref(),
        plain_src.as_os_str(),
        "--out".as_ref(),
        plain_out.as_ref(),
    ]);
    assert!(plain.status.success());
    assert!(
        String::from_utf8_lossy(&plain.stdout).contains("Plain"),
        "a `kind: will` template should report Plain, got: {}",
        String::from_utf8_lossy(&plain.stdout)
    );

    let letter_len = fs::read(&letter_out).unwrap().len();
    let plain_len = fs::read(&plain_out).unwrap().len();
    assert!(
        letter_len > plain_len,
        "letterhead PDF ({letter_len}) should exceed plain ({plain_len}) — logo missing?"
    );
}

#[test]
fn a_letter_kind_renders_on_letterhead_with_no_output_declared() {
    // The regression case: a `kind: letter` template with no `output:`
    // field must still derive letterhead by default, not silently fall
    // back to plain.
    let work = TempDir::new().unwrap();
    let src = write(&work, "demand.md", VALID_NO_OUTPUT);

    let derived_out = work.path().join("derived.pdf");
    let derived = render_pdf(&[
        "--preview".as_ref(),
        src.as_os_str(),
        "--out".as_ref(),
        derived_out.as_ref(),
    ]);
    assert!(
        derived.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&derived.stderr)
    );

    // The contrast case is a different `kind:`, not a flag: `will`
    // derives plain, so the two derivations are what differ.
    let plain_src = write(&work, "will.md", VALID_WILL_NO_OUTPUT);
    let plain_out = work.path().join("plain.pdf");
    let plain = render_pdf(&[
        "--preview".as_ref(),
        plain_src.as_os_str(),
        "--out".as_ref(),
        plain_out.as_ref(),
    ]);
    assert!(plain.status.success());

    let derived_len = fs::read(&derived_out).unwrap().len();
    let plain_len = fs::read(&plain_out).unwrap().len();
    assert!(
        derived_len > plain_len,
        "a `kind: letter` template with no `output:` should default to \
         letterhead ({derived_len}) rather than plain ({plain_len}) — logo missing?"
    );
    let stdout = String::from_utf8_lossy(&plain.stdout);
    assert!(
        stdout.contains("Plain"),
        "a `kind: will` template should report Plain, got: {stdout}"
    );
}

#[test]
fn a_plain_default_kind_renders_plain_with_no_output_declared() {
    // The mirror case: `kind: will` defaults to plain, so a template
    // declaring no `output:` must not pick up letterhead by accident.
    let work = TempDir::new().unwrap();
    let src = write(&work, "will.md", VALID_WILL_NO_OUTPUT);
    let out = work.path().join("will.pdf");
    let result = render_pdf(&[src.as_os_str(), "--out".as_ref(), out.as_os_str()]);
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("Plain"),
        "a `kind: will` template with no `output:` should render Plain, got: {stdout}"
    );
}

#[test]
fn answer_substitutes_a_placeholder() {
    let work = TempDir::new().unwrap();
    let src = write(&work, "demand.md", VALID);
    let out = work.path().join("demand.pdf");
    let result = render_pdf(&[
        src.as_os_str(),
        "--out".as_ref(),
        out.as_ref(),
        "--answer".as_ref(),
        "amount=5000 USD".as_ref(),
    ]);
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    // The rendered PDF compresses text, so we can't grep the value out;
    // success plus a valid PDF is the contract. The substitution logic
    // itself is unit-tested in the pdf crate's markdown round-trip.
    assert_eq!(&fs::read(&out).unwrap()[..4], b"%PDF");
}

#[test]
fn render_uses_shared_notation_evaluator_for_dotted_fields_and_loops() {
    let work = TempDir::new().unwrap();
    let src = write(&work, "typed_demand.md", VALID_TYPED);
    let out = work.path().join("typed_demand.pdf");
    let result = render_pdf(&[
        src.as_os_str(),
        "--out".as_ref(),
        out.as_ref(),
        "--answer".as_ref(),
        "person__client.name=Libra Prime".as_ref(),
        "--answer".as_ref(),
        r#"people__members=[{"name":"Aries","city":"Las Vegas"},{"name":"Virgo","city":"Reno"}]"#
            .as_ref(),
    ]);
    assert!(
        result.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(&fs::read(&out).unwrap()[..4], b"%PDF");
}

#[test]
fn renders_despite_a_non_blocking_advisory() {
    // `onchain`, the catalog's one allowed-but-deferred step, earns the
    // yellow N112 "not built yet" advisory — a Warning, not an Error.
    // Rendering must not be blocked by it (it is, however, still printed
    // so the author sees it), mirroring `validate` / `import`. Routed
    // through the `approved` branch after `lawyer_review` so the
    // mandatory human gate stays intact.
    let source = VALID
        .replace("    approved: END\n", "    approved: onchain\n")
        .replace(
            "    rejected: END\n  END: {}\n",
            "    rejected: END\n  onchain:\n    done: END\n  END: {}\n",
        );
    let work = TempDir::new().unwrap();
    let src = write(&work, "demand.md", &source);
    let out = work.path().join("demand.pdf");
    let result = render_pdf(&[
        "--preview".as_ref(),
        src.as_os_str(),
        "--out".as_ref(),
        out.as_os_str(),
    ]);
    assert!(
        result.status.success(),
        "a Warning-only template must still render, stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("N112"),
        "the advisory should still be surfaced, got stdout: {stdout}"
    );
    assert_eq!(
        &fs::read(&out).unwrap()[..4],
        b"%PDF",
        "output is not a PDF"
    );
}

#[test]
fn filename_supplies_the_template_code_without_frontmatter_metadata() {
    let work = TempDir::new().unwrap();
    let src = write(&work, "demand.md", VALID);
    let out = work.path().join("demand.pdf");
    let result = render_pdf(&[
        "--preview".as_ref(),
        src.as_os_str(),
        "--out".as_ref(),
        out.as_os_str(),
    ]);
    assert!(
        result.status.success(),
        "filename-derived code should render: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(out.exists(), "rendered PDF should exist");
}

#[test]
fn the_format_flag_is_retired_and_cannot_reframe_an_instrument() {
    // LAW-15: the frame comes from the document's own header. A `kind: will`
    // template renders the unadorned instrument, and a `--format letter`
    // passed out of habit would put the firm's letterhead on it, so the
    // argument parser refuses the flag rather than writing a wrongly framed
    // PDF nobody was warned about.
    let work = TempDir::new().unwrap();
    let src = write(&work, "will.md", VALID_WILL_NO_OUTPUT);
    let out = work.path().join("will.pdf");
    let result = render_pdf(&[
        src.as_os_str(),
        "--out".as_ref(),
        out.as_ref(),
        "--format".as_ref(),
        "letter".as_ref(),
    ]);
    assert!(!result.status.success(), "--format must be refused");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("unexpected argument") && stderr.contains("--format"),
        "expected clap to refuse the retired flag, got: {stderr}"
    );
    assert!(!out.exists(), "no PDF should be written");
}

/// A choice question whose options carry the prose the body reads.
/// `choices.<key>` is a `value: label` map, and the body
/// interpolates the state — so the rendered instrument must read the
/// label ("Nevada"), never the stored key ("nevada").
const VALID_CHOICE: &str = "\
---
kind: letter
title: Governed Demand
respondent_type: entity
confidential: true
prompts:
  governing_law: Which state's law governs this engagement?
choices:
  governing_law:
    nevada: Nevada
    california: California
questionnaire:
  BEGIN:
    _: custom_single_choice__governing_law
  custom_single_choice__governing_law:
    _: END
  END: {}
workflow:
  BEGIN:
    intake_submitted: lawyer_review
  lawyer_review:
    approved: END
    rejected: END
  END: {}
---

# Demand

This letter is governed by the law of {{custom_single_choice__governing_law}}.
";

#[test]
fn a_choice_answer_renders_its_label_not_its_stored_key() {
    // LAW-13: a `{{custom_single_choice__*}}` placeholder filled with a
    // declared choice *key* must reach the page as that choice's *label*.
    // The key is an answer code, not prose; substituting it verbatim put
    // "governed by the law of nevada" into the firm's own onboarding
    // letter. The portal's document path already resolves the label
    // (`retainer_walk::render_context_from_answers`); this proves the CLI
    // preview agrees with it rather than rendering a different document.
    let work = TempDir::new().unwrap();
    let src = write(&work, "governed.md", VALID_CHOICE);
    let out = work.path().join("governed.pdf");
    let result = render_pdf(&[
        src.as_os_str(),
        "--out".as_ref(),
        out.as_ref(),
        "--answer".as_ref(),
        "custom_single_choice__governing_law=nevada".as_ref(),
    ]);
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let bytes = fs::read(&out).expect("pdf written");
    assert_eq!(
        pdf::occurrence_count(&bytes, "the law of Nevada").expect("scan the rendered pdf"),
        1,
        "the choice label must reach the page"
    );
    assert_eq!(
        pdf::occurrence_count(&bytes, "the law of nevada").expect("scan the rendered pdf"),
        0,
        "the stored choice key must not reach the page"
    );
}

#[test]
fn a_free_text_answer_is_unaffected_by_choice_label_resolution() {
    // The other side of LAW-13: a state with no declared `choices:` keeps
    // its answer verbatim, so label resolution cannot swallow free text.
    let work = TempDir::new().unwrap();
    let src = write(&work, "demand.md", VALID);
    let out = work.path().join("demand.pdf");
    let result = render_pdf(&[
        src.as_os_str(),
        "--out".as_ref(),
        out.as_ref(),
        "--answer".as_ref(),
        "amount=5000 USD".as_ref(),
    ]);
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let bytes = fs::read(&out).expect("pdf written");
    assert_eq!(
        pdf::occurrence_count(&bytes, "5000 USD").expect("scan the rendered pdf"),
        1,
        "a free-text answer must render verbatim"
    );
}

/// A pleading in the shape `N123` requires: Arabic depth-1 markers, and a
/// `jurisdiction:` the court geometry is calibrated from. `{JURISDICTION}`
/// is substituted per test.
const VALID_PLEADING: &str = "\
---
kind: pleading
title: Motion to Compel
respondent_type: person
jurisdiction: {JURISDICTION}
caption_title: MOTION TO COMPEL
footer_title: MOTION TO COMPEL
confidential: true
questionnaire:
  BEGIN:
    _: END
  END: {}
workflow:
  BEGIN:
    intake_submitted: lawyer_review
  lawyer_review:
    approved: END
  END: {}
---

# Motion to Compel

## 1. Introduction

Plaintiff moves to compel further responses.

## 2. Argument

The request is proper under the governing rule.
";

fn pleading_fixture(jurisdiction: &str) -> String {
    VALID_PLEADING.replace("{JURISDICTION}", jurisdiction)
}

/// The synthetic matter's manifest beside a pleading, so the caption has a
/// court, parties, and counsel to compose from.
fn write_matter(dir: &TempDir) {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/synthetic_matter/navigator.yaml");
    fs::copy(manifest, dir.path().join("navigator.yaml")).expect("copy the synthetic manifest");
}

#[test]
fn a_pleading_renders_court_geometry_calibrated_by_its_jurisdiction() {
    // `Kind::Pleading::default_output()` is "pleading", but
    // `OutputFormat::parse` deliberately never constructs
    // `OutputFormat::Pleading` — its calibration comes from the template's
    // `jurisdiction:`, a second field a bare format name cannot supply. The
    // render path fed the derived name through that parser anyway and then
    // took `unwrap_or_default()`, so a validation-passing motion came out
    // on the plain frame: no numbered rail, wrong margins, wrong typeface.
    // Court paper rendered to the wrong geometry is a filing that can be
    // rejected, so the jurisdiction is resolved here instead.
    for (jurisdiction, expected) in [("NV", "NumberedRailTrial"), ("US", "NoRailTrial")] {
        let work = TempDir::new().unwrap();
        write_matter(&work);
        let src = write(&work, "motion.md", &pleading_fixture(jurisdiction));
        let out = work.path().join("motion.pdf");
        let result = render_pdf(&[src.as_os_str(), "--out".as_ref(), out.as_ref()]);
        assert!(
            result.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let stdout = String::from_utf8_lossy(&result.stdout);
        assert!(
            stdout.contains(&format!("Pleading({expected})")),
            "`jurisdiction: {jurisdiction}` should calibrate to {expected}, got: {stdout}"
        );
        assert_eq!(&fs::read(&out).unwrap()[..4], b"%PDF");
    }
}

/// One part of a `.docx` package, read back out of the zip.
fn docx_part(bytes: &[u8], name: &str) -> String {
    use std::io::Read as _;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("docx is a zip");
    let mut xml = String::new();
    archive
        .by_name(name)
        .unwrap_or_else(|_| panic!("docx carries {name}"))
        .read_to_string(&mut xml)
        .expect("read part");
    xml
}

#[test]
fn a_california_pleading_word_render_carries_the_numbered_rail() {
    // The editable Word document must frame a pleading as the PDF does:
    // a California pleading carries the 28-line numbered rail on the
    // exact 24pt pitch its body text sits on; a federal one carries none.
    for (jurisdiction, rail_numbers) in [("CA", 28), ("US", 0)] {
        let work = TempDir::new().unwrap();
        write_matter(&work);
        let src = write(&work, "motion.md", &pleading_fixture(jurisdiction));
        let out = work.path().join("motion.docx");
        let result = render_word(&[src.as_os_str(), "--out".as_ref(), out.as_ref()]);
        assert!(
            result.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let bytes = fs::read(&out).expect("DOCX written");
        let header = docx_part(&bytes, "word/header1.xml");
        assert_eq!(
            header.matches("<w:framePr ").count(),
            rail_numbers,
            "`jurisdiction: {jurisdiction}` rail"
        );
        if rail_numbers > 0 {
            assert!(header.contains("<w:t>1</w:t>") && header.contains("<w:t>28</w:t>"));
            assert!(!header.contains("<w:t>29</w:t>"));
            let styles = docx_part(&bytes, "word/styles.xml");
            assert!(styles.contains(r#"w:line="480" w:lineRule="exact""#));
        }
    }
}

#[test]
fn a_pleading_whose_jurisdiction_has_no_calibration_is_refused() {
    // `pleading::variant_for_jurisdiction` returns `None` for a real
    // jurisdiction the calibration table has not been extended to, and its
    // own docs are explicit that this is "a template that cannot render as
    // a pleading yet, not a reason to guess". Falling back to plain would
    // be exactly that guess, and it would be silent.
    let work = TempDir::new().unwrap();
    write_matter(&work);
    let src = write(&work, "motion.md", &pleading_fixture("CO"));
    let out = work.path().join("motion.pdf");
    let result = render_pdf(&[src.as_os_str(), "--out".as_ref(), out.as_ref()]);
    assert!(
        !result.status.success(),
        "an uncalibrated jurisdiction must refuse rather than render plain"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("no court-paper calibration"),
        "the refusal must say what is missing, got: {stderr}"
    );
    // It names the calibrations that do exist rather than echoing the
    // template's own `jurisdiction:` back — the actionable half, and it
    // keeps a value read straight from a parsed document out of the log.
    for calibrated in ["NV", "CA", "US"] {
        assert!(
            stderr.contains(calibrated),
            "the refusal must name `{calibrated}` as a supported jurisdiction, got: {stderr}"
        );
    }
    assert!(!out.exists(), "no PDF should be written on refusal");
}

/// The wholly synthetic California responses-to-requests-for-admission
/// pleading: a caption, a paper title, 23 request/response groups, and a
/// kept verification, filled from two granular answers.
const SYNTHETIC_RFA: &str = "tests/fixtures/synthetic_matter/templates/rfa_responses.md";
const SYNTHETIC_PLAINTIFF: &str = "SAMPLE SYNTHETIC HOLDINGS OF NORTHERN CALIFORNIA, A LIMITED \
                                   LIABILITY COMPANY, AND DOES 1 THROUGH 50, INCLUSIVE";

fn synthetic_rfa(subcommand: &str, out: &std::path::Path) -> std::process::Output {
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(SYNTHETIC_RFA);
    render(
        subcommand,
        &[
            source.as_os_str(),
            "--out".as_ref(),
            out.as_os_str(),
            "--answer".as_ref(),
            format!("person__client={SYNTHETIC_PLAINTIFF}").as_ref(),
            "--answer".as_ref(),
            "custom_datetime__verification_date=1 July 2026".as_ref(),
        ],
    )
}

#[test]
fn the_synthetic_ca_pleading_renders_a_complete_pdf_paper() {
    let work = TempDir::new().unwrap();
    let out = work.path().join("rfa.pdf");
    let result = synthetic_rfa("pdf", &out);
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let bytes = fs::read(&out).unwrap();
    pdf::validate_pdf(&bytes).expect("a safe PDF");
    let pages = pdf::page_count(&bytes).unwrap();
    assert!((5..=7).contains(&pages), "{pages} pages");

    // The caption prints once, from the matter, with no emphasis markup.
    assert_eq!(
        pdf::occurrence_count(&bytes, "Case No. 00CV000000").unwrap(),
        1
    );
    assert_eq!(pdf::occurrence_count(&bytes, "**").unwrap(), 0);
    // The long party names wrap in the caption without being clipped: the
    // plaintiff's and the second defendant's both end "INCLUSIVE", and the
    // body names the plaintiff once more from its answer.
    assert_eq!(pdf::occurrence_count(&bytes, "INCLUSIVE").unwrap(), 3);

    // Every page's footer carries the paper's title below the frame.
    let title = "RESPONSES TO DEFENDANT";
    let mut footer_pages = std::collections::BTreeSet::new();
    for ordinal in 1..=pdf::occurrence_count(&bytes, title).unwrap() {
        let hit = pdf::locate(&bytes, title, ordinal).unwrap();
        if hit.rects[0].rect.y * 792.0 > 744.0 {
            footer_pages.insert(hit.rects[0].page_index);
        }
    }
    assert_eq!(footer_pages, (0..pages).collect());

    // All 23 requests stay with their responses.
    for n in 1..=23 {
        let request = pdf::locate(&bytes, &format!("REQUEST FOR ADMISSION NO. {n}:"), 1).unwrap();
        let response = pdf::locate(
            &bytes,
            &format!("RESPONSE TO REQUEST FOR ADMISSION NO. {n}:"),
            1,
        )
        .unwrap();
        assert_eq!(
            request.rects[0].page_index, response.rects[0].page_index,
            "request {n} was separated from its response"
        );
    }
}

#[test]
fn the_synthetic_ca_pleading_renders_matching_word_paper() {
    let work = TempDir::new().unwrap();
    let out = work.path().join("rfa.docx");
    let result = synthetic_rfa("word", &out);
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let bytes = fs::read(&out).unwrap();
    let document = docx_part(&bytes, "word/document.xml");
    // Counsel block and caption, each a table; no markup leaks.
    assert_eq!(document.matches("<w:tbl>").count(), 2);
    assert!(document.contains("Case No. 00CV000000"));
    assert!(document.contains(SYNTHETIC_PLAINTIFF));
    assert!(!document.contains("**") && !document.contains("```"));
    // 23 request/response groups and the verification are each kept whole:
    // every paragraph in a group but the last stays with the next.
    assert!(document.matches("<w:keepLines/>").count() > 23 * 4);
    assert!(document.matches("<w:keepNext/>").count() > 23 * 3);
    // Rail, margins, the footer's number and abbreviated paper title.
    let header = docx_part(&bytes, "word/header1.xml");
    assert_eq!(header.matches("<w:framePr ").count(), 28);
    assert!(document.contains(r#"w:left="2160""#) && document.contains(r#"w:right="720""#));
    let footer = docx_part(&bytes, "word/footer1.xml");
    assert!(footer.contains(" PAGE ") && footer.contains("RESPONSES TO DEFENDANT"));
    assert!(footer.contains(r#"<w:sz w:val="20"/>"#));
}

/// A letter whose body is one substituted table. The parties are placeholders.
const SCHEDULE_LETTER: &str = "\
---
kind: letter
title: Party schedule
respondent_type: entity
confidential: true
output: letter
questionnaire:
  BEGIN:
    _: END
  END: {}
workflow:
  BEGIN:
    intake_submitted: lawyer_review
  lawyer_review:
    approved: END
    rejected: END
  END: {}
---

# Parties

{{schedule}}
";

/// `--answer` carries a Markdown table as one shell argument, with `\\n`
/// between rows and `<br>` inside a cell. Word and PDF both parse that into
/// a table, not a paragraph of pipes.
#[test]
fn a_substituted_markdown_table_renders_as_a_table_in_word_and_pdf() {
    let root = TempDir::new().unwrap();
    let src = write(&root, "schedule.md", SCHEDULE_LETTER);
    let answer = std::ffi::OsString::from(
        "schedule=| Party | Role |\\n| --- | --- |\\n| Alex Example | Claimant<br>care of agent |\\n| Blake Example | Respondent |",
    );

    let docx = root.path().join("schedule.docx");
    let word = render_word(&[
        src.as_os_str(),
        "--answer".as_ref(),
        answer.as_os_str(),
        "--out".as_ref(),
        docx.as_os_str(),
    ]);
    assert!(
        word.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&word.stderr)
    );
    let document = docx_part(&fs::read(&docx).unwrap(), "word/document.xml");
    assert!(
        document.contains("<w:tbl>"),
        "substituted table stayed prose: {document}"
    );
    assert!(document.contains("<w:br/>"), "{document}");
    assert!(document.contains("Alex Example"));
    assert!(document.contains("care of agent"));
    assert!(!document.contains('|'), "{document}");

    let pdf_out = root.path().join("schedule.pdf");
    let pdf = render_pdf(&[
        src.as_os_str(),
        "--answer".as_ref(),
        answer.as_os_str(),
        "--out".as_ref(),
        pdf_out.as_os_str(),
    ]);
    assert!(
        pdf.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&pdf.stderr)
    );
    let bytes = fs::read(&pdf_out).unwrap();
    assert_eq!(&bytes[..4], b"%PDF");
    assert_eq!(pdf::occurrence_count(&bytes, "Alex Example").unwrap(), 1);
    assert_eq!(pdf::occurrence_count(&bytes, "care of agent").unwrap(), 1);
    assert_eq!(pdf::occurrence_count(&bytes, "| --- |").unwrap(), 0);
}

/// LAW-123: a render is final unless `--preview` asks for the highlighted
/// draft. A final render refuses an unanswered placeholder by its
/// questionnaire state, in both formats, and writes nothing.
#[test]
fn a_final_render_refuses_an_unanswered_placeholder_and_preview_renders_it() {
    let work = TempDir::new().unwrap();
    let src = write(&work, "demand.md", VALID);
    for (subcommand, extension) in [("pdf", "pdf"), ("word", "docx")] {
        let out = work.path().join(format!("demand.{extension}"));
        let refused = render(
            subcommand,
            &[src.as_os_str(), "--out".as_ref(), out.as_os_str()],
        );
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert_eq!(refused.status.code(), Some(1), "{subcommand}: {stderr}");
        assert!(
            stderr.contains("answer the `amount` questionnaire state"),
            "{subcommand}: {stderr}"
        );
        assert!(
            !out.exists(),
            "{subcommand}: a refused render wrote {out:?}"
        );

        let preview = render(
            subcommand,
            &[
                src.as_os_str(),
                "--out".as_ref(),
                out.as_os_str(),
                "--preview".as_ref(),
            ],
        );
        assert!(
            preview.status.success(),
            "{subcommand}: {}",
            String::from_utf8_lossy(&preview.stderr)
        );
    }
}

/// A final render scans the filled text, so a drafting marker that arrives
/// through an answer refuses, naming it; the signature block's own blanks
/// pass. (A marker written into the template is `N131`'s, at validation.)
#[test]
fn a_final_render_refuses_drafting_markers_but_not_signature_blanks() {
    let work = TempDir::new().unwrap();
    let signed = VALID.replace(
        "- Second point\n",
        "- Second point\n\n## Signatures\n\nBy: ____________  Date: ________\n",
    );
    let src = write(&work, "demand.md", &signed);
    let out = work.path().join("demand.pdf");
    let with_answer = |amount: &str| {
        render_pdf(&[
            src.as_os_str(),
            "--out".as_ref(),
            out.as_os_str(),
            "--answer".as_ref(),
            format!("amount={amount}").as_ref(),
        ])
    };

    let refused = with_answer("[INSERT FEE]");
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert_eq!(refused.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("`[INSERT FEE]` is a drafting marker"),
        "{stderr}"
    );
    assert!(!stderr.contains("fill-in blank"), "{stderr}");

    let rendered = with_answer("5000 USD");
    assert!(
        rendered.status.success(),
        "{}",
        String::from_utf8_lossy(&rendered.stderr)
    );
}

/// LAW-141: a blank the signer completes by hand is a Signer Box — a state
/// declared `completed_by: signer` and referenced by its placeholder. Final
/// and preview renders agree: an unanswered box is an empty rule in the PDF
/// and the Word document, never a refusal and never a highlighted blank,
/// while an answered one prints its answer. A bare `____` in the prose is
/// still refused at validation.
#[test]
fn a_declared_signer_box_renders_as_a_rule_in_final_and_preview_alike() {
    let work = TempDir::new().unwrap();
    let boxed = VALID
        .replace(
            "questionnaire:\n  BEGIN:\n    _: END\n  END: {}\n",
            "questionnaire:\n  - state: custom_text__initials_l\n    prompt: Initial to grant (L).\n    \
             completed_by: signer\n  - state: custom_text__shares\n    prompt: Shares subscribed.\n    \
             completed_by: signer\n",
        )
        .replace(
            "- Second point\n",
            "- Second point\n- ({{custom_text__initials_l}}) (L) retirement benefit transactions;\n\n\
             Shares: {{custom_text__shares}}\n",
        );
    let src = write(&work, "poa.md", &boxed);
    for (subcommand, extension, preview) in [
        ("pdf", "pdf", false),
        ("pdf", "pdf", true),
        ("word", "docx", false),
    ] {
        let out = work.path().join(format!("poa-{preview}.{extension}"));
        let mut args = vec![
            src.as_os_str().to_os_string(),
            "--out".into(),
            out.as_os_str().to_os_string(),
            "--answer".into(),
            "amount=5000 USD".into(),
            "--answer".into(),
            "custom_text__shares=1,000".into(),
        ];
        if preview {
            args.push("--preview".into());
        }
        let refs: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsString::as_os_str).collect();
        let rendered = render(subcommand, &refs);
        assert!(
            rendered.status.success(),
            "{subcommand} preview={preview}: {}",
            String::from_utf8_lossy(&rendered.stderr)
        );
        assert!(out.exists(), "{subcommand} preview={preview} wrote nothing");
    }
    let document = docx_part(
        &fs::read(work.path().join("poa-false.docx")).unwrap(),
        "word/document.xml",
    );
    assert!(document.contains("<w:u w:val=\"single\"/>"), "{document}");
    assert!(document.contains("1,000"), "{document}");
    assert!(!document.contains("{{"), "{document}");

    // The same template with a `____` typed into the prose is refused
    // before anything renders, whatever the questionnaire declares.
    let bare = boxed.replace("({{custom_text__initials_l}})", "(____)");
    let src = write(&work, "poa-bare.md", &bare);
    let out = work.path().join("poa-bare.pdf");
    let refused = render_pdf(&[
        src.as_os_str(),
        "--out".as_ref(),
        out.as_os_str(),
        "--preview".as_ref(),
    ]);
    assert_eq!(refused.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&refused.stdout);
    assert!(stdout.contains("N131"), "{stdout}");
    assert!(!out.exists());
}

/// LAW-122: a pleading is final before it is filed. A certificate of service
/// whose service is not recorded refuses the render, naming the certificate;
/// once the date is answered it renders.
#[test]
fn a_pleading_with_an_incomplete_certificate_of_service_is_refused() {
    let work = TempDir::new().unwrap();
    write_matter(&work);
    let pleading = pleading_fixture("CA")
        .replace(
            "  BEGIN:\n    _: END\n  END: {}\nworkflow:",
            "  BEGIN:\n    _: custom_datetime__service_date\n  custom_datetime__service_date:\n    _: END\n  END: {}\nworkflow:",
        )
        .replace(
            "The request is proper under the governing rule.\n",
            "The request is proper under the governing rule.\n\n## 3. Certificate of Service\n\n\
             On {{custom_datetime__service_date}}, I served this motion on every party by electronic service.\n",
        );
    let src = write(&work, "motion.md", &pleading);
    let out = work.path().join("motion.pdf");

    let refused = render_pdf(&[src.as_os_str(), "--out".as_ref(), out.as_os_str()]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert_eq!(refused.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("the certificate of service is incomplete"),
        "{stderr}"
    );
    assert!(!out.exists());

    let served = render_pdf(&[
        src.as_os_str(),
        "--out".as_ref(),
        out.as_os_str(),
        "--answer".as_ref(),
        "custom_datetime__service_date=1 October 2026".as_ref(),
    ]);
    assert!(
        served.status.success(),
        "{}",
        String::from_utf8_lossy(&served.stderr)
    );
}

/// A resume carries no questionnaire, no workflow, and no `output:`: its
/// kind alone selects the dense, letterhead-free `resume` frame.
const VALID_RESUME: &str = "---
title: Resume
kind: resume
respondent_type: person
confidential: false
---

# Alex Example

alex@example.com · 555-0100

## Experience

- Led a synthetic workstream that delivered a measurable outcome.

## Education

J.D., Example School of Law, 2019
";

#[test]
fn a_resume_renders_on_its_own_frame_to_pdf_and_word() {
    let work = TempDir::new().unwrap();
    let src = write(&work, "resume.md", VALID_RESUME);
    let pdf_out = work.path().join("resume.pdf");
    let result = render_pdf(&[src.as_os_str(), "--out".as_ref(), pdf_out.as_os_str()]);
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("Resume"),
        "expected the resume frame: {stdout}"
    );
    assert_eq!(&fs::read(&pdf_out).expect("pdf written")[..4], b"%PDF");

    let word_out = work.path().join("resume.docx");
    let result = render_word(&[src.as_os_str(), "--out".as_ref(), word_out.as_os_str()]);
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        &fs::read(&word_out).expect("DOCX written")[..4],
        b"PK\x03\x04"
    );
}
