//! The one reader of a Project repository's `navigator.yaml` `version:` pin,
//! and the one comparison of that pin against a running Navigator.
//!
//! `navigator project gate` (the `cli` crate) and the Nightly Project Update's
//! `repository` child (the `workflows-service` crate) both need to know what a
//! repository pins. Neither can depend on the other, so the minimal parse
//! lives here and both call it: the CLI's full manifest reader takes `version`
//! from [`pinned_version_in`], and the worker reads a committed file through
//! [`pinned_version`]. Everything else in the manifest — hosts, handles,
//! skills — stays the CLI's.

use std::cmp::Ordering;

use semver::Version;
use serde::{Deserialize, Serialize};

/// The filename a Project repository declares itself in.
pub const FILE: &str = "navigator.yaml";

/// `version:` out of a `navigator.yaml` document. `Ok(None)` when the key is
/// absent; `Err` when the document is not a YAML mapping or the value is not
/// non-empty text.
///
/// # Errors
///
/// A one-line reason, the same text the CLI reports for `Y012`'s neighbours.
pub fn pinned_version(contents: &str) -> Result<Option<String>, String> {
    let document: serde_yaml::Value = serde_yaml::from_str(contents)
        .map_err(|error| format!("{FILE} is not valid YAML: {error}"))?;
    let mapping = document
        .as_mapping()
        .ok_or_else(|| format!("{FILE} must be a mapping"))?;
    pinned_version_in(mapping)
}

/// [`pinned_version`] for a caller that already holds the parsed mapping.
///
/// # Errors
///
/// When `version:` is present but not non-empty text.
pub fn pinned_version_in(mapping: &serde_yaml::Mapping) -> Result<Option<String>, String> {
    mapping
        .get("version")
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
                .ok_or_else(|| "value must be non-empty text".to_string())
        })
        .transpose()
}

/// `contents` with its top-level `version:` set to `version`, every other
/// line kept byte for byte — comments, key order, and the rest of the
/// mapping. A trailing comment on the `version:` line survives too.
///
/// # Errors
///
/// When `contents` has no plain top-level `version:` line, or the rewrite
/// would change anything but the pin.
pub fn with_pinned_version(contents: &str, version: &str) -> Result<String, String> {
    let mut found = false;
    let mut rewritten = String::with_capacity(contents.len());
    for line in contents.split_inclusive('\n') {
        let Some(rest) = line.strip_prefix("version:").filter(|_| !found) else {
            rewritten.push_str(line);
            continue;
        };
        found = true;
        let ending = &rest[rest.trim_end_matches(['\r', '\n']).len()..];
        let comment = rest
            .find(" #")
            .map_or("", |at| rest[at..].trim_end_matches(['\r', '\n']));
        rewritten.push_str("version: ");
        rewritten.push_str(version);
        rewritten.push_str(comment);
        rewritten.push_str(ending);
    }
    if !found {
        return Err(format!("{FILE} has no top-level `version:` line"));
    }
    let parse = |text: &str| -> Result<serde_yaml::Mapping, String> {
        serde_yaml::from_str::<serde_yaml::Value>(text)
            .map_err(|error| format!("{FILE} is not valid YAML: {error}"))?
            .as_mapping()
            .cloned()
            .ok_or_else(|| format!("{FILE} must be a mapping"))
    };
    let mut before = parse(contents)?;
    let after = parse(&rewritten)?;
    before.insert("version".into(), version.into());
    if before != after || pinned_version_in(&after)?.as_deref() != Some(version) {
        return Err(format!("rewriting {FILE} would change more than its pin"));
    }
    Ok(rewritten)
}

/// How a repository's pin stands against the Navigator that read it.
/// Mechanism only: no repository URL, no Project name, no versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinStatus {
    /// The pin is the running release, or a newer one.
    Current,
    /// The pin is an older release than the one running.
    Stale,
    /// The manifest is missing, is not YAML, has no `version:`, or pins
    /// something that is not a release version.
    Unreadable,
}

impl PinStatus {
    /// The status as the aggregate report spells it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Stale => "stale",
            Self::Unreadable => "unreadable",
        }
    }
}

/// Compare a pin read by [`pinned_version`] against `runtime`, the version
/// of the Navigator doing the reading. A `None` pin — missing file, broken
/// YAML, absent key — is [`PinStatus::Unreadable`], as is a pin or runtime
/// that is not a release version. Ordering is semver's, so a release
/// candidate ranks below its own base.
#[must_use]
pub fn pin_status(pin: Option<&str>, runtime: &str) -> PinStatus {
    let (Some(pin), Ok(runtime)) = (
        pin.and_then(|pin| Version::parse(pin.trim()).ok()),
        Version::parse(runtime.trim()),
    ) else {
        return PinStatus::Unreadable;
    };
    match pin.cmp(&runtime) {
        Ordering::Less => PinStatus::Stale,
        Ordering::Equal | Ordering::Greater => PinStatus::Current,
    }
}

#[cfg(test)]
mod tests {
    use super::{pin_status, pinned_version, with_pinned_version, PinStatus};

    #[test]
    fn rewriting_the_pin_keeps_every_other_line() {
        let yaml = "# Project manifest\nversion: 26.9.13 # pinned\nproject:\n  host: h.example.com\n  version: nested\nskills: [nv/estates]\n";
        assert_eq!(
            with_pinned_version(yaml, "26.10.5").unwrap(),
            "# Project manifest\nversion: 26.10.5 # pinned\nproject:\n  host: h.example.com\n  version: nested\nskills: [nv/estates]\n"
        );
        assert_eq!(
            with_pinned_version("version: \"26.9.13\"\r\n", "26.10.5").unwrap(),
            "version: 26.10.5\r\n"
        );
    }

    #[test]
    fn rewriting_refuses_a_manifest_without_a_plain_top_level_pin() {
        for yaml in [
            "project:\n  version: 1.0.0\n",
            "{version: 1.0.0}\n",
            "version: [\n",
        ] {
            assert!(with_pinned_version(yaml, "26.10.5").is_err(), "{yaml}");
        }
        // A block scalar pin would leave its body behind.
        assert!(with_pinned_version("version: |\n  26.9.13\n", "26.10.5").is_err());
    }

    #[test]
    fn reads_the_pin_out_of_a_nested_manifest() {
        let yaml = "version: 26.10.4\nproject:\n  host: h.example.com\n  name: acme\n";
        assert_eq!(pinned_version(yaml).unwrap().as_deref(), Some("26.10.4"));
        assert_eq!(
            pinned_version("version: \"26.10.4\"\n").unwrap().as_deref(),
            Some("26.10.4")
        );
        assert_eq!(
            pinned_version("version: ' 26.10.4 '\n").unwrap().as_deref(),
            Some("26.10.4")
        );
    }

    #[test]
    fn a_manifest_without_a_version_reads_as_none() {
        assert_eq!(pinned_version("project:\n  name: acme\n").unwrap(), None);
        assert_eq!(pinned_version("").unwrap_or_default(), None);
    }

    #[test]
    fn broken_yaml_a_list_and_a_blank_version_are_errors() {
        assert!(pinned_version("version: [\n").is_err());
        assert!(pinned_version("- a\n- b\n").is_err());
        assert!(pinned_version("version: ''\n").is_err());
        assert!(pinned_version("version: [26.10.4]\n").is_err());
    }

    #[test]
    fn a_pin_equal_to_or_ahead_of_the_runtime_is_current() {
        assert_eq!(pin_status(Some("26.10.4"), "26.10.4"), PinStatus::Current);
        assert_eq!(pin_status(Some("26.10.5"), "26.10.4"), PinStatus::Current);
    }

    #[test]
    fn a_pin_behind_the_runtime_is_stale() {
        assert_eq!(pin_status(Some("26.9.13"), "26.10.4"), PinStatus::Stale);
        assert_eq!(
            pin_status(Some("26.10.4-rc.1"), "26.10.4"),
            PinStatus::Stale,
            "a release candidate ranks below its base"
        );
    }

    #[test]
    fn a_missing_or_non_release_pin_is_unreadable() {
        assert_eq!(pin_status(None, "26.10.4"), PinStatus::Unreadable);
        assert_eq!(pin_status(Some("latest"), "26.10.4"), PinStatus::Unreadable);
        assert_eq!(pin_status(Some("main"), "26.10.4"), PinStatus::Unreadable);
        assert_eq!(pin_status(Some("26.10.4"), "dev"), PinStatus::Unreadable);
    }

    #[test]
    fn the_status_spells_itself_for_the_report() {
        assert_eq!(PinStatus::Current.as_str(), "current");
        assert_eq!(PinStatus::Stale.as_str(), "stale");
        assert_eq!(PinStatus::Unreadable.as_str(), "unreadable");
        assert_eq!(
            serde_yaml::to_string(&PinStatus::Stale).unwrap().trim(),
            "stale"
        );
    }
}
