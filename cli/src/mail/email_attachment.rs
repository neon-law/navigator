//! File the attachments of a saved message alongside its source document.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{ensure, Context, Result};
use model::document_pointers::DocumentPointer;

use crate::remote::{DocumentClient, DocumentExecution};

use super::ParsedEmlFile;

pub(super) async fn upload_all(
    client: &DocumentClient,
    parsed: &ParsedEmlFile,
    source_slug: &str,
    checkout: Option<&Path>,
    kind: &str,
    mut provenance: serde_json::Value,
) -> Result<Vec<(String, DocumentPointer)>> {
    if parsed.attachments.is_empty() {
        return Ok(Vec::new());
    }

    let (directory, source_name) = source_slug
        .rsplit_once('/')
        .context("a filed message slug must have a parent directory")?;
    let mut used = HashSet::from([source_name.to_ascii_lowercase()]);
    let inventory = client.list_documents().await?;
    provenance["attachment"] = serde_json::Value::Bool(true);
    let mut pointers = Vec::with_capacity(parsed.attachments.len());
    for (index, attachment) in parsed.attachments.iter().enumerate() {
        let filename = dated_name(&attachment.filename, parsed.dated_at, &mut used);
        let slug = format!("{directory}/{filename}");

        let extension = extension_or_bin(&attachment.filename);
        let old_slug = format!("{directory}/{index}.{extension}");
        let digest = model::documents::sha256_hex(&attachment.bytes);
        if let Some(existing) = inventory
            .iter()
            .find(|row| row.slug.as_deref() == Some(old_slug.as_str()) && row.sha256_hex == digest)
        {
            if let Some(root) = checkout {
                crate::remote::reslug_with_local(client, existing.id, &slug, root, false).await?;
            } else {
                client.reslug(existing.id, &slug, false).await?;
            }
        }

        let pointer = client
            .upload_bytes_with_details(
                &filename,
                &attachment.bytes,
                kind,
                None,
                None,
                Some(&attachment.content_type),
                Some(&slug),
                None,
                None,
                None,
                &DocumentExecution::default(),
                Some(provenance.clone()),
            )
            .await?;
        let revisions = client.list_revisions(&slug).await?;
        ensure!(
            pointer.current_version.sha256 == digest
                && revisions.revisions.first().is_some_and(|head| {
                    head.operative && head.asset_id == pointer.current_version.asset_id
                }),
            "the filed attachment does not resolve to its expected bytes at {slug}"
        );
        pointers.push((slug, pointer));
    }
    Ok(pointers)
}

fn extension_or_bin(filename: &str) -> &str {
    Path::new(filename)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty())
        .unwrap_or("bin")
}

fn dated_name(filename: &str, date: chrono::NaiveDate, used: &mut HashSet<String>) -> String {
    let leaf = filename
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .trim();
    let stem = Path::new(leaf)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    let stem = stem
        .split_once('_')
        .filter(|(prefix, _)| chrono::NaiveDate::parse_from_str(prefix, "%Y.%m.%d").is_ok())
        .map_or(stem, |(_, stem)| stem);
    let name = rules::c004::name_slug(stem).unwrap_or_else(|| "attachment".to_owned());
    let extension = extension_or_bin(leaf);
    let prefix = format!("{}_{name}", date.format("%Y.%m.%d"));
    let mut candidate = format!("{prefix}.{extension}");
    let mut suffix = 2;
    while !used.insert(candidate.to_ascii_lowercase()) {
        candidate = format!("{prefix}-{suffix}.{extension}");
        suffix += 1;
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_date_and_normalized_names_distinguish_duplicate_attachments() {
        let date = chrono::NaiveDate::from_ymd_opt(2025, 10, 9).unwrap();
        let mut used = HashSet::from(["2025.10.09_message.eml".to_owned()]);
        for (name, expected) in [
            (
                "2020.01.01_sample-notebook.docx",
                "2025.10.09_sample-notebook.docx",
            ),
            ("sample_notebook.DOCX", "2025.10.09_sample-notebook-2.DOCX"),
            ("message.eml", "2025.10.09_message-2.eml"),
            ("", "2025.10.09_attachment.bin"),
        ] {
            assert_eq!(dated_name(name, date, &mut used), expected);
        }
    }
}
