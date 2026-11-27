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

    use uuid::Uuid;

    use super::Draft;

    use crate::templates::{self, Template};

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
            firm_id: None,
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
}
