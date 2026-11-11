//! `navigator project repository jj` — put Jujutsu (`jj`) over a Project
//! checkout without changing what the forge sees.
//!
//! The checkout stays a Git repository: `jj git init --colocate` adds a `.jj`
//! store beside `.git`, so Forgejo, `navigator project gate`, and protected
//! `main` read the same commits they always have. What jj adds is local: every
//! edit is snapshotted, `jj undo` reverses any operation, and a conflict is
//! recorded in the change instead of stopping the work.
//!
//! The command is idempotent. It installs `jj` through Homebrew when it is not
//! on `PATH` (macOS only, the CLI's one development host), colocates the store
//! once, and rewrites the same repo-level settings on every run. Those settings
//! are derived from the checkout's Git config, so jj commits carry the same
//! author and the same signature the forge's merge rules already require.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::lifecycle::git;

/// The bookmark `jj git push --change` names, in the `push/` namespace the
/// `navigator project repository push` lane already branches into, so one
/// forge rule over `push/*` covers both.
pub const PUSH_BOOKMARK_TEMPLATE: &str = r#""push/" ++ change_id.short()"#;

/// The Git settings jj needs to mirror. Read once from the checkout.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GitIdentity {
    pub name: Option<String>,
    pub email: Option<String>,
    pub sign_commits: bool,
    pub signing_format: Option<String>,
    pub signing_key: Option<String>,
}

impl GitIdentity {
    fn read(dir: &Path) -> Result<Self> {
        let get = |key: &str| -> Result<Option<String>> {
            let output = git(dir, &["config", "--get", key])?;
            let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Ok((output.status.success() && !value.is_empty()).then_some(value))
        };
        Ok(Self {
            name: get("user.name")?,
            email: get("user.email")?,
            sign_commits: get("commit.gpgsign")?.is_some_and(|v| v.eq_ignore_ascii_case("true")),
            signing_format: get("gpg.format")?,
            signing_key: get("user.signingkey")?,
        })
    }
}

/// The repo-level jj settings for a checkout whose Git config is `identity`.
///
/// An author is required: jj refuses to push a change with no name or email,
/// and the forge would attribute it to nobody. Signing follows `commit.gpgsign`
/// so a jj change is signed exactly when a Git commit would be.
pub fn jj_settings(identity: &GitIdentity) -> Result<Vec<(&'static str, String)>> {
    let (Some(name), Some(email)) = (&identity.name, &identity.email) else {
        bail!(
            "set git's user.name and user.email in this checkout first; jj commits need an author"
        );
    };
    let mut settings = vec![
        ("user.name", name.clone()),
        ("user.email", email.clone()),
        (
            "templates.git_push_bookmark",
            PUSH_BOOKMARK_TEMPLATE.to_string(),
        ),
    ];
    if identity.sign_commits {
        let backend = match identity.signing_format.as_deref() {
            Some("ssh") => "ssh",
            Some("x509") => "gpgsm",
            _ => "gpg",
        };
        settings.push(("signing.behavior", "own".to_string()));
        settings.push(("signing.backend", backend.to_string()));
        if let Some(key) = &identity.signing_key {
            settings.push(("signing.key", key.clone()));
        }
    }
    Ok(settings)
}

/// Refuse every host but macOS: the CLI targets macOS for development, and
/// Homebrew is how this command acquires `jj`.
pub fn require_macos(os: &str) -> Result<()> {
    if os == "macos" {
        Ok(())
    } else {
        bail!("`navigator project repository jj` installs jj through Homebrew and runs on macOS only (this host reports `{os}`)")
    }
}

/// Install `jj` if needed, colocate it over the checkout at `dir`, and write
/// its repo-level settings.
pub fn run(dir: &Path) -> Result<()> {
    require_macos(std::env::consts::OS)?;
    if !dir.join(".git").exists() {
        bail!(
            "{} is not a Git checkout; clone the Project repository first",
            dir.display()
        );
    }
    let settings = jj_settings(&GitIdentity::read(dir)?)?;

    if Command::new("jj").arg("--version").output().is_ok() {
        println!("jj: already installed");
    } else {
        crate::host::require_homebrew()?;
        crate::host::brew(&["install", "jj"])?;
        println!("jj: installed with Homebrew");
    }

    if dir.join(".jj").is_dir() {
        println!("colocate: already a jj repository");
    } else {
        jj(dir, &["git", "init", "--colocate"])?;
        println!("colocate: jj now tracks this Git checkout");
    }

    for (key, value) in &settings {
        jj(dir, &["config", "set", "--repo", key, &toml_string(value)])?;
    }
    println!("config: author, signing, and push bookmarks follow this checkout's Git config");
    Ok(())
}

/// `jj config set` parses its value as TOML, so a template such as
/// [`PUSH_BOOKMARK_TEMPLATE`] must arrive as one quoted TOML string.
fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_string()).to_string()
}

fn jj(dir: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("jj")
        .current_dir(dir)
        .args(args)
        .output()
        .with_context(|| format!("run jj {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "jj {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn author() -> GitIdentity {
        GitIdentity {
            name: Some("Ada Lawyer".into()),
            email: Some("ada@example.com".into()),
            ..GitIdentity::default()
        }
    }

    #[test]
    fn settings_copy_the_git_author_and_name_push_bookmarks() {
        let settings = jj_settings(&author()).unwrap();
        assert_eq!(
            settings,
            vec![
                ("user.name", "Ada Lawyer".to_string()),
                ("user.email", "ada@example.com".to_string()),
                (
                    "templates.git_push_bookmark",
                    PUSH_BOOKMARK_TEMPLATE.to_string()
                ),
            ]
        );
    }

    #[test]
    fn a_checkout_with_no_author_is_refused() {
        let error = jj_settings(&GitIdentity::default()).unwrap_err();
        assert!(error.to_string().contains("user.email"), "{error}");
    }

    #[test]
    fn signing_follows_commit_gpgsign_and_the_git_signing_format() {
        let identity = GitIdentity {
            sign_commits: true,
            signing_format: Some("ssh".into()),
            signing_key: Some("~/.ssh/id_ed25519.pub".into()),
            ..author()
        };
        let settings = jj_settings(&identity).unwrap();
        assert!(settings.contains(&("signing.behavior", "own".to_string())));
        assert!(settings.contains(&("signing.backend", "ssh".to_string())));
        assert!(settings.contains(&("signing.key", "~/.ssh/id_ed25519.pub".to_string())));

        let unsigned = jj_settings(&author()).unwrap();
        assert!(unsigned.iter().all(|(key, _)| !key.starts_with("signing.")));
    }

    #[test]
    fn values_reach_jj_as_toml_strings() {
        let quoted = toml_string(PUSH_BOOKMARK_TEMPLATE);
        let parsed: toml::Value = toml::from_str(&format!("v = {quoted}")).unwrap();
        assert_eq!(parsed["v"].as_str(), Some(PUSH_BOOKMARK_TEMPLATE));
    }

    #[test]
    fn only_macos_installs() {
        assert!(require_macos("macos").is_ok());
        let error = require_macos("linux").unwrap_err();
        assert!(error.to_string().contains("macOS only"), "{error}");
    }
}
