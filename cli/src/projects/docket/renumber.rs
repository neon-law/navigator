//! `navigator project docket renumber` — the manifest of every existing
//! `di-<n>[-<att>]-<slug>.pdf` filing beside the `DKT NNNN - <title>` name its
//! Docket Entry gives it.
//!
//! The verb lists and renames nothing. A filing is matched to its entry
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

/// Every committed pointer in `root` whose slug is an older `di-` filing.
pub fn older_filings(root: &Path) -> Result<Vec<Filing>> {
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
        if ingest::docket_key(filename).is_some() {
            if let Some(pointer) = crate::document_sync::read_pointer(&root.join(&relative))? {
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
        "{} older filing(s), {unresolved} unresolved; nothing was renamed",
        rows.len()
    );
    out
}

pub async fn run(host: Option<&str>, project: Option<&str>) -> ExitCode {
    let result: Result<String> = async {
        let filings = older_filings(Path::new("."))?;
        let coordinates = entries::coordinates(host, project).await?;
        let listed = entries::list_rows(&coordinates)
            .await
            .context("read docket entries")?;
        let rows = manifest(&filings, &listed);
        Ok(render(&rows))
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
            text.ends_with("1 older filing(s), 1 unresolved; nothing was renamed\n"),
            "{text}"
        );
    }
}
