//! `navigator project docket list|add|link-document|supersede` — a Project's
//! Docket Entries through the portal API. Each write prints the row the
//! store read back; `--dry-run` prints the request it would send and sends
//! nothing.
//!
//! The Project comes from `navigator.yaml` the way `project deadline` reads
//! it. The case an entry joins comes from the manifest too: `--docket` picks
//! one of the `dockets:` case numbers, and the only one is used when there is
//! exactly one. A Project with no `case` row yet gets one opened by the
//! server from that number on the first write, captioned from `matter:`.

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

/// The deployment, the Project, and the manifest the verbs read.
pub(crate) struct Coordinates {
    pub(crate) base: String,
    pub(crate) token: String,
    pub(crate) project_id: Uuid,
    pub(crate) project_code: String,
}

pub(crate) async fn coordinates(host: Option<&str>, project: Option<&str>) -> Result<Coordinates> {
    let local = manifest::read(Path::new("."));
    let project = project
        .or_else(|| local.as_ref().and_then(|m| m.project.as_deref()))
        .ok_or_else(|| anyhow!("pass --project or run from a Project checkout"))?;
    let host = host.or_else(|| local.as_ref().and_then(|m| m.host.as_deref()));
    let (base, token) = remote::resolve(host)?;
    let project_id = surfaces::resolve_project_id(&base, &token, project).await?;
    Ok(Coordinates {
        base,
        token,
        project_id,
        project_code: project.to_string(),
    })
}

/// The case a write names, read from `navigator.yaml` at `root`: the
/// `--docket` case number, or the manifest's only one. The caption is the
/// `matter:` parties joined with `v.` when the block is present, so a case
/// the server opens on the first write reads like the court's own.
pub(crate) fn case_coordinates(root: &Path, docket: Option<&str>) -> Result<Value> {
    let path = root.join(manifest::FILE);
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("read {} — run this from a Project checkout", manifest::FILE))?;
    let parsed = manifest::parse(&contents).map_err(|error| anyhow!(error))?;
    let document: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap_or_default();
    let matter = document.get(crate::projects::matter::KEY);
    let mut dockets = parsed.dockets;
    if let Some(case) = matter
        .and_then(|matter| matter.get("case_number"))
        .and_then(serde_yaml::Value::as_str)
        .map(str::trim)
        .filter(|case| !case.is_empty())
    {
        if !dockets.iter().any(|known| known.eq_ignore_ascii_case(case)) {
            dockets.push(case.to_string());
        }
    }
    let docket_number = match docket {
        Some(wanted) => dockets
            .iter()
            .find(|known| known.eq_ignore_ascii_case(wanted))
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "`--docket` names a case number {} does not list under `dockets:`",
                    manifest::FILE
                )
            })?,
        None => match dockets.as_slice() {
            [only] => only.clone(),
            [] => {
                return Err(anyhow!(
                    "{} lists no dockets; add the case number under `dockets:`",
                    manifest::FILE
                ))
            }
            _ => {
                return Err(anyhow!(
                    "{} lists several dockets; pass --docket <case number>",
                    manifest::FILE
                ))
            }
        },
    };
    let mut body = json!({ "docket_number": docket_number });
    if let Some(matter) = matter {
        let parties: Vec<String> = matter
            .get("parties")
            .and_then(serde_yaml::Value::as_sequence)
            .map(|parties| {
                parties
                    .iter()
                    .filter_map(|party| party.get("name")?.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if parties.len() >= 2 {
            body["caption"] = json!(format!("{} v. {}", parties[0], parties[1]));
        }
        let court: Vec<&str> = matter
            .get("court")
            .and_then(serde_yaml::Value::as_sequence)
            .map(|lines| lines.iter().filter_map(serde_yaml::Value::as_str).collect())
            .unwrap_or_default();
        if !court.is_empty() {
            body["forum"] = json!(court.join(", "));
        }
        if let Some(judge) = matter
            .get("judicial_officer")
            .and_then(serde_yaml::Value::as_str)
        {
            body["judge"] = json!(judge);
        }
    }
    Ok(body)
}

async fn call(request: reqwest::RequestBuilder, action: &str) -> Result<Value> {
    let response = request
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .with_context(|| format!("{action} docket entry"))?;
    let status = response.status();
    let body = response.text().await.context("read docket response")?;
    if !status.is_success() {
        return Err(anyhow!("docket {action} refused: {status} {body}"));
    }
    serde_json::from_str(&body).context("parse docket response")
}

/// Record an entry and attach an upload that arrived after a source-pending row.
pub(crate) async fn post_entry(coordinates: &Coordinates, body: &Value) -> Result<Value> {
    let row = call(
        reqwest::Client::new()
            .post(format!(
                "{}/app/api/projects/{}/docket",
                coordinates.base, coordinates.project_id
            ))
            .bearer_auth(&coordinates.token)
            .json(body),
        "add",
    )
    .await?;
    if row.get("document_asset_id") == Some(&Value::Null)
        && (body.get("slug").is_some() || body.get("document_id").is_some())
    {
        let entry = row["id"]
            .as_str()
            .context("pending Docket Entry has no id")?
            .parse()
            .context("parse pending Docket Entry id")?;
        let document = body
            .get("document_id")
            .and_then(Value::as_str)
            .map(str::parse)
            .transpose()
            .context("parse document id")?;
        return post_link(
            coordinates,
            entry,
            &document_body(document, body["slug"].as_str())?,
        )
        .await;
    }
    Ok(row)
}

async fn post_link(c: &Coordinates, entry: Uuid, body: &Value) -> Result<Value> {
    call(
        reqwest::Client::new()
            .post(format!(
                "{}/app/api/projects/{}/docket/{entry}/link-document",
                c.base, c.project_id
            ))
            .bearer_auth(&c.token)
            .json(body),
        "link-document",
    )
    .await
}

fn finish(result: Result<Value>) -> ExitCode {
    match result {
        Ok(row) => {
            println!("{row}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator project docket: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn dry_run(verb: &str, body: &Value) -> Value {
    eprintln!("dry run: `navigator project docket {verb}` would send this and write nothing");
    body.clone()
}

pub async fn list(host: Option<&str>, project: Option<&str>) -> ExitCode {
    finish(
        async {
            let c = coordinates(host, project).await?;
            call(
                reqwest::Client::new()
                    .get(format!(
                        "{}/app/api/projects/{}/docket",
                        c.base, c.project_id
                    ))
                    .bearer_auth(c.token),
                "list",
            )
            .await
        }
        .await,
    )
}

/// The Docket Entry rows of a Project, from `GET …/docket`.
pub(crate) async fn list_rows(c: &Coordinates) -> Result<Vec<Value>> {
    let body = call(
        reqwest::Client::new()
            .get(format!(
                "{}/app/api/projects/{}/docket",
                c.base, c.project_id
            ))
            .bearer_auth(&c.token),
        "list",
    )
    .await?;
    Ok(body
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// What `navigator project docket add` records.
#[derive(Debug, Clone, Default)]
pub struct NewEntry {
    pub docket: Option<String>,
    pub entry_number: String,
    pub kind: String,
    pub title: String,
    pub party: Option<String>,
    pub filed_on: Option<String>,
    pub scheduled_on: Option<String>,
    pub document: Option<Uuid>,
    pub slug: Option<String>,
}

fn entry_body(root: &Path, new: &NewEntry) -> Result<Value> {
    let mut body = case_coordinates(root, new.docket.as_deref())?;
    body["entry_number"] = json!(new.entry_number);
    body["kind"] = json!(new.kind);
    body["title"] = json!(new.title);
    let optional = [
        ("party", new.party.clone()),
        ("filed_or_served_on", new.filed_on.clone()),
        ("scheduled_on", new.scheduled_on.clone()),
        ("slug", new.slug.clone()),
    ];
    for (key, value) in optional {
        if let Some(value) = value {
            body[key] = json!(value);
        }
    }
    if let Some(document) = new.document {
        body["document_id"] = json!(document);
    }
    Ok(body)
}

pub async fn add(host: Option<&str>, project: Option<&str>, new: NewEntry, dry: bool) -> ExitCode {
    finish(
        async {
            let body = entry_body(Path::new("."), &new)?;
            if dry {
                return Ok(dry_run("add", &body));
            }
            let c = coordinates(host, project).await?;
            let row = post_entry(&c, &body).await?;
            super::renumber::name_linked_document(&c, &row, Path::new(".")).await?;
            super::timeline_write::write_after(Path::new("."), &c.project_code, &row)?;
            Ok(row)
        }
        .await,
    )
}

fn document_body(document: Option<Uuid>, slug: Option<&str>) -> Result<Value> {
    match (document, slug) {
        (Some(id), _) => Ok(json!({ "document_id": id })),
        (None, Some(slug)) => Ok(json!({ "slug": slug })),
        (None, None) => Err(anyhow!(
            "pass --document <asset id> or --slug <path below documents/>"
        )),
    }
}

pub async fn link_document(
    host: Option<&str>,
    project: Option<&str>,
    entry: Uuid,
    document: Option<Uuid>,
    slug: Option<&str>,
    dry: bool,
) -> ExitCode {
    finish(
        async {
            let body = document_body(document, slug)?;
            if dry {
                return Ok(dry_run("link-document", &body));
            }
            let c = coordinates(host, project).await?;
            let row = post_link(&c, entry, &body).await?;
            super::renumber::name_linked_document(&c, &row, Path::new(".")).await?;
            super::timeline_write::write_after(Path::new("."), &c.project_code, &row)?;
            Ok(row)
        }
        .await,
    )
}

/// What `navigator project docket supersede` records.
#[derive(Debug, Clone, Default)]
pub struct Continuance {
    pub entry_number: String,
    pub scheduled_on: String,
    pub title: Option<String>,
    pub party: Option<String>,
    pub filed_on: Option<String>,
    pub document: Option<Uuid>,
    pub slug: Option<String>,
}

pub async fn supersede(
    host: Option<&str>,
    project: Option<&str>,
    entry: Uuid,
    continuance: Continuance,
    dry: bool,
) -> ExitCode {
    finish(
        async {
            let mut body = json!({
                "entry_number": continuance.entry_number,
                "scheduled_on": continuance.scheduled_on,
            });
            let optional = [
                ("title", continuance.title),
                ("party", continuance.party),
                ("filed_or_served_on", continuance.filed_on),
                ("slug", continuance.slug),
            ];
            for (key, value) in optional {
                if let Some(value) = value {
                    body[key] = json!(value);
                }
            }
            if let Some(document) = continuance.document {
                body["document_id"] = json!(document);
            }
            if dry {
                return Ok(dry_run("supersede", &body));
            }
            let c = coordinates(host, project).await?;
            let row = call(
                reqwest::Client::new()
                    .post(format!(
                        "{}/app/api/projects/{}/docket/{entry}/supersede",
                        c.base, c.project_id
                    ))
                    .bearer_auth(c.token)
                    .json(&body),
                "supersede",
            )
            .await?;
            super::timeline_write::write_after(Path::new("."), &c.project_code, &row)?;
            Ok(row)
        }
        .await,
    )
}

#[cfg(test)]
mod tests {
    use super::{case_coordinates, entry_body, NewEntry};

    fn checkout(manifest: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("navigator.yaml"), manifest).unwrap();
        dir
    }

    #[test]
    fn the_only_docket_is_the_case_and_the_matter_captions_it() {
        let dir = checkout(
            "host: h.example.com\nproject: acme\ndockets:\n  - 9:26-cv-00042\nmatter:\n  \
             court:\n    - SUPERIOR COURT\n    - COUNTY OF SAMPLE\n  judicial_officer: Hon. Sample\n  \
             parties:\n    - name: ALPHA\n      role: Plaintiff\n    - name: BETA\n      role: Defendant\n",
        );
        let body = case_coordinates(dir.path(), None).unwrap();
        assert_eq!(body["docket_number"], "9:26-cv-00042");
        assert_eq!(body["caption"], "ALPHA v. BETA");
        assert_eq!(body["forum"], "SUPERIOR COURT, COUNTY OF SAMPLE");
        assert_eq!(body["judge"], "Hon. Sample");
    }

    #[test]
    fn several_dockets_need_a_choice_and_the_choice_must_be_listed() {
        let dir = checkout(
            "host: h.example.com\nproject: acme\ndockets:\n  - 9:26-cv-00042\n  - 26-10042\n",
        );
        let error = case_coordinates(dir.path(), None).unwrap_err();
        assert!(error.to_string().contains("--docket"), "{error}");
        let body = case_coordinates(dir.path(), Some("26-10042")).unwrap();
        assert_eq!(body["docket_number"], "26-10042");
        assert!(body.get("caption").is_none());
        let error = case_coordinates(dir.path(), Some("1:99-cv-1")).unwrap_err();
        assert!(error.to_string().contains("dockets:"), "{error}");
    }

    #[test]
    fn matter_case_number_counts_as_a_docket() {
        let dir =
            checkout("host: h.example.com\nproject: acme\nmatter:\n  case_number: 00CV000000\n");
        let body = entry_body(
            dir.path(),
            &NewEntry {
                entry_number: "12".into(),
                kind: "motion".into(),
                title: "Motion".into(),
                slug: Some("pleadings/motion.pdf".into()),
                ..NewEntry::default()
            },
        )
        .unwrap();
        assert_eq!(body["docket_number"], "00CV000000");
        assert_eq!(body["entry_number"], "12");
        assert_eq!(body["slug"], "pleadings/motion.pdf");
        assert!(body.get("scheduled_on").is_none());
    }
}
