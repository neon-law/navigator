//! Shared rendering for the nightly project DRI operations notice.

use store::projects::{BrandOpenPitch, ProjectDriSummary};

use crate::firm_slack::DeploymentSlackFirm;

/// Render the nightly digest using the project code, status, and assigned DRI
/// markers. The display name is intentionally omitted from this internal
/// operations notice.
#[must_use]
pub fn dri_digest_message(projects: &[ProjectDriSummary], _simulated: bool) -> String {
    use std::fmt::Write as _;

    let mut out = format!("*Project DRI digest — {} projects*\n", projects.len());
    for project in projects {
        let lawyer = names_or_none(&project.lawyer_dris);
        let client = names_or_none(&project.client_dris);
        let _ = writeln!(
            out,
            "• *{}* ({}) — Lawyer DRI: {lawyer}; Client DRI: {client}",
            project.code, project.status
        );
    }
    out.trim_end().to_owned()
}

/// The nightly open-matters notice: the Slack Firm's total, then one line per
/// Brand it owns.
#[must_use]
pub fn open_matters_message(brands: &[BrandOpenPitch]) -> String {
    use std::fmt::Write as _;

    let open: usize = brands.iter().map(|line| line.open).sum();
    let pitches: usize = brands.iter().map(|line| line.pitches).sum();
    let mut out = format!("Total open matters: {open}, pitches: {pitches}");
    for line in brands {
        let _ = write!(
            out,
            "\n• {}: {} open, pitches: {}",
            line.brand, line.open, line.pitches
        );
    }
    out
}

/// The open/pitch counts of every Brand the deployment's Slack Firm owns —
/// the Firm whose channel reads the notice. No Slack Firm counts nothing.
///
/// # Errors
/// The Firm lookup or the count read failed.
pub async fn slack_firm_open_pitch_counts(
    surreal: &store::surreal::SurrealDb,
    firm: DeploymentSlackFirm,
) -> Result<Vec<BrandOpenPitch>, String> {
    let Some(firm_id) = firm.resolve(surreal).await.map_err(|e| e.to_string())? else {
        return Ok(Vec::new());
    };
    store::projects::matter_open_pitch_counts(surreal, firm_id)
        .await
        .map_err(|e| e.to_string())
}

fn names_or_none(names: &[String]) -> String {
    if names.is_empty() {
        "none".to_owned()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::{dri_digest_message, open_matters_message};
    use store::projects::{BrandOpenPitch, ProjectDriSummary};

    #[test]
    fn open_matters_notice_totals_the_brands_then_lists_each() {
        let line = |brand: &str, open, pitches| BrandOpenPitch {
            brand: brand.into(),
            open,
            pitches,
        };
        assert_eq!(
            open_matters_message(&[line("Neon Law", 3, 1), line("Same Day Legal", 0, 0)]),
            "Total open matters: 3, pitches: 1\n• Neon Law: 3 open, pitches: 1\n\
             • Same Day Legal: 0 open, pitches: 0"
        );
        assert_eq!(
            open_matters_message(&[]),
            "Total open matters: 0, pitches: 0"
        );
    }

    #[test]
    fn renders_both_dri_sides_and_omits_project_display_names() {
        let rendered = dri_digest_message(
            &[ProjectDriSummary {
                code: "sample-litigation".into(),
                name: "Private project display name".into(),
                status: "open".into(),
                lawyer_dris: vec!["Synthetic Lawyer DRI".into()],
                client_dris: vec!["Synthetic Client DRI".into()],
            }],
            false,
        );
        assert!(rendered.contains("Lawyer DRI: Synthetic Lawyer DRI"));
        assert!(rendered.contains("Client DRI: Synthetic Client DRI"));
        assert!(!rendered.contains("Private project display name"));
    }
}
