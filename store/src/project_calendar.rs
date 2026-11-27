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

use std::collections::{BTreeMap, BTreeSet};

use uuid::Uuid;

use cloud::calendar::{CalendarError, CalendarRole, CalendarService};

use crate::application_store::ApplicationStore;
use crate::persons::Role;
use crate::projects::{self, Project, ProjectCommandError};

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
    let db = PostgresDb::application().await?;
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
    application_store: &ApplicationStore,
    project_id: Uuid,
    calendar: &dyn CalendarService,
) -> Result<CalendarReport, ProjectCalendarError> {
    let project = find(application_store, project_id).await?;
    if project.status != "open" {
        return close(application_store, project_id, calendar).await;
    }
    let timezone = projects::calendar_timezone(application_store, &project)
        .await
        .map_err(|error| ProjectCalendarError::Db(error.to_string()))?;
    let (id, outcome) = if let Some(recorded) = configured(&project) {
        (recorded, CalendarOutcome::Present)
    } else {
        match calendar.find_calendar(&project.code).await? {
            Some(found) => (found.id, CalendarOutcome::Adopted),
            None => (
                calendar
                    .create_calendar(&project.code, timezone.name())
                    .await?
                    .id,
                CalendarOutcome::Created,
            ),
        }
    };
    if outcome != CalendarOutcome::Created
        && !calendar
            .recorded_calendar_matches(&project.code, &id)
            .await?
    {
        return Ok(CalendarReport {
            outcome: CalendarOutcome::RecordedResourceMissing,
            changed: 0,
        });
    }
    if outcome != CalendarOutcome::Present {
        projects::set_calendar_id(application_store, project_id, &id)
            .await?
            .ok_or(ProjectCalendarError::NotFound)?;
    }
    if outcome != CalendarOutcome::Created {
        calendar
            .reconcile_calendar_timezone(&project.code, &id, timezone.name())
            .await?;
    }
    Ok(CalendarReport {
        outcome,
        changed: 0,
    })
}

/// Open Projects and recorded calendars whose Firm has a Workspace calendar owner.
pub async fn membership_candidates() -> Result<Vec<Uuid>, ProjectCalendarError> {
    let db = crate::postgres::PostgresDb::application()
        .await
        .map_err(sync_db)?;
    crate::firms::calendar_owners_query()
        .push(
            "SELECT p.id FROM project p JOIN calendar_owners c ON c.firm_id = p.firm_id
               WHERE c.owner IS NOT NULL AND (p.status = 'open' OR p.calendar_id ~ '[^[:space:]]')
               ORDER BY p.firm_id, p.id",
        )
        .build_query_scalar()
        .fetch_all(db.pool())
        .await
        .map_err(sync_db)
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
    application_store: &ApplicationStore,
    project_id: Uuid,
    calendar: &dyn CalendarService,
) -> Result<u64, ProjectCalendarError> {
    let project = find(application_store, project_id).await?;
    let firm_id = project
        .firm_id
        .ok_or_else(|| ProjectCalendarError::Db("Project Firm is missing".into()))?;
    let Some(calendar_id) = configured(&project) else {
        return Ok(0);
    };
    let db = crate::postgres::PostgresDb::application()
        .await
        .map_err(sync_db)?;
    let Some(owner) = crate::firms::calendar_owner(firm_id)
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
}

/// Reconcile direct reader grants after provisioning. The
/// caller holds the same project write lane as setup and the Deadline mirror.
///
/// # Errors
/// Returns an error if the owning Firm, calendar, membership authority, or
/// provider readback cannot be verified.
pub async fn sync_membership(
    application_store: &ApplicationStore,
    project_id: Uuid,
    calendar: &dyn CalendarService,
) -> Result<Vec<ParticipantSync>, ProjectCalendarError> {
    let db = crate::postgres::PostgresDb::application()
        .await
        .map_err(sync_db)?;
    let project = find(application_store, project_id).await?;
    let firm_id = project
        .firm_id
        .ok_or_else(|| ProjectCalendarError::Db("Project Firm is missing".into()))?;
    let Some(owner) = crate::firms::calendar_owner(firm_id)
        .await
        .map_err(sync_db)?
    else {
        return Ok(Vec::new());
    };
    let calendar_id = match configured(&project) {
        Some(id) => id,
        None if project.status == "open" => {
            ensure(application_store, project_id, calendar).await?;
            configured(&find(application_store, project_id).await?)
                .ok_or_else(|| ProjectCalendarError::Db("Project calendar is missing".into()))?
        }
        None => return Ok(Vec::new()),
    };

    // A recorded id is an operator input. Verify it still identifies this
    // Project's calendar in the owning Firm's Workspace before sharing.
    verify_private_calendar(calendar, &project.code, &calendar_id).await?;
    let (desired, mut report) = if project.status == "open" {
        let mut rosters = load_rosters(&db, &[project.id]).await?;
        eligible_roster(
            rosters.remove(&project.id).unwrap_or_default(),
            &owner,
            CalendarPurpose::Access,
        )?
    } else {
        Default::default()
    };

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

    let members = desired.values().map(String::as_str).collect();
    let open_project_code = (project.status == "open").then_some(project.code.as_str());
    let brand_policy = if open_project_code.is_some() {
        let brands = crate::brands::for_firm(application_store, firm_id)
            .await
            .map_err(sync_db)?;
        crate::brand_domains::DomainPolicy::from_brands(&brands)
    } else {
        crate::brand_domains::DomainPolicy::default()
    };
    let workspace_domain = owner
        .trim()
        .rsplit_once('@')
        .map(|(_, domain)| domain.to_ascii_lowercase());
    let internal_domains = brand_policy
        .domains()
        .chain(workspace_domain.as_deref())
        .collect();
    calendar
        .reconcile_project_events(&calendar_id, open_project_code, &members, &internal_domains)
        .await?;
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
    let managed: BTreeSet<String> = sqlx::query_scalar("SELECT email FROM project_calendar_grant WHERE project_id = $1 AND firm_id = $2 AND calendar_id = $3")
    .bind(project_id)
    .bind(firm_id)
    .bind(calendar_id)
    .fetch_all(db.pool())
    .await
    .map_err(sync_db)?
    .into_iter()
    .collect();

    // Revoke expired job grants before attempting new grants. Direct
    // grants that predate this job never enter the ledger.
    for email in managed
        .iter()
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
            // the next pass checks the ACL before inserting another grant.
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
            if !calendar.grant_reader(calendar_id, email).await? && !managed.contains(email) {
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
    }

    Ok(())
}

type RosterRow = (Uuid, String, bool, String, String, bool, Option<String>);
type Roster = (BTreeMap<Uuid, String>, Vec<ParticipantSync>);

async fn load_rosters(
    db: &crate::postgres::PostgresDb,
    projects: &[Uuid],
) -> Result<BTreeMap<Uuid, Vec<RosterRow>>, ProjectCalendarError> {
    let rows = sqlx::query_as::<_, (Uuid, Uuid, String, bool, String, String, bool, Option<String>)>(
        "SELECT r.project_id, p.id, r.participation, r.is_lawyer_dri, p.role, p.email, p.email_confirmed, f.membership
         FROM person_project_role r JOIN person p ON p.id = r.person_id
         JOIN project pr ON pr.id = r.project_id
         LEFT JOIN person_firm_role f ON f.person_id = p.id AND f.firm_id = pr.firm_id
         WHERE r.project_id = ANY($1) AND NOT r.is_client_dri AND r.participation <> ALL($2::text[])
         ORDER BY r.inserted_at, r.id",
    )
    .bind(projects)
    .bind(projects::PARTICIPATION_CLIENT_SIDE)
    .fetch_all(db.pool())
    .await
    .map_err(sync_db)?;
    let mut rosters = BTreeMap::<_, Vec<_>>::new();
    for (project, person, participation, dri, role, email, confirmed, membership) in rows {
        rosters.entry(project).or_default().push((
            person,
            participation,
            dri,
            role,
            email,
            confirmed,
            membership,
        ));
    }
    Ok(rosters)
}

/// A recorded Project Calendar and the participants eligible for a free/busy check.
pub struct SharedCalendar {
    pub project_id: Uuid,
    pub code: String,
    pub calendar_id: Option<String>,
    pub people: Vec<Uuid>,
}

/// Shared calendars to query as each participant, limited to the caller's authorized Firms.
/// The provider checks the participant's access on every free/busy read.
pub async fn shared_freebusy_calendars(
    db: &crate::postgres::PostgresDb,
    firms: &[Uuid],
    people: &[Uuid],
) -> Result<BTreeMap<Uuid, Vec<SharedCalendar>>, ProjectCalendarError> {
    calendar_candidates(db, firms, people, CalendarPurpose::Access).await
}

/// Existing assigned Project Calendars for projecting private busy holds.
pub async fn booking_calendars(
    db: &crate::postgres::PostgresDb,
    firms: &[Uuid],
    person: Uuid,
) -> Result<BTreeMap<Uuid, Vec<SharedCalendar>>, ProjectCalendarError> {
    calendar_candidates(db, firms, &[person], CalendarPurpose::Booking).await
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CalendarPurpose {
    Access,
    Booking,
}

async fn calendar_candidates(
    db: &crate::postgres::PostgresDb,
    firms: &[Uuid],
    people: &[Uuid],
    purpose: CalendarPurpose,
) -> Result<BTreeMap<Uuid, Vec<SharedCalendar>>, ProjectCalendarError> {
    let projects: Vec<(Uuid, Uuid, String, Option<String>, String)> = crate::firms::calendar_owners_query()
        .push("SELECT p.id, p.firm_id, p.code, p.calendar_id, c.owner FROM project p JOIN calendar_owners c ON c.firm_id = p.firm_id
               WHERE c.owner IS NOT NULL AND p.status = 'open' AND p.firm_id = ANY(")
        .push_bind(firms)
        .push(") AND EXISTS (SELECT 1 FROM person_project_role r WHERE r.project_id = p.id AND r.person_id = ANY(")
        .push_bind(people)
        .push("))")
        .build_query_as()
        .fetch_all(db.pool())
        .await
        .map_err(sync_db)?;
    let ids: Vec<_> = projects.iter().map(|(id, ..)| *id).collect();
    let mut rosters = load_rosters(db, &ids).await?;
    let mut calendars = BTreeMap::<_, Vec<_>>::new();
    for (id, firm, code, calendar_id, owner) in projects {
        if purpose == CalendarPurpose::Booking
            && calendar_id.as_deref().is_none_or(|id| id.trim().is_empty())
        {
            continue;
        }
        let (eligible, _) =
            eligible_roster(rosters.remove(&id).unwrap_or_default(), &owner, purpose)?;
        let people: Vec<_> = eligible
            .into_keys()
            .filter(|person| people.contains(person))
            .collect();
        if !people.is_empty() {
            calendars.entry(firm).or_default().push(SharedCalendar {
                project_id: id,
                code,
                calendar_id,
                people,
            });
        }
    }
    Ok(calendars)
}

fn eligible_roster(
    rows: Vec<RosterRow>,
    owner: &str,
    purpose: CalendarPurpose,
) -> Result<Roster, ProjectCalendarError> {
    let owner_domain = owner.rsplit_once('@').map(|(_, domain)| domain);
    let mut desired = BTreeMap::new();
    let mut report = Vec::new();
    let mut has_lawyer_dri = false;
    let mut clerks = Vec::new();
    for (person_id, participation, is_lawyer_dri, role, email, confirmed, membership) in rows {
        let role = Role::parse(&role).ok_or_else(|| sync_db("unknown persisted Person role"))?;
        let expected = match role {
            Role::Clerk => crate::firms::FirmMembership::Clerk,
            Role::Lawyer => crate::firms::FirmMembership::Lawyer,
            Role::Owner | Role::Admin => crate::firms::FirmMembership::Admin,
            Role::Client => continue,
        };
        let address = email.trim().to_ascii_lowercase();
        // A promotion can leave an existing lawyer assignment on the same side
        // of the Project. Use it only for generating private Busy holds.
        let booking_promotion = purpose == CalendarPurpose::Booking
            && participation == "lawyer"
            && matches!(role, Role::Admin | Role::Owner);
        let gap = if participation != projects::participation_for_role(role) && !booking_promotion {
            Some("project_role_mismatch")
        } else if membership.as_deref() != Some(expected.as_str()) {
            Some("not_a_firm_member")
        } else {
            delivery_gap(&address, confirmed, owner_domain)
        };
        report.push(ParticipantSync {
            person_id,
            access: gap.unwrap_or("pending").into(),
        });
        if gap.is_none() {
            if role == Role::Clerk {
                clerks.push(person_id);
            } else {
                has_lawyer_dri |= is_lawyer_dri;
            }
            desired.insert(person_id, address);
        }
    }

    if !has_lawyer_dri {
        for clerk in clerks {
            desired.remove(&clerk);
            if let Some(row) = report.iter_mut().find(|row| row.person_id == clerk) {
                row.access = "no_supervising_lawyer".into();
            }
        }
    }

    Ok((desired, report))
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
    application_store: &ApplicationStore,
    project_id: Uuid,
    calendar: &dyn CalendarService,
) -> Result<CalendarReport, ProjectCalendarError> {
    let project = find(application_store, project_id).await?;
    if project.booking_url.is_some() {
        projects::clear_booking_url(application_store, project_id).await?;
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

async fn find(
    application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<Project, ProjectCalendarError> {
    projects::find_by_id(application_store, project_id)
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
    use crate::projects::{self, UpdateProjectCommand};
    use crate::test_support::seed_calendar_project;

    #[tokio::test]
    async fn replay_keeps_each_calendar_event_revision_once() {
        use super::backup_events;
        use cloud::calendar::CalendarEventBackup;

        crate::test_support::with_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
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
            let db = crate::application_store::ApplicationStore::uninitialized();
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
            let db = crate::application_store::ApplicationStore::uninitialized();
            let (project, firm) = seed_calendar_project().await;
            set_calendar_owner(&db, Role::Owner, None, firm, FAKE_CALENDAR_OWNER)
                .await
                .unwrap();
            assert_eq!(super::membership_candidates().await.unwrap(), [project]);
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
            assert_eq!(super::membership_candidates().await.unwrap(), [project]);

            set_calendar_owner(&db, Role::Owner, None, firm, "")
                .await
                .unwrap();
            assert!(super::membership_candidates().await.unwrap().is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn a_recorded_calendar_for_another_project_is_never_used_for_cleanup() {
        crate::test_support::with_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let (project, _) = seed_calendar_project().await;
            let calendar = FakeCalendar::default();
            let other = calendar
                .create_calendar("other-project", "UTC")
                .await
                .unwrap();
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

    async fn move_membership_to_another_firm(person_id: uuid::Uuid, firm_id: uuid::Uuid) {
        let pg = crate::postgres::PostgresDb::application().await.unwrap();
        sqlx::query(
            "WITH other AS (
                 INSERT INTO firm (name, slug, status)
                 VALUES ('Other Calendar Firm', 'other-calendar-firm', 'active') RETURNING id)
             UPDATE person_firm_role SET firm_id = other.id FROM other
             WHERE person_id = $1 AND firm_id = $2",
        )
        .bind(person_id)
        .bind(firm_id)
        .execute(pg.pool())
        .await
        .unwrap();
    }

    async fn assert_no_shared_calendars(
        db: &crate::postgres::PostgresDb,
        firms: &[uuid::Uuid],
        person: uuid::Uuid,
    ) {
        assert!(super::shared_freebusy_calendars(db, firms, &[person])
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn assignments_share_calendar_access_and_freebusy_for_the_same_roster() {
        use crate::firms::{self, FirmMembership, NewPersonFirmRole};
        use crate::participation::{
            add_participant, remove_participant, AddParticipantCommand, DriActor, DriRequest,
        };

        crate::test_support::with_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let (project_id, firm_id) = seed_calendar_project().await;
            firms::set_calendar_owner(&db, Role::Owner, None, firm_id, FAKE_CALENDAR_OWNER)
                .await
                .unwrap();
            let calendar = FakeCalendar::default();
            ensure(&db, project_id, &calendar).await.unwrap();
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
                assert_eq!(
                    calendar.sharing(&calendar_id).get(&email),
                    Some(&CalendarRole::Reader)
                );
                let pg = crate::postgres::PostgresDb::application().await.unwrap();
                let calendars = super::shared_freebusy_calendars(&pg, &[firm_id], &[person.id])
                    .await
                    .unwrap();
                assert!(calendars[&firm_id]
                    .iter()
                    .any(|shared| shared.calendar_id.as_ref() == Some(&calendar_id)
                        && shared.people.contains(&person.id)));
                assert_no_shared_calendars(&pg, &[], person.id).await;
                if role == Role::Owner {
                    sqlx::query(
                        "UPDATE person_project_role SET participation = 'lawyer' WHERE id = $1",
                    )
                    .bind(row.id)
                    .execute(pg.pool())
                    .await
                    .unwrap();
                    assert_no_shared_calendars(&pg, &[firm_id], person.id).await;
                    let booking = super::booking_calendars(&pg, &[firm_id], person.id)
                        .await
                        .unwrap();
                    assert_eq!(
                        booking[&firm_id][0].calendar_id.as_ref(),
                        Some(&calendar_id)
                    );
                    assert!(super::booking_calendars(&pg, &[], person.id)
                        .await
                        .unwrap()
                        .is_empty());
                    sqlx::query("UPDATE project SET calendar_id = NULL WHERE id = $1")
                        .bind(project_id)
                        .execute(pg.pool())
                        .await
                        .unwrap();
                    assert!(super::booking_calendars(&pg, &[firm_id], person.id)
                        .await
                        .unwrap()
                        .is_empty());
                    projects::set_calendar_id(&db, project_id, &calendar_id)
                        .await
                        .unwrap();
                }
                if row.is_lawyer_dri {
                    continue;
                }
                if name == "lawyer" {
                    move_membership_to_another_firm(person.id, firm_id).await;
                    let report = super::sync_membership(&db, project_id, &calendar)
                        .await
                        .unwrap();
                    assert!(report
                        .iter()
                        .any(|r| r.person_id == person.id && r.access == "not_a_firm_member"));
                    assert_no_shared_calendars(&pg, &[firm_id], person.id).await;
                    assert!(!calendar.sharing(&calendar_id).contains_key(&email));
                }
                remove_participant(&db, project_id, row.id, DriActor::System)
                    .await
                    .unwrap();
                super::sync_membership(&db, project_id, &calendar)
                    .await
                    .unwrap();
                assert!(!calendar.sharing(&calendar_id).contains_key(&email));
                assert_no_shared_calendars(&pg, &[firm_id], person.id).await;
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
        crate::test_support::with_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let (project, _) = seed_calendar_project().await;
            let calendar = FakeCalendar::default();
            for (email, role) in [
                ("lawyer@example.com", Role::Lawyer),
                ("clerk@example.com", Role::Clerk),
                ("client@example.com", Role::Client),
            ] {
                let person =
                    persons::create(&db, &NewPerson::with_role("Participant", email, role))
                        .await
                        .unwrap();
                projects::add_participation(
                    &db,
                    project,
                    person.id,
                    projects::participation_for_role(role),
                )
                .await
                .unwrap();
            }

            let report = ensure(&db, project, &calendar).await.unwrap();

            assert_eq!(report.outcome, CalendarOutcome::Created);
            assert_eq!(report.changed, 0);
            let created = calendar.calendars();
            assert_eq!(created.len(), 1);
            assert_eq!(created[0].summary, "calendar-test");
            let stored = projects::find_by_id(&db, project).await.unwrap().unwrap();
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
        })
        .await;
    }

    #[tokio::test]
    async fn setup_is_idempotent() {
        crate::test_support::with_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let (project, _) = seed_calendar_project().await;
            let calendar = FakeCalendar::default();

            ensure(&db, project, &calendar).await.unwrap();
            let again = ensure(&db, project, &calendar).await.unwrap();

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

            let report = ensure(&db, project, &calendar).await.unwrap();

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
        })
        .await;
    }

    #[tokio::test]
    async fn an_unrecorded_calendar_named_for_the_project_is_adopted() {
        crate::test_support::with_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let (project, _) = seed_calendar_project().await;
            let calendar = FakeCalendar::default();
            projects::update_project(
                &db,
                project,
                &UpdateProjectCommand {
                    calendar_timezone: Some("America/Phoenix".into()),
                    ..UpdateProjectCommand::default()
                },
            )
            .await
            .unwrap();
            let existing = calendar
                .create_calendar("calendar-test", "UTC")
                .await
                .unwrap();

            let report = ensure(&db, project, &calendar).await.unwrap();

            assert_eq!(report.outcome, CalendarOutcome::Adopted);
            assert_eq!(calendar.created(), 1);
            assert_eq!(
                calendar.timezone(&existing.id).as_deref(),
                Some("America/Phoenix")
            );
            let stored = projects::find_by_id(&db, project).await.unwrap().unwrap();
            assert_eq!(stored.calendar_id.as_deref(), Some(existing.id.as_str()));
        })
        .await;
    }

    #[tokio::test]
    async fn a_deleted_recorded_calendar_is_reported_not_replaced() {
        crate::test_support::with_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let (project, _) = seed_calendar_project().await;
            let calendar = FakeCalendar::default();
            ensure(&db, project, &calendar).await.unwrap();
            let id = calendar.calendars()[0].id.clone();
            calendar.delete(&id);

            let report = ensure(&db, project, &calendar).await.unwrap();

            assert_eq!(report.outcome, CalendarOutcome::RecordedResourceMissing);
            assert_eq!(calendar.created(), 1, "nothing is created in its place");
        })
        .await;
    }

    #[tokio::test]
    async fn close_makes_writers_readers_keeps_the_owner_and_clears_the_booking_link() {
        crate::test_support::with_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let (project, _) = seed_calendar_project().await;
            let calendar = FakeCalendar::default();
            ensure(&db, project, &calendar).await.unwrap();
            let id = calendar.calendars()[0].id.clone();
            calendar
                .set_member_role(&id, "lawyer@example.com", CalendarRole::Writer)
                .await
                .unwrap();
            projects::update_project(
                &db,
                project,
                &UpdateProjectCommand {
                    booking_url: Some("https://calendar.app.google/synthetic".into()),
                    ..UpdateProjectCommand::default()
                },
            )
            .await
            .unwrap();

            let report = close(&db, project, &calendar).await.unwrap();

            assert_eq!(report.outcome, CalendarOutcome::ReadOnly);
            assert_eq!(report.changed, 1);
            let sharing = calendar.sharing(&id);
            assert_eq!(
                sharing.get("lawyer@example.com"),
                Some(&CalendarRole::Reader)
            );
            assert_eq!(sharing.get(FAKE_CALENDAR_OWNER), Some(&CalendarRole::Owner));
            let stored = projects::find_by_id(&db, project).await.unwrap().unwrap();
            assert_eq!(stored.booking_url, None);
            assert_eq!(stored.calendar_id.as_deref(), Some(id.as_str()));
        })
        .await;
    }

    #[tokio::test]
    async fn setup_on_a_closed_matter_stays_read_only() {
        crate::test_support::with_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let (project, _) = seed_calendar_project().await;
            let calendar = FakeCalendar::default();
            ensure(&db, project, &calendar).await.unwrap();
            // Closed directly: the lifecycle's close needs an offboarding letter.
            let pg = crate::postgres::PostgresDb::application().await.unwrap();
            sqlx::query("UPDATE project SET status = 'closed' WHERE id = $1")
                .bind(project)
                .execute(pg.pool())
                .await
                .unwrap();

            let report = ensure(&db, project, &calendar).await.unwrap();

            assert_eq!(report.outcome, CalendarOutcome::ReadOnly);
            let id = calendar.calendars()[0].id.clone();
            assert_eq!(
                calendar.sharing(&id),
                std::collections::BTreeMap::from([(
                    FAKE_CALENDAR_OWNER.into(),
                    CalendarRole::Owner
                )])
            );
        })
        .await;
    }
}
