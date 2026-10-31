//! Canonical seed loader: insert the workspace-bundled YAML fixtures
//! (`store/seeds/*.yaml`) into every entity table the schema knows
//! about. Re-running is a no-op on the natural keys of each table.
//!
//! The YAML files use a `lookup_fields: + records:` shape (see
//! `store/seeds/`); this loader resolves nested foreign references
//! (e.g., `entity.entity_type.name`) by looking up rows by their
//! natural key.
//!
//! Both binaries in the workspace go through this module:
//! - `navigator list ...` calls [`seed_canonical`] before reading.
//! - a brand binary calls [`seed_environment`] after migrations on startup,
//!   naming the brand it serves so that brand's own seeds apply too.
//!
//! Seeds come in three layers — canonical, brand, and the sample-matter
//! fixture. [`seed_environment`] documents which reaches a deployment holding
//! real client files and why.

mod module_records;
mod sample_records;

use crate::jurisdictions::{self, NewJurisdiction};
use crate::surreal::SurrealDb;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Per-entity insert counts for one seed pass.
#[derive(Debug, Default, Clone, Copy)]
pub struct SeedReport {
    pub jurisdictions_inserted: usize,
    pub entity_types_inserted: usize,
    pub entities_inserted: usize,
    pub persons_inserted: usize,
    pub persons_updated: usize,
    pub projects_inserted: usize,
    pub notations_inserted: usize,
    pub assets_inserted: usize,
    pub communications_inserted: usize,
    pub git_repositories_inserted: usize,
    pub questions_inserted: usize,
    pub mailrooms_inserted: usize,
    pub addresses_inserted: usize,
    pub letters_inserted: usize,
    pub answers_inserted: usize,
    pub person_entity_roles_inserted: usize,
    pub person_project_roles_inserted: usize,
    pub credentials_inserted: usize,
    pub templates_inserted: usize,
    pub testimonials_inserted: usize,
    pub social_handles_written: usize,
    pub posts_inserted: usize,
    /// Glossary terms materialized from `docs/glossary/`. Reference
    /// data: environment-blind, upserted by slug on every boot.
    pub glossary_terms_written: usize,
}

impl SeedReport {
    /// One-line summary suitable for CLI output. Reports every entity
    /// even when zero so re-runs make it visible that the pass was
    /// a no-op.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "Seeded: {} jurisdictions, {} entity_types, {} entities, {} persons \
             (+{} role updates), {} projects, {} notations, {} assets, {} communications, \
             {} git_repos, {} questions, \
             {} mailrooms, {} addresses, {} letters, {} answers, \
             {} person_entity_roles, {} person_project_roles, {} credentials, \
             {} templates, {} testimonials, {} social_handles, {} posts, {} glossary_terms.",
            self.jurisdictions_inserted,
            self.entity_types_inserted,
            self.entities_inserted,
            self.persons_inserted,
            self.persons_updated,
            self.projects_inserted,
            self.notations_inserted,
            self.assets_inserted,
            self.communications_inserted,
            self.git_repositories_inserted,
            self.questions_inserted,
            self.mailrooms_inserted,
            self.addresses_inserted,
            self.letters_inserted,
            self.answers_inserted,
            self.person_entity_roles_inserted,
            self.person_project_roles_inserted,
            self.credentials_inserted,
            self.templates_inserted,
            self.testimonials_inserted,
            self.social_handles_written,
            self.posts_inserted,
            self.glossary_terms_written,
        )
    }
}

// ---------- Embedded canonical YAMLs ----------
//
// Bundled at compile time so the installed `navigator` binary is
// self-contained — no runtime lookup of `store/seeds/`.

/// The canonical jurisdiction reference data, embedded at compile time.
/// Exposed so cross-crate reconciliation tests (e.g. `cli`) can assert the
/// path vocabulary in `rules::f110` stays in sync with the seeded rows
/// without reaching into `store`'s private modules.
pub const JURISDICTION_SEED_YAML: &str = canonical::JURISDICTION;

/// The canonical Entity seed, embedded at compile time. Exposed so the
/// cross-crate parity test in `cli` can prove the row this seed writes is
/// named exactly [`FIRM_ENTITY_NAME`] and exactly what `views::brand` prints
/// as the footer's legal entity, without reading `store/seeds/` from disk.
pub const ENTITY_SEED_YAML: &str = canonical::ENTITY;

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

/// The second firm Entity the canonical seed ships. `Lex Tecnica LLP`
/// anchors the `Lex` firm, which wears only the `iep-defenders` brand —
/// brand mobility's proof that a brand can belong to a different firm.
/// Its row joins [`FIRM_ENTITY_NAME`]'s anchor-key discipline, so the
/// delete guard protects it identically.
pub const LEX_FIRM_ENTITY_NAME: &str = "Lex Tecnica LLP";
/// The firm row's wordmark, distinct from the legal entity it wears.
pub const LEX_FIRM_NAME: &str = "Lex";

/// Every firm-anchor Entity the canonical seed ships, in seed order.
/// `seed_entities` and `is_firm_anchor` read this list, so a second firm
/// is a one-line extension rather than a second hand-rolled guard.
pub const FIRM_ENTITY_NAMES: &[&str] = &[FIRM_ENTITY_NAME, LEX_FIRM_ENTITY_NAME];

/// The seeded practice's first Admin DRI (ENG-499). `User.yaml` carries the
/// canonical `role: admin` assignment for this email; [`seed_practice`]
/// resolves the row by it, so the two agree on which person opens the
/// practice's Firm without either spelling the convention twice.
const PRIMARY_ADMIN_EMAIL: &str = "nick@neonlaw.com";

/// The environment variable a white-label operator names their own firm
/// Entity with. Blank or unset falls back to [`FIRM_ENTITY_NAME`] everywhere
/// this is read — `portal::admin::bootstrap_company_from_env` and
/// [`crate::firms::anchor_firm`] agree on the one variable name rather than
/// each spelling it again.
pub const BOOTSTRAP_COMPANY_ENV: &str = "NAVIGATOR_BOOTSTRAP_COMPANY";

/// Which brand's own seeds a boot applies.
///
/// This is the third seed layer, and the only one besides the canonical set
/// that reaches production. The canonical layer is what every deployment
/// shares; the sample-matter fixture is disposable; this layer
/// is the data one brand owns and another must not carry. The Firm's postal
/// identities are the founding case: they are real, they belong in production,
/// and they belong in no other deployment's database.
///
/// A brand binary declares its own value in [`hosting::Site`], so adding a
/// binary is a new variant plus its seed directory rather than a branch in
/// this module. Distinct from `views::brand::BrandKey`, which names a
/// *request's* resolved house brand within one binary rather than the binary
/// itself — a single `neon-server` boot applies one `BrandSeed` regardless of
/// how many `BrandKey`s it serves.
///
/// [`hosting::Site`]: https://docs.rs/portal
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrandSeed {
    /// `neonlaw.com` — Neon Law: the firm, serving the whole site from its
    /// root.
    ///
    /// Rows stay keyed to the entity that owns them, so a further entity is a
    /// further record rather than a further variant.
    Neon,
    /// A white-label tenant deployment, which carries none of our own
    /// entities' data. This is a real value rather than an absent one: a
    /// tenant boot must be a deliberate "seed nothing", not a brand someone
    /// forgot to name.
    Tenant,
}

impl BrandSeed {
    /// The brand's short key, matching `hosting::Site::key`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Neon => "neon",
            Self::Tenant => "tenant",
        }
    }

    /// The brand's own `Mailroom.yaml` as `(contents, path)`, or `None` for a
    /// brand that rents no mail facility of its own.
    const fn mailrooms(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Neon => Some((brand_seeds::NEON_MAILROOM, "neon/Mailroom.yaml")),
            Self::Tenant => None,
        }
    }

    /// The handles the brand holds on each social provider, or `None`.
    const fn social_handles(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Neon => Some((brand_seeds::NEON_SOCIAL_HANDLE, "neon/SocialHandle.yaml")),
            Self::Tenant => None,
        }
    }

    /// The Blog Posts the brand has already published, or `None`.
    const fn posts(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Neon => Some((brand_seeds::NEON_POST, "neon/Post.yaml")),
            Self::Tenant => None,
        }
    }

    /// The brand's own `Address.yaml` as `(contents, path)`, embedded at
    /// compile time, or `None` for a brand that owns no addresses of ours.
    /// The path travels with the contents so a parse error names the file a
    /// reader can open rather than the brand that loaded it.
    const fn addresses(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Neon => Some((brand_seeds::NEON_ADDRESS, "neon/Address.yaml")),
            Self::Tenant => None,
        }
    }
}

/// Per-brand seeds, embedded at compile time exactly like the canonical set
/// so an installed binary carries its own brand's data with no runtime
/// lookup of `store/seeds/`.
mod brand_seeds {
    pub const NEON_MAILROOM: &str = include_str!("../seeds/neon/Mailroom.yaml");
    pub const NEON_ADDRESS: &str = include_str!("../seeds/neon/Address.yaml");
    pub const NEON_SOCIAL_HANDLE: &str = include_str!("../seeds/neon/SocialHandle.yaml");
    pub const NEON_POST: &str = include_str!("../seeds/neon/Post.yaml");
}

/// The disposable sample-matter fixture, embedded like the other layers but
/// kept under `store/seeds/sample/` so a reader and the seed-boundary test can
/// tell a synthetic row from a firm fact by its path. Applied on a `dev`
/// profile only.
mod sample_seeds {
    pub const GIT_REPOSITORY: &str = include_str!("../seeds/sample/GitRepository.yaml");
    pub const MAILROOM: &str = include_str!("../seeds/sample/Mailroom.yaml");
    pub const LETTER: &str = include_str!("../seeds/sample/Letter.yaml");
    pub const ANSWER: &str = include_str!("../seeds/sample/Answer.yaml");
    pub const PERSON_PROJECT_ROLE: &str = include_str!("../seeds/sample/PersonProjectRole.yaml");
}

mod canonical {
    pub const JURISDICTION: &str = include_str!("../seeds/Jurisdiction.yaml");
    pub const ENTITY_TYPE: &str = include_str!("../seeds/EntityType.yaml");
    pub const ENTITY: &str = include_str!("../seeds/Entity.yaml");
    pub const PERSON: &str = include_str!("../seeds/Person.yaml");
    pub const USER: &str = include_str!("../seeds/User.yaml");
    pub const QUESTION: &str = include_str!("../seeds/Question.yaml");
    pub const PERSON_ENTITY_ROLE: &str = include_str!("../seeds/PersonEntityRole.yaml");
    pub const CREDENTIAL: &str = include_str!("../seeds/Credential.yaml");
    pub const TESTIMONIAL: &str = include_str!("../seeds/Testimonial.yaml");

    /// Bundled notation templates. Each entry is `(path, full_md)`
    /// where `path` exists only as a label in the seed report.
    /// Adding a template here lets the cluster carry
    /// it without a separate `navigator site seed` step. The full
    /// shipped catalog is bundled so a fresh cluster carries every
    /// template without an import pass.
    pub const TEMPLATE_ONBOARDING_LETTER: &str =
        include_str!("../../templates/notations/neon_law/onboarding__letter.md");
    pub const TEMPLATE_OFFBOARDING_LETTER: &str =
        include_str!("../../templates/notations/neon_law/offboarding__letter.md");
    pub const TEMPLATE_ANNUAL_REPORT_NV: &str = include_str!(
        "../../templates/notations/forms/united_states/nevada/state/nv__annual_report.md"
    );
    pub const TEMPLATE_DISSOLUTION_NV: &str = include_str!(
        "../../templates/notations/forms/united_states/nevada/state/nv__dissolution.md"
    );
    pub const TEMPLATE_FORM990: &str =
        include_str!("../../templates/notations/forms/united_states/federal/irs/us__form_990.md");
    pub const TEMPLATE_NONPROFIT_501C3_NV: &str = include_str!(
        "../../templates/notations/forms/united_states/nevada/state/nv__nonprofit_501c3_formation.md"
    );
    pub const TEMPLATE_CHARITABLE_SOLICITATION_NV: &str = include_str!(
        "../../templates/notations/forms/united_states/nevada/state/nv__charitable_solicitation_registration.md"
    );
    pub const TEMPLATE_NV_MBT: &str = include_str!(
        "../../templates/notations/forms/united_states/nevada/state/nv__modified_business_tax.md"
    );
    pub const TEMPLATE_LLC_NV: &str = include_str!(
        "../../templates/notations/forms/united_states/nevada/state/nv__llc_formation.md"
    );
    pub const TEMPLATE_CORP_NV: &str = include_str!(
        "../../templates/notations/forms/united_states/nevada/state/nv__profit_corp_formation.md"
    );
    pub const TEMPLATE_BUSINESS_TRUST_NV: &str = include_str!(
        "../../templates/notations/forms/united_states/nevada/state/nv__business_trust_formation.md"
    );
    pub const TEMPLATE_NATURALIZATION: &str = include_str!(
        "../../templates/notations/forms/united_states/federal/uscis/us__naturalization.md"
    );
}

/// One bundled notation template that the canonical seed inserts into the
/// shared catalog.
#[derive(Debug, Clone, Copy)]
pub struct SeededTemplate {
    pub label: &'static str,
    pub markdown: &'static str,
}

/// The full bundled notation-template catalog, in seed insertion order.
///
/// This is the canonical list consumed by both the database seeder and
/// cross-crate catalog/spec drift guards. Adding a template here makes it a
/// shared seeded template, so its code must also resolve to a questionnaire
/// through the workflow catalog or through an intentionally carried template
/// body.
pub const SEEDED_TEMPLATES: &[SeededTemplate] = &[
    SeededTemplate {
        label: "notations/neon_law/mutual_nda.md",
        markdown: include_str!("../../templates/notations/neon_law/mutual_nda.md"),
    },
    SeededTemplate {
        label: "notations/neon_law/one_way_nda.md",
        markdown: include_str!("../../templates/notations/neon_law/one_way_nda.md"),
    },
    SeededTemplate {
        label: "notations/neon_law/advisor_agreement.md",
        markdown: include_str!("../../templates/notations/neon_law/advisor_agreement.md"),
    },
    SeededTemplate {
        label: "notations/neon_law/master_services_agreement.md",
        markdown: include_str!("../../templates/notations/neon_law/master_services_agreement.md"),
    },
    SeededTemplate {
        label: "notations/neon_law/employee_offer_letter.md",
        markdown: include_str!("../../templates/notations/neon_law/employee_offer_letter.md"),
    },
    SeededTemplate {
        label: "notations/neon_law/business_associate_agreement.md",
        markdown: include_str!(
            "../../templates/notations/neon_law/business_associate_agreement.md"
        ),
    },
    SeededTemplate {
        label: "notations/neon_law/dpa_us.md",
        markdown: include_str!("../../templates/notations/neon_law/dpa_us.md"),
    },
    SeededTemplate {
        label: "notations/neon_law/dpa_global.md",
        markdown: include_str!("../../templates/notations/neon_law/dpa_global.md"),
    },
    SeededTemplate {
        label: "notations/neon_law/privacy_policy.md",
        markdown: include_str!("../../templates/notations/neon_law/privacy_policy.md"),
    },
    SeededTemplate {
        label: "notations/neon_law/onboarding__letter.md",
        markdown: canonical::TEMPLATE_ONBOARDING_LETTER,
    },
    SeededTemplate {
        label: "notations/neon_law/offboarding__letter.md",
        markdown: canonical::TEMPLATE_OFFBOARDING_LETTER,
    },
    SeededTemplate {
        label: "notations/forms/united_states/nevada/state/nv__annual_report.md",
        markdown: canonical::TEMPLATE_ANNUAL_REPORT_NV,
    },
    SeededTemplate {
        label: "notations/forms/united_states/nevada/state/nv__dissolution.md",
        markdown: canonical::TEMPLATE_DISSOLUTION_NV,
    },
    SeededTemplate {
        label: "notations/forms/united_states/federal/irs/us__form_990.md",
        markdown: canonical::TEMPLATE_FORM990,
    },
    SeededTemplate {
        label: "notations/forms/united_states/nevada/state/nv__nonprofit_501c3_formation.md",
        markdown: canonical::TEMPLATE_NONPROFIT_501C3_NV,
    },
    SeededTemplate {
        label:
            "notations/forms/united_states/nevada/state/nv__charitable_solicitation_registration.md",
        markdown: canonical::TEMPLATE_CHARITABLE_SOLICITATION_NV,
    },
    SeededTemplate {
        label: "notations/forms/united_states/nevada/state/nv__modified_business_tax.md",
        markdown: canonical::TEMPLATE_NV_MBT,
    },
    SeededTemplate {
        label: "notations/forms/united_states/nevada/state/nv__llc_formation.md",
        markdown: canonical::TEMPLATE_LLC_NV,
    },
    SeededTemplate {
        label: "notations/forms/united_states/nevada/state/nv__profit_corp_formation.md",
        markdown: canonical::TEMPLATE_CORP_NV,
    },
    SeededTemplate {
        label: "notations/forms/united_states/nevada/state/nv__business_trust_formation.md",
        markdown: canonical::TEMPLATE_BUSINESS_TRUST_NV,
    },
    SeededTemplate {
        label: "notations/forms/united_states/federal/uscis/us__naturalization.md",
        markdown: canonical::TEMPLATE_NATURALIZATION,
    },
];

/// Wrap a list of records under the YAML's `records:` key. Every seed
/// YAML in `store/seeds/` has the same outer shape.
#[derive(Debug, Deserialize)]
struct Records<T> {
    #[serde(default = "Vec::new")]
    records: Vec<T>,
}

/// The strict envelope accepted by `navigator site import`.
///
/// Canonical seed files use [`Records`] because they carry several different
/// shapes. Operator-supplied seed documents have one closed contract, so the
/// parser rejects missing or unknown envelope keys before a write can start.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SeedRecords<T> {
    lookup_fields: Vec<String>,
    records: Vec<T>,
}

fn parse<T>(yaml: &str, file: &str) -> anyhow::Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    let r: Records<T> =
        serde_yaml::from_str(yaml).map_err(|e| anyhow::anyhow!("parse {file}: {e}"))?;
    Ok(r.records)
}

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

    /// Resolve the singular glossary term, Surreal table name, or PascalCase
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

/// Lowercase snake_case for a glossary term or a PascalCase seed filename stem.
fn glossary_token(value: &str) -> String {
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

/// The action the reconciler would take for one seed record.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileAction {
    #[default]
    New,
    Unchanged,
    Changed,
}

/// The per-record plan returned by seed reconciliation.
#[derive(Debug, Default, Serialize)]
pub struct ReconcileRecord {
    /// The natural-key value that identifies this record in the seed model.
    pub key: String,
    pub action: ReconcileAction,
    /// Fields that `--overwrite` would replace.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_fields: Vec<String>,
    /// The jurisdiction an Entity record resolved to, named explicitly so a
    /// document that leaves it out is never silently resolved to one the
    /// operator did not choose (ENG-518). `None` for every other seed model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jurisdiction: Option<String>,
    /// Plan-time cautions that do not stop the write, such as a new Person
    /// whose name already belongs to another Person (LAW-169). Dry run only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// The result of reconciling one seed document through the operator API.
#[derive(Debug, Default, Serialize)]
pub struct ReconcileReport {
    pub model: String,
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub records: Vec<ReconcileRecord>,
}

/// The authorization facts a reconcile pass needs, resolved by the adapter
/// (`POST /app/api/seed`) from the caller's session so this module stays free
/// of session types — the same split `people_commands::UpdateContext` draws.
///
/// `project_scope` is `None` for an unrestricted (interactive lawyer)
/// session and `Some(code)` for a CI-minted, project-scoped one (ENG-344).
#[derive(Debug, Clone, Copy)]
pub struct ReconcileActor<'a> {
    pub bootstrap_owner_email: Option<&'a str>,
    pub actor_role: crate::persons::Role,
    pub may_change_roles: bool,
    pub project_scope: Option<&'a str>,
    pub dry_run: bool,
    /// The Person the session belongs to, recorded as the actor on a
    /// Relationship Log row. `None` for a session with no person behind it.
    pub actor_person_id: Option<Uuid>,
}

/// A refusal distinguishable from an ordinary parse or validation failure —
/// the caller asked for something outside its session's scope, not
/// something malformed. `reconcile_seed` downcasts to this type to answer
/// `403 scope_violation` rather than `422 invalid_seed`.
#[derive(Debug, thiserror::Error)]
pub enum ScopeViolation {
    /// The session's scope names an endpoint other than the one it was
    /// presented to.
    #[error("this session's scope does not cover this endpoint")]
    EndpointNotScoped,
    /// The session's scope does not list the requested [`SeedModel`].
    #[error("this session's scope does not cover the {0} model")]
    ModelNotScoped(&'static str),
    /// A pull-request CI session is a validation capability, never a live
    /// reconciliation capability. The API checks this before parsing or
    /// dispatching the requested seed.
    #[error("this CI session permits seed validation only: dry_run must be true and overwrite must be false")]
    DryRunOnly,
    /// The session's scope names a project code with no matching `project`
    /// row — the common case while a repository has no live Project yet.
    #[error("no live project carries code {0:?}")]
    UnknownProject(String),
    /// The record exists and is linked to a project other than the one this
    /// session is scoped to.
    #[error("record is linked to a different project than this session's scope")]
    CrossProject,
    /// The record exists but carries no project link at all, so a scoped
    /// session cannot decide whether it may touch it. Refusing is the safe
    /// default.
    #[error("record carries no project link; a scoped session cannot claim it")]
    Unlinked,
    /// A scoped session tried to create a new Entity. An Entity's project
    /// link (`project.entity_id`) is set when a project opens against it,
    /// never at Entity-creation, so there is no project-owned link for a
    /// scoped write to originate — only the Entity a project already names
    /// can be reconciled under scope.
    #[error(
        "a scoped session cannot create a new entity; only the entity already \
         linked to its scoped project can be updated"
    )]
    EntityCreateNotScopable,
}

/// Reconcile one seed-shaped YAML document.
///
/// The document must use the same `lookup_fields` / `records` envelope as
/// `store/seeds`. By default an existing lookup match is left untouched; with
/// `overwrite`, the fields represented by that model's seed record replace the
/// existing values. When the actor is in `dry_run` mode, the same lookups and
/// reference checks happen but no write is attempted. The dispatch is typed so
/// this authenticated path shares the same natural-key and write machinery as
/// bootstrap seeding, without permitting a caller to name arbitrary SurrealQL
/// tables or fields.
pub async fn reconcile_yaml(
    surreal: &SurrealDb,
    model: SeedModel,
    yaml: &str,
    firm_anchor: &str,
    overwrite: bool,
    actor: &ReconcileActor<'_>,
) -> anyhow::Result<ReconcileReport> {
    validate_yaml(model, yaml)?;
    let scope_project = match actor.project_scope {
        Some(code) => Some(
            crate::projects::find_by_code(surreal, code)
                .await?
                .ok_or_else(|| {
                    anyhow::Error::new(ScopeViolation::UnknownProject(code.to_string()))
                })?,
        ),
        None => None,
    };
    match model {
        SeedModel::Person => {
            reconcile_people(surreal, yaml, overwrite, actor, scope_project.as_ref()).await
        }
        SeedModel::Entity => {
            reconcile_entities(
                surreal,
                yaml,
                firm_anchor,
                overwrite,
                scope_project.as_ref(),
                actor.dry_run,
            )
            .await
        }
        SeedModel::PersonProjectRole => {
            reconcile_person_project_roles(surreal, yaml, actor, scope_project.as_ref()).await
        }
        SeedModel::PersonEntityRole => {
            reconcile_person_entity_roles(surreal, yaml, actor, scope_project.as_ref()).await
        }
        SeedModel::Address => {
            reconcile_addresses(surreal, yaml, actor, scope_project.as_ref()).await
        }
        SeedModel::RelationshipLog => {
            reconcile_relationship_logs(surreal, yaml, actor, scope_project.as_ref()).await
        }
        SeedModel::ProjectReference => {
            reconcile_project_references(surreal, yaml, firm_anchor, actor, scope_project.as_ref())
                .await
        }
        SeedModel::FiduciaryRole => {
            module_records::reconcile_fiduciary_roles(
                surreal,
                yaml,
                overwrite,
                actor,
                scope_project.as_ref(),
            )
            .await
        }
        SeedModel::EstateAsset => {
            module_records::reconcile_estate_assets(
                surreal,
                yaml,
                overwrite,
                actor,
                scope_project.as_ref(),
            )
            .await
        }
        SeedModel::CapTableHolding => {
            module_records::reconcile_cap_table_holdings(
                surreal,
                yaml,
                overwrite,
                actor,
                scope_project.as_ref(),
            )
            .await
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
            let records = parse_seed::<EntityRec>(yaml, model, &["name", "entity_type_id"])?;
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
        SeedModel::Address => {
            let records = parse_seed::<OperatorAddressRec>(
                yaml,
                model,
                &["person_id", "line1", "postal_code"],
            )?;
            require_unique(
                model,
                "person.email, line1, and postal_code",
                records.iter().map(|record| {
                    format!(
                        "{}\u{0}{}\u{0}{}",
                        record.person.email.to_ascii_lowercase(),
                        record.line1.to_ascii_lowercase(),
                        record.postal_code.to_ascii_lowercase()
                    )
                }),
            )
        }
        SeedModel::RelationshipLog => validate_relationship_logs(yaml),
        SeedModel::ProjectReference => validate_project_references(yaml),
        SeedModel::FiduciaryRole => module_records::validate_fiduciary_roles(yaml),
        SeedModel::EstateAsset => module_records::validate_estate_assets(yaml),
        SeedModel::CapTableHolding => module_records::validate_cap_table_holdings(yaml),
    }
}

fn validate_project_references(yaml: &str) -> anyhow::Result<()> {
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

fn validate_relationship_logs(yaml: &str) -> anyhow::Result<()> {
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

fn require_unique(
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

fn parse_seed<T>(
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

/// `person_project_role` scope check for an existing person under a scoped
/// session: allowed only when a row already links this person to the
/// scoped project. Distinguishes "linked elsewhere" from "linked nowhere"
/// in the error only — both are refused, per the issue's ambiguous-case
/// default.
async fn authorize_person_project_scope(
    surreal: &SurrealDb,
    person_id: Uuid,
    project: &crate::projects::Project,
) -> anyhow::Result<()> {
    if crate::projects::participation_for_person(surreal, person_id, project.id)
        .await?
        .is_some()
    {
        return Ok(());
    }
    let elsewhere = crate::projects::participations_for_person(surreal, person_id).await?;
    Err(anyhow::Error::new(if elsewhere.is_empty() {
        ScopeViolation::Unlinked
    } else {
        ScopeViolation::CrossProject
    }))
}

#[allow(clippy::too_many_lines)]
async fn reconcile_people(
    surreal: &SurrealDb,
    yaml: &str,
    overwrite: bool,
    actor: &ReconcileActor<'_>,
    scope_project: Option<&crate::projects::Project>,
) -> anyhow::Result<ReconcileReport> {
    let mut report = ReconcileReport {
        model: SeedModel::Person.term().to_string(),
        ..ReconcileReport::default()
    };
    let update_ctx = crate::people_commands::UpdateContext {
        bootstrap_owner_email: actor.bootstrap_owner_email,
        actor_role: actor.actor_role,
        may_change_roles: actor.may_change_roles,
    };
    for rec in parse_seed::<PersonRec>(yaml, SeedModel::Person, &["email"])? {
        let key = rec.email.clone();
        let name = rec.name.clone();
        let existing = find_person_for_record(surreal, &rec).await?;
        let new_aliases = rec.wanted_aliases(&rec.email, None);
        match existing {
            None => {
                if !actor.dry_run {
                    let created = crate::people_commands::create_person(
                        surreal,
                        &crate::people_commands::CreatePersonCommand {
                            name: rec.name,
                            email: rec.email,
                            role: String::new(),
                            given_name: None,
                            family_name: None,
                            middle_name: None,
                            notion_user_id: None,
                            firm_id: None,
                        },
                    )
                    .await
                    .map_err(|error| anyhow::anyhow!(error.user_message()))?;
                    record_aliases(surreal, created.id, &new_aliases).await?;
                    if let Some(project) = scope_project {
                        crate::projects::add_participation(
                            surreal,
                            project.id,
                            created.id,
                            crate::projects::participation_for_role(created.role),
                        )
                        .await?;
                    }
                }
                let warnings = if actor.dry_run {
                    same_name_warnings(surreal, &name).await?
                } else {
                    Vec::new()
                };
                report.created += 1;
                report.records.push(ReconcileRecord {
                    key,
                    action: ReconcileAction::New,
                    warnings,
                    ..ReconcileRecord::default()
                });
            }
            Some(_) if !overwrite => {
                report.unchanged += 1;
                report.records.push(ReconcileRecord {
                    key,
                    action: ReconcileAction::Unchanged,
                    ..ReconcileRecord::default()
                });
            }
            Some(existing) => {
                if let Some(project) = scope_project {
                    authorize_person_project_scope(surreal, existing.id, project).await?;
                }
                let mut changed_fields = person_changed_fields(&existing, &rec);
                let moves_primary =
                    existing.email.trim().to_lowercase() != rec.email.trim().to_lowercase();
                if moves_primary {
                    changed_fields.push("email".to_string());
                }
                let wanted = rec
                    .wanted_aliases(&rec.email, moves_primary.then_some(existing.email.as_str()));
                // Read the held aliases only when a record names some, so a
                // plain import never depends on the alias store.
                let missing_aliases: Vec<String> = if wanted.is_empty() {
                    wanted
                } else {
                    let held = crate::persons::aliases(surreal, existing.id).await?;
                    wanted
                        .into_iter()
                        .filter(|email| !held.contains(email))
                        .collect()
                };
                if !missing_aliases.is_empty() {
                    changed_fields.push("emails".to_string());
                }
                if changed_fields.is_empty() {
                    report.unchanged += 1;
                    report.records.push(ReconcileRecord {
                        key,
                        action: ReconcileAction::Unchanged,
                        ..ReconcileRecord::default()
                    });
                } else {
                    report.updated += 1;
                    report.records.push(ReconcileRecord {
                        key,
                        action: ReconcileAction::Changed,
                        changed_fields,
                        ..ReconcileRecord::default()
                    });
                    if !actor.dry_run {
                        update_person_from_seed(surreal, &existing, rec, &update_ctx).await?;
                        record_aliases(surreal, existing.id, &missing_aliases).await?;
                    }
                }
            }
        }
    }
    Ok(report)
}

/// One caution per existing Person whose name equals `name`
/// (case-insensitively). A service-list attorney who changes firms arrives
/// under a new email, and the email-keyed import would otherwise create a
/// second Person for the same human without a word (LAW-169).
async fn same_name_warnings(surreal: &SurrealDb, name: &str) -> anyhow::Result<Vec<String>> {
    let name = name.trim();
    let namesakes = crate::persons::search(surreal, Some(name), None, 20).await?;
    Ok(namesakes
        .into_iter()
        .filter(|person| person.name.trim().eq_ignore_ascii_case(name))
        .map(|person| {
            format!(
                "a Person named `{}` already exists ({}); importing this email creates a second Person for the same name",
                person.name, person.email
            )
        })
        .collect())
}

async fn update_person_from_seed(
    surreal: &SurrealDb,
    existing: &crate::persons::Person,
    rec: PersonRec,
    update_ctx: &crate::people_commands::UpdateContext<'_>,
) -> anyhow::Result<()> {
    crate::people_commands::update_person(
        surreal,
        existing.id,
        &crate::people_commands::UpdatePersonCommand {
            name: Some(rec.name),
            email: Some(rec.email),
            role: String::new(),
            given_name: None,
            family_name: None,
            middle_name: None,
            notion_user_id: None,
            linkedin_url: None,
            csrf_token: None,
        },
        update_ctx,
    )
    .await
    .map_err(|error| anyhow::anyhow!(error.user_message()))?;
    if existing.profile_image_url != rec.profile_image_url {
        crate::persons::edit(
            surreal,
            existing.id,
            &crate::persons::PersonEdit {
                profile_image_url: Some(rec.profile_image_url),
                ..crate::persons::PersonEdit::default()
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    }
    Ok(())
}

async fn reconcile_entities(
    surreal: &SurrealDb,
    yaml: &str,
    firm_anchor: &str,
    overwrite: bool,
    scope_project: Option<&crate::projects::Project>,
    dry_run: bool,
) -> anyhow::Result<ReconcileReport> {
    let mut report = ReconcileReport {
        model: SeedModel::Entity.term().to_string(),
        ..ReconcileReport::default()
    };
    for rec in parse_seed::<EntityRec>(yaml, SeedModel::Entity, &["name", "entity_type_id"])? {
        let entity_type = crate::entity_types::find_by_name(surreal, &rec.entity_type.name)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "entity seed references unknown entity type {:?}",
                    rec.entity_type.name
                )
            })?;
        let jurisdiction_name = declared_jurisdiction_name(&rec.name, &rec.entity_type)?;
        let jurisdiction = jurisdictions::find_by_name(surreal, jurisdiction_name)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("entity seed references unknown jurisdiction {jurisdiction_name:?}")
            })?;
        let existing =
            crate::entities::find_by_name_and_type(surreal, &rec.name, entity_type.id).await?;
        let key = format!("{} / {}", rec.name, rec.entity_type.name);
        match existing {
            None => {
                if scope_project.is_some() {
                    return Err(anyhow::Error::new(ScopeViolation::EntityCreateNotScopable));
                }
                if !dry_run {
                    crate::entity_commands::create_entity(
                        surreal,
                        firm_anchor,
                        &crate::entity_commands::CreateEntityCommand {
                            name: rec.name,
                            entity_type_id: entity_type.id,
                            jurisdiction_id: jurisdiction.id,
                        },
                    )
                    .await
                    .map_err(|error| anyhow::anyhow!(error.user_message()))?;
                }
                report.created += 1;
                report.records.push(ReconcileRecord {
                    key,
                    action: ReconcileAction::New,
                    jurisdiction: Some(jurisdiction_name.to_string()),
                    ..ReconcileRecord::default()
                });
            }
            Some(_) if !overwrite => {
                report.unchanged += 1;
                report.records.push(ReconcileRecord {
                    key,
                    action: ReconcileAction::Unchanged,
                    jurisdiction: Some(jurisdiction_name.to_string()),
                    ..ReconcileRecord::default()
                });
            }
            Some(existing) => {
                if let Some(project) = scope_project {
                    if existing.id != project.entity_id {
                        return Err(anyhow::Error::new(ScopeViolation::CrossProject));
                    }
                }
                let changed_fields = entity_changed_fields(&existing, jurisdiction.id);
                if changed_fields.is_empty() {
                    report.unchanged += 1;
                    report.records.push(ReconcileRecord {
                        key,
                        action: ReconcileAction::Unchanged,
                        jurisdiction: Some(jurisdiction_name.to_string()),
                        ..ReconcileRecord::default()
                    });
                } else {
                    report.updated += 1;
                    report.records.push(ReconcileRecord {
                        key,
                        action: ReconcileAction::Changed,
                        changed_fields,
                        jurisdiction: Some(jurisdiction_name.to_string()),
                        ..ReconcileRecord::default()
                    });
                    if !dry_run {
                        update_entity_from_seed(
                            surreal,
                            existing.id,
                            firm_anchor,
                            rec.name,
                            entity_type.id,
                            jurisdiction.id,
                        )
                        .await?;
                    }
                }
            }
        }
    }
    Ok(report)
}

async fn update_entity_from_seed(
    surreal: &SurrealDb,
    id: Uuid,
    firm_anchor: &str,
    name: String,
    entity_type_id: Uuid,
    jurisdiction_id: Uuid,
) -> anyhow::Result<()> {
    crate::entity_commands::update_entity(
        surreal,
        id,
        firm_anchor,
        &crate::entity_commands::UpdateEntityCommand {
            name: Some(name),
            entity_type_id: Some(entity_type_id),
            jurisdiction_id: Some(jurisdiction_id),
            xero_id: None,
        },
    )
    .await
    .map_err(|error| anyhow::anyhow!(error.user_message()))?;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorPersonRef {
    email: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorProjectRef {
    code: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorEntityNameRef {
    name: String,
}

/// One Relationship Log row about a Person. The natural key is the whole
/// row — `(subject.email, action, detail)` — so re-importing the same
/// filing's service list appends nothing. `project` names the Project whose
/// work produced the row; it scopes the row and grants no role.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorRelationshipLogRec {
    subject: OperatorPersonRef,
    action: String,
    detail: String,
    #[serde(default)]
    project: Option<OperatorProjectRef>,
}

/// The `lookup_fields` a `project_reference` document declares: the natural
/// key is the Project, the party, and the reason, and the party half is the
/// nested `person.email` or `entity.name` the record carries.
const PROJECT_REFERENCE_LOOKUP_FIELDS: &[&str] = &["project_id", "reason"];

/// A Person a Project references without granting participation. `name` is
/// what a first reference creates the row with; an existing row keeps its
/// own name.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorReferencedPersonRef {
    email: String,
    name: String,
}

/// One Project Reference: exactly one of `person` or `entity`, the reason
/// the Project names them, and — for an unscoped session — the Project.
/// A scoped session always references from its own Project and refuses a
/// record naming another.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorProjectReferenceRec {
    reason: String,
    #[serde(default)]
    person: Option<OperatorReferencedPersonRef>,
    #[serde(default)]
    entity: Option<EntityRec>,
    #[serde(default)]
    project: Option<OperatorProjectRef>,
}

impl OperatorProjectReferenceRec {
    /// The natural-key half naming the party, or why the record is
    /// malformed: it must name exactly one of `person` or `entity`, and a
    /// reason the schema admits.
    fn party_key(&self) -> anyhow::Result<String> {
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
struct OperatorPersonProjectRoleRec {
    person: OperatorPersonRef,
    project: OperatorProjectRef,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorPersonEntityRoleRec {
    person: OperatorPersonRef,
    entity: OperatorEntityNameRef,
    role: String,
}

/// A person's postal address (LAW-57). The natural key is
/// `(person.email, line1, postal_code)` — the same (street, ZIP) pair
/// `store::addresses::find_or_create_for_person` matches on, scoped to the
/// referenced person.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorAddressRec {
    person: OperatorPersonRef,
    line1: String,
    #[serde(default)]
    line2: Option<String>,
    city: String,
    region: String,
    postal_code: String,
    country: String,
}

async fn reconcile_person_project_roles(
    surreal: &SurrealDb,
    yaml: &str,
    actor: &ReconcileActor<'_>,
    scope_project: Option<&crate::projects::Project>,
) -> anyhow::Result<ReconcileReport> {
    let mut report = ReconcileReport {
        model: SeedModel::PersonProjectRole.term().to_string(),
        ..ReconcileReport::default()
    };
    for rec in parse_seed::<OperatorPersonProjectRoleRec>(
        yaml,
        SeedModel::PersonProjectRole,
        &["person_id", "project_id"],
    )? {
        let key = format!("{} / {}", rec.person.email, rec.project.code);
        let person = crate::persons::find_by_email_ci(surreal, &rec.person.email)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "person_project_role seed references unknown person email {:?}",
                    rec.person.email
                )
            })?;
        let project = crate::projects::find_by_code(surreal, &rec.project.code)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "person_project_role seed references unknown project code {:?}",
                    rec.project.code
                )
            })?;
        if let Some(scope) = scope_project {
            if project.id != scope.id {
                return Err(anyhow::Error::new(ScopeViolation::CrossProject));
            }
        }
        let participation = crate::projects::participation_for_role(person.role);
        if crate::projects::participation_for_person(surreal, person.id, project.id)
            .await?
            .is_some()
        {
            report.unchanged += 1;
            report.records.push(ReconcileRecord {
                key,
                action: ReconcileAction::Unchanged,
                ..ReconcileRecord::default()
            });
        } else {
            if !actor.dry_run {
                crate::projects::add_participation(surreal, project.id, person.id, participation)
                    .await?;
            }
            report.created += 1;
            report.records.push(ReconcileRecord {
                key,
                action: ReconcileAction::New,
                ..ReconcileRecord::default()
            });
        }
    }
    Ok(report)
}

async fn reconcile_person_entity_roles(
    surreal: &SurrealDb,
    yaml: &str,
    actor: &ReconcileActor<'_>,
    scope_project: Option<&crate::projects::Project>,
) -> anyhow::Result<ReconcileReport> {
    let mut report = ReconcileReport {
        model: SeedModel::PersonEntityRole.term().to_string(),
        ..ReconcileReport::default()
    };
    for rec in parse_seed::<OperatorPersonEntityRoleRec>(
        yaml,
        SeedModel::PersonEntityRole,
        &["person_id", "entity_id"],
    )? {
        let key = format!("{} / {} / {}", rec.person.email, rec.entity.name, rec.role);
        let person = crate::persons::find_by_email_ci(surreal, &rec.person.email)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "person_entity_role seed references unknown person email {:?}",
                    rec.person.email
                )
            })?;
        let entity = crate::entities::find_by_name(surreal, &rec.entity.name)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "person_entity_role seed references unknown entity {:?}",
                    rec.entity.name
                )
            })?;
        if let Some(scope) = scope_project {
            if entity.id != scope.entity_id {
                return Err(anyhow::Error::new(ScopeViolation::CrossProject));
            }
        }
        if crate::entity_roles::find(surreal, person.id, entity.id, &rec.role)
            .await?
            .is_some()
        {
            report.unchanged += 1;
            report.records.push(ReconcileRecord {
                key,
                action: ReconcileAction::Unchanged,
                ..ReconcileRecord::default()
            });
        } else {
            if !actor.dry_run {
                crate::entity_roles::grant(surreal, person.id, entity.id, &rec.role).await?;
            }
            report.created += 1;
            report.records.push(ReconcileRecord {
                key,
                action: ReconcileAction::New,
                ..ReconcileRecord::default()
            });
        }
    }
    Ok(report)
}

/// Append a Relationship Log row about each named Person. The log is
/// append-only, so an identical `(subject, action, detail)` row already on
/// the trail is reported unchanged rather than written twice. Writing a row
/// grants nothing: no `person_project_role`, no `entity_role`. A scoped
/// session may write only about a person already on its Project, and every
/// row it writes carries that Project; a row naming another Project is
/// refused. An unrestricted session records the Project a row names.
async fn reconcile_relationship_logs(
    surreal: &SurrealDb,
    yaml: &str,
    actor: &ReconcileActor<'_>,
    scope_project: Option<&crate::projects::Project>,
) -> anyhow::Result<ReconcileReport> {
    let mut report = ReconcileReport {
        model: SeedModel::RelationshipLog.term().to_string(),
        ..ReconcileReport::default()
    };
    for rec in parse_seed::<OperatorRelationshipLogRec>(
        yaml,
        SeedModel::RelationshipLog,
        &["subject_id", "action", "detail"],
    )? {
        let key = format!("{} / {} / {}", rec.subject.email, rec.action, rec.detail);
        let person = crate::persons::find_by_email_ci(surreal, &rec.subject.email)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "relationship_log seed references unknown person email {:?}",
                    rec.subject.email
                )
            })?;
        if let Some(project) = scope_project {
            authorize_person_project_scope(surreal, person.id, project).await?;
        }
        let project_id = match (scope_project, rec.project) {
            (Some(scope), Some(named)) if named.code != scope.code => {
                return Err(anyhow::Error::new(ScopeViolation::CrossProject));
            }
            (Some(scope), _) => Some(scope.id),
            (None, Some(named)) => Some(
                crate::projects::find_by_code(surreal, &named.code)
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "relationship_log seed references unknown project code {:?}",
                            named.code
                        )
                    })?
                    .id,
            ),
            (None, None) => None,
        };
        let entry = crate::relationship_logs::NewRelationshipLog {
            actor_person_id: actor.actor_person_id,
            subject_type: crate::relationship_logs::PERSON_SUBJECT.to_string(),
            subject_id: person.id,
            action: rec.action,
            detail: rec.detail,
            project_id,
        };
        if crate::relationship_logs::exists(surreal, &entry).await? {
            report.unchanged += 1;
            report.records.push(ReconcileRecord {
                key,
                action: ReconcileAction::Unchanged,
                ..ReconcileRecord::default()
            });
            continue;
        }
        if !actor.dry_run {
            crate::relationship_logs::record(surreal, &entry).await?;
        }
        report.created += 1;
        report.records.push(ReconcileRecord {
            key,
            action: ReconcileAction::New,
            ..ReconcileRecord::default()
        });
    }
    Ok(report)
}

/// Reference a counterparty Person or Entity from a Project, granting no
/// participation. The one seed route through which a scoped session may
/// create an Entity — the counterparty organization the matter verified —
/// and only as the referenced party: the Project's own `entity_id` never
/// moves, and no `person_project_role` row is written for a referenced
/// Person. An existing Person or Entity is referenced as it is, whatever
/// Project it belongs to, because a reference changes nothing on the row.
async fn reconcile_project_references(
    surreal: &SurrealDb,
    yaml: &str,
    firm_anchor: &str,
    actor: &ReconcileActor<'_>,
    scope_project: Option<&crate::projects::Project>,
) -> anyhow::Result<ReconcileReport> {
    let mut report = ReconcileReport {
        model: SeedModel::ProjectReference.term().to_string(),
        ..ReconcileReport::default()
    };
    for rec in parse_seed::<OperatorProjectReferenceRec>(
        yaml,
        SeedModel::ProjectReference,
        PROJECT_REFERENCE_LOOKUP_FIELDS,
    )? {
        let party_key = rec.party_key()?;
        let reason = rec.reason.trim().to_string();
        let project = match (scope_project, rec.project.as_ref()) {
            (Some(scope), Some(named)) if named.code != scope.code => {
                return Err(anyhow::Error::new(ScopeViolation::CrossProject));
            }
            (Some(scope), _) => scope.clone(),
            (None, Some(named)) => crate::projects::find_by_code(surreal, &named.code)
                .await?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "project_reference seed references unknown project code {:?}",
                        named.code
                    )
                })?,
            (None, None) => anyhow::bail!(
                "project_reference seed record {party_key:?} names no project; add `project: {{ code: <code> }}`"
            ),
        };
        let key = format!("{} / {party_key} / {reason}", project.code);
        let party =
            resolve_reference_party(surreal, firm_anchor, actor.dry_run, rec.person, rec.entity)
                .await?;
        let exists = match party {
            Some(party) => crate::project_references::find(surreal, project.id, party, &reason)
                .await?
                .is_some(),
            None => false,
        };
        if exists {
            report.unchanged += 1;
            report.records.push(ReconcileRecord {
                key,
                action: ReconcileAction::Unchanged,
                ..ReconcileRecord::default()
            });
            continue;
        }
        if let (false, Some(party)) = (actor.dry_run, party) {
            crate::project_references::reference(surreal, project.id, party, &reason).await?;
        }
        report.created += 1;
        report.records.push(ReconcileRecord {
            key,
            action: ReconcileAction::New,
            ..ReconcileRecord::default()
        });
    }
    Ok(report)
}

/// The row a `project_reference` record names, creating it when absent.
/// `None` only on a dry run whose party does not exist yet: there is nothing
/// to look up, and the plan reports the reference as new.
async fn resolve_reference_party(
    surreal: &SurrealDb,
    firm_anchor: &str,
    dry_run: bool,
    person: Option<OperatorReferencedPersonRef>,
    entity: Option<EntityRec>,
) -> anyhow::Result<Option<crate::project_references::Party>> {
    match (person, entity) {
        (Some(person), _) => {
            let email = person.email.trim().to_string();
            match crate::persons::find_by_email_ci(surreal, &email).await? {
                Some(existing) => Ok(Some(crate::project_references::Party::person(existing.id))),
                None if dry_run => Ok(None),
                None => {
                    let created = crate::people_commands::create_person(
                        surreal,
                        &crate::people_commands::CreatePersonCommand {
                            name: person.name,
                            email,
                            role: String::new(),
                            given_name: None,
                            family_name: None,
                            middle_name: None,
                            notion_user_id: None,
                            firm_id: None,
                        },
                    )
                    .await
                    .map_err(|error| anyhow::anyhow!(error.user_message()))?;
                    Ok(Some(crate::project_references::Party::person(created.id)))
                }
            }
        }
        (None, Some(entity)) => {
            let entity_type = crate::entity_types::find_by_name(surreal, &entity.entity_type.name)
                .await?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "project_reference seed references unknown entity type {:?}",
                        entity.entity_type.name
                    )
                })?;
            let jurisdiction_name = declared_jurisdiction_name(&entity.name, &entity.entity_type)?;
            let jurisdiction = jurisdictions::find_by_name(surreal, jurisdiction_name)
                .await?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "project_reference seed references unknown jurisdiction {jurisdiction_name:?}"
                    )
                })?;
            match crate::entities::find_by_name_and_type(surreal, &entity.name, entity_type.id)
                .await?
            {
                Some(existing) => Ok(Some(crate::project_references::Party::entity(existing.id))),
                None if dry_run => Ok(None),
                None => {
                    let created = crate::entity_commands::create_entity(
                        surreal,
                        firm_anchor,
                        &crate::entity_commands::CreateEntityCommand {
                            name: entity.name,
                            entity_type_id: entity_type.id,
                            jurisdiction_id: jurisdiction.id,
                        },
                    )
                    .await
                    .map_err(|error| anyhow::anyhow!(error.user_message()))?;
                    Ok(Some(crate::project_references::Party::entity(created.id)))
                }
            }
        }
        (None, None) => {
            anyhow::bail!("project_reference seed record names neither a person nor an entity")
        }
    }
}

/// Attach a person's postal address (LAW-57). Like
/// [`reconcile_person_project_roles`] and [`reconcile_person_entity_roles`],
/// this is a fact table keyed by its natural key rather than a mutable
/// record: an existing (person, line1, postal_code) match is left
/// untouched — `overwrite` has no meaning here, since the natural key
/// already names every field this model's identity depends on.
async fn reconcile_addresses(
    surreal: &SurrealDb,
    yaml: &str,
    actor: &ReconcileActor<'_>,
    scope_project: Option<&crate::projects::Project>,
) -> anyhow::Result<ReconcileReport> {
    let mut report = ReconcileReport {
        model: SeedModel::Address.term().to_string(),
        ..ReconcileReport::default()
    };
    for rec in parse_seed::<OperatorAddressRec>(
        yaml,
        SeedModel::Address,
        &["person_id", "line1", "postal_code"],
    )? {
        let key = format!("{} / {} {}", rec.person.email, rec.line1, rec.postal_code);
        let person = crate::persons::find_by_email_ci(surreal, &rec.person.email)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "address seed references unknown person email {:?}",
                    rec.person.email
                )
            })?;
        if let Some(scope) = scope_project {
            authorize_person_project_scope(surreal, person.id, scope).await?;
        }
        let new = crate::addresses::NewAddress {
            person_id: Some(person.id),
            entity_id: None,
            line1: rec.line1,
            line2: rec.line2,
            city: rec.city,
            region: rec.region,
            postal_code: rec.postal_code,
            country: rec.country,
        };
        let existing = crate::addresses::for_person(surreal, person.id)
            .await?
            .into_iter()
            .any(|a| a.line1 == new.line1 && a.postal_code == new.postal_code);
        if existing {
            report.unchanged += 1;
            report.records.push(ReconcileRecord {
                key,
                action: ReconcileAction::Unchanged,
                ..ReconcileRecord::default()
            });
        } else {
            if !actor.dry_run {
                crate::addresses::create(surreal, &new).await?;
            }
            report.created += 1;
            report.records.push(ReconcileRecord {
                key,
                action: ReconcileAction::New,
                ..ReconcileRecord::default()
            });
        }
    }
    Ok(report)
}

fn person_changed_fields(existing: &crate::persons::Person, incoming: &PersonRec) -> Vec<String> {
    let mut fields = Vec::new();
    if existing.name != incoming.name {
        fields.push("name".to_string());
    }
    if existing.profile_image_url != incoming.profile_image_url {
        fields.push("profile_image_url".to_string());
    }
    fields
}

fn entity_changed_fields(existing: &crate::entities::Entity, jurisdiction_id: Uuid) -> Vec<String> {
    if existing.jurisdiction_id == jurisdiction_id {
        Vec::new()
    } else {
        vec!["jurisdiction".to_string()]
    }
}

/// Run the full canonical seed pass against `db`. Each entity table
/// is populated from its corresponding `store/seeds/*.yaml` file.
/// Idempotent: re-running inserts no new rows.
/// Apply the production-safe canonical seed: reference data plus the
/// firm-owned baseline (jurisdictions, entity types, the protected firm
/// [`FIRM_ENTITY_NAME`] Entity and its people, questions, credentials,
/// templates, testimonials). It is **environment-blind** — it
/// runs identically in production, so it must never insert a disposable
/// Project, mailroom, letter, or answer row. Those live in
/// [`seed_sample_portfolio`] and are applied on a `dev` profile only.
pub async fn seed_canonical(
    surreal: &SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
) -> anyhow::Result<SeedReport> {
    let mut r = SeedReport::default();
    seed_canonical_into(surreal, storage, &mut r).await?;
    Ok(r)
}

/// Apply the compiled sample-matter fixture on top of the canonical seed.
/// It gives an environment the three synthetic matters, their participants, and
/// the reference rows the portal walkthrough needs. Applied on a `dev` profile
/// only. Idempotent: a second run inserts zero duplicates.
pub async fn seed_sample_portfolio(
    surreal: &SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
) -> anyhow::Result<SeedReport> {
    let mut r = SeedReport::default();
    seed_sample_portfolio_into(surreal, storage, &mut r).await?;
    Ok(r)
}

/// Apply one brand's own seeds. This is production data, deliberately: the
/// Firm's postal identities are real, and they belong only in the deployment
/// that serves that brand. Idempotent on the same natural keys as every other
/// layer.
///
/// # Errors
///
/// Propagates any store error from the underlying writes.
pub async fn seed_brand(surreal: &SurrealDb, brand: BrandSeed) -> anyhow::Result<SeedReport> {
    let mut r = SeedReport::default();
    seed_brand_into(surreal, brand, &mut r).await?;
    Ok(r)
}

/// Apply only one brand's social handles and Blog Posts. [`seed_brand`] runs
/// this as its last step; it is separate so a test can hold a Brand's blog
/// without the Firm's entities and mail center.
///
/// # Errors
///
/// A seed that breaks the Post contract, or a store failure.
pub async fn seed_brand_blog(surreal: &SurrealDb, brand: BrandSeed) -> anyhow::Result<SeedReport> {
    let mut r = SeedReport::default();
    seed_brand_blog_into(surreal, brand, &mut r).await?;
    Ok(r)
}

async fn seed_brand_blog_into(
    surreal: &SurrealDb,
    brand: BrandSeed,
    r: &mut SeedReport,
) -> anyhow::Result<()> {
    // Handles first: a Post's social link may only name a recorded handle.
    if let Some((yaml, path)) = brand.social_handles() {
        seed_social_handles(surreal, yaml, path, r).await?;
    }
    if let Some((yaml, path)) = brand.posts() {
        seed_posts(surreal, yaml, path, r).await?;
    }
    Ok(())
}

/// The single environment-aware orchestration call, and the three layers it
/// composes.
///
/// 1. The **canonical** seed, on every boot of every brand in every
///    environment: the shared identities, reference data, and catalog.
/// 2. The booting **brand's own** seed, likewise in every environment
///    including production. This is the layer that carries data one brand owns
///    and another must not: `neon` seeds the Firm's own mailboxes, and a
///    white-label tenant seeds none.
/// 3. The disposable **sample-matter fixture**, on a `dev` profile only, so
///    synthetic Project, mail, or answer rows can never reach a deployment
///    holding real files.
///
/// The profile is the whole predicate for layer 3, and
/// [`crate::config::sample_matters`] is **not** consulted here.
/// `NAVIGATOR_SIMULATED_MATTERS` decides one thing — whether a deployment
/// announces that its matters are simulated — and writes nothing. Keeping the
/// announcement separate from the writing is what lets a production-profile
/// deployment carry the banner over a portfolio it was given once and now
/// keeps, rather than one this seed re-asserts on every boot.
///
/// Every layer is idempotent, so a reset/recreate that runs this again
/// restores the exact same baseline.
///
/// # Errors
///
/// Propagates any store error from the underlying writes.
pub async fn seed_environment(
    surreal: &SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    environment: crate::DeploymentEnvironment,
    brand: BrandSeed,
) -> anyhow::Result<SeedReport> {
    seed_environment_with(surreal, storage, environment, brand).await
}

/// [`seed_environment`], with the profile as an argument so a test drives both
/// answers without mutating process environment.
///
/// # Errors
///
/// Propagates any store error from the underlying writes.
pub async fn seed_environment_with(
    surreal: &SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    environment: crate::DeploymentEnvironment,
    brand: BrandSeed,
) -> anyhow::Result<SeedReport> {
    let mut r = SeedReport::default();
    seed_canonical_into(surreal, storage, &mut r).await?;
    seed_brand_into(surreal, brand, &mut r).await?;
    if environment != crate::DeploymentEnvironment::Production {
        seed_sample_portfolio_into(surreal, storage, &mut r).await?;
    }
    Ok(r)
}

/// The brand layer runs after [`seed_canonical_into`] because its addresses
/// hang off the canonical firm Entity — `seed_addresses` *skips* a record whose
/// Entity it cannot resolve, so the wrong order would be a silent no-op rather
/// than an error. A brand seeds no Entity of its own: an identity row is a
/// public firm fact in the canonical layer or private runtime data.
async fn seed_brand_into(
    surreal: &SurrealDb,
    brand: BrandSeed,
    r: &mut SeedReport,
) -> anyhow::Result<()> {
    if let Some((yaml, path)) = brand.mailrooms() {
        seed_mailrooms(surreal, yaml, path, r).await?;
    }
    seed_addresses(surreal, brand, r).await?;
    seed_brand_blog_into(surreal, brand, r).await
}

async fn seed_canonical_into(
    surreal: &SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    r: &mut SeedReport,
) -> anyhow::Result<()> {
    seed_jurisdictions(surreal, r).await?;
    seed_entity_types(surreal, r).await?;
    rename_legacy_firm_entity(surreal).await?;
    seed_entities(surreal, canonical::ENTITY, "Entity.yaml", r).await?;
    // The practice's Firm needs an existing `role = admin` person for its
    // Admin DRI (ENG-499: creation is atomic with one), so persons and their
    // roles must be seeded first.
    seed_persons(surreal, r).await?;
    seed_user_roles(surreal, r).await?;
    // `seed_practice` resolves/creates the practice Firm, then migrates the
    // compiled house-brand keys into `brand` rows scoped to it before
    // attaching each one: `store::firms::attach_brand` validates a key
    // against this table now, not the closed `CLOSED_BRAND_KEYS` array
    // (ENG-496), and every row it creates must already carry that Firm's id
    // (ENG-659).
    seed_practice(surreal).await?;
    seed_firm_memberships(surreal).await?;
    seed_questions(surreal, r).await?;
    seed_person_entity_roles(surreal, r).await?;
    seed_credentials(surreal, r).await?;
    seed_templates(surreal, storage, r).await?;
    seed_testimonials(surreal, r).await?;
    seed_glossary_terms(surreal, r).await?;
    Ok(())
}

async fn seed_sample_portfolio_into(
    surreal: &SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    r: &mut SeedReport,
) -> anyhow::Result<()> {
    seed_mailrooms(surreal, sample_seeds::MAILROOM, "sample/Mailroom.yaml", r).await?;
    seed_role_matrix_sample(surreal, storage, r).await?;
    seed_firm_memberships(surreal).await?;
    seed_git_repositories(surreal, r).await?;
    seed_letters(surreal, r).await?;
    seed_answers(surreal, r).await?;
    seed_person_project_roles(surreal, r).await?;
    seed_project_firm_ownership(surreal).await?;
    seed_sample_invoices(surreal).await?;
    Ok(())
}

/// One trailing-30-day Xero invoice per sample matter (ENG-591), invented
/// exactly like every other row this table carries, so the local Firm show
/// page's invoice graphs have something to draw. One is fully paid, one is
/// partly paid, and one is unpaid, across the fixture's three house brands.
async fn seed_sample_invoices(surreal: &SurrealDb) -> anyhow::Result<()> {
    let now = chrono::Utc::now();
    for (matter, amount_cents, paid_cents, days_ago) in [
        (&SAMPLE_MATTERS[0], 480_000_i64, 480_000_i64, 3_i64),
        (&SAMPLE_MATTERS[1], 250_000_i64, 0_i64, 10_i64),
        (&SAMPLE_MATTERS[2], 120_000_i64, 60_000_i64, 20_i64),
    ] {
        let Some(project) = crate::projects::find_by_name(surreal, matter.name).await? else {
            continue;
        };
        let xero_invoice_id = format!("sample-invoice-{}", matter.code);
        crate::xero_invoices::upsert(
            surreal,
            &crate::xero_invoices::UpsertXeroInvoice {
                project_id: project.id,
                xero_invoice_id: xero_invoice_id.clone(),
                reference: format!("Sample invoice for {}", matter.code),
                status: "AUTHORISED".to_string(),
                amount_cents,
                currency: "USD".to_string(),
                issued_at: now - chrono::Duration::days(days_ago),
                due_at: None,
            },
        )
        .await?;
        if paid_cents > 0 {
            let status = if paid_cents >= amount_cents {
                "PAID"
            } else {
                "AUTHORISED"
            };
            crate::xero_invoices::record_reconcile(surreal, &xero_invoice_id, status, paid_cents)
                .await?;
        }
    }
    Ok(())
}

/// One sample matter the development and staging fixture carries.
///
/// Three of these ship. Each is a whole matter — a client Entity, a Project
/// code that is also its portal's URL segment, the practice it demonstrates,
/// and the public repository whose Vite bundle mounts on it — so adding a
/// fourth is a row here rather than a fourth branch through the seed.
///
/// **Everything in this table is invented.** No client, matter, dispute, or
/// estate named here corresponds to a real one, which is the whole reason the
/// fixture may be published to a deployment anyone can reach.
struct SampleMatter {
    /// The Project code, which is also the URL segment its portal mounts under
    /// at `/app/projects/{code}/portal/`.
    code: &'static str,
    /// The matter as a reader sees it named.
    name: &'static str,
    /// The client Entity this matter is opened for.
    client_entity: &'static str,
    /// Whether that client is a person or a company. The two `Entity` shapes
    /// the fixture needs, resolved against the seeded entity types.
    client_kind: SampleClientKind,
    /// The practice the matter demonstrates, rendered as the Project's
    /// description.
    description: &'static str,
    /// The repository that declares this matter's pages and Drafts.
    ///
    /// Recorded on the Project rather than compiled into the command that
    /// clones it, so each sample matter demonstrates the real mechanism: a
    /// Project names its own repository, on whatever forge hosts it.
    repository_url: &'static str,
    /// Which house brand's storefront this matter was opened through — a
    /// closed key from `views::brand::BrandKey`. The sample matters carry
    /// registered values so the local fixture exercises the brand registry.
    brand: &'static str,
}

/// Which seeded entity type a sample matter's client resolves to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SampleClientKind {
    /// A natural person — the individual plaintiff and the individual testator.
    Human,
    /// A Nevada C-Corp — the company that engages the firm as outside counsel.
    Company,
}

impl SampleClientKind {
    /// The seeded `entity_types.name` this kind resolves to.
    const fn entity_type(self) -> &'static str {
        match self {
            Self::Human => "Human",
            Self::Company => "C-Corp",
        }
    }
}

/// The three sample matters, in the order a reader meets them.
///
/// They are three deliberately different shapes of legal work, because one
/// matter could only ever demonstrate one: a dispute in front of a court, a
/// company on a monthly retainer, and an estate plan. A person signing in as
/// the fixture Client sees all three at once, which is what makes the
/// participation-scoped list worth looking at.
const SAMPLE_MATTERS: &[SampleMatter] = &[
    SampleMatter {
        code: SAMPLE_LITIGATION_CODE,
        name: "Cruller v. Prine",
        client_entity: "Dermot Cruller",
        client_kind: SampleClientKind::Human,
        description: "trespass to land, and rescission of the doughnut instrument",
        repository_url: "https://github.com/neon-law/sample-litigation",
        brand: "neon",
    },
    SampleMatter {
        code: SAMPLE_TRANSACTIONAL_CODE,
        name: "Widget Works — Outside Counsel",
        client_entity: "Widget Works, Inc.",
        client_kind: SampleClientKind::Company,
        description: "employment agreements and contract review on a monthly retainer",
        repository_url: "https://github.com/neon-law/sample-transactional",
        brand: "delete-your-data",
    },
    SampleMatter {
        code: SAMPLE_ESTATE_CODE,
        name: "Estate of Cornelius Montgomery",
        client_entity: "Cornelius Montgomery",
        client_kind: SampleClientKind::Human,
        description: "estate plan dividing the residue among nieces and nephews",
        repository_url: "https://github.com/neon-law/sample-estate",
        brand: "vesta",
    },
];

/// The environment variable naming the deployment's bootstrap owner.
pub const BOOTSTRAP_OWNER_EMAIL_ENV: &str = "NAVIGATOR_BOOTSTRAP_OWNER_EMAIL";

/// The bootstrap owner a tier falls back to when it configures none — the KIND
/// Rauthy fixture's Owner, so a local sample matter's DRI is an account a
/// developer can sign in as.
const DEFAULT_BOOTSTRAP_OWNER_EMAIL: &str = "owner@neonlaw.com";

/// The email of the identity that owns this deployment.
///
/// The same variable the OIDC callback reads to decide which unseeded identity
/// may create itself as `owner`, so the sample portfolio's accountable lawyer
/// and the deployment's bootstrap identity cannot drift apart. A blank value is
/// treated as unset rather than as an empty address.
#[must_use]
pub fn bootstrap_owner_email() -> String {
    std::env::var(BOOTSTRAP_OWNER_EMAIL_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_BOOTSTRAP_OWNER_EMAIL.to_string())
}

/// The disputes matter's Project code, and therefore its portal's URL segment.
///
/// Exposed because the browser walkthrough and the `dev sample-project` command
/// both name the matter they drive, and a code that drifted between the seed
/// and either of them would 404 at the mount rather than fail a test.
pub const SAMPLE_LITIGATION_CODE: &str = "sample-litigation";

/// The transactional matter's Project code.
pub const SAMPLE_TRANSACTIONAL_CODE: &str = "sample-transactional";

/// The estate matter's Project code.
pub const SAMPLE_ESTATE_CODE: &str = "sample-estate";

/// Look up one sample matter by its Project code.
#[must_use]
pub fn sample_matter_codes() -> Vec<&'static str> {
    SAMPLE_MATTERS.iter().map(|matter| matter.code).collect()
}

/// Every sample matter's caption, in table order.
///
/// Exposed so a test asserting what a participant sees can derive the list
/// rather than restate it: a fourth matter would otherwise silently narrow
/// what those tests check to the three they were written against.
#[must_use]
pub fn sample_matter_names() -> Vec<&'static str> {
    SAMPLE_MATTERS.iter().map(|matter| matter.name).collect()
}

/// The public repository whose bundle mounts on one sample matter, or
/// `None` for a code that names no sample matter.
#[must_use]
pub fn sample_matter_repository(code: &str) -> Option<&'static str> {
    SAMPLE_MATTERS
        .iter()
        .find(|matter| matter.code == code)
        .map(|matter| matter.repository_url)
}

/// Seed the sample matters every fixture login lands on, with a participant
/// for each firm and client tier so the same projects can be opened through
/// every lens the KIND Rauthy fixture signs in as — including Owner, who
/// carries a firm-side row so they appear in an Owner's participation-scoped
/// `/app/projects` list.
///
/// Idempotent.
async fn seed_role_matrix_sample(
    surreal: &SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    report: &mut SeedReport,
) -> anyhow::Result<()> {
    let nevada = jurisdictions::find_by_name(surreal, "Nevada")
        .await?
        .ok_or_else(|| anyhow::anyhow!("seed: jurisdiction `Nevada` must be seeded first"))?;

    // One person per tier, matching the KIND Rauthy fixture's five accounts.
    let owner_id = ensure_dev_person(
        surreal,
        report,
        "Olive Owner",
        "owner@neonlaw.com",
        crate::persons::Role::Owner,
    )
    .await?;
    // The Admin Person exists so the fixture account can sign in; it is
    // deliberately given no participation on any of these matters (see below),
    // so its id is not bound.
    ensure_dev_person(
        surreal,
        report,
        "Ada Admin",
        "admin@neonlaw.com",
        crate::persons::Role::Admin,
    )
    .await?;
    let lawyer_id = ensure_dev_person(
        surreal,
        report,
        "Lawrence Lawyer",
        "lawyer@neonlaw.com",
        crate::persons::Role::Lawyer,
    )
    .await?;
    let clerk_id = ensure_dev_person(
        surreal,
        report,
        "Clara Clerk",
        "clerk@neonlaw.com",
        crate::persons::Role::Clerk,
    )
    .await?;
    let client_id = ensure_dev_person(
        surreal,
        report,
        "Cleo Client",
        "client@neonlaw.com",
        crate::persons::Role::Client,
    )
    .await?;

    // The lawyer DRI is the deployment's bootstrap owner — the one identity a
    // deployment is guaranteed to have, created on first login when nothing else
    // is seeded. A local tier resolves it to the fixture Owner, so the
    // accountable side of a sample matter stays an account a developer can
    // sign in as.
    let dri_email = bootstrap_owner_email();
    let dri_id = if dri_email.eq_ignore_ascii_case("owner@neonlaw.com") {
        owner_id
    } else {
        ensure_dev_person(
            surreal,
            report,
            "Navigator Owner",
            &dri_email,
            crate::persons::Role::Owner,
        )
        .await?
    };

    let cast = FixtureCast {
        owner: owner_id,
        lawyer: lawyer_id,
        clerk: clerk_id,
        client: client_id,
        dri: dri_id,
    };
    for matter in SAMPLE_MATTERS {
        open_sample_matter(surreal, storage, report, matter, &cast, nevada.id).await?;
    }
    Ok(())
}

/// The four fixture accounts that get a participation row on every sample
/// matter, resolved once.
///
/// Admin is deliberately absent, and its absence is the point — see
/// [`open_sample_matter`]. Grouping the four into one value is what keeps the
/// per-matter half a readable argument list rather than seven positional ids.
struct FixtureCast {
    owner: Uuid,
    lawyer: Uuid,
    clerk: Uuid,
    client: Uuid,
    /// The lawyer DRI on every sample matter: the deployment's bootstrap
    /// owner. Equal to `owner` when that identity *is* the fixture Owner, which
    /// is the local default.
    dri: Uuid,
}

/// Open one sample matter: its client Entity, its Project, its repository
/// pointer, its participation rows, its two DRIs, the records its declared
/// pages read (with its staged pages and Drafts, when staged).
///
/// Split from [`seed_role_matrix_sample`] because the two halves answer
/// different questions — who the fixture accounts are, and what one matter is —
/// and only the second one repeats.
async fn open_sample_matter(
    surreal: &SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    report: &mut SeedReport,
    matter: &SampleMatter,
    cast: &FixtureCast,
    jurisdiction_id: Uuid,
) -> anyhow::Result<()> {
    let FixtureCast {
        owner: owner_id,
        lawyer: lawyer_id,
        clerk: clerk_id,
        client: client_id,
        dri: dri_id,
    } = *cast;
    {
        let entity_type =
            crate::entity_types::find_by_name(surreal, matter.client_kind.entity_type())
                .await?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "seed: entity_type `{}` must be seeded first",
                        matter.client_kind.entity_type()
                    )
                })?;
        let entity_id = ensure_dev_entity(
            surreal,
            report,
            matter.client_entity,
            entity_type.id,
            jurisdiction_id,
        )
        .await?;
        let project_id = ensure_dev_project(
            surreal,
            report,
            matter.code,
            matter.name,
            entity_id,
            matter.description,
            matter.brand,
        )
        .await?;

        crate::projects::set_repository_url(surreal, project_id, Some(matter.repository_url))
            .await?;

        // Client side and firm side. The disclosed lawyer is what lets the
        // supervised Clerk resolve the matter.
        ensure_participation(surreal, report, project_id, client_id, "client").await?;
        ensure_participation(surreal, report, project_id, lawyer_id, "lawyer").await?;
        ensure_participation(surreal, report, project_id, clerk_id, "clerk").await?;
        // Owner gets a firm-side row so the demo matters appear in the Owner's
        // own list: since ENG-81 the whole matter surface — `/app/projects` and
        // `/app/projects/{code}` alike — is participation-scoped for every tier,
        // with no privileged bypass.
        ensure_participation(surreal, report, project_id, owner_id, "owner").await?;
        // The DRI needs a participation row of its own: the matter surface is
        // participation-scoped for every tier, so accountability without
        // participation would name a DRI who cannot open the matter.
        if dri_id != owner_id {
            ensure_participation(surreal, report, project_id, dri_id, "owner").await?;
        }
        // Admin deliberately gets **no** row. These matters are what an
        // administrator who was never assigned to a matter looks like, which is
        // the ENG-81 decision made visible: privileged reach is a place you
        // navigate to (`/app/admin`, which reads and writes the participation
        // ledger), not a silent widening of a shared route. Because one row
        // gates both the list and the detail view, this is also why none of
        // these appear in an admin's `/app/projects` list — the absence is the
        // point, not an oversight.
        // The lawyer DRI is the deployment's bootstrap owner — the one identity
        // a deployment is guaranteed to have, created on first login when
        // nothing else is seeded. Naming it rather than the fixture lawyer keeps
        // the accountable side of every sample matter pointing at whoever
        // actually runs the deployment.
        crate::projects::designate_dri_in_surreal(
            surreal,
            project_id,
            dri_id,
            crate::projects::DriSide::Lawyer,
        )
        .await?;
        crate::projects::designate_dri_in_surreal(
            surreal,
            project_id,
            client_id,
            crate::projects::DriSide::Client,
        )
        .await?;

        // The rows each declared page reads, and — when a source is staged —
        // the pages and Drafts themselves.
        let project = crate::projects::find_by_id(surreal, project_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("seed: Project `{}` vanished", matter.code))?;
        sample_records::seed(
            surreal,
            storage,
            &project,
            lawyer_id,
            jurisdiction_id,
            |key| std::env::var(key).ok(),
        )
        .await?;
    }
    Ok(())
}

async fn ensure_dev_person(
    surreal: &SurrealDb,
    report: &mut SeedReport,
    name: &str,
    email: &str,
    role: crate::persons::Role,
) -> anyhow::Result<Uuid> {
    let existing = crate::persons::find_by_email_ci(surreal, email).await?;
    let row = crate::persons::find_or_create(
        surreal,
        &crate::persons::NewPerson::with_role(name, email, role),
    )
    .await?;
    if existing.is_none() {
        report.persons_inserted += 1;
        return Ok(row.id);
    }
    if row.name != name || row.role != role {
        crate::persons::edit(
            surreal,
            row.id,
            &crate::persons::PersonEdit {
                name: Some(name.into()),
                role: Some(role),
                ..crate::persons::PersonEdit::default()
            },
        )
        .await?;
        report.persons_updated += 1;
    }
    Ok(row.id)
}

/// Idempotently ensure one sample matter's client Entity, of whatever type
/// that matter's client is. The type is a parameter rather than fixed to
/// `Human` because the fixture carries a company client as well as two people,
/// and a company engaging outside counsel is a different Entity shape from an
/// individual plaintiff.
async fn ensure_dev_entity(
    surreal: &SurrealDb,
    report: &mut SeedReport,
    name: &str,
    entity_type_id: Uuid,
    jurisdiction_id: Uuid,
) -> anyhow::Result<Uuid> {
    if let Some(row) = crate::entities::find_by_name_and_type(surreal, name, entity_type_id).await?
    {
        // Same repair as `seed_entities`: a persisted row's
        // `jurisdiction_id` may name a jurisdiction the reset local
        // engine no longer holds, and the engine never validated it.
        if row.jurisdiction_id != jurisdiction_id {
            crate::entities::repoint_jurisdiction(surreal, row.id, jurisdiction_id).await?;
        }
        return Ok(row.id);
    }
    let row = crate::entities::create(surreal, &new_entity(name, entity_type_id, jurisdiction_id))
        .await?;
    report.entities_inserted += 1;
    Ok(row.id)
}

/// The write shape every seeded Entity shares.
///
/// The firm's own row is the reason this is a helper rather than a
/// literal: it must carry `firm_anchor_key`, or the seeded firm would be
/// forkable through `entity_commands` — the guard reads the key, and a
/// row seeded without one leaves the `entity_firm_anchor` index free.
/// The shipped defaults stand in for the configured anchor here because
/// `is_firm_anchor` protects them under every configuration, and the
/// canonical seed ships exactly the firm anchors [`FIRM_ENTITY_NAMES`]
/// names.
fn new_entity(
    name: &str,
    entity_type_id: Uuid,
    jurisdiction_id: Uuid,
) -> crate::entities::NewEntity {
    crate::entities::NewEntity {
        name: name.to_string(),
        entity_type_id,
        jurisdiction_id,
        phone: None,
        url: None,
        xero_id: None,
        firm_anchor_key: FIRM_ENTITY_NAMES
            .iter()
            .find_map(|configured| crate::entity_commands::firm_anchor_key(configured, name)),
    }
}

async fn ensure_dev_project(
    surreal: &SurrealDb,
    report: &mut SeedReport,
    code: &str,
    name: &str,
    entity_id: Uuid,
    description: &str,
    brand: &str,
) -> anyhow::Result<Uuid> {
    let input = crate::projects::NewProject {
        code: code.to_string(),
        name: name.to_string(),
        status: "open".to_string(),
        brand: brand.to_string(),
        entity_id,
        firm_id: None,
        // A dev matter's governing jurisdiction is a lawyer's call, so the
        // seed leaves it unset rather than asserting one.
        jurisdiction_id: None,
        description: Some(description.to_string()),
    };
    let row = match crate::projects::find_by_code(surreal, code).await? {
        Some(existing) => crate::projects::upsert_with_id(surreal, existing.id, &input).await?,
        None => crate::projects::find_or_create_by_code(surreal, Uuid::now_v7(), &input).await?,
    };
    report.projects_inserted += 1;
    Ok(row.id)
}

/// Idempotently record one person's participation on a project.
async fn ensure_participation(
    surreal: &SurrealDb,
    report: &mut SeedReport,
    project_id: Uuid,
    person_id: Uuid,
    participation: &str,
) -> anyhow::Result<()> {
    if let Some(row) =
        crate::projects::participation_for_person(surreal, person_id, project_id).await?
    {
        // Restore the seeded participation if a persisted database drifted it:
        // the client/lawyer visibility this fixture demonstrates depends on the
        // exact value, so a stale `paralegal` or blank row must be reconciled.
        if row.participation != participation {
            crate::projects::update_participation(surreal, row.id, person_id, participation)
                .await?;
        }
        return Ok(());
    }
    ensure_participation_in_surreal(surreal, project_id, person_id, participation).await?;
    report.person_project_roles_inserted += 1;
    Ok(())
}

/// Reconcile one participation row in the Surreal ledger. Split out because
/// the memory-backed local engine resets with its pod, so this must
/// converge whether the row is already there or not.
async fn ensure_participation_in_surreal(
    surreal: &SurrealDb,
    project_id: Uuid,
    person_id: Uuid,
    participation: &str,
) -> anyhow::Result<()> {
    match crate::projects::participation_for_person(surreal, person_id, project_id).await? {
        Some(existing) if existing.participation == participation => Ok(()),
        Some(existing) => {
            crate::projects::update_participation(surreal, existing.id, person_id, participation)
                .await?;
            Ok(())
        }
        None => {
            match crate::projects::add_participation(surreal, project_id, person_id, participation)
                .await
            {
                Ok(_) => Ok(()),
                // `person_project_role_pair` is UNIQUE, so a concurrent seed
                // may have filed this pair between the read above and this
                // write. Losing that race is not an error — adopt the winner's
                // row and reconcile its value, the same settle-on-one-row
                // contract `find_or_create_by_code` gives the project itself.
                Err(crate::projects::ProjectStoreError::Db(error))
                    if crate::surreal::retry::unique_violation(&error)
                        == Some("person_project_role_pair") =>
                {
                    if let Some(existing) =
                        crate::projects::participation_for_person(surreal, person_id, project_id)
                            .await?
                    {
                        if existing.participation != participation {
                            crate::projects::update_participation(
                                surreal,
                                existing.id,
                                person_id,
                                participation,
                            )
                            .await?;
                        }
                    }
                    Ok(())
                }
                Err(err) => Err(err.into()),
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct TemplateFrontmatter {
    #[serde(default)]
    code: Option<String>,
    title: String,
    respondent_type: String,
    /// forms-registry code of the government form this template fills
    /// (`form: nv__llc_formation`); absent for Typst-rendered
    /// templates.
    #[serde(default)]
    form: Option<String>,
    /// Declared notation kind (`retainer`/`letter`/`filing`) from the
    /// `kind:` key; `None` until declared.
    #[serde(default)]
    kind: Option<String>,
}

/// Template codes in the shared seeded catalog, derived from its file names.
pub fn seeded_template_codes() -> anyhow::Result<Vec<String>> {
    SEEDED_TEMPLATES
        .iter()
        .map(|template| template_code(template.label).map(str::to_string))
        .collect()
}

/// The `code@version` Clause sources the seeded shared Template `code` pins,
/// when `stored_body` is exactly the body [`seed_canonical`] stores for it.
///
/// The seed stores a Template's body without its frontmatter, so the
/// `clause_sources:` that [`rules::clauses::compile`] wrote there are not in
/// the stored bytes. They are recovered from the bundled source this binary
/// seeds. A body that is not byte-identical to that seed's (an older version,
/// or a code the seed does not ship) yields `None`.
#[must_use]
pub fn seeded_clause_sources(code: &str, stored_body: &str) -> Option<Vec<String>> {
    let template = SEEDED_TEMPLATES
        .iter()
        .find(|template| template_code(template.label).is_ok_and(|seeded| seeded == code))?;
    let composed = rules::clauses::compile(template.markdown).ok()?;
    let (_, body) = split_template(&composed)?;
    (normalized_body_bytes(body).as_ref() == stored_body.as_bytes())
        .then(|| rules::clauses::pinned_sources(&composed))
}

fn template_code(label: &str) -> anyhow::Result<&str> {
    std::path::Path::new(label)
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|stem| !stem.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{label}: template path has no filename stem"))
}

/// Split a notation template's markdown into `(frontmatter, body)`.
/// The frontmatter is the YAML between the opening and closing `---`
/// markers; the body is everything after.
///
/// Accepts both LF and CRLF line endings, mirroring
/// `rules::frontmatter::extract`. The two parsers exist for different
/// call paths — that one validates other repositories' files, this one
/// splits the templates bundled into this binary — but they read the
/// same delimiters and must agree about them.
///
/// The CRLF arm is not hypothetical. These templates reach the parser
/// through `include_str!`, which reads the working tree at compile
/// time, so the line endings of whichever checkout compiled the binary
/// are baked in with no runtime opportunity to normalise. Git for
/// Windows defaults to `core.autocrlf=true` and the tree carries no
/// `.gitattributes`, so a Windows checkout materialises every template
/// with `\r\n`; probing only for `\n` made this return `None` for all
/// of them, and both callers turn `None` into a hard error on the boot
/// path.
///
/// The returned slices are borrowed and so may retain interior `\r`.
/// `serde_yaml` accepts that in the frontmatter. For the body, see the
/// normalisation note at the ingest call in `seed_templates`.
fn split_template(md: &str) -> Option<(&str, &str)> {
    let after_open = md
        .strip_prefix("---\n")
        .or_else(|| md.strip_prefix("---\r\n"))?;

    // Empty frontmatter: the closer immediately follows the opener.
    if let Some(body) = after_open
        .strip_prefix("---\n")
        .or_else(|| after_open.strip_prefix("---\r\n"))
    {
        return Some(("", body));
    }
    if after_open == "---" || after_open == "---\r" {
        return Some(("", ""));
    }

    if let Some((end, delim_len)) = find_closer(after_open) {
        return Some((&after_open[..end], &after_open[end + delim_len..]));
    }

    // Closer at EOF with no trailing newline, so no body follows.
    let fm = after_open
        .strip_suffix("\r\n---")
        .or_else(|| after_open.strip_suffix("\n---"))?;
    Some((fm, ""))
}

/// Byte offset of the closing `---` delimiter line within the text
/// following the opener, paired with that delimiter's own length so the
/// caller can slice the body that starts after it.
///
/// Takes the earlier of the two matches, so a file with mixed endings
/// closes at the first real delimiter rather than the first *LF* one.
/// `"\n---\n"` cannot match inside `"\r\n---\r\n"`, so the two probes
/// never overlap on a single delimiter and the earlier offset is always
/// a genuine closer.
fn find_closer(after_open: &str) -> Option<(usize, usize)> {
    const LF: &str = "\n---\n";
    const CRLF: &str = "\r\n---\r\n";
    match (after_open.find(LF), after_open.find(CRLF)) {
        (Some(lf), Some(crlf)) => {
            if lf <= crlf {
                Some((lf, LF.len()))
            } else {
                Some((crlf, CRLF.len()))
            }
        }
        (Some(lf), None) => Some((lf, LF.len())),
        (None, Some(crlf)) => Some((crlf, CRLF.len())),
        (None, None) => None,
    }
}

/// The exact bytes of a template body that get content-addressed, with
/// leading whitespace trimmed and line endings normalised to LF.
///
/// The normalisation is load-bearing rather than tidiness. These bytes
/// decide the `asset_id`, and `templates::save_version` is immutable by
/// policy: a body whose bytes differ from the stored version appends a
/// new current version and retires the prior one. Because `include_str!`
/// bakes in the compiling checkout's line endings, hashing them raw
/// would make a template's identity depend on which platform built the
/// binary — the same logical document would fork into two versions, and
/// a Notation pinned to one would resolve to bytes the other tier never
/// writes.
///
/// This is a no-op for every deployment that exists: images are built on
/// Linux, where these bodies are already LF, so the bytes are unchanged
/// and no new version is written. What it does is keep that true if an
/// image is ever built from a CRLF checkout. The borrowed arm means the
/// LF path stays allocation-free and provably identical to the bytes
/// hashed before this function existed.
///
/// Only the body is normalised. The frontmatter slice is left as
/// `split_template` returned it, interior `\r` and all, because
/// `serde_yaml` accepts that and it is the parsed values that get
/// stored, not the raw slice.
fn normalized_body_bytes(body: &str) -> std::borrow::Cow<'_, [u8]> {
    let trimmed = body.trim_start();
    if trimmed.contains('\r') {
        std::borrow::Cow::Owned(trimmed.replace("\r\n", "\n").into_bytes())
    } else {
        std::borrow::Cow::Borrowed(trimmed.as_bytes())
    }
}

/// Seed the workspace-bundled notation templates into the
/// `templates` table. Idempotent on `code` — re-running is a
/// no-op. The full shipped catalog is bundled; add more
/// `include_str!` entries in `canonical` above and a row here to
/// extend.
#[allow(clippy::too_many_lines)]
async fn seed_templates(
    surreal: &SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    report: &mut SeedReport,
) -> anyhow::Result<()> {
    for template in SEEDED_TEMPLATES {
        let label = template.label;
        let composed = rules::clauses::compile(template.markdown).map_err(anyhow::Error::msg)?;
        let md = composed.as_str();
        let (fm_str, body) = split_template(md)
            .ok_or_else(|| anyhow::anyhow!("{label}: missing YAML frontmatter"))?;
        let fm: TemplateFrontmatter = serde_yaml::from_str(fm_str)
            .map_err(|e| anyhow::anyhow!("{label}: parse frontmatter: {e}"))?;
        let code = template_code(label)?;
        if fm.code.as_deref().is_some_and(|declared| declared != code) {
            tracing::warn!(
                template = label,
                declared_code = fm.code.as_deref().unwrap_or_default(),
                filename_code = code,
                "Template code frontmatter does not match the filename"
            );
        }

        // The body lives in a content-addressed asset; ingest it (sha
        // dedup) and reference it by `asset_id`. See
        // `normalized_body_bytes` for why the line endings are normalised
        // before hashing.
        let body_bytes = normalized_body_bytes(body);
        let asset_id =
            crate::assets::ingest_content(surreal, storage, &body_bytes, "text/markdown")
                .await
                .map_err(|e| anyhow::anyhow!("{label}: ingest body asset: {e}"))?;

        // Immutable by policy: a fresh cluster writes the first version;
        // an unchanged re-seed is a no-op; a changed body/form/title
        // appends a new current version and retires the prior one, so a
        // Notation already opened against the old bytes keeps resolving to
        // them (`notation.template_id` pins the version).
        let saved = crate::templates::save_version(
            surreal,
            None,
            code,
            crate::templates::Version {
                title: fm.title,
                respondent_type: fm.respondent_type,
                asset_id: Some(asset_id),
                form_code: fm.form,
                kind: fm.kind,
                // The seeded workspace catalog comes from bundled files,
                // not a git repo — no commit provenance.
                source_commit_sha: None,
            },
        )
        .await?;
        if saved.was_written() {
            report.templates_inserted += 1;
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct CredentialRec {
    person: PersonEmailRef,
    jurisdiction: JurisdictionRef,
    license_number: String,
}

async fn seed_credentials(surreal: &SurrealDb, report: &mut SeedReport) -> anyhow::Result<()> {
    for rec in parse::<CredentialRec>(canonical::CREDENTIAL, "Credential.yaml")? {
        let Some(p) = crate::persons::find_by_email_ci(surreal, &rec.person.email).await? else {
            continue;
        };
        let Some(j) = jurisdictions::find_by_name(surreal, &rec.jurisdiction.name).await? else {
            continue;
        };
        // Find-or-grant rather than read-then-write: the seed runs on
        // every boot, and two boots racing would otherwise both miss the
        // read and collide on `credential_person_jurisdiction`.
        let before = crate::credentials::find_by_person_and_jurisdiction(surreal, p.id, j.id)
            .await?
            .is_some();
        crate::credentials::find_or_grant(surreal, p.id, j.id, &rec.license_number).await?;
        if !before {
            report.credentials_inserted += 1;
        }
    }
    Ok(())
}

// ---------- Per-entity loaders ----------

#[derive(Debug, Deserialize)]
struct JurisdictionRec {
    name: String,
    code: String,
    jurisdiction_type: String,
}

/// Materialize the authored firm glossary into `glossary_term` rows — in
/// SurrealDB, where the table lives since its slice of #1093 (ENG-20).
///
/// Reference data, so it sits in the canonical seed beside jurisdictions:
/// environment-blind, identical in every deployment, and idempotent — the
/// write is keyed on slug, so an edited definition converges rather than
/// appending a second row. It inserts nothing matter-scoped, which is why
/// it is safe in production.
async fn seed_glossary_terms(surreal: &SurrealDb, report: &mut SeedReport) -> anyhow::Result<()> {
    report.glossary_terms_written +=
        crate::glossary::materialize(surreal, crate::glossary::terms()).await?;
    Ok(())
}

/// Seed the jurisdiction reference table — into SurrealDB, where the
/// table lives since its slice of #1093 (ENG-20). Canonical seed data:
/// it runs identically in every environment, production included, and
/// is idempotent on `code`.
async fn seed_jurisdictions(surreal: &SurrealDb, report: &mut SeedReport) -> anyhow::Result<()> {
    for rec in parse::<JurisdictionRec>(canonical::JURISDICTION, "Jurisdiction.yaml")? {
        if jurisdictions::find_by_code(surreal, &rec.code)
            .await?
            .is_some()
        {
            continue;
        }
        match jurisdictions::create(
            surreal,
            &NewJurisdiction::new(rec.name, rec.code, rec.jurisdiction_type),
        )
        .await
        {
            Ok(_) => report.jurisdictions_inserted += 1,
            // A concurrent boot won the `jurisdiction_code` unique index
            // between the check and the insert; the row exists, which is
            // all the seed wants.
            Err(jurisdictions::JurisdictionError::CodeTaken) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct EntityTypeRec {
    name: String,
}

/// Seed the entity-type reference table — into SurrealDB, where the
/// table lives since its slice of #1093 (ENG-20). Canonical seed data:
/// it runs identically in every environment, production included, and
/// is idempotent on `name` — `find_or_create` absorbs a concurrent
/// boot's winning write.
async fn seed_entity_types(surreal: &SurrealDb, report: &mut SeedReport) -> anyhow::Result<()> {
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for rec in parse::<EntityTypeRec>(canonical::ENTITY_TYPE, "EntityType.yaml")? {
        if !seen.insert(rec.name.clone()) {
            continue;
        }
        if crate::entity_types::find_by_name(surreal, &rec.name)
            .await?
            .is_some()
        {
            continue;
        }
        crate::entity_types::find_or_create(surreal, &rec.name).await?;
        report.entity_types_inserted += 1;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntityRec {
    name: String,
    entity_type: EntityTypeRef,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntityTypeRef {
    name: String,
    #[serde(default)]
    jurisdiction: Option<JurisdictionRef>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JurisdictionRef {
    name: String,
}

/// The jurisdiction name a seed record's nested `entity_type.jurisdiction`
/// declares. No default: an Entity's jurisdiction is a fact about where it
/// (or, for a `Human` entity, the person it represents) is domiciled, so a
/// document that omits it is refused rather than silently resolved to the
/// firm's own state (ENG-518 — a production seed once wrote a client's
/// jurisdiction as `Nevada` this way, silently and wrongly).
fn declared_jurisdiction_name<'a>(
    entity_name: &str,
    entity_type: &'a EntityTypeRef,
) -> anyhow::Result<&'a str> {
    entity_type
        .jurisdiction
        .as_ref()
        .map(|jurisdiction| jurisdiction.name.as_str())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "entity {entity_name:?} declares no jurisdiction — add `jurisdiction: \
                 {{ name: <Jurisdiction> }}` under its `entity_type:` block"
            )
        })
}

async fn seed_entities(
    surreal: &SurrealDb,
    yaml: &str,
    path: &str,
    report: &mut SeedReport,
) -> anyhow::Result<()> {
    // Rows the seed already owns whose `firm_anchor_key` disagrees with
    // the firm-anchor names the seed now ships. Setting is deferred to a
    // second pass so every stale key is surrendered before the new one is
    // claimed: the `entity_firm_anchor` index is UNIQUE, and a rename
    // between two names the seed both ships would otherwise be refused by
    // the row it is moving away from.
    let mut claim_anchor: Vec<(Uuid, String)> = Vec::new();
    for rec in parse::<EntityRec>(yaml, path)? {
        let et = crate::entity_types::find_by_name(surreal, &rec.entity_type.name)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Entity.yaml references unknown entity_type {name:?}",
                    name = rec.entity_type.name
                )
            })?;
        let jurisdiction_name = declared_jurisdiction_name(&rec.name, &rec.entity_type)?;
        let jur = jurisdictions::find_by_name(surreal, jurisdiction_name)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("Entity.yaml references unknown jurisdiction {jurisdiction_name:?}")
            })?;
        if let Some(row) = crate::entities::find_by_name_and_type(surreal, &rec.name, et.id).await?
        {
            // A persisted database can outlive the memory-backed local
            // engine, whose re-seeded jurisdictions carry fresh ids, and
            // the engine never validated the link. Repoint so the
            // reference resolves again instead of dangling.
            if row.jurisdiction_id != jur.id {
                crate::entities::repoint_jurisdiction(surreal, row.id, jur.id).await?;
            }
            // Nothing above rewrites an existing row, so moving the
            // configured anchor would strand `firm_anchor_key` on the
            // outgoing firm and never mint it on the incoming one — and
            // that column, not the name, is what `delete_unless_firm_anchor`
            // reads. Reconcile the row to the name it should carry now.
            let expected = FIRM_ENTITY_NAMES.iter().find_map(|configured| {
                crate::entity_commands::firm_anchor_key(configured, &rec.name)
            });
            if row.firm_anchor_key != expected {
                match expected {
                    Some(key) => claim_anchor.push((row.id, key)),
                    None => {
                        crate::entities::set_firm_anchor_key(surreal, row.id, None).await?;
                    }
                }
            }
            continue;
        }
        match crate::entities::create(surreal, &new_entity(&rec.name, et.id, jur.id)).await {
            Ok(_) => report.entities_inserted += 1,
            // The firm's own row is the one fixture two seeds can race for,
            // because it is the only one that takes `firm_anchor_key` — and
            // the UNIQUE index is what makes the loser lose. The cucumber
            // suites run scenarios concurrently against one shared engine and
            // each re-seeds this fixture, so losing that race has to be
            // "someone else already did it", not a failed seed.
            Err(crate::entities::EntityError::FirmAnchorTaken) => {}
            Err(error) => return Err(error.into()),
        }
    }
    for (id, key) in claim_anchor {
        // Losing the key to a concurrent seed is the same outcome the create
        // arm absorbs: another pass already moved the anchor onto this
        // identity, so this one has nothing left to do.
        match crate::entities::set_firm_anchor_key(surreal, id, Some(key)).await {
            Ok(_) | Err(crate::entities::EntityError::FirmAnchorTaken) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// The firm's earlier names, newest first. `Shook Law PLLC` predates the
/// California professional corporation (only a PC may practise law in
/// California); `Shook Law PC` is that corporation before it was renamed
/// [`FIRM_ENTITY_NAME`].
const LEGACY_FIRM_ENTITY_NAMES: &[&str] = &["Shook Law PC", "Shook Law PLLC"];

/// Move a database that still carries a legacy firm row onto the PC.
///
/// The seed finds rows by name and type, so without this a persisted
/// database would keep the old row holding `firm_anchor_key` and the new
/// row could never be minted. Renaming in place keeps the row's id, so
/// every Firm, Address, and Matter that points at it stays pointed.
/// The anchor is surrendered first because `entities::update` refuses to
/// rename a row that holds it. Idempotent: a no-op once no legacy row is
/// left, and when the PC row already exists.
async fn rename_legacy_firm_entity(surreal: &SurrealDb) -> anyhow::Result<()> {
    let mut legacy = None;
    for name in LEGACY_FIRM_ENTITY_NAMES {
        legacy = crate::entities::find_by_name(surreal, name).await?;
        if legacy.is_some() {
            break;
        }
    }
    let Some(legacy) = legacy else {
        return Ok(());
    };
    if crate::entities::find_by_name(surreal, FIRM_ENTITY_NAME)
        .await?
        .is_some()
    {
        return Ok(());
    }
    let pc = crate::entity_types::find_by_name(surreal, "Professional Corporation")
        .await?
        .ok_or_else(|| anyhow::anyhow!("seed: entity_type `Professional Corporation` missing"))?;
    let california = jurisdictions::find_by_name(surreal, "California")
        .await?
        .ok_or_else(|| anyhow::anyhow!("seed: jurisdiction `California` missing"))?;
    crate::entities::set_firm_anchor_key(surreal, legacy.id, None).await?;
    let renamed = crate::entities::update(
        surreal,
        legacy.id,
        &crate::entities::NewEntity {
            name: FIRM_ENTITY_NAME.to_string(),
            entity_type_id: pc.id,
            jurisdiction_id: california.id,
            phone: legacy.phone,
            url: legacy.url,
            xero_id: legacy.xero_id,
            firm_anchor_key: crate::entity_commands::firm_anchor_key(
                FIRM_ENTITY_NAME,
                FIRM_ENTITY_NAME,
            ),
        },
    )
    .await?;
    anyhow::ensure!(
        renamed.is_some(),
        "seed: renaming {} to {FIRM_ENTITY_NAME} wrote no row",
        legacy.name
    );
    Ok(())
}

/// One compiled house brand, as `store` has to hold it.
///
/// `store` cannot depend on `views`, so each row copies four compiled
/// facts: the key (`views::brand::BrandKey::as_str`), the published
/// wordmark (`SiteBrand::site_name`, which the footer's "Our Family" list
/// renders from the seeded row), the typeface id
/// (`BrandKey::default_typeface`), and the primary hex for a light directory canvas
/// (usually `BrandKey::default_palette().light.primary`). `webapp::firm_footer`'s
/// `every_compiled_brand_seeds_its_published_wordmark` holds the wordmark
/// against `SiteBrand::site_name`. `webapp/tests/brand_seed_presentation.rs`
/// holds the typeface id and primary hex against the compiled presentation.
/// Aegis seeds a darker gold for readable directory cards while its site uses yellow on black.
struct CompiledBrand {
    key: &'static str,
    name: &'static str,
    typeface: &'static str,
    primary_hex: &'static str,
    /// The brand's production website, `https://` plus its compiled
    /// `BrandKey::canonical_host()`. Its host is the brand's approved email
    /// domain ([`crate::brand_domains`]).
    website_url: &'static str,
}

/// Every compiled house brand this deployment migrates into a system-wide
/// `brand` row, in [`crate::firms::CLOSED_BRAND_KEYS`] order.
///
/// THE TWO LISTS ARE ONE FACT. `seed_practice` attaches every
/// `CLOSED_BRAND_KEYS` entry to the firm, and `attach_brand` validates each
/// one against the `brand` rows THIS table writes — so a key in that list
/// with no row here fails the boot seed outright, and the pod crash-loops on
/// `unknown brand key <key>` before it ever serves a request. That is how
/// the five practice brands shipped: `CLOSED_BRAND_KEYS` grew and this table
/// did not. `compiled_brands_cover_every_closed_brand_key` holds them
/// together now.
const COMPILED_BRANDS: &[CompiledBrand] = &[
    CompiledBrand {
        key: "neon",
        website_url: "https://www.neonlaw.com",
        name: "Neon Law",
        typeface: "gorp-serif",
        primary_hex: "#007c91",
    },
    CompiledBrand {
        key: "delete-your-data",
        website_url: "https://www.deleteyourdata.com",
        name: "DeleteYourData.com",
        typeface: "plus-jakarta-sans",
        primary_hex: "#b91c1c",
    },
    CompiledBrand {
        key: "delete-your-debt",
        website_url: "https://www.deleteyourdebt.com",
        name: "DeleteYourDebt.com",
        typeface: "public-sans",
        primary_hex: "#1F6F4A",
    },
    CompiledBrand {
        key: "vesta",
        website_url: "https://www.vestaestateplanning.com",
        name: "Vesta Estate Planning",
        typeface: "eb-garamond",
        primary_hex: "#8A5A2B",
    },
    CompiledBrand {
        key: "misericordia",
        website_url: "https://www.misericordialaw.com",
        name: "Misericordia Injury Law",
        typeface: "source-sans-3",
        primary_hex: "#7A1F2B",
    },
    CompiledBrand {
        key: "abhaya",
        website_url: "https://www.abhayaimmigration.com",
        name: "Abhaya Immigration",
        typeface: "mukta",
        primary_hex: "#1F4E79",
    },
    CompiledBrand {
        key: "lawyer-shook",
        website_url: "https://www.lawyershook.com",
        name: "Lawyer Shook",
        typeface: "tinos",
        primary_hex: "#5c5100",
    },
    // The public Summons Defense wordmark is a brand of Guieb & Shook PC, so
    // this row copies the public name rendered by `SUMMONS_BRANDING`.
    CompiledBrand {
        key: "summons",
        website_url: "https://www.summonsdefense.nyc",
        name: "Summons Defense",
        typeface: "libre-franklin",
        primary_hex: "#2F4550",
    },
    CompiledBrand {
        key: "daybridge",
        website_url: "https://www.daybridgedivorce.com",
        name: "Daybridge Divorce Law",
        typeface: "source-serif-4",
        primary_hex: "#255C67",
    },
    CompiledBrand {
        key: "death-and-divorce",
        website_url: "https://www.deathanddivorcelaw.com",
        name: "Death & Divorce",
        typeface: "pirata-one",
        primary_hex: "#B45535",
    },
    CompiledBrand {
        key: "cyber-injury-law",
        website_url: "https://www.cyberinjurylaw.com",
        name: "CyberInjuryLaw",
        typeface: "dm-sans",
        primary_hex: "#446300",
    },
    CompiledBrand {
        key: "aegis",
        website_url: "https://www.aegiscriminaldefense.com",
        name: "Aegis Criminal Defense",
        typeface: "dm-sans",
        // Directory cards use a light canvas; the compiled site palette uses yellow on black.
        primary_hex: "#756300",
    },
    CompiledBrand {
        key: "groundwork",
        website_url: "https://www.groundworkrealestatelaw.com",
        name: "Groundwork Real Estate Law",
        typeface: "source-serif-4",
        primary_hex: "#8C252B",
    },
    CompiledBrand {
        key: "guieb-shook",
        website_url: "https://www.guiebandshook.com",
        name: "Guieb & Shook",
        typeface: "eb-garamond",
        primary_hex: "#1f3a5f",
    },
];

/// The second firm's compiled brands: Lex wears only `iep-defenders`,
/// whose row is firm-scoped to Lex from the moment it is first seeded.
/// The key joins [`crate::firms::CLOSED_BRAND_KEYS`] for registry-ordered
/// footer display and the website-URL backfill, but no practice-firm
/// attachment — the brand belongs to Lex Tecnica LLP alone. Its row copies
/// the compiled presentation: `BrandKey::IepDefenders`
/// `default_typeface` and `default_palette().light.primary`.
const LEX_COMPILED_BRANDS: &[CompiledBrand] = &[CompiledBrand {
    key: "iep-defenders",
    website_url: "https://www.iepdefenders.com",
    name: "IEP Defenders",
    typeface: "source-sans-3",
    primary_hex: "#5B21B6",
}];

/// The website URL [`COMPILED_BRANDS`] seeds for `key`, if it names a
/// compiled brand. Public for the same reason as [`compiled_brand_name`]: a
/// crate that sees `views` holds it against `BrandKey::canonical_host`.
#[must_use]
pub fn compiled_brand_website_url(key: &str) -> Option<&'static str> {
    COMPILED_BRANDS
        .iter()
        .chain(LEX_COMPILED_BRANDS)
        .find(|brand| brand.key == key)
        .map(|brand| brand.website_url)
}

/// The published wordmark [`COMPILED_BRANDS`] seeds for `key`, if it names a
/// compiled brand. Public so a crate that can see `views` — which `store`
/// cannot — holds this copy against the compiled `SiteBrand::site_name` the
/// footer renders when no `brand` row answers.
#[must_use]
pub fn compiled_brand_name(key: &str) -> Option<&'static str> {
    COMPILED_BRANDS
        .iter()
        .chain(LEX_COMPILED_BRANDS)
        .find(|brand| brand.key == key)
        .map(|brand| brand.name)
}

/// Migrate every compiled house brand into a `brand` row scoped to `firm`
/// (ENG-659: `store::brands::create` refuses `firm_id: None`, so every
/// compiled brand is Firm-scoped from the moment it is first seeded — there
/// is no system-wide row created here any more), with the identity values
/// their compiled `Branding` entries carry. `store` cannot depend on
/// `views`, so those values — including each palette's light-mode primary
/// hex (ENG-586: `primary_color` holds a validated hex, not a palette id) —
/// are copied into [`COMPILED_BRANDS`] rather than read from it. The
/// compiled brands' real presentation still renders from the existing
/// static stylesheet path (`views::brand_presentation`'s own compiled
/// `PALETTE`/`TYPEFACES`), not from these columns; this migration keeps the
/// row's stored hex in step with that compiled palette so nothing renders
/// differently on upgrade (`views::brand_presentation`'s own test asserts
/// every compiled palette clears the same WCAG AA gate this write
/// enforces).
///
/// Goes through [`crate::brands::seed_upsert`] rather than the authorized
/// `create`/`update`: this is trusted system seeding, not a caller-driven
/// request, and a test engine may already carry the same compiled keys
/// under an unrelated fixture Firm (`store::surreal::test_support::mem`),
/// which the authorized pair would correctly refuse to let this Firm's own
/// Admin DRI edit. Idempotent: an existing name or key is this migration
/// having already run, and only its typeface/colour are refreshed.
async fn seed_brands(
    surreal: &SurrealDb,
    firm: &crate::firms::Firm,
    brands: &[CompiledBrand],
) -> anyhow::Result<()> {
    for brand in brands {
        crate::brands::seed_upsert(
            surreal,
            &crate::brands::NewBrand {
                name: brand.name.to_string(),
                key: brand.key.to_string(),
                firm_id: Some(firm.id),
                typeface: Some(brand.typeface.to_string()),
                primary_color: Some(brand.primary_hex.to_string()),
                ..crate::brands::NewBrand::default()
            },
            Some(brand.website_url),
        )
        .await?;
    }
    Ok(())
}

/// Find or create the one practice this deployment already is: the `Shook
/// Law PC` entity's Firm, with its own Admin DRI. Idempotent: an existing
/// Firm on this Entity is left exactly as it is, Admin DRI included — this
/// only chooses [`PRIMARY_ADMIN_EMAIL`] for the *first* creation (ENG-499).
async fn ensure_practice_firm(surreal: &SurrealDb) -> anyhow::Result<crate::firms::Firm> {
    let entity = crate::entities::find_by_name(surreal, FIRM_ENTITY_NAME)
        .await?
        .ok_or_else(|| anyhow::anyhow!("canonical seed is missing {FIRM_ENTITY_NAME}"))?;
    let admin_dri = crate::persons::find_by_email_ci(surreal, PRIMARY_ADMIN_EMAIL)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("canonical seed is missing the primary admin {PRIMARY_ADMIN_EMAIL}")
        })?
        .id;
    match crate::firms::find_by_entity_id(surreal, entity.id).await? {
        Some(firm) => Ok(firm),
        None => match crate::firms::create(
            surreal,
            &crate::firms::NewFirm {
                name: FIRM_ENTITY_NAME.to_string(),
                status: "active".to_string(),
                entity_id: entity.id,
                admin_dri_person_id: admin_dri,
            },
        )
        .await
        {
            Ok(firm) => Ok(firm),
            Err(crate::firms::FirmError::DuplicateEntity) => {
                crate::firms::find_by_entity_id(surreal, entity.id)
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!("firm for {FIRM_ENTITY_NAME} exists but did not read")
                    })
            }
            Err(error) => Err(error.into()),
        },
    }
}

/// Find or create the deployment's second firm: the `Lex Tecnica LLP`
/// entity's Firm, wearing only the `iep-defenders` brand. Idempotent on
/// the same terms as [`ensure_practice_firm`] — an existing Firm on this
/// Entity is left exactly as it is. Its Admin DRI is the same primary
/// admin: the canonical seed ships exactly one person of record, and
/// both firms' administration starts there.
async fn ensure_lex_firm(surreal: &SurrealDb) -> anyhow::Result<crate::firms::Firm> {
    let entity = crate::entities::find_by_name(surreal, LEX_FIRM_ENTITY_NAME)
        .await?
        .ok_or_else(|| anyhow::anyhow!("canonical seed is missing {LEX_FIRM_ENTITY_NAME}"))?;
    let admin_dri = crate::persons::find_by_email_ci(surreal, PRIMARY_ADMIN_EMAIL)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("canonical seed is missing the primary admin {PRIMARY_ADMIN_EMAIL}")
        })?
        .id;
    match crate::firms::find_by_entity_id(surreal, entity.id).await? {
        Some(firm) => Ok(firm),
        None => match crate::firms::create(
            surreal,
            &crate::firms::NewFirm {
                name: LEX_FIRM_NAME.to_string(),
                status: "active".to_string(),
                entity_id: entity.id,
                admin_dri_person_id: admin_dri,
            },
        )
        .await
        {
            Ok(firm) => Ok(firm),
            Err(crate::firms::FirmError::DuplicateEntity) => {
                crate::firms::find_by_entity_id(surreal, entity.id)
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!("firm for {LEX_FIRM_ENTITY_NAME} exists but did not read")
                    })
            }
            Err(error) => Err(error.into()),
        },
    }
}

/// The compiled brand keys the canonical practice firm wears: every closed
/// key except the ones the Lex firm owns. `seed_practice` attaches exactly
/// this set to the practice firm, and the seed tests assert against it
/// rather than re-deriving the two-firm split.
#[must_use]
pub fn practice_worn_brand_keys() -> Vec<&'static str> {
    crate::firms::CLOSED_BRAND_KEYS
        .iter()
        .copied()
        .filter(|key| !LEX_COMPILED_BRANDS.iter().any(|brand| brand.key == *key))
        .collect()
}

/// The one practice this deployment already is, wearing every registered
/// house-brand key the Lex firm does not own. Resolves or creates the Firm
/// first (ENG-659: every compiled brand must be created *as* that Firm's
/// own row, so the Firm has to exist before [`seed_brands`] runs), then
/// attaches each key through `firm_brand`.
async fn seed_practice(surreal: &SurrealDb) -> anyhow::Result<()> {
    let firm = ensure_practice_firm(surreal).await?;
    seed_brands(surreal, &firm, COMPILED_BRANDS).await?;
    for key in practice_worn_brand_keys() {
        match crate::firms::ensure_brand(surreal, firm.id, key).await {
            Ok(()) | Err(crate::firms::FirmError::DuplicateBrand) => {}
            Err(error) => return Err(error.into()),
        }
    }
    // The whole point of brand mobility is that the same key could wear
    // either firm at runtime, so the seed proves both shapes exist.
    let lex = ensure_lex_firm(surreal).await?;
    seed_brands(surreal, &lex, LEX_COMPILED_BRANDS).await?;
    for brand in LEX_COMPILED_BRANDS {
        match crate::firms::ensure_brand(surreal, lex.id, brand.key).await {
            Ok(()) | Err(crate::firms::FirmError::DuplicateBrand) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Grant `person_firm_role` to every Admin, Lawyer, and Clerk against the
/// seeded practice. Owner stays the system-wide tier and gets no row.
async fn seed_firm_memberships(surreal: &SurrealDb) -> anyhow::Result<()> {
    let entity = crate::entities::find_by_name(surreal, FIRM_ENTITY_NAME)
        .await?
        .ok_or_else(|| anyhow::anyhow!("canonical seed is missing {FIRM_ENTITY_NAME}"))?;
    let Some(firm) = crate::firms::find_by_entity_id(surreal, entity.id).await? else {
        return Ok(());
    };
    for person in crate::persons::list_directory(surreal, "", "", &[]).await? {
        let Some(membership) = crate::firms::FirmMembership::for_role(person.role) else {
            continue;
        };
        crate::firms::ensure_membership(
            surreal,
            &crate::firms::NewPersonFirmRole {
                person_id: person.id,
                firm_id: firm.id,
                membership,
                is_dri: membership == crate::firms::FirmMembership::Admin,
            },
        )
        .await?;
    }
    Ok(())
}

/// Give unowned Projects to their brand's Firm, falling back to the seeded
/// practice for brands no Firm wears.
async fn seed_project_firm_ownership(surreal: &SurrealDb) -> anyhow::Result<()> {
    let entity = crate::entities::find_by_name(surreal, FIRM_ENTITY_NAME)
        .await?
        .ok_or_else(|| anyhow::anyhow!("canonical seed is missing {FIRM_ENTITY_NAME}"))?;
    let Some(firm) = crate::firms::find_by_entity_id(surreal, entity.id).await? else {
        return Ok(());
    };
    crate::firms::backfill_unowned_projects(surreal, Some(firm.id)).await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersonRec {
    email: String,
    name: String,
    #[serde(default)]
    profile_image_url: Option<String>,
    /// The address this Person used before `email`. A Person who answers to
    /// it is updated to `email`, not duplicated (LAW-169).
    #[serde(default)]
    previous_email: Option<String>,
    /// Further addresses the same Person answers to, kept as aliases.
    #[serde(default)]
    emails: Vec<String>,
}

impl PersonRec {
    /// Every address that may already belong to this Person, `email` first.
    fn candidate_emails(&self) -> Vec<&str> {
        std::iter::once(self.email.as_str())
            .chain(self.previous_email.as_deref())
            .chain(self.emails.iter().map(String::as_str))
            .collect()
    }

    /// Addresses this record wants kept as aliases beside `primary`, lowercased.
    fn wanted_aliases(&self, primary: &str, old_primary: Option<&str>) -> Vec<String> {
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
}

/// The one Person any of `rec`'s addresses belongs to, primary or alias.
/// Addresses split across two Persons are refused: they are one human the
/// directory holds twice, and `navigator site person merge` is the fix.
async fn find_person_for_record(
    surreal: &SurrealDb,
    rec: &PersonRec,
) -> anyhow::Result<Option<crate::persons::Person>> {
    let mut found: Vec<crate::persons::Person> = Vec::new();
    for email in rec.candidate_emails() {
        if let Some(person) = crate::persons::find_by_any_email(surreal, email).await? {
            if !found.iter().any(|known| known.id == person.id) {
                found.push(person);
            }
        }
    }
    if found.len() > 1 {
        anyhow::bail!(
            "the addresses for `{}` belong to {} different Persons ({}); merge them first with \
             `navigator site person merge <keep> <drop>`",
            rec.email,
            found.len(),
            found
                .iter()
                .map(|person| person.email.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(found.pop())
}

/// Record every wanted alias on `person_id`.
async fn record_aliases(
    surreal: &SurrealDb,
    person_id: Uuid,
    wanted: &[String],
) -> anyhow::Result<()> {
    for email in wanted {
        crate::persons::add_alias(surreal, person_id, email)
            .await
            .map_err(|error| anyhow::anyhow!("alias `{email}`: {error}"))?;
    }
    Ok(())
}

async fn seed_persons(surreal: &SurrealDb, report: &mut SeedReport) -> anyhow::Result<()> {
    for rec in parse::<PersonRec>(canonical::PERSON, "Person.yaml")? {
        let email = rec.email.clone();
        let before = crate::persons::find_by_email_ci(surreal, &email).await?;
        crate::persons::find_or_create(
            surreal,
            &crate::persons::NewPerson {
                profile_image_url: rec.profile_image_url,
                ..crate::persons::NewPerson::new(rec.name, rec.email)
            },
        )
        .await?;
        if before.is_none() {
            report.persons_inserted += 1;
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct TestimonialRec {
    project: ProjectCodenameRef,
    person: PersonEmailRef,
    #[serde(default)]
    quote: String,
    #[serde(default)]
    attribution_label: Option<String>,
    #[serde(default)]
    consented_at: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    display_order: i32,
}

async fn seed_testimonials(surreal: &SurrealDb, report: &mut SeedReport) -> anyhow::Result<()> {
    for rec in parse::<TestimonialRec>(canonical::TESTIMONIAL, "Testimonial.yaml")? {
        let Some(project) = crate::projects::find_by_name(surreal, &rec.project.codename).await?
        else {
            continue;
        };
        let Some(person) = crate::persons::find_by_email_ci(surreal, &rec.person.email).await?
        else {
            continue;
        };
        crate::testimonials::find_or_create(
            surreal,
            &crate::testimonials::NewTestimonial {
                project_id: project.id,
                person_id: person.id,
                quote: &rec.quote,
                attribution_label: rec.attribution_label,
                consented_at: rec.consented_at,
                published_at: rec.published_at,
                display_order: rec.display_order,
            },
        )
        .await?;
        report.testimonials_inserted += 1;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct UserRec {
    person: PersonEmailRef,
    role: String,
}

#[derive(Debug, Deserialize)]
struct PersonEmailRef {
    email: String,
}

/// Firm-domain convention for seeded organization roles: any `owner`, `admin`,
/// or `clerk` row must use a **lowercase** `@neonlaw.com` email. `lawyer` is a
/// licensed-lawyer tier, not an employment or email-domain assertion, so an
/// outside lawyer's Lawyer seed may use their own domain. The
/// lowercase requirement is exact-match: the seed is the canonical
/// source of truth, so it stores one spelling rather than relying on
/// readers to normalize. Lookups themselves are case-insensitive
/// (`store::persons::find_by_email_ci`, backed by the
/// `persons_email_lower_key` unique index), so mixed-case input
/// resolves correctly — this rule keeps the seed data itself tidy.
/// See `docs/access-model.md`.
fn require_firm_domain(email: &str, role: crate::persons::Role) -> anyhow::Result<()> {
    use crate::persons::Role;
    if !matches!(role, Role::Owner | Role::Admin | Role::Clerk) {
        return Ok(());
    }
    if email != email.to_ascii_lowercase() {
        anyhow::bail!(
            "User.yaml: {role:?} seed for {email:?} must be lowercase \
             (see docs/access-model.md)",
        );
    }
    if !email.ends_with("@neonlaw.com") {
        anyhow::bail!(
            "User.yaml: {role:?} seed for {email:?} violates the firm-domain \
             convention — owner/admin/clerk records must use an @neonlaw.com email \
             (see docs/access-model.md)",
        );
    }
    Ok(())
}

/// User.yaml carries a `role` per person; the `users` table doesn't
/// exist as its own entity here — the system-wide tier lives on
/// `persons.role`. Resolve each user record by email, parse the role
/// token, and update the row if the requested tier is higher than
/// what's already stored. The ladder is owner > admin > lawyer > clerk > client.
async fn seed_user_roles(surreal: &SurrealDb, report: &mut SeedReport) -> anyhow::Result<()> {
    use crate::persons::Role;

    fn parse_role_token(s: &str) -> Role {
        match s {
            "owner" => Role::Owner,
            "admin" => Role::Admin,
            "lawyer" => Role::Lawyer,
            "clerk" => Role::Clerk,
            _ => Role::Client,
        }
    }
    for rec in parse::<UserRec>(canonical::USER, "User.yaml")? {
        let requested = parse_role_token(&rec.role);
        require_firm_domain(&rec.person.email, requested)?;
        let Some(p) = crate::persons::find_by_email_ci(surreal, &rec.person.email).await? else {
            continue;
        };
        if p.role.authority_rank() >= requested.authority_rank() {
            continue;
        }
        crate::persons::set_role(surreal, p.id, requested).await?;
        report.persons_updated += 1;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct GitRepoRec {
    repository_name: String,
}

/// Seed the tracked-repository provenance rows — into SurrealDB, where
/// the table lives since its slice of #1093 (ENG-20). Idempotent on
/// `remote_hash`; `find_or_create` absorbs a concurrent boot's winning
/// write.
async fn seed_git_repositories(surreal: &SurrealDb, report: &mut SeedReport) -> anyhow::Result<()> {
    for rec in parse::<GitRepoRec>(sample_seeds::GIT_REPOSITORY, "sample/GitRepository.yaml")? {
        let remote_hash = remote_hash(&rec.repository_name);
        if crate::git_repositories::find_by_remote_hash(surreal, &remote_hash)
            .await?
            .is_some()
        {
            continue;
        }
        crate::git_repositories::find_or_create(surreal, &remote_hash, &"0".repeat(40)).await?;
        report.git_repositories_inserted += 1;
    }
    Ok(())
}

fn remote_hash(name: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(name.as_bytes());
    h.finalize().iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[derive(Debug, Deserialize)]
struct QuestionRec {
    code: String,
    prompt: String,
    #[serde(default)]
    question_type: Option<String>,
    /// `lawyer` | `client` | `both` — which side of the intake sees this
    /// question. Defaults `both` when the YAML omits it.
    #[serde(default)]
    audience: Option<String>,
    // `help_text` exists in the YAML but the schema has no column for it —
    // dropped here, and read back from the canonical YAML by
    // [`question_help_text`] at the point a surface renders the question.
}

async fn seed_questions(surreal: &SurrealDb, report: &mut SeedReport) -> anyhow::Result<()> {
    for rec in parse::<QuestionRec>(canonical::QUESTION, "Question.yaml")? {
        // `find_or_create` rather than read-then-insert: the cucumber
        // suites run scenarios concurrently against one shared engine, so a
        // seeder that assumed exclusivity would lose the race and surface
        // `CodeTaken`. It also keeps the second dev boot a no-op.
        let existed = crate::questions::find_by_code(surreal, &rec.code)
            .await?
            .is_some();
        let row = crate::questions::find_or_create(
            surreal,
            &crate::questions::NewQuestion::new(
                rec.code.clone(),
                rec.prompt,
                rec.question_type.unwrap_or_else(|| "string".into()),
            )
            .with_audience(
                rec.audience
                    .clone()
                    .unwrap_or_else(|| crate::questions::AUDIENCE_BOTH.to_string()),
            ),
        )
        .await?;
        if !existed {
            report.questions_inserted += 1;
        }
        // A declared audience is canonical: a row seeded before the YAML
        // narrowed it is brought in line, since a notation's `audiences:`
        // may only speak for its own `custom_*` questions (N129).
        if let Some(audience) = rec.audience.filter(|a| *a != row.audience) {
            crate::questions::set_audience(surreal, &rec.code, &audience).await?;
        }
    }
    Ok(())
}

/// A question's canonical definition narrowed to its `code` and the
/// presentational `help_text:` field the schema has no column for. This is
/// the slice of `Question.yaml` the [`question_help_text`] reader needs;
/// every other field (prompt, audience, …) is ignored.
#[derive(Debug, Deserialize)]
struct QuestionPresentationRec {
    code: String,
    #[serde(default)]
    help_text: Option<String>,
}

/// The attorney-authored guidance for a question — the `help_text:` of
/// its canonical `Question.yaml` definition. Returns `None` for an
/// unknown code, and for a question whose `help_text:` is absent or
/// blank, so a caller can distinguish "no guidance" from empty prose.
///
/// Help text is authored in the question's canonical seed definition, but with no column on the
/// `question` table — presentational, and so dropped at seed time (see
/// [`QuestionRec`]). Reading it back here rather than from the row is
/// what puts it in front of a caller at all, and keeps the guidance a
/// surface shows the same bytes the seed defines.
///
/// The `__role` suffix a questionnaire state carries (`person__trustee`)
/// is stripped first, so every role that reuses a question inherits that
/// question's guidance.
#[must_use]
pub fn question_help_text(code: &str) -> Option<String> {
    let code = code.split_once("__").map_or(code, |(prefix, _)| prefix);
    serde_yaml::from_str::<Records<QuestionPresentationRec>>(canonical::QUESTION)
        .ok()?
        .records
        .into_iter()
        .find(|r| r.code == code)
        .and_then(|r| r.help_text)
        .filter(|h| !h.trim().is_empty())
}

#[derive(Debug, Deserialize)]
struct MailroomRec {
    name: String,
}

/// `mailrooms.address_id` is NOT NULL; the YAML carries no separate
/// address for the mailroom itself. We synthesize a placeholder
/// address per mailroom so the column is satisfied — flagged with a
/// `(via mailroom)` line1 so it's obvious in row dumps. That placeholder
/// is the row the `address` schema's missing XOR assert exists for: it
/// names neither a person nor an entity.
async fn seed_mailrooms(
    surreal: &SurrealDb,
    yaml: &str,
    path: &str,
    report: &mut SeedReport,
) -> anyhow::Result<()> {
    for rec in parse::<MailroomRec>(yaml, path)? {
        if crate::mailrooms::find_by_name(surreal, &rec.name)
            .await?
            .is_some()
        {
            continue;
        }
        let addr = crate::addresses::create(
            surreal,
            &crate::addresses::NewAddress {
                line1: format!("(via mailroom: {})", rec.name),
                ..crate::addresses::NewAddress::default()
            },
        )
        .await?;
        // Find-or-create, not create: the cucumber suites seed
        // concurrently against one shared engine, so the read above can
        // miss a mailroom another scenario is creating right now.
        let created = crate::mailrooms::find_or_create(surreal, &rec.name, addr.id).await?;
        if created.address_id == addr.id {
            report.mailrooms_inserted += 1;
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct AddressRec {
    entity: EntityNameRef,
    street: String,
    city: String,
    state: String,
    country: String,
    zip: String,
}

#[derive(Debug, Deserialize)]
struct EntityNameRef {
    name: String,
}

/// Both `person_id` and `entity_id` are real `record<>` links since the
/// entities cluster ported (ENG-120), so this step is single-engine.
async fn seed_addresses(
    surreal: &SurrealDb,
    brand: BrandSeed,
    report: &mut SeedReport,
) -> anyhow::Result<()> {
    let Some((yaml, path)) = brand.addresses() else {
        return Ok(());
    };
    for rec in parse::<AddressRec>(yaml, path)? {
        let Some(ent) = crate::entities::find_by_name(surreal, &rec.entity.name).await? else {
            continue;
        };
        let (_, created) = crate::addresses::find_or_create_for_entity(
            surreal,
            &crate::addresses::NewAddress {
                entity_id: Some(ent.id),
                line1: rec.street,
                city: rec.city,
                region: rec.state,
                postal_code: rec.zip,
                country: rec.country,
                ..crate::addresses::NewAddress::default()
            },
        )
        .await?;
        if created {
            report.addresses_inserted += 1;
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SocialHandleRec {
    brand: String,
    provider: String,
    handle: String,
}

async fn seed_social_handles(
    surreal: &SurrealDb,
    yaml: &str,
    path: &str,
    report: &mut SeedReport,
) -> anyhow::Result<()> {
    for rec in parse::<SocialHandleRec>(yaml, path)? {
        if crate::seed::compiled_brand_name(&rec.brand).is_none() {
            anyhow::bail!("{path}: `{}` is not a registered Brand", rec.brand);
        }
        crate::social_handles::set(surreal, &rec.brand, &rec.provider, &rec.handle)
            .await
            .map_err(|error| anyhow::anyhow!("{path}: {error}"))?;
        report.social_handles_written += 1;
    }
    Ok(())
}

/// A provider link a seeded Post declares. Its handle is the Brand's recorded
/// one, so the seed names only what is not yet known: both fields are nullable.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PostSocialRec {
    post_id: Option<String>,
    permalink: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PostRec {
    brand: String,
    slug: String,
    title: String,
    description: String,
    status: String,
    publish_at: String,
    #[serde(default)]
    video_asset: Option<String>,
    body_markdown: String,
    #[serde(default)]
    social: std::collections::BTreeMap<String, Option<PostSocialRec>>,
}

/// The Markdown body of every Post the brand seeds apply. The asset publish
/// gate reads it, because a seeded Post's images are published media a
/// deployment must hold even though no Markdown file in the content tree
/// names them.
///
/// # Errors
///
/// A seed document that does not parse.
pub fn seeded_post_markdown() -> anyhow::Result<Vec<String>> {
    let mut bodies = Vec::new();
    for brand in [BrandSeed::Neon, BrandSeed::Tenant] {
        if let Some((yaml, path)) = brand.posts() {
            bodies.extend(
                parse::<PostRec>(yaml, path)?
                    .into_iter()
                    .map(|rec| rec.body_markdown),
            );
        }
    }
    Ok(bodies)
}

async fn seed_posts(
    surreal: &SurrealDb,
    yaml: &str,
    path: &str,
    report: &mut SeedReport,
) -> anyhow::Result<()> {
    for rec in parse::<PostRec>(yaml, path)? {
        let id = format!("{path}: {}/{}", rec.brand, rec.slug);
        let publish_at = chrono::DateTime::parse_from_rfc3339(&rec.publish_at)
            .map_err(|_| anyhow::anyhow!("{id}: publish_at must be RFC 3339 with an offset"))?
            .with_timezone(&chrono::Utc);
        let mut links = Vec::new();
        for (provider, link) in &rec.social {
            let handle = crate::social_handles::find(surreal, &rec.brand, provider)
                .await?
                .ok_or_else(|| {
                    anyhow::anyhow!("{id}: the Brand has no {provider} handle in SocialHandle.yaml")
                })?;
            let link = link.as_ref();
            links.push(crate::posts::SocialLink {
                provider: provider.clone(),
                account: handle.handle,
                post_id: link.and_then(|link| link.post_id.clone()),
                permalink: link.and_then(|link| link.permalink.clone()),
            });
        }
        let change = crate::posts::seed_post(
            surreal,
            &crate::posts::SourcePost {
                brand_key: &rec.brand,
                slug: &rec.slug,
                title: &rec.title,
                description: &rec.description,
                body_html: "",
                body_markdown: Some(&rec.body_markdown),
                status: &rec.status,
                publish_at,
                video_asset: rec.video_asset.as_deref(),
                vertical_video: None,
                tiktok_ai_generated: None,
                captions: crate::posts::SourceCaptions::default(),
                accounts: &[],
                social_links: &links,
            },
            chrono::Utc::now(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("{id}: {error}"))?;
        if change == crate::posts::ImportChange::Created {
            report.posts_inserted += 1;
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct LetterRec {
    subject: String,
    sender: String,
    mailroom: MailroomNameRef,
}

#[derive(Debug, Deserialize)]
struct MailroomNameRef {
    name: String,
}

async fn seed_letters(surreal: &SurrealDb, report: &mut SeedReport) -> anyhow::Result<()> {
    for rec in parse::<LetterRec>(sample_seeds::LETTER, "sample/Letter.yaml")? {
        let Some(mr) = crate::mailrooms::find_by_name(surreal, &rec.mailroom.name).await? else {
            continue;
        };
        if crate::letters::find_by_mailroom_sender_summary(
            surreal,
            mr.id,
            &rec.sender,
            &rec.subject,
        )
        .await?
        .is_some()
        {
            continue;
        }
        crate::letters::record(
            surreal,
            &crate::letters::NewLetter {
                mailroom_id: mr.id,
                // The canonical seed is firm-wide reference data, not tied to
                // any one matter (ENG-310).
                project_id: None,
                direction: crate::letters::DIRECTION_INCOMING.to_string(),
                sender: rec.sender,
                recipient: rec.mailroom.name.clone(),
                summary: rec.subject,
            },
        )
        .await?;
        report.letters_inserted += 1;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct AnswerRec {
    question_code: String,
    person_email: String,
    value: String,
}

async fn seed_answers(surreal: &SurrealDb, report: &mut SeedReport) -> anyhow::Result<()> {
    for rec in parse::<AnswerRec>(sample_seeds::ANSWER, "sample/Answer.yaml")? {
        let Some(q) = crate::questions::find_by_code(surreal, &rec.question_code).await? else {
            continue;
        };
        let Some(p) = crate::persons::find_by_email_ci(surreal, &rec.person_email).await? else {
            continue;
        };
        let value = crate::answers::primitive(&rec.value);
        // Idempotent on the lookup fields (question, person, value) so a
        // second dev boot inserts zero duplicates. These fixtures are
        // person-scoped and carry no Notation, so there is no state to key
        // on — the value itself is the natural key.
        if crate::answers::exists_with_value(surreal, q.id, p.id, &value).await? {
            continue;
        }
        crate::answers::record(surreal, &crate::answers::NewAnswer::new(q.id, p.id, value)).await?;
        report.answers_inserted += 1;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct PersonEntityRoleRec {
    person: PersonEmailRef,
    entity: EntityNameRef,
    role: String,
}

async fn seed_person_entity_roles(
    surreal: &SurrealDb,
    report: &mut SeedReport,
) -> anyhow::Result<()> {
    for rec in parse::<PersonEntityRoleRec>(canonical::PERSON_ENTITY_ROLE, "PersonEntityRole.yaml")?
    {
        let Some(p) = crate::persons::find_by_email_ci(surreal, &rec.person.email).await? else {
            continue;
        };
        let Some(e) = crate::entities::find_by_name(surreal, &rec.entity.name).await? else {
            continue;
        };
        // `grant` is find-or-create behind the UNIQUE `entity_role_tie`
        // index, so re-seeding a live database adds nothing and two
        // concurrent seeds settle on one edge rather than racing.
        let existing = crate::entity_roles::find(surreal, p.id, e.id, &rec.role).await?;
        crate::entity_roles::grant(surreal, p.id, e.id, &rec.role).await?;
        if existing.is_none() {
            report.person_entity_roles_inserted += 1;
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct PersonProjectRoleRec {
    person: PersonEmailRef,
    project: ProjectCodenameRef,
    role: String,
}

#[derive(Debug, Deserialize)]
struct ProjectCodenameRef {
    codename: String,
}

async fn seed_person_project_roles(
    surreal: &SurrealDb,
    report: &mut SeedReport,
) -> anyhow::Result<()> {
    for rec in parse::<PersonProjectRoleRec>(
        sample_seeds::PERSON_PROJECT_ROLE,
        "sample/PersonProjectRole.yaml",
    )? {
        let Some(p) = crate::persons::find_by_email_ci(surreal, &rec.person.email).await? else {
            continue;
        };
        let Some(pr) = crate::projects::find_by_name(surreal, &rec.project.codename).await? else {
            continue;
        };
        if crate::projects::participation_for_person(surreal, p.id, pr.id)
            .await?
            .is_some()
        {
            continue;
        }
        crate::projects::add_participation(surreal, pr.id, p.id, &rec.role).await?;
        report.person_project_roles_inserted += 1;
    }
    Ok(())
}

/// The fee basis for a zero-fee engagement — "no fee" is a fee basis like
/// any other, and NRPC 1.5(b) requires it be told to the client in writing.
/// The generic engagement agreement (`onboarding__letter`) states no amount
/// or cadence of its own and leaves the basis to arrive as a custom clause
/// spliced into § I (ENG-146); this is that clause's canonical text (ENG-559).
pub const NO_CHARGE_FEE_CLAUSE_BODY: &str = "The Firm charges no fee for this engagement.";

/// Seed [`NO_CHARGE_FEE_CLAUSE_BODY`] onto `notation_id`, idempotently: a
/// clause already carrying this exact body is reused rather than duplicated,
/// so seeding twice never doubles the fee-basis paragraph the client signs.
///
/// The row this writes is an ordinary `notation_clauses` row
/// ([`crate::notation_clauses`]) — indistinguishable from one a lawyer typed
/// by hand, so it is edited, reordered, or removed the same way at the
/// lawyer-review clause editor.
///
/// # Errors
///
/// Propagates any database error reading or writing the notation's clauses.
pub async fn seed_no_charge_fee_clause(
    surreal: &SurrealDb,
    notation_id: Uuid,
) -> anyhow::Result<Uuid> {
    let existing = crate::notation_clauses::for_notation(surreal, notation_id).await?;
    if let Some(clause) = existing
        .iter()
        .find(|c| c.body_markdown == NO_CHARGE_FEE_CLAUSE_BODY)
    {
        return Ok(clause.id);
    }
    let id = crate::notation_clauses::append(surreal, notation_id, NO_CHARGE_FEE_CLAUSE_BODY, None)
        .await?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn brand_posts_seed_once_and_keep_confirmed_video_links() {
        let db = crate::test_support::mem_surreal().await;
        let (handles, handles_path) = BrandSeed::Neon.social_handles().unwrap();
        let (posts, posts_path) = BrandSeed::Neon.posts().unwrap();
        let mut first = SeedReport::default();
        seed_social_handles(&db, handles, handles_path, &mut first)
            .await
            .unwrap();
        seed_posts(&db, posts, posts_path, &mut first)
            .await
            .unwrap();
        assert_eq!(first.posts_inserted, 15);
        let announcement =
            crate::posts::find_by_brand_slug(&db, "neon", "navigator-is-open-source")
                .await
                .unwrap()
                .expect("the public-source announcement is seeded");
        assert_eq!(announcement.status, "published");
        let mut second = SeedReport::default();
        seed_social_handles(&db, handles, handles_path, &mut second)
            .await
            .unwrap();
        seed_posts(&db, posts, posts_path, &mut second)
            .await
            .unwrap();
        assert_eq!(second.posts_inserted, 0, "a second boot inserts nothing");
        let draft = crate::posts::find_by_brand_slug(&db, "neon", "one-message-several-views")
            .await
            .unwrap()
            .expect("the message architecture draft is seeded");
        assert_eq!(draft.status, "draft");
        let published = crate::posts::published_for_brand(&db, "neon", chrono::Utc::now())
            .await
            .unwrap();
        assert!(published.iter().all(|post| post.id != draft.id));
        let vesta = crate::posts::published_for_brand(&db, "vesta", chrono::Utc::now())
            .await
            .unwrap();
        let expected = [
            ("video-update-2026-10-18", "2026-10-18"),
            ("video-update-2026-10-17", "2026-10-17"),
            ("video-update-2026-10-16", "2026-10-16"),
            ("video-update-2026-10-15", "2026-10-15"),
            ("video-update-2026-10-14", "2026-10-14"),
            ("astrology-forecast-2026-10-13", "2026-10-13"),
            ("the-hardest-part-is-getting-started", "2026-10-12"),
        ];
        assert_eq!(vesta.len(), expected.len() + 2);
        for (post, (slug, asset, caption)) in vesta.iter().take(2).zip([
            (
                "where-should-you-go-in-life-20261020",
                "img/vesta/where-should-you-go-in-life-20261020.mp4",
                "Where should you go in life? #EstatePlanning #YOLO",
            ),
            (
                "video-update-2026-10-19",
                "img/vesta/video-update-20261019.mp4",
                "Happy Monday, another day, another opportunity to have meaningful conversations with our clients. #EstatePlanning",
            ),
        ]) {
            assert_eq!(post.slug, slug);
            assert_eq!(post.video_asset.as_deref(), Some(asset));
            assert_eq!(post.description, caption);
            assert_eq!(post.body_markdown.as_deref().map(str::trim), Some(caption));
            assert!(post.social_links.is_empty());
        }
        for (post, (slug, date)) in vesta.iter().skip(2).zip(expected) {
            assert_eq!(post.slug, slug);
            assert_eq!(
                post.video_asset,
                Some(format!("img/vesta-updates/{date}.mp4"))
            );
            let mut providers: Vec<_> = post
                .social_links
                .iter()
                .map(|link| link.provider.as_str())
                .collect();
            providers.sort_unstable();
            assert_eq!(
                providers,
                ["facebook", "instagram", "tiktok"],
                "{}",
                post.slug
            );
            assert!(post
                .social_links
                .iter()
                .all(|link| link.permalink.is_some()));
        }

        let post = crate::posts::find_by_brand_slug(&db, "neon", "thanks-apple")
            .await
            .unwrap()
            .expect("the first post is seeded");
        assert_eq!(post.title, "Thanks, Apple");
        assert!(post.body_markdown.is_some_and(|md| md.contains("Friends,")));
        assert!(post.social_links.is_empty());
        let handle = crate::social_handles::find(&db, "neon", "x").await.unwrap();
        assert_eq!(handle.map(|h| h.handle).as_deref(), Some("NeonLawUSA"));
    }

    #[tokio::test]
    async fn a_seeded_post_cannot_link_a_provider_the_brand_has_no_handle_on() {
        let db = crate::test_support::mem_surreal().await;
        let yaml = "records:\n  - brand: neon\n    slug: linked\n    title: Linked\n    \
                    description: A post.\n    status: published\n    \
                    publish_at: \"2026-01-02T00:00:00Z\"\n    body_markdown: Body.\n    \
                    social:\n      x: ~\n";
        let mut report = SeedReport::default();
        let error = seed_posts(&db, yaml, "test.yaml", &mut report)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("no x handle"), "{error}");
        crate::social_handles::set(&db, "neon", "x", "NeonLawUSA")
            .await
            .unwrap();
        seed_posts(&db, yaml, "test.yaml", &mut report)
            .await
            .unwrap();
        let post = crate::posts::find_by_brand_slug(&db, "neon", "linked")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(post.social_links.len(), 1);
        assert_eq!(post.social_links[0].account, "NeonLawUSA");
        assert_eq!(post.social_links[0].permalink, None);
    }

    use super::{
        normalized_body_bytes, reconcile_yaml, seed_canonical, seed_no_charge_fee_clause,
        seed_posts, seed_social_handles, seeded_clause_sources, seeded_template_codes,
        split_template, template_code, validate_yaml, BrandSeed, ReconcileAction, ReconcileActor,
        ScopeViolation, SeedModel, SeedReport, TemplateFrontmatter, COMPILED_BRANDS,
        NO_CHARGE_FEE_CLAUSE_BODY, SEEDED_TEMPLATES,
    };
    use crate::jurisdictions::{self, NewJurisdiction};
    use crate::persons::{self, NewPerson, Role};
    use crate::question_registry::QuestionType;
    use crate::test_support::mem_surreal;
    use crate::{entities, entity_types, projects};

    /// Every key a Firm is seeded wearing has a `brand` row to wear.
    ///
    /// `seed_practice` attaches each [`crate::firms::CLOSED_BRAND_KEYS`]
    /// entry through `attach_brand`, which refuses a key no `brand` row
    /// names — so a key missing from [`COMPILED_BRANDS`] fails the boot seed
    /// and crash-loops the pod, which is what five practice brands did in
    /// the release gate. Order is asserted too, because the two lists are
    /// read as one registry.
    #[test]
    fn compiled_brands_cover_every_closed_brand_key() {
        let seeded: Vec<&str> = COMPILED_BRANDS.iter().map(|brand| brand.key).collect();
        assert_eq!(
            seeded,
            crate::firms::CLOSED_BRAND_KEYS.to_vec(),
            "every CLOSED_BRAND_KEYS entry needs a COMPILED_BRANDS row, in the same order"
        );
    }

    /// Each seeded brand carries a distinct name and key, because
    /// `brand.name` and `brand.brand_key` are both UNIQUE indexes: a
    /// repeated value would make the second row's `create` fail as a
    /// duplicate of the first and leave that key unregistered.
    #[test]
    fn compiled_brand_names_and_keys_are_distinct() {
        for (index, brand) in COMPILED_BRANDS.iter().enumerate() {
            for other in &COMPILED_BRANDS[index + 1..] {
                assert_ne!(brand.name, other.name, "two compiled brands share a name");
                assert_ne!(brand.key, other.key, "two compiled brands share a key");
            }
        }
    }

    /// An unrestricted (interactive lawyer) reconcile actor — no session
    /// scope, full authority. The baseline every unscoped test reconciles
    /// under; scope tests below build their own [`ReconcileActor`].
    fn unrestricted_actor() -> ReconcileActor<'static> {
        ReconcileActor {
            bootstrap_owner_email: None,
            actor_role: Role::Owner,
            may_change_roles: true,
            project_scope: None,
            dry_run: false,
            actor_person_id: None,
        }
    }

    /// A filesystem-backed storage at a fixed path so the bytes a seed
    /// writes are readable by a later `templates::body` call in the same
    /// test — blobs are content-addressed, so sharing the dir across
    /// tests is safe (identical bytes dedup).
    async fn fs_storage() -> std::sync::Arc<dyn cloud::StorageService> {
        std::sync::Arc::new(
            cloud::FsStorage::new(std::env::temp_dir().join("navigator-seed-test-storage"))
                .await
                .expect("temp FsStorage"),
        )
    }

    #[tokio::test]
    async fn operator_person_seed_creates_then_overwrites_only_when_requested() {
        let surreal = mem_surreal().await;
        let initial = r"
lookup_fields:
  - email
records:
  - email: operator@example.com
    name: First Name
";
        let changed = r"
lookup_fields:
  - email
records:
  - email: operator@example.com
    name: Updated Name
";

        let created = reconcile_yaml(
            &surreal,
            SeedModel::Person,
            initial,
            "Firm",
            false,
            &unrestricted_actor(),
        )
        .await
        .expect("create from seed");
        assert_eq!(
            (created.created, created.updated, created.unchanged),
            (1, 0, 0)
        );

        let unchanged = reconcile_yaml(
            &surreal,
            SeedModel::Person,
            changed,
            "Firm",
            false,
            &unrestricted_actor(),
        )
        .await
        .expect("default leaves match alone");
        assert_eq!(
            (unchanged.created, unchanged.updated, unchanged.unchanged),
            (0, 0, 1)
        );
        assert_eq!(
            persons::find_by_email_ci(&surreal, "operator@example.com")
                .await
                .unwrap()
                .unwrap()
                .name,
            "First Name"
        );

        let overwritten = reconcile_yaml(
            &surreal,
            SeedModel::Person,
            changed,
            "Firm",
            true,
            &unrestricted_actor(),
        )
        .await
        .expect("overwrite matching record");
        assert_eq!(
            (
                overwritten.created,
                overwritten.updated,
                overwritten.unchanged
            ),
            (0, 1, 0)
        );
        assert_eq!(
            persons::find_by_email_ci(&surreal, "operator@example.com")
                .await
                .unwrap()
                .unwrap()
                .name,
            "Updated Name"
        );
    }

    #[tokio::test]
    async fn operator_person_seed_dry_run_flags_a_new_person_sharing_a_name() {
        let surreal = mem_surreal().await;
        reconcile_yaml(
            &surreal,
            SeedModel::Person,
            &person_yaml("old.firm@example.com", "Pat Attorney"),
            "Firm",
            false,
            &unrestricted_actor(),
        )
        .await
        .expect("seed existing person");

        let moved = "lookup_fields:\n  - email\nrecords:\n  - email: new.firm@example.com\n    name: pat attorney\n  - email: other@example.com\n    name: Someone Else\n";
        let dry = ReconcileActor {
            dry_run: true,
            actor_person_id: None,
            ..unrestricted_actor()
        };
        let report = reconcile_yaml(&surreal, SeedModel::Person, moved, "Firm", false, &dry)
            .await
            .expect("plan person seed");

        assert_eq!(report.records[0].action, ReconcileAction::New);
        assert_eq!(report.records[0].warnings.len(), 1);
        assert!(report.records[0].warnings[0].contains("old.firm@example.com"));
        assert!(report.records[1].warnings.is_empty());

        // A real write is not annotated, and the planned row is not created.
        assert!(persons::find_by_email_ci(&surreal, "new.firm@example.com")
            .await
            .expect("read")
            .is_none());
    }

    fn moved_person_yaml(extra: &str) -> String {
        format!(
            "lookup_fields:\n  - email\nrecords:\n  - email: new.firm@example.com\n    name: Pat Attorney\n{extra}"
        )
    }

    #[tokio::test]
    async fn operator_person_seed_matches_previous_email_so_a_move_updates_one_person() {
        let surreal = mem_surreal().await;
        reconcile_yaml(
            &surreal,
            SeedModel::Person,
            &person_yaml("old.firm@example.com", "Pat Attorney"),
            "Firm",
            false,
            &unrestricted_actor(),
        )
        .await
        .expect("seed existing person");
        let before = persons::find_by_email_ci(&surreal, "old.firm@example.com")
            .await
            .unwrap()
            .unwrap();
        let moved = moved_person_yaml("    previous_email: old.firm@example.com\n");

        let dry = ReconcileActor {
            dry_run: true,
            ..unrestricted_actor()
        };
        let plan = reconcile_yaml(&surreal, SeedModel::Person, &moved, "Firm", true, &dry)
            .await
            .expect("plan the move");
        assert_eq!(plan.records[0].action, ReconcileAction::Changed);
        assert!(plan.records[0]
            .changed_fields
            .contains(&"email".to_string()));
        assert!(plan.records[0].warnings.is_empty());
        assert!(persons::find_by_email_ci(&surreal, "new.firm@example.com")
            .await
            .unwrap()
            .is_none());

        let report = reconcile_yaml(
            &surreal,
            SeedModel::Person,
            &moved,
            "Firm",
            true,
            &unrestricted_actor(),
        )
        .await
        .expect("apply the move");
        assert_eq!(report.created, 0);
        assert_eq!(report.updated, 1);
        let after = persons::find_by_email_ci(&surreal, "new.firm@example.com")
            .await
            .unwrap()
            .expect("the person now answers to the new primary email");
        assert_eq!(after.id, before.id);
        let found = persons::find_by_any_email(&surreal, "old.firm@example.com")
            .await
            .unwrap();
        assert_eq!(found.map(|p| p.id), Some(before.id));

        // Re-running the same document changes nothing.
        let again = reconcile_yaml(
            &surreal,
            SeedModel::Person,
            &moved,
            "Firm",
            true,
            &unrestricted_actor(),
        )
        .await
        .expect("rerun");
        assert_eq!(again.updated, 0);
        assert_eq!(again.unchanged, 1);
    }

    #[tokio::test]
    async fn operator_person_seed_records_listed_emails_as_aliases() {
        let surreal = mem_surreal().await;
        let yaml = moved_person_yaml("    emails:\n      - pat@elsewhere.example\n");
        reconcile_yaml(
            &surreal,
            SeedModel::Person,
            &yaml,
            "Firm",
            false,
            &unrestricted_actor(),
        )
        .await
        .expect("create with an alias");
        let person = persons::find_by_email_ci(&surreal, "new.firm@example.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            persons::aliases(&surreal, person.id).await.unwrap(),
            vec!["pat@elsewhere.example"]
        );
        let found = persons::find_by_any_email(&surreal, "pat@elsewhere.example")
            .await
            .unwrap();
        assert_eq!(found.map(|p| p.id), Some(person.id));
    }

    #[tokio::test]
    async fn operator_person_seed_refuses_addresses_that_belong_to_two_people() {
        let surreal = mem_surreal().await;
        for (email, name) in [("a@example.com", "Pat A"), ("b@example.com", "Pat B")] {
            reconcile_yaml(
                &surreal,
                SeedModel::Person,
                &person_yaml(email, name),
                "Firm",
                false,
                &unrestricted_actor(),
            )
            .await
            .expect("seed");
        }
        let yaml = "lookup_fields:\n  - email\nrecords:\n  - email: a@example.com\n    name: Pat A\n    previous_email: b@example.com\n";
        let error = reconcile_yaml(
            &surreal,
            SeedModel::Person,
            yaml,
            "Firm",
            true,
            &unrestricted_actor(),
        )
        .await
        .expect_err("two people");
        assert!(error.to_string().contains("site person merge"), "{error}");
    }

    #[tokio::test]
    async fn operator_person_seed_dry_run_plans_without_writing() {
        let surreal = mem_surreal().await;
        let existing = person_yaml("existing@example.com", "Original Name");
        reconcile_yaml(
            &surreal,
            SeedModel::Person,
            &existing,
            "Firm",
            false,
            &unrestricted_actor(),
        )
        .await
        .expect("seed existing person");

        let planned = "lookup_fields:\n  - email\nrecords:\n  - email: existing@example.com\n    name: Revised Name\n    profile_image_url: https://example.com/revised.png\n  - email: new@example.com\n    name: New Person\n";
        let report = reconcile_yaml(
            &surreal,
            SeedModel::Person,
            planned,
            "Firm",
            true,
            &ReconcileActor {
                dry_run: true,
                actor_person_id: None,
                ..unrestricted_actor()
            },
        )
        .await
        .expect("plan person seed");

        assert_eq!(
            (report.created, report.updated, report.unchanged),
            (1, 1, 0)
        );
        assert_eq!(report.records[0].key, "existing@example.com");
        assert_eq!(report.records[0].action, ReconcileAction::Changed);
        assert_eq!(
            report.records[0].changed_fields,
            vec!["name", "profile_image_url"]
        );
        assert_eq!(report.records[1].action, ReconcileAction::New);
        assert!(persons::find_by_email_ci(&surreal, "new@example.com")
            .await
            .expect("read planned person")
            .is_none());

        let unchanged = reconcile_yaml(
            &surreal,
            SeedModel::Person,
            &existing,
            "Firm",
            true,
            &ReconcileActor {
                dry_run: true,
                actor_person_id: None,
                ..unrestricted_actor()
            },
        )
        .await
        .expect("plan unchanged person seed");
        assert_eq!(
            (unchanged.created, unchanged.updated, unchanged.unchanged),
            (0, 0, 1)
        );
        assert_eq!(unchanged.records[0].action, ReconcileAction::Unchanged);
    }

    #[tokio::test]
    async fn operator_entity_seed_dry_run_resolves_references_without_writing() {
        let surreal = mem_surreal().await;
        let entity_type = entity_types::create(&surreal, "Corporation")
            .await
            .expect("create entity type");
        let original_jurisdiction =
            jurisdictions::create(&surreal, &NewJurisdiction::new("Nevada", "NV", "state"))
                .await
                .expect("create original jurisdiction");
        let revised_jurisdiction =
            jurisdictions::create(&surreal, &NewJurisdiction::new("California", "CA", "state"))
                .await
                .expect("create revised jurisdiction");
        let entity = entities::create(
            &surreal,
            &entities::NewEntity {
                name: "Acme Example".into(),
                entity_type_id: entity_type.id,
                jurisdiction_id: original_jurisdiction.id,
                phone: None,
                url: None,
                xero_id: None,
                firm_anchor_key: None,
            },
        )
        .await
        .expect("create existing entity");
        let planned = "lookup_fields:\n  - name\n  - entity_type_id\nrecords:\n  - name: Acme Example\n    entity_type:\n      name: Corporation\n      jurisdiction:\n        name: California\n  - name: New Example\n    entity_type:\n      name: Corporation\n      jurisdiction:\n        name: California\n";
        let report = reconcile_yaml(
            &surreal,
            SeedModel::Entity,
            planned,
            "Firm",
            true,
            &ReconcileActor {
                dry_run: true,
                actor_person_id: None,
                ..unrestricted_actor()
            },
        )
        .await
        .expect("plan entity seed");

        assert_eq!(
            (report.created, report.updated, report.unchanged),
            (1, 1, 0)
        );
        assert_eq!(report.records[0].key, "Acme Example / Corporation");
        assert_eq!(report.records[0].action, ReconcileAction::Changed);
        assert_eq!(report.records[0].changed_fields, vec!["jurisdiction"]);
        assert_eq!(report.records[1].action, ReconcileAction::New);
        let persisted = entities::find_by_id(&surreal, entity.id)
            .await
            .expect("read existing entity")
            .expect("existing entity remains");
        assert_eq!(persisted.jurisdiction_id, original_jurisdiction.id);
        assert_ne!(persisted.jurisdiction_id, revised_jurisdiction.id);
        assert!(
            entities::find_by_name_and_type(&surreal, "New Example", entity_type.id)
                .await
                .expect("read planned entity")
                .is_none()
        );
    }

    /// A create names the jurisdiction it resolved, so `--dry-run` (and the
    /// applied run) tells the operator which one an entity landed under
    /// rather than leaving it to be discovered by reading the row back.
    #[tokio::test]
    async fn entity_seed_names_the_jurisdiction_it_resolved() {
        let surreal = mem_surreal().await;
        entity_types::create(&surreal, "Human")
            .await
            .expect("create entity type");
        jurisdictions::create(&surreal, &NewJurisdiction::new("Washington", "WA", "state"))
            .await
            .expect("create jurisdiction");
        let yaml = "lookup_fields:\n  - name\n  - entity_type_id\nrecords:\n  - name: A Client\n    entity_type:\n      name: Human\n      jurisdiction:\n        name: Washington\n";

        let report = reconcile_yaml(
            &surreal,
            SeedModel::Entity,
            yaml,
            "Firm",
            true,
            &ReconcileActor {
                dry_run: true,
                actor_person_id: None,
                ..unrestricted_actor()
            },
        )
        .await
        .expect("plan entity seed");

        assert_eq!(
            report.records[0].jurisdiction.as_deref(),
            Some("Washington")
        );
    }

    /// A record with no declared jurisdiction is refused rather than
    /// silently resolved to the firm's own state — the ENG-518 defect: a
    /// production seed once wrote a client's entity into `Nevada` this way,
    /// with nothing in the output saying so.
    #[tokio::test]
    async fn entity_seed_without_a_declared_jurisdiction_is_refused() {
        let surreal = mem_surreal().await;
        entity_types::create(&surreal, "Human")
            .await
            .expect("create entity type");
        let yaml = "lookup_fields:\n  - name\n  - entity_type_id\nrecords:\n  - name: A Client\n    entity_type:\n      name: Human\n";

        let error = reconcile_yaml(
            &surreal,
            SeedModel::Entity,
            yaml,
            "Firm",
            true,
            &ReconcileActor {
                dry_run: true,
                actor_person_id: None,
                ..unrestricted_actor()
            },
        )
        .await
        .expect_err("a record with no jurisdiction must be refused, not defaulted");
        let message = error.to_string();
        assert!(message.contains("A Client"), "{message}");
        assert!(message.contains("jurisdiction"), "{message}");
        assert!(
            entities::find_by_name(&surreal, "A Client")
                .await
                .expect("read")
                .is_none(),
            "the refused record must not be written"
        );
    }

    async fn project_fixture(surreal: &crate::surreal::SurrealDb, code: &str) -> projects::Project {
        projects::create(
            surreal,
            &projects::NewProject {
                code: code.into(),
                name: code.into(),
                status: "open".into(),
                entity_id: crate::test_support::seed_entity(surreal).await,
                ..Default::default()
            },
        )
        .await
        .expect("create fixture project")
    }

    fn scoped_actor(project_code: &str) -> ReconcileActor<'_> {
        ReconcileActor {
            bootstrap_owner_email: None,
            actor_role: Role::Lawyer,
            may_change_roles: false,
            project_scope: Some(project_code),
            dry_run: false,
            actor_person_id: None,
        }
    }

    fn person_yaml(email: &str, name: &str) -> String {
        format!("lookup_fields:\n  - email\nrecords:\n  - email: {email}\n    name: {name}\n")
    }

    fn downcasts_to<E: std::error::Error + Send + Sync + 'static>(
        error: &anyhow::Error,
        expected: impl Fn(&E) -> bool,
    ) -> bool {
        error.downcast_ref::<E>().is_some_and(expected)
    }

    #[tokio::test]
    async fn scoped_person_create_links_to_the_scoped_project() {
        let surreal = mem_surreal().await;
        let project = project_fixture(&surreal, "acme").await;

        let report = reconcile_yaml(
            &surreal,
            SeedModel::Person,
            &person_yaml("new@example.com", "New Person"),
            "Firm",
            false,
            &scoped_actor("acme"),
        )
        .await
        .expect("scoped create is allowed");
        assert_eq!((report.created, report.updated), (1, 0));

        let person = persons::find_by_email_ci(&surreal, "new@example.com")
            .await
            .unwrap()
            .unwrap();
        assert!(
            projects::participation_for_person(&surreal, person.id, project.id)
                .await
                .unwrap()
                .is_some(),
            "a newly seeded person must be linked to the scope's project"
        );
    }

    #[tokio::test]
    async fn scoped_person_update_is_allowed_when_already_in_the_scoped_project() {
        let surreal = mem_surreal().await;
        let project = project_fixture(&surreal, "acme").await;
        let person = persons::create(&surreal, &NewPerson::new("Old Name", "in@example.com"))
            .await
            .unwrap();
        projects::add_participation(&surreal, project.id, person.id, "client")
            .await
            .unwrap();

        let report = reconcile_yaml(
            &surreal,
            SeedModel::Person,
            &person_yaml("in@example.com", "Renamed"),
            "Firm",
            true,
            &scoped_actor("acme"),
        )
        .await
        .expect("update within the scoped project is allowed");
        assert_eq!((report.created, report.updated), (0, 1));
    }

    #[tokio::test]
    async fn scoped_person_update_refuses_a_record_linked_to_a_different_project() {
        let surreal = mem_surreal().await;
        project_fixture(&surreal, "acme").await;
        let other = project_fixture(&surreal, "widget-works").await;
        let person = persons::create(
            &surreal,
            &NewPerson::new("Elsewhere", "elsewhere@example.com"),
        )
        .await
        .unwrap();
        projects::add_participation(&surreal, other.id, person.id, "client")
            .await
            .unwrap();

        let error = reconcile_yaml(
            &surreal,
            SeedModel::Person,
            &person_yaml("elsewhere@example.com", "Renamed"),
            "Firm",
            true,
            &scoped_actor("acme"),
        )
        .await
        .expect_err("a record linked to another project must be refused");
        assert!(downcasts_to::<ScopeViolation>(&error, |v| matches!(
            v,
            ScopeViolation::CrossProject
        )));
    }

    #[tokio::test]
    async fn scoped_person_update_refuses_an_unlinked_record() {
        let surreal = mem_surreal().await;
        project_fixture(&surreal, "acme").await;
        persons::create(
            &surreal,
            &NewPerson::new("Unlinked", "unlinked@example.com"),
        )
        .await
        .unwrap();

        let error = reconcile_yaml(
            &surreal,
            SeedModel::Person,
            &person_yaml("unlinked@example.com", "Renamed"),
            "Firm",
            true,
            &scoped_actor("acme"),
        )
        .await
        .expect_err("a record with no project link is the ambiguous case and must be refused");
        assert!(downcasts_to::<ScopeViolation>(&error, |v| matches!(
            v,
            ScopeViolation::Unlinked
        )));
    }

    #[tokio::test]
    async fn scoped_reconcile_refuses_a_project_code_naming_no_live_project() {
        let surreal = mem_surreal().await;
        let error = reconcile_yaml(
            &surreal,
            SeedModel::Person,
            &person_yaml("anyone@example.com", "Anyone"),
            "Firm",
            false,
            &scoped_actor("no-such-project"),
        )
        .await
        .expect_err("an unresolvable project code must be refused, not silently no-op");
        assert!(downcasts_to::<ScopeViolation>(&error, |v| matches!(
            v,
            ScopeViolation::UnknownProject(code) if code == "no-such-project"
        )));
    }

    async fn entity_type_and_jurisdiction(
        surreal: &crate::surreal::SurrealDb,
    ) -> (entity_types::EntityType, jurisdictions::Jurisdiction) {
        let entity_type = entity_types::create(surreal, "LLC").await.unwrap();
        let jurisdiction =
            jurisdictions::create(surreal, &NewJurisdiction::new("Nevada", "NV", "state"))
                .await
                .unwrap();
        (entity_type, jurisdiction)
    }

    fn entity_yaml(name: &str) -> String {
        format!(
            "lookup_fields:\n  - name\n  - entity_type_id\nrecords:\n  - name: {name}\n    entity_type:\n      name: LLC\n      jurisdiction:\n        name: Nevada\n"
        )
    }

    #[tokio::test]
    async fn scoped_entity_create_is_refused() {
        let surreal = mem_surreal().await;
        let (entity_type, jurisdiction) = entity_type_and_jurisdiction(&surreal).await;
        // The project's own entity exists, but the seed record names a
        // second, not-yet-created one — creation is what a scoped session
        // can never do (no project-owned link to originate).
        let anchor = entities::create(
            &surreal,
            &entities::NewEntity {
                name: "Acme Holdco".into(),
                entity_type_id: entity_type.id,
                jurisdiction_id: jurisdiction.id,
                phone: None,
                url: None,
                xero_id: None,
                firm_anchor_key: None,
            },
        )
        .await
        .unwrap();
        projects::create(
            &surreal,
            &projects::NewProject {
                code: "acme".into(),
                name: "acme".into(),
                status: "open".into(),
                entity_id: anchor.id,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let error = reconcile_yaml(
            &surreal,
            SeedModel::Entity,
            &entity_yaml("Acme Newco"),
            "Firm",
            false,
            &scoped_actor("acme"),
        )
        .await
        .expect_err("a scoped session cannot create a new entity");
        assert!(downcasts_to::<ScopeViolation>(&error, |v| matches!(
            v,
            ScopeViolation::EntityCreateNotScopable
        )));
    }

    #[tokio::test]
    async fn scoped_entity_update_allowed_only_for_the_projects_own_entity() {
        let surreal = mem_surreal().await;
        let (entity_type, jurisdiction) = entity_type_and_jurisdiction(&surreal).await;
        let acme_entity = entities::create(
            &surreal,
            &entities::NewEntity {
                name: "Acme Holdco".into(),
                entity_type_id: entity_type.id,
                jurisdiction_id: jurisdiction.id,
                phone: None,
                url: None,
                xero_id: None,
                firm_anchor_key: None,
            },
        )
        .await
        .unwrap();
        let other_entity = entities::create(
            &surreal,
            &entities::NewEntity {
                name: "Widget Works".into(),
                entity_type_id: entity_type.id,
                jurisdiction_id: jurisdiction.id,
                phone: None,
                url: None,
                xero_id: None,
                firm_anchor_key: None,
            },
        )
        .await
        .unwrap();
        projects::create(
            &surreal,
            &projects::NewProject {
                code: "acme".into(),
                name: "acme".into(),
                status: "open".into(),
                entity_id: acme_entity.id,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        projects::create(
            &surreal,
            &projects::NewProject {
                code: "widget-works".into(),
                name: "widget-works".into(),
                status: "open".into(),
                entity_id: other_entity.id,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // Updating the scoped project's own entity succeeds.
        let report = reconcile_yaml(
            &surreal,
            SeedModel::Entity,
            &entity_yaml("Acme Holdco"),
            "Firm",
            true,
            &scoped_actor("acme"),
        )
        .await
        .expect("updating the scoped project's own entity is allowed");
        assert_eq!(
            (report.created, report.updated, report.unchanged),
            (0, 0, 1)
        );

        // Updating a different project's entity is refused.
        let error = reconcile_yaml(
            &surreal,
            SeedModel::Entity,
            &entity_yaml("Widget Works"),
            "Firm",
            true,
            &scoped_actor("acme"),
        )
        .await
        .expect_err("a different project's entity must be refused");
        assert!(downcasts_to::<ScopeViolation>(&error, |v| matches!(
            v,
            ScopeViolation::CrossProject
        )));
    }

    fn person_project_role_yaml(email: &str, code: &str) -> String {
        format!(
            "lookup_fields:\n  - person_id\n  - project_id\nrecords:\n  - person:\n      email: {email}\n    project:\n      code: {code}\n"
        )
    }

    fn person_entity_role_yaml(email: &str, entity: &str, role: &str) -> String {
        format!(
            "lookup_fields:\n  - person_id\n  - entity_id\nrecords:\n  - person:\n      email: {email}\n    entity:\n      name: {entity}\n    role: {role}\n"
        )
    }

    fn address_yaml(email: &str, line1: &str, postal_code: &str) -> String {
        format!(
            "lookup_fields:\n  - person_id\n  - line1\n  - postal_code\nrecords:\n  - person:\n      email: {email}\n    line1: {line1}\n    city: Reno\n    region: NV\n    postal_code: {postal_code}\n    country: US\n"
        )
    }

    #[test]
    fn seed_model_parse_accepts_glossary_terms_and_pascal_case_file_stems() {
        assert_eq!(SeedModel::parse("person").unwrap(), SeedModel::Person);
        assert_eq!(
            SeedModel::parse("PersonProjectRole").unwrap(),
            SeedModel::PersonProjectRole
        );
        assert_eq!(
            SeedModel::parse("person_entity_role").unwrap(),
            SeedModel::PersonEntityRole
        );
        assert_eq!(SeedModel::parse("address").unwrap(), SeedModel::Address);
        assert_eq!(
            SeedModel::parse("RelationshipLog").unwrap(),
            SeedModel::RelationshipLog
        );
        assert_eq!(
            SeedModel::parse("ProjectReference").unwrap(),
            SeedModel::ProjectReference
        );
        assert!(SeedModel::parse("question").is_err());
    }

    fn reference_entity_yaml(name: &str, project: Option<&str>) -> String {
        let project = project.map_or(String::new(), |code| {
            format!("    project:\n      code: {code}\n")
        });
        format!(
            "lookup_fields:\n  - project_id\n  - reason\nrecords:\n  - reason: counterparty_entity\n    entity:\n      name: {name}\n      entity_type:\n        name: LLC\n        jurisdiction:\n          name: Nevada\n{project}"
        )
    }

    fn reference_person_yaml(email: &str, reason: &str) -> String {
        format!(
            "lookup_fields:\n  - project_id\n  - reason\nrecords:\n  - reason: {reason}\n    person:\n      email: {email}\n      name: Referenced Person\n"
        )
    }

    /// The one route through which a scoped session creates an Entity: the
    /// counterparty organization lands as a referenced party, the Project's
    /// own Entity is untouched, and re-running finds the row.
    #[tokio::test]
    async fn scoped_project_reference_creates_a_counterparty_entity_only_as_a_reference() {
        let surreal = mem_surreal().await;
        let (entity_type, _) = entity_type_and_jurisdiction(&surreal).await;
        let project = project_fixture(&surreal, "acme").await;

        let report = reconcile_yaml(
            &surreal,
            SeedModel::ProjectReference,
            &reference_entity_yaml("Widget Works LLC", None),
            "Firm",
            false,
            &scoped_actor("acme"),
        )
        .await
        .expect("a scoped session references a counterparty entity");
        assert_eq!((report.created, report.unchanged), (1, 0));

        let counterparty =
            entities::find_by_name_and_type(&surreal, "Widget Works LLC", entity_type.id)
                .await
                .unwrap()
                .expect("the referenced entity now exists");
        let references = crate::project_references::for_project(&surreal, project.id)
            .await
            .unwrap();
        assert_eq!(references.len(), 1);
        assert_eq!(
            references[0].party,
            crate::project_references::Party::entity(counterparty.id)
        );
        assert_eq!(references[0].reason, "counterparty_entity");
        assert_eq!(
            projects::find_by_id(&surreal, project.id)
                .await
                .unwrap()
                .unwrap()
                .entity_id,
            project.entity_id,
            "the project still opens against its own entity"
        );

        let again = reconcile_yaml(
            &surreal,
            SeedModel::ProjectReference,
            &reference_entity_yaml("Widget Works LLC", None),
            "Firm",
            false,
            &scoped_actor("acme"),
        )
        .await
        .unwrap();
        assert_eq!((again.created, again.unchanged), (0, 1));

        // The plain entity model still refuses the same create under scope:
        // the reference is the only door.
        let refused = reconcile_yaml(
            &surreal,
            SeedModel::Entity,
            &entity_yaml("Widget Works Holdings LLC"),
            "Firm",
            false,
            &scoped_actor("acme"),
        )
        .await
        .expect_err("the entity model stays closed to a scoped session");
        assert!(downcasts_to::<ScopeViolation>(&refused, |v| matches!(
            v,
            ScopeViolation::EntityCreateNotScopable
        )));
    }

    #[tokio::test]
    async fn scoped_project_reference_to_a_person_grants_no_participation() {
        let surreal = mem_surreal().await;
        let project = project_fixture(&surreal, "acme").await;
        let report = reconcile_yaml(
            &surreal,
            SeedModel::ProjectReference,
            &reference_person_yaml("agent@example.com", "registered_agent"),
            "Firm",
            false,
            &scoped_actor("acme"),
        )
        .await
        .unwrap();
        assert_eq!(report.created, 1);
        let agent = persons::find_by_email_ci(&surreal, "agent@example.com")
            .await
            .unwrap()
            .expect("the referenced person now exists");
        assert!(
            projects::participation_for_person(&surreal, agent.id, project.id)
                .await
                .unwrap()
                .is_none(),
            "a reference writes no participation row"
        );
        assert!(crate::project_references::is_referenced(
            &surreal,
            project.id,
            crate::project_references::Party::person(agent.id)
        )
        .await
        .unwrap());
    }

    #[tokio::test]
    async fn scoped_project_reference_naming_another_project_is_cross_project() {
        let surreal = mem_surreal().await;
        entity_type_and_jurisdiction(&surreal).await;
        project_fixture(&surreal, "acme").await;
        project_fixture(&surreal, "other").await;
        let error = reconcile_yaml(
            &surreal,
            SeedModel::ProjectReference,
            &reference_entity_yaml("Widget Works LLC", Some("other")),
            "Firm",
            false,
            &scoped_actor("acme"),
        )
        .await
        .expect_err("a scoped session references from its own project only");
        assert!(downcasts_to::<ScopeViolation>(&error, |v| matches!(
            v,
            ScopeViolation::CrossProject
        )));
    }

    #[tokio::test]
    async fn unscoped_project_reference_must_name_its_project() {
        let surreal = mem_surreal().await;
        entity_type_and_jurisdiction(&surreal).await;
        let project = project_fixture(&surreal, "acme").await;
        let actor = ReconcileActor {
            bootstrap_owner_email: None,
            actor_role: Role::Lawyer,
            may_change_roles: false,
            project_scope: None,
            dry_run: false,
            actor_person_id: None,
        };
        let error = reconcile_yaml(
            &surreal,
            SeedModel::ProjectReference,
            &reference_entity_yaml("Widget Works LLC", None),
            "Firm",
            false,
            &actor,
        )
        .await
        .expect_err("no project to reference from");
        assert!(error.to_string().contains("names no project"), "{error}");
        assert!(
            !downcasts_to::<ScopeViolation>(&error, |_| true),
            "an unscoped omission is an invalid seed, not a scope violation"
        );

        let report = reconcile_yaml(
            &surreal,
            SeedModel::ProjectReference,
            &reference_entity_yaml("Widget Works LLC", Some("acme")),
            "Firm",
            false,
            &actor,
        )
        .await
        .unwrap();
        assert_eq!(report.created, 1);
        assert_eq!(
            crate::project_references::for_project(&surreal, project.id)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn project_reference_dry_run_plans_without_writing() {
        let surreal = mem_surreal().await;
        let (entity_type, _) = entity_type_and_jurisdiction(&surreal).await;
        let project = project_fixture(&surreal, "acme").await;
        let actor = ReconcileActor {
            dry_run: true,
            ..scoped_actor("acme")
        };
        let report = reconcile_yaml(
            &surreal,
            SeedModel::ProjectReference,
            &reference_entity_yaml("Widget Works LLC", None),
            "Firm",
            false,
            &actor,
        )
        .await
        .unwrap();
        assert_eq!(report.created, 1, "the plan reports the reference as new");
        assert!(
            entities::find_by_name_and_type(&surreal, "Widget Works LLC", entity_type.id)
                .await
                .unwrap()
                .is_none(),
            "a dry run creates nothing"
        );
        assert!(crate::project_references::for_project(&surreal, project.id)
            .await
            .unwrap()
            .is_empty());
    }

    #[test]
    fn project_reference_seed_validation_names_the_malformed_record() {
        let both = "lookup_fields:\n  - project_id\n  - reason\nrecords:\n  - reason: signatory\n    person:\n      email: a@example.com\n      name: A\n    entity:\n      name: B LLC\n      entity_type:\n        name: LLC\n";
        let error = validate_yaml(SeedModel::ProjectReference, both).unwrap_err();
        assert!(
            error.to_string().contains("both a person and an entity"),
            "{error}"
        );
        let neither =
            "lookup_fields:\n  - project_id\n  - reason\nrecords:\n  - reason: signatory\n";
        let error = validate_yaml(SeedModel::ProjectReference, neither).unwrap_err();
        assert!(error.to_string().contains("neither"), "{error}");
        let bad_reason = reference_person_yaml("a@example.com", "co_counsel");
        let error = validate_yaml(SeedModel::ProjectReference, &bad_reason).unwrap_err();
        assert!(error.to_string().contains("co_counsel"), "{error}");
        validate_yaml(
            SeedModel::ProjectReference,
            &reference_person_yaml("a@example.com", "signatory"),
        )
        .expect("a well-formed reference validates");
    }

    fn relationship_log_yaml(email: &str, detail: &str) -> String {
        format!(
            "lookup_fields:\n  - subject_id\n  - action\n  - detail\nrecords:\n  - subject:\n      email: {email}\n    action: appears_for\n    detail: {detail}\n"
        )
    }

    /// LAW-137: a relationship_log seed appends one row about the Person,
    /// attributed to the actor, and a re-import appends nothing.
    #[tokio::test]
    async fn relationship_log_seed_appends_once_and_grants_nothing() {
        let surreal = mem_surreal().await;
        let person = persons::create(&surreal, &NewPerson::new("Gus Gavel", "gus@example.com"))
            .await
            .unwrap();
        let lawyer = uuid::Uuid::now_v7();
        let actor = ReconcileActor {
            actor_person_id: Some(lawyer),
            ..unrestricted_actor()
        };
        let yaml = relationship_log_yaml(
            "GUS@example.com",
            "Gus Gavel appears for Debtor Widget Works LLC in 26-10042, per D.I. 29",
        );

        let first = reconcile_yaml(
            &surreal,
            SeedModel::RelationshipLog,
            &yaml,
            "Firm",
            false,
            &actor,
        )
        .await
        .unwrap();
        assert_eq!((first.created, first.unchanged), (1, 0));
        let again = reconcile_yaml(
            &surreal,
            SeedModel::RelationshipLog,
            &yaml,
            "Firm",
            false,
            &actor,
        )
        .await
        .unwrap();
        assert_eq!((again.created, again.unchanged), (0, 1));

        let rows = crate::relationship_logs::for_subject(&surreal, "person", person.id)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].action, "appears_for");
        assert_eq!(rows[0].actor_person_id, Some(lawyer));
        assert!(
            crate::projects::participations_for_person(&surreal, person.id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn relationship_log_seed_dry_run_writes_nothing_and_unknown_people_are_refused() {
        let surreal = mem_surreal().await;
        persons::create(&surreal, &NewPerson::new("Gus Gavel", "gus@example.com"))
            .await
            .unwrap();
        let dry = ReconcileActor {
            dry_run: true,
            ..unrestricted_actor()
        };
        let report = reconcile_yaml(
            &surreal,
            SeedModel::RelationshipLog,
            &relationship_log_yaml("gus@example.com", "on the service list"),
            "Firm",
            false,
            &dry,
        )
        .await
        .unwrap();
        assert_eq!(report.created, 1);
        assert!(crate::relationship_logs::all(&surreal)
            .await
            .unwrap()
            .is_empty());

        let error = reconcile_yaml(
            &surreal,
            SeedModel::RelationshipLog,
            &relationship_log_yaml("nobody@example.com", "on the service list"),
            "Firm",
            false,
            &unrestricted_actor(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("unknown person"), "{error}");
        assert!(validate_yaml(
            SeedModel::RelationshipLog,
            &relationship_log_yaml("gus@example.com", "\"\"")
        )
        .is_err());
    }

    fn project_relationship_log_yaml(email: &str, code: &str, detail: &str) -> String {
        format!(
            "lookup_fields:\n  - subject_id\n  - action\n  - detail\nrecords:\n  - subject:\n      email: {email}\n    action: appears_for\n    detail: {detail}\n    project:\n      code: {code}\n"
        )
    }

    /// A Project-scoped session writes its own Project onto the row.
    #[tokio::test]
    async fn relationship_log_seed_writes_the_scoped_project() {
        let surreal = mem_surreal().await;
        let project = project_fixture(&surreal, "acme").await;
        let person = persons::create(&surreal, &NewPerson::new("Gus Gavel", "gus@example.com"))
            .await
            .unwrap();
        projects::add_participation(&surreal, project.id, person.id, "client")
            .await
            .unwrap();

        let report = reconcile_yaml(
            &surreal,
            SeedModel::RelationshipLog,
            &relationship_log_yaml("gus@example.com", "on the service list in 26-10042"),
            "Firm",
            false,
            &scoped_actor("acme"),
        )
        .await
        .unwrap();
        assert_eq!(report.created, 1);

        let rows = crate::relationship_logs::for_project(&surreal, project.id)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].subject_id, person.id);
        assert_eq!(rows[0].project_id, Some(project.id));
    }

    /// An unrestricted session records the Project a row names, and still
    /// grants no role on it.
    #[tokio::test]
    async fn relationship_log_seed_records_a_named_project_for_an_unrestricted_session() {
        let surreal = mem_surreal().await;
        let project = project_fixture(&surreal, "acme").await;
        let person = persons::create(&surreal, &NewPerson::new("Gus Gavel", "gus@example.com"))
            .await
            .unwrap();

        reconcile_yaml(
            &surreal,
            SeedModel::RelationshipLog,
            &project_relationship_log_yaml("gus@example.com", "acme", "appears in 26-10042"),
            "Firm",
            false,
            &unrestricted_actor(),
        )
        .await
        .unwrap();

        let rows = crate::relationship_logs::for_project(&surreal, project.id)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].subject_id, person.id);
        assert!(projects::participations_for_person(&surreal, person.id)
            .await
            .unwrap()
            .is_empty());

        let error = reconcile_yaml(
            &surreal,
            SeedModel::RelationshipLog,
            &project_relationship_log_yaml("gus@example.com", "nope", "appears elsewhere"),
            "Firm",
            false,
            &unrestricted_actor(),
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("unknown project code"),
            "{error}"
        );
    }

    /// A scoped session cannot file a row against a Project outside its
    /// scope.
    #[tokio::test]
    async fn relationship_log_seed_refuses_a_project_outside_the_session_scope() {
        let surreal = mem_surreal().await;
        let project = project_fixture(&surreal, "acme").await;
        project_fixture(&surreal, "widget-works").await;
        let person = persons::create(&surreal, &NewPerson::new("Gus Gavel", "gus@example.com"))
            .await
            .unwrap();
        projects::add_participation(&surreal, project.id, person.id, "client")
            .await
            .unwrap();

        let error = reconcile_yaml(
            &surreal,
            SeedModel::RelationshipLog,
            &project_relationship_log_yaml("gus@example.com", "widget-works", "appears"),
            "Firm",
            false,
            &scoped_actor("acme"),
        )
        .await
        .unwrap_err();
        assert!(downcasts_to::<ScopeViolation>(&error, |v| matches!(
            v,
            ScopeViolation::CrossProject
        )));
        assert!(crate::relationship_logs::all(&surreal)
            .await
            .unwrap()
            .is_empty());
    }

    /// LAW-57: importing an address attaches it to the Person.
    #[tokio::test]
    async fn address_seed_attaches_to_the_person() {
        let surreal = mem_surreal().await;
        persons::create(
            &surreal,
            &NewPerson::new("Jane Example", "jane@example.com"),
        )
        .await
        .unwrap();

        let report = reconcile_yaml(
            &surreal,
            SeedModel::Address,
            &address_yaml("jane@example.com", "123 Main St", "89501"),
            "Firm",
            false,
            &unrestricted_actor(),
        )
        .await
        .expect("address seed reconciles");
        assert_eq!((report.created, report.unchanged), (1, 0));

        let person = persons::find_by_email_ci(&surreal, "jane@example.com")
            .await
            .unwrap()
            .unwrap();
        let addresses = crate::addresses::for_person(&surreal, person.id)
            .await
            .unwrap();
        assert_eq!(addresses.len(), 1, "the address attached to the person");
        assert_eq!(addresses[0].line1, "123 Main St");
        assert_eq!(addresses[0].postal_code, "89501");

        // Re-seeding the same (person, line1, postal_code) is a no-op.
        let again = reconcile_yaml(
            &surreal,
            SeedModel::Address,
            &address_yaml("jane@example.com", "123 Main St", "89501"),
            "Firm",
            false,
            &unrestricted_actor(),
        )
        .await
        .expect("re-seed is a no-op");
        assert_eq!((again.created, again.unchanged), (0, 1));
        assert_eq!(
            crate::addresses::for_person(&surreal, person.id)
                .await
                .unwrap()
                .len(),
            1,
            "re-seeding must not duplicate the address"
        );
    }

    #[tokio::test]
    async fn address_seed_referencing_an_unknown_person_is_refused() {
        let surreal = mem_surreal().await;

        let refused = reconcile_yaml(
            &surreal,
            SeedModel::Address,
            &address_yaml("nobody@example.com", "123 Main St", "89501"),
            "Firm",
            false,
            &unrestricted_actor(),
        )
        .await;
        assert!(refused.is_err());
    }

    #[tokio::test]
    async fn person_project_role_seed_resolves_email_and_code_lookups() {
        let surreal = mem_surreal().await;
        let project = project_fixture(&surreal, "acme").await;
        persons::create(
            &surreal,
            &NewPerson::new("Jane Example", "jane@example.com"),
        )
        .await
        .unwrap();

        let report = reconcile_yaml(
            &surreal,
            SeedModel::PersonProjectRole,
            &person_project_role_yaml("jane@example.com", "acme"),
            "Firm",
            false,
            &unrestricted_actor(),
        )
        .await
        .expect("nested lookups resolve");
        assert_eq!((report.created, report.unchanged), (1, 0));
        let person = persons::find_by_email_ci(&surreal, "jane@example.com")
            .await
            .unwrap()
            .unwrap();
        let row = projects::participation_for_person(&surreal, person.id, project.id)
            .await
            .unwrap()
            .expect("participation written from nested keys");
        assert_eq!(row.participation, "client");

        let again = reconcile_yaml(
            &surreal,
            SeedModel::PersonProjectRole,
            &person_project_role_yaml("jane@example.com", "acme"),
            "Firm",
            false,
            &unrestricted_actor(),
        )
        .await
        .expect("re-seed is a no-op");
        assert_eq!((again.created, again.unchanged), (0, 1));
    }

    #[tokio::test]
    async fn scoped_person_project_role_refuses_a_different_project_code() {
        let surreal = mem_surreal().await;
        project_fixture(&surreal, "acme").await;
        project_fixture(&surreal, "widget-works").await;
        persons::create(
            &surreal,
            &NewPerson::new("Jane Example", "jane@example.com"),
        )
        .await
        .unwrap();

        let error = reconcile_yaml(
            &surreal,
            SeedModel::PersonProjectRole,
            &person_project_role_yaml("jane@example.com", "widget-works"),
            "Firm",
            false,
            &scoped_actor("acme"),
        )
        .await
        .expect_err("a scoped session cannot write another project's participation");
        assert!(downcasts_to::<ScopeViolation>(&error, |v| matches!(
            v,
            ScopeViolation::CrossProject
        )));
    }

    #[tokio::test]
    async fn person_entity_role_seed_resolves_email_and_name_lookups() {
        let surreal = mem_surreal().await;
        let project = project_fixture(&surreal, "acme").await;
        let entity = entities::find_by_id(&surreal, project.entity_id)
            .await
            .unwrap()
            .unwrap();
        persons::create(
            &surreal,
            &NewPerson::new("Jane Example", "jane@example.com"),
        )
        .await
        .unwrap();

        let report = reconcile_yaml(
            &surreal,
            SeedModel::PersonEntityRole,
            &person_entity_role_yaml("jane@example.com", &entity.name, "manages"),
            "Firm",
            false,
            &unrestricted_actor(),
        )
        .await
        .expect("nested entity lookup resolves");
        assert_eq!(report.created, 1);
        let person = persons::find_by_email_ci(&surreal, "jane@example.com")
            .await
            .unwrap()
            .unwrap();
        assert!(
            crate::entity_roles::find(&surreal, person.id, entity.id, "manages")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn scoped_person_entity_role_refuses_an_entity_the_project_does_not_name() {
        let surreal = mem_surreal().await;
        let project = project_fixture(&surreal, "acme").await;
        let other = crate::test_support::seed_entity(&surreal).await;
        let other_entity = entities::find_by_id(&surreal, other)
            .await
            .unwrap()
            .unwrap();
        persons::create(
            &surreal,
            &NewPerson::new("Jane Example", "jane@example.com"),
        )
        .await
        .unwrap();

        let error = reconcile_yaml(
            &surreal,
            SeedModel::PersonEntityRole,
            &person_entity_role_yaml("jane@example.com", &other_entity.name, "manages"),
            "Firm",
            false,
            &scoped_actor("acme"),
        )
        .await
        .expect_err("a scoped session cannot grant a role on another project's entity");
        assert!(downcasts_to::<ScopeViolation>(&error, |v| matches!(
            v,
            ScopeViolation::CrossProject
        )));
        let _ = project;
    }

    #[tokio::test]
    async fn person_seed_can_rename_the_bootstrap_owner_but_not_its_role() {
        // `reconcile_yaml` shares `people_commands::update_person` with the
        // interactive admin edit form, so it inherits the same narrower
        // invariant: only the bootstrap Owner's email and role are pinned.
        // Its name is an ordinary field, editable through this path exactly
        // like every other seeded person's.
        let surreal = mem_surreal().await;
        persons::create(
            &surreal,
            &NewPerson::with_role("The Owner", "owner@example.com", Role::Owner),
        )
        .await
        .unwrap();

        // An Admin actor can't reach an Owner-rank row at all — the
        // pre-existing authority-rank lock refuses that outright, regardless
        // of the bootstrap pin this test covers. Only an Owner actor (equal
        // rank) reaches the bootstrap-specific pin.
        let actor = ReconcileActor {
            bootstrap_owner_email: Some("owner@example.com"),
            actor_role: Role::Owner,
            may_change_roles: true,
            project_scope: None,
            dry_run: false,
            actor_person_id: None,
        };
        let report = reconcile_yaml(
            &surreal,
            SeedModel::Person,
            &person_yaml("owner@example.com", "Renamed Owner"),
            "Firm",
            true,
            &actor,
        )
        .await
        .expect("a seed document may rename the bootstrap Owner");
        assert_eq!(report.updated, 1);
        let row = persons::find_by_email_ci(&surreal, "owner@example.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.name, "Renamed Owner");
        assert_eq!(
            row.role,
            Role::Owner,
            "the bootstrap Owner's role stays pinned through the seed path too"
        );
    }

    #[tokio::test]
    async fn person_seed_cannot_edit_a_higher_ranked_person() {
        let surreal = mem_surreal().await;
        persons::create(
            &surreal,
            &NewPerson::with_role("Ranking Owner", "ranking-owner@example.com", Role::Owner),
        )
        .await
        .unwrap();

        let actor = ReconcileActor {
            bootstrap_owner_email: None,
            actor_role: Role::Lawyer,
            may_change_roles: false,
            project_scope: None,
            dry_run: false,
            actor_person_id: None,
        };
        let error = reconcile_yaml(
            &surreal,
            SeedModel::Person,
            &person_yaml("ranking-owner@example.com", "Hacked Name"),
            "Firm",
            true,
            &actor,
        )
        .await
        .expect_err("a lawyer-tier seed caller must not edit a higher-ranked person");
        assert!(error.to_string().contains("higher system role"));
    }

    #[test]
    fn seed_document_validation_uses_the_reconciliation_shape() {
        let valid = r"
lookup_fields:
  - email
records:
  - email: operator@example.com
    name: Operator
";
        validate_yaml(SeedModel::Person, valid).expect("valid person seed");

        let unknown_field = r"
lookup_fields:
  - email
records:
  - email: operator@example.com
    display_name: Operator
";
        assert!(validate_yaml(SeedModel::Person, unknown_field)
            .unwrap_err()
            .to_string()
            .contains("unknown field `display_name`"));

        let duplicate = r"
lookup_fields:
  - email
records:
  - email: operator@example.com
    name: One
  - email: OPERATOR@example.com
    name: Two
";
        assert!(validate_yaml(SeedModel::Person, duplicate)
            .unwrap_err()
            .to_string()
            .contains("duplicate email"));
    }

    /// The firm's own Entity is the one seeded row that takes
    /// `firm_anchor_key`, so it is the one row the `entity_firm_anchor`
    /// index can refuse (ENG-120). The seed re-runs over a live database
    /// on every boot, so it has to survive losing that write.
    ///
    /// Guieb & Shook PC holds its own private mailbox at the Ridgeview Mail
    /// Center, and within that mail centre the box number is the whole address
    /// — every box shares a street, suite, and ZIP, so a wrong suffix delivers
    /// the firm's mail to another entity rather than bouncing. Only the box the
    /// firm publishes seeds; any other is private runtime data.
    ///
    /// The two halves land in different layers on purpose, and this asserts
    /// both: the Entity is canonical, so every deployment carries it, while
    /// the mailbox is the Firm's own and rides the brand layer.
    #[tokio::test]
    async fn shook_law_holds_mailbox_9002_at_ridgeview() {
        let surreal = mem_surreal().await;
        let storage = fs_storage().await;

        seed_canonical(&surreal, &storage).await.expect("seed");
        let firm = crate::entities::find_by_name(&surreal, super::FIRM_ENTITY_NAME)
            .await
            .unwrap()
            .expect("the firm anchor is canonical, so every deployment carries it");

        // Canonical alone carries no address: the mailbox is the Firm's.
        assert!(
            crate::addresses::for_entity(&surreal, firm.id)
                .await
                .unwrap()
                .is_empty(),
            "the Firm's own addresses must stay out of the canonical layer"
        );

        super::seed_brand(&surreal, super::BrandSeed::Neon)
            .await
            .expect("brand seed");
        let at_ridgeview: Vec<String> = crate::addresses::for_entity(&surreal, firm.id)
            .await
            .unwrap()
            .into_iter()
            .map(|a| a.line1)
            .filter(|line| line.starts_with("5150 Mae Anne Ave"))
            .collect();
        assert_eq!(
            at_ridgeview,
            vec!["5150 Mae Anne Ave Ste 405-9002".to_string()],
            "the firm holds exactly one box at the mail centre, and 9002 is that box"
        );

        // The box is unique across the mail center: no other seeded entity
        // may answer to it, or mail routes to whichever row is read first.
        let holders = crate::addresses::list_all(&surreal)
            .await
            .unwrap()
            .into_iter()
            .filter(|a| a.line1.ends_with("405-9002"))
            .count();
        assert_eq!(holders, 1, "one box, one holder");
    }

    /// No entity beyond the two firm anchors seeds in either layer. A
    /// further corporation, a trust, or a personal entity has no verified public
    /// source in this repository, so it is supplied to a deployment privately.
    #[tokio::test]
    async fn only_the_firm_anchors_seed_as_entities() {
        let surreal = mem_surreal().await;

        seed_canonical(&surreal, &fs_storage().await)
            .await
            .expect("seed");
        super::seed_brand(&surreal, super::BrandSeed::Neon)
            .await
            .expect("brand seed");

        let mut names: Vec<String> = crate::entities::all(&surreal)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        names.sort();
        let mut expected = super::FIRM_ENTITY_NAMES
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(names, expected);
    }

    /// A Florida P.A. is a distinct designation under Ch. 621, Fla. Stat.,
    /// not a `Professional Corporation` — so the vocabulary needs its own
    /// entry rather than absorbing P.A.s into the nearest available form.
    #[tokio::test]
    async fn professional_association_seeds_as_its_own_entity_type() {
        let surreal = mem_surreal().await;

        seed_canonical(&surreal, &fs_storage().await)
            .await
            .expect("seed");

        let pa = crate::entity_types::find_by_name(&surreal, "Professional Association")
            .await
            .unwrap()
            .expect("the professional-association type seeds");

        let pc = crate::entity_types::find_by_name(&surreal, "Professional Corporation")
            .await
            .unwrap()
            .expect("the professional-corporation type seeds");

        assert_ne!(pa.id, pc.id, "a P.A. is not a P.C.");
    }

    /// A database seeded before the firm became a California professional
    /// corporation still holds `Shook Law PLLC` as the anchor. Boot must move
    /// that same row to the PC, so its id — and every Firm and Address that
    /// points at it — survives, and no second anchor is forked.
    #[tokio::test]
    async fn a_legacy_pllc_firm_row_is_renamed_to_the_pc_in_place() {
        a_legacy_firm_row_is_renamed_in_place("Shook Law PLLC", "Professional LLC", "Nevada").await;
    }

    /// The same move for a database seeded before the corporation was
    /// renamed from `Shook Law PC` to [`super::FIRM_ENTITY_NAME`].
    #[tokio::test]
    async fn a_shook_law_pc_firm_row_is_renamed_to_the_current_name_in_place() {
        a_legacy_firm_row_is_renamed_in_place(
            "Shook Law PC",
            "Professional Corporation",
            "California",
        )
        .await;
    }

    async fn a_legacy_firm_row_is_renamed_in_place(
        legacy_name: &str,
        legacy_type: &str,
        legacy_jurisdiction: &str,
    ) {
        assert!(super::LEGACY_FIRM_ENTITY_NAMES.contains(&legacy_name));
        let surreal = mem_surreal().await;
        seed_canonical(&surreal, &fs_storage().await)
            .await
            .expect("seed");

        let anchor = crate::entities::find_by_name(&surreal, super::FIRM_ENTITY_NAME)
            .await
            .unwrap()
            .expect("the anchor seeds");
        let legacy_type = crate::entity_types::find_by_name(&surreal, legacy_type)
            .await
            .unwrap()
            .expect("the legacy entity type seeds");
        let legacy_jurisdiction = jurisdictions::find_by_name(&surreal, legacy_jurisdiction)
            .await
            .unwrap()
            .expect("the legacy jurisdiction seeds");
        crate::entities::set_firm_anchor_key(&surreal, anchor.id, None)
            .await
            .unwrap();
        crate::entities::update(
            &surreal,
            anchor.id,
            &crate::entities::NewEntity {
                name: legacy_name.to_string(),
                entity_type_id: legacy_type.id,
                jurisdiction_id: legacy_jurisdiction.id,
                phone: None,
                url: None,
                xero_id: None,
                firm_anchor_key: Some(legacy_name.to_ascii_lowercase()),
            },
        )
        .await
        .unwrap()
        .expect("rewound to the legacy name");

        seed_canonical(&surreal, &fs_storage().await)
            .await
            .expect("a reseed renames the legacy firm");

        let renamed = crate::entities::find_by_id(&surreal, anchor.id)
            .await
            .unwrap()
            .expect("the row survives under its old id");
        assert_eq!(renamed.name, super::FIRM_ENTITY_NAME);
        assert!(renamed.is_firm_anchor());
        assert!(
            crate::entities::find_by_name(&surreal, legacy_name)
                .await
                .unwrap()
                .is_none(),
            "no {legacy_name} row may remain"
        );
        let pc = crate::entity_types::find_by_name(&surreal, "Professional Corporation")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(renamed.entity_type_id, pc.id);
    }

    /// Losing it is not hypothetical: this seed finds an existing entity
    /// by `(name, entity_type_id)`, so an anchor row carrying a different
    /// entity type is invisible to the find and reaches the create — and
    /// the index refuses it. That has to read as "already seeded", not as
    /// a failed boot.
    #[tokio::test]
    async fn a_seed_that_loses_the_firm_anchor_write_still_succeeds() {
        let surreal = mem_surreal().await;

        // An anchor the seed's own find cannot see: right name, a type it
        // will not look under.
        let decoy_type = crate::entity_types::create(&surreal, "Decoy Type")
            .await
            .unwrap();
        crate::entities::create(
            &surreal,
            &crate::entities::NewEntity {
                name: super::FIRM_ENTITY_NAME.into(),
                entity_type_id: decoy_type.id,
                jurisdiction_id: uuid::Uuid::now_v7(),
                phone: None,
                url: None,
                xero_id: None,
                firm_anchor_key: Some(super::FIRM_ENTITY_NAME.to_lowercase()),
            },
        )
        .await
        .unwrap();

        seed_canonical(&surreal, &fs_storage().await)
            .await
            .expect("a seed that loses the anchor write must still succeed");

        let anchors = crate::entities::all(&surreal)
            .await
            .unwrap()
            .into_iter()
            .filter(|row| row.name == super::FIRM_ENTITY_NAME)
            .count();
        assert_eq!(anchors, 1, "the firm anchor must stay a single row");
    }

    #[tokio::test]
    async fn seeds_full_question_set() {
        let surreal = mem_surreal().await;
        let report = seed_canonical(&surreal, &fs_storage().await)
            .await
            .expect("seed");
        // The canonical question catalog is the closed type registry.
        // Template-specific prompt keys live after the `__` discriminator
        // in state names rather than as seeded question rows.
        let expected = QuestionType::all_tokens().len();
        let qs = crate::questions::list_all(&surreal).await.unwrap();
        assert_eq!(qs.len(), expected);
        assert!(qs.iter().any(|q| q.code == "person"));
        assert!(qs.iter().any(|q| q.code == "people"));
        assert!(qs.iter().any(|q| q.code == "custom_text"));
        assert!(qs.iter().any(|q| q.code == "custom_single_choice"));
        assert!(qs.iter().any(|q| q.code == "custom_datetime"));
        assert_eq!(report.questions_inserted, expected);
    }

    #[tokio::test]
    async fn seeds_full_jurisdiction_set() {
        let surreal = mem_surreal().await;
        seed_canonical(&surreal, &fs_storage().await)
            .await
            .expect("seed");
        let js = jurisdictions::list_all(&surreal).await.unwrap();
        // 50 states + DC + the ISO 3166-1 country set (alpha-3 codes,
        // with United States and Germany on their pre-ISO codes).
        assert_eq!(js.len(), 248);
        let codes: Vec<&str> = js.iter().map(|j| j.code.as_str()).collect();
        for code in [
            "NV", "CA", "NY", "TX", "WY", "DC", "US", "GMBH", "MEX", "CAN", "GBR",
        ] {
            assert!(codes.contains(&code), "expected `{code}` in jurisdictions");
        }
        // `jurisdiction_type` is reconciled with the seed: states are
        // `state`, sovereigns are `country` — the boundary the `country`
        // question type's option filter rides on.
        let by_code = |c: &str| js.iter().find(|j| j.code == c).unwrap();
        assert_eq!(by_code("NV").jurisdiction_type, "state");
        assert_eq!(by_code("US").jurisdiction_type, "country");
        assert_eq!(by_code("GMBH").jurisdiction_type, "country");
        assert_eq!(by_code("MEX").jurisdiction_type, "country");
        // The state Georgia and the country Georgia stay distinct by
        // name, so a name-keyed answer can never be ambiguous.
        assert_eq!(by_code("GA").name, "Georgia");
        assert_eq!(by_code("GEO").name, "Georgia (country)");
    }

    #[test]
    fn a_seeded_body_recovers_its_clause_sources_only_when_it_matches_the_seed() {
        let template = SEEDED_TEMPLATES
            .iter()
            .find(|t| t.label.ends_with("/mutual_nda.md"))
            .expect("mutual_nda is seeded");
        let composed = rules::clauses::compile(template.markdown).unwrap();
        let (_, body) = split_template(&composed).unwrap();
        let stored = String::from_utf8(normalized_body_bytes(body).into_owned()).unwrap();
        assert!(
            rules::clauses::pinned_sources(&stored).is_empty(),
            "the stored body carries no frontmatter"
        );
        let sources = seeded_clause_sources("mutual_nda", &stored).expect("the seed's own body");
        assert!(
            sources.contains(&"dispute_resolution@1".to_string()),
            "{sources:?}"
        );
        assert_eq!(
            seeded_clause_sources("mutual_nda", &format!("{stored}edited")),
            None
        );
        assert_eq!(seeded_clause_sources("not_seeded", &stored), None);
    }

    #[test]
    fn seeded_template_codes_are_derived_from_the_bundled_catalog() {
        let codes = seeded_template_codes().expect("seeded template codes");
        assert_eq!(codes.len(), SEEDED_TEMPLATES.len());
        for (template, code) in SEEDED_TEMPLATES.iter().zip(&codes) {
            assert_eq!(
                std::path::Path::new(template.label)
                    .file_stem()
                    .and_then(std::ffi::OsStr::to_str),
                Some(code.as_str()),
                "{} derives its code from its filename",
                template.label
            );
        }
        assert!(codes.iter().any(|code| code == "onboarding__letter"));
        assert!(codes.iter().any(|code| code == "offboarding__letter"));
        assert!(codes.iter().any(|code| code == "nv__llc_formation"));
        assert!(
            !codes
                .iter()
                .any(|code| code.starts_with("onboarding__letter_")),
            "project-scoped letter variants are not seeded; one shared onboarding letter remains"
        );
    }

    /// A canonical LF template, and the byte-for-byte CRLF twin a
    /// Windows checkout materialises from it.
    ///
    /// The assertions below compare *parsed values* across the pair
    /// rather than counting errors. The failure mode this guards is
    /// silent absence — `split_template` returning `None`, or returning
    /// a frontmatter slice that happens to deserialise into different
    /// values — and neither shows up in an error count. A count-based
    /// test would have passed against the original LF-only parser on
    /// Linux and told us nothing about Windows.
    const LF_DOC: &str = "---\ncode: t__demo\ntitle: Demo\nrespondent_type: org\nkind: letter\n---\n# Body\n\nSecond paragraph.\n";

    fn crlf(s: &str) -> String {
        s.replace('\n', "\r\n")
    }

    fn parse_fm(md: &str) -> (TemplateFrontmatter, String) {
        let (fm_str, body) = split_template(md).expect("frontmatter present");
        let fm: TemplateFrontmatter = serde_yaml::from_str(fm_str).expect("frontmatter parses");
        (fm, body.to_string())
    }

    #[test]
    fn split_template_parses_the_same_values_from_lf_and_crlf() {
        let (lf, lf_body) = parse_fm(LF_DOC);
        let (crlf_fm, crlf_body) = parse_fm(&crlf(LF_DOC));

        // Every frontmatter value, not merely "it parsed".
        assert_eq!(lf.code, crlf_fm.code);
        assert_eq!(lf.title, crlf_fm.title);
        assert_eq!(lf.respondent_type, crlf_fm.respondent_type);
        assert_eq!(lf.form, crlf_fm.form);
        assert_eq!(lf.kind, crlf_fm.kind);
        assert_eq!(crlf_fm.code.as_deref(), Some("t__demo"));
        assert_eq!(crlf_fm.kind.as_deref(), Some("letter"));

        // The body is delimited at the same logical point in both. It
        // still carries CRLF here; normalisation happens at ingest.
        assert_eq!(crlf_body, crlf(&lf_body));
        assert_eq!(lf_body, "# Body\n\nSecond paragraph.\n");
    }

    #[test]
    fn split_template_accepts_a_crlf_opener_and_closer() {
        let (fm, body) =
            parse_fm("---\r\ncode: a\r\ntitle: A\r\nrespondent_type: org\r\n---\r\nbody\r\n");
        assert_eq!(fm.code.as_deref(), Some("a"));
        assert_eq!(fm.title, "A");
        assert_eq!(body, "body\r\n");
    }

    #[test]
    fn split_template_accepts_a_crlf_closer_at_eof_without_a_trailing_newline() {
        for md in [
            "---\r\ncode: a\r\ntitle: A\r\nrespondent_type: org\r\n---",
            "---\ncode: a\ntitle: A\nrespondent_type: org\n---",
        ] {
            let (fm, body) = parse_fm(md);
            assert_eq!(fm.code.as_deref(), Some("a"), "{md:?}");
            assert_eq!(fm.respondent_type, "org", "{md:?}");
            assert_eq!(body, "", "closer at EOF leaves no body: {md:?}");
        }
    }

    #[test]
    fn split_template_accepts_empty_frontmatter_in_either_ending() {
        for (md, want_body) in [
            ("---\n---\nbody\n", "body\n"),
            ("---\r\n---\r\nbody\r\n", "body\r\n"),
            ("---\n---", ""),
            ("---\r\n---\r", ""),
        ] {
            let (fm_str, body) =
                split_template(md).expect("empty frontmatter is still frontmatter");
            assert_eq!(fm_str, "", "{md:?}");
            assert_eq!(body, want_body, "{md:?}");
        }
    }

    #[test]
    fn split_template_closes_a_mixed_ending_file_at_the_first_real_delimiter() {
        // CRLF frontmatter, but the closer was written with LF. The
        // earlier of the two matches must win, so the `---` inside the
        // body is not mistaken for the closer.
        let (fm, body) = parse_fm(
            "---\r\ncode: a\r\ntitle: A\r\nrespondent_type: org\n---\nbody\n---\ntrailing\n",
        );
        assert_eq!(fm.code.as_deref(), Some("a"));
        assert_eq!(body, "body\n---\ntrailing\n");
    }

    #[test]
    fn split_template_still_reports_absent_frontmatter() {
        // The widened parser must not start accepting documents that
        // genuinely have no frontmatter — that would trade a false
        // negative for a false positive.
        for md in [
            "# no frontmatter\n",
            "",
            "--\ncode: a\n--\n",
            "---\ncode: a\n",
        ] {
            assert!(split_template(md).is_none(), "{md:?} has no frontmatter");
        }
    }

    #[test]
    fn every_bundled_template_splits_on_this_checkout() {
        // The regression that motivated the fix: on a CRLF checkout this
        // failed for all of them, and the first caller turned it into a
        // fatal boot error. Asserts against the real bundled catalog, so
        // it fails on whichever platform is actually broken.
        for template in SEEDED_TEMPLATES {
            let (fm_str, _) = split_template(template.markdown)
                .unwrap_or_else(|| panic!("{}: frontmatter not found", template.label));
            let fm: TemplateFrontmatter = serde_yaml::from_str(fm_str)
                .unwrap_or_else(|e| panic!("{}: frontmatter parse: {e}", template.label));
            assert!(!fm.title.is_empty(), "{}: empty title", template.label);
            assert!(
                !template_code(template.label)
                    .expect("template code")
                    .is_empty(),
                "{}: empty filename stem",
                template.label
            );
        }
    }

    #[test]
    fn template_body_bytes_are_platform_independent() {
        // The asset-sha question from ENG-265. `ingest_content` is
        // content-addressed, so these bytes decide the `asset_id`; if an
        // LF and a CRLF checkout produced different bytes, the same
        // logical template would fork into two immutable versions
        // depending on which platform compiled the binary.
        let crlf_doc = crlf(LF_DOC);
        let lf_bytes = normalized_body_bytes(split_template(LF_DOC).expect("lf").1);
        let crlf_bytes = normalized_body_bytes(split_template(&crlf_doc).expect("crlf").1);
        assert_eq!(lf_bytes, crlf_bytes);
        assert!(
            !crlf_bytes.contains(&b'\r'),
            "normalised body must carry no carriage returns"
        );
    }

    #[tokio::test]
    async fn seeds_the_bundled_template_catalog() {
        let surreal = mem_surreal().await;
        let report = seed_canonical(&surreal, &fs_storage().await)
            .await
            .expect("seed");
        assert_eq!(
            report.templates_inserted,
            SEEDED_TEMPLATES.len(),
            "expected the full bundled template catalog to be inserted on first pass"
        );
        // Spot-check templates from across the catalog so a dropped
        // `include_str!` entry is caught, not just the retainer.
        for code in [
            "onboarding__letter",
            "offboarding__letter",
            "nv__llc_formation",
            "nv__profit_corp_formation",
            "us__form_990",
            "us__naturalization",
        ] {
            assert!(
                crate::templates::resolve(&surreal, None, code)
                    .await
                    .unwrap()
                    .is_some(),
                "expected bundled template `{code}` to be seeded"
            );
        }
        let tmpl = crate::templates::resolve(&surreal, None, "onboarding__letter")
            .await
            .unwrap()
            .expect("template row");
        assert_eq!(tmpl.title, "Onboarding Letter");
        assert_eq!(tmpl.respondent_type, "person_and_entity");
        assert!(tmpl.project_id.is_none(), "bundled templates are shared");
        // The body now lives in a blob — fetch it via the storage
        // accessor. Just the markdown body, no frontmatter, so the
        // renderer's dotted glossary interpolation finds
        // its targets.
        let body = crate::templates::body(&surreal, &fs_storage().await, &tmpl)
            .await
            .expect("template body in storage");
        assert!(
            !body.starts_with("---"),
            "body should not include the YAML frontmatter; got {:?}",
            &body[..body.len().min(20)]
        );
        assert!(body.contains("{{person__client.name}}"));
        assert!(body.contains("contact@neonlaw.com"));
        assert!(body.contains("{{project__engagement.name}}"));
        assert!(body.contains("{{custom_clauses}}"));
    }

    #[tokio::test]
    async fn template_seeder_is_idempotent_on_second_pass() {
        let surreal = mem_surreal().await;
        let first = seed_canonical(&surreal, &fs_storage().await).await.unwrap();
        let second = seed_canonical(&surreal, &fs_storage().await).await.unwrap();
        assert_eq!(first.templates_inserted, SEEDED_TEMPLATES.len());
        assert_eq!(
            second.templates_inserted, 0,
            "second pass must skip every existing template"
        );
        let count = crate::templates::list_current(&surreal)
            .await
            .unwrap()
            .into_iter()
            .filter(|t| t.code == "onboarding__letter")
            .count();
        assert_eq!(
            count, 1,
            "exactly one current onboarding letter template row"
        );
    }

    /// The firm's one engagement agreement carries the three load-bearing
    /// elements every matter needs: the AAA arbitration clause (forum
    /// selection only, with the non-waivable fee-arbitration carve-out across
    /// the three governing-law choices and the independent-counsel sentence —
    /// never a liability limitation), the `contact@neonlaw.com` reach-the-Firm
    /// clause, and the custom-clause slot extra work arrives through.
    ///
    /// These moved here when the twelve service-specific retainers retired:
    /// they were the only bodies carrying them, so without this the firm
    /// would have shipped an engagement agreement with no arbitration clause
    /// and no fee-arbitration disclosure.
    #[tokio::test]
    async fn the_retainer_carries_arbitration_contact_and_the_clause_slot() {
        let surreal = mem_surreal().await;
        seed_canonical(&surreal, &fs_storage().await).await.unwrap();
        let storage = fs_storage().await;

        // Distinctive phrases from the three clauses. Checked against the
        // body with its line wrapping collapsed, so reflowing a paragraph to
        // satisfy the Markdown linter cannot silently drop a clause from
        // this guard.
        let required = [
            "binding arbitration administered by the American Arbitration Association",
            "limit, cap, or waive our responsibility for our own work",
            "consult independent counsel before agreeing to it",
            "Write to contact@neonlaw.com",
            "{{custom_clauses}}",
            "{{client.signature}}",
            "{{firm.signature}}",
        ];

        let code = "onboarding__letter";
        let tmpl = crate::templates::resolve(&surreal, None, code)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("{code} seeded"));
        let body = crate::templates::body(&surreal, &storage, &tmpl)
            .await
            .expect("retainer body");
        let flat = body
            .split_whitespace()
            .filter(|token| *token != ">")
            .collect::<Vec<_>>()
            .join(" ");
        for phrase in required {
            assert!(
                flat.contains(phrase),
                "{code} must carry the clause phrase {phrase:?}"
            );
        }

        // The price sheet and the day-rate cadence live in the body. Extra
        // work still arrives through the custom-clause slot (ENG-146).
        assert!(
            flat.contains("We bill it monthly") && flat.contains("Base Membership"),
            "the engagement letter states the day-rate cadence and the price sheet"
        );

        // It is practice-neutral. The old body excluded litigation, which
        // would have made the firm's own litigation practice unopenable on
        // its only engagement agreement.
        assert!(
            !flat.contains("does not include litigation"),
            "the engagement agreement must not exclude the firm's own practice areas"
        );

        // The arbitration clause must not read as a liability waiver
        // (RPC 1.8(h)). Guard against a regression that re-introduces
        // limiting language.
        for forbidden in ["limit our liability", "waive any claim against the Firm"] {
            assert!(
                !flat.contains(forbidden),
                "{code} must not limit malpractice liability ({forbidden:?})"
            );
        }

        // Governing law is fillable per engagement (#364 pattern propagated
        // in #363): the clause names the questionnaire variable, not a
        // hardcoded jurisdiction. The token is bare, not a code span, so the
        // letter renderer fills and highlights it like every other
        // placeholder. The arbitration forum does not flex with governing
        // law.
        assert!(
            flat.contains("The law of")
                && flat.contains("{{custom_single_choice__governing_law}}")
                && flat.contains("governs this letter"),
            "{code} must fill governing law from the questionnaire, not hardcode it"
        );
        assert!(
            !flat.contains("decided under Nevada law"),
            "{code} must not hardcode 'decided under Nevada law'; use the fillable clause"
        );
    }

    /// ENG-559: a no-charge engagement states its fee basis — "no fee" — as
    /// a custom clause, the same slot every other fee arrangement uses. An
    /// onboarding letter carrying only this clause must clear the
    /// `clauses_required` gate `portal::retainer_walk::dispatch_signature`
    /// enforces (mirrored here as the same boolean: the custom-clause marker
    /// present with no clauses on the notation) and must render inside § I,
    /// before § II states the fee's amount and cadence.
    #[tokio::test]
    async fn no_charge_clause_clears_the_clauses_required_gate_and_renders_in_section_i() {
        let surreal = mem_surreal().await;
        seed_canonical(&surreal, &fs_storage().await).await.unwrap();
        let storage = fs_storage().await;

        let tmpl = crate::templates::resolve(&surreal, None, "onboarding__letter")
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("onboarding__letter seeded"));
        let raw_body = crate::templates::body(&surreal, &storage, &tmpl)
            .await
            .expect("retainer body");

        let project_id =
            crate::test_support::seed_project_surreal(&surreal, "No-Charge Onboarding").await;
        let person_id = crate::test_support::dri_person(&surreal).await;
        let notation_id = crate::notations::create(
            &surreal,
            &crate::notations::NewNotation::new(tmpl.id, person_id, project_id, "BEGIN"),
        )
        .await
        .expect("open notation")
        .id;

        let clauses = crate::notation_clauses::for_notation(&surreal, notation_id)
            .await
            .unwrap();
        assert!(
            raw_body.contains(crate::notation_clauses::CUSTOM_CLAUSES_MARKER) && clauses.is_empty(),
            "the empty slot must fail the gate before the clause is seeded"
        );

        seed_no_charge_fee_clause(&surreal, notation_id)
            .await
            .unwrap();

        let clauses = crate::notation_clauses::for_notation(&surreal, notation_id)
            .await
            .unwrap();
        assert!(
            !(raw_body.contains(crate::notation_clauses::CUSTOM_CLAUSES_MARKER)
                && clauses.is_empty()),
            "the seeded no-charge clause must clear the clauses_required gate"
        );

        let rendered = crate::notation_clauses::splice(&raw_body, &clauses);
        let section_i = rendered.find("## I.").expect("§ I heading present");
        let section_ii = rendered.find("## II.").expect("§ II heading present");
        let clause_pos = rendered
            .find(NO_CHARGE_FEE_CLAUSE_BODY)
            .expect("no-charge clause rendered");
        assert!(
            section_i < clause_pos && clause_pos < section_ii,
            "the no-charge clause must render inside § I, before § II"
        );
    }

    /// Re-seeding the same notation — a retried CLI command, a re-run
    /// fixture — must not double the fee-basis paragraph the client signs.
    #[tokio::test]
    async fn seed_no_charge_fee_clause_is_idempotent() {
        let surreal = mem_surreal().await;
        let notation_id = crate::test_support::seed_notation(&surreal).await;

        let first = seed_no_charge_fee_clause(&surreal, notation_id)
            .await
            .unwrap();
        let second = seed_no_charge_fee_clause(&surreal, notation_id)
            .await
            .unwrap();
        assert_eq!(
            first, second,
            "re-seeding must reuse the existing clause id"
        );

        let clauses = crate::notation_clauses::for_notation(&surreal, notation_id)
            .await
            .unwrap();
        assert_eq!(clauses.len(), 1, "re-seeding must not duplicate the row");
        assert_eq!(clauses[0].body_markdown, NO_CHARGE_FEE_CLAUSE_BODY);
    }

    #[tokio::test]
    async fn seed_is_idempotent() {
        let surreal = mem_surreal().await;
        let first = seed_canonical(&surreal, &fs_storage().await)
            .await
            .expect("seed 1");
        let second = seed_canonical(&surreal, &fs_storage().await)
            .await
            .expect("seed 2");
        assert_eq!(second.questions_inserted, 0);
        assert_eq!(second.jurisdictions_inserted, 0);
        assert_eq!(second.persons_inserted, 0);
        assert!(first.questions_inserted > 0);
        let neon = crate::brands::find_by_key(&surreal, "neon")
            .await
            .unwrap()
            .expect("neon brand");
        assert_eq!(neon.typeface.as_deref(), Some("gorp-serif"));
        assert_eq!(neon.primary_color.as_deref(), Some("#007c91"));
        let dyd = crate::brands::find_by_key(&surreal, "delete-your-data")
            .await
            .unwrap()
            .expect("delete-your-data brand");
        assert_eq!(dyd.typeface.as_deref(), Some("plus-jakarta-sans"));
        assert_eq!(dyd.primary_color.as_deref(), Some("#b91c1c"));
    }

    #[tokio::test]
    async fn seeds_two_firms_with_iep_defenders_at_lex() {
        let surreal = mem_surreal().await;
        seed_canonical(&surreal, &fs_storage().await)
            .await
            .expect("seed");

        // Two firm-anchor entities, each delete-guarded by its own
        // anchor key.
        for name in super::FIRM_ENTITY_NAMES {
            let entity = crate::entities::find_by_name(&surreal, name)
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("canonical seed is missing entity {name}"));
            assert_eq!(
                entity.firm_anchor_key.as_deref(),
                Some(name.trim().to_lowercase()).as_deref(),
                "{name} must carry its own anchor key"
            );
        }

        // Two firm rows, one per firm-anchor entity.
        let firms = crate::firms::all(&surreal).await.unwrap();
        assert_eq!(firms.len(), 2, "the seed ships exactly two firms");
        let neon_entity = crate::entities::find_by_name(&surreal, super::FIRM_ENTITY_NAME)
            .await
            .unwrap()
            .expect("practice entity");
        let lex_entity = crate::entities::find_by_name(&surreal, super::LEX_FIRM_ENTITY_NAME)
            .await
            .unwrap()
            .expect("lex entity");
        let neon = crate::firms::find_by_entity_id(&surreal, neon_entity.id)
            .await
            .unwrap()
            .expect("practice firm");
        let lex = crate::firms::find_by_entity_id(&surreal, lex_entity.id)
            .await
            .unwrap()
            .expect("lex firm");
        assert_ne!(neon.id, lex.id);

        // The practice firm wears every closed house brand; Lex wears
        // only IEP Defenders.
        let mut neon_keys = crate::firms::brand_keys_for_firm(&surreal, neon.id)
            .await
            .unwrap();
        neon_keys.sort();
        let mut closed = crate::firms::CLOSED_BRAND_KEYS
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>();
        closed.sort();
        assert_eq!(neon_keys, closed);
        let lex_keys = crate::firms::brand_keys_for_firm(&surreal, lex.id)
            .await
            .unwrap();
        assert_eq!(lex_keys, vec!["iep-defenders".to_string()]);

        // The Lex key resolves to the Lex firm and to nothing else.
        assert_eq!(
            crate::firms::firm_id_for_brand_key(&surreal, "iep-defenders")
                .await
                .unwrap(),
            Some(lex.id)
        );
        let iep = crate::brands::find_by_key(&surreal, "iep-defenders")
            .await
            .unwrap()
            .expect("iep-defenders brand");
        assert_eq!(iep.firm_id, Some(lex.id));
        assert_eq!(iep.name, "IEP Defenders");
        assert_eq!(iep.typeface.as_deref(), Some("source-sans-3"));
        assert_eq!(iep.primary_color.as_deref(), Some("#5B21B6"));
        assert_eq!(
            iep.website_url.as_deref(),
            Some("https://www.iepdefenders.com")
        );

        // A second pass changes nothing: the two firms and their brand
        // associations are already exactly as seeded.
        let second = seed_canonical(&surreal, &fs_storage().await)
            .await
            .expect("reseed");
        assert_eq!(second.entities_inserted, 0);
        let firms_after = crate::firms::all(&surreal).await.unwrap();
        assert_eq!(firms_after.len(), 2);
        assert_eq!(
            crate::firms::firm_id_for_brand_key(&surreal, "iep")
                .await
                .unwrap(),
            Some(lex.id)
        );
    }

    #[tokio::test]
    async fn seed_is_idempotent_when_a_seeded_email_has_different_casing() {
        let surreal = mem_surreal().await;
        seed_canonical(&surreal, &fs_storage().await)
            .await
            .expect("initial seed");
        let nick = persons::find_by_email_ci(&surreal, "nick@neonlaw.com")
            .await
            .unwrap()
            .expect("nick exists");
        persons::edit(
            &surreal,
            nick.id,
            &crate::persons::PersonEdit {
                email: Some("Nick@NeonLaw.com".into()),
                ..crate::persons::PersonEdit::default()
            },
        )
        .await
        .expect("re-case Nick's email");

        let rerun = seed_canonical(&surreal, &fs_storage().await)
            .await
            .expect("re-seeding must resolve case-insensitive email references");
        assert_eq!(rerun.persons_inserted, 0);
        assert_eq!(
            persons::find_by_email_ci(&surreal, "nick@neonlaw.com")
                .await
                .unwrap()
                .expect("case-insensitive lookup preserves Nick")
                .role,
            Role::Admin
        );
    }

    #[tokio::test]
    async fn seeds_attorney_credentials_with_correct_numbers() {
        let surreal = mem_surreal().await;
        let report = seed_canonical(&surreal, &fs_storage().await)
            .await
            .expect("seed");
        let nick = persons::find_by_email_ci(&surreal, "nick@neonlaw.com")
            .await
            .unwrap()
            .expect("nick exists");
        let creds = crate::credentials::for_person(&surreal, nick.id)
            .await
            .unwrap();
        assert_eq!(creds.len(), 3, "expected NV + CA + WA admissions");
        // The state bar numbers are public-record disclosures; pin them
        // explicitly so a seed YAML edit can't silently change the
        // attorney advertising disclosure rendered on the firm site.
        let mut by_juris: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for c in &creds {
            let j = jurisdictions::find_by_id(&surreal, c.jurisdiction_id)
                .await
                .unwrap()
                .expect("the credential's jurisdiction is a seeded Surreal row");
            by_juris.insert(j.code, c.license_number.clone());
        }
        assert_eq!(by_juris.get("NV").map(String::as_str), Some("13400"));
        assert_eq!(by_juris.get("CA").map(String::as_str), Some("337252"));
        assert_eq!(by_juris.get("WA").map(String::as_str), Some("63446"));
        assert_eq!(report.credentials_inserted, 3);
    }

    #[tokio::test]
    async fn user_role_lifts_persons_to_admin() {
        let surreal = mem_surreal().await;
        seed_canonical(&surreal, &fs_storage().await)
            .await
            .expect("seed");
        let nick = persons::find_by_email_ci(&surreal, "nick@neonlaw.com")
            .await
            .unwrap()
            .expect("nick exists");
        assert_eq!(nick.role, Role::Admin);
    }

    #[test]
    fn firm_domain_convention_accepts_lowercase_neon_law_for_organization_roles() {
        use super::require_firm_domain;
        use crate::persons::Role;
        assert!(require_firm_domain("owner@neonlaw.com", Role::Owner).is_ok());
        assert!(require_firm_domain("nick@neonlaw.com", Role::Admin).is_ok());
        assert!(require_firm_domain("clerk@neonlaw.com", Role::Clerk).is_ok());
    }

    #[test]
    fn firm_domain_convention_rejects_mixed_case_privileged_emails() {
        use super::require_firm_domain;
        use crate::persons::Role;
        assert!(require_firm_domain("Owner@NeonLaw.com", Role::Owner).is_err());
        let err = require_firm_domain("Nick@NeonLaw.com", Role::Admin).unwrap_err();
        assert!(
            err.to_string().contains("lowercase"),
            "error should call out lowercase, got: {err}",
        );
        assert!(require_firm_domain("nick@NEONLAW.COM", Role::Admin).is_err());
    }

    #[test]
    fn firm_domain_convention_allows_any_domain_for_client() {
        use super::require_firm_domain;
        use crate::persons::Role;
        assert!(require_firm_domain("libra@example.com", Role::Client).is_ok());
        // Client rows aren't held to lowercase here; that's a normalization
        // concern for the persons table, not the seed convention.
        assert!(require_firm_domain("Libra@Example.com", Role::Client).is_ok());
    }

    #[test]
    fn firm_domain_convention_allows_an_external_lawyer() {
        use super::require_firm_domain;
        use crate::persons::Role;
        assert!(require_firm_domain("counsel@legalaid.example", Role::Lawyer).is_ok());
    }

    #[test]
    fn question_help_text_reads_the_authored_guidance() {
        use super::question_help_text;
        // Assert the authored string, not `is_some()`: a reader that
        // resolved to the wrong question, or to a placeholder, would still
        // satisfy a presence check.
        assert_eq!(
            question_help_text("entity").as_deref(),
            Some("Select the entity record this answer refers to."),
        );
        // A `__role` state inherits the base question's guidance, stripping
        // the suffix before lookup — so every role that reuses `person` gets
        // `person`'s help text.
        assert!(question_help_text("person").is_some());
        assert_eq!(
            question_help_text("person__trustee"),
            question_help_text("person")
        );
        // An unknown code answers `None` rather than panicking or inventing
        // an empty string a surface would render as a blank line.
        assert_eq!(question_help_text("no_such_question_code"), None);
    }

    #[test]
    fn every_seeded_question_resolves_its_help_text() {
        use super::{canonical, question_help_text, QuestionPresentationRec, Records};
        // The defect this reader closes is that guidance authored for every
        // question reached nobody. Pin the whole set rather than a sample: a
        // question added without `help_text:`, or a reader that quietly
        // stopped resolving, fails here instead of degrading into a
        // questionnaire step that silently shows no guidance.
        let parsed = serde_yaml::from_str::<Records<QuestionPresentationRec>>(canonical::QUESTION)
            .expect("Question.yaml parses");
        assert!(
            !parsed.records.is_empty(),
            "Question.yaml seeds no questions"
        );
        for rec in parsed.records {
            assert!(
                question_help_text(&rec.code).is_some(),
                "question `{}` has no help_text",
                rec.code,
            );
        }
    }

    /// The seed vocabulary is exactly the closed registry — every question
    /// is a glossary ORM model (record/reference), its plural list form, or
    /// a `custom_*` primitive. No bespoke per-matter codes. This grounds
    /// `Question.yaml` to `store::question_registry::QuestionType` so the two
    /// can never drift (issue #235).
    #[test]
    fn question_yaml_is_exactly_the_registry() {
        use std::collections::BTreeSet;
        let codes: BTreeSet<String> =
            super::parse::<super::QuestionRec>(super::canonical::QUESTION, "Question.yaml")
                .unwrap()
                .into_iter()
                .map(|q| q.code)
                .collect();
        let registry: BTreeSet<String> = crate::question_registry::QuestionType::all_tokens()
            .into_iter()
            .map(str::to_string)
            .collect();
        assert_eq!(
            codes, registry,
            "Question.yaml codes must be exactly store::question_registry::QuestionType"
        );
    }

    /// Every localized prompt maps to a real question code — no orphaned
    /// translations after a rename.
    #[test]
    fn firm_domain_convention_rejects_off_domain_organization_role_seeds() {
        use super::require_firm_domain;
        use crate::persons::Role;
        let err = require_firm_domain("libra@example.com", Role::Clerk).unwrap_err();
        assert!(
            err.to_string().contains("@neonlaw.com"),
            "error should mention the firm domain, got: {err}",
        );
        assert!(require_firm_domain("aries@mail.example.org", Role::Admin).is_err());
    }
}
