//! Shared rendering for the nightly project DRI operations notice.

use store::projects::ProjectDriSummary;

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

fn names_or_none(names: &[String]) -> String {
    if names.is_empty() {
        "none".to_owned()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::dri_digest_message;
    use store::projects::ProjectDriSummary;

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
