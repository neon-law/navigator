//! The repository `templates/` tree, embedded once at build time.
//!
//! Every binary that needs the shipped Notation catalog reads it here: the
//! portal serves its public subset over HTTP, and the CLIs read the whole
//! catalog on a firm machine. One embedding keeps the bytes in one place.

use include_dir::{include_dir, Dir};

/// The repository `templates/` tree, resolved against this crate's manifest
/// dir so it tracks the tree at the workspace root.
pub static TEMPLATES: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../templates");

/// Every file in the embedded `templates/` tree as `(repository-relative
/// path, bytes)`, in a stable depth-first order.
///
/// This is the whole shipped catalog, *including* the templates
/// portal's `find_raw_path` refuses. That is deliberate and the two doors are not
/// interchangeable: the portal's `find_raw_path` answers the public web, where
/// `confidential: true` must fail closed, while this one answers the CLI
/// running on a firm machine, where the operator is entitled to the same
/// catalog a checkout would give them. A caller that serves bytes over the
/// network wants `find_raw_path`, never this.
#[must_use]
pub fn bundled_files() -> Vec<(&'static str, &'static [u8])> {
    fn walk(dir: &'static Dir<'static>, out: &mut Vec<(&'static str, &'static [u8])>) {
        for file in dir.files() {
            if let Some(path) = file.path().to_str() {
                out.push((path, file.contents()));
            }
        }
        for child in dir.dirs() {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(&TEMPLATES, &mut out);
    out.sort_unstable_by_key(|(path, _)| *path);
    out
}
