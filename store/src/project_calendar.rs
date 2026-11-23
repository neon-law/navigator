//! Provision a private Project Calendar and reconcile assigned Workspace members.
//!
//! The provider derives the calendar name from the Project code, keeping
//! client names out of the Firm's calendar list. The id is recorded as
//! `project.calendar_id` before sharing, so a partial failure can resume
//! against the same calendar.
//!
//! Setup and cleanup verify the recorded id against the expected Project name
//! and owning account. A missing or mismatched calendar requires review.
//!
//! Firm-side participants receive read access. Navigator writes Deadline
//! events through the delegated Calendar owner. A client books through the
//! Project's `booking_url`. The DBOS job tracks the direct grants it creates
//! and preserves other Google Calendar grants.

mod retirement;
pub use retirement::confirm_archived_calendars;

use std::collections::BTreeMap;

use uuid::Uuid;

use cloud::calendar::{CalendarError, CalendarRole, CalendarService};

use crate::persons::{self, Role};
use crate::projects::{self, Project, ProjectCommandError};
use crate::surreal::SurrealDb;

/// What one calendar pass did, as a closed slug a door reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalendarOutcome {
    /// This pass created the calendar and recorded its id.
    Created,
    /// This pass found an unrecorded calendar named for the Project and
    /// recorded its id.
    Adopted,
    /// The recorded calendar matches the Project name and owner.
    Present,
    /// The matter is closed: writers were made readers and the booking link
    /// was cleared.
    ReadOnly,
    /// The recorded id cannot be verified as this Project's calendar. An
    /// operator reviews the link before another calendar can be provisioned.
    RecordedResourceMissing,
}

impl CalendarOutcome {
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Adopted => "adopted",
            Self::Present => "present",
            Self::ReadOnly => "read_only",
            Self::RecordedResourceMissing => "recorded_resource_missing",
        }
    }
}

/// A calendar pass's outcome and how many people it changed. Counts only:
/// addresses stay inside the pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalendarReport {
    pub outcome: CalendarOutcome,
    /// People newly shared as readers or whose writer grant became a reader grant.
    pub changed: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectCalendarError {
    #[error("no matter with that id")]
    NotFound,
    #[error("calendar: {0}")]
    Calendar(#[from] CalendarError),
    #[error("Calendar readback failed for Person {person_id}: {source}")]
    Participant {
        person_id: Uuid,
        #[source]
        source: CalendarError,
    },
    #[error("{0}")]
    Command(#[from] ProjectCommandError),
    #[error("database: {0}")]
    Db(String),
}

/// A cross-replica Calendar write lane. Dropping it releases the transaction lock.
pub struct CalendarWriteLock {
    _transaction: sqlx::Transaction<'static, sqlx::Postgres>,
}

/// Lock Calendar writes for one Project when PostgreSQL is configured.
/// # Errors
/// Returns an error when the configured database or the write lane is unavailable.
pub async fn lock_writes(
    project_id: Uuid,
) -> Result<Option<CalendarWriteLock>, crate::postgres::PostgresError> {
    use crate::postgres::{PostgresDb, PostgresError};
    let Some(db) = PostgresDb::configured().await? else {
        return Ok(None);
    };
    let mut transaction = db
        .pool()
        .begin()
        .await
        .map_err(|_| PostgresError::Connection)?;
    sqlx::query("SET LOCAL lock_timeout = '5s'")
        .execute(&mut *transaction)
        .await
        .map_err(|_| PostgresError::Connection)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('project_calendar'), hashtext($1))")
        .bind(project_id.to_string())
        .execute(&mut *transaction)
        .await
        .map_err(|_| PostgresError::Connection)?;
    Ok(Some(CalendarWriteLock {
        _transaction: transaction,
    }))
}

/// Create or adopt the Project's calendar. A closed or archived matter is
/// made read-only instead.
///
/// # Errors
/// A store or provider failure.
pub async fn ensure(
    surreal: &SurrealDb,
    project_id: Uuid,
    calendar: &dyn CalendarService,
) -> Result<CalendarReport, ProjectCalendarError> {
    let project = find(surreal, project_id).await?;
    if project.status != "open" {
        return close(surreal, project_id, calendar).await;
    }
    let outcome = if let Some(recorded) = configured(&project) {
        if !calendar
            .recorded_calendar_matches(&project.code, &recorded)
            .await?
        {
            return Ok(CalendarReport {
                outcome: CalendarOutcome::RecordedResourceMissing,
                changed: 0,
            });
        }
        CalendarOutcome::Present
    } else {
        let (found, outcome) = match calendar.find_calendar(&project.code).await? {
            Some(found) => (found, CalendarOutcome::Adopted),
            None => (
                calendar.create_calendar(&project.code).await?,
                CalendarOutcome::Created,
            ),
        };
        projects::set_calendar_id(surreal, project_id, &found.id)
            .await?
            .ok_or(ProjectCalendarError::NotFound)?;
        outcome
    };
    Ok(CalendarReport {
        outcome,
        changed: 0,
    })
}

/// Open Projects and recorded calendars whose Firm has a Workspace calendar owner.
pub async fn membership_candidates(surreal: &SurrealDb) -> Result<Vec<Uuid>, ProjectCalendarError> {
    let db = crate::postgres::PostgresDb::application()
        .await
        .map_err(sync_db)?;
    let groups = sqlx::query_as::<_, (Uuid, Vec<Uuid>)>(
        "SELECT firm_id, array_agg(id ORDER BY id) FROM project
         WHERE firm_id IS NOT NULL AND (status = 'open' OR calendar_id ~ '[^[:space:]]')
         GROUP BY firm_id ORDER BY firm_id",
    )
    .fetch_all(db.pool())
    .await
    .map_err(sync_db)?;
    let mut candidates = Vec::new();
    for (firm, projects) in groups {
        if crate::firms::calendar_owner(surreal, firm)
            .await
            .map_err(sync_db)?
            .is_some()
        {
            candidates.extend(projects);
        }
    }
    Ok(candidates)
}

async fn verify_private_calendar(
    calendar: &dyn CalendarService,
    code: &str,
    calendar_id: &str,
) -> Result<(), ProjectCalendarError> {
    if !calendar
        .recorded_calendar_matches(code, calendar_id)
        .await?
    {
        return Err(CalendarError::Api {
            action: "verifying Project Calendar ownership",
            status: 404,
        }
        .into());
    }
    calendar.verify_private_acl(calendar_id).await?;
    Ok(())
}

/// Preserve every observed event revision on the verified private calendar.
pub async fn backup_events(
    surreal: &SurrealDb,
    project_id: Uuid,
    calendar: &dyn CalendarService,
) -> Result<u64, ProjectCalendarError> {
    let project = find(surreal, project_id).await?;
    let firm_id = project
        .firm_id
        .ok_or_else(|| ProjectCalendarError::Db("Project Firm is missing".into()))?;
    let Some(calendar_id) = configured(&project) else {
        return Ok(0);
    };
    let db = crate::postgres::PostgresDb::application()
        .await
        .map_err(sync_db)?;
    let Some(owner) = crate::firms::calendar_owner(surreal, firm_id)
        .await
        .map_err(sync_db)?
    else {
        return Ok(0);
    };
    if retirement::finish_missing(&db, project_id, firm_id, &calendar_id, &owner, calendar).await? {
        return Ok(0);
    }
    verify_private_calendar(calendar, &project.code, &calendar_id).await?;
    let mut events = calendar.event_backups(&calendar_id).await?;
    let closed = matches!(project.status.as_str(), "closed" | "archived");
    let inventory = closed.then(|| retirement::Inventory::capture(&mut events));
    let mut resources = Vec::with_capacity(events.len());
    let mut etags = Vec::with_capacity(events.len());
    let mut bodies = Vec::with_capacity(events.len());
    for event in events {
        resources.push(event.href);
        etags.push(event.etag);
        bodies.push(event.ics);
    }
    let (inserted, complete) = sqlx::query_as::<_, (i64, bool)>(
        "WITH incoming AS (SELECT * FROM unnest($4::text[], $5::text[], $6::text[]) AS e(href,etag,ics)),
         saved AS (INSERT INTO project_calendar_event_backup (project_id,firm_id,calendar_id,resource_href,etag,ics)
                   SELECT $1,$2,$3,href,etag,ics FROM incoming ON CONFLICT DO NOTHING RETURNING 1),
         existing AS (SELECT 1 FROM incoming i JOIN project_calendar_event_backup b
                      ON (b.project_id,b.firm_id,b.calendar_id,b.resource_href,b.etag,b.ics)=($1,$2,$3,i.href,i.etag,i.ics))
         SELECT count(*), count(*)+(SELECT count(*) FROM existing)=(SELECT count(*) FROM incoming) FROM saved",
    )
    .bind(project_id).bind(firm_id).bind(&calendar_id)
    .bind(&resources).bind(&etags).bind(&bodies)
    .fetch_one(db.pool()).await.map_err(sync_db)?;
    if !complete {
        return Err(CalendarError::InvalidEventBackup.into());
    }
    if let Some(inventory) = inventory {
        retirement::retire(
            &db,
            &project,
            firm_id,
            &calendar_id,
            &owner,
            &inventory,
            calendar,
        )
        .await?;
    }
    u64::try_from(inserted).map_err(sync_db)
}

/// Provider readback for one assigned Person, without persisting their
/// delivery address or event details in DBOS.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParticipantSync {
    pub person_id: Uuid,
    pub access: String,
    pub calendar_invitation: String,
    pub event_responses: BTreeMap<String, usize>,
}

/// Reconcile direct reader grants and event guests after provisioning. The
/// caller holds the same project write lane as setup and the Deadline mirror.
///
/// # Errors
/// Returns an error if the owning Firm, calendar, membership authority, or
/// provider readback cannot be verified.
pub async fn sync_membership(
    surreal: &SurrealDb,
    project_id: Uuid,
    calendar: &dyn CalendarService,
) -> Result<Vec<ParticipantSync>, ProjectCalendarError> {
    let db = crate::postgres::PostgresDb::configured()
        .await
        .map_err(sync_db)?
        .ok_or_else(|| ProjectCalendarError::Db("PostgreSQL not configured".into()))?;
    let project = find(surreal, project_id).await?;
    let firm_id = project
        .firm_id
        .ok_or_else(|| ProjectCalendarError::Db("Project Firm is missing".into()))?;
    let Some(owner) = crate::firms::calendar_owner(surreal, firm_id)
        .await
        .map_err(sync_db)?
    else {
        return Ok(Vec::new());
    };
    let calendar_id = match configured(&project) {
        Some(id) => id,
        None if project.status == "open" => {
            ensure(surreal, project_id, calendar).await?;
            configured(&find(surreal, project_id).await?)
                .ok_or_else(|| ProjectCalendarError::Db("Project calendar is missing".into()))?
        }
        None => return Ok(Vec::new()),
    };

    // A recorded id is an operator input. Verify it still identifies this
    // Project's calendar in the owning Firm's Workspace before ACL or event writes.
    verify_private_calendar(calendar, &project.code, &calendar_id).await?;
    let (desired, guests, mut report) = current_roster(surreal, &project, firm_id, &owner).await?;

    reconcile_reader_grants(
        &db,
        calendar,
        project_id,
        firm_id,
        &calendar_id,
        &desired,
        &mut report,
    )
    .await?;

    let mut event_responses = calendar.sync_event_guests(&calendar_id, &guests).await?;
    for row in &mut report {
        if let Some(responses) = event_responses.remove(&row.person_id.simple().to_string()) {
            row.event_responses = responses;
        }
    }
    Ok(report)
}

async fn reconcile_reader_grants(
    db: &crate::postgres::PostgresDb,
    calendar: &dyn CalendarService,
    project_id: Uuid,
    firm_id: Uuid,
    calendar_id: &str,
    desired: &BTreeMap<Uuid, String>,
    report: &mut [ParticipantSync],
) -> Result<(), ProjectCalendarError> {
    let mut managed: BTreeMap<String, bool> = sqlx::query_as::<_, (String, bool)>("SELECT email, notified FROM project_calendar_grant WHERE project_id = $1 AND firm_id = $2 AND calendar_id = $3")
    .bind(project_id)
    .bind(firm_id)
    .bind(calendar_id)
    .fetch_all(db.pool())
    .await
    .map_err(sync_db)?
    .into_iter()
    .collect();

    // Revoke expired job grants before attempting new invitations. Direct
    // grants that predate this job never enter the ledger.
    for email in managed
        .keys()
        .filter(|email| !desired.values().any(|value| value == *email))
    {
        calendar.revoke_reader(calendar_id, email).await?;
        delete_grant(db, project_id, firm_id, calendar_id, email).await?;
    }
    for (person_id, email) in desired {
        calendar
            .verify_workspace_member(email)
            .await
            .map_err(|source| ProjectCalendarError::Participant {
                person_id: *person_id,
                source,
            })?;
    }

    let current: BTreeMap<_, _> = calendar
        .members(calendar_id)
        .await?
        .into_iter()
        .map(|member| (member.email.to_ascii_lowercase(), member.role))
        .collect();
    for row in report {
        let Some(email) = desired.get(&row.person_id) else {
            continue;
        };
        if !current.contains_key(email) {
            // Record intent before making a side effect. On a lost response
            // the next pass reads ACL state before it ever tries to invite.
            sqlx::query(
                "INSERT INTO project_calendar_grant (project_id, firm_id, calendar_id, email)
                 VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
            )
            .bind(project_id)
            .bind(firm_id)
            .bind(calendar_id)
            .bind(email)
            .execute(db.pool())
            .await
            .map_err(sync_db)?;
            if calendar.invite_reader(calendar_id, email).await? {
                sqlx::query("UPDATE project_calendar_grant SET notified = true WHERE project_id = $1 AND firm_id = $2 AND calendar_id = $3 AND email = $4")
                .bind(project_id).bind(firm_id).bind(calendar_id).bind(email)
                .execute(db.pool()).await.map_err(sync_db)?;
                managed.insert(email.clone(), true);
            } else if !managed.contains_key(email) {
                delete_grant(db, project_id, firm_id, calendar_id, email).await?;
            }
        }
        let error_for = |source| ProjectCalendarError::Participant {
            person_id: row.person_id,
            source,
        };
        if !calendar
            .ensure_reader_calendar(calendar_id, email)
            .await
            .map_err(error_for)?
        {
            return Err(error_for(CalendarError::Api {
                action: "verifying assigned participant access",
                status: 403,
            }));
        }
        row.access = "verified".into();
        row.calendar_invitation = match managed.get(email) {
            Some(true) => "notification_requested",
            Some(false) => "notification_unconfirmed",
            None => "existing_access",
        }
        .into();
    }

    Ok(())
}

type Roster = (
    BTreeMap<Uuid, String>,
    BTreeMap<String, String>,
    Vec<ParticipantSync>,
);

async fn current_roster(
    surreal: &SurrealDb,
    project: &Project,
    firm_id: Uuid,
    owner: &str,
) -> Result<Roster, ProjectCalendarError> {
    let owner_domain = owner.rsplit_once('@').map(|(_, domain)| domain);
    let rows = projects::participations_for_project(surreal, project.id)
        .await
        .map_err(sync_db)?;
    let mut desired = BTreeMap::new();
    let mut guests = BTreeMap::new();
    let mut report = Vec::new();
    let mut has_lawyer_dri = false;
    let mut clerks = Vec::new();
    if project.status == "open" {
        for row in rows {
            if row.is_client_dri
                || projects::PARTICIPATION_CLIENT_SIDE.contains(&row.participation.as_str())
            {
                continue;
            }
            let Some(person) = persons::find_by_id(surreal, row.person_id)
                .await
                .map_err(sync_db)?
            else {
                continue;
            };
            let expected = match person.role {
                Role::Clerk => crate::firms::FirmMembership::Clerk,
                Role::Lawyer => crate::firms::FirmMembership::Lawyer,
                Role::Owner | Role::Admin => crate::firms::FirmMembership::Admin,
                Role::Client => continue,
            };
            let membership = crate::firms::membership_for_person(surreal, row.person_id, firm_id)
                .await
                .map_err(sync_db)?;
            let address = person.email.trim().to_ascii_lowercase();
            let gap = if row.participation != projects::participation_for_role(person.role) {
                Some("project_role_mismatch")
            } else if membership.is_none_or(|membership| membership.membership != expected) {
                Some("not_a_firm_member")
            } else {
                delivery_gap(&address, person.email_confirmed, owner_domain)
            };
            report.push(ParticipantSync {
                person_id: person.id,
                access: gap.unwrap_or("pending").into(),
                calendar_invitation: "not_requested".into(),
                event_responses: BTreeMap::new(),
            });
            if gap.is_none() {
                if person.role == Role::Clerk {
                    clerks.push(person.id);
                } else {
                    has_lawyer_dri |= row.is_lawyer_dri;
                }
                if receives_event_invitation(person.role) {
                    guests.insert(person.id.simple().to_string(), address.clone());
                }
                desired.insert(person.id, address);
            }
        }
    }

    if !has_lawyer_dri {
        for clerk in clerks {
            desired.remove(&clerk);
            guests.remove(&clerk.simple().to_string());
            if let Some(row) = report.iter_mut().find(|row| row.person_id == clerk) {
                row.access = "no_supervising_lawyer".into();
            }
        }
    }

    Ok((desired, guests, report))
}

fn receives_event_invitation(role: Role) -> bool {
    matches!(role, Role::Clerk | Role::Lawyer | Role::Admin)
}

fn delivery_gap(
    address: &str,
    confirmed: bool,
    owner_domain: Option<&str>,
) -> Option<&'static str> {
    let Some((local, domain)) = address.rsplit_once('@') else {
        return Some("missing_or_placeholder_address");
    };
    if local.is_empty()
        || local.contains('@')
        || address.chars().any(char::is_whitespace)
        || domain.ends_with(".invalid")
        || domain == "example.com"
    {
        Some("missing_or_placeholder_address")
    } else if !confirmed {
        Some("unverified_address")
    } else if !owner_domain.is_some_and(|owner| domain.eq_ignore_ascii_case(owner)) {
        Some("outside_firm_workspace")
    } else {
        None
    }
}

async fn delete_grant(
    db: &crate::postgres::PostgresDb,
    project_id: Uuid,
    firm_id: Uuid,
    calendar_id: &str,
    email: &str,
) -> Result<(), ProjectCalendarError> {
    sqlx::query("DELETE FROM project_calendar_grant WHERE project_id = $1 AND firm_id = $2 AND calendar_id = $3 AND email = $4")
    .bind(project_id)
    .bind(firm_id)
    .bind(calendar_id)
    .bind(email)
    .execute(db.pool())
    .await
    .map_err(sync_db)?;
    Ok(())
}

fn sync_db(error: impl std::fmt::Display) -> ProjectCalendarError {
    ProjectCalendarError::Db(error.to_string())
}

/// Make the Project's calendar read-only and clear its booking link. Every
/// writer becomes a reader; the owner is untouched. The calendar and its
/// events are kept: they are part of the matter's record.
///
/// A Project with no recorded calendar, or whose calendar is gone, still has
/// its booking link cleared.
///
/// # Errors
/// A store or provider failure.
pub async fn close(
    surreal: &SurrealDb,
    project_id: Uuid,
    calendar: &dyn CalendarService,
) -> Result<CalendarReport, ProjectCalendarError> {
    let project = find(surreal, project_id).await?;
    if project.booking_url.is_some() {
        projects::clear_booking_url(surreal, project_id).await?;
    }
    let Some(calendar_id) = configured(&project) else {
        return Ok(CalendarReport {
            outcome: CalendarOutcome::ReadOnly,
            changed: 0,
        });
    };
    if !calendar
        .recorded_calendar_matches(&project.code, &calendar_id)
        .await?
    {
        return Ok(CalendarReport {
            outcome: CalendarOutcome::RecordedResourceMissing,
            changed: 0,
        });
    }
    let mut changed = 0;
    for member in calendar.members(&calendar_id).await? {
        if member.role == CalendarRole::Writer {
            calendar
                .set_member_role(&calendar_id, &member.email, CalendarRole::Reader)
                .await?;
            changed += 1;
        }
    }
    Ok(CalendarReport {
        outcome: CalendarOutcome::ReadOnly,
        changed,
    })
}

async fn find(surreal: &SurrealDb, project_id: Uuid) -> Result<Project, ProjectCalendarError> {
    projects::find_by_id(surreal, project_id)
        .await
        .map_err(|error| ProjectCalendarError::Db(error.to_string()))?
        .ok_or(ProjectCalendarError::NotFound)
}

fn configured(project: &Project) -> Option<String> {
    project
        .calendar_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use cloud::calendar::{CalendarRole, CalendarService, FakeCalendar, FAKE_CALENDAR_OWNER};

    use super::{close, ensure, CalendarOutcome};
    use crate::persons::{self, NewPerson, Role};
    use crate::projects::{self, OpenMatterCommand, UpdateProjectCommand};
    use crate::test_support::{
        mem_surreal, seed_calendar_project, seed_entity, seed_firm_with_brand,
    };

    async fn open_matter(surreal: &crate::surreal::SurrealDb) -> projects::Project {
        seed_firm_with_brand(surreal, "neon").await;
        let entity_id = seed_entity(surreal).await;
        let lawyer = persons::create(
            surreal,
            &NewPerson::with_role("Lawyer", "Lawyer@Example.com", Role::Lawyer),
        )
        .await
        .expect("lawyer");
        let client = persons::create(
            surreal,
            &NewPerson::with_role("Client", "client@example.com", Role::Client),
        )
        .await
        .expect("client");
        projects::open_matter(
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
        .expect("open")
    }

    #[test]
    fn assigned_lawyers_join_event_guests_without_inviting_clients_or_owners() {
        assert!(super::receives_event_invitation(Role::Lawyer));
        assert!(super::receives_event_invitation(Role::Clerk));
        assert!(super::receives_event_invitation(Role::Admin));
        assert!(!super::receives_event_invitation(Role::Client));
        assert!(!super::receives_event_invitation(Role::Owner));
    }

    #[tokio::test]
    async fn replay_keeps_each_calendar_event_revision_once() {
        use super::backup_events;
        use cloud::calendar::CalendarEventBackup;

        crate::test_support::with_postgres(async {
            let db = crate::surreal::SurrealDb::uninitialized();
            let (project, firm) = seed_calendar_project().await;
            crate::firms::set_calendar_owner(&db, Role::Owner, None, firm, FAKE_CALENDAR_OWNER).await.unwrap();
            let calendar = FakeCalendar::default();
            ensure(&db, project, &calendar).await.unwrap();
            let id = calendar.calendars().pop().unwrap().id;
            for etag in ["one", "two"] {
                calendar.set_event_backups(&id, vec![CalendarEventBackup {
                    href: "/event.ics".into(), etag: etag.into(),
                    ics: format!("BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:{etag}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"),
                }]);
                assert_eq!(backup_events(&db, project, &calendar).await.unwrap(), 1);
                assert_eq!(backup_events(&db, project, &calendar).await.unwrap(), 0);
            }
            let pg = crate::postgres::PostgresDb::application().await.unwrap();
            let saved = sqlx::query_scalar::<_, String>("SELECT ics FROM project_calendar_event_backup WHERE project_id=$1 ORDER BY etag")
                .bind(project).fetch_all(pg.pool()).await.unwrap();
            assert_eq!(saved.len(), 2);
            assert!(saved[0].contains("SUMMARY:one\r\n"));
            assert!(saved[1].contains("SUMMARY:two\r\n"));
        }).await;
    }

    #[tokio::test]
    async fn a_closed_calendar_waits_for_an_archive_covering_its_latest_events() {
        use cloud::calendar::CalendarEventBackup;
        crate::test_support::with_postgres(async {
            let db = crate::surreal::SurrealDb::uninitialized();
            let pg = crate::postgres::PostgresDb::application().await.unwrap();
            let (project, firm) = seed_calendar_project().await;
            crate::firms::set_calendar_owner(&db, Role::Owner, None, firm, FAKE_CALENDAR_OWNER).await.unwrap();
            let calendar = FakeCalendar::default();
            ensure(&db, project, &calendar).await.unwrap();
            let id = calendar.calendars().pop().unwrap().id;
            sqlx::query("UPDATE project SET status='closed' WHERE id=$1").bind(project).execute(pg.pool()).await.unwrap();

            let snapshot = || sqlx::query_scalar::<_, String>("SELECT pg_current_snapshot()::text").fetch_one(pg.pool());
            let too_early = snapshot().await.unwrap();
            super::backup_events(&db, project, &calendar).await.unwrap();
            super::confirm_archived_calendars(&pg, &too_early, uuid::Uuid::now_v7()).await.unwrap();
            super::backup_events(&db, project, &calendar).await.unwrap();
            assert!(calendar.calendar_exists(&id).await.unwrap());

            super::confirm_archived_calendars(&pg, &snapshot().await.unwrap(), uuid::Uuid::now_v7()).await.unwrap();
            calendar.set_event_backups(&id, vec![CalendarEventBackup {
                href: "/late.ics".into(), etag: "late".into(), ics: "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:late\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n".into(),
            }]);
            super::backup_events(&db, project, &calendar).await.unwrap();
            assert!(calendar.calendar_exists(&id).await.unwrap());
            super::confirm_archived_calendars(&pg, &snapshot().await.unwrap(), uuid::Uuid::now_v7()).await.unwrap();
            sqlx::query("UPDATE project SET status='open' WHERE id=$1").bind(project).execute(pg.pool()).await.unwrap();
            super::backup_events(&db, project, &calendar).await.unwrap();
            assert!(calendar.calendar_exists(&id).await.unwrap());
            sqlx::query("UPDATE project SET status='closed' WHERE id=$1").bind(project).execute(pg.pool()).await.unwrap();
            super::backup_events(&db, project, &calendar).await.unwrap();
            assert!(!calendar.calendar_exists(&id).await.unwrap());
            assert!(projects::find_by_id(&db, project).await.unwrap().unwrap().calendar_id.is_none());
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM project_calendar_event_backup WHERE project_id=$1")
                .bind(project).fetch_one(pg.pool()).await.unwrap();
            assert_eq!(count, 1);
        }).await;
    }

    #[tokio::test]
    async fn membership_candidates_follow_the_firm_calendar_owner() {
        use crate::firms::set_calendar_owner;

        crate::test_support::with_postgres(async {
            let db = crate::surreal::SurrealDb::uninitialized();
            let (project, firm) = seed_calendar_project().await;
            set_calendar_owner(&db, Role::Owner, None, firm, FAKE_CALENDAR_OWNER)
                .await
                .unwrap();
            assert_eq!(super::membership_candidates(&db).await.unwrap(), [project]);
            let calendar = FakeCalendar::default();
            for _ in 0..2 {
                super::sync_membership(&db, project, &calendar)
                    .await
                    .unwrap();
            }
            assert_eq!(calendar.created(), 1);
            let saved = projects::find_by_id(&db, project).await.unwrap().unwrap();
            assert_eq!(
                saved.calendar_id.as_deref(),
                Some(calendar.calendars()[0].id.as_str())
            );
            assert_eq!(super::membership_candidates(&db).await.unwrap(), [project]);

            set_calendar_owner(&db, Role::Owner, None, firm, "")
                .await
                .unwrap();
            assert!(super::membership_candidates(&db).await.unwrap().is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn a_recorded_calendar_for_another_project_is_never_used_for_cleanup() {
        crate::test_support::with_postgres(async {
            let db = crate::surreal::SurrealDb::uninitialized();
            let (project, _) = seed_calendar_project().await;
            let calendar = FakeCalendar::default();
            let other = calendar.create_calendar("other-project").await.unwrap();
            let writer = "writer@calendar.test";
            calendar
                .set_member_role(&other.id, writer, CalendarRole::Writer)
                .await
                .unwrap();
            projects::set_calendar_id(&db, project, &other.id)
                .await
                .unwrap();

            assert_eq!(
                ensure(&db, project, &calendar).await.unwrap().outcome,
                CalendarOutcome::RecordedResourceMissing
            );
            let preview = crate::deadline_calendar::reconcile(&db, &calendar, project, None, true)
                .await
                .unwrap();
            assert_eq!(
                preview.outcome,
                crate::deadline_calendar::MirrorOutcome::RecordedResourceMissing
            );
            assert_eq!(
                close(&db, project, &calendar).await.unwrap().outcome,
                CalendarOutcome::RecordedResourceMissing
            );
            assert_eq!(
                calendar.sharing(&other.id).get(writer),
                Some(&CalendarRole::Writer)
            );
            assert_eq!(calendar.created(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn postgres_assignments_add_and_remove_each_eligible_event_guest() {
        use crate::firms::{self, FirmMembership, NewPersonFirmRole};
        use crate::participation::{
            add_participant, remove_participant, AddParticipantCommand, DriActor, DriRequest,
        };

        crate::test_support::with_postgres(async {
            let db = crate::surreal::SurrealDb::uninitialized();
            let (project_id, firm_id) = seed_calendar_project().await;
            firms::set_calendar_owner(&db, Role::Owner, None, firm_id, FAKE_CALENDAR_OWNER)
                .await
                .unwrap();
            let calendar = FakeCalendar::default();
            assert_eq!(
                ensure(&db, project_id, &calendar).await.unwrap().outcome,
                CalendarOutcome::Created
            );
            assert_eq!(
                ensure(&db, project_id, &calendar).await.unwrap().outcome,
                CalendarOutcome::Present
            );
            assert_eq!(calendar.created(), 1);
            let calendar_id = calendar.calendars().pop().unwrap().id;
            for (name, role, membership) in [
                ("supervisor", Role::Lawyer, FirmMembership::Lawyer),
                ("clerk", Role::Clerk, FirmMembership::Clerk),
                ("lawyer", Role::Lawyer, FirmMembership::Lawyer),
                ("admin", Role::Admin, FirmMembership::Admin),
                ("owner", Role::Owner, FirmMembership::Admin),
            ] {
                let dri = if name == "supervisor" {
                    DriRequest::Designate(projects::DriSide::Lawyer)
                } else {
                    DriRequest::Unchanged
                };
                let email = format!("participant-{name}@calendar.test");
                let person = persons::create(&db, &NewPerson::with_role(name, &email, role))
                    .await
                    .unwrap();
                persons::set_email_confirmed(&db, person.id, true)
                    .await
                    .unwrap();
                firms::add_membership(
                    &db,
                    &NewPersonFirmRole {
                        person_id: person.id,
                        firm_id,
                        membership,
                        is_dri: false,
                    },
                )
                .await
                .unwrap();
                let row = add_participant(
                    &db,
                    &AddParticipantCommand {
                        project_id,
                        person_id: person.id,
                        dri,
                        actor: DriActor::System,
                    },
                )
                .await
                .unwrap();
                super::sync_membership(&db, project_id, &calendar)
                    .await
                    .unwrap();
                let guests = calendar.assigned_guests(&calendar_id);
                let key = person.id.simple().to_string();
                if role == Role::Owner {
                    assert!(!guests.contains_key(&key));
                } else {
                    assert_eq!(guests.get(&key), Some(&email));
                }
                if row.is_lawyer_dri {
                    continue;
                }
                remove_participant(&db, project_id, row.id, DriActor::System)
                    .await
                    .unwrap();
                super::sync_membership(&db, project_id, &calendar)
                    .await
                    .unwrap();
                let guests = calendar.assigned_guests(&calendar_id);
                assert!(!guests.contains_key(&key));
                assert!(guests
                    .values()
                    .any(|email| email == "participant-supervisor@calendar.test"));
            }
        })
        .await;
    }

    #[test]
    fn only_confirmed_addresses_inside_the_firm_workspace_are_eligible() {
        let firm = Some("firm.test");
        assert_eq!(super::delivery_gap("clerk@firm.test", true, firm), None);
        assert_eq!(
            super::delivery_gap("clerk@another-firm.test", true, firm),
            Some("outside_firm_workspace")
        );
        assert_eq!(
            super::delivery_gap("clerk@firm.test", false, firm),
            Some("unverified_address")
        );
        assert_eq!(
            super::delivery_gap("clerk@placeholder.invalid", true, firm),
            Some("missing_or_placeholder_address")
        );
    }

    #[tokio::test]
    async fn setup_creates_and_records_a_private_calendar() {
        let surreal = mem_surreal().await;
        let project = open_matter(&surreal).await;
        let calendar = FakeCalendar::default();
        let clerk = persons::create(
            &surreal,
            &NewPerson::with_role("Clerk", "Clerk@Example.com", Role::Clerk),
        )
        .await
        .unwrap();
        projects::add_participation(&surreal, project.id, clerk.id, "clerk")
            .await
            .unwrap();

        let report = ensure(&surreal, project.id, &calendar).await.unwrap();

        assert_eq!(report.outcome, CalendarOutcome::Created);
        assert_eq!(report.changed, 0);
        let created = calendar.calendars();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].summary, "matter-one");
        let stored = projects::find_by_id(&surreal, project.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.calendar_id.as_deref(), Some(created[0].id.as_str()));
        let sharing = calendar.sharing(&created[0].id);
        for email in ["lawyer@example.com", "clerk@example.com"] {
            assert!(!sharing.contains_key(email));
        }
        assert_eq!(sharing.get(FAKE_CALENDAR_OWNER), Some(&CalendarRole::Owner));
        assert!(
            !sharing.contains_key("client@example.com"),
            "a client is never shared the calendar: {sharing:?}"
        );
    }

    #[tokio::test]
    async fn setup_is_idempotent() {
        let surreal = mem_surreal().await;
        let project = open_matter(&surreal).await;
        let calendar = FakeCalendar::default();

        ensure(&surreal, project.id, &calendar).await.unwrap();
        let again = ensure(&surreal, project.id, &calendar).await.unwrap();

        assert_eq!(again.outcome, CalendarOutcome::Present);
        assert_eq!(again.changed, 0, "nobody is re-shared");
        assert_eq!(calendar.created(), 1, "no second calendar");

        let id = calendar.calendars()[0].id.clone();
        calendar
            .set_member_role(&id, "lawyer@example.com", CalendarRole::Writer)
            .await
            .unwrap();
        calendar
            .set_member_role(&id, "manual@example.com", CalendarRole::Writer)
            .await
            .unwrap();

        let report = ensure(&surreal, project.id, &calendar).await.unwrap();

        assert_eq!(report.changed, 0);
        let sharing = calendar.sharing(&id);
        assert_eq!(
            sharing.get("lawyer@example.com"),
            Some(&CalendarRole::Writer)
        );
        assert_eq!(sharing.get(FAKE_CALENDAR_OWNER), Some(&CalendarRole::Owner));
        assert_eq!(
            sharing.get("manual@example.com"),
            Some(&CalendarRole::Writer)
        );
    }

    #[tokio::test]
    async fn an_unrecorded_calendar_named_for_the_project_is_adopted() {
        let surreal = mem_surreal().await;
        let project = open_matter(&surreal).await;
        let calendar = FakeCalendar::default();
        let existing = calendar.create_calendar("matter-one").await.unwrap();

        let report = ensure(&surreal, project.id, &calendar).await.unwrap();

        assert_eq!(report.outcome, CalendarOutcome::Adopted);
        assert_eq!(calendar.created(), 1);
        let stored = projects::find_by_id(&surreal, project.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.calendar_id.as_deref(), Some(existing.id.as_str()));
    }

    #[tokio::test]
    async fn a_deleted_recorded_calendar_is_reported_not_replaced() {
        let surreal = mem_surreal().await;
        let project = open_matter(&surreal).await;
        let calendar = FakeCalendar::default();
        ensure(&surreal, project.id, &calendar).await.unwrap();
        let id = calendar.calendars()[0].id.clone();
        calendar.delete(&id);

        let report = ensure(&surreal, project.id, &calendar).await.unwrap();

        assert_eq!(report.outcome, CalendarOutcome::RecordedResourceMissing);
        assert_eq!(calendar.created(), 1, "nothing is created in its place");
    }

    #[tokio::test]
    async fn close_makes_writers_readers_keeps_the_owner_and_clears_the_booking_link() {
        let surreal = mem_surreal().await;
        let project = open_matter(&surreal).await;
        let calendar = FakeCalendar::default();
        ensure(&surreal, project.id, &calendar).await.unwrap();
        let id = calendar.calendars()[0].id.clone();
        calendar
            .set_member_role(&id, "lawyer@example.com", CalendarRole::Writer)
            .await
            .unwrap();
        projects::update_project(
            &surreal,
            project.id,
            &UpdateProjectCommand {
                booking_url: Some("https://calendar.app.google/synthetic".into()),
                ..UpdateProjectCommand::default()
            },
        )
        .await
        .unwrap();

        let report = close(&surreal, project.id, &calendar).await.unwrap();

        assert_eq!(report.outcome, CalendarOutcome::ReadOnly);
        assert_eq!(report.changed, 1);
        let sharing = calendar.sharing(&id);
        assert_eq!(
            sharing.get("lawyer@example.com"),
            Some(&CalendarRole::Reader)
        );
        assert_eq!(sharing.get(FAKE_CALENDAR_OWNER), Some(&CalendarRole::Owner));
        let stored = projects::find_by_id(&surreal, project.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.booking_url, None);
        assert_eq!(stored.calendar_id.as_deref(), Some(id.as_str()));
    }

    #[tokio::test]
    async fn setup_on_a_closed_matter_stays_read_only() {
        let surreal = mem_surreal().await;
        let project = open_matter(&surreal).await;
        let calendar = FakeCalendar::default();
        ensure(&surreal, project.id, &calendar).await.unwrap();
        // Closed directly: the lifecycle's close needs an offboarding letter.
        surreal
            .query("UPDATE $id SET status = 'closed'")
            .bind(("id", crate::surreal::record_id("project", project.id)))
            .await
            .and_then(surrealdb::IndexedResults::check)
            .unwrap();

        let report = ensure(&surreal, project.id, &calendar).await.unwrap();

        assert_eq!(report.outcome, CalendarOutcome::ReadOnly);
        let id = calendar.calendars()[0].id.clone();
        assert_eq!(
            calendar.sharing(&id),
            std::collections::BTreeMap::from([(FAKE_CALENDAR_OWNER.into(), CalendarRole::Owner)])
        );
    }
}
