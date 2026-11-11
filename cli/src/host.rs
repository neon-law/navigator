//! Host-side helpers every Navigator binary shares: Homebrew, the browser,
//! and Git remote spelling.

use std::process::Command;

use anyhow::{bail, Context, Result};

/// Run a `brew` subcommand, surfacing its stderr on failure.
pub fn brew(args: &[&str]) -> Result<String> {
    let output = Command::new("brew")
        .args(args)
        .output()
        .with_context(|| format!("run `brew {}`", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "`brew {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Locate Homebrew, returning its prefix.
pub fn require_homebrew() -> Result<String> {
    let output = Command::new("brew").arg("--prefix").output().context(
        "Homebrew is required but `brew` is not on PATH.\n\
             Install it from https://brew.sh, then re-run this command.",
    )?;
    if !output.status.success() {
        bail!(
            "`brew --prefix` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Split either remote spelling into `(host, path)`: the URL form
/// `scheme://[user@]host[:port]/path`, and Git's scp-like `[user@]host:path`.
#[must_use]
pub fn split_remote(url: &str) -> Option<(&str, &str)> {
    if let Some((_scheme, rest)) = url.split_once("://") {
        let rest = rest.rsplit_once('@').map_or(rest, |(_, after)| after);
        let (authority, path) = rest.split_once('/')?;
        Some((
            authority
                .split_once(':')
                .map_or(authority, |(host, _)| host),
            path,
        ))
    } else {
        let rest = url.rsplit_once('@').map_or(url, |(_, after)| after);
        rest.split_once(':')
    }
}

/// The single status check every administered repository is gated on.
///
/// Repositories do not agree on what CI *does* — this workspace runs `cargo
/// test --workspace` on a large runner, `neon-law/ui` runs its own `lint`/`verify`
/// pair — and binding the gate to whatever the slowest job
/// happened to be called made the required context per-repository. That is
/// fragile in the one direction that fails silently: renaming a job renames
/// its check run, the ruleset keeps requiring the old spelling, and a context
/// nothing posts is a gate that never goes green.
///
/// So the contract is inverted. Every repository terminates its `ci` workflow
/// in one aggregating job spelled exactly `ci`, which succeeds only when the
/// real jobs behind it did. Those jobs stay free to differ per repository and
/// to be renamed at will; the required context never moves.
///
/// `assert_required_check_job` refuses to bind this gate on a repository
/// whose workflows do not actually define that job, so adopting the convention
/// is checked rather than assumed.
///
/// Shared here because [`crate::projects::repository`]'s generated gate must
/// terminate in a job spelled the same way `navigator-deploy ops github setup`
/// binds the ruleset to — two constants for one convention is a rename waiting
/// to leave one of them stale.
pub const REQUIRED_CHECK: &str = "ci";

/// Best-effort: open `url` in the reader's default browser. Returns whether
/// it worked, but opening is convenience, not a contract — the URL is
/// printed either way, so a failure here never loses the one thing the
/// reader actually needs.
///
/// macOS's `open` and Linux's `xdg-open` cover every platform this binary
/// ships for without a crate dependency. A
/// failure here (headless CI, a missing binary, no display) is silently
/// non-fatal — the printed URL is the real interface.
#[must_use]
pub fn open_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    let opened = std::process::Command::new("open").arg(url).status();
    #[cfg(target_os = "linux")]
    let opened = std::process::Command::new("xdg-open").arg(url).status();
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let opened: std::io::Result<std::process::ExitStatus> =
        Err(std::io::Error::other("no known browser-open command"));

    opened.is_ok_and(|status| status.success())
}
