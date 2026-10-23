//! Draft — the lawyer's word for a [Template](crate::templates).
//!
//! Draft and Template name one thing (`docs/glossary/draft.md`), so this
//! module is [`crate::templates`] under its other name and adds nothing of
//! its own: [`Draft`] *is* [`Template`], and every function here — save,
//! resolve, list, body — is the canonical one. There is no second write
//! path, no second version line, and no second identity.
//!
//! # Rust ids versus SQL projection ids
//!
//! The Rust names above are aliases, so a Draft's id is its Template's id,
//! the one `notation.template_id` pins and an event references.
//!
//! SQL is different. The `draft` table is a read-only *projection* of
//! `template` (a view, `navigator.surql`), and a view row has its own id:
//!
//! | | `template` row | `draft` row |
//! | --- | --- | --- |
//! | id | `template:<key>` | `draft:<key>` |
//! | canonical source | itself | `source_template_id` = `template:<key>` |
//! | writable | yes, through this module | never; the engine refuses it |
//!
//! Never treat the two ids as interchangeable: compare a projection row's
//! `source_template_id` — not its `id` — with `notation.template_id`. Every
//! other column is the source row's own, so a retired version appears with
//! its own `is_current`, and a Notation pinned to an older version resolves
//! that version through the same reference.
//!
//! # Lifecycle
//!
//! `store::schema::apply` defines the view with `IF NOT EXISTS` (re-defining
//! a populated view with `OVERWRITE` fails), which fills it from the
//! `template` rows that already exist. After that the engine keeps it
//! current: a new version, a retired version, and a deleted source row all
//! show up without any step of ours. `apply` also removes and re-defines a
//! `draft` that is not the shipped view — a changed `SELECT`, or the plain
//! table an import leaves behind — and operational exports leave the view
//! out ([`crate::schema::strip_derived_tables`]) so a restore rebuilds it.

pub use crate::templates::*;

/// A Draft is a Template: the same type, the same canonical id.
pub type Draft = crate::templates::Template;

/// The SQL table that projects `template`, read-only, under the Draft name.
pub const PROJECTION_TABLE: &str = "draft";

#[cfg(test)]
mod tests {
    use std::any::type_name_of_val;

    use serde_json::Value;
    use uuid::Uuid;

    use super::{Draft, PROJECTION_TABLE};
    use crate::notations::{self, NewNotation};
    use crate::schema;
    use crate::surreal::test_support::{mem, unmigrated};
    use crate::surreal::{record_id, SurrealDb};
    use crate::templates::{self, Saved, Template, Version};

    fn version(title: &str, asset: Option<Uuid>, sha: Option<&str>) -> Version {
        Version {
            title: title.into(),
            respondent_type: "entity".into(),
            asset_id: asset,
            form_code: None,
            kind: Some("agreement".into()),
            source_commit_sha: sha.map(ToString::to_string),
        }
    }

    /// Every row of `table` as JSON, ordered so two tables compare row by row.
    async fn rows(db: &SurrealDb, table: &str) -> Vec<Value> {
        let mut response = db
            .query(format!("SELECT * FROM {table} ORDER BY inserted_at, code"))
            .await
            .unwrap();
        let rows: Vec<surrealdb::types::Value> = response.take(0).unwrap();
        rows.into_iter()
            .map(surrealdb::types::Value::into_json_value)
            .collect()
    }

    fn without(row: &Value, keys: &[&str]) -> Value {
        let mut row = row.clone();
        if let Some(object) = row.as_object_mut() {
            for key in keys {
                object.remove(*key);
            }
        }
        row
    }

    /// The projection equals the canonical table: same rows, same fields,
    /// with the distinct view id and the added source reference accounted
    /// for, and each reference naming its own canonical row.
    async fn assert_projection_matches_canonical(db: &SurrealDb) {
        let canonical = rows(db, "template").await;
        let projected = rows(db, PROJECTION_TABLE).await;
        assert_eq!(canonical.len(), projected.len(), "one projection row each");
        for (template, draft) in canonical.iter().zip(&projected) {
            assert_eq!(
                draft["source_template_id"], template["id"],
                "the reference resolves to the exact canonical row"
            );
            assert_ne!(draft["id"], template["id"], "the view has its own id");
            assert!(
                draft["id"].as_str().unwrap().starts_with("draft:"),
                "{draft}"
            );
            assert_eq!(
                without(draft, &["id", "source_template_id"]),
                without(template, &["id"]),
                "every source field matches the canonical row"
            );
        }
    }

    #[test]
    fn a_draft_is_the_canonical_template_through_the_same_functions() {
        let template: Template = Draft {
            ..sample_template()
        };
        let _: Draft = template;
        assert_eq!(
            type_name_of_val(&super::save_version),
            type_name_of_val(&templates::save_version)
        );
        assert_eq!(
            type_name_of_val(&super::resolve),
            type_name_of_val(&templates::resolve)
        );
        assert_eq!(
            std::any::type_name::<Draft>(),
            std::any::type_name::<Template>()
        );
    }

    fn sample_template() -> Template {
        Template {
            id: Uuid::nil(),
            code: "sample".into(),
            title: "Sample".into(),
            respondent_type: "entity".into(),
            project_id: None,
            is_current: true,
            asset_id: None,
            form_code: None,
            kind: None,
            source_commit_sha: None,
            body_sha256: None,
            inserted_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn an_empty_database_defines_an_empty_projection() {
        let db = mem().await;
        assert!(rows(&db, PROJECTION_TABLE).await.is_empty());
        // And it tracks the first write with no further step.
        templates::save_version(&db, None, "first", version("First", None, None))
            .await
            .unwrap();
        assert_eq!(rows(&db, PROJECTION_TABLE).await.len(), 1);
        assert_projection_matches_canonical(&db).await;
    }

    /// A database that held Template versions before the view existed gains
    /// a populated projection from the first apply — no backfill step.
    #[tokio::test]
    async fn a_database_with_historical_versions_gains_a_populated_projection() {
        let db = unmigrated().await;
        schema::apply(&db).await.unwrap();
        db.query("REMOVE TABLE draft").await.unwrap();
        let v1 = templates::save_version(&db, None, "lease", version("Lease", None, Some("aaa")))
            .await
            .unwrap()
            .into_model();
        templates::save_version(&db, None, "lease", version("Lease v2", None, Some("bbb")))
            .await
            .unwrap();
        templates::save_version(&db, None, "nda", version("NDA", None, None))
            .await
            .unwrap();
        assert!(
            db.query("SELECT * FROM draft")
                .await
                .and_then(surrealdb::IndexedResults::check)
                .map(|mut r| r.take::<Vec<surrealdb::types::Value>>(0).unwrap())
                .map_or(true, |rows| rows.is_empty()),
            "without the view there is nothing to read"
        );

        schema::apply(&db).await.unwrap();

        assert_eq!(rows(&db, PROJECTION_TABLE).await.len(), 3);
        assert_projection_matches_canonical(&db).await;
        let retired = rows(&db, PROJECTION_TABLE)
            .await
            .into_iter()
            .find(|row| {
                row["source_template_id"]
                    .as_str()
                    .unwrap()
                    .contains(&v1.id.to_string())
            })
            .expect("the historical version is projected");
        assert_eq!(retired["is_current"], false);
    }

    /// Apply, write, apply again — at least twice more: every application
    /// succeeds and no row, version, flag, hash, or pin moves.
    #[tokio::test]
    async fn reapplying_the_schema_leaves_rows_versions_hashes_and_pins_intact() {
        let db = mem().await;
        let asset = crate::test_support::seed_body_asset(&db, b"# Lease").await;
        let v1 = templates::save_version(
            &db,
            None,
            "lease",
            version("Lease", Some(asset), Some("1111111")),
        )
        .await
        .unwrap()
        .into_model();
        let v2 = templates::save_version(
            &db,
            None,
            "lease",
            version("Lease v2", Some(asset), Some("2222222")),
        )
        .await
        .unwrap()
        .into_model();
        let notation = notations::create(
            &db,
            &NewNotation::new(v1.id, Uuid::now_v7(), Uuid::now_v7(), "BEGIN"),
        )
        .await
        .unwrap();

        let before = (
            rows(&db, "template").await,
            rows(&db, PROJECTION_TABLE).await,
        );
        for _ in 0..3 {
            schema::apply(&db).await.unwrap();
        }
        let after = (
            rows(&db, "template").await,
            rows(&db, PROJECTION_TABLE).await,
        );
        assert_eq!(before, after, "three more applications changed nothing");

        assert_eq!(
            templates::versions_of(&db, None, "lease")
                .await
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            templates::resolve(&db, None, "lease")
                .await
                .unwrap()
                .unwrap()
                .id,
            v2.id
        );
        let pinned = notations::find_by_id(&db, notation.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pinned.template_id, v1.id, "the pin is untouched");
        assert_projection_matches_canonical(&db).await;
    }

    /// Re-saving identical source is a no-op the projection never sees;
    /// changing it retires the earlier version, and the view shows both.
    #[tokio::test]
    async fn a_no_op_resave_adds_nothing_and_a_change_shows_both_versions() {
        let db = mem().await;
        let first = templates::save_version(&db, None, "msa", version("MSA", None, Some("a")))
            .await
            .unwrap();
        assert!(first.was_written());
        let same = templates::save_version(&db, None, "msa", version("MSA", None, Some("b")))
            .await
            .unwrap();
        assert!(matches!(same, Saved::Unchanged(_)));
        assert_eq!(rows(&db, PROJECTION_TABLE).await.len(), 1);

        templates::save_version(&db, None, "msa", version("MSA revised", None, Some("c")))
            .await
            .unwrap();
        let projected = rows(&db, PROJECTION_TABLE).await;
        assert_eq!(projected.len(), 2);
        let current: Vec<_> = projected
            .iter()
            .filter(|r| r["is_current"] == true)
            .collect();
        assert_eq!(current.len(), 1, "exactly one current version");
        assert_eq!(current[0]["title"], "MSA revised");
        let retired: Vec<_> = projected
            .iter()
            .filter(|r| r["is_current"] == false)
            .collect();
        assert_eq!(retired.len(), 1);
        assert!(
            retired[0].get("current_key").is_none(),
            "a retired version has no key"
        );
        assert_projection_matches_canonical(&db).await;
    }

    /// A Notation pinned to an older version resolves that exact version
    /// through the projection's canonical reference — and a project-scoped
    /// version carries its scope.
    #[tokio::test]
    async fn a_pinned_older_version_resolves_through_the_source_reference() {
        let db = mem().await;
        let project = Uuid::now_v7();
        let old_body = crate::test_support::seed_body_asset(&db, b"# Old").await;
        let new_body = crate::test_support::seed_body_asset(&db, b"# New").await;
        let old = templates::save_version(
            &db,
            Some(project),
            "lease",
            version("Old", Some(old_body), Some("o")),
        )
        .await
        .unwrap()
        .into_model();
        templates::save_version(
            &db,
            Some(project),
            "lease",
            version("New", Some(new_body), Some("n")),
        )
        .await
        .unwrap();
        let notation = notations::create(
            &db,
            &NewNotation::new(old.id, Uuid::now_v7(), project, "BEGIN"),
        )
        .await
        .unwrap();

        let projected = rows(&db, PROJECTION_TABLE).await;
        let pinned_row = projected
            .iter()
            .find(|row| row["title"] == "Old")
            .expect("the pinned version is projected");
        // The notation's own pin, read back as the canonical record, is the
        // row the projection row names.
        let mut response = db
            .query("SELECT * FROM $pin")
            .bind(("pin", record_id("template", notation.template_id)))
            .await
            .unwrap();
        let canonical: Vec<surrealdb::types::Value> = response.take(0).unwrap();
        let canonical = canonical[0].clone().into_json_value();
        assert_eq!(pinned_row["source_template_id"], canonical["id"]);
        assert_eq!(canonical["title"], "Old");
        assert_eq!(pinned_row["project_id"], canonical["project_id"]);
        assert!(
            canonical["project_id"].is_string(),
            "project scope is carried"
        );
        assert_eq!(pinned_row["is_current"], false);
        assert_projection_matches_canonical(&db).await;
    }

    /// The engine refuses every ordinary write to the view, and a Rust
    /// alias write goes to `template`, where the view follows it.
    #[tokio::test]
    async fn the_view_refuses_writes_and_the_alias_writes_the_canonical_row() {
        let db = mem().await;
        let draft = templates::save_version(&db, None, "nda", version("NDA", None, None))
            .await
            .unwrap()
            .into_model();
        let projected = rows(&db, PROJECTION_TABLE).await[0]["id"]
            .as_str()
            .unwrap()
            .to_string();
        for statement in [
            "CREATE draft:bad SET code = 'bad'".to_string(),
            "INSERT INTO draft { id: draft:bad, code: 'bad' }".to_string(),
            format!("UPSERT {projected} SET code = 'bad'"),
            format!("UPDATE {projected} SET code = 'bad'"),
            format!("DELETE {projected}"),
            format!("RELATE {projected}->draft->{projected}"),
        ] {
            let error = db
                .query(&statement)
                .await
                .and_then(surrealdb::IndexedResults::check)
                .expect_err(&statement);
            assert!(error.to_string().contains("view"), "{statement}: {error}");
        }
        assert_eq!(rows(&db, PROJECTION_TABLE).await[0]["code"], "nda");
        assert_eq!(
            draft.id,
            templates::resolve(&db, None, "nda")
                .await
                .unwrap()
                .unwrap()
                .id
        );
        let _ = record_id("template", draft.id);
    }

    /// An import that carried the view leaves a plain, writable `draft`
    /// holding copies; the next apply replaces it with the real view.
    #[tokio::test]
    async fn a_plain_table_named_draft_is_replaced_by_the_view_on_apply() {
        let db = mem().await;
        templates::save_version(&db, None, "nda", version("NDA", None, None))
            .await
            .unwrap();
        db.query("REMOVE TABLE draft; CREATE draft:stray SET code = 'stray'")
            .await
            .unwrap();
        schema::apply(&db).await.unwrap();
        assert_eq!(
            rows(&db, PROJECTION_TABLE).await.len(),
            1,
            "the copy is gone"
        );
        assert!(db
            .query("CREATE draft:x SET code = 'x'")
            .await
            .and_then(surrealdb::IndexedResults::check)
            .is_err());
        assert_projection_matches_canonical(&db).await;
    }

    /// A view whose `SELECT` no longer matches the shipped one is rebuilt.
    #[tokio::test]
    async fn a_view_with_a_stale_definition_is_redefined_on_apply() {
        let db = mem().await;
        templates::save_version(&db, None, "nda", version("NDA", None, None))
            .await
            .unwrap();
        db.query("REMOVE TABLE draft; DEFINE TABLE draft TYPE NORMAL AS SELECT code FROM template")
            .await
            .unwrap();
        assert!(rows(&db, PROJECTION_TABLE).await[0]
            .get("source_template_id")
            .is_none());
        schema::apply(&db).await.unwrap();
        assert!(rows(&db, PROJECTION_TABLE).await[0]
            .get("source_template_id")
            .is_some());
        assert_projection_matches_canonical(&db).await;
    }

    /// Export, strip, import, apply: the canonical records come back and the
    /// projection is rebuilt from them, then survives another apply. The
    /// unstripped export is the failure this guards: it cannot import.
    #[tokio::test]
    async fn an_export_restores_the_canonical_rows_and_rebuilds_the_projection() {
        let source = mem().await;
        let asset = crate::test_support::seed_body_asset(&source, b"# Lease").await;
        let v1 = templates::save_version(
            &source,
            None,
            "lease",
            version("Lease", Some(asset), Some("a")),
        )
        .await
        .unwrap()
        .into_model();
        templates::save_version(
            &source,
            None,
            "lease",
            version("Lease v2", Some(asset), Some("b")),
        )
        .await
        .unwrap();
        let dir = std::env::temp_dir().join(format!("navigator-draft-restore-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("export.surql");
        source.export(raw.as_path()).await.unwrap();
        let export = std::fs::read_to_string(&raw).unwrap();
        assert!(
            export.contains("-- TABLE: draft"),
            "the raw export carries the view"
        );

        // The raw export is what fails: the view is defined before its source.
        let broken = unmigrated().await;
        assert!(broken.import(raw.as_path()).await.is_err());

        let stripped = schema::strip_derived_tables(&export);
        assert!(!stripped.contains("TABLE: draft") && !stripped.contains("TABLE DATA: draft"));
        assert_eq!(
            schema::strip_derived_tables(&stripped),
            stripped,
            "stripping repeats"
        );
        let clean = dir.join("stripped.surql");
        std::fs::write(&clean, &stripped).unwrap();

        let restored = unmigrated().await;
        restored.import(clean.as_path()).await.unwrap();
        schema::apply(&restored).await.unwrap();
        schema::apply(&restored).await.unwrap();

        assert_eq!(
            rows(&restored, "template").await,
            rows(&source, "template").await
        );
        assert_eq!(rows(&restored, PROJECTION_TABLE).await.len(), 2);
        assert_projection_matches_canonical(&restored).await;
        assert!(templates::find_by_id(&restored, v1.id)
            .await
            .unwrap()
            .is_some());
        assert!(restored
            .query("CREATE draft:x SET code = 'x'")
            .await
            .and_then(surrealdb::IndexedResults::check)
            .is_err());
        std::fs::remove_dir_all(dir).ok();
    }
}
