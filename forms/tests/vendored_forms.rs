//! Provenance guard for vendored government forms.
//!
//! The repository path is the public bucket path: each blank lives in
//! the assets bucket at `object_path`, and the repo keeps the diffable
//! text beside the markdown template — the `.fields.toml` map and the
//! `.sha256` pin the fill path verifies the pulled bytes against.

use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("forms crate sits one level under the workspace root")
        .to_path_buf()
}

#[test]
fn vendored_forms_pin_their_bucket_objects() {
    let forms = forms::registry().expect("registry loads");
    assert!(!forms.is_empty(), "registry lists no forms");

    for form in &forms {
        assert!(
            form.object_path.starts_with("forms/united_states/"),
            "{}: object path must be bucket-relative under forms/united_states",
            form.code
        );
        assert!(
            form.object_path.ends_with(&format!("{}.pdf", form.code)),
            "{}: object path must end with the form code stem",
            form.code
        );

        // The blank's bytes are NOT in the tree — only its pin is. The
        // pin file sits beside the markdown template at the
        // bucket-shaped repo path, and the registry compiles it in.
        let templates_path = workspace_root()
            .join("templates")
            .join("notations")
            .join(form.object_path);
        let pin_path = templates_path.with_extension("sha256");
        let pin_on_disk = std::fs::read_to_string(&pin_path).unwrap_or_else(|e| {
            panic!(
                "{}: cannot read pin file {}: {e}",
                form.code,
                pin_path.display()
            )
        });
        assert_eq!(
            pin_on_disk.trim(),
            form.pinned_sha256(),
            "{}: compiled-in pin diverges from {} — rebuild after `forms sync`",
            form.code,
            pin_path.display()
        );
        let pin = form.pinned_sha256();
        assert_eq!(pin.len(), 64, "{}: pin is not a sha-256 digest", form.code);
        assert!(
            pin.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "{}: pin is not lowercase hex",
            form.code
        );

        assert!(
            official_origin(form.origin_url),
            "{}: origin_url must be an HTTPS government URL (or an allowlisted \
             official court domain), got `{}`",
            form.code,
            form.origin_url
        );
    }
}

/// Official court domains that do not sit under `.gov`. Washoe County's
/// courts publish their forms on washoecourts.com, and the Eighth
/// Judicial District Court's Civil Law Self-Help Center publishes the
/// Clark County small-claims forms on civillawselfhelpcenter.org — both
/// verified as the official sources in the project's court-form surveys
/// (October 2026) and SHA-256-pin-matched against the downloaded blanks.
const OFFICIAL_NON_GOV_DOMAINS: [&str; 2] = ["washoecourts.com", "civillawselfhelpcenter.org"];

/// Whether an origin URL points at a verified official government source:
/// any HTTPS host under `.gov`, or an allowlisted official court domain.
fn official_origin(url: &str) -> bool {
    if !url.starts_with("https://") {
        return false;
    }
    let host = url
        .trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap_or_default();
    host.contains(".gov")
        || OFFICIAL_NON_GOV_DOMAINS
            .iter()
            .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}
