//! `navigator-deploy dev build-nautilus` — build the Nautilus web preview bundle.
//!
//! Mirrors `devx::webapp` (`build-webapp`): drives `dx` to compile a Dioxus
//! crate to `wasm32-unknown-unknown` and stages the resulting bundle —
//! `index.html`, the wasm module, and the wasm-bindgen glue — under
//! `server/public/`, where `web` serves it same-origin. Here the crate is
//! `nautilus-preview/`, the web launcher for the Nautilus mobile shell, and
//! the staging home is `server/public/nautilus`.
//!
//! `nautilus-preview/` is deliberately its own workspace with its own
//! `Dioxus.toml` (see its `Cargo.toml`), so `dx` must run from that
//! directory — its output lands in `nautilus-preview/target/dx/`, not the
//! root `target/`. Like the `dioxus` bundle, the staged output is a build
//! artifact: gitignored, never committed.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

use super::orchestrate::{require_tools, run, workspace_root};
use super::webapp::copy_dir_all;

/// Build the `nautilus-preview` wasm bundle and stage it under
/// `server/public/nautilus`. `release` selects an optimized build (the
/// deploy default); debug builds are faster for local iteration.
pub fn build(release: bool) -> Result<()> {
    require_tools(&["dx"]).context(
        "the Dioxus CLI `dx` is required to build the Nautilus web preview; install it with \
         `cargo install dioxus-cli --version 0.7.9 --locked`",
    )?;
    let root = workspace_root()?;

    let crate_dir = root.join("nautilus-preview");
    if !crate_dir.join("Dioxus.toml").is_file() {
        anyhow::bail!(
            "{} is missing Dioxus.toml — run this from a Navigator checkout that has the \
             nautilus-preview crate",
            crate_dir.display()
        );
    }

    eprintln!(
        "==> dx build --platform web{} (from nautilus-preview/)",
        if release { " --release" } else { "" }
    );
    let mut cmd = Command::new("dx");
    cmd.current_dir(&crate_dir)
        .arg("build")
        .arg("--platform")
        .arg("web");
    if release {
        // The wasm-opt lesson from `build-webapp` applies identically: dx's
        // `--debug-symbols` defaults to TRUE even under `--release`, keeps
        // the DWARF dx's release profile preserves, and makes binaryen abort
        // re-emitting it — silently shipping the unoptimized module through.
        // Turning it off lets the `-Oz` pass actually land. Release only: a
        // debug build wants the symbols, and its bundle is never served to
        // the public.
        cmd.arg("--release").arg("--debug-symbols").arg("false");
    }
    run(&mut cmd)?;

    let bundle = bundle_dir(&root, release);
    if !bundle.join("index.html").is_file() {
        anyhow::bail!(
            "dx did not produce a bundle at {} (no index.html)",
            bundle.display()
        );
    }

    let dest = staging_dir(&root);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)
            .with_context(|| format!("clearing stale bundle at {}", dest.display()))?;
    }
    copy_dir_all(&bundle, &dest)?;
    eprintln!("==> staged Nautilus preview bundle -> {}", dest.display());
    Ok(())
}

/// The environment variable overriding where the staged Nautilus bundle is
/// looked up — the same name the portal's nautilus router reads for the
/// bundle-aware asset mount, so one override points both the preview server
/// and the hydration mechanics at a bundle built anywhere.
pub(crate) const STAGED_BUNDLE_ENV: &str = "NAUTILUS_DIOXUS_PUBLIC_PATH";

/// The staged Nautilus web bundle, or `None` when nothing is staged. An
/// explicit [`STAGED_BUNDLE_ENV`] wins, so a bundle built anywhere can be
/// served; otherwise this is the staging home [`build`] fills in this
/// checkout. A directory without an `index.html` is not a bundle — the
/// preview's degraded mode is what that case is for, not a lookup error.
pub(crate) fn staged_bundle_dir() -> Option<PathBuf> {
    let dir = match std::env::var_os(STAGED_BUNDLE_ENV) {
        Some(overridden) => PathBuf::from(overridden),
        None => staging_dir(&workspace_root().ok()?),
    };
    dir.join("index.html").is_file().then_some(dir)
}

/// Where `dx` drops the `nautilus-preview` bundle: `<crate>/target/dx/<app
/// name>/<profile>/web/public`, relative to the crate's own workspace root.
fn bundle_dir(root: &Path, release: bool) -> PathBuf {
    root.join("nautilus-preview")
        .join("target/dx/nautilus-preview")
        .join(if release { "release" } else { "debug" })
        .join("web/public")
}

/// Where `web` serves the staged bundle from. A gitignored build artifact,
/// under the same rule as `server/public/dioxus`.
fn staging_dir(root: &Path) -> PathBuf {
    root.join("server/public/nautilus")
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{bundle_dir, staging_dir};

    #[test]
    fn the_bundle_path_follows_the_dx_layout_for_the_crate_workspace() {
        let root = Path::new("/checkout");
        assert_eq!(
            bundle_dir(root, false),
            PathBuf::from("/checkout/nautilus-preview/target/dx/nautilus-preview/debug/web/public")
        );
        assert_eq!(
            bundle_dir(root, true),
            PathBuf::from(
                "/checkout/nautilus-preview/target/dx/nautilus-preview/release/web/public"
            )
        );
    }

    #[test]
    fn the_bundle_stages_under_server_public_nautilus() {
        assert_eq!(
            staging_dir(Path::new("/checkout")),
            PathBuf::from("/checkout/server/public/nautilus")
        );
    }

    /// The override points at a bundle built anywhere: a directory with an
    /// `index.html` is the staged bundle; a directory without one is the
    /// degraded mode, not a lookup error.
    #[test]
    fn the_env_override_names_the_staged_bundle() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::env::set_var(super::STAGED_BUNDLE_ENV, dir.path());
        assert_eq!(super::staged_bundle_dir(), None);
        std::fs::write(dir.path().join("index.html"), "<html></html>").expect("fixture");
        assert_eq!(super::staged_bundle_dir(), Some(dir.path().to_path_buf()));
        std::env::remove_var(super::STAGED_BUNDLE_ENV);
    }
}
