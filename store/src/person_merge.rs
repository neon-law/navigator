//! Merge two Person rows that are one human (LAW-169).
//!
//! An attorney who changes firms arrives under a new email, and a Person is
//! keyed by email, so the directory ends up with two rows for one human and
//! the Conflict-Check Graph splits their relationships across both.
//! [`merge`] folds the duplicate into the Person to keep.

use uuid::Uuid;

use crate::persons::{self, PersonError};
use crate::postgres_mirror::MirrorKey;
use crate::surreal::{record_id, SurrealDb};

/// Why a merge was refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum PersonMergeError {
    /// `keep` and `drop` name the same Person.
    #[error("keep and drop are the same Person")]
    SameRecord,
    /// One of the two Persons does not exist.
    #[error("no such Person: {0}")]
    NotFound(String),
    /// The drop Person is not a client; privileged tiers are edit-only.
    #[error("only a client Person can be merged away; Owner, admin, lawyer, and clerk people are edit-only")]
    Blocked,
    /// A Person read or write failed.
    #[error(transparent)]
    Person(#[from] PersonError),
    /// A database operation failed.
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
}

/// What a merge did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PersonMergeReport {
    pub kept_id: Uuid,
    pub dropped_id: Uuid,
    /// Every address the kept Person answers to afterwards, lowercased.
    pub emails: Vec<String>,
}

/// Tables whose `record<person>` link is moved with a plain `UPDATE`:
/// no unique index spans the link, so nothing can collide.
const PLAIN_LINKS: &[(&str, &str)] = &[
    ("project_module", "enabled_by_person_id"),
    ("project_deadlines", "owner_person_id"),
    ("project_service", "recipient_person_id"),
    ("project_service", "declarant_person_id"),
    ("git_access_token", "person_id"),
    ("lead", "person_id"),
    ("person_delegate", "delegate_person_id"),
    ("person_delegate", "subject_person_id"),
    ("person_delegate", "granted_by_person_id"),
    ("person_delegate", "revoked_by_person_id"),
    ("firm_integration_secret", "actor_id"),
    ("social_account", "connected_by_person_id"),
    ("social_account", "revoked_by_person_id"),
    ("social_oauth_attempt", "actor_person_id"),
    ("relationship_log", "actor_person_id"),
    ("email_token", "person_id"),
    ("address", "person_id"),
    ("summary_sender_approval", "person_id"),
    ("summary_sender_approval_event", "actor_person_id"),
    ("summary_sender_approval_event", "person_id"),
    ("answer", "person_id"),
    ("answer", "authored_by_person_id"),
    ("asset", "deleted_by_person_id"),
    ("notation", "person_id"),
    ("notation_clause", "authored_by_person_id"),
    ("notation_document_version", "authored_by_person_id"),
    ("notation_document_comment", "person_id"),
    ("notation_event", "acting_person_id"),
    ("document_comment", "person_id"),
    ("signature", "signer_person_id"),
    ("notarization", "notary_person_id"),
    ("verification", "verifier_person_id"),
    ("expunge_record", "authorized_by_person_id"),
    ("expunge_request", "requested_by_person_id"),
    ("expunge_request", "resolved_by_person_id"),
    ("communication", "author_person_id"),
    ("email_conversation", "person_id"),
    ("message", "author_person_id"),
    ("inbox_assignment", "actor_person_id"),
];

/// Tables where a unique index spans the Person link: `(table, link field,
/// the other columns of the index)`. A row the dropped Person holds that the
/// kept Person already holds an equivalent of is deleted, and the kept
/// Person's row stands.
const UNIQUE_LINKS: &[(&str, &str, &str)] = &[
    ("person_firm_role", "person_id", "firm_id"),
    ("credential", "person_id", "jurisdiction_id"),
    ("person_external_identity", "person_id", "system"),
    ("testimonial", "person_id", "[project_id, quote]"),
];

/// The whole move as one transaction: it commits entirely or not at all, and
/// rerunning it after a failure converges because the dropped row is deleted
/// only afterwards.
fn relink_script() -> String {
    use std::fmt::Write as _;
    let mut sql = String::from("BEGIN;\n");
    // A shared Project keeps one participation row; the kept row takes any
    // DRI flag the dropped row carried.
    for flag in ["is_lawyer_dri", "is_client_dri"] {
        let _ = writeln!(
            sql,
            "UPDATE person_project_role SET {flag} = true WHERE person_id = $keep \
             AND project_id IN (SELECT VALUE project_id FROM person_project_role \
             WHERE person_id = $drop AND {flag} = true);"
        );
    }
    sql.push_str(
        "DELETE person_project_role WHERE person_id = $drop AND project_id IN \
         (SELECT VALUE project_id FROM person_project_role WHERE person_id = $keep);\n\
         UPDATE person_project_role SET person_id = $keep WHERE person_id = $drop;\n",
    );
    for (table, field, other) in UNIQUE_LINKS {
        let _ = writeln!(
            sql,
            "DELETE {table} WHERE {field} = $drop AND {other} IN \
             (SELECT VALUE {other} FROM {table} WHERE {field} = $keep);\n\
             UPDATE {table} SET {field} = $keep WHERE {field} = $drop;"
        );
    }
    // The Conflict-Check Graph's edges. An edge's endpoints are not
    // rewritten in place: each edge is drawn again from the kept Person
    // carrying its own fields, and the old one is deleted.
    for (table, end, others, columns) in [
        ("entity_role", "in", "[out, role]", "out, role"),
        ("project_reference", "out", "[in, reason]", "in, reason"),
        (
            "relationship",
            "in",
            "[out, kind]",
            "out, kind, confidence_pct, source_kind, source_id, detail",
        ),
        (
            "relationship",
            "out",
            "[in, kind]",
            "in, kind, confidence_pct, source_kind, source_id, detail",
        ),
    ] {
        let _ = writeln!(
            sql,
            "DELETE {table} WHERE {end} = $drop AND {others} IN \
             (SELECT VALUE {others} FROM {table} WHERE {end} = $keep);\n\
             INSERT RELATION INTO {table} (SELECT $keep AS {end}, {columns}, inserted_at, updated_at \
             FROM {table} WHERE {end} = $drop);\n\
             DELETE {table} WHERE {end} = $drop;"
        );
    }
    sql.push_str(
        "UPDATE relationship_log SET subject_id = $keep_uuid \
         WHERE subject_type = 'person' AND subject_id = $drop_uuid;\n",
    );
    for (table, field) in PLAIN_LINKS {
        let _ = writeln!(
            sql,
            "UPDATE {table} SET {field} = $keep WHERE {field} = $drop;"
        );
    }
    // Identity subjects: the kept Person takes one it lacks, so the human
    // can still sign in.
    for field in ["oidc_subject", "microsoft_subject", "apple_subject"] {
        let _ = writeln!(
            sql,
            "LET ${field} = (SELECT VALUE {field} FROM ONLY $drop);\n\
             UPDATE $drop SET {field} = NONE;\n\
             UPDATE $keep SET {field} = ${field} WHERE {field} IS NONE AND ${field} IS NOT NONE;"
        );
    }
    // Both addresses end up held by the kept Person.
    sql.push_str("UPDATE person_mailbox SET person_id = $keep WHERE person_id = $drop;\nCOMMIT;\n");
    sql
}

/// [`merge`], naming each Person by an address it answers to, primary or
/// alias, case-insensitively.
///
/// # Errors
///
/// See [`PersonMergeError`].
pub async fn merge_by_email(
    db: &SurrealDb,
    keep: &str,
    dropped: &str,
) -> Result<PersonMergeReport, PersonMergeError> {
    let find = |email: &str| {
        let email = email.to_string();
        async move {
            persons::find_by_any_email(db, &email)
                .await?
                .ok_or(PersonMergeError::NotFound(email))
        }
    };
    let keep = find(keep).await?;
    let dropped = find(dropped).await?;
    merge(db, keep.id, dropped.id).await
}

/// Fold `drop_id` into `keep_id`: every relationship the dropped Person held
/// moves to the kept Person, the kept Person answers to both addresses, and
/// the duplicate row is deleted last.
///
/// Only a client Person can be dropped, matching
/// [`crate::people_commands::delete_person`]. Nothing is deleted until the
/// relationships have moved, so a failure leaves both Persons in place and
/// the merge can be run again.
///
/// # Errors
///
/// See [`PersonMergeError`].
pub async fn merge(
    db: &SurrealDb,
    keep_id: Uuid,
    drop_id: Uuid,
) -> Result<PersonMergeReport, PersonMergeError> {
    if keep_id == drop_id {
        return Err(PersonMergeError::SameRecord);
    }
    let keep = persons::find_by_id(db, keep_id)
        .await?
        .ok_or_else(|| PersonMergeError::NotFound(keep_id.to_string()))?;
    let dropped = persons::find_by_id(db, drop_id)
        .await?
        .ok_or_else(|| PersonMergeError::NotFound(drop_id.to_string()))?;
    if dropped.role != persons::Role::Client {
        return Err(PersonMergeError::Blocked);
    }
    // Aliases are SurrealDB mailbox claims; refuse early under PostgreSQL
    // People authority rather than half-move.
    persons::aliases(db, keep_id).await?;

    let script = relink_script();
    crate::surreal::retry::writing(|| {
        db.query(script.as_str())
            .bind(("keep", record_id("person", keep_id)))
            .bind(("drop", record_id("person", drop_id)))
            .bind(("keep_uuid", keep_id))
            .bind(("drop_uuid", drop_id))
    })
    .await?;
    crate::postgres_mirror::after_write(db, MirrorKey::PersonMailbox(keep_id)).await;
    persons::delete(db, drop_id).await?;

    let mut emails = persons::aliases(db, keep_id).await?;
    emails.push(keep.email.trim().to_lowercase());
    emails.sort();
    emails.dedup();
    Ok(PersonMergeReport {
        kept_id: keep_id,
        dropped_id: drop_id,
        emails,
    })
}

#[cfg(test)]
mod tests {
    use super::{merge, PersonMergeError};
    use crate::persons::{self, NewPerson, Role};
    use crate::surreal::test_support::mem;
    use crate::surreal::{record_id, SurrealDb};
    use uuid::Uuid;

    async fn person(db: &SurrealDb, name: &str, email: &str) -> persons::Person {
        persons::create(db, &NewPerson::new(name, email))
            .await
            .unwrap()
    }

    async fn count(db: &SurrealDb, sql: &str, id: Uuid) -> i64 {
        let mut res = db
            .query(sql)
            .bind(("id", record_id("person", id)))
            .bind(("uuid", id))
            .await
            .unwrap();
        let rows: Vec<i64> = res.take(0).unwrap();
        rows.first().copied().unwrap_or(0)
    }

    #[tokio::test]
    async fn merge_moves_relationships_keeps_both_addresses_and_removes_the_duplicate() {
        let db = mem().await;
        let keep = person(&db, "Pat Attorney", "pat@old.example").await;
        let drop = person(&db, "Pat Attorney", "pat@new.example").await;
        let project = Uuid::now_v7();
        let shared = Uuid::now_v7();
        let entity = Uuid::now_v7();
        for (who, proj, dri) in [
            (drop.id, project, true),
            (keep.id, shared, false),
            (drop.id, shared, true),
        ] {
            db.query(
                "CREATE person_project_role SET person_id = $p, project_id = $proj, \
                 participation = 'client', is_lawyer_dri = false, is_client_dri = $dri, \
                 inserted_at = 'x', updated_at = 'x'",
            )
            .bind(("p", record_id("person", who)))
            .bind(("proj", record_id("project", proj)))
            .bind(("dri", dri))
            .await
            .unwrap()
            .check()
            .unwrap();
        }
        db.query("RELATE $p->entity_role->$e SET role = 'agent'")
            .bind(("p", record_id("person", drop.id)))
            .bind(("e", record_id("entity", entity)))
            .await
            .unwrap()
            .check()
            .unwrap();
        db.query(
            "CREATE relationship_log SET subject_type = 'person', subject_id = $uuid, \
             action = 'on_service_list', detail = 'd'",
        )
        .bind(("uuid", drop.id))
        .await
        .unwrap()
        .check()
        .unwrap();

        let report = merge(&db, keep.id, drop.id).await.unwrap();

        assert_eq!(report.kept_id, keep.id);
        assert_eq!(report.emails, vec!["pat@new.example", "pat@old.example"]);
        assert!(persons::find_by_id(&db, drop.id).await.unwrap().is_none());
        let found = persons::find_by_any_email(&db, "PAT@new.example")
            .await
            .unwrap();
        assert_eq!(found.map(|p| p.id), Some(keep.id));
        assert_eq!(
            persons::aliases(&db, keep.id).await.unwrap(),
            vec!["pat@new.example"]
        );
        assert_eq!(
            count(
                &db,
                "SELECT VALUE count() FROM person_project_role WHERE person_id = $id GROUP ALL",
                keep.id
            )
            .await,
            2
        );
        assert_eq!(
            count(
                &db,
                "SELECT VALUE count() FROM person_project_role WHERE person_id = $id GROUP ALL",
                drop.id
            )
            .await,
            0
        );
        // The shared project's row keeps the DRI flag the dropped row carried.
        assert_eq!(
            count(&db, "SELECT VALUE count() FROM person_project_role WHERE person_id = $id AND is_client_dri = true GROUP ALL", keep.id).await,
            2
        );
        assert_eq!(
            count(
                &db,
                "SELECT VALUE count() FROM entity_role WHERE in = $id GROUP ALL",
                keep.id
            )
            .await,
            1
        );
        assert_eq!(
            count(
                &db,
                "SELECT VALUE count() FROM relationship_log WHERE subject_id = $uuid GROUP ALL",
                keep.id
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn merge_redraws_every_graph_edge_and_collapses_a_duplicate_edge() {
        let db = mem().await;
        let keep = person(&db, "Pat", "pat@old.example").await;
        let drop = person(&db, "Pat", "pat@new.example").await;
        let project = record_id("project", Uuid::now_v7());
        let entity = record_id("entity", Uuid::now_v7());
        for who in [keep.id, drop.id] {
            db.query("RELATE $p->entity_role->$e SET role = 'agent'")
                .bind(("p", record_id("person", who)))
                .bind(("e", entity.clone()))
                .await
                .unwrap()
                .check()
                .unwrap();
        }
        db.query("RELATE $proj->project_reference->$p SET reason = 'adverse_party'")
            .bind(("proj", project))
            .bind(("p", record_id("person", drop.id)))
            .await
            .unwrap()
            .check()
            .unwrap();
        for sql in [
            "RELATE $p->relationship->$e SET kind = 'employs', confidence_pct = 90, source_kind = 'manual'",
            "RELATE $e->relationship->$p SET kind = 'owns', confidence_pct = 80, source_kind = 'manual'",
        ] {
            db.query(sql)
                .bind(("p", record_id("person", drop.id)))
                .bind(("e", entity.clone()))
                .await.unwrap().check().unwrap();
        }

        merge(&db, keep.id, drop.id).await.unwrap();

        let q = |sql: &'static str| {
            let db = db.clone();
            async move { count(&db, sql, keep.id).await }
        };
        assert_eq!(
            q("SELECT VALUE count() FROM entity_role WHERE in = $id GROUP ALL").await,
            1
        );
        assert_eq!(
            q("SELECT VALUE count() FROM project_reference WHERE out = $id GROUP ALL").await,
            1
        );
        assert_eq!(
            q("SELECT VALUE count() FROM relationship WHERE in = $id GROUP ALL").await,
            1
        );
        assert_eq!(
            q("SELECT VALUE count() FROM relationship WHERE out = $id GROUP ALL").await,
            1
        );
        assert_eq!(
            count(
                &db,
                "SELECT VALUE count() FROM entity_role WHERE in = $id GROUP ALL",
                drop.id
            )
            .await,
            0
        );
    }

    #[tokio::test]
    async fn merge_by_email_resolves_aliases_and_refuses_one_person_named_twice() {
        let db = mem().await;
        let keep = person(&db, "Pat", "pat@old.example").await;
        let drop = person(&db, "Pat", "pat@new.example").await;
        persons::add_alias(&db, drop.id, "pat@home.example")
            .await
            .unwrap();
        assert!(matches!(
            super::merge_by_email(&db, "pat@old.example", "PAT@old.example").await,
            Err(PersonMergeError::SameRecord)
        ));
        assert!(matches!(
            super::merge_by_email(&db, "pat@old.example", "nobody@x.example").await,
            Err(PersonMergeError::NotFound(email)) if email == "nobody@x.example"
        ));
        let report = super::merge_by_email(&db, "pat@old.example", "pat@home.example")
            .await
            .unwrap();
        assert_eq!(report.kept_id, keep.id);
        assert_eq!(
            report.emails,
            vec!["pat@home.example", "pat@new.example", "pat@old.example"]
        );
    }

    #[tokio::test]
    async fn merge_refuses_the_same_person_a_missing_person_and_a_privileged_drop() {
        let db = mem().await;
        let keep = person(&db, "Pat", "pat@one.example").await;
        assert!(matches!(
            merge(&db, keep.id, keep.id).await,
            Err(PersonMergeError::SameRecord)
        ));
        assert!(matches!(
            merge(&db, keep.id, Uuid::now_v7()).await,
            Err(PersonMergeError::NotFound(_))
        ));
        assert!(matches!(
            merge(&db, Uuid::now_v7(), keep.id).await,
            Err(PersonMergeError::NotFound(_))
        ));
        let lawyer = persons::create(
            &db,
            &NewPerson::with_role("Lex", "lex@firm.example", Role::Lawyer),
        )
        .await
        .unwrap();
        assert!(matches!(
            merge(&db, keep.id, lawyer.id).await,
            Err(PersonMergeError::Blocked)
        ));
        assert!(persons::find_by_id(&db, lawyer.id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn an_alias_is_found_and_refused_to_a_second_person() {
        let db = mem().await;
        let a = person(&db, "A", "a@one.example").await;
        let b = person(&db, "B", "b@one.example").await;
        persons::add_alias(&db, a.id, "A@Two.example")
            .await
            .unwrap();
        persons::add_alias(&db, a.id, "a@two.example")
            .await
            .unwrap();
        assert_eq!(
            persons::aliases(&db, a.id).await.unwrap(),
            vec!["a@two.example"]
        );
        assert_eq!(
            persons::find_by_any_email(&db, "a@two.example")
                .await
                .unwrap()
                .map(|p| p.id),
            Some(a.id)
        );
        assert!(matches!(
            persons::add_alias(&db, b.id, "a@two.example").await,
            Err(persons::PersonError::EmailTaken)
        ));
        assert!(matches!(
            persons::add_alias(&db, b.id, "a@one.example").await,
            Err(persons::PersonError::EmailTaken)
        ));
    }
}
