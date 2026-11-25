//! Shared local draft rendering, with authenticated runtime letterheads.
use crate::commands::{parse_answer, print_violation, severity_counts, strip_frontmatter};
use crate::{answers_file, palette, projects, remote};
use crate::{forms_render, rendering_letterhead as letterhead};
use chrono::TimeZone as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use model::question_registry::{choice_label, choice_labels_from_yaml as choice_map};

/// Render one notation template to PDF or editable Word (`pdf`/`word`,
/// `output_extension` `"pdf"`/`"docx"` respectively). Validates the file
/// against the notation rule set, resolves the render frame (`output:`
/// frontmatter → the `kind:`-derived default → plain), fills any
/// `{{code}}` placeholders from `answers`, and writes the compiled document
/// to `out`, which must carry the matching extension.
/// The render profile a template selects by declaring no `output:` and no
/// `kind:` with a frame of its own. Never a declarable `output:` value —
/// omitting the key is how a template selects it.
const PLAIN_PROFILE: &str = "plain";

/// The one profile `pdf::OutputFormat::parse` cannot construct from its
/// name, because its calibration comes from the template's
/// `jurisdiction:`. `run_render` resolves it itself.
const PLEADING_PROFILE: &str = "pleading";

fn rendered_answers(
    contents: &str,
    answers: &[(String, String)],
) -> std::collections::BTreeMap<String, String> {
    let choices = rules::frontmatter::extract(contents)
        .and_then(|fm| choice_map(fm).ok())
        .unwrap_or_default();
    answers
        .iter()
        .map(|(code, value)| {
            let display = choice_label(&choices, code, value).unwrap_or_else(|| value.clone());
            // A multiline answer is Markdown structure (a table, a list), not
            // one inline fact. Wrapping it in `**…**` would keep the pipes
            // and breaks from parsing as blocks.
            let rendered = if display.contains('\n') {
                display
            } else {
                pdf::markdown::bold_answer(&display)
            };
            (code.clone(), rendered)
        })
        .collect()
}

/// Run the validation gate for a render: print every violation and return
/// the exit code to stop with, or `None` when rendering may proceed.
fn render_validation_gate(
    file: &std::path::Path,
    contents: &str,
    preview: bool,
) -> Option<ExitCode> {
    // Gate on validation: render only when there are no blocking
    // (Error-severity) violations. Use the same DB-free classified rule
    // set as `validate`. Yellow advisories (e.g. N112, "step allowed but
    // not built yet") are printed but must not block rendering, mirroring
    // `validate` / `site seed`.
    let source = rules::SourceFile {
        path: file.to_path_buf(),
        contents: contents.to_string(),
    };
    let violations: Vec<rules::Violation> =
        rules::navigator_classified_rules_with_codes(&source, &[])
            .iter()
            .flat_map(|r| r.lint(&source))
            .collect();
    // A preview exists to show a draft before it is final, so the one rule
    // that refuses a draft for being a draft, N131 (drafting markers), is
    // reported but does not block it; the markers render as highlighted
    // flags instead. A final render keeps the refusal.
    let blocking = |v: &&rules::Violation| !(preview && v.code == "N131");
    let blocking_violations: Vec<rules::Violation> =
        violations.iter().filter(blocking).cloned().collect();
    let (error_count, _) = severity_counts(&blocking_violations);
    for v in &violations {
        print_violation(&v.path.display().to_string(), v.line, v.code, &v.message);
    }
    if error_count > 0 {
        eprintln!("navigator: {error_count} validation error(s); not rendering");
        return Some(ExitCode::from(1));
    }
    None
}

/// `hudson notation frame`: print the pleading frame a render would use.
#[must_use]
pub fn frame(file: &std::path::Path) -> ExitCode {
    let contents = match std::fs::read_to_string(file) {
        Ok(contents) => contents,
        Err(error) => {
            eprintln!(
                "navigator: could not read the template file: {}",
                error.kind()
            );
            return ExitCode::from(2);
        }
    };
    match crate::projects::matter::pleading_frame(file, &contents) {
        Ok(variant) => {
            println!("{}", variant.as_str());
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("navigator: {message}");
            ExitCode::from(2)
        }
    }
}

fn resolve_render_format(
    file: &std::path::Path,
    contents: &str,
    field: impl Fn(&str) -> Option<String>,
) -> Result<(String, pdf::OutputFormat), ExitCode> {
    let profile = field("output")
        .or_else(|| {
            field("kind")
                .and_then(|k| rules::Kind::parse(&k))
                .map(|k| k.default_output().to_string())
        })
        .unwrap_or_else(|| PLAIN_PROFILE.to_string());
    // `pleading` is the one profile a bare name cannot construct: its court
    // paper is chosen from the template's `pleading_frame:`, `jurisdiction:`
    // and the matter's court (LAW-435), fields `OutputFormat::parse` never
    // sees.
    let format = if profile == PLEADING_PROFILE {
        match crate::projects::matter::pleading_frame(file, contents) {
            Ok(variant) => pdf::OutputFormat::Pleading(variant),
            Err(message) => {
                eprintln!("navigator: {message}");
                return Err(ExitCode::from(2));
            }
        }
    } else {
        pdf::OutputFormat::parse(&profile).unwrap_or_default()
    };
    Ok((profile, format))
}

#[must_use]
pub fn run_render(
    file: &std::path::Path,
    out: &std::path::Path,
    answers: &[(String, String)],
    output_extension: &str,
    preview: bool,
    ci: bool,
) -> ExitCode {
    if !has_extension(out, output_extension) {
        eprintln!("navigator: output file extension does not match the render format");
        return ExitCode::from(2);
    }
    let contents = match std::fs::read_to_string(file) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("navigator: could not read the template file: {}", e.kind());
            return ExitCode::from(2);
        }
    };

    if let Some(code) = render_validation_gate(file, &contents, preview) {
        return code;
    }

    // Resolve the render frame from the document itself: the template's
    // own `output:` override first, else the default `Kind::default_output`
    // derives from its declared `kind:`, else plain.
    //
    // There is no third input. `--format` used to win over both, which
    // meant the frame was chosen twice and the outside choice silently
    // beat a correct header — `--format letter` on a will put the firm's
    // letterhead on an executed instrument with nothing to warn the
    // author (LAW-15). `output:` is now the only override, and it lives
    // in the document it frames.
    //
    // A template without a declared `kind:` or `output:` uses the plain frame.
    // N109 refuses an unknown output profile before rendering.
    let field = |key: &str| {
        rules::frontmatter::extract(&contents)
            .and_then(|fm| rules::frontmatter::field(fm, key))
            .filter(|value| !value.is_empty())
    };
    let (profile, format) = match resolve_render_format(file, &contents, field) {
        Ok(resolved) => resolved,
        Err(code) => return code,
    };

    // `form` is the AcroForm fill mode, not a Typst frame: it fills the
    // pinned blank from the vendored forms registry (or refuses loudly)
    // and never reaches the body renderer below. `Kind::Filing`'s
    // default output is this profile, so a `kind: filing` template that
    // declares no `output:` fills too — `kind:` is not a second silent
    // path into the old plain fallback.
    if profile == forms_render::FORM_PROFILE {
        return forms_render::run(file, &contents, out, answers, output_extension, ci);
    }

    // Body is everything after the frontmatter block; fill placeholders
    // through the same evaluator as preview and final document generation.
    // Bold each answer as it goes in, so a reader can find every fact
    // particular to their matter without reading the boilerplate. The other
    // half of the same idea lives in `pdf::markdown`: a placeholder that no
    // answer filled gets a yellow wash instead, so an unfinished document
    // is unmistakably unfinished.
    // A choice answer is *stored* as its declared key (`nevada`), but the
    // body interpolates it as prose ("the law of Nevada"). Resolve the key
    // back to its label through the template's own `choices:` /
    // `choices.<key>` frontmatter — the same map
    // `portal::retainer_walk::render_context_from_answers` resolves a
    // generated document against, so a preview renders the document the
    // matter will actually get rather than a second, differently-worded
    // one. A state with no declared options, or a value that is not one of
    // them, keeps its answer verbatim (`choice_label` returns `None`), so
    // free text is untouched.
    // An unanswered Signer Box prints as an empty rule for the signer's
    // hand, in the preview and the final render alike.
    let mut answer_context = rendered_answers(&contents, answers);
    // LAW-466: the Firm identity is resolved exactly once — one manifest
    // read, one Brand resolution — and the letterhead and the body's
    // `{{firm.legal_name}}` are two consumers of that one answer, so the
    // two surfaces cannot disagree.
    let Ok(identity) = resolve_firm_identity(file, &contents, answers, &format, ci)
        .map_err(|message| eprintln!("navigator: {message}"))
    else {
        return ExitCode::from(2);
    };
    fill_firm_legal_name(identity.legal_name, &mut answer_context);
    fill_firm_office(file, &contents, &mut answer_context);
    let Ok(body) = pleading_body(file, &contents, &profile, &answer_context, preview)
        .map_err(|message| eprintln!("navigator: {message}"))
    else {
        return ExitCode::from(2);
    };
    if !preview && !final_body_is_finished(&body) {
        return ExitCode::from(1);
    }

    // Every letter render names the letterhead it wore: which source
    // framed it, which revision of that source (LAW-266).
    eprintln!("{}", identity.provenance);
    for warning in &identity.warnings {
        eprintln!("navigator: {warning}");
    }
    let letterhead = identity.letterhead;
    // A memo is the Firm's analysis for its client, so every page of it
    // says it is privileged, whatever its author typed (LAW-224).
    let privileged = field("kind").as_deref() == Some("memo");
    let body = if preview {
        flag_open_markers(&body)
    } else {
        body
    };
    let format_debug = format!("{format:?}");
    let bytes = match render_notation_artifact(
        file,
        &body,
        contents.as_bytes(),
        output_extension,
        format,
        &letterhead,
        preview,
        privileged,
    ) {
        Ok(bytes) => bytes,
        Err(_error) => {
            eprintln!("navigator: render failed");
            return ExitCode::from(2);
        }
    };
    if let Err(e) = std::fs::write(out, &bytes) {
        eprintln!("navigator: write {}: {e}", out.display());
        return ExitCode::from(2);
    }
    println!(
        "{}",
        palette::dim(format!(
            "Rendered {} ({format_debug}, {} bytes) → {}",
            file.display(),
            bytes.len(),
            out.display()
        ))
    );
    ExitCode::SUCCESS
}

/// `notation word --redline-against`: render exactly as `notation word`
/// does, then turn the difference from the previous version into real Word
/// tracked changes ([`word::redline::against`]). The render stages in a
/// temporary file first, so a refusal never leaves an un-redlined document
/// sitting at `--out` looking final.
#[must_use]
pub fn run_word_redline(
    file: &std::path::Path,
    out: &std::path::Path,
    answers: &[(String, String)],
    preview: bool,
    previous: &std::path::Path,
    author: Option<&str>,
) -> ExitCode {
    let staging = match tempfile::Builder::new()
        .prefix("navigator-redline-")
        .suffix(".docx")
        .tempfile_in(out.parent().unwrap_or(std::path::Path::new(".")))
    {
        Ok(staging) => staging,
        Err(error) => {
            eprintln!("navigator: could not stage the render: {error}");
            return ExitCode::from(2);
        }
    };
    let code = run_render(file, staging.path(), answers, "docx", preview, false);
    if code != ExitCode::SUCCESS {
        return code;
    }
    let rendered = match std::fs::read(staging.path()) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("navigator: read the staged render: {error}");
            return ExitCode::from(2);
        }
    };
    let previous_bytes = match std::fs::read(previous) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("navigator: read {}: {error}", previous.display());
            return ExitCode::from(2);
        }
    };
    let author = match author {
        Some(name) => name.to_string(),
        None => match resolve_redline_author(file) {
            Ok(name) => name,
            Err(message) => {
                eprintln!("navigator: {message}");
                return ExitCode::from(2);
            }
        },
    };
    let redlined = match word::redline::against(&rendered, &previous_bytes, &author) {
        Ok(redline) => redline,
        Err(error) => {
            eprintln!("navigator: {error}");
            return ExitCode::from(2);
        }
    };
    if let Err(error) = std::fs::write(out, &redlined.bytes) {
        eprintln!("navigator: write {}: {error}", out.display());
        return ExitCode::from(2);
    }
    let counts = redlined.counts;
    if counts.total() == 0 {
        println!(
            "{}",
            palette::dim("No differences from the previous version; the render is unchanged.")
        );
    } else {
        println!(
            "{}",
            palette::dim(format!(
                "Redlined {} insertion(s), {} deletion(s), {} rewrite(s), authored by \
                 {author} → {}",
                counts.insertions,
                counts.deletions,
                counts.replacements,
                out.display()
            ))
        );
    }
    ExitCode::SUCCESS
}

/// The signing lawyer's name, for the revision author of every tracked
/// change a redline writes. The same source the pleading `/s/` line signs
/// with: `matter.counsel.name` in the Project's manifest.
///
/// # Errors
///
/// When the manifest is missing or malformed, naming both outs — the flag
/// and the manifest key.
fn resolve_redline_author(file: &std::path::Path) -> Result<String, String> {
    projects::matter::counsel_name(file).map_err(|error| {
        format!(
            "a redline records who made each change: pass --redline-author, or add \
             `matter.counsel.name` to navigator.yaml ({error})"
        )
    })
}

/// Final documents only: the filled text carries nothing a person is still
/// meant to complete, and no draft counsel signature. Missing facts route to
/// the questionnaire state that answers them. Prints each refusal.
fn final_body_is_finished(body: &str) -> bool {
    if word::paper::has_draft_counsel_signature(body) {
        eprintln!(
            "navigator: a counsel signature is signed from the Project's `matter.counsel.name`, \
             which only a pleading reads; render this draft with `--preview`"
        );
        return false;
    }
    let unfinished = rules::final_document::scan(body);
    if unfinished.is_empty() {
        return true;
    }
    for finding in &unfinished {
        eprintln!("navigator: rendered {finding}");
    }
    eprintln!(
        "navigator: a final render carries no unfinished text; answer the questionnaire, \
         or pass `--preview` for a highlighted draft"
    );
    false
}

/// Binds `firm.legal_name` to the Firm legal name the render already
/// resolved (LAW-466), unless the answers already supply one.
fn fill_firm_legal_name(
    legal_name: Option<String>,
    answer_context: &mut std::collections::BTreeMap<String, String>,
) {
    if let Some(name) = legal_name {
        answer_context
            .entry("firm.legal_name".to_string())
            .or_insert_with(|| pdf::markdown::bold_answer(&name));
    }
}

/// Binds `{{firm.office.address}}`, `{{firm.office.phone}}`, and
/// `{{firm.office.email}}` to the Firm's office in the template's
/// `jurisdiction:` (LAW-329), unless the answers already supply them, and
/// says on stderr which office it used — a fallback to the default office
/// is never silent. A template that names none of them reads nothing.
fn fill_firm_office(
    file: &Path,
    contents: &str,
    answer_context: &mut std::collections::BTreeMap<String, String>,
) {
    if !contents.contains("firm.office.") {
        return;
    }
    let jurisdiction = projects::matter::jurisdiction_of(contents);
    let office = match projects::matter::find_manifest(file) {
        Some(path) => std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| projects::manifest::parse(&text).ok())
            .and_then(|manifest| {
                crate::letterhead::office_for_project(
                    manifest.brand.as_deref(),
                    manifest.host.as_deref(),
                    &jurisdiction,
                )
                .ok()
                .flatten()
            }),
        None => crate::letterhead::office_for_brand(views::brand::BrandKey::Neon, &jurisdiction),
    };
    let Some(office) = office else {
        eprintln!(
            "navigator: the Project's Brand publishes no office; {{{{firm.office.*}}}} stays open"
        );
        return;
    };
    if office.fallback {
        eprintln!(
            "navigator: the Firm has no office in this template's jurisdiction; \
             {{{{firm.office.*}}}} is filled from its default office ({})",
            office.state
        );
    } else {
        eprintln!(
            "navigator: {{{{firm.office.*}}}} is filled from the Firm's {} office",
            office.state
        );
    }
    for (key, value) in [
        ("firm.office.address", office.address.join(", ")),
        ("firm.office.phone", office.phone),
        ("firm.office.email", office.email),
    ] {
        answer_context
            .entry(key.to_string())
            .or_insert_with(|| pdf::markdown::bold_answer(&value));
    }
}

/// The Firm identity one render wears (LAW-466): the frame it is drawn in
/// and the legal name its body binds, resolved together.
struct FirmIdentity {
    /// The frame the document is drawn in.
    letterhead: pdf::Letterhead,
    /// The provenance line the render reports beside the letterhead.
    provenance: String,
    /// Non-fatal degradations the render announces on stderr.
    warnings: Vec<String>,
    /// The resolved Brand's Firm legal name, as the body's
    /// `{{firm.legal_name}}` prints it. `None` when nothing resolves: the
    /// placeholder then washes in a preview or refuses a final render, the
    /// fallback the body always had.
    legal_name: Option<String>,
}

/// The Brand's legal name a compiled letterhead carries, trimmed of
/// padding; `None` when the Brand names none.
fn compiled_legal_name(letterhead: &pdf::Letterhead) -> Option<String> {
    let name = letterhead.entity.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Resolve the Project's identity exactly once (LAW-466): one manifest
/// read and one Brand resolution per render, returning the letterhead the
/// document wears and the legal name its body binds, so the two surfaces
/// cannot disagree.
///
/// A `Letter` frame wears the Project's letterhead — the runtime row the
/// deployment serves when it answers, the compiled registry when it
/// cannot (LAW-266) — and its `entity` line is the body's
/// `{{firm.legal_name}}`. Every other frame wears no letterhead, but its
/// body still binds the name, from the same manifest read. A template
/// outside any Project names the default Brand's Firm.
///
/// An explicit `firm.legal_name` answer wins on both surfaces at once
/// (LAW-466): a letter may name a matter-specific signing entity, but its
/// letterhead then names the same entity, so the two can never split.
///
/// The fallback asymmetry is deliberate, not unified: an unresolvable
/// Brand refuses a letter (the letterhead would name the wrong firm) and
/// leaves any other frame nameless (the placeholder washes or the final
/// render refuses, as it already did).
///
/// # Errors
/// A letter whose manifest cannot be read or parsed, or whose Brand does
/// not resolve, or whose letterhead row cannot be read under `--ci`.
fn resolve_firm_identity(
    file: &Path,
    contents: &str,
    answers: &[(String, String)],
    format: &pdf::OutputFormat,
    ci: bool,
) -> Result<FirmIdentity, String> {
    let letter = matches!(format, pdf::OutputFormat::Letter(_));
    let manifest = match projects::matter::find_manifest(file) {
        Some(path) => match std::fs::read_to_string(&path)
            .map_err(|error| format!("read {}: {error}", projects::manifest::FILE))
            .and_then(|text| projects::manifest::parse(&text))
        {
            Ok(manifest) => Some(manifest),
            // A letter refuses; any other frame renders nameless.
            Err(message) if letter => return Err(message),
            Err(_) => None,
        },
        None => None,
    };
    let mut identity = match manifest.as_ref() {
        Some(manifest) if letter => {
            let key =
                letterhead::resolve_brand(manifest.brand.as_deref(), manifest.host.as_deref())
                    .map_err(|error| error.to_string())?;
            let mut rendered = attempt_runtime_letterhead(key, ci)?;
            if key == views::brand::BrandKey::GuiebShook {
                let template_jurisdiction = projects::matter::jurisdiction_of(contents);
                let jurisdiction = manifest
                    .jurisdiction
                    .as_deref()
                    .unwrap_or(&template_jurisdiction);
                letterhead::address_guieb_shook_letterhead(&mut rendered.letterhead, jurisdiction);
            }
            FirmIdentity {
                legal_name: compiled_legal_name(&rendered.letterhead),
                letterhead: rendered.letterhead,
                provenance: rendered.provenance,
                warnings: rendered.warnings,
            }
        }
        // No frame to wear, but the body still binds the name, from the
        // same one manifest read.
        nameless => {
            let compiled = match nameless {
                Some(manifest) => {
                    letterhead::for_project(manifest.brand.as_deref(), manifest.host.as_deref())
                }
                None => letterhead::for_brand(views::brand::BrandKey::Neon),
            };
            FirmIdentity {
                letterhead: pdf::Letterhead::default(),
                provenance: String::new(),
                warnings: Vec::new(),
                legal_name: compiled.ok().as_ref().and_then(compiled_legal_name),
            }
        }
    };
    if let Some((_, answered)) = answers.iter().find(|(key, _)| key == "firm.legal_name") {
        identity.letterhead.entity.clone_from(answered);
    }
    Ok(identity)
}

/// The letterhead resolution's intermediate: the frame a render wears,
/// with the provenance the render reports beside it and the degradations
/// it announces. `resolve_firm_identity` (LAW-466) is the one caller that
/// turns this into a render's identity.
struct ProjectLetterhead {
    letterhead: pdf::Letterhead,
    provenance: String,
    warnings: Vec<String>,
}

/// The runtime source, tried within a bounded budget so a hung deployment
/// cannot hang the render. Any failure — no session, connect, read, or
/// budget — falls back to the compiled registry, announced; under `--ci`
/// the same failure refuses, because a CI render's bytes must not depend
/// on which answer a live read would have given (LAW-19).
fn attempt_runtime_letterhead(
    key: views::brand::BrandKey,
    ci: bool,
) -> Result<ProjectLetterhead, String> {
    let budget = std::time::Duration::from_secs(3);
    // The timeout's timer is built under the runtime, not as a `block_on`
    // argument — `tokio::time::timeout` constructs its `Sleep` eagerly, and
    // eager construction needs the time driver it is about to run under.
    let attempted = tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(async move { tokio::time::timeout(budget, runtime_letterhead(key, ci)).await });
    match attempted {
        Ok(Ok(rendered)) => Ok(rendered),
        Ok(Err(reason)) => letterhead_failure(key, &reason, ci),
        Err(elapsed) => letterhead_failure(
            key,
            &format!(
                "the deployment's letterhead door did not answer within {budget:?}: {elapsed:?}"
            ),
            ci,
        ),
    }
}

/// A letterhead read that could not answer: under `--ci` it refuses — a CI
/// render must render pinned bytes or not at all — and anywhere else it
/// falls back to the compiled registry, announced on stderr.
fn letterhead_failure(
    key: views::brand::BrandKey,
    reason: &str,
    ci: bool,
) -> Result<ProjectLetterhead, String> {
    if ci {
        return Err(format!(
            "--ci refuses a letter whose letterhead row could not be read from the deployment \
             ({reason}); warm the letterhead cache with a non---ci render, or repair the \
             `site login` session, before rendering under --ci"
        ));
    }
    compiled_fallback(key, reason)
}

/// Test-harness override for the letterhead door's base URL: points the
/// row read at a double instead of the Brand's signed-in deployment. The
/// double serves the row itself, so no `site login` session is resolved.
pub const LETTERHEAD_API_BASE_ENV: &str = "NAVIGATOR_LETTERHEAD_API_BASE";

/// The deployment base and bearer the letterhead row is read with: the
/// Brand's own host, as `navigator site login` stored the lawyer's
/// session. The `NAVIGATOR_LETTERHEAD_API_BASE` override points the read
/// at a test double or harness endpoint instead, and skips the session —
/// such an endpoint serves the row itself.
fn letterhead_endpoint(key: views::brand::BrandKey) -> Result<(String, Option<String>), String> {
    if let Some(base) = std::env::var_os(LETTERHEAD_API_BASE_ENV).filter(|v| !v.is_empty()) {
        return Ok((base.to_string_lossy().into_owned(), None));
    }
    let compiled = letterhead::for_brand(key).map_err(|error| error.to_string())?;
    match remote::matching_stored_session(&compiled.web)
        .map_err(|error| format!("the `site login` session could not be read: {error}"))?
    {
        remote::MatchingSession::Valid(base, token) => Ok((base, Some(token))),
        remote::MatchingSession::Absent => Err(format!(
            "no `navigator site login` session for {} — run `navigator site login --host {}`",
            compiled.web,
            key.apex().unwrap_or_else(|| key.as_str())
        )),
        remote::MatchingSession::Expired => Err(format!(
            "the `site login` session for {} has expired — run `navigator site login --host {}`",
            compiled.web,
            key.apex().unwrap_or_else(|| key.as_str())
        )),
    }
}

/// The Brand's letterhead from the signed-in deployment's letterhead door —
/// the same rows `/app/admin/brands` edits, served by
/// `GET /app/api/brands/{key}/letterhead` with the `site login` bearer
/// (LAW-341). `Err` names why the door could not answer; the caller falls
/// back to the compiled registry and announces it.
async fn runtime_letterhead(
    key: views::brand::BrandKey,
    ci: bool,
) -> Result<ProjectLetterhead, String> {
    let (base, token) = letterhead_endpoint(key)?;
    let runtime = letterhead::for_brand_runtime(key, &base, token.as_deref(), ci)
        .await
        .map_err(|error| error.to_string())?;
    let Some(runtime) = runtime else {
        // A Brand with no runtime row wears its compiled frame — the
        // compiled mark included — but the render still names the source.
        return compiled_letterhead(key);
    };
    let mark = match runtime.letterhead.mark {
        pdf::LetterheadMark::Png(_) => "logo",
        pdf::LetterheadMark::Wordmark => "wordmark",
        pdf::LetterheadMark::Compiled | pdf::LetterheadMark::GuiebShook => "compiled",
    };
    let revision = runtime
        .revision
        .clone()
        .unwrap_or_else(|| "n/a".to_string());
    let mut provenance = format!(
        "letterhead: source=runtime brand={} revision={revision} mark={mark}",
        key.as_str()
    );
    if let Some(font) = runtime.font_object_key.as_deref() {
        provenance.push_str(" font=");
        provenance.push_str(provenance_font(font));
    }
    Ok(ProjectLetterhead {
        provenance,
        letterhead: runtime.letterhead,
        warnings: runtime.warnings,
    })
}

/// The compiled frame for `key`, with its provenance line.
fn compiled_letterhead(key: views::brand::BrandKey) -> Result<ProjectLetterhead, String> {
    let compiled = letterhead::for_brand(key).map_err(|error| error.to_string())?;
    let mark = match compiled.mark {
        pdf::LetterheadMark::Compiled | pdf::LetterheadMark::GuiebShook => "compiled",
        _ => "wordmark",
    };
    Ok(ProjectLetterhead {
        provenance: format!(
            "letterhead: source=compiled brand={} revision=n/a mark={mark}",
            key.as_str()
        ),
        letterhead: compiled,
        warnings: Vec::new(),
    })
}

/// The compiled fallback, announced: a plain connectivity miss renders on
/// the compiled registry rather than refusing, and says so.
fn compiled_fallback(
    key: views::brand::BrandKey,
    reason: &str,
) -> Result<ProjectLetterhead, String> {
    let mut compiled = compiled_letterhead(key)?;
    compiled.warnings.push(format!(
        "letterhead: source=compiled brand={} revision=n/a (deployment unreachable: {reason}; \
         using compiled registry — re-render may differ from admin-set letterhead)",
        key.as_str()
    ));
    Ok(compiled)
}

/// A font object's bare file name for the provenance line.
fn provenance_font(font: &str) -> &str {
    font.rsplit('/').next().unwrap_or(font)
}

/// A preview's drafting markers, rewritten as visible flags (LAW-183): each
/// bracketed marker such as `[CONFIRM: amount and purpose]` becomes
/// `«OPEN: amount and purpose»`, which the PDF render washes yellow.
fn flag_open_markers(body: &str) -> String {
    let mut flagged = body.to_string();
    for finding in rules::final_document::drafting_markers(body) {
        if let rules::final_document::Marker::Bracket(text) = finding.marker {
            let inner = text.trim_start_matches('[').trim_end_matches(']').trim();
            let inner = inner
                .strip_prefix("CONFIRM:")
                .map_or(inner, str::trim_start);
            flagged = flagged.replace(
                &text,
                &format!(
                    "{}OPEN: {inner}{}",
                    pdf::markdown::OPEN_FLAG_START,
                    pdf::markdown::OPEN_FLAG_END
                ),
            );
        }
    }
    flagged
}

/// The Signer Box states a template declares, or none: the validation gate
/// above already refused a frontmatter that does not parse.
fn declared_signer_boxes(contents: &str) -> Vec<String> {
    rules::frontmatter::extract(contents)
        .and_then(|frontmatter| rules::signer_boxes(frontmatter).ok())
        .unwrap_or_default()
}

/// The filled body: every answer substituted and each unanswered Signer Box
/// printed as the rule marker, in the preview and the final render alike. A
/// pleading's caption and footer title are composed from the Project's
/// `matter:` and the notation's frontmatter, never from its body, so both
/// renders print one caption that a manifest edit changes everywhere. Its
/// certificate of service is composed the same way, from the Service
/// records on the pointer its `served_document:` names: a preview omits the
/// certificate while none is recorded, and a final render is refused. Any
/// other profile's body passes through.
fn pleading_body(
    file: &Path,
    contents: &str,
    profile: &str,
    answers: &std::collections::BTreeMap<String, String>,
    preview: bool,
) -> Result<String, String> {
    let composed = rules::clauses::compile(contents)?;
    let contents = composed.as_str();
    let signer_boxes = declared_signer_boxes(contents);
    // Each `{{<signer>.signature|initials|date}}` becomes a signing line with
    // the provider's anchor hidden on it, in every frame (LAW-344).
    let body = word::paper::mark_signing_lines(&views::notation::fill_signer_boxes(
        strip_frontmatter(contents),
        answers,
        signer_boxes.iter().map(String::as_str),
    ));
    if profile != PLEADING_PROFILE {
        return Ok(body);
    }
    let caption = if preview {
        projects::matter::caption_for_preview(file, contents)?
    } else {
        projects::matter::caption_for_final(file, contents)?
    };
    let certificate = projects::service::certificate_for(file, contents, preview)?
        .map(|certificate| format!("\n\n{certificate}"))
        .unwrap_or_default();
    // A draft's counsel signature prints `/s/ DRAFT`; a final render signs
    // it `/s/ <counsel>` from `matter.counsel.name`.
    let body = pleading_signature_dates(&body, file, preview)?;
    let body = if preview {
        body
    } else {
        word::paper::sign_counsel_signatures(&body, &projects::matter::counsel_name(file)?)
    };
    Ok(format!("{caption}{body}{certificate}"))
}

/// The token a preview's date line carries while `matter.signed_on` is
/// unset: an unfilled placeholder, which both renders wash yellow. It is
/// inserted at fill time, so it never sits in the template for `F115` to
/// flag, and a final render — which never inserts it — never carries it.
pub const UNSIGNED_DATE_PLACEHOLDER: &str = "{{matter.signed_on}}";

/// Insert the signature block's date line (LAW-242): `Dated this …`,
/// sourced only from `matter.signed_on`, above every counsel signature
/// block. A preview with no `signed_on:` prints the highlighted
/// [`UNSIGNED_DATE_PLACEHOLDER`]; a final render with none is refused — an
/// undated signature page is not filable.
///
/// # Errors
///
/// No manifest above the pleading, a `signed_on:` that is not a real
/// calendar date, or a final pleading whose signature block has no
/// `signed_on:` to date it.
fn pleading_signature_dates(body: &str, file: &Path, preview: bool) -> Result<String, String> {
    if !word::paper::has_counsel_signature(body) {
        return Ok(body.to_string());
    }
    let line = match projects::matter::signed_on(file)? {
        Some(date) => word::matter::date_line(&date)
            .ok_or_else(|| format!("`matter.signed_on` is not a calendar date: {date}"))?,
        None if preview => UNSIGNED_DATE_PLACEHOLDER.to_string(),
        // LAW-242: a final pleading never prints an undated signature page.
        None => {
            return Err(
                "this final pleading has a signature block, but its `matter:` block declares \
                 no `signed_on:` signing date; set it before final export"
                    .to_string(),
            )
        }
    };
    Ok(word::paper::insert_before_counsel_signatures(body, &line))
}

#[must_use]
fn has_extension(out: &Path, expected: &str) -> bool {
    out.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
}

#[allow(clippy::too_many_arguments)]
fn render_notation_artifact(
    file: &Path,
    body: &str,
    template_bytes: &[u8],
    output_extension: &str,
    format: pdf::OutputFormat,
    letterhead: &pdf::Letterhead,
    preview: bool,
    privileged: bool,
) -> Result<Vec<u8>, String> {
    if output_extension == "docx" {
        let word_letterhead =
            matches!(&format, pdf::OutputFormat::Letter(_)).then(|| word::RenderLetterhead {
                name: &letterhead.name,
                entity: &letterhead.entity,
                phone: &letterhead.phone,
                email: &letterhead.email,
                web: &letterhead.web,
                postal_address: &letterhead.postal_address,
                font_family: &letterhead.font_family,
                wide_wordmark: letterhead.mark == pdf::LetterheadMark::GuiebShook,
                logo_png: match &letterhead.mark {
                    pdf::LetterheadMark::Png(bytes) => bytes.as_slice(),
                    pdf::LetterheadMark::Compiled => pdf::firm_logo_png(),
                    pdf::LetterheadMark::GuiebShook => pdf::guieb_shook_wordmark_png(),
                    pdf::LetterheadMark::Wordmark => &[],
                },
            });
        // A pleading's Word render takes the same calibrated court-paper
        // geometry as its PDF, so both carry the same frame and rail.
        let pleading = match &format {
            pdf::OutputFormat::Pleading(variant) => Some(variant.word_frame()),
            _ => None,
        };
        let source_revision = notation_render_revision(file);
        // Every Word render is a draft a lawyer may edit and import back,
        // so it embeds the export provenance the import verifies: the
        // template's file name and the hash of the template's bytes at
        // export time. The name — not the full path — keeps renders
        // byte-identical wherever the template lives; `notation import`
        // takes the template path explicitly and verifies it by hash.
        let template_name = file.file_name().map_or_else(
            || file.to_string_lossy().into_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        let template_hash = word::template_hash(template_bytes);
        let provenance = word::Provenance {
            template_path: template_name.as_str(),
            template_hash: template_hash.as_str(),
        };
        return word::render_notation(
            body,
            word_letterhead.as_ref(),
            &word::RenderOptions {
                source_revision: source_revision.as_deref(),
                pleading,
                provenance: Some(provenance),
                preview,
            },
        )
        .map_err(|error| format!("render Word document: {error}"));
    }

    pdf::render_document_with_options(
        body,
        format,
        letterhead,
        &pdf::RenderOptions {
            creation_timestamp: notation_render_timestamp(file),
            preview,
            privileged,
        },
    )
    .map_err(|error| format!("render {}: {error}", file.display()))
}

/// One `git log --format` field of the latest commit that touched `file`,
/// or `None` for a file with no Git history.
fn latest_commit_field(file: &Path, format: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(file.parent()?)
        .args(["log", "-1", &format!("--format={format}"), "--"])
        .arg(file.file_name()?)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|field| !field.is_empty())
}

fn notation_render_revision(file: &Path) -> Option<String> {
    latest_commit_field(file, "%H")
}

/// The creation timestamp a rendered artifact carries: the latest commit
/// timestamp of a tracked file, so the artifact follows its source's own
/// history, and the Unix epoch for a file outside Git, which still needs a
/// stable artifact rather than the wall clock.
#[must_use]
fn notation_render_timestamp(file: &Path) -> chrono::DateTime<chrono::Utc> {
    latest_commit_field(file, "%ct")
        .and_then(|seconds| seconds.parse::<i64>().ok())
        .and_then(|seconds| chrono::Utc.timestamp_opt(seconds, 0).single())
        .unwrap_or(chrono::DateTime::UNIX_EPOCH)
}

#[derive(clap::Subcommand)]
pub enum RenderCmd {
    /// Render an uncommitted draft to a PDF without creating a Notation or workflow.
    Preview {
        file: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long = "answer", value_parser = parse_answer)]
        answers: Vec<(String, String)>,
        #[arg(long)]
        answers_file: Option<PathBuf>,
        #[arg(long)]
        no_open: bool,
    },
    /// List every `kind:` and `output:` value a notation may declare, with
    /// the frame each gets (letterhead, plain, contract, pleading, form).
    Kinds,
    /// Render a single notation template to PDF, framed by the render
    /// profile its declared `kind:` selects.
    ///
    /// The file is validated against the same notation rule set as
    /// `validate` first — a template with any violation is refused.
    ///
    /// A notation already declares what it is, and that is enough to pick
    /// the frame: `Kind::default_output` derives it from `kind:`, so a
    /// `letter` gets letterhead and a `will` gets the unadorned
    /// instrument. A template's own `output:` frontmatter field remains
    /// the deliberate override for the kinds that legitimately render two
    /// ways, and omitting `output:` is how a template selects the plain
    /// frame. There is no `--format` flag: it chose the frame a second
    /// time, from outside the document, and `--format letter` passed out
    /// of habit put the firm's letterhead on an executed instrument with
    /// no warning (LAW-15).
    ///
    /// A `kind: pleading` template is the one case that needs a second
    /// field: court geometry is calibrated by its `jurisdiction:`. That is
    /// resolved here, and a jurisdiction with no calibration is refused
    /// rather than quietly rendered on the plain frame.
    ///
    /// Compiled in pure Rust (no shell-out). Every render is final unless
    /// `--preview` is passed: a document that leaves Navigator carries no
    /// unanswered `{{placeholder}}` (fill one with `--answer code=value`),
    /// bracketed drafting marker, stray fill-in blank, or drafting note.
    #[command(after_long_help = crate::notation_kinds::RENDER_FRAME_HELP)]
    Pdf {
        /// Path to the notation template (`.md`).
        file: PathBuf,
        /// Where to write the rendered PDF. Must end in `.pdf`.
        #[arg(long)]
        out: PathBuf,
        /// Fill a `{{code}}` placeholder with `value`. Repeatable:
        /// `--answer counterparty_legal_name="NEON GmbH"`.
        #[arg(long = "answer", value_parser = parse_answer)]
        answers: Vec<(String, String)>,
        /// Load questionnaire Answers from a local YAML file.
        #[arg(long)]
        answers_file: Option<PathBuf>,
        /// Reuse Answers from the latest Notation for this Draft or a Notation UUID.
        #[arg(long, conflicts_with = "answers_file")]
        answers_from: Option<String>,
        /// Render a draft for review: unanswered placeholders survive,
        /// highlighted. Without it the render is final and refuses any
        /// unanswered placeholder, bracketed drafting marker, fill-in blank
        /// outside a signature block, or drafting note.
        #[arg(long)]
        preview: bool,
        /// Refuse to fetch render inputs over the network: render only
        /// from bytes already pinned in the checkout or the local blank
        /// cache (LAW-19). A form template whose blank is neither vendored
        /// nor cached is a loud refusal, never a silent download that
        /// could change the bytes between two runs.
        #[arg(long)]
        ci: bool,
    },
    /// Render a single notation template to editable Word, framed the same
    /// way as `pdf` — see its documentation for how the frame, jurisdiction,
    /// and placeholders resolve.
    #[command(after_long_help = crate::notation_kinds::RENDER_FRAME_HELP)]
    Word {
        /// Path to the notation template (`.md`).
        file: PathBuf,
        /// Where to write the rendered Word document. Must end in `.docx`.
        #[arg(long)]
        out: PathBuf,
        /// Fill a `{{code}}` placeholder with `value`. Repeatable:
        /// `--answer counterparty_legal_name="NEON GmbH"`.
        #[arg(long = "answer", value_parser = parse_answer)]
        answers: Vec<(String, String)>,
        /// Render a draft for review: unanswered placeholders survive,
        /// highlighted. Without it the render is final and refuses any
        /// unanswered placeholder, bracketed drafting marker, fill-in blank
        /// outside a signature block, or drafting note.
        #[arg(long)]
        preview: bool,
        /// Write the render as a **redline** against this previous `.docx`:
        /// every difference from it becomes a real Word tracked change
        /// (`w:ins`/`w:del`) in the output, for the lawyer to accept or
        /// reject in Word before the version is delivered. The previous
        /// document must be a fresh `notation word` export carrying no
        /// unresolved revisions or review comments of its own. Pairs with
        /// `notation import`, which still refuses any document carrying the
        /// revisions this writes — a redline is for review, never for
        /// splicing.
        #[arg(long, value_name = "PREVIOUS.docx")]
        redline_against: Option<PathBuf>,
        /// The name the tracked changes record as their author. Defaults to
        /// `matter.counsel.name` in `navigator.yaml` — the same name the
        /// `/s/` signature line signs with.
        #[arg(long, value_name = "NAME")]
        redline_author: Option<String>,
    },
    /// Apply an edited Word document back into the notation template it was
    /// rendered from — the second half of `notation word`.
    ///
    /// The edited `.docx` carries the export's provenance record; the
    /// import verifies the template hash and refuses a template that
    /// changed since the export, refuses drafts still carrying tracked
    /// changes or review comments, and refuses any edit that drops a
    /// `{{placeholder}}` or `{{> partial}}` the template needs or invents
    /// one it never had. With `--dry-run` it prints the block-level
    /// diff without writing.
    Import {
        /// Path to the notation template (`.md`) the document was rendered
        /// from. Updated in place without `--dry-run`.
        file: PathBuf,
        /// Path to the edited Word document (`navigator notation word`'s
        /// output, edited in Word and re-saved). Must end in `.docx`.
        edited: PathBuf,
        /// Print the block-level diff without writing.
        #[arg(long)]
        dry_run: bool,
    },
}

/// Run the local notation rendering and authoring commands.
#[must_use]
pub fn run(action: RenderCmd) -> ExitCode {
    let runtime = || tokio::runtime::Runtime::new().expect("tokio runtime");
    match action {
        RenderCmd::Preview {
            file,
            out,
            answers,
            answers_file,
            no_open,
        } => {
            let out = out.unwrap_or_else(|| {
                std::env::temp_dir().join(format!("navigator-preview-{}.pdf", uuid::Uuid::now_v7()))
            });
            let answers = match answers_file {
                Some(path) => match answers_file::pdf_answers(&path, answers) {
                    Ok(answers) => answers,
                    Err(error) => {
                        eprintln!("navigator: {error:#}");
                        return ExitCode::FAILURE;
                    }
                },
                None => answers,
            };
            let result = run_render(&file, &out, &answers, "pdf", true, false);
            if result == ExitCode::SUCCESS && !no_open {
                crate::login::open_in_browser(out.to_string_lossy().as_ref());
            }
            result
        }
        RenderCmd::Kinds => crate::notation_kinds::run(),
        RenderCmd::Pdf {
            file,
            out,
            answers,
            answers_file,
            answers_from,
            preview,
            ci,
        } => {
            let answers = match answers_file {
                Some(path) => match answers_file::pdf_answers(&path, answers) {
                    Ok(answers) => answers,
                    Err(error) => {
                        eprintln!("navigator: {error:#}");
                        return ExitCode::from(2);
                    }
                },
                None => match runtime().block_on(remote::pdf_project_answers(
                    &file,
                    answers_from.as_deref(),
                    answers,
                )) {
                    Ok(answers) => answers,
                    Err(error) => {
                        eprintln!("navigator: {error:#}");
                        return ExitCode::from(2);
                    }
                },
            };
            run_render(&file, &out, &answers, "pdf", preview, ci)
        }
        RenderCmd::Word {
            file,
            out,
            answers,
            preview,
            redline_against,
            redline_author,
        } => match redline_against {
            Some(previous) => run_word_redline(
                &file,
                &out,
                &answers,
                preview,
                &previous,
                redline_author.as_deref(),
            ),
            None => run_render(&file, &out, &answers, "docx", preview, false),
        },
        RenderCmd::Import {
            file,
            edited,
            dry_run,
        } => crate::notation_import::run(&file, &edited, dry_run),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notation_render_timestamp_uses_source_commit_or_fixed_fallback() {
        let repository = tempfile::tempdir().expect("temporary repository");
        let source = repository.path().join("notation.md");
        std::fs::write(&source, "source").expect("write source");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(repository.path())
                .args(args)
                .output()
                .expect("git runs");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "--quiet", "--initial-branch=main"]);
        git(&["config", "user.email", "notation-render@example.com"]);
        git(&["config", "user.name", "notation render"]);
        git(&["config", "commit.gpgsign", "false"]);
        git(&["add", "notation.md"]);
        let commit = std::process::Command::new("git")
            .arg("-C")
            .arg(repository.path())
            .args(["commit", "--quiet", "-m", "source"])
            .env("GIT_AUTHOR_DATE", "2024-01-02T03:04:05Z")
            .env("GIT_COMMITTER_DATE", "2024-01-02T03:04:05Z")
            .output()
            .expect("git commit runs");
        assert!(commit.status.success());

        assert_eq!(
            notation_render_timestamp(&source).timestamp(),
            chrono::DateTime::parse_from_rfc3339("2024-01-02T03:04:05Z")
                .expect("fixed test timestamp")
                .timestamp()
        );

        let non_git = tempfile::tempdir().expect("temporary input directory");
        let untracked = non_git.path().join("notation.md");
        std::fs::write(&untracked, "source").expect("write untracked source");
        assert_eq!(
            notation_render_timestamp(&untracked),
            chrono::DateTime::UNIX_EPOCH
        );
        assert_eq!(notation_render_revision(&untracked), None);
        assert_eq!(
            notation_render_revision(&source).as_deref().map(str::len),
            Some(40)
        );
    }

    #[test]
    fn has_extension_compares_case_insensitively() {
        assert!(has_extension(std::path::Path::new("out.pdf"), "pdf"));
        assert!(has_extension(std::path::Path::new("out.PDF"), "pdf"));
        assert!(!has_extension(std::path::Path::new("out.docx"), "pdf"));
        assert!(!has_extension(std::path::Path::new("out"), "pdf"));
    }
}
