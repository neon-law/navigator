//! `navigator project service add|list|show` — a Project's
//! [Service](../../../docs/glossary/service.md) records through the portal
//! API, and [`certificate_for`], the certificate of service a pleading
//! render composes from the committed `service:` list on the served
//! document's pointer.
//!
//! The Project comes from `navigator.yaml` the way `project deadline` reads
//! it. `add --dry-run` prints the request and sends nothing; every write
//! prints the row the store read back.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    projects::{manifest, surfaces},
    remote,
};

async fn coordinates(host: Option<&str>, project: Option<&str>) -> Result<(String, String, Uuid)> {
    let local = manifest::read(Path::new("."));
    let project = project
        .or_else(|| local.as_ref().and_then(|m| m.project.as_deref()))
        .ok_or_else(|| anyhow!("pass --project or run from a Project checkout"))?;
    let host = host.or_else(|| local.as_ref().and_then(|m| m.host.as_deref()));
    let (base, token) = remote::resolve(host)?;
    let id = surfaces::resolve_project_id(&base, &token, project).await?;
    Ok((base, token, id))
}

async fn call(request: reqwest::RequestBuilder, action: &str) -> Result<Value> {
    let response = request
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .with_context(|| format!("{action} service record"))?;
    let status = response.status();
    let body = response.text().await.context("read service response")?;
    if !status.is_success() {
        return Err(anyhow!("service {action} refused: {status} {body}"));
    }
    serde_json::from_str(&body).context("parse service response")
}

fn finish(result: Result<Value>) -> ExitCode {
    match result {
        Ok(row) => {
            println!("{row}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator project service: {error:#}");
            ExitCode::FAILURE
        }
    }
}

pub async fn list(host: Option<&str>, project: Option<&str>) -> ExitCode {
    finish(
        async {
            let (base, token, id) = coordinates(host, project).await?;
            call(
                reqwest::Client::new()
                    .get(format!("{base}/app/api/projects/{id}/service"))
                    .bearer_auth(token),
                "list",
            )
            .await
        }
        .await,
    )
}

pub async fn show(host: Option<&str>, project: Option<&str>, id: Uuid) -> ExitCode {
    finish(
        async {
            let (base, token, project_id) = coordinates(host, project).await?;
            call(
                reqwest::Client::new()
                    .get(format!("{base}/app/api/projects/{project_id}/service/{id}"))
                    .bearer_auth(token),
                "show",
            )
            .await
        }
        .await,
    )
}

/// What `navigator project service add` records.
#[derive(Debug, Clone, Default)]
pub struct NewService {
    pub document: Option<Uuid>,
    pub slug: Option<String>,
    pub recipient: String,
    pub recipient_person: Option<Uuid>,
    pub recipient_entity: Option<Uuid>,
    pub counsel_for: Option<String>,
    pub method: String,
    pub served_on: String,
    pub address: String,
    pub declarant: String,
    pub declarant_person: Option<Uuid>,
    pub letter: Option<Uuid>,
    pub sent_email: Option<Uuid>,
}

fn add_body(new: &NewService) -> Result<Value> {
    let mut body = json!({
        "recipient": new.recipient,
        "method": new.method,
        "served_on": new.served_on,
        "address": new.address,
        "declarant": new.declarant,
    });
    match (new.document, new.slug.as_deref()) {
        (Some(id), _) => body["document_id"] = json!(id),
        (None, Some(slug)) => body["slug"] = json!(slug),
        (None, None) => {
            return Err(anyhow!(
                "pass --document <asset id> or --slug <path below documents/>"
            ))
        }
    }
    if let Some(counsel_for) = &new.counsel_for {
        body["counsel_for"] = json!(counsel_for);
    }
    let ids = [
        ("recipient_person_id", new.recipient_person),
        ("recipient_entity_id", new.recipient_entity),
        ("declarant_person_id", new.declarant_person),
        ("letter_id", new.letter),
        ("sent_email_id", new.sent_email),
    ];
    for (key, value) in ids {
        if let Some(value) = value {
            body[key] = json!(value);
        }
    }
    Ok(body)
}

pub async fn add(
    host: Option<&str>,
    project: Option<&str>,
    new: NewService,
    dry_run: bool,
) -> ExitCode {
    finish(
        async {
            let body = add_body(&new)?;
            if dry_run {
                eprintln!(
                    "dry run: `navigator project service add` would send this and write nothing"
                );
                return Ok(body);
            }
            let (base, token, id) = coordinates(host, project).await?;
            call(
                reqwest::Client::new()
                    .post(format!("{base}/app/api/projects/{id}/service"))
                    .bearer_auth(token)
                    .json(&body),
                "add",
            )
            .await
        }
        .await,
    )
}

/// The frontmatter key that names the served document's slug.
pub const SERVED_DOCUMENT_KEY: &str = "served_document";

/// The certificate of service for the pleading at `file` whose source is
/// `contents`, composed from the `service:` list on the committed pointer of
/// the document its frontmatter names as `served_document:`.
///
/// Returns `Ok(None)` when the pleading names no served document. With a
/// named document and no records, a preview omits the certificate
/// (`Ok(None)`) and a final render is refused by name.
///
/// # Errors
///
/// A message naming the pointer that is missing or carries no records, or
/// the manifest field the declarant block needs.
pub fn certificate_for(
    file: &Path,
    contents: &str,
    preview: bool,
) -> Result<Option<String>, String> {
    let served_document = rules::frontmatter::extract(contents)
        .and_then(|fm| rules::frontmatter::field(fm, SERVED_DOCUMENT_KEY))
        .map(|value| {
            let value = value.trim();
            value
                .strip_prefix("blobs/")
                .or_else(|| value.strip_prefix("documents/"))
                .unwrap_or(value)
                .to_string()
        })
        .filter(|value| !value.is_empty());
    let Some(slug) = served_document else {
        return Ok(None);
    };
    let manifest_path = super::matter::find_manifest(file).ok_or_else(|| {
        format!(
            "no {} found above this pleading; `{SERVED_DOCUMENT_KEY}:` is read relative to it",
            manifest::FILE
        )
    })?;
    let root = manifest_path
        .parent()
        .ok_or_else(|| format!("{} has no parent", manifest_path.display()))?;
    let pointer_path = crate::document_sync::pointer_path_for_slug(root, Path::new(&slug));
    let pointer = match std::fs::read_to_string(&pointer_path) {
        Ok(raw) => store::document_pointers::DocumentPointer::from_yaml(&raw)
            .map_err(|error| format!("{}: {error}", pointer_path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "`{SERVED_DOCUMENT_KEY}: {slug}` names no committed pointer at {}; file the \
                 document and run `navigator project sync`",
                pointer_path.display()
            ))
        }
        Err(error) => return Err(format!("read {}: {error}", pointer_path.display())),
    };
    if pointer.service.is_empty() {
        if preview {
            return Ok(None);
        }
        return Err(format!(
            "no service is recorded for documents/{slug}: its pointer {} carries no `service:` \
             records, so the certificate of service cannot be composed; record each recipient \
             with `navigator project service add` and run `navigator project sync`",
            pointer_path.display()
        ));
    }
    let matter = super::matter::read(&manifest_path)?;
    let filing = super::matter::filing_from(contents);
    let title = if filing.caption_title.trim().is_empty() {
        slug.clone()
    } else {
        filing.caption_title.clone()
    };
    let served: Vec<word::service::Served> = pointer
        .service
        .iter()
        .map(|record| word::service::Served {
            recipient: record.recipient.clone(),
            counsel_for: record.counsel_for.clone(),
            method: record.method.certificate_phrase().to_string(),
            served_on: court_date(record.served_on),
            address: record.address.clone(),
        })
        .collect();
    let executed_on = pointer
        .service
        .iter()
        .map(|record| record.served_on)
        .max()
        .map(court_date)
        .unwrap_or_default();
    let declarant_name = pointer
        .service
        .first()
        .map(|record| record.declarant.clone())
        .unwrap_or_default();
    let executed_at = matter.counsel.address.last().cloned().unwrap_or_default();
    let declarant = word::service::Declarant {
        name: declarant_name,
        business_address: matter.counsel.address.join(", "),
        executed_on,
        executed_at,
    };
    Ok(word::service::compose(&title, &served, &declarant))
}

/// `2026-03-05` as a court writes it: `March 5, 2026`.
fn court_date(date: chrono::NaiveDate) -> String {
    word::matter::display_date(&date.format("%Y-%m-%d").to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{add_body, NewService};

    #[test]
    fn the_add_body_names_the_document_once_and_carries_only_set_ids() {
        let body = add_body(&NewService {
            slug: Some("pleadings/motion.pdf".into()),
            recipient: "Example Counsel".into(),
            method: "mail".into(),
            served_on: "2026-03-05".into(),
            address: "200 Sample Street".into(),
            declarant: "Pat Sample".into(),
            counsel_for: Some("Defendant".into()),
            ..NewService::default()
        })
        .unwrap();
        assert_eq!(body["slug"], "pleadings/motion.pdf");
        assert_eq!(body["counsel_for"], "Defendant");
        assert!(body.get("document_id").is_none());
        assert!(body.get("letter_id").is_none());
        assert!(add_body(&NewService::default()).is_err());
    }
}
