//! `navigator project skill feedback`: prepare a Kit defect report for the
//! Lawyers team's outside tracker, from inside the matter.
//!
//! The draft never leaves this process. The command reads the matter's own
//! `navigator.yaml` for Kit provenance, resolves the matter's private terms
//! through the same pre-filing door `navigator project feedback-check` uses,
//! and scans the draft with the shared [`cli::feedback_scan`] scanner. A clean
//! draft prints the prepared issue; a draft carrying an identifier prints the
//! placeholder to use instead and exits non-zero. Filing itself stays with
//! the tracker: the command prepares, a person or an agent files.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{anyhow, Result};

use super::{feedback_check, manifest};
use crate::remote;

/// The outside-tracker destination the canonical repository contract names
/// for wrong templates. The command prepares the report; filing stays with
/// the tracker, and this names that destination in the printed footer.
const DESTINATION: &str = "the Lawyers team's issue tracker";

/// Read the draft — `file`, or stdin when `None` — exactly as
/// `feedback-check` does, so one habit covers both commands.
fn read_draft(file: Option<PathBuf>) -> Result<String> {
    let text = match file {
        Some(path) => std::fs::read_to_string(path),
        None => std::io::read_to_string(std::io::stdin()),
    };
    text.map_err(|error| anyhow!("could not read the draft: {error}"))
}

/// The pinned Kit to report against: the one `skills:` pin, or the pin the
/// caller names. A matter with no pin has no Kit to blame; a matter with
/// several must name one, or the report cannot state which template is wrong.
fn select_pin<'m>(
    pins: &'m [manifest::SkillPin],
    jurisdiction: Option<&str>,
    practice_area: Option<&str>,
) -> Result<&'m manifest::SkillPin> {
    if pins.is_empty() {
        return Err(anyhow!(
            "no Kit is pinned: run `navigator project skill use` first"
        ));
    }
    let matched: Vec<&manifest::SkillPin> = pins
        .iter()
        .filter(|pin| {
            jurisdiction.is_none_or(|j| pin.jurisdiction.eq_ignore_ascii_case(j))
                && practice_area.is_none_or(|a| pin.practice_area.eq_ignore_ascii_case(a))
        })
        .collect();
    match matched.as_slice() {
        [one] => Ok(one),
        [] => Err(anyhow!(
            "no pinned Kit matches that jurisdiction and practice area"
        )),
        [first, rest @ ..] => Err(anyhow!(
            "several Kits are pinned ({} through {}); name the one to report with \
             --jurisdiction and --practice-area",
            first.practice_area,
            rest.last().map_or(first.practice_area.as_str(), |last| last
                .practice_area
                .as_str())
        )),
    }
}

/// The prepared issue: Kit provenance on top, the lawyer's draft under it,
/// the filing destination on the bottom. Provenance names the catalog pair
/// and version only — never the Firm, its economics, or the matter's
/// participants.
fn prepared_issue(pin: &manifest::SkillPin, draft: &str) -> String {
    format!(
        "Kit {} {} version {}: template defect report\n\n{}\n\nFile with {DESTINATION}.",
        pin.jurisdiction, pin.practice_area, pin.version, draft
    )
}

/// Outcome of one run. `Scan` keeps the findings' count for the exit banner.
enum Outcome {
    Prepared,
    Identifier(usize),
    Fatal(anyhow::Error),
}

/// Run the feedback flow. Exit 0 prints the prepared issue for the tracker.
/// Exit 1 names each placeholder a scan finding requires; exit 2 when the
/// draft, manifest, or private terms cannot be read.
pub async fn run(
    file: Option<PathBuf>,
    jurisdiction: Option<String>,
    practice_area: Option<String>,
) -> ExitCode {
    match run_checked(file, jurisdiction.as_deref(), practice_area.as_deref()).await {
        Outcome::Prepared => ExitCode::SUCCESS,
        Outcome::Identifier(count) => {
            eprintln!(
                "navigator: {count} identifier{} found; replace each and re-run",
                if count == 1 { "" } else { "s" }
            );
            ExitCode::FAILURE
        }
        Outcome::Fatal(error) => {
            eprintln!("navigator: {error:#}");
            ExitCode::from(2)
        }
    }
}

async fn run_checked(
    file: Option<PathBuf>,
    jurisdiction: Option<&str>,
    practice_area: Option<&str>,
) -> Outcome {
    // 1. The draft.
    let draft = match read_draft(file) {
        Ok(draft) => draft,
        Err(error) => return Outcome::Fatal(error),
    };
    // 2. The matter's manifest: pins for provenance, host and project for the
    //    private-terms door.
    let Some(parsed) = manifest::read(Path::new(".")) else {
        return Outcome::Fatal(anyhow!(
            "no readable navigator.yaml here; run from the Project repository root"
        ));
    };
    let pin = match select_pin(&parsed.skills, jurisdiction, practice_area) {
        Ok(pin) => pin.clone(),
        Err(error) => return Outcome::Fatal(error),
    };
    // 3. The matter's private terms, through the shared pre-filing door. The
    //    check is strict, like feedback-check: a scan that cannot read the
    //    terms cannot promise coverage.
    let Some(host) = parsed.host.as_deref() else {
        return Outcome::Fatal(anyhow!(
            "navigator.yaml names no host; privacy coverage is incomplete"
        ));
    };
    let Some(project) = parsed.project.clone() else {
        return Outcome::Fatal(anyhow!(
            "navigator.yaml names no project; privacy coverage is incomplete"
        ));
    };
    let outcomes = match remote::resolve(Some(host)) {
        Ok((base, token)) => feedback_check::private_terms(&base, &token, &[project]).await,
        Err(_) => {
            return Outcome::Fatal(anyhow!(
                "site login unavailable or expired; privacy coverage is incomplete"
            ));
        }
    };
    let Ok(outcomes) = outcomes else {
        return Outcome::Fatal(anyhow!(
            "could not read private terms; privacy coverage is incomplete"
        ));
    };
    // The check is strict, like feedback-check: one named matter has
    // nothing to continue past, so a refusal is fatal and its reason —
    // which names the Project — is the message.
    let mut terms = Vec::new();
    for outcome in &outcomes {
        match &outcome.read {
            feedback_check::ProjectRead::Resolved(resolved) => terms.extend(resolved.clone()),
            feedback_check::ProjectRead::Refused { reason } => {
                return Outcome::Fatal(anyhow!("{reason}"));
            }
        }
    }
    // 4. The scan, over the draft with the matter's terms merged in.
    let merged = cli::pii_audit::merge_terms(&terms, &[]);
    let findings = cli::feedback_scan::scan(&draft, &merged);
    if findings.is_empty() {
        println!("{}", prepared_issue(&pin, &draft));
        return Outcome::Prepared;
    }
    for finding in &findings {
        eprintln!(
            "line {}: {} found; write {} instead",
            finding.line, finding.kind, finding.placeholder
        );
    }
    Outcome::Identifier(findings.len())
}

#[cfg(test)]
mod tests {
    use super::{prepared_issue, select_pin};
    use crate::projects::manifest::SkillPin;

    fn pin(jurisdiction: &str, practice_area: &str, version: &str) -> SkillPin {
        SkillPin {
            jurisdiction: jurisdiction.to_string(),
            practice_area: practice_area.to_string(),
            version: version.to_string(),
        }
    }

    /// The single pin is the default subject; a name selects case-insensitively.
    #[test]
    fn one_pin_selects_itself_and_a_name_selects_among_many() {
        let pins = vec![pin("NV", "estates", "1")];
        assert_eq!(select_pin(&pins, None, None).unwrap().version, "1");

        let pins = vec![
            pin("NV", "estates", "1"),
            pin("ny", "uncontested-divorce", "1.0.0"),
        ];
        assert_eq!(
            select_pin(&pins, Some("NY"), Some("UNCONTESTED-DIVORCE"))
                .unwrap()
                .version,
            "1.0.0"
        );
        assert!(select_pin(&pins, Some("TX"), None).is_err());
        assert!(select_pin(&pins, None, None).is_err());
    }

    /// No pin means no Kit to blame, and the message points at the cure.
    #[test]
    fn no_pin_is_refused_with_the_cure_named() {
        let pins: Vec<SkillPin> = Vec::new();
        let error = select_pin(&pins, None, None).unwrap_err().to_string();
        assert!(error.contains("navigator project skill use"), "{error}");
    }

    /// The prepared issue carries Kit provenance and the destination, and
    /// nothing else the command knows about the matter.
    #[test]
    fn a_prepared_issue_names_the_kit_and_the_destination_only() {
        let issue = prepared_issue(
            &pin("NV", "estates", "1"),
            "Section 3 cites the wrong statute.",
        );
        assert!(
            issue.starts_with("Kit NV estates version 1: template defect report"),
            "{issue}"
        );
        assert!(issue.contains("wrong statute"), "{issue}");
        assert!(
            issue.ends_with("File with the Lawyers team's issue tracker."),
            "{issue}"
        );
    }
}
