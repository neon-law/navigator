//! Writing a Docket Entry row's [Case Timeline](crate::projects::docket::case_timeline)
//! record into the checkout, and reconciling the folder it holds.
//!
//! The docket verbs (`add`, `ingest`, `link-document`, `supersede`) call
//! [`write_after`] with the row the store read back, so every write leaves
//! the checkout's `documents/Case Timeline/` holding the record the row
//! renders. `navigator project sync` calls [`plan`] for the same
//! reconciliation across every entry at once: a missing or stale record is
//! rewritten from its row, and a record no row names any more is removed.
//! The records are renderings the store owns — the canonical `case-timeline`
//! skill keeps the docket in Navigator, never a hand-edited memo — so the
//! gate ([`super::super::document_check::check_timeline_records`]) refuses a
//! record that disagrees with its row.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

use super::case_timeline::{self, Record};
use crate::document_sync;

/// The folder below `documents/` the records render into.
pub(crate) const FOLDER: &str = "Case Timeline";

/// The checkout-relative path of one record file.
pub(crate) fn record_relative(file_name: &str) -> String {
    format!("{}/{FOLDER}/{file_name}", document_sync::DOCUMENT_DIRECTORY)
}

/// The `document_slug` a rendered record carries for a linked document:
/// the Docket Filings path of the DKT name, without its extension — the
/// same identity `name_linked_document` files the document and its Pointer
/// under. An entry with no linked document enriches nothing; its record's
/// `document:` stays null.
pub(crate) fn enrich(row: &Value) -> Value {
    let mut enriched = row.clone();
    let linked = row
        .get("document_asset_id")
        .is_some_and(|asset| !asset.is_null());
    if linked {
        let number = row
            .get("entry_number")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let title = row.get("title").and_then(Value::as_str).unwrap_or_default();
        if let Ok(file_name) = store::cases::filing_name(number, title, "yaml") {
            if let Some(stem) = file_name.strip_suffix(".yaml") {
                enriched["document_slug"] = Value::String(format!(
                    "{}/{}",
                    store::document_pointers::MatterFolder::DocketFilings.as_str(),
                    stem
                ));
            }
        }
    }
    enriched
}

/// Render one row the way the writers do: the row, plus the `document_slug`
/// its linked document carries once it has been filed under its DKT name.
pub(crate) fn render(row: &Value) -> Result<Record, String> {
    case_timeline::render(&enrich(row))
}

/// Write the record a docket write just stored, into the checkout the verb
/// ran from. A checkout whose manifest names a different Project is left
/// alone — the same rule `name_linked_document` applies — and the write is
/// atomic, so a reader never sees half a record.
pub(crate) fn write_after(root: &Path, project_code: &str, row: &Value) -> Result<()> {
    let matching = crate::projects::manifest::read(root)
        .and_then(|manifest| manifest.project)
        .is_some_and(|project| project == project_code);
    if !matching {
        return Ok(());
    }
    // The store write already succeeded; the record is a rendering of it
    // that `navigator project sync` will reconcile. A row the store did
    // not return in full (or a checkout whose tree refuses the write) is
    // a warning, not a failure of the verb that just stored the entry.
    let record = match render(row) {
        Ok(record) => record,
        Err(error) => {
            eprintln!("could not render the Case Timeline record; `navigator project sync` will reconcile it: {error}");
            return Ok(());
        }
    };
    let relative = record_relative(&record.file_name);
    document_sync::ensure_documents_tree_is_safe(&root.join(document_sync::DOCUMENT_DIRECTORY))?;
    document_sync::write_pointer_atomically(&root.join(&relative), &record.yaml)
        .with_context(|| format!("write {relative}"))
}

/// One record write the reconcile plans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Write {
    /// The checkout-relative path of the record file.
    pub(crate) relative: String,
    /// The record's rendered bytes.
    pub(crate) yaml: String,
    /// Whether a file was already there under a different rendering —
    /// hand-written drift, which the gate refuses.
    pub(crate) stale: bool,
}

/// What reconciling the records with the live rows does.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    /// Records to write: a row's record that is missing, or a file that is
    /// there under different bytes.
    pub(crate) writes: Vec<Write>,
    /// Records no row names any more, relative to the checkout.
    pub(crate) removals: Vec<String>,
}

/// Compare the records on disk with the rendered rows. Pure: reads the
/// folder, writes nothing. Every row renders its record; a stale or missing
/// file is planned as a write, and a `*.yaml` file no row names — a
/// hand-written record, or one whose entry the store no longer carries — is
/// planned as a removal. Files that are not records (a lawyer's own note)
/// are left where they are.
pub(crate) fn plan(root: &Path, rows: &[Value]) -> Result<Plan> {
    let mut desired: BTreeMap<String, String> = BTreeMap::new();
    for row in rows {
        let record = render(row).map_err(anyhow::Error::msg)?;
        desired.insert(record.file_name, record.yaml);
    }
    let mut plan = Plan::default();
    let folder = root.join(document_sync::DOCUMENT_DIRECTORY).join(FOLDER);
    if folder.is_dir() {
        document_sync::ensure_documents_tree_is_safe(
            &root.join(document_sync::DOCUMENT_DIRECTORY),
        )?;
        for entry in walkdir::WalkDir::new(&folder).follow_links(false) {
            let entry = entry.with_context(|| format!("walk {}", folder.display()))?;
            let name = entry.file_name().to_string_lossy();
            if !entry.file_type().is_file() || name.starts_with('.') || !name.ends_with(".yaml") {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(&folder)
                .context("record path below the folder")?
                .to_string_lossy()
                .to_string();
            let contents = std::fs::read_to_string(entry.path())
                .with_context(|| format!("read {}", entry.path().display()))?;
            match desired.remove(relative.as_str()) {
                Some(yaml) if yaml == contents => {}
                Some(yaml) => plan.writes.push(Write {
                    relative: record_relative(&relative),
                    yaml,
                    stale: true,
                }),
                None => plan.removals.push(record_relative(&relative)),
            }
        }
    }
    for (file_name, yaml) in desired {
        plan.writes.push(Write {
            relative: record_relative(&file_name),
            yaml,
            stale: false,
        });
    }
    plan.removals.sort();
    Ok(plan)
}

/// Whether this checkout carries any Case Timeline record at all. A checkout
/// with none gives the gate nothing to validate and spares it the docket
/// read.
pub(crate) fn has_records(root: &Path) -> Result<bool> {
    let documents = root.join(document_sync::DOCUMENT_DIRECTORY);
    let folder = documents.join(FOLDER);
    if !folder.is_dir() {
        return Ok(false);
    }
    document_sync::ensure_documents_tree_is_safe(&documents)?;
    for file in walkdir::WalkDir::new(&folder).follow_links(false) {
        match file {
            Ok(file)
                if file.file_type().is_file()
                    && file.file_name().to_string_lossy().ends_with(".yaml") =>
            {
                return Ok(true)
            }
            Ok(_) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A row the way the docket doors return one, with a linked document.
    fn row() -> Value {
        json!({
            "id": "0190c2a0-0000-7000-8000-000000000009",
            "entry_number": "4",
            "kind": "hearing",
            "title": "Notice of Hearing",
            "party": "Court",
            "filed_or_served_on": "2026-03-05",
            "scheduled_on": "2026-04-01T16:00:00Z",
            "supersedes": null,
            "document_asset_id": "0190c2a0-0000-7000-8000-000000000001"
        })
    }

    fn checkout(manifest: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("navigator.yaml"), manifest).unwrap();
        dir
    }

    #[test]
    fn enriching_names_the_linked_document_by_its_dkt_filings_identity() {
        let enriched = enrich(&row());
        assert_eq!(
            enriched["document_slug"],
            "Docket Filings/DKT 0004 - Notice of Hearing"
        );
    }

    #[test]
    fn an_entry_without_a_document_renders_a_null_document() {
        let mut bare = row();
        bare["document_asset_id"] = Value::Null;
        let record = render(&bare).unwrap();
        assert!(record.yaml.contains("document: null\n"), "{}", record.yaml);
        assert!(!record.yaml.contains("Docket Filings"));
    }

    #[test]
    fn a_docket_write_leaves_the_golden_record_in_the_checkout() {
        let dir = checkout("host: h.example.com\nproject: acme\n");
        write_after(dir.path(), "acme", &row()).unwrap();
        let relative = "documents/Case Timeline/DKT 0004 - Notice of Hearing.yaml";
        let written = std::fs::read_to_string(dir.path().join(relative)).unwrap();
        assert_eq!(written, render(&row()).unwrap().yaml);
        // The write is idempotent: the same row again changes nothing.
        write_after(dir.path(), "acme", &row()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(relative)).unwrap(),
            written
        );
    }

    #[test]
    fn a_checkout_for_another_project_is_left_alone() {
        let dir = checkout("host: h.example.com\nproject: other\n");
        write_after(dir.path(), "acme", &row()).unwrap();
        assert!(!dir.path().join("documents/Case Timeline").exists());
    }

    #[test]
    fn a_written_checkout_reconciles_to_an_empty_plan() {
        let dir = checkout("host: h.example.com\nproject: acme\n");
        write_after(dir.path(), "acme", &row()).unwrap();
        let rows = [row()];
        assert_eq!(plan(dir.path(), &rows).unwrap(), Plan::default());
    }

    #[test]
    fn the_reconcile_writes_missing_and_stale_records_and_removes_untitled_ones() {
        let dir = checkout("host: h.example.com\nproject: acme\n");
        let mut stale = row();
        stale["party"] = json!("Defendant");
        write_after(dir.path(), "acme", &stale).unwrap();

        let rows = [row()];
        let planned = plan(dir.path(), &rows).unwrap();
        assert_eq!(planned.removals, Vec::<String>::new());
        assert_eq!(planned.writes.len(), 1);
        assert!(planned.writes[0].stale, "{:?}", planned.writes);
        assert_eq!(
            planned.writes[0].relative,
            "documents/Case Timeline/DKT 0004 - Notice of Hearing.yaml"
        );
        assert_eq!(planned.writes[0].yaml, render(&row()).unwrap().yaml);

        // A record whose entry is gone is a removal, and a missing one is a
        // fresh write.
        std::fs::remove_file(
            dir.path()
                .join("documents/Case Timeline/DKT 0004 - Notice of Hearing.yaml"),
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("documents/Case Timeline")).unwrap();
        std::fs::write(
            dir.path()
                .join("documents/Case Timeline/DKT 0009 - Ghost.yaml"),
            "number: '9'\n",
        )
        .unwrap();
        let planned = plan(dir.path(), &rows).unwrap();
        assert_eq!(
            planned.removals,
            vec!["documents/Case Timeline/DKT 0009 - Ghost.yaml".to_string()]
        );
        assert_eq!(planned.writes.len(), 1);
        assert!(!planned.writes[0].stale, "{:?}", planned.writes);
    }

    #[test]
    fn a_checkout_with_no_records_has_none() {
        let dir = checkout("host: h.example.com\nproject: acme\n");
        assert!(!has_records(dir.path()).unwrap());
        std::fs::create_dir_all(dir.path().join("documents/Case Timeline")).unwrap();
        assert!(
            !has_records(dir.path()).unwrap(),
            "an empty folder has none"
        );
        write_after(dir.path(), "acme", &row()).unwrap();
        assert!(has_records(dir.path()).unwrap());
    }
}
