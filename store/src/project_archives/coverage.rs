//! Recovery coverage: every Project the inventory expects a Project Recovery
//! Archive for, open and closed alike, and whether its newest committed
//! generation still covers it.
//!
//! The inventory is the Projects table, never a workstation's checkout
//! folders. A closed Project stays expected: its record has to outlive its
//! repository. The report carries Project and generation ids, states, and
//! counts only, never content or repository coordinates.
//!
//! Freshness is read from a generation's cutoff, the instant its record was
//! captured, not from when it committed or from a live HEAD that kept moving.
//! An open Project is current while its newest committed cutoff is within
//! [`BASELINE`]. A closed Project is current once a committed cutoff is at or
//! after its close, the same test [`crate::project_closeout`] applies.
//!
//! Reading coverage requests nothing and moves no bytes.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{generation_from, store_error, ArchiveError, Generation, GENERATION_COLUMNS};
use crate::postgres::PostgresDb;
use crate::projects::Project;
use crate::surreal::SurrealDb;

/// The twice-daily full-archive baseline an open Project's newest committed
/// generation is measured against. A reporting threshold, not a
/// recovery-point objective.
pub const BASELINE: TimeDelta = TimeDelta::hours(12);

/// How one expected Project stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    /// The newest committed generation covers the Project.
    Current,
    /// A committed generation exists, but its cutoff is older than
    /// [`BASELINE`] (open) or predates the close (closed).
    Stale,
    /// No committed generation exists.
    Missing,
    /// The Project records a repository, but its newest committed generation
    /// holds no verified Git bundle.
    SourceUnbound,
    /// The Project belongs to no Firm, so no generation can be requested.
    NoFirm,
}

/// What coverage reads from one Project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedProject {
    pub project_id: Uuid,
    pub firm_id: Option<Uuid>,
    pub has_repository: bool,
    /// When the Project closed. `None` for an open Project, and for a closed
    /// one whose close instant is unrecorded, which is then held to
    /// [`BASELINE`] like an open one.
    pub closed_at: Option<DateTime<Utc>>,
}

impl From<&Project> for ExpectedProject {
    fn from(project: &Project) -> Self {
        let closed = matches!(project.status.as_str(), "closed" | "archived");
        Self {
            project_id: project.id,
            firm_id: project.firm_id,
            has_repository: project.repository_url.is_some(),
            closed_at: project
                .closed_at
                .as_deref()
                .filter(|_| closed)
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .map(|value| value.with_timezone(&Utc)),
        }
    }
}

/// One expected Project's coverage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageEntry {
    pub project_id: Uuid,
    pub firm_id: Option<Uuid>,
    pub closed: bool,
    pub has_repository: bool,
    pub coverage: Coverage,
    /// The newest committed generation, if any.
    pub generation_id: Option<Uuid>,
    /// That generation's cutoff: the last verified recovery point.
    pub cutoff_at: Option<DateTime<Utc>>,
    /// An uncommitted generation is already requested for the Project.
    pub pending: bool,
}

/// Counts across one report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageCounts {
    pub expected: usize,
    pub current: usize,
    pub stale: usize,
    pub missing: usize,
    pub source_unbound: usize,
    pub no_firm: usize,
    pub pending: usize,
}

/// One Project that is not current.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Uncovered {
    pub project_id: Uuid,
    pub coverage: Coverage,
    pub pending: bool,
}

/// The counts and the uncovered Projects of one report, small enough to keep
/// as a scheduled run's output.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageSummary {
    pub counts: CoverageCounts,
    pub uncovered: Vec<Uncovered>,
}

/// Every expected Project's coverage at one instant, ordered by Project id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageReport {
    pub checked_at: DateTime<Utc>,
    pub entries: Vec<CoverageEntry>,
}

impl CoverageReport {
    #[must_use]
    pub fn counts(&self) -> CoverageCounts {
        let mut counts = CoverageCounts {
            expected: self.entries.len(),
            ..CoverageCounts::default()
        };
        for entry in &self.entries {
            let slot = match entry.coverage {
                Coverage::Current => &mut counts.current,
                Coverage::Stale => &mut counts.stale,
                Coverage::Missing => &mut counts.missing,
                Coverage::SourceUnbound => &mut counts.source_unbound,
                Coverage::NoFirm => &mut counts.no_firm,
            };
            *slot += 1;
            counts.pending += usize::from(entry.pending);
        }
        counts
    }

    #[must_use]
    pub fn summary(&self) -> CoverageSummary {
        CoverageSummary {
            counts: self.counts(),
            uncovered: self
                .entries
                .iter()
                .filter(|entry| entry.coverage != Coverage::Current)
                .map(|entry| Uncovered {
                    project_id: entry.project_id,
                    coverage: entry.coverage,
                    pending: entry.pending,
                })
                .collect(),
        }
    }
}

fn judge(project: &ExpectedProject, latest: Option<&Generation>, now: DateTime<Utc>) -> Coverage {
    if project.firm_id.is_none() {
        return Coverage::NoFirm;
    }
    let Some(generation) = latest else {
        return Coverage::Missing;
    };
    if project.has_repository && generation.source_backup().is_none() {
        return Coverage::SourceUnbound;
    }
    let cutoff = generation.cutoff_at.unwrap_or(generation.requested_at);
    let covered = match project.closed_at {
        Some(closed_at) => cutoff >= closed_at,
        None => now - cutoff <= BASELINE,
    };
    if covered {
        Coverage::Current
    } else {
        Coverage::Stale
    }
}

/// Judge every expected Project against its newest committed generation
/// (`latest`, keyed by Project) and the Projects with an uncommitted one
/// (`pending`). Generations of Projects outside `projects` are ignored.
#[must_use]
pub fn assess(
    projects: &[ExpectedProject],
    latest: &BTreeMap<Uuid, Generation>,
    pending: &BTreeSet<Uuid>,
    now: DateTime<Utc>,
) -> CoverageReport {
    let mut entries: Vec<CoverageEntry> = projects
        .iter()
        .map(|project| {
            let generation = latest.get(&project.project_id);
            CoverageEntry {
                project_id: project.project_id,
                firm_id: project.firm_id,
                closed: project.closed_at.is_some(),
                has_repository: project.has_repository,
                coverage: judge(project, generation, now),
                generation_id: generation.map(|generation| generation.id),
                cutoff_at: generation
                    .map(|generation| generation.cutoff_at.unwrap_or(generation.requested_at)),
                pending: pending.contains(&project.project_id),
            }
        })
        .collect();
    entries.sort_by_key(|entry| entry.project_id);
    CoverageReport {
        checked_at: now,
        entries,
    }
}

/// Read the whole Project inventory and every Project's newest committed
/// generation, and judge coverage at `now`. Not authority-scoped: like
/// [`super::latest_committed`], it is a fleet fact for the recovery schedule.
///
/// # Errors
/// A Project inventory or database read failure.
pub async fn report(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    now: DateTime<Utc>,
) -> Result<CoverageReport, ArchiveError> {
    let projects: Vec<ExpectedProject> = crate::projects::all(surreal)
        .await
        .map_err(store_error)?
        .iter()
        .map(ExpectedProject::from)
        .collect();
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT DISTINCT ON (project_id) {GENERATION_COLUMNS} FROM project_archive_generation \
         WHERE state = 'committed' ORDER BY project_id, committed_at DESC, id DESC"
    )))
    .fetch_all(pg.pool())
    .await?;
    let latest = rows
        .iter()
        .map(|row| generation_from(row).map(|generation| (generation.project_id, generation)))
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let pending: BTreeSet<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT project_id FROM project_archive_generation WHERE state <> 'committed'",
    )
    .fetch_all(pg.pool())
    .await?
    .into_iter()
    .collect();
    Ok(assess(&projects, &latest, &pending, now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_archives::{GenerationState, SourceBackupReference};
    use chrono::TimeZone;

    fn at(hours: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap() + TimeDelta::hours(hours)
    }

    fn project(firm: bool, repository: bool, closed_at: Option<DateTime<Utc>>) -> ExpectedProject {
        ExpectedProject {
            project_id: Uuid::now_v7(),
            firm_id: firm.then(Uuid::now_v7),
            has_repository: repository,
            closed_at,
        }
    }

    fn committed(project: &ExpectedProject, cutoff: DateTime<Utc>, bundle: bool) -> Generation {
        Generation {
            id: Uuid::now_v7(),
            firm_id: project.firm_id.unwrap_or_default(),
            project_id: project.project_id,
            operation_key: format!("synthetic-{}", project.project_id),
            state: GenerationState::Committed,
            requested_by_person_id: Uuid::now_v7(),
            requested_at: cutoff - TimeDelta::minutes(5),
            cutoff_at: Some(cutoff),
            inventory_sha256: None,
            manifest_key: None,
            manifest_sha256: None,
            manifest_size_bytes: None,
            item_count: Some(1),
            attempts: 1,
            last_error: None,
            committed_at: Some(cutoff + TimeDelta::minutes(1)),
            source_backup: bundle.then(|| SourceBackupReference {
                format: 1,
                object: "synthetic/repository.bundle".into(),
                generation: 1,
                size_bytes: 1,
                sha256: "0".repeat(64),
                head: "refs/heads/main".into(),
                refs: format!("{} refs/heads/main", "a".repeat(40)),
            }),
        }
    }

    fn coverage_of(report: &CoverageReport, project: &ExpectedProject) -> Coverage {
        report
            .entries
            .iter()
            .find(|entry| entry.project_id == project.project_id)
            .expect("every expected Project has an entry")
            .coverage
    }

    #[test]
    fn every_expected_project_is_enumerated_and_judged() {
        let now = at(24);
        let fresh = project(true, true, None);
        let old = project(true, false, None);
        let never = project(true, true, None);
        let unbound = project(true, true, None);
        let orphan = project(false, false, None);
        let closed_after = project(true, true, Some(at(0)));
        let closed_before = project(true, true, Some(at(0)));
        let latest = BTreeMap::from([
            (fresh.project_id, committed(&fresh, at(20), true)),
            (old.project_id, committed(&old, at(1), false)),
            (unbound.project_id, committed(&unbound, at(23), false)),
            // Long past the baseline, but cut after the close: a closed
            // Project's record no longer changes.
            (
                closed_after.project_id,
                committed(&closed_after, at(1), true),
            ),
            (
                closed_before.project_id,
                committed(&closed_before, at(-1), true),
            ),
        ]);
        let projects = vec![
            fresh.clone(),
            old.clone(),
            never.clone(),
            unbound.clone(),
            orphan.clone(),
            closed_after.clone(),
            closed_before.clone(),
        ];
        let report = assess(&projects, &latest, &BTreeSet::from([never.project_id]), now);

        assert_eq!(report.entries.len(), projects.len());
        assert_eq!(coverage_of(&report, &fresh), Coverage::Current);
        assert_eq!(coverage_of(&report, &old), Coverage::Stale);
        assert_eq!(coverage_of(&report, &never), Coverage::Missing);
        assert_eq!(coverage_of(&report, &unbound), Coverage::SourceUnbound);
        assert_eq!(coverage_of(&report, &orphan), Coverage::NoFirm);
        assert_eq!(coverage_of(&report, &closed_after), Coverage::Current);
        assert_eq!(coverage_of(&report, &closed_before), Coverage::Stale);
        assert_eq!(
            report.counts(),
            CoverageCounts {
                expected: 7,
                current: 2,
                stale: 2,
                missing: 1,
                source_unbound: 1,
                no_firm: 1,
                pending: 1,
            }
        );
        let summary = report.summary();
        assert_eq!(summary.uncovered.len(), 5);
        assert!(summary
            .uncovered
            .iter()
            .any(|entry| entry.project_id == never.project_id && entry.pending));
    }

    #[test]
    fn freshness_reads_the_cutoff_and_the_baseline_edge_is_current() {
        let open = project(true, false, None);
        let mut generation = committed(&open, at(0), false);
        // Committed long after the cutoff: the recovery point is still the
        // cutoff, so it is stale.
        generation.committed_at = Some(at(12));
        let latest = BTreeMap::from([(open.project_id, generation)]);
        let projects = [open.clone()];
        let edge = assess(&projects, &latest, &BTreeSet::new(), at(0) + BASELINE);
        assert_eq!(coverage_of(&edge, &open), Coverage::Current);
        assert_eq!(edge.entries[0].cutoff_at, Some(at(0)));
        let past = assess(
            &projects,
            &latest,
            &BTreeSet::new(),
            at(0) + BASELINE + TimeDelta::seconds(1),
        );
        assert_eq!(coverage_of(&past, &open), Coverage::Stale);
    }

    #[test]
    fn a_closed_project_without_a_close_instant_is_held_to_the_baseline() {
        let fields = |status: &str, closed_at: Option<&str>| -> Project {
            serde_json::from_value(serde_json::json!({
                "id": Uuid::nil(), "code": "synthetic", "name": "Synthetic",
                "status": status, "brand": "neon", "entity_id": Uuid::nil(),
                "firm_id": null, "jurisdiction_id": null, "onboarding_jurisdiction": null,
                "description": null, "drive_folder_id": null, "repository_url": null,
                "git_initialized_at": null, "forge_provisioned_at": null,
                "closed_at": closed_at, "closure_reason": null,
                "internal_slack_channel_url": null, "external_slack_channel_url": null,
                "internal_slack_channel_id": null, "private_notion_page_url": null,
                "shared_notion_page_url": null,
                "inserted_at": "2026-10-01T00:00:00Z", "updated_at": "2026-10-01T00:00:00Z"
            }))
            .expect("a synthetic Project")
        };
        let closed = ExpectedProject::from(&fields("closed", Some("2026-11-01T00:00:00Z")));
        assert_eq!(closed.closed_at, Some(at(0)));
        assert!(!closed.has_repository);
        let undated = ExpectedProject::from(&fields("archived", None));
        assert_eq!(undated.closed_at, None);
        // A close instant on an open row is not a close.
        let reopened = ExpectedProject::from(&fields("open", Some("2026-11-01T00:00:00Z")));
        assert_eq!(reopened.closed_at, None);
    }

    #[test]
    fn generations_of_projects_outside_the_inventory_are_ignored() {
        let known = project(true, false, None);
        let stray = project(true, false, None);
        let latest = BTreeMap::from([(stray.project_id, committed(&stray, at(0), false))]);
        let report = assess(
            std::slice::from_ref(&known),
            &latest,
            &BTreeSet::from([stray.project_id]),
            at(1),
        );
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].coverage, Coverage::Missing);
        assert!(!report.entries[0].pending);
    }
}
