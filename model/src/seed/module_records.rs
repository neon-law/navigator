//! The matter-record seed models: Fiduciary Roles, Estate Assets, and Cap
//! Table Holdings.

use serde::Deserialize;

use super::{
    parse_seed, require_unique, OperatorEntityNameRef, OperatorPersonRef, OperatorProjectRef,
    SeedModel,
};

pub const FIDUCIARY_ROLE_LOOKUP_FIELDS: &[&str] = &["project_id", "role", "successor_order"];

pub const ESTATE_ASSET_LOOKUP_FIELDS: &[&str] = &["project_id", "category", "description"];

pub const CAP_TABLE_HOLDING_LOOKUP_FIELDS: &[&str] = &["project_id", "holder_id", "share_class"];

/// One Fiduciary Role record: the slot, and exactly one of `person` or
/// `entity` holding it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FiduciaryRoleRec {
    #[serde(default)]
    pub project: Option<OperatorProjectRef>,
    pub role: String,
    pub successor_order: i64,
    #[serde(default)]
    pub person: Option<OperatorPersonRef>,
    #[serde(default)]
    pub entity: Option<OperatorEntityNameRef>,
}

/// One Estate Asset record: a line of the schedule.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EstateAssetRec {
    #[serde(default)]
    pub project: Option<OperatorProjectRef>,
    pub category: String,
    pub description: String,
    pub passes_by: String,
    pub funding_state: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// One Cap Table Holding record: exactly one of `person` or `entity`, a
/// share class, and a whole number of shares.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapTableHoldingRec {
    #[serde(default)]
    pub project: Option<OperatorProjectRef>,
    #[serde(default)]
    pub person: Option<OperatorPersonRef>,
    #[serde(default)]
    pub entity: Option<OperatorEntityNameRef>,
    pub share_class: String,
    pub shares: i64,
    #[serde(default)]
    pub vesting_note: Option<String>,
}

/// The natural-key half naming a party, or why the record is malformed.
pub fn party_key(
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

#[must_use]
pub fn project_key(project: Option<&OperatorProjectRef>) -> String {
    project
        .map(|project| project.code.trim().to_lowercase())
        .unwrap_or_default()
}

pub fn validate_fiduciary_roles(yaml: &str) -> anyhow::Result<()> {
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

pub fn validate_estate_assets(yaml: &str) -> anyhow::Result<()> {
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

pub fn validate_cap_table_holdings(yaml: &str) -> anyhow::Result<()> {
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
