//! Seed documents: the models a `seeds/*.yaml` file can name, and the parser
//! that validates one without a deployment.

pub mod module_records;

use serde::{Deserialize, Serialize};

/// The firm Entity that anchors the canonical seed. `Entity.yaml` re-creates
/// this row by exact name on every boot, so every deployment carries it.
/// `web` reads this to keep the row's delete and rename guards aligned with
/// the name the seed looks up.
///
/// This is the professional corporation a client engages — the entity of record behind
/// the Neon Law mark, which is why it is the row the application refuses to
/// delete. `Neon Law` is what the site is signed with; `Guieb & Shook PC` is the
/// legal person that renders the services and owns the mark, and only a legal
/// person can anchor a client relationship. It is also the copyright holder in
/// Navigator and the Licensor named in `LICENSE`. Moving this name is a data
/// change as well as a code one: `seed_entities` reconciles
/// `entities.firm_anchor_key` on every boot, because the delete guard reads
/// that column and not the name.
pub const FIRM_ENTITY_NAME: &str = "Guieb & Shook PC";

/// The glossary-backed Surreal tables that may be reconciled from a seed
/// document by an authenticated operator. This is deliberately a typed,
/// closed registry rather than a table-name escape hatch: each model keeps
/// its store invariants (mailbox claims, reference resolution, and firm-anchor
/// protection) on the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeedModel {
    Person,
    Entity,
    PersonProjectRole,
    PersonEntityRole,
    /// A person's postal address (LAW-57).
    Address,
    /// A Relationship Log row about a Person — the one-sided entry the
    /// Conflict-Check Graph may later parse into Relationship Edges.
    RelationshipLog,
    /// A Project Reference: a counterparty Person or Entity the Project
    /// names without granting any standing on the matter. The one model
    /// through which a scoped session may create an Entity, and only as
    /// the referenced party.
    ProjectReference,
    /// A Fiduciary Role an estate plan names, in successor order.
    FiduciaryRole,
    /// One line of an estate plan's asset schedule.
    EstateAsset,
    /// One holder's shares of one class on a Project's cap table.
    CapTableHolding,
}

impl SeedModel {
    /// Every model a CI-minted seed session may reconcile.
    pub const ALL: [Self; 10] = [
        Self::Person,
        Self::Entity,
        Self::PersonProjectRole,
        Self::PersonEntityRole,
        Self::Address,
        Self::RelationshipLog,
        Self::ProjectReference,
        Self::FiduciaryRole,
        Self::EstateAsset,
        Self::CapTableHolding,
    ];

    /// Resolve the singular glossary term, Surreal table name, or `PascalCase`
    /// `seeds/<Model>.yaml` stem supplied by `navigator site import`.
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match glossary_token(value).as_str() {
            "person" => Ok(Self::Person),
            "entity" => Ok(Self::Entity),
            "person_project_role" => Ok(Self::PersonProjectRole),
            "person_entity_role" => Ok(Self::PersonEntityRole),
            "address" => Ok(Self::Address),
            "relationship_log" => Ok(Self::RelationshipLog),
            "project_reference" => Ok(Self::ProjectReference),
            "fiduciary_role" => Ok(Self::FiduciaryRole),
            "estate_asset" => Ok(Self::EstateAsset),
            "cap_table_holding" => Ok(Self::CapTableHolding),
            _ => anyhow::bail!(
                "unsupported seed model `{value}`; supported glossary terms: person, entity, person_project_role, person_entity_role, address, relationship_log, project_reference, fiduciary_role, estate_asset, cap_table_holding"
            ),
        }
    }

    #[must_use]
    pub const fn term(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Entity => "entity",
            Self::PersonProjectRole => "person_project_role",
            Self::PersonEntityRole => "person_entity_role",
            Self::Address => "address",
            Self::RelationshipLog => "relationship_log",
            Self::ProjectReference => "project_reference",
            Self::FiduciaryRole => "fiduciary_role",
            Self::EstateAsset => "estate_asset",
            Self::CapTableHolding => "cap_table_holding",
        }
    }
}

/// Validate a seed document without connecting to a deployment.
///
/// This is the same parser the authenticated reconciliation path uses, so a
/// document accepted by `navigator project gate` cannot fail later because the
/// two paths disagree about its shape.
pub fn validate_yaml(model: SeedModel, yaml: &str) -> anyhow::Result<()> {
    match model {
        SeedModel::Person => {
            let records = parse_seed::<PersonRec>(yaml, model, &["email"])?;
            require_unique(
                model,
                "email",
                records
                    .iter()
                    .map(|record| record.email.to_ascii_lowercase()),
            )
        }
        SeedModel::Entity => {
            let records =
                parse_seed::<OperatorEntityRec>(yaml, model, &["name", "entity_type_id"])?;
            for record in &records {
                crate::entity_commands::validate_metadata(record.metadata.as_ref())
                    .map_err(|message| anyhow::anyhow!(message))?;
            }
            require_unique(
                model,
                "name and entity_type.name",
                records.iter().map(|record| {
                    format!(
                        "{}\u{0}{}",
                        record.name.to_ascii_lowercase(),
                        record.entity_type.name.to_ascii_lowercase()
                    )
                }),
            )
        }
        SeedModel::PersonProjectRole => {
            let records = parse_seed::<OperatorPersonProjectRoleRec>(
                yaml,
                model,
                &["person_id", "project_id"],
            )?;
            require_unique(
                model,
                "person.email and project.code",
                records.iter().map(|record| {
                    format!(
                        "{}\u{0}{}",
                        record.person.email.to_ascii_lowercase(),
                        record.project.code.to_ascii_lowercase()
                    )
                }),
            )
        }
        SeedModel::PersonEntityRole => {
            let records = parse_seed::<OperatorPersonEntityRoleRec>(
                yaml,
                model,
                &["person_id", "entity_id"],
            )?;
            require_unique(
                model,
                "person.email, entity.name, and role",
                records.iter().map(|record| {
                    format!(
                        "{}\u{0}{}\u{0}{}",
                        record.person.email.to_ascii_lowercase(),
                        record.entity.name.to_ascii_lowercase(),
                        record.role.to_ascii_lowercase()
                    )
                }),
            )
        }
        SeedModel::Address => parse_address_seed(yaml).map(|_| ()),
        SeedModel::RelationshipLog => validate_relationship_logs(yaml),
        SeedModel::ProjectReference => validate_project_references(yaml),
        SeedModel::FiduciaryRole => module_records::validate_fiduciary_roles(yaml),
        SeedModel::EstateAsset => module_records::validate_estate_assets(yaml),
        SeedModel::CapTableHolding => module_records::validate_cap_table_holdings(yaml),
    }
}

pub fn validate_project_references(yaml: &str) -> anyhow::Result<()> {
    let model = SeedModel::ProjectReference;
    let records =
        parse_seed::<OperatorProjectReferenceRec>(yaml, model, PROJECT_REFERENCE_LOOKUP_FIELDS)?;
    let mut keys = Vec::with_capacity(records.len());
    for record in &records {
        keys.push(format!(
            "{}\u{0}{}",
            record.party_key()?.to_ascii_lowercase(),
            record.reason.trim().to_ascii_lowercase()
        ));
    }
    require_unique(model, "party and reason", keys)
}

pub fn validate_relationship_logs(yaml: &str) -> anyhow::Result<()> {
    let model = SeedModel::RelationshipLog;
    let records =
        parse_seed::<OperatorRelationshipLogRec>(yaml, model, &["subject_id", "action", "detail"])?;
    for record in &records {
        if record.action.trim().is_empty() || record.detail.trim().is_empty() {
            anyhow::bail!("relationship_log seed records need a non-empty action and detail");
        }
    }
    require_unique(
        model,
        "subject.email, action, and detail",
        records.iter().map(|record| {
            format!(
                "{}\u{0}{}\u{0}{}",
                record.subject.email.to_ascii_lowercase(),
                record.action,
                record.detail
            )
        }),
    )
}

pub fn require_unique(
    model: SeedModel,
    field: &str,
    values: impl IntoIterator<Item = String>,
) -> anyhow::Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            anyhow::bail!("{} seed contains duplicate {field}", model.term());
        }
    }
    Ok(())
}

pub fn parse_seed<T>(
    yaml: &str,
    model: SeedModel,
    expected_lookup_fields: &[&str],
) -> anyhow::Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    let parsed: SeedRecords<T> = serde_yaml::from_str(yaml)
        .map_err(|error| anyhow::anyhow!("parse {} seed: {error}", model.term()))?;
    let supplied: Vec<&str> = parsed.lookup_fields.iter().map(String::as_str).collect();
    if supplied != expected_lookup_fields {
        anyhow::bail!(
            "{} seed must declare lookup_fields: {}",
            model.term(),
            expected_lookup_fields.join(", ")
        );
    }
    Ok(parsed.records)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorEntityRec {
    pub name: String,
    pub entity_type: EntityTypeRef,
    pub phone: Option<String>,
    pub url: Option<String>,
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorPersonProjectRoleRec {
    pub person: OperatorPersonRef,
    pub project: OperatorProjectRef,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorPersonEntityRoleRec {
    pub person: OperatorPersonRef,
    pub entity: OperatorEntityNameRef,
    pub role: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorAddressRec {
    pub person: Option<OperatorPersonRef>,
    pub entity: Option<AddressEntityRef>,
    #[serde(alias = "street")]
    pub line1: String,
    pub line2: Option<String>,
    pub city: String,
    #[serde(alias = "state")]
    pub region: String,
    #[serde(alias = "zip")]
    pub postal_code: String,
    pub country: String,
}

pub fn parse_address_seed(yaml: &str) -> anyhow::Result<Vec<OperatorAddressRec>> {
    let parsed: SeedRecords<OperatorAddressRec> = serde_yaml::from_str(yaml)
        .map_err(|error| anyhow::anyhow!("parse address seed: {error}"))?;
    let fields: Vec<&str> = parsed.lookup_fields.iter().map(String::as_str).collect();
    let by_entity = match fields.as_slice() {
        ["person_id", "line1", "postal_code"] => false,
        ["entity_id", "street", "zip"] => true,
        _ => anyhow::bail!(
            "address seed must declare lookup_fields: person_id, line1, postal_code or entity_id, street, zip"
        ),
    };
    let mut keys = Vec::new();
    for record in &parsed.records {
        let owner = match (&record.person, &record.entity, by_entity) {
            (Some(person), None, false) => person.email.to_ascii_lowercase(),
            (None, Some(entity), true) => format!(
                "{}\u{0}{}",
                entity.name.trim().to_ascii_lowercase(),
                entity
                    .entity_type
                    .as_ref()
                    .map_or("", |kind| kind.name.as_str())
                    .to_ascii_lowercase()
            ),
            _ => {
                anyhow::bail!("address seed must name exactly the owner declared by lookup_fields")
            }
        };
        keys.push(format!(
            "{}\u{0}{}\u{0}{}",
            owner,
            record.line1.to_ascii_lowercase(),
            record.postal_code.to_ascii_lowercase()
        ));
    }
    require_unique(SeedModel::Address, "owner, street, and postal code", keys)?;
    Ok(parsed.records)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonRec {
    pub email: String,
    pub name: String,
    #[serde(default)]
    pub profile_image_url: Option<String>,
    /// The address this Person used before `email`. A Person who answers to
    /// it is updated to `email`, not duplicated (LAW-169).
    #[serde(default)]
    pub previous_email: Option<String>,
    /// Further addresses the same Person answers to, kept as aliases.
    #[serde(default)]
    pub emails: Vec<String>,
    /// The Person's phone number — the primary mobile. The short form
    /// of `phones`; a record may name it or the list or both.
    #[serde(default)]
    pub phone: Option<String>,
    /// Further phone numbers the Person answers to, mirroring `emails`:
    /// an E.164 number with a label and a primary flag.
    #[serde(default)]
    pub phones: Vec<PhoneRec>,
    /// The Person's system-wide tier. Omitted means `client` on a create and
    /// leaves an existing Person's tier alone; see [`check_seed_roles`].
    #[serde(default)]
    pub role: Option<crate::persons::Role>,
}

impl PersonRec {
    /// Every address that may already belong to this Person, `email` first.
    pub fn candidate_emails(&self) -> Vec<&str> {
        std::iter::once(self.email.as_str())
            .chain(self.previous_email.as_deref())
            .chain(self.emails.iter().map(String::as_str))
            .collect()
    }

    /// Addresses this record wants kept as aliases beside `primary`, lowercased.
    pub fn wanted_aliases(&self, primary: &str, old_primary: Option<&str>) -> Vec<String> {
        let primary = primary.trim().to_lowercase();
        let mut wanted: Vec<String> = self
            .previous_email
            .iter()
            .chain(self.emails.iter())
            .map(String::as_str)
            .chain(old_primary)
            .map(|email| email.trim().to_lowercase())
            .filter(|email| *email != primary)
            .collect();
        wanted.sort();
        wanted.dedup();
        wanted
    }

    /// Every phone number the record wants on the Person, normalized to
    /// E.164. The `phone` shorthand is the primary mobile; the `phones`
    /// list carries its own label and primary flag, and a number named
    /// twice — shorthand and list — is written once, first spelling wins.
    /// A number that will not normalize is refused here, naming the
    /// expected format, before any write happens — even on a dry run.
    pub fn wanted_phones(&self) -> anyhow::Result<Vec<SeedPhone>> {
        fn normalize(raw: &str) -> anyhow::Result<String> {
            crate::person_phones::normalize_e164(raw).map_err(|_| {
                anyhow::anyhow!(
                    "phone {raw:?}: expected an E.164 number — a leading `+` \
                     and 8 to 15 digits, like `+17025550123`"
                )
            })
        }
        let mut wanted: Vec<SeedPhone> = Vec::new();
        let mut push = |raw: &str, label, primary| -> anyhow::Result<()> {
            let e164 = normalize(raw)?;
            if !wanted.iter().any(|known| known.e164 == e164) {
                wanted.push(SeedPhone {
                    e164,
                    label,
                    primary,
                });
            }
            Ok(())
        };
        if let Some(raw) = self
            .phone
            .as_deref()
            .map(str::trim)
            .filter(|raw| !raw.is_empty())
        {
            push(raw, crate::person_phones::PhoneLabel::Mobile, true)?;
        }
        for phone in &self.phones {
            push(phone.number.trim(), phone.label, phone.primary)?;
        }
        Ok(wanted)
    }
}

/// The strict envelope accepted by `navigator site import`.
///
/// Canonical seed files use [`Records`] because they carry several different
/// shapes. Operator-supplied seed documents have one closed contract, so the
/// parser rejects missing or unknown envelope keys before a write can start.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeedRecords<T> {
    pub lookup_fields: Vec<String>,
    pub records: Vec<T>,
}

/// Lowercase `snake_case` for a glossary term or a `PascalCase` seed filename stem.
#[must_use]
pub fn glossary_token(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.contains('_')
        || trimmed.contains('-')
        || trimmed.chars().all(|character| !character.is_uppercase())
    {
        return trimmed.replace('-', "_").to_ascii_lowercase();
    }
    let mut out = String::new();
    for (index, character) in trimmed.chars().enumerate() {
        if character.is_uppercase() && index > 0 {
            out.push('_');
        }
        out.push(character.to_ascii_lowercase());
    }
    out
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorPersonRef {
    pub email: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorProjectRef {
    pub code: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorEntityNameRef {
    pub name: String,
}

/// One Relationship Log row about a Person. The natural key is the whole
/// row — `(subject.email, action, detail)` — so re-importing the same
/// filing's service list appends nothing. `project` names the Project whose
/// work produced the row; it scopes the row and grants no role.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorRelationshipLogRec {
    pub subject: OperatorPersonRef,
    pub action: String,
    pub detail: String,
    #[serde(default)]
    pub project: Option<OperatorProjectRef>,
}

/// The `lookup_fields` a `project_reference` document declares: the natural
/// key is the Project, the party, and the reason, and the party half is the
/// nested `person.email` or `entity.name` the record carries.
pub const PROJECT_REFERENCE_LOOKUP_FIELDS: &[&str] = &["project_id", "reason"];

/// One Project Reference: exactly one of `person` or `entity`, the reason
/// the Project names them, and — for an unscoped session — the Project.
/// A scoped session always references from its own Project and refuses a
/// record naming another.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorProjectReferenceRec {
    pub reason: String,
    #[serde(default)]
    pub person: Option<OperatorReferencedPersonRef>,
    #[serde(default)]
    pub entity: Option<EntityRec>,
    #[serde(default)]
    pub project: Option<OperatorProjectRef>,
}

impl OperatorProjectReferenceRec {
    /// The natural-key half naming the party, or why the record is
    /// malformed: it must name exactly one of `person` or `entity`, and a
    /// reason the schema admits.
    pub fn party_key(&self) -> anyhow::Result<String> {
        if !crate::project_references::is_reason(self.reason.trim()) {
            anyhow::bail!(
                "project_reference seed reason {:?} is not one of {}",
                self.reason,
                crate::project_references::REASONS.join(", ")
            );
        }
        match (&self.person, &self.entity) {
            (Some(person), None) => Ok(format!("person:{}", person.email.trim())),
            (None, Some(entity)) => Ok(format!(
                "entity:{} / {}",
                entity.name.trim(),
                entity.entity_type.name.trim()
            )),
            (Some(_), Some(_)) => {
                anyhow::bail!("project_reference seed record names both a person and an entity")
            }
            (None, None) => {
                anyhow::bail!("project_reference seed record names neither a person nor an entity")
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddressEntityRef {
    pub name: String,
    pub entity_type: Option<EntityTypeRef>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityTypeRef {
    pub name: String,
    #[serde(default)]
    pub jurisdiction: Option<JurisdictionRef>,
}

/// One entry of a Person record's `phones` list: the number, the kind of
/// line, and whether it is the number a callback uses first.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhoneRec {
    pub number: String,
    #[serde(default)]
    pub label: crate::person_phones::PhoneLabel,
    #[serde(default)]
    pub primary: bool,
}

/// One phone number a seed record wants on a Person, normalized to the
/// stored spelling. The write shape [`record_phones`] applies.
#[derive(Clone, Debug)]
pub struct SeedPhone {
    pub e164: String,
    pub label: crate::person_phones::PhoneLabel,
    pub primary: bool,
}

/// A Person a Project references without granting participation. `name` is
/// what a first reference creates the row with; an existing row keeps its
/// own name.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorReferencedPersonRef {
    pub email: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityRec {
    pub name: String,
    pub entity_type: EntityTypeRef,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JurisdictionRef {
    pub name: String,
}
