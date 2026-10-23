//! Seed import for the rows behind the `estate` and `cap_table` Modules:
//! `seeds/FiduciaryRole.yaml`, `seeds/EstateAsset.yaml`, and
//! `seeds/CapTableHolding.yaml` in a Project repository.
//!
//! Each record names its Project by `project: { code }`. A scoped session
//! may omit it and always writes to its own Project; a record naming another
//! Project is refused. A Person is named by `person: { email }` and an Entity
//! by `entity: { name }`; neither is created here, so the parties import
//! first (`Person.yaml`, `Entity.yaml`). The natural key is each table's
//! unique line. An existing match is left alone unless `overwrite` asks for
//! the other fields to be replaced.

use serde::Deserialize;

use super::{
    parse_seed, require_unique, OperatorEntityNameRef, OperatorPersonRef, OperatorProjectRef,
    ReconcileAction, ReconcileActor, ReconcileRecord, ReconcileReport, ScopeViolation, SeedModel,
};
use crate::project_references::Party;
use crate::projects::Project;
use crate::surreal::SurrealDb;

pub(super) const FIDUCIARY_ROLE_LOOKUP_FIELDS: &[&str] = &["project_id", "role", "successor_order"];
pub(super) const ESTATE_ASSET_LOOKUP_FIELDS: &[&str] = &["project_id", "category", "description"];
pub(super) const CAP_TABLE_HOLDING_LOOKUP_FIELDS: &[&str] =
    &["project_id", "holder_id", "share_class"];

/// One Fiduciary Role record: the slot, and exactly one of `person` or
/// `entity` holding it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FiduciaryRoleRec {
    #[serde(default)]
    project: Option<OperatorProjectRef>,
    role: String,
    successor_order: i64,
    #[serde(default)]
    person: Option<OperatorPersonRef>,
    #[serde(default)]
    entity: Option<OperatorEntityNameRef>,
}

/// One Estate Asset record: a line of the schedule.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EstateAssetRec {
    #[serde(default)]
    project: Option<OperatorProjectRef>,
    category: String,
    description: String,
    passes_by: String,
    funding_state: String,
    #[serde(default)]
    note: Option<String>,
}

/// One Cap Table Holding record: exactly one of `person` or `entity`, a
/// share class, and a whole number of shares.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CapTableHoldingRec {
    #[serde(default)]
    project: Option<OperatorProjectRef>,
    #[serde(default)]
    person: Option<OperatorPersonRef>,
    #[serde(default)]
    entity: Option<OperatorEntityNameRef>,
    share_class: String,
    shares: i64,
    #[serde(default)]
    vesting_note: Option<String>,
}

/// The natural-key half naming a party, or why the record is malformed.
fn party_key(
    model: SeedModel,
    person: Option<&OperatorPersonRef>,
    entity: Option<&OperatorEntityNameRef>,
) -> anyhow::Result<String> {
    match (person, entity) {
        (Some(person), None) => Ok(format!("person:{}", person.email.trim().to_lowercase())),
        (None, Some(entity)) => Ok(format!("entity:{}", entity.name.trim().to_lowercase())),
        (Some(_), Some(_)) => anyhow::bail!(
            "{} seed record names both a person and an entity",
            model.term()
        ),
        (None, None) => anyhow::bail!(
            "{} seed record names neither a person nor an entity",
            model.term()
        ),
    }
}

fn project_key(project: Option<&OperatorProjectRef>) -> String {
    project
        .map(|project| project.code.trim().to_lowercase())
        .unwrap_or_default()
}

pub(super) fn validate_fiduciary_roles(yaml: &str) -> anyhow::Result<()> {
    let model = SeedModel::FiduciaryRole;
    let records = parse_seed::<FiduciaryRoleRec>(yaml, model, FIDUCIARY_ROLE_LOOKUP_FIELDS)?;
    let mut keys = Vec::with_capacity(records.len());
    for record in &records {
        crate::matter_record_links::one_of(
            "fiduciary_role role",
            &record.role,
            &crate::fiduciary_roles::ROLES,
        )
        .map_err(anyhow::Error::msg)?;
        party_key(model, record.person.as_ref(), record.entity.as_ref())?;
        keys.push(format!(
            "{}\u{0}{}\u{0}{}",
            project_key(record.project.as_ref()),
            record.role.trim(),
            record.successor_order
        ));
    }
    require_unique(model, "project.code, role, and successor_order", keys)
}

pub(super) fn validate_estate_assets(yaml: &str) -> anyhow::Result<()> {
    let model = SeedModel::EstateAsset;
    let records = parse_seed::<EstateAssetRec>(yaml, model, ESTATE_ASSET_LOOKUP_FIELDS)?;
    for record in &records {
        crate::matter_record_links::one_of(
            "estate_asset passes_by",
            &record.passes_by,
            &crate::estate_assets::PASSES_BY,
        )
        .map_err(anyhow::Error::msg)?;
        crate::matter_record_links::one_of(
            "estate_asset funding_state",
            &record.funding_state,
            &crate::estate_assets::FUNDING_STATES,
        )
        .map_err(anyhow::Error::msg)?;
    }
    require_unique(
        model,
        "project.code, category, and description",
        records.iter().map(|record| {
            format!(
                "{}\u{0}{}\u{0}{}",
                project_key(record.project.as_ref()),
                record.category.trim(),
                record.description.trim()
            )
        }),
    )
}

pub(super) fn validate_cap_table_holdings(yaml: &str) -> anyhow::Result<()> {
    let model = SeedModel::CapTableHolding;
    let records = parse_seed::<CapTableHoldingRec>(yaml, model, CAP_TABLE_HOLDING_LOOKUP_FIELDS)?;
    let mut keys = Vec::with_capacity(records.len());
    for record in &records {
        if record.shares < 0 {
            anyhow::bail!("cap_table_holding seed shares must be zero or more");
        }
        keys.push(format!(
            "{}\u{0}{}\u{0}{}",
            project_key(record.project.as_ref()),
            party_key(model, record.person.as_ref(), record.entity.as_ref())?,
            record.share_class.trim()
        ));
    }
    require_unique(model, "project.code, holder, and share_class", keys)
}

/// The Project a record writes to: the session's own under scope, or the
/// one the record names.
async fn resolve_project(
    surreal: &SurrealDb,
    model: SeedModel,
    scope: Option<&Project>,
    named: Option<&OperatorProjectRef>,
) -> anyhow::Result<Project> {
    match (scope, named) {
        (Some(scope), Some(named)) if named.code.trim() != scope.code => {
            Err(anyhow::Error::new(ScopeViolation::CrossProject))
        }
        (Some(scope), _) => Ok(scope.clone()),
        (None, Some(named)) => crate::projects::find_by_code(surreal, named.code.trim())
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{} seed references unknown project code {:?}",
                    model.term(),
                    named.code
                )
            }),
        (None, None) => anyhow::bail!(
            "{} seed record names no project; add `project: {{ code: <code> }}`",
            model.term()
        ),
    }
}

/// The existing Person or Entity a record names.
async fn resolve_party(
    surreal: &SurrealDb,
    model: SeedModel,
    person: Option<&OperatorPersonRef>,
    entity: Option<&OperatorEntityNameRef>,
) -> anyhow::Result<Party> {
    party_key(model, person, entity)?;
    if let Some(person) = person {
        let found = crate::persons::find_by_email_ci(surreal, person.email.trim())
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{} seed references unknown person email {:?}",
                    model.term(),
                    person.email
                )
            })?;
        return Ok(Party::person(found.id));
    }
    let entity = entity.expect("party_key admits exactly one of person or entity");
    let found = crate::entities::find_by_name(surreal, entity.name.trim())
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{} seed references unknown entity {:?}",
                model.term(),
                entity.name
            )
        })?;
    Ok(Party::entity(found.id))
}

/// What one record does to the report, given what is already stored.
enum Plan {
    New,
    Unchanged,
    Changed(Vec<String>),
}

fn record_plan(report: &mut ReconcileReport, key: String, plan: &Plan) {
    let (action, changed_fields) = match plan {
        Plan::New => {
            report.created += 1;
            (ReconcileAction::New, Vec::new())
        }
        Plan::Unchanged => {
            report.unchanged += 1;
            (ReconcileAction::Unchanged, Vec::new())
        }
        Plan::Changed(fields) => {
            report.updated += 1;
            (ReconcileAction::Changed, fields.clone())
        }
    };
    report.records.push(ReconcileRecord {
        key,
        action,
        changed_fields,
        jurisdiction: None,
    });
}

/// `Changed` with the fields that differ when `overwrite` asks for them,
/// otherwise `Unchanged`.
fn plan_existing(overwrite: bool, differing: Vec<String>) -> Plan {
    if overwrite && !differing.is_empty() {
        Plan::Changed(differing)
    } else {
        Plan::Unchanged
    }
}

fn differing<const N: usize>(pairs: [(&str, bool); N]) -> Vec<String> {
    pairs
        .into_iter()
        .filter(|(_, same)| !same)
        .map(|(name, _)| name.to_string())
        .collect()
}

pub(super) async fn reconcile_fiduciary_roles(
    surreal: &SurrealDb,
    yaml: &str,
    overwrite: bool,
    actor: &ReconcileActor<'_>,
    scope: Option<&Project>,
) -> anyhow::Result<ReconcileReport> {
    let model = SeedModel::FiduciaryRole;
    let mut report = ReconcileReport {
        model: model.term().to_string(),
        ..ReconcileReport::default()
    };
    for rec in parse_seed::<FiduciaryRoleRec>(yaml, model, FIDUCIARY_ROLE_LOOKUP_FIELDS)? {
        let project = resolve_project(surreal, model, scope, rec.project.as_ref()).await?;
        let party = resolve_party(surreal, model, rec.person.as_ref(), rec.entity.as_ref()).await?;
        let role = rec.role.trim().to_string();
        let key = format!("{} / {role} / {}", project.code, rec.successor_order);
        let existing =
            crate::fiduciary_roles::find_by_slot(surreal, project.id, &role, rec.successor_order)
                .await?;
        let fields = crate::fiduciary_roles::FiduciaryRoleFields {
            role,
            fiduciary: party,
            successor_order: rec.successor_order,
            notation_id: existing.as_ref().and_then(|row| row.notation_id),
        };
        let plan = match &existing {
            None => Plan::New,
            Some(row) => plan_existing(
                overwrite,
                differing([("fiduciary", row.fiduciary == party)]),
            ),
        };
        if !actor.dry_run {
            match (&plan, &existing) {
                (Plan::New, _) => {
                    crate::fiduciary_roles::insert(surreal, project.id, &fields).await?;
                }
                (Plan::Changed(_), Some(row)) => {
                    crate::fiduciary_roles::update(surreal, project.id, row.id, &fields).await?;
                }
                _ => {}
            }
        }
        record_plan(&mut report, key, &plan);
    }
    Ok(report)
}

pub(super) async fn reconcile_estate_assets(
    surreal: &SurrealDb,
    yaml: &str,
    overwrite: bool,
    actor: &ReconcileActor<'_>,
    scope: Option<&Project>,
) -> anyhow::Result<ReconcileReport> {
    let model = SeedModel::EstateAsset;
    let mut report = ReconcileReport {
        model: model.term().to_string(),
        ..ReconcileReport::default()
    };
    for rec in parse_seed::<EstateAssetRec>(yaml, model, ESTATE_ASSET_LOOKUP_FIELDS)? {
        let project = resolve_project(surreal, model, scope, rec.project.as_ref()).await?;
        let fields = crate::estate_assets::EstateAssetFields {
            category: rec.category.trim().to_string(),
            description: rec.description.trim().to_string(),
            passes_by: rec.passes_by.trim().to_string(),
            funding_state: rec.funding_state.trim().to_string(),
            note: crate::matter_record_links::optional_text(rec.note.as_deref()),
        };
        let key = format!(
            "{} / {} / {}",
            project.code, fields.category, fields.description
        );
        let existing = crate::estate_assets::find_by_line(
            surreal,
            project.id,
            &fields.category,
            &fields.description,
        )
        .await?;
        let plan = match &existing {
            None => Plan::New,
            Some(row) => plan_existing(
                overwrite,
                differing([
                    ("passes_by", row.passes_by == fields.passes_by),
                    ("funding_state", row.funding_state == fields.funding_state),
                    ("note", row.note == fields.note),
                ]),
            ),
        };
        if !actor.dry_run {
            match (&plan, &existing) {
                (Plan::New, _) => {
                    crate::estate_assets::insert(surreal, project.id, &fields).await?;
                }
                (Plan::Changed(_), Some(row)) => {
                    crate::estate_assets::update(surreal, project.id, row.id, &fields).await?;
                }
                _ => {}
            }
        }
        record_plan(&mut report, key, &plan);
    }
    Ok(report)
}

pub(super) async fn reconcile_cap_table_holdings(
    surreal: &SurrealDb,
    yaml: &str,
    overwrite: bool,
    actor: &ReconcileActor<'_>,
    scope: Option<&Project>,
) -> anyhow::Result<ReconcileReport> {
    let model = SeedModel::CapTableHolding;
    let mut report = ReconcileReport {
        model: model.term().to_string(),
        ..ReconcileReport::default()
    };
    for rec in parse_seed::<CapTableHoldingRec>(yaml, model, CAP_TABLE_HOLDING_LOOKUP_FIELDS)? {
        let project = resolve_project(surreal, model, scope, rec.project.as_ref()).await?;
        let holder_key = party_key(model, rec.person.as_ref(), rec.entity.as_ref())?;
        let holder =
            resolve_party(surreal, model, rec.person.as_ref(), rec.entity.as_ref()).await?;
        let fields = crate::cap_table_holdings::CapTableHoldingFields {
            holder,
            share_class: rec.share_class.trim().to_string(),
            shares: rec.shares,
            vesting_note: crate::matter_record_links::optional_text(rec.vesting_note.as_deref()),
        };
        let key = format!("{} / {holder_key} / {}", project.code, fields.share_class);
        let existing = crate::cap_table_holdings::find_by_line(
            surreal,
            project.id,
            holder,
            &fields.share_class,
        )
        .await?;
        let plan = match &existing {
            None => Plan::New,
            Some(row) => plan_existing(
                overwrite,
                differing([
                    ("shares", row.shares == fields.shares),
                    ("vesting_note", row.vesting_note == fields.vesting_note),
                ]),
            ),
        };
        if !actor.dry_run {
            match (&plan, &existing) {
                (Plan::New, _) => {
                    crate::cap_table_holdings::insert(surreal, project.id, &fields).await?;
                }
                (Plan::Changed(_), Some(row)) => {
                    crate::cap_table_holdings::update(surreal, project.id, row.id, &fields).await?;
                }
                _ => {}
            }
        }
        record_plan(&mut report, key, &plan);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::super::{reconcile_yaml, validate_yaml};
    use super::*;
    use crate::persons::{NewPerson, Role};
    use crate::test_support::{mem_surreal, seed_entity};

    fn actor(project_scope: Option<&str>) -> ReconcileActor<'_> {
        ReconcileActor {
            bootstrap_owner_email: None,
            actor_role: if project_scope.is_some() {
                Role::Lawyer
            } else {
                Role::Owner
            },
            may_change_roles: project_scope.is_none(),
            project_scope,
            dry_run: false,
            actor_person_id: None,
        }
    }

    async fn project(surreal: &SurrealDb, code: &str) -> Project {
        crate::projects::create(
            surreal,
            &crate::projects::NewProject {
                code: code.into(),
                name: "Sample matter".into(),
                status: "open".into(),
                entity_id: seed_entity(surreal).await,
                ..Default::default()
            },
        )
        .await
        .unwrap()
    }

    async fn person(surreal: &SurrealDb, email: &str) -> uuid::Uuid {
        crate::persons::create(
            surreal,
            &NewPerson::with_role("Sample Person", email, Role::Client),
        )
        .await
        .unwrap()
        .id
    }

    async fn named_entity(surreal: &SurrealDb, name: &str) -> uuid::Uuid {
        crate::entities::create(
            surreal,
            &crate::entities::NewEntity {
                name: name.into(),
                entity_type_id: crate::test_support::SEED_ENTITY_TYPE_ID,
                jurisdiction_id: crate::test_support::SEED_ENTITY_JURISDICTION_ID,
                phone: None,
                url: None,
                xero_id: None,
                firm_anchor_key: None,
            },
        )
        .await
        .unwrap()
        .id
    }

    async fn import(
        surreal: &SurrealDb,
        model: SeedModel,
        yaml: &str,
        overwrite: bool,
        scope: Option<&str>,
    ) -> anyhow::Result<ReconcileReport> {
        reconcile_yaml(surreal, model, yaml, "Firm", overwrite, &actor(scope)).await
    }

    fn counts(report: &ReconcileReport) -> (usize, usize, usize) {
        (report.created, report.updated, report.unchanged)
    }

    const FIDUCIARY_ROLES: &str = "\
lookup_fields:
  - project_id
  - role
  - successor_order
records:
  - project: { code: sample-estate }
    role: trustee
    successor_order: 1
    person: { email: grantor@example.com }
  - project: { code: sample-estate }
    role: successor_trustee
    successor_order: 1
    entity: { name: Sample Trust Company }
";

    #[tokio::test]
    async fn fiduciary_roles_import_then_reimport_unchanged() {
        let surreal = mem_surreal().await;
        let estate = project(&surreal, "sample-estate").await;
        let grantor = person(&surreal, "grantor@example.com").await;
        let trust_company = named_entity(&surreal, "Sample Trust Company").await;

        let first = import(
            &surreal,
            SeedModel::FiduciaryRole,
            FIDUCIARY_ROLES,
            false,
            None,
        )
        .await
        .unwrap();
        assert_eq!(counts(&first), (2, 0, 0));
        let roster = crate::fiduciary_roles::list(&surreal, estate.id)
            .await
            .unwrap();
        assert_eq!(roster.len(), 2);
        assert_eq!(roster[0].fiduciary, Party::person(grantor));
        assert_eq!(roster[1].fiduciary, Party::entity(trust_company));

        let again = import(
            &surreal,
            SeedModel::FiduciaryRole,
            FIDUCIARY_ROLES,
            false,
            None,
        )
        .await
        .unwrap();
        assert_eq!(counts(&again), (0, 0, 2));
        assert_eq!(
            crate::fiduciary_roles::list(&surreal, estate.id)
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn a_fiduciary_role_moves_to_a_new_person_only_with_overwrite() {
        let surreal = mem_surreal().await;
        let estate = project(&surreal, "sample-estate").await;
        person(&surreal, "first@example.com").await;
        let second = person(&surreal, "second@example.com").await;
        let seed = |email: &str| {
            format!(
                "lookup_fields: [project_id, role, successor_order]\nrecords:\n  - project: {{ code: sample-estate }}\n    role: executor\n    successor_order: 1\n    person: {{ email: {email} }}\n"
            )
        };
        let first = seed("first@example.com");
        let second_yaml = seed("second@example.com");
        import(&surreal, SeedModel::FiduciaryRole, &first, false, None)
            .await
            .unwrap();
        let kept = import(
            &surreal,
            SeedModel::FiduciaryRole,
            &second_yaml,
            false,
            None,
        )
        .await
        .unwrap();
        assert_eq!(counts(&kept), (0, 0, 1));
        let moved = import(&surreal, SeedModel::FiduciaryRole, &second_yaml, true, None)
            .await
            .unwrap();
        assert_eq!(counts(&moved), (0, 1, 0));
        assert_eq!(
            moved.records[0].changed_fields,
            vec!["fiduciary".to_string()]
        );
        let roster = crate::fiduciary_roles::list(&surreal, estate.id)
            .await
            .unwrap();
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].fiduciary, Party::person(second));
    }

    const ESTATE_ASSETS: &str = "\
lookup_fields:
  - project_id
  - category
  - description
records:
  - category: real_property
    description: Primary residence
    passes_by: trust
    funding_state: in_progress
    note: Deed drafted
  - category: account
    description: Retirement account
    passes_by: beneficiary_designation
    funding_state: consent_needed
";

    #[tokio::test]
    async fn estate_assets_import_under_scope_then_reimport_unchanged() {
        let surreal = mem_surreal().await;
        let estate = project(&surreal, "sample-estate").await;
        let scope = Some("sample-estate");
        let first = import(
            &surreal,
            SeedModel::EstateAsset,
            ESTATE_ASSETS,
            false,
            scope,
        )
        .await
        .unwrap();
        assert_eq!(counts(&first), (2, 0, 0));
        let schedule = crate::estate_assets::list(&surreal, estate.id)
            .await
            .unwrap();
        assert_eq!(schedule.len(), 2);
        assert_eq!(schedule[1].note.as_deref(), Some("Deed drafted"));

        let again = import(
            &surreal,
            SeedModel::EstateAsset,
            ESTATE_ASSETS,
            false,
            scope,
        )
        .await
        .unwrap();
        assert_eq!(counts(&again), (0, 0, 2));

        let funded = ESTATE_ASSETS.replace("funding_state: in_progress", "funding_state: funded");
        let changed = import(&surreal, SeedModel::EstateAsset, &funded, true, scope)
            .await
            .unwrap();
        assert_eq!(counts(&changed), (0, 1, 1));
        let schedule = crate::estate_assets::list(&surreal, estate.id)
            .await
            .unwrap();
        assert_eq!(schedule[1].funding_state, "funded");
    }

    #[tokio::test]
    async fn a_scoped_record_naming_another_project_is_refused() {
        let surreal = mem_surreal().await;
        project(&surreal, "sample-estate").await;
        let other = project(&surreal, "sample-transactional").await;
        let yaml = "lookup_fields: [project_id, category, description]\nrecords:\n  - project: { code: sample-transactional }\n    category: account\n    description: Checking\n    passes_by: will\n    funding_state: not_funded\n";
        let error = import(
            &surreal,
            SeedModel::EstateAsset,
            yaml,
            false,
            Some("sample-estate"),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<ScopeViolation>(),
            Some(ScopeViolation::CrossProject)
        ));
        assert!(crate::estate_assets::list(&surreal, other.id)
            .await
            .unwrap()
            .is_empty());
    }

    const CAP_TABLE: &str = "\
lookup_fields:
  - project_id
  - holder_id
  - share_class
records:
  - project: { code: sample-transactional }
    person: { email: founder@example.com }
    share_class: Common
    shares: 6000000
  - project: { code: sample-transactional }
    person: { email: employee@example.com }
    share_class: Common
    shares: 250000
    vesting_note: Four years, one-year cliff
";

    #[tokio::test]
    async fn cap_table_holdings_import_then_reimport_unchanged() {
        let surreal = mem_surreal().await;
        let company = project(&surreal, "sample-transactional").await;
        let founder = person(&surreal, "founder@example.com").await;
        person(&surreal, "employee@example.com").await;

        let first = import(&surreal, SeedModel::CapTableHolding, CAP_TABLE, false, None)
            .await
            .unwrap();
        assert_eq!(counts(&first), (2, 0, 0));
        let table = crate::cap_table_holdings::list(&surreal, company.id)
            .await
            .unwrap();
        assert_eq!(table.len(), 2);
        assert_eq!(table[0].holder, Party::person(founder));
        assert_eq!(table[0].shares, 6_000_000);

        let again = import(&surreal, SeedModel::CapTableHolding, CAP_TABLE, false, None)
            .await
            .unwrap();
        assert_eq!(counts(&again), (0, 0, 2));
    }

    #[tokio::test]
    async fn a_dry_run_plans_without_writing() {
        let surreal = mem_surreal().await;
        let company = project(&surreal, "sample-transactional").await;
        person(&surreal, "founder@example.com").await;
        person(&surreal, "employee@example.com").await;
        let report = reconcile_yaml(
            &surreal,
            SeedModel::CapTableHolding,
            CAP_TABLE,
            "Firm",
            false,
            &ReconcileActor {
                dry_run: true,
                ..actor(None)
            },
        )
        .await
        .unwrap();
        assert_eq!(counts(&report), (2, 0, 0));
        assert!(crate::cap_table_holdings::list(&surreal, company.id)
            .await
            .unwrap()
            .is_empty());
    }

    #[test]
    fn validation_refuses_a_malformed_document() {
        let wrong_lookup = "lookup_fields: [project_id]\nrecords: []\n";
        assert!(validate_yaml(SeedModel::FiduciaryRole, wrong_lookup).is_err());
        let both = "lookup_fields: [project_id, holder_id, share_class]\nrecords:\n  - person: { email: a@example.com }\n    entity: { name: Sample Co }\n    share_class: Common\n    shares: 1\n";
        assert!(validate_yaml(SeedModel::CapTableHolding, both).is_err());
        let unknown_role = FIDUCIARY_ROLES.replace("role: trustee", "role: beneficiary");
        assert!(validate_yaml(SeedModel::FiduciaryRole, &unknown_role).is_err());
        let unknown_state = ESTATE_ASSETS.replace("consent_needed", "pending");
        assert!(validate_yaml(SeedModel::EstateAsset, &unknown_state).is_err());
        let duplicate = format!(
            "{CAP_TABLE}  - project: {{ code: sample-transactional }}\n    person: {{ email: founder@example.com }}\n    share_class: Common\n    shares: 1\n"
        );
        assert!(validate_yaml(SeedModel::CapTableHolding, &duplicate).is_err());
        for (model, yaml) in [
            (SeedModel::FiduciaryRole, FIDUCIARY_ROLES),
            (SeedModel::EstateAsset, ESTATE_ASSETS),
            (SeedModel::CapTableHolding, CAP_TABLE),
        ] {
            validate_yaml(model, yaml).unwrap();
        }
    }

    #[test]
    fn the_models_parse_from_their_seed_file_stems() {
        for (stem, model) in [
            ("FiduciaryRole", SeedModel::FiduciaryRole),
            ("EstateAsset", SeedModel::EstateAsset),
            ("CapTableHolding", SeedModel::CapTableHolding),
        ] {
            assert_eq!(SeedModel::parse(stem).unwrap(), model);
            assert_eq!(SeedModel::parse(model.term()).unwrap(), model);
        }
    }
}
