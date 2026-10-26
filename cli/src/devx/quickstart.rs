//! `navigator quickstart` — one command from a fresh machine to a running,
//! seeded local Navigator.
//!
//! It composes what already exists rather than adding a second topology:
//!
//! 1. **Preflight.** Every host tool the KIND tier needs is checked at once,
//!    and the Docker runtime must answer, so a newcomer reads one install
//!    line instead of failing once per missing tool.
//! 2. **Checkout.** Run inside a Navigator checkout, it uses that checkout.
//!    Run anywhere else (a Homebrew install), it clones the source into
//!    `~/.navigator/quickstart` and hands off to that checkout's own CLI, so
//!    the binary never drives manifests from a different release.
//! 3. **Tier.** `dev worktree-env up` gives the checkout its own KIND cluster
//!    on its own slot ports, applies the schema, and stages the sample
//!    matters; `dev forgejo up` adds PostgreSQL and Forgejo on that slot.
//! 4. **Web.** `dev serve` builds and starts `web`, which seeds the Guieb & Shook
//!    PC firm and its sample matters on boot, and the browser opens once it
//!    is ready.
//!
//! The decisions (which tools are missing, the install line, where the
//! checkout lives, the closing summary) are pure and unit-tested here; the
//! orchestration they feed is the existing, separately tested commands.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use super::KindConfig;

/// The public source the Homebrew binary clones when it runs outside a
/// checkout. Overridable for a fork or a mirror.
const REPOSITORY_ENV: &str = "NAVIGATOR_QUICKSTART_REPOSITORY";
const DEFAULT_REPOSITORY: &str = "https://github.com/neon-law/navigator.git";
/// Where that clone lives. Overridable so CI and tests never touch `$HOME`.
const DIRECTORY_ENV: &str = "NAVIGATOR_QUICKSTART_DIR";

/// Where to reach the firm about adapting Navigator for another practice.
const CONTACT_URL: &str = "https://www.neonlaw.com/contact";

/// One host tool the quickstart needs, with the Homebrew formula that
/// provides it.
struct Tool {
    command: &'static str,
    formula: &'static str,
}

/// Every tool the KIND tier, Forgejo, and the `web` build shell out to.
const TOOLS: &[Tool] = &[
    Tool {
        command: "docker",
        formula: "--cask orbstack",
    },
    Tool {
        command: "kind",
        formula: "kind",
    },
    Tool {
        command: "kubectl",
        formula: "kubernetes-cli",
    },
    Tool {
        command: "helm",
        formula: "helm",
    },
    Tool {
        command: "restate",
        formula: "restatedev/tap/restate",
    },
    Tool {
        command: "git",
        formula: "git",
    },
    Tool {
        command: "openssl",
        formula: "openssl@3",
    },
    Tool {
        command: "cargo",
        formula: "rustup",
    },
    // `libz-ng-sys` runs it while building `web` (see docs/oss-install.md).
    Tool {
        command: "cmake",
        formula: "cmake",
    },
];

/// The commands in [`TOOLS`] that `present` does not find, in table order.
fn missing_tools(present: &dyn Fn(&str) -> bool) -> Vec<&'static Tool> {
    TOOLS.iter().filter(|tool| !present(tool.command)).collect()
}

/// The one message a newcomer needs to install everything missing.
fn install_hint(missing: &[&Tool]) -> String {
    let names: Vec<&str> = missing.iter().map(|tool| tool.command).collect();
    let mut formulas: Vec<&str> = missing
        .iter()
        .map(|tool| tool.formula)
        .filter(|formula| !formula.starts_with("--cask"))
        .collect();
    formulas.dedup();
    let mut lines = vec![format!("missing required tools: {}", names.join(", "))];
    if !formulas.is_empty() {
        lines.push(format!("  brew install {}", formulas.join(" ")));
    }
    if let Some(cask) = missing
        .iter()
        .find(|tool| tool.formula.starts_with("--cask"))
    {
        lines.push(format!(
            "  brew install {}   # a Docker runtime",
            cask.formula
        ));
    }
    if missing.iter().any(|tool| tool.command == "cargo") {
        lines.push("  rustup-init -y   # then open a new shell".to_string());
    }
    lines.push("Then run `navigator quickstart` again.".to_string());
    lines.join("\n")
}

fn on_path(command: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {command}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn preflight() -> Result<()> {
    eprintln!("==> checking host tools");
    let missing = missing_tools(&on_path);
    if !missing.is_empty() {
        bail!(install_hint(&missing));
    }
    let docker_answers = Command::new("docker")
        .arg("info")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !docker_answers {
        bail!(
            "the Docker runtime is not running; start OrbStack or Docker Desktop, \
             then run `navigator quickstart` again"
        );
    }
    Ok(())
}

/// Where the Homebrew binary keeps its checkout.
fn clone_directory(get: &dyn Fn(&str) -> Option<String>) -> Result<PathBuf> {
    if let Some(dir) = get(DIRECTORY_ENV).filter(|value| !value.trim().is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = get("HOME")
        .filter(|value| !value.trim().is_empty())
        .context(
            "HOME is unset; set NAVIGATOR_QUICKSTART_DIR to choose where to clone Navigator",
        )?;
    Ok(Path::new(&home).join(".navigator").join("quickstart"))
}

fn repository(get: &dyn Fn(&str) -> Option<String>) -> String {
    get(REPOSITORY_ENV)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_REPOSITORY.to_string())
}

/// Clone (once) and re-run the quickstart from that checkout's own CLI.
fn hand_off() -> Result<()> {
    let get = |key: &str| std::env::var(key).ok();
    let dir = clone_directory(&get)?;
    if dir.join("Cargo.toml").is_file() {
        eprintln!("==> reusing the Navigator checkout at {}", dir.display());
    } else {
        let repo = repository(&get);
        eprintln!("==> cloning {repo} into {}", dir.display());
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        super::run(
            Command::new("git")
                .args(["clone", "--depth", "1", &repo])
                .arg(&dir),
        )?;
    }
    eprintln!("==> building the checkout's CLI; the first build takes several minutes");
    super::run(Command::new("cargo").current_dir(&dir).args([
        "run",
        "--quiet",
        "-p",
        "cli",
        "--",
        "quickstart",
    ]))
}

/// The closing summary: every URL the tier serves, how to sign in, and what
/// to do next.
fn summary(cfg: &KindConfig, root: &Path) -> String {
    let web = format!("http://localhost:{}", cfg.web_port);
    format!(
        "\
Navigator is running on this machine.

  Neon Law website      {web}
  Shook Law website     http://localhost:{shook}
  DeleteYourData        http://localhost:{dyd}
  Forgejo               http://localhost:{forgejo}/
  PostgreSQL            127.0.0.1:{postgres} (TLS; synthetic logins in the navigator-forgejo-kind Secret)
  Rauthy (sign-in)      {rauthy}

Sign in at {web}/auth/login as owner@, lawyer@, clerk@, or client@neonlaw.com
with the password `password`. The seeded firm is Guieb & Shook PC with three sample
matters: Cruller v. Prine, Widget Works, and the Estate of Cornelius Montgomery.

Make it yours: copy .env.example to .env and add your own Meta, TikTok, or other
provider keys, then re-run `navigator quickstart`. Use your own brand, not ours.
Want Navigator skinned for your firm? Contact us any time at {CONTACT_URL};
we help you write token-efficient prompts.

Ctrl-C stops the website. `navigator dev worktree-env down --path {root}`
removes the cluster.",
        shook = cfg.lawyer_shook_web_port,
        dyd = cfg.delete_your_data_web_port,
        forgejo = cfg.forgejo_port,
        postgres = cfg.postgres_port,
        rauthy = super::rauthy_origin(cfg.rauthy_port),
        root = super::shell_single_quote(&root.display().to_string()),
    )
}

/// CI runs the same command but has no desktop to open.
fn opens_browser(get: &dyn Fn(&str) -> Option<String>) -> bool {
    get("CI").is_none_or(|value| value.is_empty() || value == "false")
}

pub(super) fn run(base_cfg: &KindConfig) -> Result<()> {
    preflight()?;
    let Ok(root) = super::workspace_root() else {
        return hand_off();
    };
    super::worktree_env::up_kind(&root, base_cfg)?;
    // `forgejo up` and `serve` read the slot this checkout was just given.
    dotenvy::from_path_override(root.join(".devx/env")).context("load .devx/env")?;
    let cfg = KindConfig::from_env();
    super::forgejo::up(&cfg)?;
    eprintln!("\n{}\n", summary(&cfg, &root));
    let get = |key: &str| std::env::var(key).ok();
    super::serve::run_opening(opens_browser(&get))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        move |key: &str| {
            pairs
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn a_ready_host_is_missing_nothing() {
        assert!(missing_tools(&|_| true).is_empty());
    }

    #[test]
    fn every_missing_tool_is_reported_in_one_install_line() {
        let missing = missing_tools(&|command| !matches!(command, "kind" | "helm" | "cargo"));
        let hint = install_hint(&missing);
        assert!(hint.starts_with("missing required tools: kind, helm, cargo"));
        assert!(hint.contains("  brew install kind helm rustup\n"));
        assert!(hint.contains("rustup-init -y"));
        assert!(hint.ends_with("Then run `navigator quickstart` again."));
        assert!(!hint.contains("orbstack"));
    }

    #[test]
    fn a_missing_docker_suggests_a_runtime_cask_not_a_formula() {
        let missing = missing_tools(&|command| command != "docker");
        let hint = install_hint(&missing);
        assert!(hint.contains("  brew install --cask orbstack   # a Docker runtime"));
        assert!(!hint.contains("brew install --cask orbstack\n  brew install"));
        assert!(!hint.contains("brew install docker"));
    }

    #[test]
    fn the_tool_table_covers_what_the_composed_commands_require() {
        let commands: Vec<&str> = TOOLS.iter().map(|tool| tool.command).collect();
        // `dev worktree-env up` (kind, kubectl, docker, helm, restate),
        // `dev forgejo up` (kubectl, openssl), sample staging (git), and
        // `dev serve` (cargo, cmake).
        for required in [
            "docker", "kind", "kubectl", "helm", "restate", "git", "openssl", "cargo", "cmake",
        ] {
            assert!(commands.contains(&required), "{required} is not checked");
        }
    }

    #[test]
    fn the_clone_lives_under_home_unless_overridden() {
        assert_eq!(
            clone_directory(&env(&[("HOME", "/Users/someone")])).unwrap(),
            Path::new("/Users/someone/.navigator/quickstart")
        );
        assert_eq!(
            clone_directory(&env(&[
                ("HOME", "/Users/someone"),
                (DIRECTORY_ENV, "/tmp/nav")
            ]))
            .unwrap(),
            Path::new("/tmp/nav")
        );
        assert!(clone_directory(&env(&[])).is_err());
    }

    #[test]
    fn the_public_repository_is_the_default_source() {
        assert_eq!(repository(&env(&[])), DEFAULT_REPOSITORY);
        assert_eq!(
            repository(&env(&[(REPOSITORY_ENV, "https://example.test/fork.git")])),
            "https://example.test/fork.git"
        );
    }

    #[test]
    fn the_summary_names_every_slot_port_and_the_next_steps() {
        let mut cfg = KindConfig::from_env();
        cfg.web_port = 20_637;
        cfg.lawyer_shook_web_port = 21_337;
        cfg.delete_your_data_web_port = 20_337;
        cfg.forgejo_port = 21_437;
        cfg.postgres_port = 21_537;
        cfg.rauthy_port = 20_437;
        let text = summary(&cfg, Path::new("/Users/someone/.navigator/quickstart"));
        for expected in [
            "http://localhost:20637\n",
            "http://localhost:21337\n",
            "http://localhost:20337\n",
            "http://localhost:21437/\n",
            "127.0.0.1:21537 (TLS",
            "http://localhost:20437",
            "http://localhost:20637/auth/login",
            "Guieb & Shook PC",
            ".env.example",
            "Meta, TikTok",
            CONTACT_URL,
            "token-efficient prompts",
            "navigator dev worktree-env down --path '/Users/someone/.navigator/quickstart'",
        ] {
            assert!(text.contains(expected), "summary is missing {expected:?}");
        }
    }

    #[test]
    fn ci_runs_without_opening_a_browser() {
        assert!(opens_browser(&env(&[])));
        assert!(opens_browser(&env(&[("CI", "false")])));
        assert!(!opens_browser(&env(&[("CI", "true")])));
    }
}
