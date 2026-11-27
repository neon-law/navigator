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

use crate::application_store::ApplicationStore;
use crate::persons::{Person, Role};
use crate::postgres::{participation, projects as pg_projects, PostgresDb};

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
    #[error("provider did not retain the requested contact changes")]
    NotVerified,
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
    _application_store: &ApplicationStore,
    project_id: Uuid,
    books: &dyn ContactBooks,
) -> Result<ContactSyncReport, ContactSyncError> {
    let pg = crate::persons::postgres_authority().await.map_err(db)?;
    sync_project_members_postgres(&pg, project_id, books).await
}

/// Reconcile one Project using an explicit PostgreSQL authority.
///
/// # Errors
/// A store or provider failure.
#[cfg(test)]
pub(crate) async fn sync_project_members_with_postgres(
    pg: &PostgresDb,
    project_id: Uuid,
    books: &dyn ContactBooks,
) -> Result<ContactSyncReport, ContactSyncError> {
    sync_project_members_postgres(pg, project_id, books).await
}

async fn sync_project_members_postgres(
    pg: &PostgresDb,
    project_id: Uuid,
    books: &dyn ContactBooks,
) -> Result<ContactSyncReport, ContactSyncError> {
    let rows = participation::contacts_for_projects_on(pg, &[project_id])
        .await
        .map_err(db)?;
    let mut report = ContactSyncReport::default();
    for (person_id, name, email, role) in rows {
        if role == Role::Client.as_str() || email.trim().is_empty() {
            continue;
        }
        let member = person_from_projection(person_id, name, email, &role);
        report += sync_member_postgres(pg, &member, books).await?;
    }
    Ok(report)
}

/// Make `member`'s Navigator contacts exactly the other people on their open
/// matters.
///
/// # Errors
/// A store or provider failure.
pub async fn sync_member(
    _application_store: &ApplicationStore,
    member: &Person,
    books: &dyn ContactBooks,
) -> Result<ContactSyncReport, ContactSyncError> {
    let pg = crate::persons::postgres_authority().await.map_err(db)?;
    sync_member_postgres(&pg, member, books).await
}

fn map_contacts(error: ContactsError) -> ContactSyncError {
    if matches!(error, ContactsError::NotVerified) {
        ContactSyncError::NotVerified
    } else {
        ContactSyncError::Contacts(error)
    }
}

fn person_from_projection(id: Uuid, name: String, email: String, role: &str) -> Person {
    Person {
        id,
        name,
        given_name: None,
        family_name: None,
        middle_name: None,
        email,
        oidc_subject: None,
        microsoft_subject: None,
        apple_subject: None,
        role: Role::parse(role).unwrap_or(Role::Client),
        title: None,
        phone: None,
        xero_contact_id: None,
        profile_image_url: None,
        linkedin_url: None,
        email_confirmed: false,
        inserted_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

async fn sync_member_postgres(
    pg: &PostgresDb,
    member: &Person,
    books: &dyn ContactBooks,
) -> Result<ContactSyncReport, ContactSyncError> {
    let wanted = wanted_contacts_postgres(pg, member.id).await?;
    reconcile(member, wanted, books).await
}

async fn reconcile(
    member: &Person,
    wanted: BTreeMap<String, ManagedContact>,
    books: &dyn ContactBooks,
) -> Result<ContactSyncReport, ContactSyncError> {
    let book = books.book(member.email.trim()).await?;
    let mut report = ContactSyncReport {
        members: 1,
        ..Default::default()
    };
    let mut held = BTreeSet::new();
    for stored in book.managed_contacts().await? {
        match wanted.get(&stored.contact.person_id) {
            Some(_) if !held.insert(stored.contact.person_id.clone()) => {
                book.delete_contact(&stored).await.map_err(map_contacts)?;
                report.removed += 1;
            }
            Some(contact) if same(contact, &stored.contact) => {}
            Some(contact) => {
                book.update_contact(&stored, contact)
                    .await
                    .map_err(map_contacts)?;
                report.updated += 1;
            }
            None => {
                book.delete_contact(&stored).await.map_err(map_contacts)?;
                report.removed += 1;
            }
        }
    }
    for (id, contact) in &wanted {
        if !held.contains(id) {
            book.create_contact(contact).await.map_err(map_contacts)?;
            report.created += 1;
        }
    }
    Ok(report)
}

async fn wanted_contacts_postgres(
    pg: &PostgresDb,
    member_id: Uuid,
) -> Result<BTreeMap<String, ManagedContact>, ContactSyncError> {
    let mut projects = BTreeSet::new();
    for row in participation::for_person_projects_on(pg, member_id)
        .await
        .map_err(db)?
    {
        if pg_projects::find_by_id_on(pg, row.project_id)
            .await
            .map_err(db)?
            .is_some_and(|project| project.status == "open")
        {
            projects.insert(row.project_id);
        }
    }
    let people =
        participation::contacts_for_projects_on(pg, &projects.iter().copied().collect::<Vec<_>>())
            .await
            .map_err(db)?;
    let mut wanted = BTreeMap::new();
    for (person_id, name, email, role) in people {
        if person_id == member_id {
            continue;
        }
        let person = person_from_projection(person_id, name, email, &role);
        wanted.insert(person.id.to_string(), contact_for_person(pg, person).await?);
    }
    Ok(wanted)
}

async fn contact_for_person(
    pg: &PostgresDb,
    person: Person,
) -> Result<ManagedContact, ContactSyncError> {
    let phones = crate::person_phones::list_phones_on(pg, person.id)
        .await
        .map_err(db)?
        .into_iter()
        .map(|phone| ContactPhone {
            value: phone.e164,
            label: phone.label.as_str().to_string(),
        })
        .collect();
    let mut emails = BTreeSet::new();
    let primary = person.email.trim().to_lowercase();
    if !primary.is_empty() {
        emails.insert(primary);
    }
    emails.extend(
        crate::postgres::people::aliases(pg, person.id)
            .await
            .map_err(db)?,
    );
    Ok(ManagedContact {
        person_id: person.id.to_string(),
        name: person.name,
        emails: emails.into_iter().collect(),
        phones,
    })
}

/// Equal up to phone order, which the People API does not promise to keep.
fn same(left: &ManagedContact, right: &ManagedContact) -> bool {
    let emails = |contact: &ManagedContact| contact.emails.iter().cloned().collect::<BTreeSet<_>>();
    let phones = |contact: &ManagedContact| contact.phones.iter().cloned().collect::<BTreeSet<_>>();
    left.name == right.name && emails(left) == emails(right) && phones(left) == phones(right)
}
