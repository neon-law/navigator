//! `output: form` — the `AcroForm` fill branch of the CLI render path
//! (LAW-306).
//!
//! The validation gate (N109) guarantees a form template declares both
//! `form:` (the registry code) and `origin_url:`; this module turns that
//! declaration into a filled PDF or a loud refusal — it never falls back
//! to a plain page, because a form template that silently renders as a
//! typeset document produces an unfilled government form an author may
//! not notice until filing day.
//!
//! The canonical fill — flattened, filed, persisted into the matter — is
//! owned by the server-side `generate_pdf__*` walk
//! (`workflows::document::dispatch_generate_pdf`). The CLI produces the
//! **attorney-review artifact**: filled, pin-verified, and deliberately
//! unflattened, so a reviewer can open the fields in a PDF reader before
//! the walk's lawyer-review step freezes the approved values.
//!
//! Blank bytes come from one of two pinned sources, verified against the
//! form's sha-256 pin either way (LAW-19 — never a silent fresh download
//! that changes bytes between runs):
//!
//! 1. the checkout's working copy at `templates/notations/<object_path>`,
//!    vendored by `navigator forms sync` — the pin file beside it wins
//!    over the compiled-in pin, the same rule `forms sync` and
//!    `forms re-author` follow so a re-vendor verifies without a rebuild;
//! 2. the local blank cache, keyed on the template's `origin_url` plus
//!    the effective content hash, populated by a pin-verified download
//!    when no working copy exists.
//!
//! Under `--ci` only cached or vendored bytes are read: a miss refuses
//! rather than reaching the network.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use cloud::StorageService;

use cli::palette;

/// The render profile that selects this mode: `output: form`, or the
/// `kind: filing` default (`Kind::Filing`'s `default_output`).
pub const FORM_PROFILE: &str = "form";

/// Cache-root override, so CI and test harnesses can pin the cache to a
/// known directory instead of the user's `XDG_CACHE_HOME`.
const CACHE_ROOT_ENV: &str = "NAVIGATOR_FORM_CACHE";

/// Render a form template: resolve the pinned blank from the vendored
/// forms registry, fill its `AcroForm` fields from the answers, and write
/// the unflattened PDF. Every failure is a loud non-zero exit naming the
/// next step — never a plain-page fallback.
pub fn run(
    file: &Path,
    contents: &str,
    out: &Path,
    answers: &[(String, String)],
    output_extension: &str,
    ci: bool,
) -> ExitCode {
    match render(file, contents, out, answers, output_extension, ci) {
        Ok(filled) => {
            println!(
                "{}",
                palette::dim(format!(
                    "Rendered {} (form, {} field(s) filled, unflattened for review) → {}",
                    file.display(),
                    filled,
                    out.display()
                ))
            );
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("navigator: {message}");
            ExitCode::from(2)
        }
    }
}

/// The body of [`run`]: `Ok` carries the number of fields filled.
fn render(
    file: &Path,
    contents: &str,
    out: &Path,
    answers: &[(String, String)],
    output_extension: &str,
    ci: bool,
) -> Result<usize, String> {
    // An AcroForm fill produces a PDF; there is no Word twin of a form.
    if output_extension != "pdf" {
        return Err(
            "this template's `output: form` fills an AcroForm, which has no Word frame; \
             render it with `notation pdf`"
                .to_string(),
        );
    }

    let frontmatter = rules::frontmatter::extract(contents)
        .ok_or("a form template carries no frontmatter — `output: form` needs a `form:` code")?;
    let code = rules::frontmatter::field(frontmatter, "form")
        .filter(|value| !value.is_empty())
        .ok_or(
            "this template declares `output: form` without a `form:` code naming the \
                blank to fill (N109 requires the pair)",
        )?;
    // N110 requires the same pair; read it here for the cache key. The
    // gate refuses the template before this runs, so a miss is a
    // defensive loud error, not a silent one.
    let origin_url = rules::frontmatter::field(frontmatter, "origin_url")
        .filter(|value| !value.is_empty())
        .ok_or(
            "this template declares `output: form` without an `origin_url:` pointing \
                at the blank's canonical government page (N109 requires the pair)",
        )?;

    let form = forms::get(&code)
        .map_err(|e| format!("loading the vendored forms registry: {e}"))?
        .ok_or_else(|| {
            format!(
                "`{code}` is not in the vendored forms registry (`templates/notations/forms/`); \
             vendor the blank with `navigator forms sync` — the canonical fill is the \
             server-side `generate_pdf__*` walk (`workflows::document::dispatch_generate_pdf`)"
            )
        })?;

    let blank = resolve_blank(file, &form, &origin_url, ci)?;

    // Structural verification: strip a static XFA packet when present
    // (the same preprocessing `forms re-author` applies) and require the
    // result to parse as an AcroForm. Unreadable bytes refuse loudly —
    // the canonical fill lives in the server-side walk, and the CLI
    // never guesses at what a form's fields are.
    let blank =
        pdf::strip_static_xfa(&blank).map_err(|e| format!("verifying the `{code}` blank: {e}"))?;
    pdf::field_names(&blank).map_err(|e| {
        format!(
            "verifying the `{code}` blank: {e} — the bytes are not a readable AcroForm; \
             the canonical fill is the server-side `generate_pdf__*` walk \
             (`workflows::document::dispatch_generate_pdf`)"
        )
    })?;

    let fields = match forms::fill_values(&code, &answer_map(answers)) {
        Ok(Some(fields)) => fields,
        Ok(None) => BTreeMap::new(),
        Err(e) => return Err(format!("mapping answers onto the `{code}` fields: {e}")),
    };
    let filled = fields.len();

    let output = pdf::fill_acroform(&blank, &fields)
        .map_err(|e| format!("filling the `{code}` blank: {e}"))?;
    // Unflattened on purpose: the CLI's product is the attorney-review
    // artifact. Flattening freezes the values, and that freeze belongs
    // to the walk's post-`lawyer_review` step, not to a local render.
    std::fs::write(out, &output).map_err(|e| format!("write {}: {e}", out.display()))?;
    Ok(filled)
}

/// Raw answer values keyed by question code — the shape
/// `forms::fill_values` resolves against the form's re-authored
/// `.fields` manifest. Answers reach an `AcroForm` verbatim: the
/// Markdown-safe `**…**` wrapping the body renderer applies is for
/// typeset prose, and a form field is not prose.
fn answer_map(answers: &[(String, String)]) -> BTreeMap<String, String> {
    answers.iter().cloned().collect()
}

/// Resolve the pinned blank bytes: vendored working copy, then local
/// cache, then a pin-verified download — refused outright under `--ci`.
/// Every source verifies against the effective pin before its bytes are
/// used; a mismatch refuses rather than falling through to another
/// source, because bytes that contradict their own pin mean the
/// checkout or cache is inconsistent and must be re-synced, not ignored.
fn resolve_blank(
    file: &Path,
    form: &forms::FormMeta,
    origin_url: &str,
    ci: bool,
) -> Result<Vec<u8>, String> {
    let workspace = workspace_root(file);
    let working_copy = workspace.as_ref().map(|root| {
        root.join("templates")
            .join("notations")
            .join(form.object_path)
    });
    // The pin file beside the working copy wins over the compiled-in
    // pin — the same rule `forms sync` follows — so a re-vendor in this
    // checkout verifies without a rebuild.
    let pin = working_copy
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path.with_extension("sha256")).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| form.pinned_sha256().to_string());

    if let Some(copy) = &working_copy {
        if let Ok(bytes) = std::fs::read(copy) {
            return verify_pin(&pin, &bytes, form).map(|()| bytes);
        }
    }

    if let Some(bytes) = cache_get(origin_url, &pin) {
        return verify_pin(&pin, &bytes, form).map(|()| bytes);
    }

    if ci {
        return Err(format!(
            "--ci refuses to fetch the `{}` blank over the network: render only from \
             pin-verified bytes vendored by `navigator forms sync` or already in the \
             local blank cache",
            form.code
        ));
    }

    let bytes = fetch_from_assets(form.object_path)?;
    verify_pin(&pin, &bytes, form)?;
    cache_put(origin_url, &pin, &bytes)?;
    Ok(bytes)
}

/// Verify `bytes` against the effective pin — the content hash the
/// repository commits, so the same pin always means the same bytes
/// (LAW-19).
fn verify_pin(pin: &str, bytes: &[u8], form: &forms::FormMeta) -> Result<(), String> {
    forms::verify_sha256(pin, bytes).map_err(|e| {
        format!(
            "the `{}` blank does not match its `.sha256` pin ({e}) — re-vendor it with \
             `navigator forms sync`",
            form.code
        )
    })
}

/// First ancestor of the template's directory (or the current
/// directory) carrying the vendored forms shelf — the same workspace
/// signature `forms sync` walks up to.
fn workspace_root(file: &Path) -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok();
    file.parent()
        .into_iter()
        .chain(cwd.as_deref())
        .flat_map(Path::ancestors)
        .find(|p| p.join("templates/notations/forms").is_dir())
        .map(Path::to_path_buf)
}

/// The cache key: the template's `origin_url` plus the effective
/// content hash. A template re-pointed at a different source page, or a
/// re-vendored blank, misses and re-verifies rather than serving stale
/// bytes.
fn cache_path(origin_url: &str, pin: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(CACHE_ROOT_ENV) {
        if !path.is_empty() {
            return Some(PathBuf::from(path).join(cache_key(origin_url, pin)));
        }
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(
        base.join("navigator")
            .join("forms")
            .join(cache_key(origin_url, pin)),
    )
}

fn cache_key(origin_url: &str, pin: &str) -> PathBuf {
    PathBuf::from(format!(
        "{}.pdf",
        forms::sha256_hex(format!("{origin_url}\n{pin}").as_bytes())
    ))
}

fn cache_get(origin_url: &str, pin: &str) -> Option<Vec<u8>> {
    let path = cache_path(origin_url, pin)?;
    std::fs::read(path).ok()
}

fn cache_put(origin_url: &str, pin: &str, bytes: &[u8]) -> Result<(), String> {
    let path = cache_path(origin_url, pin)
        .ok_or("no cache root — set XDG_CACHE_HOME or HOME to cache the blank")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cache {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, bytes).map_err(|e| format!("cache {}: {e}", path.display()))
}

/// Pull the blank from the assets lane — the same bucket `forms sync`
/// vendors to — and leave verification to the caller.
fn fetch_from_assets(object_path: &str) -> Result<Vec<u8>, String> {
    let runtime = tokio::runtime::Runtime::new().map_err(|e| format!("tokio runtime: {e}"))?;
    runtime.block_on(async {
        let storage: Arc<dyn StorageService> =
            cloud::assets_from_lookup(|key| std::env::var(key).ok())
                .await
                .map_err(|e| {
                    format!(
                        "open assets storage: {e} — pass --bucket or set \
                NAVIGATOR_ASSETS_BUCKET (or NAVIGATOR_STORAGE_BUCKET)"
                    )
                })?;
        let blank = storage.get(object_path).await.map_err(|e| {
            format!("pull `{object_path}`: {e} — vendor the blank with `navigator forms sync`")
        })?;
        Ok(blank.bytes)
    })
}
