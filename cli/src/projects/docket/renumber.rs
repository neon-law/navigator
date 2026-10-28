//! `navigator project docket renumber` — the manifest of every existing
//! `di-<n>[-<att>]-<slug>.pdf` filing beside the `DKT NNNN - <title>` name its
//! Docket Entry gives it.
//!
//! The verb previews names; `--apply` reconciles linked documents. A filing is matched to its entry
//! through the pointer's asset id (the entry's `document_asset_id`), so the
//! new name always comes from the entry's own number and title. A filing no
//! entry links, and two filings that would take one name, are listed as
//! unresolved for a lawyer to settle.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use serde_json::Value;

use super::{case_timeline, entries, ingest};

/// One existing `di-` filing and the name it would take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub old_slug: String,
    pub new_name: Option<String>,
    /// Why there is no `new_name`.
    pub reason: Option<String>,
}

/// A committed `di-` filing: its slug and its pointer's asset id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filing {
    pub slug: String,
    pub asset_id: String,
}

fn extension(slug: &str) -> &str {
    Path::new(slug)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("pdf")
}

/// The complete listing, in slug order, one row per filing.
pub fn manifest(filings: &[Filing], entries: &[Value]) -> Vec<Row> {
    let by_asset: BTreeMap<String, &Value> = entries
        .iter()
        .filter_map(|entry| {
            let asset = entry.get("document_asset_id")?.as_str()?;
            Some((asset.to_ascii_lowercase(), entry))
        })
        .collect();
    let mut rows: Vec<Row> = filings
        .iter()
        .map(|filing| {
            let named = by_asset
                .get(&filing.asset_id.to_ascii_lowercase())
                .and_then(|entry| {
                    let number = entry.get("entry_number")?.as_str()?;
                    let title = entry.get("title")?.as_str()?;
                    case_timeline::filing_name(number, title, extension(&filing.slug)).ok()
                });
            match named {
                Some(new_name) => Row {
                    old_slug: filing.slug.clone(),
                    new_name: Some(new_name),
                    reason: None,
                },
                None => Row {
                    old_slug: filing.slug.clone(),
                    new_name: None,
                    reason: Some("no Docket Entry links this document".to_string()),
                },
            }
        })
        .collect();
    let mut taken: BTreeMap<String, usize> = BTreeMap::new();
    for row in &rows {
        if let Some(name) = &row.new_name {
            *taken.entry(name.clone()).or_default() += 1;
        }
    }
    for row in &mut rows {
        if row.new_name.as_ref().is_some_and(|name| taken[name] > 1) {
            row.new_name = None;
            row.reason = Some("another filing would take the same name".to_string());
        }
    }
    rows.sort_by(|a, b| a.old_slug.cmp(&b.old_slug));
    rows
}

/// Refuse a filing whose linked court entry does not supply its exact name.
pub(crate) fn validate_filing(slug: &str, asset_id: &str, entries: &[Value]) -> Result<()> {
    let matches: Vec<_> = entries
        .iter()
        .filter(|row| {
            row["asset_id"]
                .as_str()
                .is_some_and(|id| id.eq_ignore_ascii_case(asset_id))
        })
        .collect();
    anyhow::ensure!(
        matches.len() == 1,
        "{slug}: expected exactly one live document integrity result"
    );
    let row = matches[0];
    anyhow::ensure!(row["slug"].as_str() == Some(slug) && row["docket_name_matches"].as_bool() == Some(true),
        "{slug}: linked Docket Entry naming is missing or disagrees; run navigator project docket renumber --apply");
    Ok(())
}

/// Whether this checkout has court filings requiring live verification.
pub(crate) fn has_filings(root: &Path) -> Result<bool> {
    crate::document_sync::ensure_documents_tree_is_safe(&root.join("documents"))?;
    for file in walkdir::WalkDir::new(root.join("documents/Docket Filings")).follow_links(false) {
        match file {
            Ok(file)
                if file.file_type().is_file()
                    && !file.file_name().to_string_lossy().starts_with('.') =>
            {
                return Ok(true)
            }
            Err(error)
                if error
                    .io_error()
                    .is_none_or(|error| error.kind() != std::io::ErrorKind::NotFound) =>
            {
                return Err(error.into())
            }
            _ => {}
        }
    }
    for path in crate::document_read::discover_pointers(root)? {
        if crate::document_sync::read_pointer(&root.join(path))?.is_some_and(|pointer| {
            pointer.matter_folder() == store::document_pointers::MatterFolder::DocketFilings
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Check only pointers selecting Docket Filings. No document bytes are read.
pub(crate) fn validate_checkout(root: &Path, entries: &[Value]) -> Result<()> {
    let folder = root.join("documents/Docket Filings");
    crate::document_sync::ensure_documents_tree_is_safe(&root.join("documents"))?;
    if folder.exists() {
        for file in walkdir::WalkDir::new(&folder).follow_links(false) {
            let file = file?;
            if !file.file_type().is_file() || file.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let relative = file.path().strip_prefix(&folder)?;
            let slug = crate::document_sync::slash_path(relative)?;
            let matches = entries
                .iter()
                .filter(|entry| {
                    entry["slug"].as_str() == Some(slug.as_str())
                        && entry["docket_name_matches"].as_bool() == Some(true)
                })
                .count();
            anyhow::ensure!(
                matches == 1,
                "{slug}: filename must match exactly one Docket Entry"
            );
        }
    }
    for path in crate::document_read::discover_pointers(root)? {
        let Some(pointer) = crate::document_sync::read_pointer(&root.join(&path))? else {
            continue;
        };
        if pointer.matter_folder() != store::document_pointers::MatterFolder::DocketFilings {
            continue;
        }
        let slash = crate::document_sync::slash_path(&path)?;
        let slug = slash
            .strip_prefix("blobs/")
            .and_then(crate::document_sync::strip_pointer_extension)
            .context("invalid Pointer path")?;
        validate_filing(slug, &pointer.current_version.asset_id.to_string(), entries)?;
    }
    Ok(())
}

/// Name a linked document from its court entry using the existing collision-safe reslug operation.
pub(crate) async fn name_linked_document(
    c: &entries::Coordinates,
    row: &Value,
    root: &Path,
) -> Result<()> {
    let Some(asset) = row["document_asset_id"].as_str() else {
        return Ok(());
    };
    let client = crate::remote::DocumentClient::with_credential(
        c.base.clone(),
        c.token.clone(),
        &c.project_code,
    )
    .await?;
    let documents = client.list_documents().await?;
    let document = documents
        .iter()
        .find(|doc| doc.id.to_string() == asset)
        .context("linked document is absent from Project")?;
    let slug = document
        .slug
        .as_deref()
        .context("linked document has no Document Identity")?;
    let target = case_timeline::filing_name(
        row["entry_number"]
            .as_str()
            .context("Docket Entry has no number")?,
        row["title"].as_str().context("Docket Entry has no title")?,
        extension(slug),
    )
    .map_err(anyhow::Error::msg)?;
    let matching_checkout = crate::projects::manifest::read(root)
        .and_then(|manifest| manifest.project)
        .is_some_and(|project| project == c.project_code);
    if slug != target {
        if matching_checkout {
            crate::remote::reslug_with_local(&client, document.id, &target, root, false).await?;
        } else {
            client.reslug(document.id, &target, false).await?;
        }
    }
    if !matching_checkout {
        return Ok(());
    }
    let path = root.join(format!("blobs/{target}.yaml"));
    if let Some(mut pointer) = crate::document_sync::read_pointer(&path)? {
        let source = root
            .join("documents")
            .join(crate::document_layout::bytes_relative(
                Path::new(&target),
                pointer.matter_folder(),
            ));
        let destination = root.join("documents/Docket Filings").join(&target);
        if source != destination {
            crate::document_sync::ensure_documents_tree_is_safe(&root.join("documents"))?;
            std::fs::create_dir_all(destination.parent().context("filing has no parent")?)?;
            crate::document_sync::ensure_document_path_is_safe(root, &destination, true, true)?;
            anyhow::ensure!(
                !destination.exists(),
                "Docket Filings destination already exists"
            );
            if source.exists() {
                std::fs::create_dir_all(destination.parent().context("filing has no parent")?)?;
                std::fs::rename(&source, &destination)?;
            }
            pointer.folder = crate::document_layout::recorded_folder(
                &pointer.kind,
                store::document_pointers::MatterFolder::DocketFilings,
            );
            if let Err(error) =
                crate::document_sync::write_pointer_atomically(&path, &pointer.to_yaml()?)
            {
                if destination.exists() {
                    std::fs::rename(&destination, &source)
                        .context("restore filing after Pointer write failure")?;
                }
                return Err(error);
            }
        }
    }
    Ok(())
}

/// Every Pointer selecting Docket Filings, including documents awaiting their DKT name.
pub fn filings(root: &Path) -> Result<Vec<Filing>> {
    let mut filings = Vec::new();
    for relative in crate::document_read::discover_pointers(root)? {
        let slash = crate::document_sync::slash_path(&relative)?;
        let Some(slug) = slash
            .strip_prefix(&format!("{}/", crate::document_sync::POINTER_DIRECTORY))
            .and_then(crate::document_sync::strip_pointer_extension)
        else {
            continue;
        };
        let filename = slug.rsplit('/').next().unwrap_or(slug);
        if let Some(pointer) = crate::document_sync::read_pointer(&root.join(&relative))? {
            if pointer.matter_folder() == store::document_pointers::MatterFolder::DocketFilings
                || ingest::docket_key(filename).is_some()
            {
                filings.push(Filing {
                    slug: slug.to_string(),
                    asset_id: pointer.current_version.asset_id.to_string(),
                });
            }
        }
    }
    Ok(filings)
}

/// The printable listing, ending with its totals.
pub fn render(rows: &[Row]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for row in rows {
        let _ = match (&row.new_name, &row.reason) {
            (Some(new), _) => writeln!(out, "{} -> {new}", row.old_slug),
            (None, reason) => writeln!(
                out,
                "{} -> UNRESOLVED: {}",
                row.old_slug,
                reason.as_deref().unwrap_or("unknown")
            ),
        };
    }
    let unresolved = rows.iter().filter(|row| row.new_name.is_none()).count();
    let _ = writeln!(
        out,
        "{} filing(s), {unresolved} unresolved; nothing was renamed",
        rows.len()
    );
    out
}

async fn apply_names(
    coordinates: &entries::Coordinates,
    root: &Path,
    filings: &[Filing],
    listed: &[Value],
) -> Result<()> {
    let rows = manifest(filings, listed);
    let mut assets = std::collections::BTreeSet::new();
    let mut targets = std::collections::BTreeSet::new();
    for entry in listed {
        let Some(asset) = entry["document_asset_id"].as_str() else {
            continue;
        };
        anyhow::ensure!(
            assets.insert(asset.to_ascii_lowercase()),
            "multiple Docket Entries link one document; no names changed"
        );
        let number = entry["entry_number"]
            .as_str()
            .context("Docket Entry has no number")?;
        let title = entry["title"]
            .as_str()
            .context("Docket Entry has no title")?;
        let name = case_timeline::filing_name(number, title, "").map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            targets.insert(name.to_lowercase()),
            "Docket Entry names collide; no names changed"
        );
    }
    anyhow::ensure!(
        rows.iter().all(|row| row.new_name.is_some()),
        "unresolved filings; no names changed\n{}",
        render(&rows)
    );
    for entry in listed {
        name_linked_document(coordinates, entry, root).await.context("reconciling docket names stopped; earlier successful names remain; retry after resolving this failure")?;
    }
    Ok(())
}

pub async fn run(host: Option<&str>, project: Option<&str>, apply: bool) -> ExitCode {
    let result: Result<String> = async {
        let filings = filings(Path::new("."))?;
        let coordinates = entries::coordinates(host, project).await?;
        let listed = entries::list_rows(&coordinates)
            .await
            .context("read docket entries")?;
        let rows = manifest(&filings, &listed);
        if !apply {
            return Ok(render(&rows));
        }
        apply_names(&coordinates, Path::new("."), &filings, &listed).await?;
        Ok("Docket Filings names reconciled\n".to_string())
    }
    .await;
    match result {
        Ok(listing) => {
            print!("{listing}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator project docket renumber: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn filing(slug: &str, asset: &str) -> Filing {
        Filing {
            slug: slug.into(),
            asset_id: asset.into(),
        }
    }

    #[tokio::test]
    async fn renumber_refuses_unresolved_or_colliding_names_before_any_remote_write() {
        let server = wiremock::MockServer::start().await;
        let c = entries::Coordinates {
            base: server.uri(),
            token: "fixture".into(),
            project_id: uuid::Uuid::now_v7(),
            project_code: "sample".into(),
        };
        let dir = tempfile::tempdir().unwrap();
        let listed = [
            json!({"entry_number":"4", "title":"Order", "document_asset_id":"a"}),
            json!({"entry_number":"4", "title":"Order", "document_asset_id":"b"}),
        ];
        let files = [filing("di-4-order.pdf", "a"), filing("di-5-order.pdf", "b")];
        let error = apply_names(&c, dir.path(), &files, &listed)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no names changed"), "{error:#}");
        let error = apply_names(
            &c,
            dir.path(),
            &[filing("di-9-order.pdf", "missing")],
            &listed[..1],
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("UNRESOLVED"), "{error:#}");
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn linking_a_document_uses_the_entries_number_and_title_at_the_remote_door() {
        use wiremock::{
            matchers::{body_json, method, path},
            Mock, MockServer, ResponseTemplate,
        };
        let server = MockServer::start().await;
        let project = uuid::Uuid::now_v7();
        let asset = uuid::Uuid::now_v7();
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!([{"id": project, "code":"sample"}])),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/projects/{project}/documents")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "id":asset, "byte_size":4, "sha256_hex":"a".repeat(64), "filename":"motion.pdf",
                "kind":"pleading", "visibility":"internal", "slug":"pleadings/motion.pdf",
                "inserted_at":"2026-01-01T00:00:00Z"
            }])))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(format!(
                "/app/api/projects/{project}/documents/{asset}"
            )))
            .and(body_json(
                json!({"slug":"DKT 0029-1 - Motion re- Stay.pdf", "reslug":true, "dry_run":false}),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"slug":"DKT 0029-1 - Motion re- Stay.pdf"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let c = entries::Coordinates {
            base: server.uri(),
            token: "fixture".into(),
            project_id: project,
            project_code: "sample".into(),
        };
        let dir = tempfile::tempdir().unwrap();
        name_linked_document(
            &c,
            &json!({"entry_number":"29-1", "title":"Motion re: Stay", "document_asset_id":asset}),
            dir.path(),
        )
        .await
        .unwrap();
        assert!(
            !dir.path().join("blobs").exists(),
            "an unrelated checkout must stay untouched"
        );
    }

    #[tokio::test]
    async fn a_later_remote_refusal_preserves_successful_names_and_reports_partial_progress() {
        use wiremock::{
            matchers::{method, path},
            Mock, MockServer, ResponseTemplate,
        };
        let server = MockServer::start().await;
        let project = uuid::Uuid::now_v7();
        let first = uuid::Uuid::now_v7();
        let second = uuid::Uuid::now_v7();
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!([{"id":project,"code":"sample"}])),
            )
            .expect(2)
            .mount(&server)
            .await;
        let documents: Vec<_> = [first, second]
            .into_iter()
            .map(|id| {
                json!({
                    "id":id, "byte_size":4, "sha256_hex":"a".repeat(64), "filename":"motion.pdf",
                    "kind":"pleading", "visibility":"internal", "slug":format!("{id}.pdf"),
                    "inserted_at":"2026-01-01T00:00:00Z"
                })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path(format!("/app/api/projects/{project}/documents")))
            .respond_with(ResponseTemplate::new(200).set_body_json(documents))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(format!(
                "/app/api/projects/{project}/documents/{first}"
            )))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"slug":"DKT 0001 - Order.pdf"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(format!(
                "/app/api/projects/{project}/documents/{second}"
            )))
            .respond_with(ResponseTemplate::new(409).set_body_string("target occupied"))
            .expect(1)
            .mount(&server)
            .await;
        let rows = [
            json!({"entry_number":"1","title":"Order","document_asset_id":first}),
            json!({"entry_number":"2","title":"Order","document_asset_id":second}),
        ];
        let c = entries::Coordinates {
            base: server.uri(),
            token: "fixture".into(),
            project_id: project,
            project_code: "sample".into(),
        };
        let dir = tempfile::tempdir().unwrap();
        let error = apply_names(&c, dir.path(), &[], &rows).await.unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("earlier successful names remain"),
            "{message}"
        );
        assert!(message.contains("409"), "{message}");
        assert_eq!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .filter(|request| request.method.as_str() == "PATCH")
                .count(),
            2
        );
    }

    #[test]
    fn an_orphan_or_nonconforming_file_is_refused_without_reading_its_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("documents/Docket Filings");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("DKT 0004 - Order.pdf"), b"synthetic").unwrap();
        let rows =
            [json!({"asset_id":"a", "slug":"DKT 0004 - Order.pdf", "docket_name_matches":true})];
        assert!(validate_checkout(dir.path(), &rows).is_ok());
        assert!(validate_checkout(dir.path(), &[]).is_err());
        std::fs::rename(
            folder.join("DKT 0004 - Order.pdf"),
            folder.join("order.pdf"),
        )
        .unwrap();
        assert!(validate_checkout(dir.path(), &rows).is_err());
    }

    #[test]
    fn an_empty_sync_created_filing_folder_needs_no_live_docket_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("documents/Docket Filings")).unwrap();
        assert!(!has_filings(dir.path()).unwrap());
    }

    #[test]
    fn every_filing_in_the_matter_folder_needs_its_linked_entry_name() {
        let entry = serde_json::json!({"asset_id":"a", "slug":"DKT 0004 - Order.pdf", "docket_name_matches":true});
        assert!(validate_filing("DKT 0004 - Order.pdf", "a", std::slice::from_ref(&entry)).is_ok());
        assert!(
            validate_filing("DKT 0004 - Wrong.pdf", "a", std::slice::from_ref(&entry)).is_err()
        );
        assert!(validate_filing("DKT 4 - Order.pdf", "a", std::slice::from_ref(&entry)).is_err());
        assert!(
            validate_filing("DKT 0004 - Order.pdf", "b", std::slice::from_ref(&entry)).is_err()
        );
        let mut invalid = entry.clone();
        invalid["docket_name_matches"] = json!(false);
        assert!(validate_filing("DKT 0004 - Order.pdf", "a", &[invalid]).is_err());
        let missing = json!({"asset_id":"a", "slug":"DKT 0004 - Order.pdf"});
        assert!(validate_filing("DKT 0004 - Order.pdf", "a", &[missing]).is_err());
        assert!(validate_filing("DKT 0004 - Order.pdf", "a", &[entry.clone(), entry]).is_err());
    }

    #[test]
    fn every_older_filing_is_listed_with_the_name_its_entry_gives_it() {
        let entries = vec![
            json!({"entry_number": "29", "title": "Motion re: Stay", "document_asset_id": "AAA"}),
            json!({"entry_number": "29-1", "title": "Exhibit A", "document_asset_id": "bbb"}),
        ];
        let rows = manifest(
            &[
                filing("docket/c/di-29-1-exhibit-a.pdf", "BBB"),
                filing("docket/c/di-29-motion.pdf", "aaa"),
            ],
            &entries,
        );
        assert_eq!(
            rows,
            vec![
                Row {
                    old_slug: "docket/c/di-29-1-exhibit-a.pdf".into(),
                    new_name: Some("DKT 0029-1 - Exhibit A.pdf".into()),
                    reason: None
                },
                Row {
                    old_slug: "docket/c/di-29-motion.pdf".into(),
                    new_name: Some("DKT 0029 - Motion re- Stay.pdf".into()),
                    reason: None
                },
            ]
        );
    }

    #[test]
    fn a_filing_no_entry_links_is_unresolved_not_dropped() {
        let rows = manifest(&[filing("docket/c/di-3-order.pdf", "zzz")], &[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].new_name, None);
        assert!(rows[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("no Docket Entry"));
    }

    #[test]
    fn two_filings_that_would_share_a_name_are_both_unresolved() {
        let entries = vec![
            json!({"entry_number": "4", "title": "Order", "document_asset_id": "a"}),
            json!({"entry_number": "4", "title": "Order", "document_asset_id": "b"}),
        ];
        let rows = manifest(
            &[
                filing("docket/c/di-4-order.pdf", "a"),
                filing("docket/d/di-4-order.pdf", "b"),
            ],
            &entries,
        );
        assert!(rows.iter().all(|row| row.new_name.is_none()), "{rows:?}");
        assert!(rows[0].reason.as_deref().unwrap().contains("same name"));
    }

    #[test]
    fn the_listing_ends_with_totals_and_says_nothing_was_renamed() {
        let rows = manifest(&[filing("docket/c/di-3-order.pdf", "zzz")], &[]);
        let text = render(&rows);
        assert!(
            text.contains("docket/c/di-3-order.pdf -> UNRESOLVED:"),
            "{text}"
        );
        assert!(
            text.ends_with("1 filing(s), 1 unresolved; nothing was renamed\n"),
            "{text}"
        );
    }
}
