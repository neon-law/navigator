//! Each firm member's Google Contacts hold the people on their open matters.
//!
//! A lawyer or clerk who participates in a Project gets a contact for every
//! other participant on each of their open Projects, so Google Voice and
//! Google Fi name the caller. One member's book is reconciled whole: a person
//! who is on none of the member's open matters any more is removed, which a
//! single-matter pass could not decide. Only contacts Navigator created are
//! touched ([`cloud::contacts::MANAGED_KEY`]).

use std::collections::{BTreeMap, BTreeSet};

use uuid::Uuid;

use cloud::contacts::{ContactBooks, ContactPhone, ContactsError, ManagedContact};

use crate::persons::{self, Person, Role};
use crate::projects;
use crate::surreal::SurrealDb;

/// What one sync changed, in counts only: names and numbers stay inside it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContactSyncReport {
    pub members: usize,
    pub created: usize,
    pub updated: usize,
    pub removed: usize,
}

impl std::ops::AddAssign for ContactSyncReport {
    fn add_assign(&mut self, other: Self) {
        self.members += other.members;
        self.created += other.created;
        self.updated += other.updated;
        self.removed += other.removed;
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ContactSyncError {
    #[error("contacts: {0}")]
    Contacts(#[from] ContactsError),
    #[error("database: {0}")]
    Db(String),
}

fn db(error: impl std::fmt::Display) -> ContactSyncError {
    ContactSyncError::Db(error.to_string())
}

/// Reconcile the book of every firm-side participant on `project_id`.
///
/// # Errors
/// A store or provider failure; members already synced stay synced.
pub async fn sync_project_members(
    surreal: &SurrealDb,
    project_id: Uuid,
    books: &dyn ContactBooks,
) -> Result<ContactSyncReport, ContactSyncError> {
    let mut report = ContactSyncReport::default();
    for row in projects::participations_for_project(surreal, project_id)
        .await
        .map_err(db)?
    {
        let Some(member) = persons::find_by_id(surreal, row.person_id)
            .await
            .map_err(db)?
        else {
            continue;
        };
        if member.role == Role::Client || member.email.trim().is_empty() {
            continue;
        }
        report += sync_member(surreal, &member, books).await?;
    }
    Ok(report)
}

/// Make `member`'s Navigator contacts exactly the other people on their open
/// matters.
///
/// # Errors
/// A store or provider failure.
pub async fn sync_member(
    surreal: &SurrealDb,
    member: &Person,
    books: &dyn ContactBooks,
) -> Result<ContactSyncReport, ContactSyncError> {
    let wanted = wanted_contacts(surreal, member.id).await?;
    let book = books.book(member.email.trim()).await?;
    let mut report = ContactSyncReport {
        members: 1,
        ..ContactSyncReport::default()
    };
    let mut held = BTreeSet::new();
    for stored in book.managed_contacts().await? {
        match wanted.get(&stored.contact.person_id) {
            // A second copy of one Person is removed like a stale one.
            Some(_) if !held.insert(stored.contact.person_id.clone()) => {
                book.delete_contact(&stored).await?;
                report.removed += 1;
            }
            Some(contact) if same(contact, &stored.contact) => {}
            Some(contact) => {
                book.update_contact(&stored, contact).await?;
                report.updated += 1;
            }
            None => {
                book.delete_contact(&stored).await?;
                report.removed += 1;
            }
        }
    }
    for (person_id, contact) in &wanted {
        if !held.contains(person_id) {
            book.create_contact(contact).await?;
            report.created += 1;
        }
    }
    Ok(report)
}

/// Equal up to phone order, which the People API does not promise to keep.
fn same(left: &ManagedContact, right: &ManagedContact) -> bool {
    let phones = |contact: &ManagedContact| contact.phones.iter().cloned().collect::<BTreeSet<_>>();
    left.name == right.name && left.email == right.email && phones(left) == phones(right)
}

/// Every other participant on the member's open matters, by Person id.
async fn wanted_contacts(
    surreal: &SurrealDb,
    member_id: Uuid,
) -> Result<BTreeMap<String, ManagedContact>, ContactSyncError> {
    let mut people = BTreeSet::new();
    for own in projects::participations_for_person(surreal, member_id)
        .await
        .map_err(db)?
    {
        let open = projects::find_by_id(surreal, own.project_id)
            .await
            .map_err(db)?
            .is_some_and(|project| project.status == "open");
        if !open {
            continue;
        }
        for row in projects::participations_for_project(surreal, own.project_id)
            .await
            .map_err(db)?
        {
            if row.person_id != member_id {
                people.insert(row.person_id);
            }
        }
    }
    let mut wanted = BTreeMap::new();
    for person_id in people {
        let Some(person) = persons::find_by_id(surreal, person_id).await.map_err(db)? else {
            continue;
        };
        let phones = crate::person_phones::list_phones(surreal, person_id)
            .await
            .map_err(db)?
            .into_iter()
            .map(|phone| ContactPhone {
                value: phone.e164,
                label: phone.label.as_str().to_string(),
            })
            .collect();
        let email = Some(person.email.trim().to_string()).filter(|email| !email.is_empty());
        wanted.insert(
            person_id.to_string(),
            ManagedContact {
                person_id: person_id.to_string(),
                name: person.name,
                email,
                phones,
            },
        );
    }
    Ok(wanted)
}

#[cfg(test)]
mod tests {
    use cloud::contacts::FakeContactBooks;

    use super::sync_project_members;
    use crate::person_phones::{self, PhoneLabel};
    use crate::persons::{self, NewPerson, Role};
    use crate::projects::{self, OpenMatterCommand};
    use crate::test_support::{mem_surreal, seed_entity, seed_firm_with_brand};

    const LAWYER: &str = "lawyer@example.com";

    async fn open_matter(surreal: &crate::surreal::SurrealDb) -> (projects::Project, uuid::Uuid) {
        seed_firm_with_brand(surreal, "neon").await;
        let entity_id = seed_entity(surreal).await;
        let lawyer = persons::create(
            surreal,
            &NewPerson::with_role("Lawyer", LAWYER, Role::Lawyer),
        )
        .await
        .expect("lawyer");
        let client = persons::create(
            surreal,
            &NewPerson::with_role("Ada Client", "client@example.com", Role::Client),
        )
        .await
        .expect("client");
        person_phones::set_phone(
            surreal,
            client.id,
            "+1 555 555 0100",
            PhoneLabel::Mobile,
            true,
        )
        .await
        .expect("phone");
        let project = projects::open_matter(
            surreal,
            &OpenMatterCommand {
                firm_id: None,
                name: "Synthetic matter".into(),
                code: "matter-one".into(),
                client_id: client.id,
                entity_id,
                jurisdiction_id: None,
                description: None,
                brand: "neon".to_string(),
                attestation: true,
                acting_person_id: lawyer.id,
                closed_at: None,
            },
        )
        .await
        .expect("open");
        (project, client.id)
    }

    #[tokio::test]
    async fn the_lawyer_gets_the_client_and_the_client_gets_no_book() {
        let surreal = mem_surreal().await;
        let (project, client_id) = open_matter(&surreal).await;
        let books = FakeContactBooks::default();

        let report = sync_project_members(&surreal, project.id, &books)
            .await
            .unwrap();

        assert_eq!(report.members, 1, "only the firm side gets a book");
        assert_eq!(report.created, 1);
        let held = books.contacts(LAWYER);
        let contact = &held[&client_id.to_string()];
        assert_eq!(contact.name, "Ada Client");
        assert_eq!(contact.email.as_deref(), Some("client@example.com"));
        assert_eq!(contact.phones[0].value, "+15555550100");
        assert!(books.contacts("client@example.com").is_empty());
    }

    #[tokio::test]
    async fn a_rerun_changes_nothing_and_a_new_number_is_an_update() {
        let surreal = mem_surreal().await;
        let (project, client_id) = open_matter(&surreal).await;
        let books = FakeContactBooks::default();
        sync_project_members(&surreal, project.id, &books)
            .await
            .unwrap();

        let again = sync_project_members(&surreal, project.id, &books)
            .await
            .unwrap();
        assert_eq!((again.created, again.updated, again.removed), (0, 0, 0));

        person_phones::set_phone(
            &surreal,
            client_id,
            "+1 555 555 0199",
            PhoneLabel::Work,
            false,
        )
        .await
        .unwrap();
        let changed = sync_project_members(&surreal, project.id, &books)
            .await
            .unwrap();
        assert_eq!(changed.updated, 1);
        assert_eq!(
            books.contacts(LAWYER)[&client_id.to_string()].phones.len(),
            2
        );
    }

    #[tokio::test]
    async fn closing_the_matter_removes_its_people() {
        let surreal = mem_surreal().await;
        let (project, _) = open_matter(&surreal).await;
        let books = FakeContactBooks::default();
        sync_project_members(&surreal, project.id, &books)
            .await
            .unwrap();

        // Closing through the lifecycle needs an offboarding document; the
        // sync reads only the status.
        surreal
            .query("UPDATE $id SET status = 'closed', closed_at = '2026-01-01T00:00:00Z'")
            .bind(("id", crate::surreal::record_id("project", project.id)))
            .await
            .and_then(surrealdb::IndexedResults::check)
            .unwrap();
        let report = sync_project_members(&surreal, project.id, &books)
            .await
            .unwrap();

        assert_eq!(report.removed, 1);
        assert!(books.contacts(LAWYER).is_empty());
    }
}
