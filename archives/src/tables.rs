//! `SurrealDB` table discovery and row-to-`RecordBatch` reads.
//!
//! [`ALL_TABLES`] is derived from every `DEFINE TABLE` in the shipped
//! `navigator.surql`, rather than a hand-maintained registry. A newly defined
//! table therefore reaches the analytical archive automatically; a read error
//! remains a per-table failure in the nightly summary instead of becoming a
//! silent omission.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use arrow::array::RecordBatch;
use store::surreal::SurrealDb;

use crate::batch_from_rows;

/// Every table declared in Navigator's Surreal schema that holds its own
/// rows, in stable order.
///
/// A derived table (`draft`, the read-only projection of `template`) is left
/// out: its rows are the same records as its source's, so snapshotting both
/// would count one entity twice. The canonical `template` rows carry them.
pub static ALL_TABLES: LazyLock<Vec<String>> = LazyLock::new(store::schema::base_table_names);

/// The canonical snapshot tables plus any table a live database contains
/// that this binary's schema has not named yet. Derived tables remain
/// excluded because their rows are recomputed from their source.
pub async fn source_tables(db: &SurrealDb) -> Result<Vec<String>> {
    let live: Option<std::collections::BTreeMap<String, String>> = db
        .query("RETURN (INFO FOR DB).tables")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .context("inventory SurrealDB tables for archive export")?
        .take(0)
        .context("read SurrealDB table inventory for archive export")?;
    let live = require_live_inventory(live)?;
    let mut tables: BTreeSet<String> = ALL_TABLES.iter().cloned().collect();
    let derived: BTreeSet<String> = store::schema::derived_tables()
        .into_iter()
        .map(|table| table.name)
        .collect();
    tables.extend(live.into_keys().filter(|table| !derived.contains(table)));
    Ok(tables.into_iter().collect())
}

fn require_live_inventory(
    live: Option<std::collections::BTreeMap<String, String>>,
) -> Result<std::collections::BTreeMap<String, String>> {
    live.context("SurrealDB returned no table inventory for archive export")
}

/// Table names are used as object-key path segments. A runtime-created name
/// outside the schema's simple identifier form is refused rather than
/// allowed to change the archive key's path.
#[must_use]
pub fn is_safe_archive_table_name(table: &str) -> bool {
    !table.is_empty()
        && table
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Select one table's rows and convert their JSON representation to a batch.
/// Returns `Ok(None)` when the table is empty.
pub async fn fetch_batch(db: &SurrealDb, table: &str) -> Result<Option<RecordBatch>> {
    let mut response = db
        .query("SELECT * FROM type::table($table)")
        .bind(("table", table.to_string()))
        .await
        .with_context(|| format!("select rows from Surreal table `{table}`"))?;
    let rows: Vec<surrealdb::types::Value> = response
        .take(0)
        .with_context(|| format!("read rows from Surreal table `{table}`"))?;
    let rows: Vec<serde_json::Value> = rows
        .into_iter()
        .map(surrealdb::types::Value::into_json_value)
        .collect();
    batch_from_rows(&rows)
}

#[cfg(test)]
mod tests {
    use super::{is_safe_archive_table_name, require_live_inventory, ALL_TABLES};

    #[test]
    fn all_entities_are_registered() {
        assert_eq!(
            ALL_TABLES.as_slice(),
            store::schema::base_table_names().as_slice()
        );
        assert!(!ALL_TABLES.is_empty());
    }

    /// The projection is a table, but not a second entity: a snapshot keeps
    /// the canonical Template records once and does not export `draft`.
    #[test]
    fn the_derived_draft_projection_is_not_snapshotted_as_a_second_entity() {
        assert!(ALL_TABLES.contains(&"template".to_string()));
        assert!(!ALL_TABLES.contains(&"draft".to_string()));
        assert!(store::schema::table_names().contains(&"draft".to_string()));
    }

    #[test]
    fn all_tables_are_sorted_so_diffs_are_minimal() {
        let mut sorted = ALL_TABLES.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, *ALL_TABLES);
    }

    #[test]
    fn runtime_names_cannot_escape_the_archive_table_path() {
        assert!(is_safe_archive_table_name("source_table_2"));
        assert!(!is_safe_archive_table_name("../source"));
        assert!(!is_safe_archive_table_name(""));
    }

    #[test]
    fn absent_live_inventory_fails_closed_but_empty_inventory_is_valid() {
        assert!(require_live_inventory(None)
            .unwrap_err()
            .to_string()
            .contains("no table inventory"));
        assert!(require_live_inventory(Some(std::collections::BTreeMap::new())).is_ok());
    }
}
