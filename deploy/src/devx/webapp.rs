//! `hudson dev build-webapp` — build the Dioxus client bundle (issue #641).
//!
//! Drives `dx` (the Dioxus CLI, itself a Rust binary — consistent with the
//! Rust-only invariant, exactly as this CLI already orchestrates `kubectl`,
//! `helm`, `docker`, and `kind`) to compile the `webapp` crate to
//! `wasm32-unknown-unknown` and copies the resulting bundle — `index.html`, the
//! wasm module, and the wasm-bindgen glue — into `server/public/dioxus`, where
//! `web` serves it same-origin and SSR-hydrates the `/dioxus-demo` page.
//!
//! The bundle is a build artifact: `images/Containerfile.web` runs this at image
//! build time and `server/public/dioxus` is gitignored, never committed.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

use super::orchestrate::{require_tools, run, workspace_root};

pub(super) const DIOXUS_CLI_VERSION: &str = "0.7.10";

fn pinned_cli_version(version: &str) -> bool {
    let mut words = version.split_whitespace();
    words.next() == Some("dioxus") && words.next() == Some(DIOXUS_CLI_VERSION)
}

fn require_pinned_cli() -> Result<()> {
    let version = Command::new("dx")
        .arg("--version")
        .output()
        .context("read installed Dioxus CLI version")?;
    anyhow::ensure!(
        version.status.success()
            && std::str::from_utf8(&version.stdout).is_ok_and(pinned_cli_version),
        "Dioxus CLI {DIOXUS_CLI_VERSION} is required; install it with \
         `cargo install dioxus-cli --version {DIOXUS_CLI_VERSION} --locked`"
    );
    Ok(())
}

/// Build the `webapp` wasm client bundle and stage it under
/// `server/public/dioxus`. `release` selects an optimized build (the deploy and CI
/// default); debug builds are faster for local iteration.
pub fn build(release: bool) -> Result<()> {
    require_tools(&["dx"]).with_context(|| {
        format!(
            "Dioxus CLI {DIOXUS_CLI_VERSION} is required; install it with \
         `cargo install dioxus-cli --version {DIOXUS_CLI_VERSION} --locked`"
        )
    })?;
    require_pinned_cli()?;
    let root = workspace_root()?;

    let profile = if release { "release" } else { "debug" };
    eprintln!(
        "==> dx build --package webapp --platform web{}",
        if release {
            " --release --debug-symbols false"
        } else {
            ""
        }
    );
    let mut cmd = build_command(&root, release);
    run(&mut cmd)?;

    let bundle = root
        .join("target/dx/webapp")
        .join(profile)
        .join("web/public");
    let dest = root.join("server/public/dioxus");
    stage_bundle(&bundle, &dest)?;
    eprintln!("==> staged Dioxus client bundle -> {}", dest.display());
    Ok(())
}

/// Replace whatever is staged at `dest` with the bundle `dx` left at `bundle`.
pub(super) fn stage_bundle(bundle: &Path, dest: &Path) -> Result<()> {
    if !bundle.join("index.html").is_file() {
        anyhow::bail!(
            "dx did not produce a bundle at {} (no index.html)",
            bundle.display()
        );
    }
    if dest.exists() {
        fs::remove_dir_all(dest)
            .with_context(|| format!("clearing stale bundle at {}", dest.display()))?;
    }
    copy_dir_all(bundle, dest)
}

fn build_command(root: &Path, release: bool) -> Command {
    let mut cmd = Command::new("dx");
    cmd.current_dir(root)
        .arg("build")
        .arg("--package")
        .arg("webapp")
        .arg("--platform")
        .arg("web")
        .arg("--force-sequential")
        .arg("true");
    if release {
        // `dx`'s own `--debug-symbols` defaults to TRUE, including under
        // `--release`, and it is the flag that decides whether wasm-opt is
        // invoked with `--debuginfo` or `--strip-debug`. Left at its default the
        // release bundle keeps the ~340 KB of DWARF that dx's ad-hoc
        // `wasm-release` profile preserves (that profile sets `strip=false`), and
        // binaryen then aborts re-emitting it — `UNREACHABLE executed at
        // DWARFEmitter.cpp:201`, SIGABRT. dioxus-cli treats a failed wasm-opt as
        // non-fatal and copies the UNOPTIMIZED module through, so the abort never
        // fails a build; it just silently ships 941 KB where 534 KB would do.
        //
        // Turning it off is what makes wasm-opt strip rather than rewrite, so the
        // `-Oz` pass we are already paying for actually lands. Release only: a
        // debug build wants the symbols, and its bundle is never served to the
        // public. The `name` section is unaffected — dx's separate `--keep-names`
        // already defaults to false and wasm-bindgen already runs with
        // `--remove-name-section`.
        cmd.arg("--release").arg("--debug-symbols").arg("false");
    }
    cmd
}

/// Recursively copy `src` into `dst`, creating `dst` and any parents.
fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            fs::copy(&from, &to)
                .with_context(|| format!("copying {} -> {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_pinned_dioxus_cli_can_produce_the_local_client() {
        assert!(pinned_cli_version("dioxus 0.7.10 (build-id)"));
        assert!(pinned_cli_version("dioxus 0.7.10"));
        for incompatible in [
            "dioxus 0.7.9 (old)",
            "dioxus 0.7.100",
            "dioxus 0.7.10-dev",
            "other 0.7.10",
            "",
        ] {
            assert!(!pinned_cli_version(incompatible));
        }
    }

    #[test]
    fn staging_replaces_the_previous_bundle_and_refuses_a_build_without_an_index() {
        let dir = tempfile::tempdir().unwrap();
        let (bundle, dest) = (dir.path().join("out"), dir.path().join("public/dioxus"));
        fs::create_dir_all(bundle.join("assets/wasm")).unwrap();
        fs::write(bundle.join("assets/wasm/app.wasm"), "module").unwrap();
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("stale.js"), "previous build").unwrap();

        let refused = stage_bundle(&bundle, &dest).unwrap_err().to_string();
        assert!(refused.contains("no index.html"), "{refused}");
        assert!(dest.join("stale.js").is_file(), "a refusal stages nothing");

        fs::write(bundle.join("index.html"), "<html>").unwrap();
        stage_bundle(&bundle, &dest).unwrap();
        assert_eq!(
            fs::read_to_string(dest.join("index.html")).unwrap(),
            "<html>"
        );
        assert_eq!(
            fs::read_to_string(dest.join("assets/wasm/app.wasm")).unwrap(),
            "module"
        );
        assert!(!dest.join("stale.js").exists());
    }

    #[test]
    fn debug_and_release_clients_serialize_compilers_and_keep_their_symbol_policy() {
        for release in [false, true] {
            let command = build_command(Path::new("/synthetic-checkout"), release);
            assert_eq!(
                command.get_current_dir(),
                Some(Path::new("/synthetic-checkout"))
            );
            let args: Vec<_> = command
                .get_args()
                .map(|arg| arg.to_str().unwrap())
                .collect();
            assert!(args
                .windows(2)
                .any(|pair| pair == ["--force-sequential", "true"]));
            assert_eq!(args.contains(&"--release"), release);
            assert_eq!(
                args.windows(2)
                    .any(|pair| pair == ["--debug-symbols", "false"]),
                release
            );
        }
    }
}
