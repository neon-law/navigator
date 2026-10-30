//! Recoverable PostgreSQL mirror of the Person, Entity, and reference rows
//! ([`identity`]), the Firm, Brand, Project, and membership rows
//! ([`tenancy`]), the [`Post`], Social Handle, and [`SocialPublication`]
//! rows, and what a Person or Entity holds beside its own row: addresses,
//! credentials, email tokens, entity roles, delegations, external
//! identities, and mailbox claims ([`identity_detail`]).
//!
//! SurrealDB is the source of truth for every read and for every write's
//! outcome. After a Surreal write succeeds, [`after_write`] reads the row back
//! from SurrealDB by its key and upserts it into PostgreSQL, or deletes the
//! PostgreSQL row when SurrealDB no longer has it. The mirror is opt-in
//! ([`MIRROR_ENV`] set to `1` and `NAVIGATOR_POSTGRES_HOST` configured), bounded
//! by [`MIRROR_TIMEOUT`], including retries, and never returns an error to the writer.
//!
//! Each row keeps one stable identity in both stores: a Firm, Brand, Project,
//! Project participation, or Post by its record UUID, a `firm_brand` row by its
//! Brand key, a Firm membership by its Person and Firm, a Social Handle by its
//! Brand and provider, and a Social Publication by its Post and provider. Those are the unique keys both schemas enforce, so a
//! replay converges on the same row. An upsert writes only when a column
//! differs, so a replay changes nothing.
//!
//! A failed or timed-out mirror leaves the two stores divergent and logs the
//! table and sanitized error. The divergence is the durable record: [`parity`] compares every
//! row of the three editorial tables and names each divergent key, and
//! [`reconcile`] re-mirrors exactly those keys from SurrealDB. Deployment opt-in
//! installs a recurring reconciliation Job and requires a clean initial scan.
//! Recovery requires both stores and retained source rows; this is eventual
//! convergence, not a transaction spanning both databases or an event journal.
//! [`tenancy::parity`] and [`tenancy::reconcile`] do the same for the tenancy
//! tables, and [`identity::parity`] and [`identity::reconcile`] for the
//! identity tables. A tenancy row mirrors the Person or Entity it names on
//! its own. [`catalog::parity`] and [`catalog::reconcile`] cover the shared
//! catalogs: Authority, Glossary Term, Question, Notation package and its
//! templates, and Notation rate. [`identity_detail::parity`] and
//! [`identity_detail::reconcile`] cover the identity-detail tables; an
//! identity-detail row mirrors the Person, Entity, or Jurisdiction it names
//! on its own.
//! [`social_accounts::parity`] and [`social_accounts::reconcile`] compare
//! encrypted Social Account connections after their Firm, Brand, and Person
//! parents have been mirrored.
//! [`attestations::parity`] and [`attestations::reconcile`] retain each
//! durable on-chain outcome by its source UUID. PostgreSQL restricts deleting
//! its Notation while the record exists; a source-side orphan stays divergent.
//!
//! [`MIRRORS`] lists every mirror parents first: identity, then tenancy,
//! since a Firm names its Entity and a membership its Person, then Social
//! Accounts, since a connection names its Firm, Brand, and People, then documents
//! ([`documents`]), since an asset names its Project, then post, since a
//! Post needs its Brand, then identity detail, whose rows name only
//! identity rows, then catalog, since an Authority names its archived
//! asset, then workflow ([`workflow`]) and Templates ([`templates`]), whose
//! rows name their Project and body Asset, then Project Services whose rows
//! name a Project, Asset, identity rows, and optional sent email, then inbound email receipts and their delivery attempts,
//! then a Project's synced Matter Pages and Matter Caption ([`matter_pages_captions`]), keyed by their Project,
//! then the whole-row [`records`] groups, whose rows name Projects, People, Entities, and Notations.
//! [`parity_all`], [`reconcile_all`], and
//! [`drift`] back the operator command `navigator ops postgres mirror`.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, SubsecRound, Utc};
use sqlx::Row;
use uuid::Uuid;

use crate::postgres::PostgresDb;
use crate::posts::{Post, SocialLink};
use crate::social_handles::SocialHandle;
use crate::social_publications::SocialPublication;
use crate::surreal::SurrealDb;

pub mod attestations;
pub mod blog_videos;
pub mod catalog;
pub mod communications;
pub mod conversations;
pub mod deadlines;
pub mod documents;
pub mod email_deliveries;
pub mod firm_secrets;
pub mod identity;
pub mod identity_detail;
pub mod inbox;
pub mod leads;
pub mod mailrooms_letters;
pub mod matter_pages_captions;
pub mod notation_clauses;
pub mod project_references;
pub mod project_services;
pub mod records;
pub mod sent_emails;
pub mod social_account_selections;
pub mod social_accounts;
pub mod templates;
pub mod tenancy;
pub mod workflow;

/// Set to `1`, together with `NAVIGATOR_POSTGRES_HOST`, to mirror writes.
pub const MIRROR_ENV: &str = "NAVIGATOR_POSTGRES_MIRROR";

/// The total synchronous projection budget after a committed Surreal write.
pub const MIRROR_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy)]
struct CompletionPolicy {
    attempts: usize,
    attempt_timeout: Duration,
    timeout: Duration,
    retry_delay: Duration,
}

const COMPLETION: CompletionPolicy = CompletionPolicy {
    attempts: 3,
    attempt_timeout: Duration::from_secs(4),
    timeout: MIRROR_TIMEOUT,
    retry_delay: Duration::from_millis(250),
};

/// The stable identity of one mirrored row.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MirrorKey {
    SentEmail(Uuid),
    EmailReceipt(Uuid),
    EmailDelivery(Uuid),
    EmailDeliveryAttempt(Uuid),
    Post(Uuid),
    BlogVideo(Uuid),
    SocialHandle {
        brand_key: String,
        provider: String,
    },
    SocialPublication {
        post_id: Uuid,
        provider: String,
    },
    SocialAccount(String),
    SocialAccountSelection {
        brand_key: String,
        provider: String,
    },
    FirmIntegrationSecrets(Uuid),
    Firm(Uuid),
    Brand(Uuid),
    FirmBrand {
        brand_key: String,
    },
    Project(Uuid),
    ProjectReference {
        project_id: Uuid,
        party_kind: String,
        party_id: Uuid,
        reason: String,
    },
    FirmMembership {
        person_id: Uuid,
        firm_id: Uuid,
    },
    ProjectParticipation(Uuid),
    Jurisdiction(Uuid),
    EntityType(Uuid),
    Entity(Uuid),
    Person(Uuid),
    Authority(Uuid),
    GlossaryTerm(Uuid),
    Question(Uuid),
    NotationPackage(Uuid),
    NotationPackageTemplate(Uuid),
    NotationRate(Uuid),
    Asset(Uuid),
    Notation(Uuid),
    NotationEvent(Uuid),
    Answer(Uuid),
    NotationClause(Uuid),
    Attestation(Uuid),
    Address(Uuid),
    Credential(Uuid),
    EmailToken(Uuid),
    EntityRole {
        person_id: Uuid,
        entity_id: Uuid,
        role: String,
    },
    PersonDelegate(Uuid),
    ExternalIdentity(Uuid),
    /// Every mailbox one Person claims, keyed by the Person: the claim's
    /// own key is an email.
    PersonMailbox(Uuid),
    SummarySenderApproval(Uuid),
    SummarySenderApprovalEvent(Uuid),
    ProjectDeadline(Uuid),
    ProjectService(Uuid),
    Communication(Uuid),
    EmailConversation(Uuid),
    EmailConversationMessage(Uuid),
    InboxConversation(Uuid),
    InboxMessage(Uuid),
    InboxIngress(Uuid),
    InboxAssignment(Uuid),
    InboxDeliveryAttempt(Uuid),
    InboxTriage(Uuid),
    Mailroom(Uuid),
    Letter(Uuid),
    Lead(Uuid),
    LeadConsent(Uuid),
    Template(Uuid),
    /// Every Matter Page of one Project, keyed by the Project: a sync
    /// replaces the whole set, and each page's own id changes with it.
    MatterPages(Uuid),
    /// The one Matter Caption of a Project, keyed by the Project.
    MatterCaption(Uuid),
    /// One row of a table [`records`] mirrors whole, keyed by its record UUID.
    Record {
        table: &'static str,
        id: Uuid,
    },
    /// A generic source record whose SurrealDB record key is a domain string.
    RecordString {
        table: &'static str,
        id: String,
    },
}

impl MirrorKey {
    /// A divergent row's UUID key, or `None` when its key carries text that
    /// could identify a client, such as a Brand key.
    #[must_use]
    pub fn report_id(&self) -> Option<String> {
        match self {
            Self::SentEmail(id)
            | Self::Post(id)
            | Self::BlogVideo(id)
            | Self::Firm(id)
            | Self::Brand(id)
            | Self::Project(id)
            | Self::ProjectParticipation(id)
            | Self::Jurisdiction(id)
            | Self::EntityType(id)
            | Self::Entity(id)
            | Self::Person(id)
            | Self::Authority(id)
            | Self::GlossaryTerm(id)
            | Self::Question(id)
            | Self::NotationPackage(id)
            | Self::NotationPackageTemplate(id)
            | Self::NotationRate(id)
            | Self::Asset(id)
            | Self::Template(id)
            | Self::Notation(id)
            | Self::NotationEvent(id)
            | Self::Answer(id)
            | Self::Attestation(id)
            | Self::Address(id)
            | Self::Credential(id)
            | Self::EmailToken(id)
            | Self::PersonDelegate(id)
            | Self::ExternalIdentity(id)
            | Self::PersonMailbox(id)
            | Self::Communication(id)
            | Self::EmailConversation(id)
            | Self::EmailConversationMessage(id) => Some(id.to_string()),
            Self::EntityRole {
                person_id,
                entity_id,
                ..
            } => Some(format!("{person_id}/{entity_id}")),
            Self::SocialPublication { post_id, .. } => Some(post_id.to_string()),
            Self::FirmMembership { person_id, firm_id } => Some(format!("{firm_id}/{person_id}")),
            _ => None,
        }
    }

    /// The table the row lives in, named the same in both stores.
    #[must_use]
    pub fn table(&self) -> &'static str {
        match self {
            Self::SentEmail(_) => "sent_email",
            Self::EmailReceipt(_) => "email_receipt",
            Self::EmailDelivery(_) => "email_delivery",
            Self::EmailDeliveryAttempt(_) => "email_delivery_attempt",
            Self::Post(_) => "post",
            Self::BlogVideo(_) => "blog_video",
            Self::SocialHandle { .. } => "social_handle",
            Self::SocialPublication { .. } => "social_publication",
            Self::SocialAccount(_) => "social_account",
            Self::SocialAccountSelection { .. } => "social_account_selection",
            Self::FirmIntegrationSecrets(_) => "firm_integration_secret",
            Self::Firm(_) => "firm",
            Self::Brand(_) => "brand",
            Self::FirmBrand { .. } => "firm_brand",
            Self::Project(_) => "project",
            Self::ProjectReference { .. } => "project_reference",
            Self::FirmMembership { .. } => "person_firm_role",
            Self::ProjectParticipation(_) => "person_project_role",
            Self::Jurisdiction(_) => "jurisdiction",
            Self::EntityType(_) => "entity_type",
            Self::Entity(_) => "entity",
            Self::Person(_) => "person",
            Self::Authority(_) => "authority",
            Self::GlossaryTerm(_) => "glossary_term",
            Self::Question(_) => "question",
            Self::NotationPackage(_) => "notation_package",
            Self::NotationPackageTemplate(_) => "notation_package_template",
            Self::NotationRate(_) => "notation_rate",
            Self::Asset(_) => "asset",
            Self::Template(_) => "template",
            Self::Notation(_) => "notation",
            Self::NotationEvent(_) => "notation_event",
            Self::Answer(_) => "answer",
            Self::NotationClause(_) => "notation_clause",
            Self::Attestation(_) => "attestation",
            Self::Address(_) => "address",
            Self::Credential(_) => "credential",
            Self::EmailToken(_) => "email_token",
            Self::EntityRole { .. } => "entity_role",
            Self::PersonDelegate(_) => "person_delegate",
            Self::ExternalIdentity(_) => "person_external_identity",
            Self::PersonMailbox(_) => "person_mailbox",
            Self::SummarySenderApproval(_) => "summary_sender_approval",
            Self::SummarySenderApprovalEvent(_) => "summary_sender_approval_event",
            Self::ProjectDeadline(_) => "project_deadlines",
            Self::ProjectService(_) => "project_service",
            Self::Communication(_) => "communication",
            Self::EmailConversation(_) => "email_conversation",
            Self::EmailConversationMessage(_) => "email_conversation_message",
            Self::InboxConversation(_) => "inbox_conversation",
            Self::InboxMessage(_) => "message",
            Self::InboxIngress(_) => "inbox_ingress",
            Self::InboxAssignment(_) => "inbox_assignment",
            Self::InboxDeliveryAttempt(_) => "inbox_delivery_attempt",
            Self::InboxTriage(_) => "inbox_triage",
            Self::Mailroom(_) => "mailroom",
            Self::Letter(_) => "letter",
            Self::Lead(_) => "lead",
            Self::LeadConsent(_) => "lead_consent",
            Self::MatterPages(_) => "matter_page",
            Self::MatterCaption(_) => "matter_caption",
            Self::Record { table, .. } | Self::RecordString { table, .. } => table,
        }
    }
}

impl std::fmt::Display for MirrorKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SentEmail(id) => write!(f, "sent_email:{id}"),
            Self::EmailReceipt(id) => write!(f, "email_receipt:{id}"),
            Self::EmailDelivery(id) => write!(f, "email_delivery:{id}"),
            Self::EmailDeliveryAttempt(id) => write!(f, "email_delivery_attempt:{id}"),
            Self::Post(id) => write!(f, "post:{id}"),
            Self::BlogVideo(id) => write!(f, "blog_video:{id}"),
            Self::SocialHandle {
                brand_key,
                provider,
            } => write!(f, "social_handle:{brand_key}/{provider}"),
            Self::SocialPublication { post_id, provider } => {
                write!(f, "social_publication:{post_id}/{provider}")
            }
            Self::SocialAccount(key) => f.write_str(key),
            Self::SocialAccountSelection {
                brand_key,
                provider,
            } => {
                write!(f, "social_account_selection:{brand_key}:{provider}")
            }
            Self::FirmIntegrationSecrets(id) => write!(f, "firm_integration_secret:{id}"),
            Self::Firm(id) => write!(f, "firm:{id}"),
            Self::Brand(id) => write!(f, "brand:{id}"),
            Self::FirmBrand { brand_key } => write!(f, "firm_brand:{brand_key}"),
            Self::Project(id) => write!(f, "project:{id}"),
            Self::ProjectReference {
                project_id,
                party_kind,
                party_id,
                reason,
            } => write!(
                f,
                "project_reference:{project_id}/{party_kind}:{party_id}/{reason}"
            ),
            Self::FirmMembership { person_id, firm_id } => {
                write!(f, "person_firm_role:{firm_id}/{person_id}")
            }
            Self::ProjectParticipation(id) => write!(f, "person_project_role:{id}"),
            Self::Jurisdiction(id) => write!(f, "jurisdiction:{id}"),
            Self::EntityType(id) => write!(f, "entity_type:{id}"),
            Self::Entity(id) => write!(f, "entity:{id}"),
            Self::Person(id) => write!(f, "person:{id}"),
            Self::Authority(id)
            | Self::GlossaryTerm(id)
            | Self::Question(id)
            | Self::NotationPackage(id)
            | Self::NotationPackageTemplate(id)
            | Self::NotationRate(id) => write!(f, "{}:{id}", self.table()),
            Self::Asset(id) => write!(f, "asset:{id}"),
            Self::Notation(id) => write!(f, "notation:{id}"),
            Self::NotationEvent(id) => write!(f, "notation_event:{id}"),
            Self::Answer(id) => write!(f, "answer:{id}"),
            Self::NotationClause(id) => write!(f, "notation_clause:{id}"),
            Self::Attestation(id) => write!(f, "attestation:{id}"),
            Self::Address(id) => write!(f, "address:{id}"),
            Self::Credential(id) => write!(f, "credential:{id}"),
            Self::EmailToken(id) => write!(f, "email_token:{id}"),
            Self::EntityRole {
                person_id,
                entity_id,
                role,
            } => write!(f, "entity_role:{person_id}/{entity_id}/{role}"),
            Self::PersonDelegate(id) => write!(f, "person_delegate:{id}"),
            Self::ExternalIdentity(id) => write!(f, "person_external_identity:{id}"),
            Self::PersonMailbox(person_id) => write!(f, "person_mailbox:{person_id}"),
            Self::Template(id) => write!(f, "template:{id}"),
            Self::SummarySenderApproval(id) => write!(f, "summary_sender_approval:{id}"),
            Self::SummarySenderApprovalEvent(id) => write!(f, "summary_sender_approval_event:{id}"),
            Self::ProjectDeadline(id) => write!(f, "project_deadlines:{id}"),
            Self::ProjectService(id) => write!(f, "project_service:{id}"),
            Self::Communication(id) => write!(f, "communication:{id}"),
            Self::EmailConversation(id) => write!(f, "email_conversation:{id}"),
            Self::EmailConversationMessage(id) => write!(f, "email_conversation_message:{id}"),
            Self::InboxConversation(id) => write!(f, "inbox_conversation:{id}"),
            Self::InboxMessage(id) => write!(f, "message:{id}"),
            Self::InboxIngress(id) => write!(f, "inbox_ingress:{id}"),
            Self::InboxAssignment(id) => write!(f, "inbox_assignment:{id}"),
            Self::InboxDeliveryAttempt(id) => write!(f, "inbox_delivery_attempt:{id}"),
            Self::InboxTriage(id) => write!(f, "inbox_triage:{id}"),
            Self::Mailroom(id) => write!(f, "mailroom:{id}"),
            Self::Letter(id) => write!(f, "letter:{id}"),
            Self::Lead(id) => write!(f, "lead:{id}"),
            Self::LeadConsent(id) => write!(f, "lead_consent:{id}"),
            Self::MatterPages(project_id) => write!(f, "matter_page:{project_id}"),
            Self::MatterCaption(project_id) => write!(f, "matter_caption:{project_id}"),
            Self::Record { table, id } => write!(f, "{table}:{id}"),
            Self::RecordString { table, id } => write!(f, "{table}:{id}"),
        }
    }
}

/// What one mirror did to PostgreSQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mirrored {
    /// The row was inserted or a column changed.
    Written,
    /// SurrealDB has no such row, and PostgreSQL's copy was removed.
    Deleted,
    /// PostgreSQL already matched SurrealDB.
    Unchanged,
}

/// What [`after_write`] did. The writer never acts on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorOutcome {
    /// The mirror is not configured.
    Disabled,
    Mirrored(Mirrored),
    /// The mirror failed or timed out; [`parity`] will report the row.
    Failed,
}

/// Why one mirror failed. Carries no row values.
#[derive(Debug, thiserror::Error)]
pub enum MirrorError {
    #[error("the SurrealDB source row could not be read")]
    Source,
    #[error("the SurrealDB source row carries an unreadable timestamp")]
    SourceTimestamp,
    #[error("the SurrealDB source row in table {table} has {count} unmapped field(s); update its PostgreSQL import mapping")]
    UnmappedSourceFields { table: &'static str, count: usize },
    #[error("PostgreSQL is unavailable")]
    Unavailable,
    #[error("PostgreSQL mirror completion timed out")]
    Timeout,
    #[error("PostgreSQL refused the row (SQLSTATE {0})")]
    Target(String),
}

impl MirrorError {
    fn retryable(&self) -> bool {
        match self {
            Self::Source | Self::Unavailable | Self::Timeout => true,
            Self::SourceTimestamp | Self::UnmappedSourceFields { .. } => false,
            Self::Target(code) => {
                code.starts_with("08")
                    || matches!(
                        code.as_str(),
                        "40001" | "40P01" | "55P03" | "53300" | "57P01" | "57P02" | "57P03"
                    )
            }
        }
    }
}

// Retry only the projection, reading the current source again on every
// attempt. Never replay the domain mutation or any external effect.
async fn complete<F, Fut>(policy: CompletionPolicy, mut attempt: F) -> Result<Mirrored, MirrorError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Mirrored, MirrorError>>,
{
    tokio::time::timeout(policy.timeout, async {
        for index in 0..policy.attempts {
            let result = tokio::time::timeout(policy.attempt_timeout, attempt())
                .await
                .unwrap_or(Err(MirrorError::Timeout));
            match result {
                Ok(mirrored) => return Ok(mirrored),
                Err(error) if !error.retryable() || index + 1 == policy.attempts => {
                    return Err(error);
                }
                Err(_) => tokio::time::sleep(policy.retry_delay).await,
            }
        }
        Err(MirrorError::Unavailable)
    })
    .await
    .unwrap_or(Err(MirrorError::Timeout))
}

impl From<sqlx::Error> for MirrorError {
    fn from(error: sqlx::Error) -> Self {
        match error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
        {
            Some(code) => Self::Target(code.into_owned()),
            None => Self::Unavailable,
        }
    }
}

/// Whether the environment opts this process into the mirror.
#[must_use]
pub fn enabled() -> bool {
    enabled_by(
        std::env::var(MIRROR_ENV).ok().as_deref(),
        std::env::var("NAVIGATOR_POSTGRES_HOST").ok().as_deref(),
    )
}

fn enabled_by(flag: Option<&str>, host: Option<&str>) -> bool {
    flag == Some("1") && host.is_some_and(|host| !host.trim().is_empty())
}

/// Mirror one row after its Surreal write succeeded.
///
/// Returns [`MirrorOutcome::Disabled`] without touching PostgreSQL when the
/// mirror is not configured. A failure or a timeout is logged by table and
/// returned as [`MirrorOutcome::Failed`]; it is never an error, so it can
/// neither fail nor roll back the Surreal write.
pub async fn after_write(surreal: &SurrealDb, key: MirrorKey) -> MirrorOutcome {
    if !enabled() {
        return MirrorOutcome::Disabled;
    }
    // Boxed behind `dyn Future` so every writer that awaits this hook keeps
    // a small, shallow future type instead of embedding the whole mirror.
    let attempt = || -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Mirrored, MirrorError>> + Send + '_>,
    > { Box::pin(async {
        let pg = PostgresDb::from_env()
            .await
            .map_err(|_| MirrorError::Unavailable)?;
        mirror(surreal, &pg, &key).await
    }) };
    match complete(COMPLETION, attempt).await {
        Ok(mirrored) => MirrorOutcome::Mirrored(mirrored),
        Err(error) => {
            tracing::error!(table = key.table(), error = %error, "PostgreSQL mirror incomplete; recurring reconciliation must repair durable drift");
            MirrorOutcome::Failed
        }
    }
}

/// Mirror several rows after one Surreal write, in order. Name parents
/// before children on a create, and children before parents on a delete.
pub async fn after_writes(surreal: &SurrealDb, keys: impl IntoIterator<Item = MirrorKey>) {
    if !enabled() {
        return;
    }
    for key in keys {
        after_write(surreal, key).await;
    }
}

/// Make PostgreSQL's copy of one row match SurrealDB's.
///
/// # Errors
///
/// SurrealDB could not be read, or PostgreSQL refused the write.
pub async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    key: &MirrorKey,
) -> Result<Mirrored, MirrorError> {
    match key {
        MirrorKey::SentEmail(id) => sent_emails::mirror(surreal, pg, *id).await,
        key @ (MirrorKey::InboxConversation(_)
        | MirrorKey::InboxMessage(_)
        | MirrorKey::InboxIngress(_)
        | MirrorKey::InboxAssignment(_)
        | MirrorKey::InboxDeliveryAttempt(_)
        | MirrorKey::InboxTriage(_)) => inbox::mirror(surreal, pg, key).await,
        MirrorKey::SocialAccount(key) => social_accounts::mirror(surreal, pg, key).await,
        key @ MirrorKey::SocialAccountSelection { .. } => {
            social_account_selections::mirror(surreal, pg, key).await
        }
        MirrorKey::FirmIntegrationSecrets(id) => firm_secrets::mirror(surreal, pg, *id).await,
        key @ (MirrorKey::EmailReceipt(_)
        | MirrorKey::EmailDelivery(_)
        | MirrorKey::EmailDeliveryAttempt(_)) => email_deliveries::mirror(surreal, pg, key).await,
        key @ MirrorKey::ProjectReference { .. } => {
            project_references::mirror(surreal, pg, key).await
        }
        MirrorKey::Post(id) => {
            match crate::posts::find_by_id(surreal, *id)
                .await
                .map_err(|_| MirrorError::Source)?
            {
                Some(post) => upsert_post(pg, &post).await,
                None => delete_post(pg, *id).await,
            }
        }
        MirrorKey::BlogVideo(id) => blog_videos::mirror(surreal, pg, *id).await,
        MirrorKey::SocialHandle {
            brand_key,
            provider,
        } => mirror_social_handle(surreal, pg, brand_key, provider).await,
        MirrorKey::SocialPublication { post_id, provider } => {
            let source = crate::social_publications::find(surreal, *post_id, provider)
                .await
                .map_err(|_| MirrorError::Source)?;
            if let Some(publication) = source {
                return upsert_publication(pg, &publication).await;
            }
            let deleted =
                sqlx::query("DELETE FROM social_publication WHERE post_id = $1 AND provider = $2")
                    .bind(post_id)
                    .bind(provider)
                    .execute(&pg.0)
                    .await?;
            Ok(changed(deleted.rows_affected(), Mirrored::Deleted))
        }
        MirrorKey::Firm(_)
        | MirrorKey::Brand(_)
        | MirrorKey::FirmBrand { .. }
        | MirrorKey::Project(_)
        | MirrorKey::FirmMembership { .. }
        | MirrorKey::ProjectParticipation(_) => tenancy::mirror(surreal, pg, key).await,
        MirrorKey::Jurisdiction(_)
        | MirrorKey::EntityType(_)
        | MirrorKey::Entity(_)
        | MirrorKey::Person(_) => identity::mirror(surreal, pg, key).await,
        MirrorKey::Authority(_)
        | MirrorKey::GlossaryTerm(_)
        | MirrorKey::Question(_)
        | MirrorKey::NotationPackage(_)
        | MirrorKey::NotationPackageTemplate(_)
        | MirrorKey::NotationRate(_) => catalog::mirror(surreal, pg, key).await,
        MirrorKey::Asset(_) => documents::mirror(surreal, pg, key).await,
        MirrorKey::Template(id) => templates::mirror(surreal, pg, *id).await,
        MirrorKey::Notation(_) | MirrorKey::NotationEvent(_) | MirrorKey::Answer(_) => {
            workflow::mirror(surreal, pg, key).await
        }
        MirrorKey::NotationClause(id) => notation_clauses::mirror(surreal, pg, *id).await,
        MirrorKey::Attestation(id) => attestations::mirror(surreal, pg, *id).await,
        MirrorKey::Address(_)
        | MirrorKey::Credential(_)
        | MirrorKey::EmailToken(_)
        | MirrorKey::EntityRole { .. }
        | MirrorKey::PersonDelegate(_)
        | MirrorKey::ExternalIdentity(_)
        | MirrorKey::PersonMailbox(_) => identity_detail::mirror(surreal, pg, key).await,
        MirrorKey::SummarySenderApproval(_) | MirrorKey::SummarySenderApprovalEvent(_) => {
            communications::mirror(surreal, pg, key).await
        }
        MirrorKey::ProjectDeadline(_) => deadlines::mirror(surreal, pg, key).await,
        MirrorKey::ProjectService(_) => project_services::mirror(surreal, pg, key).await,
        MirrorKey::Communication(_)
        | MirrorKey::EmailConversation(_)
        | MirrorKey::EmailConversationMessage(_) => conversations::mirror(surreal, pg, key).await,
        MirrorKey::Mailroom(_) | MirrorKey::Letter(_) => {
            mailrooms_letters::mirror(surreal, pg, key).await
        }
        MirrorKey::Lead(_) | MirrorKey::LeadConsent(_) => leads::mirror(surreal, pg, key).await,
        MirrorKey::MatterPages(_) | MirrorKey::MatterCaption(_) => {
            matter_pages_captions::mirror(surreal, pg, key).await
        }
        MirrorKey::Record { table, id } => records::mirror(surreal, pg, table, *id).await,
        MirrorKey::RecordString { table, id } => {
            records::mirror_string(surreal, pg, table, id).await
        }
    }
}

async fn mirror_social_handle(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    brand_key: &str,
    provider: &str,
) -> Result<Mirrored, MirrorError> {
    let source = crate::social_handles::find(surreal, brand_key, provider)
        .await
        .map_err(|_| MirrorError::Source)?;
    if let Some(handle) = source {
        return upsert_handle(pg, &handle).await;
    }
    let deleted = sqlx::query("DELETE FROM social_handle WHERE brand_key = $1 AND provider = $2")
        .bind(brand_key)
        .bind(provider)
        .execute(&pg.0)
        .await?;
    Ok(changed(deleted.rows_affected(), Mirrored::Deleted))
}

fn changed(rows: u64, effect: Mirrored) -> Mirrored {
    if rows == 0 {
        Mirrored::Unchanged
    } else {
        effect
    }
}

/// PostgreSQL stores microseconds; SurrealDB stores nanoseconds.
fn micros(at: DateTime<Utc>) -> DateTime<Utc> {
    at.trunc_subsecs(6)
}

async fn upsert_post(pg: &PostgresDb, post: &Post) -> Result<Mirrored, MirrorError> {
    let mut tx = pg.0.begin().await?;
    // A Brand+slug held in PostgreSQL by another id is a Post SurrealDB no
    // longer has under that identity; SurrealDB's row replaces it.
    let stale = sqlx::query(
        "DELETE FROM social_publication WHERE post_id IN \
         (SELECT id FROM post WHERE brand_key = $1 AND slug = $2 AND id <> $3)",
    )
    .bind(&post.brand_key)
    .bind(&post.slug)
    .bind(post.id)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        + sqlx::query("DELETE FROM post WHERE brand_key = $1 AND slug = $2 AND id <> $3")
            .bind(&post.brand_key)
            .bind(&post.slug)
            .bind(post.id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    let written = sqlx::query(
        "INSERT INTO post (id, brand_key, slug, title, description, body_html, body_markdown, \
         status, publish_at, video_asset, tiktok_ai_generated, vertical_video, video_orientation, \
         source_digest, social_links, facebook_caption, instagram_caption, threads_caption, \
         youtube_caption, selected_social_accounts, inserted_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, \
         $18, $19, $20, $21, $22) \
         ON CONFLICT (id) DO UPDATE SET brand_key = EXCLUDED.brand_key, slug = EXCLUDED.slug, \
         title = EXCLUDED.title, description = EXCLUDED.description, \
         body_html = EXCLUDED.body_html, body_markdown = EXCLUDED.body_markdown, \
         status = EXCLUDED.status, publish_at = EXCLUDED.publish_at, \
         video_asset = EXCLUDED.video_asset, tiktok_ai_generated = EXCLUDED.tiktok_ai_generated, \
         vertical_video = EXCLUDED.vertical_video, video_orientation = EXCLUDED.video_orientation, \
         source_digest = EXCLUDED.source_digest, social_links = EXCLUDED.social_links, \
         facebook_caption = EXCLUDED.facebook_caption, \
         instagram_caption = EXCLUDED.instagram_caption, \
         threads_caption = EXCLUDED.threads_caption, youtube_caption = EXCLUDED.youtube_caption, \
         selected_social_accounts = EXCLUDED.selected_social_accounts, \
         inserted_at = EXCLUDED.inserted_at, updated_at = EXCLUDED.updated_at \
         WHERE (post.brand_key, post.slug, post.title, post.description, post.body_html, \
         post.body_markdown, post.status, post.publish_at, post.video_asset, \
         post.tiktok_ai_generated, post.vertical_video, post.video_orientation, \
         post.source_digest, post.social_links, post.facebook_caption, post.instagram_caption, \
         post.threads_caption, post.youtube_caption, post.selected_social_accounts, \
         post.inserted_at, post.updated_at) IS DISTINCT FROM \
         (EXCLUDED.brand_key, EXCLUDED.slug, EXCLUDED.title, EXCLUDED.description, \
         EXCLUDED.body_html, EXCLUDED.body_markdown, EXCLUDED.status, EXCLUDED.publish_at, \
         EXCLUDED.video_asset, EXCLUDED.tiktok_ai_generated, EXCLUDED.vertical_video, \
         EXCLUDED.video_orientation, EXCLUDED.source_digest, EXCLUDED.social_links, \
         EXCLUDED.facebook_caption, EXCLUDED.instagram_caption, EXCLUDED.threads_caption, \
         EXCLUDED.youtube_caption, EXCLUDED.selected_social_accounts, EXCLUDED.inserted_at, \
         EXCLUDED.updated_at)",
    )
    .bind(post.id)
    .bind(&post.brand_key)
    .bind(&post.slug)
    .bind(&post.title)
    .bind(&post.description)
    .bind(&post.body_html)
    .bind(&post.body_markdown)
    .bind(&post.status)
    .bind(post.publish_at.map(micros))
    .bind(&post.video_asset)
    .bind(post.tiktok_ai_generated)
    .bind(post.vertical_video)
    .bind(&post.video_orientation)
    .bind(&post.source_digest)
    .bind(sqlx::types::Json(&post.social_links))
    .bind(&post.facebook_caption)
    .bind(&post.instagram_caption)
    .bind(&post.threads_caption)
    .bind(&post.youtube_caption)
    .bind(&post.selected_social_accounts)
    .bind(micros(post.inserted_at))
    .bind(micros(post.updated_at))
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(changed(stale + written, Mirrored::Written))
}

async fn delete_post(pg: &PostgresDb, id: Uuid) -> Result<Mirrored, MirrorError> {
    let mut tx = pg.0.begin().await?;
    let publications = sqlx::query("DELETE FROM social_publication WHERE post_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    let posts = sqlx::query("DELETE FROM post WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(changed(publications + posts, Mirrored::Deleted))
}

async fn upsert_handle(pg: &PostgresDb, handle: &SocialHandle) -> Result<Mirrored, MirrorError> {
    let written = sqlx::query(
        "INSERT INTO social_handle (brand_key, provider, handle) VALUES ($1, $2, $3) \
         ON CONFLICT (brand_key, provider) DO UPDATE SET handle = EXCLUDED.handle \
         WHERE social_handle.handle IS DISTINCT FROM EXCLUDED.handle",
    )
    .bind(&handle.brand_key)
    .bind(&handle.provider)
    .bind(&handle.handle)
    .execute(&pg.0)
    .await?;
    Ok(changed(written.rows_affected(), Mirrored::Written))
}

async fn upsert_publication(
    pg: &PostgresDb,
    publication: &SocialPublication,
) -> Result<Mirrored, MirrorError> {
    let attempts = i32::try_from(publication.attempt_count).map_err(|_| MirrorError::Source)?;
    let written = sqlx::query(
        "INSERT INTO social_publication (post_id, provider, state, attempt_count, \
         provider_task_id, provider_post_id, permalink, last_error, inserted_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
         ON CONFLICT (post_id, provider) DO UPDATE SET state = EXCLUDED.state, \
         attempt_count = EXCLUDED.attempt_count, provider_task_id = EXCLUDED.provider_task_id, \
         provider_post_id = EXCLUDED.provider_post_id, permalink = EXCLUDED.permalink, \
         last_error = EXCLUDED.last_error, inserted_at = EXCLUDED.inserted_at, \
         updated_at = EXCLUDED.updated_at \
         WHERE (social_publication.state, social_publication.attempt_count, \
         social_publication.provider_task_id, social_publication.provider_post_id, \
         social_publication.permalink, social_publication.last_error, \
         social_publication.inserted_at, social_publication.updated_at) IS DISTINCT FROM \
         (EXCLUDED.state, EXCLUDED.attempt_count, EXCLUDED.provider_task_id, \
         EXCLUDED.provider_post_id, EXCLUDED.permalink, EXCLUDED.last_error, \
         EXCLUDED.inserted_at, EXCLUDED.updated_at)",
    )
    .bind(publication.post_id)
    .bind(&publication.provider)
    .bind(&publication.state)
    .bind(attempts)
    .bind(&publication.provider_task_id)
    .bind(&publication.provider_post_id)
    .bind(&publication.permalink)
    .bind(&publication.last_error)
    .bind(micros(publication.inserted_at))
    .bind(micros(publication.updated_at))
    .execute(&pg.0)
    .await?;
    Ok(changed(written.rows_affected(), Mirrored::Written))
}

/// One table's comparison between the two stores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableParity {
    pub table: &'static str,
    pub source_rows: usize,
    pub target_rows: usize,
    /// Keys missing from either store or whose columns differ.
    pub divergent: Vec<MirrorKey>,
}

/// The comparison of every mirrored table. It names rows by key only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParityReport {
    pub tables: Vec<TableParity>,
}

impl ParityReport {
    /// Whether both stores hold the same rows with the same columns.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.tables.iter().all(|table| table.divergent.is_empty())
    }

    /// Every divergent key, Posts first so a publication's Post exists first.
    pub fn divergent(&self) -> impl Iterator<Item = &MirrorKey> {
        self.tables.iter().flat_map(|table| table.divergent.iter())
    }
}

/// Comparable form of a mirrored row.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Columns {
    Inbox(serde_json::Value),
    SentEmail(Box<sent_emails::SentEmailImage>),
    Post(Box<Post>),
    Handle(String),
    Publication(Box<SocialPublication>),
    SummarySenderApproval(Box<communications::ApprovalImage>),
    SummarySenderApprovalEvent(Box<communications::ApprovalEventImage>),
    ProjectDeadline(Box<deadlines::DeadlineImage>),
    ProjectService(Box<project_services::ProjectServiceImage>),
    Communication(Box<conversations::CommunicationImage>),
    EmailConversation(Box<conversations::ConversationImage>),
    EmailConversationMessage(Box<conversations::MessageImage>),
    EmailReceipt(Box<email_deliveries::ReceiptImage>),
    EmailDelivery(Box<email_deliveries::DeliveryImage>),
    EmailDeliveryAttempt(Box<email_deliveries::AttemptImage>),
    SocialAccount(Box<social_accounts::SocialAccountImage>),
    SocialAccountSelection(Box<social_account_selections::SelectionImage>),
    Lead(Box<leads::LeadImage>),
    LeadConsent(Box<leads::ConsentImage>),
    MatterPages(Vec<matter_pages_captions::PageImage>),
    MatterCaption(Box<matter_pages_captions::CaptionImage>),
    Record(serde_json::Value),
}

fn compare(
    table: &'static str,
    source: &BTreeMap<MirrorKey, Columns>,
    target: &BTreeMap<MirrorKey, Columns>,
) -> TableParity {
    let mut divergent: Vec<MirrorKey> = source
        .iter()
        .filter(|(key, columns)| target.get(*key) != Some(*columns))
        .map(|(key, _)| key.clone())
        .collect();
    divergent.extend(
        target
            .keys()
            .filter(|key| !source.contains_key(*key))
            .cloned(),
    );
    divergent.sort();
    TableParity {
        table,
        source_rows: source.len(),
        target_rows: target.len(),
        divergent,
    }
}

/// Compare every Post, Social Handle, and Social Publication in both stores.
///
/// Reads whole tables; these editorial tables are small. Timestamps compare
/// at PostgreSQL's microsecond precision.
///
/// # Errors
///
/// Either store could not be read.
pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let source_posts: BTreeMap<_, _> = crate::posts::all(surreal)
        .await
        .map_err(|_| MirrorError::Source)?
        .into_iter()
        .map(|mut post| {
            post.publish_at = post.publish_at.map(micros);
            post.inserted_at = micros(post.inserted_at);
            post.updated_at = micros(post.updated_at);
            (MirrorKey::Post(post.id), Columns::Post(Box::new(post)))
        })
        .collect();
    let target_posts: BTreeMap<_, _> = target_posts(pg)
        .await?
        .into_iter()
        .map(|post| (MirrorKey::Post(post.id), Columns::Post(Box::new(post))))
        .collect();

    let source_handles: BTreeMap<_, _> = crate::social_handles::all(surreal)
        .await
        .map_err(|_| MirrorError::Source)?
        .into_iter()
        .map(handle_entry)
        .collect();
    let target_handles: BTreeMap<_, _> =
        sqlx::query("SELECT brand_key, provider, handle FROM social_handle")
            .fetch_all(&pg.0)
            .await?
            .into_iter()
            .map(|row| {
                Ok(handle_entry(SocialHandle {
                    brand_key: row.try_get("brand_key")?,
                    provider: row.try_get("provider")?,
                    handle: row.try_get("handle")?,
                }))
            })
            .collect::<Result<_, sqlx::Error>>()?;

    let source_publications: BTreeMap<_, _> = crate::social_publications::all(surreal)
        .await
        .map_err(|_| MirrorError::Source)?
        .into_iter()
        .map(|mut publication| {
            publication.inserted_at = micros(publication.inserted_at);
            publication.updated_at = micros(publication.updated_at);
            publication_entry(publication)
        })
        .collect();
    let target_publications: BTreeMap<_, _> = target_publications(pg)
        .await?
        .into_iter()
        .map(publication_entry)
        .collect();

    Ok(ParityReport {
        tables: vec![
            compare("post", &source_posts, &target_posts),
            compare("social_handle", &source_handles, &target_handles),
            compare(
                "social_publication",
                &source_publications,
                &target_publications,
            ),
        ],
    })
}

/// Re-mirror every divergent row from SurrealDB, then compare again.
///
/// A row that still fails stays in the returned report.
///
/// # Errors
///
/// Either store could not be read for the comparison.
pub async fn reconcile(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let before = parity(surreal, pg).await?;
    for key in before.divergent() {
        if let Err(error) = mirror(surreal, pg, key).await {
            tracing::warn!(table = key.table(), error = %error, "PostgreSQL reconcile could not mirror the row");
        }
    }
    parity(surreal, pg).await
}

fn handle_entry(handle: SocialHandle) -> (MirrorKey, Columns) {
    (
        MirrorKey::SocialHandle {
            brand_key: handle.brand_key,
            provider: handle.provider,
        },
        Columns::Handle(handle.handle),
    )
}

fn publication_entry(publication: SocialPublication) -> (MirrorKey, Columns) {
    (
        MirrorKey::SocialPublication {
            post_id: publication.post_id,
            provider: publication.provider.clone(),
        },
        Columns::Publication(Box::new(publication)),
    )
}

async fn target_posts(pg: &PostgresDb) -> Result<Vec<Post>, MirrorError> {
    let rows = sqlx::query(
        "SELECT id, brand_key, slug, title, description, body_html, body_markdown, status, \
         publish_at, video_asset, tiktok_ai_generated, vertical_video, video_orientation, \
         source_digest, social_links, facebook_caption, instagram_caption, threads_caption, \
         youtube_caption, selected_social_accounts, inserted_at, updated_at FROM post",
    )
    .fetch_all(&pg.0)
    .await?;
    rows.into_iter()
        .map(|row| {
            let links: sqlx::types::Json<Vec<SocialLink>> = row.try_get("social_links")?;
            Ok(Post {
                id: row.try_get("id")?,
                brand_key: row.try_get("brand_key")?,
                slug: row.try_get("slug")?,
                title: row.try_get("title")?,
                description: row.try_get("description")?,
                body_html: row.try_get("body_html")?,
                body_markdown: row.try_get("body_markdown")?,
                status: row.try_get("status")?,
                publish_at: row.try_get("publish_at")?,
                inserted_at: row.try_get("inserted_at")?,
                updated_at: row.try_get("updated_at")?,
                video_asset: row.try_get("video_asset")?,
                vertical_video: row.try_get("vertical_video")?,
                tiktok_ai_generated: row.try_get("tiktok_ai_generated")?,
                video_orientation: row.try_get("video_orientation")?,
                facebook_caption: row.try_get("facebook_caption")?,
                instagram_caption: row.try_get("instagram_caption")?,
                threads_caption: row.try_get("threads_caption")?,
                youtube_caption: row.try_get("youtube_caption")?,
                selected_social_accounts: row.try_get("selected_social_accounts")?,
                source_digest: row.try_get("source_digest")?,
                social_links: links.0,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(Into::into)
}

async fn target_publications(pg: &PostgresDb) -> Result<Vec<SocialPublication>, MirrorError> {
    let rows = sqlx::query(
        "SELECT post_id, provider, state, attempt_count, provider_task_id, provider_post_id, \
         permalink, last_error, inserted_at, updated_at FROM social_publication",
    )
    .fetch_all(&pg.0)
    .await?;
    rows.into_iter()
        .map(|row| {
            let post_id: Uuid = row.try_get("post_id")?;
            let provider: String = row.try_get("provider")?;
            let attempts: i32 = row.try_get("attempt_count")?;
            Ok(SocialPublication {
                id: crate::social_publications::publication_key(post_id, &provider),
                post_id,
                provider,
                state: row.try_get("state")?,
                attempt_count: u32::try_from(attempts).unwrap_or_default(),
                provider_task_id: row.try_get("provider_task_id")?,
                provider_post_id: row.try_get("provider_post_id")?,
                permalink: row.try_get("permalink")?,
                last_error: row.try_get("last_error")?,
                inserted_at: row.try_get("inserted_at")?,
                updated_at: row.try_get("updated_at")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(Into::into)
}

/// A boxed comparison of one mirror's tables.
type Comparison<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<ParityReport, MirrorError>> + 'a>>;

/// One mirror's `parity` or `reconcile`.
type Run = for<'a> fn(&'a SurrealDb, &'a PostgresDb) -> Comparison<'a>;

/// One mirrored group of tables, compared and reconciled together.
pub struct Mirror {
    /// The group's name in operator output.
    pub name: &'static str,
    parity: Run,
    reconcile: Run,
}

/// Every mirror, parents first: a Firm names its Entity and a membership its
/// Person, an asset and a Notation their Project, and a Post needs its Brand. A new mirror is one entry here, after
/// the mirrors whose rows it references.
pub const MIRRORS: &[Mirror] = &[
    Mirror {
        name: "identity",
        parity: |surreal, pg| Box::pin(identity::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(identity::reconcile(surreal, pg)),
    },
    Mirror {
        name: "tenancy",
        parity: |surreal, pg| Box::pin(tenancy::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(tenancy::reconcile(surreal, pg)),
    },
    Mirror {
        name: "social_accounts",
        parity: |surreal, pg| Box::pin(social_accounts::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(social_accounts::reconcile(surreal, pg)),
    },
    Mirror {
        name: "social_account_selections",
        parity: |surreal, pg| Box::pin(social_account_selections::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(social_account_selections::reconcile(surreal, pg)),
    },
    Mirror {
        name: "firm_integration_secrets",
        parity: |surreal, pg| Box::pin(firm_secrets::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(firm_secrets::reconcile(surreal, pg)),
    },
    Mirror {
        name: "documents",
        parity: |surreal, pg| Box::pin(documents::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(documents::reconcile(surreal, pg)),
    },
    Mirror {
        name: "post",
        parity: |surreal, pg| Box::pin(parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(reconcile(surreal, pg)),
    },
    Mirror {
        name: "blog_videos",
        parity: |surreal, pg| Box::pin(blog_videos::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(blog_videos::reconcile(surreal, pg)),
    },
    Mirror {
        name: "identity_detail",
        parity: |surreal, pg| Box::pin(identity_detail::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(identity_detail::reconcile(surreal, pg)),
    },
    Mirror {
        name: "mailrooms_letters",
        parity: |surreal, pg| Box::pin(mailrooms_letters::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(mailrooms_letters::reconcile(surreal, pg)),
    },
    Mirror {
        name: "leads",
        parity: |surreal, pg| Box::pin(leads::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(leads::reconcile(surreal, pg)),
    },
    Mirror {
        name: "catalog",
        parity: |surreal, pg| Box::pin(catalog::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(catalog::reconcile(surreal, pg)),
    },
    Mirror {
        name: "workflow",
        parity: |surreal, pg| Box::pin(workflow::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(workflow::reconcile(surreal, pg)),
    },
    Mirror {
        name: "notation_clauses",
        parity: |surreal, pg| Box::pin(notation_clauses::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(notation_clauses::reconcile(surreal, pg)),
    },
    Mirror {
        name: "attestations",
        parity: |surreal, pg| Box::pin(attestations::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(attestations::reconcile(surreal, pg)),
    },
    Mirror {
        name: "templates",
        parity: |surreal, pg| Box::pin(templates::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(templates::reconcile(surreal, pg)),
    },
    Mirror {
        name: "communications",
        parity: |surreal, pg| Box::pin(communications::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(communications::reconcile(surreal, pg)),
    },
    Mirror {
        name: "deadlines",
        parity: |surreal, pg| Box::pin(deadlines::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(deadlines::reconcile(surreal, pg)),
    },
    Mirror {
        name: "sent_email",
        parity: |surreal, pg| Box::pin(sent_emails::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(sent_emails::reconcile(surreal, pg)),
    },
    Mirror {
        name: "project_services",
        parity: |surreal, pg| Box::pin(project_services::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(project_services::reconcile(surreal, pg)),
    },
    Mirror {
        name: "project_references",
        parity: |surreal, pg| Box::pin(project_references::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(project_references::reconcile(surreal, pg)),
    },
    Mirror {
        name: "communications_conversations",
        parity: |surreal, pg| Box::pin(conversations::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(conversations::reconcile(surreal, pg)),
    },
    Mirror {
        name: "inbox",
        parity: |surreal, pg| Box::pin(inbox::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(inbox::reconcile(surreal, pg)),
    },
    Mirror {
        name: "email_deliveries",
        parity: |surreal, pg| Box::pin(email_deliveries::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(email_deliveries::reconcile(surreal, pg)),
    },
    Mirror {
        name: "matter_pages_captions",
        parity: |surreal, pg| Box::pin(matter_pages_captions::parity(surreal, pg)),
        reconcile: |surreal, pg| Box::pin(matter_pages_captions::reconcile(surreal, pg)),
    },
    Mirror {
        name: "project_records",
        parity: |surreal, pg| Box::pin(records::parity(surreal, pg, "project_records")),
        reconcile: |surreal, pg| Box::pin(records::reconcile(surreal, pg, "project_records")),
    },
    Mirror {
        name: "notation_inputs",
        parity: |surreal, pg| Box::pin(records::parity(surreal, pg, "notation_inputs")),
        reconcile: |surreal, pg| Box::pin(records::reconcile(surreal, pg, "notation_inputs")),
    },
    Mirror {
        name: "git_access",
        parity: |surreal, pg| Box::pin(records::parity(surreal, pg, "git_access")),
        reconcile: |surreal, pg| Box::pin(records::reconcile(surreal, pg, "git_access")),
    },
    Mirror {
        name: "source_domain_records",
        parity: |surreal, pg| Box::pin(records::parity(surreal, pg, "source_domain_records")),
        reconcile: |surreal, pg| Box::pin(records::reconcile(surreal, pg, "source_domain_records")),
    },
];

impl Mirror {
    /// Compare this mirror's tables in both stores. Writes nothing.
    ///
    /// # Errors
    ///
    /// Either store could not be read.
    pub async fn parity(
        &self,
        surreal: &SurrealDb,
        pg: &PostgresDb,
    ) -> Result<ParityReport, MirrorError> {
        (self.parity)(surreal, pg).await
    }
}

/// One mirror's comparison, named for operator output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorParity {
    pub mirror: &'static str,
    pub report: ParityReport,
}

/// Compare every mirror in [`MIRRORS`] order. Writes nothing.
///
/// # Errors
///
/// Either store could not be read.
pub async fn parity_all(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<Vec<MirrorParity>, MirrorError> {
    let mut reports = Vec::with_capacity(MIRRORS.len());
    for mirror in MIRRORS {
        reports.push(MirrorParity {
            mirror: mirror.name,
            report: (mirror.parity)(surreal, pg).await?,
        });
    }
    Ok(reports)
}

/// Reconcile every mirror, parents first, then compare again.
///
/// A stale PostgreSQL parent cannot be deleted while a stale child from a
/// later mirror still references it, so a run that leaves drift retries once
/// in reverse order, children first. Each pass mirrors only divergent keys,
/// so a run over clean stores writes nothing.
///
/// # Errors
///
/// Either store could not be read for a comparison.
pub async fn reconcile_all(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<Vec<MirrorParity>, MirrorError> {
    for mirror in MIRRORS {
        (mirror.reconcile)(surreal, pg).await?;
    }
    let after = parity_all(surreal, pg).await?;
    if after.iter().all(|mirror| mirror.report.is_clean()) {
        return Ok(after);
    }
    for mirror in MIRRORS.iter().rev() {
        (mirror.reconcile)(surreal, pg).await?;
    }
    parity_all(surreal, pg).await
}

/// How one table's divergent rows split between the stores.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Drift {
    /// In SurrealDB, absent from PostgreSQL.
    pub missing: usize,
    /// In PostgreSQL, absent from SurrealDB.
    pub extra: usize,
    /// In both, with a column that differs.
    pub differing: usize,
}

/// Split one table's divergent keys into missing, extra, and differing rows.
///
/// A divergent key PostgreSQL lacks is missing. The rest are extra or
/// differing, told apart by the row counts: PostgreSQL holds the shared rows
/// plus the extra ones.
///
/// # Errors
///
/// PostgreSQL could not be read.
pub async fn drift(pg: &PostgresDb, table: &TableParity) -> Result<Drift, MirrorError> {
    let missing = missing(pg, table).await?.len();
    let shared = table.source_rows.saturating_sub(missing);
    let extra = table.target_rows.saturating_sub(shared);
    let held = table.divergent.len() - missing;
    Ok(Drift {
        missing,
        extra,
        differing: held.saturating_sub(extra),
    })
}

/// One table's divergent keys PostgreSQL holds no row for, in report
/// order: the rows SurrealDB has that the mirror never wrote.
///
/// # Errors
///
/// PostgreSQL could not be read.
pub async fn missing(pg: &PostgresDb, table: &TableParity) -> Result<Vec<MirrorKey>, MirrorError> {
    let mut missing = Vec::new();
    for key in &table.divergent {
        if !held_by_target(pg, key).await? {
            missing.push(key.clone());
        }
    }
    Ok(missing)
}

/// Whether PostgreSQL holds a row under this key.
#[allow(clippy::too_many_lines)] // Key variants have distinct closed SQL identities that stay explicit here.
async fn held_by_target(pg: &PostgresDb, key: &MirrorKey) -> Result<bool, MirrorError> {
    let exists = sqlx::query_scalar::<_, bool>;
    if let MirrorKey::Record {
        table: "firm_anchor",
        id,
    } = key
    {
        return Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM firm_anchor WHERE id_kind = 'uuid' AND id = $1)",
        )
        .bind(id.to_string())
        .fetch_one(&pg.0)
        .await?);
    }
    if let Some(id) = row_id(key) {
        // `MirrorKey::table` is a closed set of static names, never input.
        let sql = format!(
            "SELECT EXISTS (SELECT 1 FROM \"{}\" WHERE id = $1)",
            key.table()
        );
        return Ok(sqlx::query_scalar::<_, bool>(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_one(&pg.0)
            .await?);
    }
    if let MirrorKey::RecordString { table, id } = key {
        let statement = if *table == "firm_anchor" {
            "SELECT EXISTS (SELECT 1 FROM firm_anchor WHERE id_kind = 'string' AND id = $1)"
                .to_owned()
        } else {
            format!("SELECT EXISTS (SELECT 1 FROM \"{table}\" WHERE id = $1)")
        };
        return Ok(
            sqlx::query_scalar::<_, bool>(sqlx::AssertSqlSafe(statement))
                .bind(id)
                .fetch_one(&pg.0)
                .await?,
        );
    }
    let query = match key {
        MirrorKey::SocialHandle {
            brand_key,
            provider,
        } => exists(
            "SELECT EXISTS (SELECT 1 FROM social_handle WHERE brand_key = $1 AND provider = $2)",
        )
        .bind(brand_key)
        .bind(provider),
        MirrorKey::SocialPublication { post_id, provider } => exists(
            "SELECT EXISTS (SELECT 1 FROM social_publication WHERE post_id = $1 AND provider = $2)",
        )
        .bind(*post_id)
        .bind(provider),
        MirrorKey::SocialAccount(key) => {
            exists("SELECT EXISTS (SELECT 1 FROM social_account WHERE id = $1)").bind(key)
        }
        MirrorKey::SocialAccountSelection { brand_key, provider } => exists(
            "SELECT EXISTS (SELECT 1 FROM social_account_selection WHERE brand_key = $1 AND provider = $2)",
        )
        .bind(brand_key)
        .bind(provider),
        MirrorKey::FirmIntegrationSecrets(firm_id) => {
            exists("SELECT EXISTS (SELECT 1 FROM firm_integration_secret WHERE firm_id = $1)")
                .bind(*firm_id)
        }
        MirrorKey::FirmBrand { brand_key } => {
            exists("SELECT EXISTS (SELECT 1 FROM firm_brand WHERE brand_key = $1)").bind(brand_key)
        }
        MirrorKey::FirmMembership { person_id, firm_id } => exists(
            "SELECT EXISTS (SELECT 1 FROM person_firm_role WHERE person_id = $1 AND firm_id = $2)",
        )
        .bind(*person_id)
        .bind(*firm_id),
        MirrorKey::ProjectReference {
            project_id,
            party_kind,
            party_id,
            reason,
        } => {
            let statement = match party_kind.as_str() {
                "person" => "SELECT EXISTS (SELECT 1 FROM project_reference WHERE project_id = $1 AND person_id = $2 AND reason = $3)",
                "entity" => "SELECT EXISTS (SELECT 1 FROM project_reference WHERE project_id = $1 AND entity_id = $2 AND reason = $3)",
                _ => return Err(MirrorError::Source),
            };
            exists(statement)
                .bind(*project_id)
                .bind(*party_id)
                .bind(reason)
        }
        MirrorKey::EntityRole {
            person_id,
            entity_id,
            role,
        } => exists(
            "SELECT EXISTS (SELECT 1 FROM entity_role \
             WHERE person_id = $1 AND entity_id = $2 AND role = $3)",
        )
        .bind(*person_id)
        .bind(*entity_id)
        .bind(role),
        MirrorKey::PersonMailbox(person_id) => {
            exists("SELECT EXISTS (SELECT 1 FROM person_mailbox WHERE person_id = $1)")
                .bind(*person_id)
        }
        MirrorKey::MatterPages(project_id) => {
            exists("SELECT EXISTS (SELECT 1 FROM matter_page WHERE project_id = $1)")
                .bind(*project_id)
        }
        MirrorKey::MatterCaption(project_id) => {
            exists("SELECT EXISTS (SELECT 1 FROM matter_caption WHERE project_id = $1)")
                .bind(*project_id)
        }
        _ => unreachable!("non-row-key mirror variants must be handled before this branch"),
    };
    Ok(query.fetch_one(&pg.0).await?)
}

/// The row's own UUID when the key is one, so its table's `id` column
/// finds it; `None` for keys made of other columns.
fn row_id(key: &MirrorKey) -> Option<Uuid> {
    match key {
        MirrorKey::SentEmail(id)
        | MirrorKey::EmailReceipt(id)
        | MirrorKey::EmailDelivery(id)
        | MirrorKey::EmailDeliveryAttempt(id)
        | MirrorKey::Post(id)
        | MirrorKey::BlogVideo(id)
        | MirrorKey::Firm(id)
        | MirrorKey::Brand(id)
        | MirrorKey::Project(id)
        | MirrorKey::ProjectParticipation(id)
        | MirrorKey::Jurisdiction(id)
        | MirrorKey::EntityType(id)
        | MirrorKey::Entity(id)
        | MirrorKey::Person(id)
        | MirrorKey::Authority(id)
        | MirrorKey::GlossaryTerm(id)
        | MirrorKey::Question(id)
        | MirrorKey::NotationPackage(id)
        | MirrorKey::NotationPackageTemplate(id)
        | MirrorKey::NotationRate(id)
        | MirrorKey::Asset(id)
        | MirrorKey::Template(id)
        | MirrorKey::Notation(id)
        | MirrorKey::NotationEvent(id)
        | MirrorKey::Answer(id)
        | MirrorKey::NotationClause(id)
        | MirrorKey::Attestation(id)
        | MirrorKey::Address(id)
        | MirrorKey::Credential(id)
        | MirrorKey::EmailToken(id)
        | MirrorKey::SummarySenderApproval(id)
        | MirrorKey::SummarySenderApprovalEvent(id)
        | MirrorKey::ProjectDeadline(id)
        | MirrorKey::ProjectService(id)
        | MirrorKey::Communication(id)
        | MirrorKey::EmailConversation(id)
        | MirrorKey::EmailConversationMessage(id)
        | MirrorKey::InboxConversation(id)
        | MirrorKey::InboxMessage(id)
        | MirrorKey::InboxIngress(id)
        | MirrorKey::InboxAssignment(id)
        | MirrorKey::InboxDeliveryAttempt(id)
        | MirrorKey::InboxTriage(id)
        | MirrorKey::Mailroom(id)
        | MirrorKey::Letter(id)
        | MirrorKey::Lead(id)
        | MirrorKey::LeadConsent(id)
        | MirrorKey::PersonDelegate(id)
        | MirrorKey::ExternalIdentity(id)
        | MirrorKey::Record { id, .. } => Some(*id),
        MirrorKey::RecordString { .. }
        | MirrorKey::SocialHandle { .. }
        | MirrorKey::SocialPublication { .. }
        | MirrorKey::SocialAccount(_)
        | MirrorKey::SocialAccountSelection { .. }
        | MirrorKey::FirmIntegrationSecrets(_)
        | MirrorKey::FirmBrand { .. }
        | MirrorKey::FirmMembership { .. }
        | MirrorKey::EntityRole { .. }
        | MirrorKey::PersonMailbox(_)
        | MirrorKey::MatterPages(_)
        | MirrorKey::MatterCaption(_)
        | MirrorKey::ProjectReference { .. } => None,
    }
}

#[cfg(test)]
mod blog_videos_tests;
#[cfg(test)]
pub(crate) mod tests;
