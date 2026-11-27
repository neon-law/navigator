//! A Firm user's SSH public keys, stored relationally in PostgreSQL and
//! reconciled onto their account on the installation's Forgejo (ENG-1219).
//!
//! A Person registers a key once in Navigator; [`sync_person`] makes it a key
//! on their Forgejo account through the dedicated key-sync service identity
//! (`cloud::forgejo_user_keys`). Navigator's list is authoritative only while
//! the Person holds a Firm membership: a Person with none — a Client, or a
//! Lawyer removed from their Firm — wants no keys on the forge, so the next
//! reconciliation removes every key Navigator put there.
//!
//! Only the public half is accepted. [`parse`] admits the algorithm
//! allowlist (ed25519 and the three NIST ECDSA curves), checks that the
//! base64 body decodes to a well-formed key of the declared type, refuses
//! anything that looks like a private key, and stores the canonical
//! `<algorithm> <base64>` line with the OpenSSH `SHA256:` fingerprint. A
//! fingerprint is live on at most one row in the installation, so the same
//! key is refused for a second Person.
//!
//! Removing a key is a revocation first: `revoked_at` drops it from the
//! desired set at once, and [`mark_synced`] deletes the row only after a
//! reconciliation has taken the key off Forgejo. A Person manages only their
//! own keys; a Firm administrator who may manage one of the key owner's
//! Firm memberships may also revoke them ([`may_revoke`]). See
//! `docs/glossary/ssh-public-key.md`.

use base64::Engine as _;
use chrono::{DateTime, Utc};
use cloud::forgejo_user_keys::{
    fingerprint_of_blob, DesiredKey, ForgejoUserKeys, KeyReconciliation, KeySyncError,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::firm_capability::FirmCapability;
use crate::persons::Role;
use crate::postgres::PostgresDb;

/// The longest label a key carries, in characters.
pub const LABEL_MAX_CHARS: usize = 100;
/// The longest key line accepted, comment included.
const INPUT_MAX_BYTES: usize = 4096;

/// The admitted key algorithms. RSA, DSA, and security-key (`sk-`) types are
/// refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Algorithm {
    #[serde(rename = "ssh-ed25519")]
    Ed25519,
    #[serde(rename = "ecdsa-sha2-nistp256")]
    EcdsaP256,
    #[serde(rename = "ecdsa-sha2-nistp384")]
    EcdsaP384,
    #[serde(rename = "ecdsa-sha2-nistp521")]
    EcdsaP521,
}

impl Algorithm {
    /// The OpenSSH key-type name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ed25519 => "ssh-ed25519",
            Self::EcdsaP256 => "ecdsa-sha2-nistp256",
            Self::EcdsaP384 => "ecdsa-sha2-nistp384",
            Self::EcdsaP521 => "ecdsa-sha2-nistp521",
        }
    }

    /// Parse an OpenSSH key-type name on the allowlist.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ssh-ed25519" => Some(Self::Ed25519),
            "ecdsa-sha2-nistp256" => Some(Self::EcdsaP256),
            "ecdsa-sha2-nistp384" => Some(Self::EcdsaP384),
            "ecdsa-sha2-nistp521" => Some(Self::EcdsaP521),
            _ => None,
        }
    }

    /// The ECDSA curve name and uncompressed point length.
    const fn curve(self) -> Option<(&'static str, usize)> {
        match self {
            Self::Ed25519 => None,
            Self::EcdsaP256 => Some(("nistp256", 65)),
            Self::EcdsaP384 => Some(("nistp384", 97)),
            Self::EcdsaP521 => Some(("nistp521", 133)),
        }
    }
}

/// Why a key was refused or a key operation failed.
#[derive(Debug, thiserror::Error)]
pub enum SshKeyError {
    #[error("that is a private key; paste only the public key (the .pub file)")]
    PrivateKey,
    #[error("only ed25519 and ECDSA (nistp256, nistp384, nistp521) public keys are accepted")]
    UnsupportedAlgorithm,
    #[error("the SSH public key is malformed: {0}")]
    Invalid(&'static str),
    #[error("the label must be 1 to 100 printable characters")]
    InvalidLabel,
    #[error("this SSH public key is already registered")]
    Duplicate,
    #[error("only a Firm member can register an SSH public key")]
    NotFirmMember,
    #[error("the Person was not found")]
    PersonNotFound,
    #[error("the SSH public key was not found")]
    NotFound,
    #[error("Forgejo key sync failed")]
    Sync(#[from] KeySyncError),
    #[error("database operation failed")]
    Database(#[source] sqlx::Error),
    #[error("PostgreSQL is unavailable")]
    Postgres(#[from] crate::postgres::PostgresError),
    #[error("reading Firm memberships failed")]
    Firm(#[from] crate::firms::FirmError),
    #[error("reading the Person failed")]
    Person(#[from] crate::persons::PersonError),
}

impl From<sqlx::Error> for SshKeyError {
    fn from(error: sqlx::Error) -> Self {
        match error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref()
        {
            Some("23505") => Self::Duplicate,
            Some("23503") => Self::PersonNotFound,
            _ => Self::Database(error),
        }
    }
}

/// A validated public key, ready to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedKey {
    pub algorithm: Algorithm,
    /// The canonical `<algorithm> <base64>` line, comment dropped.
    pub public_key: String,
    /// The OpenSSH `SHA256:` fingerprint.
    pub fingerprint: String,
    /// The trailing comment, if any — offered as the default label.
    pub comment: Option<String>,
}

/// Read one length-prefixed SSH wire string.
fn wire_string<'a>(blob: &mut &'a [u8]) -> Option<&'a [u8]> {
    let (length, rest) = blob.split_first_chunk::<4>()?;
    let length = usize::try_from(u32::from_be_bytes(*length)).ok()?;
    if rest.len() < length {
        return None;
    }
    let (value, rest) = rest.split_at(length);
    *blob = rest;
    Some(value)
}

/// Check the decoded body is exactly a well-formed key of `algorithm`.
fn check_blob(algorithm: Algorithm, blob: &[u8]) -> Result<(), SshKeyError> {
    let mut rest = blob;
    let malformed = SshKeyError::Invalid("the key body does not match its type");
    if wire_string(&mut rest) != Some(algorithm.as_str().as_bytes()) {
        return Err(malformed);
    }
    match algorithm.curve() {
        None => {
            if wire_string(&mut rest).map(<[u8]>::len) != Some(32) {
                return Err(malformed);
            }
        }
        Some((curve, point_len)) => {
            if wire_string(&mut rest) != Some(curve.as_bytes()) {
                return Err(malformed);
            }
            let point = wire_string(&mut rest)
                .ok_or(SshKeyError::Invalid("the key body does not match its type"))?;
            if point.len() != point_len || point.first() != Some(&0x04) {
                return Err(malformed);
            }
        }
    }
    if rest.is_empty() {
        Ok(())
    } else {
        Err(malformed)
    }
}

/// Validate one OpenSSH public-key line: `<algorithm> <base64> [comment]`.
///
/// # Errors
/// [`SshKeyError::PrivateKey`] for anything that looks like private key
/// material, [`SshKeyError::UnsupportedAlgorithm`] off the allowlist, and
/// [`SshKeyError::Invalid`] for a malformed line or body.
pub fn parse(input: &str) -> Result<ParsedKey, SshKeyError> {
    let line = input.trim();
    if line.contains("PRIVATE KEY") || line.starts_with("PuTTY-User-Key-File") {
        return Err(SshKeyError::PrivateKey);
    }
    if line.is_empty() {
        return Err(SshKeyError::Invalid("the key is empty"));
    }
    if line.len() > INPUT_MAX_BYTES {
        return Err(SshKeyError::Invalid("the key is too long"));
    }
    if line.contains(['\n', '\r']) {
        return Err(SshKeyError::Invalid("paste exactly one key, on one line"));
    }
    let mut parts = line.splitn(3, [' ', '\t']);
    let kind = parts.next().unwrap_or_default();
    let algorithm = Algorithm::parse(kind).ok_or(SshKeyError::UnsupportedAlgorithm)?;
    let body = parts
        .next()
        .map(str::trim)
        .filter(|body| !body.is_empty())
        .ok_or(SshKeyError::Invalid("the key has no body"))?;
    let blob = base64::engine::general_purpose::STANDARD
        .decode(body)
        .map_err(|_| SshKeyError::Invalid("the key body is not base64"))?;
    check_blob(algorithm, &blob)?;
    let comment = parts
        .next()
        .map(str::trim)
        .filter(|comment| !comment.is_empty())
        .map(str::to_string);
    Ok(ParsedKey {
        algorithm,
        public_key: format!(
            "{} {}",
            algorithm.as_str(),
            base64::engine::general_purpose::STANDARD.encode(&blob)
        ),
        fingerprint: fingerprint_of_blob(&blob),
        comment,
    })
}

/// The label to store: the caller's, else the key's comment, else the
/// algorithm. Control characters and over-long labels are refused.
///
/// # Errors
/// [`SshKeyError::InvalidLabel`].
pub fn label_for(label: Option<&str>, parsed: &ParsedKey) -> Result<String, SshKeyError> {
    let chosen = label
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .or(parsed.comment.as_deref())
        .unwrap_or(parsed.algorithm.as_str())
        .trim();
    if chosen.is_empty()
        || chosen.chars().count() > LABEL_MAX_CHARS
        || chosen.chars().any(char::is_control)
    {
        return Err(SshKeyError::InvalidLabel);
    }
    Ok(chosen.to_string())
}

/// One live registered key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshPublicKey {
    pub id: Uuid,
    pub person_id: Uuid,
    pub algorithm: String,
    pub public_key: String,
    pub fingerprint: String,
    pub label: String,
    pub created_at: DateTime<Utc>,
    pub last_synced_at: Option<DateTime<Utc>>,
}

const COLUMNS: &str =
    "id,person_id,algorithm,public_key,fingerprint,label,created_at,last_synced_at";

fn from_row(row: &sqlx::postgres::PgRow) -> SshPublicKey {
    SshPublicKey {
        id: row.get("id"),
        person_id: row.get("person_id"),
        algorithm: row.get("algorithm"),
        public_key: row.get("public_key"),
        fingerprint: row.get("fingerprint"),
        label: row.get("label"),
        created_at: row.get("created_at"),
        last_synced_at: row.get("last_synced_at"),
    }
}

/// Register a key for `person_id`. The caller has already checked the
/// Person is a Firm member ([`require_firm_member`]).
///
/// # Errors
/// Validation errors from [`parse`] and [`label_for`];
/// [`SshKeyError::Duplicate`] when the fingerprint is live anywhere;
/// [`SshKeyError::PersonNotFound`] when PostgreSQL holds no such Person.
pub async fn add(
    pg: &PostgresDb,
    person_id: Uuid,
    input: &str,
    label: Option<&str>,
) -> Result<SshPublicKey, SshKeyError> {
    let parsed = parse(input)?;
    let label = label_for(label, &parsed)?;
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO ssh_public_key (person_id,algorithm,public_key,fingerprint,label) \
         VALUES ($1,$2,$3,$4,$5) RETURNING {COLUMNS}"
    )))
    .bind(person_id)
    .bind(parsed.algorithm.as_str())
    .bind(&parsed.public_key)
    .bind(&parsed.fingerprint)
    .bind(&label)
    .fetch_one(&pg.0)
    .await?;
    Ok(from_row(&row))
}

/// A Person's live keys, oldest first.
///
/// # Errors
/// Database errors.
pub async fn list_for_person(
    pg: &PostgresDb,
    person_id: Uuid,
) -> Result<Vec<SshPublicKey>, SshKeyError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM ssh_public_key \
         WHERE person_id=$1 AND revoked_at IS NULL ORDER BY created_at, id"
    )))
    .bind(person_id)
    .fetch_all(&pg.0)
    .await?;
    Ok(rows.iter().map(from_row).collect())
}

/// One live key by id.
///
/// # Errors
/// Database errors.
pub async fn find(pg: &PostgresDb, id: Uuid) -> Result<Option<SshPublicKey>, SshKeyError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM ssh_public_key WHERE id=$1 AND revoked_at IS NULL"
    )))
    .bind(id)
    .fetch_optional(&pg.0)
    .await?;
    Ok(row.as_ref().map(from_row))
}

/// Revoke a live key, recording who revoked it. The row stays until a
/// reconciliation has removed the key from Forgejo ([`mark_synced`]).
///
/// # Errors
/// [`SshKeyError::NotFound`] when the key is not live.
pub async fn revoke(
    pg: &PostgresDb,
    id: Uuid,
    actor_person_id: Option<Uuid>,
) -> Result<SshPublicKey, SshKeyError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE ssh_public_key SET revoked_at=now(), revoked_by_person_id=$2 \
         WHERE id=$1 AND revoked_at IS NULL RETURNING {COLUMNS}"
    )))
    .bind(id)
    .bind(actor_person_id)
    .fetch_optional(&pg.0)
    .await?;
    row.as_ref().map(from_row).ok_or(SshKeyError::NotFound)
}

/// Record a completed reconciliation for one Person: stamp their live keys
/// as synced and delete the revoked rows the reconciliation has removed.
///
/// # Errors
/// Database errors.
pub async fn mark_synced(
    pg: &PostgresDb,
    person_id: Uuid,
    at: DateTime<Utc>,
) -> Result<(), SshKeyError> {
    let mut tx = pg.0.begin().await?;
    sqlx::query(
        "UPDATE ssh_public_key SET last_synced_at=$2 WHERE person_id=$1 AND revoked_at IS NULL",
    )
    .bind(person_id)
    .bind(at)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM ssh_public_key WHERE person_id=$1 AND revoked_at IS NOT NULL")
        .bind(person_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Every Person holding any key row, live or revoked-awaiting-sync — the
/// operator sweep's worklist.
///
/// # Errors
/// Database errors.
pub async fn person_ids_with_keys(pg: &PostgresDb) -> Result<Vec<Uuid>, SshKeyError> {
    let rows = sqlx::query("SELECT DISTINCT person_id FROM ssh_public_key ORDER BY person_id")
        .fetch_all(&pg.0)
        .await?;
    Ok(rows.iter().map(|row| row.get("person_id")).collect())
}

/// Whether the Person holds any Firm membership — the condition for having
/// keys on the forge at all.
///
/// # Errors
/// Membership read errors.
pub async fn is_firm_member(
    application_store: &ApplicationStore,
    person_id: Uuid,
) -> Result<bool, SshKeyError> {
    Ok(
        !crate::firms::memberships_for_person(application_store, person_id)
            .await?
            .is_empty(),
    )
}

/// Refuse a Person with no Firm membership.
///
/// # Errors
/// [`SshKeyError::NotFirmMember`].
pub async fn require_firm_member(
    application_store: &ApplicationStore,
    person_id: Uuid,
) -> Result<(), SshKeyError> {
    if is_firm_member(application_store, person_id).await? {
        Ok(())
    } else {
        Err(SshKeyError::NotFirmMember)
    }
}

/// Whether `actor` may revoke a key `owner_id` holds: the owner always; an
/// Owner anywhere; otherwise someone who may manage the membership of one of
/// the owner's Firms (that Firm's administrator).
///
/// # Errors
/// Membership read errors.
pub async fn may_revoke(
    application_store: &ApplicationStore,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    owner_id: Uuid,
) -> Result<bool, SshKeyError> {
    if actor_person_id == Some(owner_id) || matches!(actor_role, Role::Owner) {
        return Ok(true);
    }
    if !matches!(actor_role, Role::Admin) {
        return Ok(false);
    }
    for membership in crate::firms::memberships_for_person(application_store, owner_id).await? {
        let decision = crate::firm_capability::resolve(
            application_store,
            actor_role,
            actor_person_id,
            membership.firm_id,
            FirmCapability::ManageMembership,
        )
        .await?;
        if decision.is_allowed() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// What one Person's reconciliation came to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SyncOutcome {
    /// The deployment has not configured the Forgejo key sync.
    NotConfigured,
    /// No Forgejo account carries the Person's email yet; the keys reach the
    /// forge on the first reconciliation after they sign in there.
    NoForgejoAccount,
    /// The account was reconciled (or, in a dry run, planned).
    Synced(KeyReconciliation),
}

/// Reconcile one Person's keys onto their Forgejo account. Desired is the
/// Person's live keys while they hold a Firm membership, and nothing
/// otherwise. A completed (non-dry) run stamps `last_synced_at` and purges
/// revoked rows.
///
/// # Errors
/// Store and [`KeySyncError`]s. The keys in Navigator are unchanged by a
/// failure; the next run converges.
pub async fn sync_person(
    application_store: &ApplicationStore,
    pg: &PostgresDb,
    client: Option<&ForgejoUserKeys>,
    person_id: Uuid,
    dry_run: bool,
) -> Result<SyncOutcome, SshKeyError> {
    let Some(client) = client else {
        return Ok(SyncOutcome::NotConfigured);
    };
    let person = crate::persons::find_by_id(application_store, person_id)
        .await?
        .ok_or(SshKeyError::PersonNotFound)?;
    let desired = if is_firm_member(application_store, person_id).await? {
        list_for_person(pg, person_id)
            .await?
            .into_iter()
            .map(|key| DesiredKey {
                public_key: key.public_key,
                fingerprint: key.fingerprint,
                label: key.label,
            })
            .collect()
    } else {
        Vec::new()
    };
    let Some(report) = client.reconcile(&person.email, &desired, dry_run).await? else {
        return Ok(SyncOutcome::NoForgejoAccount);
    };
    if !dry_run {
        mark_synced(pg, person_id, Utc::now()).await?;
    }
    if !report.drift.is_empty() {
        tracing::warn!(
            person_id = %person_id,
            drift = report.drift.len(),
            "Forgejo account holds SSH keys Navigator does not manage"
        );
    }
    Ok(SyncOutcome::Synced(report))
}

/// [`sync_person`] with the key-sync client from the environment.
///
/// # Errors
/// As [`sync_person`], plus a misconfigured key-sync setting.
pub async fn sync_person_from_env(
    application_store: &ApplicationStore,
    pg: &PostgresDb,
    person_id: Uuid,
) -> Result<SyncOutcome, SshKeyError> {
    let client = ForgejoUserKeys::from_env()?;
    sync_person(application_store, pg, client.as_ref(), person_id, false).await
}

/// Best-effort reconciliation after a Person's Firm membership was removed,
/// so their keys leave the forge with their membership. Runs only when both
/// PostgreSQL and the key sync are configured; a failure is logged and left
/// for the operator sweep (`navigator ops forgejo ssh-keys sync`), never
/// surfaced as a failure of the membership change itself.
pub async fn after_membership_removed(application_store: &ApplicationStore, person_id: Uuid) {
    let client = match ForgejoUserKeys::from_env() {
        Ok(Some(client)) => client,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(%error, "Forgejo key sync is misconfigured");
            return;
        }
    };
    let Ok(pg) = PostgresDb::application().await else {
        return;
    };
    if let Err(error) = sync_person(application_store, &pg, Some(&client), person_id, false).await {
        tracing::warn!(
            %error,
            person_id = %person_id,
            "SSH key reconciliation after a membership removal failed; the sweep will retry"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic keys generated for these tests only; `ssh-keygen -lf`
    // printed the fingerprints below.
    const ED25519: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIO+kywI2x0e1JcY7eJ50pKflZgKO0UKaS+cV8ssit45e synthetic@example.test";
    const ED25519_FINGERPRINT: &str = "SHA256:ud4b4+SttkBnAA6wyLOfaG711rzSh+nvmxq0OUDKf3M";
    const ECDSA: &str = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBMBDBXbUHEgKCmZcCq487Y5Tq23FmDLS5vKE8xWlJcvgXyr2fDNyvpj/fa93vD9gCE+SxSN6MpdDEUlUuZlNWzw=";
    const ECDSA_FINGERPRINT: &str = "SHA256:PoKfY/7fGyoZjVY/mg7XdndMkTFswlLY4pKJWrkTnkk";

    #[test]
    fn ed25519_parses_to_the_canonical_line_and_openssh_fingerprint() {
        let parsed = parse(&format!("  {ED25519}\n")).unwrap();
        assert_eq!(parsed.algorithm, Algorithm::Ed25519);
        assert_eq!(parsed.fingerprint, ED25519_FINGERPRINT);
        assert_eq!(
            parsed.public_key,
            ED25519.rsplit_once(' ').unwrap().0,
            "the comment is not stored in the key line"
        );
        assert_eq!(parsed.comment.as_deref(), Some("synthetic@example.test"));
    }

    #[test]
    fn ecdsa_parses_without_a_comment() {
        let parsed = parse(ECDSA).unwrap();
        assert_eq!(parsed.algorithm, Algorithm::EcdsaP256);
        assert_eq!(parsed.fingerprint, ECDSA_FINGERPRINT);
        assert_eq!(parsed.comment, None);
        assert_eq!(label_for(None, &parsed).unwrap(), "ecdsa-sha2-nistp256");
    }

    #[test]
    fn private_keys_are_refused_outright() {
        for input in [
            "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----",
            "-----BEGIN EC PRIVATE KEY-----",
            "PuTTY-User-Key-File-3: ssh-ed25519",
        ] {
            assert!(matches!(parse(input), Err(SshKeyError::PrivateKey)), "{input}");
        }
    }

    #[test]
    fn algorithms_off_the_allowlist_are_refused() {
        for kind in [
            "ssh-rsa",
            "ssh-dss",
            "sk-ssh-ed25519@openssh.com",
            "ssh-ed25519-cert-v01@openssh.com",
            "from=\"10.0.0.1\"",
        ] {
            let input = format!(
                "{kind} AAAAC3NzaC1lZDI1NTE5AAAAIO+kywI2x0e1JcY7eJ50pKflZgKO0UKaS+cV8ssit45e"
            );
            assert!(
                matches!(parse(&input), Err(SshKeyError::UnsupportedAlgorithm)),
                "{kind}"
            );
        }
    }

    #[test]
    fn a_body_that_is_not_base64_or_not_its_declared_type_is_refused() {
        assert!(matches!(
            parse("ssh-ed25519 not*base64"),
            Err(SshKeyError::Invalid(_))
        ));
        assert!(matches!(parse("ssh-ed25519"), Err(SshKeyError::Invalid(_))));
        // An ECDSA body declared as ed25519.
        let ecdsa_body = ECDSA.split(' ').nth(1).unwrap();
        assert!(matches!(
            parse(&format!("ssh-ed25519 {ecdsa_body}")),
            Err(SshKeyError::Invalid(_))
        ));
        // A truncated ed25519 point.
        let mut blob = base64::engine::general_purpose::STANDARD
            .decode(ED25519.split(' ').nth(1).unwrap())
            .unwrap();
        blob.pop();
        let short = base64::engine::general_purpose::STANDARD.encode(&blob);
        assert!(matches!(
            parse(&format!("ssh-ed25519 {short}")),
            Err(SshKeyError::Invalid(_))
        ));
        // Trailing bytes after a well-formed key.
        let mut long = base64::engine::general_purpose::STANDARD
            .decode(ED25519.split(' ').nth(1).unwrap())
            .unwrap();
        long.push(0);
        let long = base64::engine::general_purpose::STANDARD.encode(&long);
        assert!(matches!(
            parse(&format!("ssh-ed25519 {long}")),
            Err(SshKeyError::Invalid(_))
        ));
    }

    #[test]
    fn two_keys_at_once_are_refused() {
        assert!(matches!(
            parse(&format!("{ED25519}\n{ECDSA}")),
            Err(SshKeyError::Invalid(_))
        ));
    }

    #[test]
    fn the_label_prefers_the_caller_then_the_comment() {
        let parsed = parse(ED25519).unwrap();
        assert_eq!(label_for(Some(" laptop "), &parsed).unwrap(), "laptop");
        assert_eq!(
            label_for(Some("   "), &parsed).unwrap(),
            "synthetic@example.test"
        );
        assert!(matches!(
            label_for(Some(&"x".repeat(101)), &parsed),
            Err(SshKeyError::InvalidLabel)
        ));
        assert!(matches!(
            label_for(Some("bad\u{7}label"), &parsed),
            Err(SshKeyError::InvalidLabel)
        ));
    }

    // The remaining assertions need a live PostgreSQL: fingerprint
    // uniqueness and the revoke-then-purge lifecycle are database rules.
    mod database {
        use super::super::*;
        use super::{ECDSA, ED25519, ED25519_FINGERPRINT};
        use crate::postgres::fixtures::{connect, fresh_schema};

        async fn person(pg: &PostgresDb) -> Uuid {
            let unique = Uuid::new_v4().simple().to_string();
            sqlx::query(
                "INSERT INTO person (id,name,email,role) VALUES ($1,'Synthetic Person',$2,'lawyer') RETURNING id",
            )
            .bind(Uuid::new_v4())
            .bind(format!("{unique}@example.com"))
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0)
        }

        async fn database() -> Option<PostgresDb> {
            let (options, database) = fresh_schema().await?;
            Some(connect(&options, &database).await)
        }

        #[tokio::test]
        async fn a_fingerprint_is_live_on_one_row_across_people() {
            let Some(pg) = database().await else { return };
            let first = person(&pg).await;
            let second = person(&pg).await;
            let key = add(&pg, first, ED25519, Some("laptop")).await.unwrap();
            assert_eq!(key.fingerprint, ED25519_FINGERPRINT);
            assert_eq!(key.label, "laptop");
            assert_eq!(key.last_synced_at, None);
            assert!(matches!(
                add(&pg, second, ED25519, None).await,
                Err(SshKeyError::Duplicate)
            ));
            assert!(matches!(
                add(&pg, first, ED25519, None).await,
                Err(SshKeyError::Duplicate)
            ));
            add(&pg, second, ECDSA, None).await.unwrap();
            assert_eq!(list_for_person(&pg, first).await.unwrap(), vec![key]);
        }

        #[tokio::test]
        async fn an_unknown_person_is_refused() {
            let Some(pg) = database().await else { return };
            assert!(matches!(
                add(&pg, Uuid::new_v4(), ED25519, None).await,
                Err(SshKeyError::PersonNotFound)
            ));
        }

        #[tokio::test]
        async fn revoke_hides_the_key_until_a_sync_purges_it() {
            let Some(pg) = database().await else { return };
            let owner = person(&pg).await;
            let key = add(&pg, owner, ED25519, None).await.unwrap();
            let revoked = revoke(&pg, key.id, Some(owner)).await.unwrap();
            assert_eq!(revoked.id, key.id);
            assert!(list_for_person(&pg, owner).await.unwrap().is_empty());
            assert!(find(&pg, key.id).await.unwrap().is_none());
            assert!(matches!(
                revoke(&pg, key.id, Some(owner)).await,
                Err(SshKeyError::NotFound)
            ));
            // Still on the sweep's worklist until a reconciliation finishes.
            assert_eq!(person_ids_with_keys(&pg).await.unwrap(), vec![owner]);
            // A revoked key does not block registering it again.
            let again = add(&pg, owner, ED25519, None).await.unwrap();
            let at = Utc::now();
            mark_synced(&pg, owner, at).await.unwrap();
            let live = list_for_person(&pg, owner).await.unwrap();
            assert_eq!(live.len(), 1);
            assert_eq!(live[0].id, again.id);
            assert!(live[0].last_synced_at.is_some());
            let rows: i64 = sqlx::query("SELECT count(*) FROM ssh_public_key")
                .fetch_one(pg.pool())
                .await
                .unwrap()
                .get(0);
            assert_eq!(rows, 1, "the revoked row is purged after the sync");
        }

        #[tokio::test]
        async fn the_database_refuses_an_algorithm_off_the_allowlist() {
            let Some(pg) = database().await else { return };
            let owner = person(&pg).await;
            let refused = sqlx::query(
                "INSERT INTO ssh_public_key (person_id,algorithm,public_key,fingerprint,label) \
                 VALUES ($1,'ssh-rsa','ssh-rsa AAAA','SHA256:x','rsa')",
            )
            .bind(owner)
            .execute(pg.pool())
            .await;
            assert!(refused.is_err());
        }
    }
}
