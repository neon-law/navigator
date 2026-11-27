//! Compose court paper for the PDF that counsel approves for signature.

use crate::retainer_walk::WorkflowDriveError;
use store::application_store::ApplicationStore;
use uuid::Uuid;

pub(crate) async fn source(
    db: &ApplicationStore,
    project: Uuid,
    draft: &str,
    mut body: String,
) -> Result<String, WorkflowDriveError> {
    let incomplete = |message: &str| WorkflowDriveError::Unfinished(vec![message.to_owned()]);
    let mut matter = store::matter_captions::for_project(db, project)
        .await
        .map_err(|error| WorkflowDriveError::Db(error.to_string()))?
        .ok_or_else(|| {
            incomplete("Sync the Project's Matter Caption before approving the pleading")
        })?
        .matter;
    let filing = store::pleading::filing_from(draft);
    let mut missing = matter.missing();
    missing.extend(filing.missing());
    if !missing.is_empty() {
        return Err(WorkflowDriveError::Unfinished(missing));
    }
    store::pleading::require_assigned(&matter).map_err(|message| incomplete(&message))?;
    let fm = rules::frontmatter::extract(draft).unwrap_or_default();
    let field =
        |key: &str| rules::frontmatter::field(fm, key).filter(|value| !value.trim().is_empty());
    if let Some(docket) =
        field("docket").filter(|value| !value.eq_ignore_ascii_case(&matter.case_number))
    {
        matter.case_number = store::cases::for_project(db, project)
            .await
            .map_err(|error| WorkflowDriveError::Db(error.to_string()))?
            .into_iter()
            .filter_map(|case| case.docket_number)
            .find(|number| number.eq_ignore_ascii_case(&docket))
            .ok_or_else(|| incomplete("Sync the pleading's docket to this Project"))?;
    }
    if word::paper::has_counsel_signature(&body) {
        let date = matter
            .signed_on
            .as_deref()
            .and_then(word::matter::date_line)
            .ok_or_else(|| incomplete("Set the counsel signing date in matter.signed_on"))?;
        body = word::paper::sign_counsel_signatures(
            &word::paper::insert_before_counsel_signatures(&body, &date),
            &matter.counsel.name,
        );
    }
    if let Some(slug) = field("served_document") {
        let slug = slug
            .strip_prefix("blobs/")
            .or_else(|| slug.strip_prefix("documents/"))
            .unwrap_or(&slug);
        let revision = store::assets::current(db, project, slug, store::assets::Lens::Lawyer)
            .await
            .map_err(|error| WorkflowDriveError::Db(error.to_string()))?
            .ok_or_else(|| {
                incomplete("File the served document before approving its certificate")
            })?;
        let records = store::project_service::list(db, project)
            .await
            .map_err(|error| WorkflowDriveError::Db(error.to_string()))?
            .into_iter()
            .filter(|row| row.document_id == revision.id)
            .map(|row| row.record())
            .collect::<Vec<_>>();
        let certificate = store::pleading::certificate(&filing.caption_title, &matter, &records)
            .ok_or_else(|| {
                incomplete("Record service of the document before approving its certificate")
            })?;
        body.push_str("\n\n");
        body.push_str(&certificate);
    }
    court_paper(
        &body,
        &matter,
        &filing,
        field("jurisdiction").as_deref().unwrap_or_default(),
        field("pleading_frame").as_deref(),
    )
}

fn court_paper(
    body: &str,
    matter: &word::matter::Matter,
    filing: &word::matter::Filing,
    jurisdiction: &str,
    frame: Option<&str>,
) -> Result<String, WorkflowDriveError> {
    let incomplete = |message: &str| WorkflowDriveError::Unfinished(vec![message.to_owned()]);
    let variant = match frame {
        Some(frame) => pdf::pleading::Variant::parse(frame)
            .ok_or_else(|| incomplete("Unknown pleading_frame"))?,
        None => pdf::pleading::variant_for(jurisdiction, &matter.court),
    };
    let caption = word::matter::compose(matter, filing, variant.caption_style(jurisdiction));
    let paper = word::paper::parse(&format!("{}{body}", caption.to_fence()))
        .map_err(|error| incomplete(&error.to_string()))?;
    let format = pdf::OutputFormat::Pleading(variant);
    Ok(format!(
        "{}{}{}",
        format.preamble(&pdf::Letterhead::default()),
        pdf::pleading::chrome(variant, &paper),
        format.document_source(&paper.body)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn approved_pleading_has_court_caption_and_both_consent_tabs_in_its_pdf() {
        let matter = word::matter::Matter {
            case_number: "CASE-EXAMPLE".into(),
            court: vec!["Example Court".into()],
            ..Default::default()
        };
        let filing = word::matter::Filing {
            caption_title: "Consent".into(),
            footer_title: "Consent".into(),
            ..Default::default()
        };
        let source = court_paper(
            "{{client.signature}}\n\n{{prior_counsel.signature}}",
            &matter,
            &filing,
            "CA",
            None,
        )
        .unwrap();
        let (source, fields) = crate::signature_render::expand_signatures(&source);
        assert_eq!(
            fields
                .iter()
                .map(|field| field.recipient_role.as_str())
                .collect::<Vec<_>>(),
            ["client", "prior_counsel"]
        );
        assert!(source.contains("CASE-EXAMPLE"));
        let pdf = pdf::render(&source).unwrap();
        crate::signature::validate_anchors(&pdf, &fields).unwrap();
    }
}
