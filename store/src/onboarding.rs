//! PostgreSQL execution evidence and document filing for onboarding.
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::Arc,
};

use cloud::StorageService;
use uuid::Uuid;

use crate::{
    assets::{Asset, AssetError, ExpectedHead, Filed, RevisionError},
    documents::{DocumentIdentity, IngestArgs, Signing, SigningMethod},
    postgres::PostgresDb,
};

/// Projects whose onboarding letter is executed, from verified PostgreSQL evidence.
///
/// A completed DocuSign request on an Onboarding Notation or a server-verified
/// DocuSign upload counts. A manually exported Dropbox Sign retainer counts
/// when a lawyer records verification against its current document revision.
pub async fn official_projects(
    pg: &PostgresDb,
    projects: &[Uuid],
) -> Result<HashSet<Uuid>, sqlx::Error> {
    Ok(verified_providers(pg, projects)
        .await?
        .into_keys()
        .collect())
}

/// Signing methods backed by completed provider evidence or verified imports, keyed by Project.
pub async fn verified_providers(
    pg: &PostgresDb,
    projects: &[Uuid],
) -> Result<HashMap<Uuid, BTreeSet<SigningMethod>>, sqlx::Error> {
    crate::postgres::history::require_scope(pg, crate::postgres::history::Family::Notation)
        .await
        .map_err(|error| sqlx::Error::Configuration(Box::new(error)))?;
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT n.project_id, s.provider, s.provider_id FROM signature s
         JOIN notation n ON n.id=s.notation_id JOIN template t ON t.id=n.template_id
         WHERE n.project_id=ANY($1) AND t.kind='onboarding'
           AND s.provider='docusign' AND s.state='completed' AND s.signed_at IS NOT NULL
         UNION
         SELECT project_id, 'docusign', metadata->>'docusign_envelope_id' FROM asset
         WHERE project_id=ANY($1) AND kind='onboarding' AND deleted_at IS NULL
           AND metadata->>'verified_onboarding_envelope_id'=metadata->>'docusign_envelope_id'
         UNION
         SELECT v.project_id,v.provider,v.provider_request_id
         FROM onboarding_signature_evidence v
         JOIN asset a ON a.id=v.asset_id AND a.project_id=v.project_id
         JOIN project p ON p.id=v.project_id AND p.firm_id=v.firm_id
         WHERE v.project_id=ANY($1) AND v.provider='dropbox_sign'
           AND a.kind='onboarding' AND a.deleted_at IS NULL
           AND NOT EXISTS (SELECT 1 FROM asset newer
               WHERE newer.project_id=a.project_id AND newer.slug=a.slug AND newer.id>a.id)",
    )
    .bind(projects)
    .fetch_all(pg.pool())
    .await?;
    let mut methods = HashMap::<_, BTreeSet<_>>::new();
    for (project, provider, id) in rows {
        if let Some(method) =
            SigningMethod::parse(&provider).filter(|_| evidence_id_is_valid(&provider, &id))
        {
            methods.entry(project).or_default().insert(method);
        }
    }
    Ok(methods)
}

/// A lawyer's verification of an exported, completed signing request for a
/// previously filed Onboarding document.
#[derive(Clone, Copy)]
pub struct VerifiedDocument<'a> {
    pub asset_id: Uuid,
    pub project_id: Uuid,
    pub firm_id: Uuid,
    pub client_id: Uuid,
    pub client_email: &'a str,
    pub sha256: &'a str,
    pub request_id: &'a str,
    pub account_email: &'a str,
    pub signed_on: chrono::NaiveDate,
    pub actor: Uuid,
}

/// Write verification once for the current revision of an open Project. The
/// unique provider request prevents the same agreement activating two Projects.
pub async fn record_verified_document(
    pg: &PostgresDb,
    evidence: &VerifiedDocument<'_>,
    dry_run: bool,
) -> Result<bool, sqlx::Error> {
    if !valid_dropbox_sign_request_id(evidence.request_id) {
        return Ok(false);
    }
    crate::postgres::history::require_scope(pg, crate::postgres::history::Family::Notation)
        .await
        .map_err(|error| sqlx::Error::Configuration(Box::new(error)))?;
    let mut tx = pg.pool().begin().await?;
    let current = sqlx::query_scalar::<_, Uuid>(
        "SELECT a.id FROM asset a
         JOIN project p ON p.id=a.project_id JOIN firm f ON f.id=p.firm_id
         JOIN person client ON client.id=$4
         WHERE a.id=$1 AND p.id=$2 AND f.id=$3 AND client.email=$5
           AND EXISTS (SELECT 1 FROM person_project_role r
                       WHERE r.project_id=p.id AND r.person_id=client.id
                         AND r.participation='client' AND r.is_client_dri)
           AND (p.client_dri_person_id=client.id OR
               (p.client_dri_person_id IS NULL AND NOT EXISTS (
                   SELECT 1 FROM person_project_role other
                   WHERE other.project_id=p.id AND other.person_id<>client.id
                     AND other.participation='client' AND other.is_client_dri)))
           AND a.sha256_hex=$6 AND p.status IN ('pitch','open')
           AND a.deleted_at IS NULL AND a.kind='onboarding' AND a.slug IS NOT NULL
           AND a.content_type='application/pdf'
           AND (a.signing IS NULL OR (a.signing->>'method'='dropbox_sign' AND a.signing->>'signed_on'=$7::date::text))
           AND NOT EXISTS (SELECT 1 FROM asset newer
                           WHERE newer.project_id=p.id AND newer.slug=a.slug AND newer.id>a.id)
         FOR SHARE OF a,p,f,client",
    )
    .bind(evidence.asset_id)
    .bind(evidence.project_id)
    .bind(evidence.firm_id)
    .bind(evidence.client_id)
    .bind(evidence.client_email)
    .bind(evidence.sha256)
    .bind(evidence.signed_on)
    .fetch_optional(&mut *tx)
    .await?;
    if current.is_none() {
        return Ok(false);
    }
    let written = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO onboarding_signature_evidence
             (asset_id,project_id,firm_id,provider,provider_request_id,provider_account_email,signed_on,verified_by)
         VALUES ($1,$2,$3,'dropbox_sign',$4,$5,$6,$7)
         ON CONFLICT DO NOTHING
         RETURNING asset_id",
    )
    .bind(evidence.asset_id)
    .bind(evidence.project_id)
    .bind(evidence.firm_id)
    .bind(evidence.request_id)
    .bind(evidence.account_email)
    .bind(evidence.signed_on)
    .bind(evidence.actor)
    .fetch_optional(&mut *tx)
    .await?;
    let verified = written.is_some()
        || sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM onboarding_signature_evidence
            WHERE asset_id=$1 AND project_id=$2 AND firm_id=$3 AND provider='dropbox_sign'
              AND provider_request_id=$4 AND provider_account_email=$5 AND signed_on=$6)",
        )
        .bind(evidence.asset_id)
        .bind(evidence.project_id)
        .bind(evidence.firm_id)
        .bind(evidence.request_id)
        .bind(evidence.account_email)
        .bind(evidence.signed_on)
        .fetch_one(&mut *tx)
        .await?;
    if dry_run {
        tx.rollback().await?;
    } else {
        tx.commit().await?;
    }
    Ok(verified)
}

/// Methods recorded on current, signed onboarding filings. These are document
/// declarations and do not change the Project's verified lifecycle.
pub async fn filed_signing_methods(
    pg: &PostgresDb,
    projects: &[Uuid],
) -> Result<HashMap<Uuid, BTreeSet<SigningMethod>>, sqlx::Error> {
    crate::postgres::history::require_scope(pg, crate::postgres::history::Family::Notation)
        .await
        .map_err(|error| sqlx::Error::Configuration(Box::new(error)))?;
    let rows: Vec<(Uuid, serde_json::Value)> = sqlx::query_as(
        "SELECT DISTINCT h.project_id,h.signing FROM asset h
         WHERE h.project_id=ANY($1) AND h.deleted_at IS NULL AND h.signing IS NOT NULL
           AND NOT EXISTS (
               SELECT 1 FROM asset newer
               WHERE newer.project_id=h.project_id AND newer.slug=h.slug AND newer.id>h.id
           ) AND (h.kind='onboarding' OR EXISTS (
                 SELECT 1 FROM notation n JOIN template t ON t.id=n.template_id
                 LEFT JOIN notation_document d ON d.notation_id=n.id
                 WHERE n.project_id=h.project_id AND t.kind='onboarding' AND
                     (d.original_asset_id=h.id OR
                      'notations/'||n.id||'/signed-document.pdf' IN (h.storage_key,h.secondary_storage_key) OR
                      'notations/'||n.id||'/document.pdf' IN (h.storage_key,h.secondary_storage_key))
             ))"
    )
    .bind(projects)
    .fetch_all(pg.pool())
    .await?;
    let mut methods = HashMap::<_, BTreeSet<_>>::new();
    for (project, record) in rows {
        if let Ok(signing) = serde_json::from_value::<Signing>(record) {
            methods.entry(project).or_default().insert(signing.method);
        }
    }
    Ok(methods)
}

/// The document ID printed on a Dropbox Sign audit trail.
#[must_use]
pub fn valid_dropbox_sign_request_id(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Whether a provider's request ID has the shape required for execution evidence.
fn evidence_id_is_valid(provider: &str, id: &str) -> bool {
    match provider {
        "docusign" => Uuid::parse_str(id).is_ok_and(|id| !id.is_nil()),
        "dropbox_sign" => valid_dropbox_sign_request_id(id),
        _ => false,
    }
}

#[cfg(test)]
mod evidence_tests {
    use super::evidence_id_is_valid;

    #[test]
    fn execution_evidence_requires_a_provider_specific_request_id() {
        assert!(evidence_id_is_valid(
            "docusign",
            "be7c549b-29fc-8c65-8185-fd527ff4e506"
        ));
        assert!(!evidence_id_is_valid("docusign", "x"));
        assert!(!evidence_id_is_valid(
            "docusign",
            "00000000-0000-0000-0000-000000000000"
        ));
        assert!(evidence_id_is_valid(
            "dropbox_sign",
            "0123456789abcdef0123456789abcdef01234567"
        ));
        assert!(!evidence_id_is_valid("dropbox_sign", "c3f1a9e0d2b4"));
        assert!(!evidence_id_is_valid(
            "other",
            "0123456789abcdef0123456789abcdef01234567"
        ));
    }
}

/// Where a pitch's e-signature request stands, for the lawyer dashboard pill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EsignSummary {
    /// The provider key, such as `docusign` or `dropbox_sign`.
    pub provider: String,
    /// The provider's envelope or request id.
    pub provider_id: String,
    /// `requested`, `declined`, `voided` or `expired`.
    pub state: String,
}

/// Existing onboarding agreements on the caller's participating Projects, including standalone filings.
pub async fn agreements(
    pg: &PostgresDb,
    projects: &[Uuid],
) -> Result<Vec<model::projects::OnboardingAgreement>, sqlx::Error> {
    crate::postgres::history::require_scope(pg, crate::postgres::history::Family::Notation)
        .await
        .map_err(|e| sqlx::Error::Configuration(Box::new(e)))?;
    let rows = sqlx::query_scalar::<_, sqlx::types::Json<model::projects::OnboardingAgreement>>(
        "WITH runs AS (
            SELECT n.project_id, t.title, n.id AS notation_id, COALESCE(s.state,n.state) AS state,
                   s.provider_id AS envelope_id
            FROM notation n JOIN template t ON t.id=n.template_id
            LEFT JOIN LATERAL (SELECT state,provider_id FROM signature WHERE notation_id=n.id
                               ORDER BY inserted_at DESC,id DESC LIMIT 1) s ON true
            WHERE n.project_id=ANY($1) AND t.kind='onboarding'
        ), filings AS (
            SELECT DISTINCT ON (a.project_id,COALESCE(a.slug,a.id::text)) a.project_id,a.filename,a.id,a.metadata,a.source
            FROM asset a WHERE a.project_id=ANY($1) AND a.kind='onboarding' AND a.deleted_at IS NULL
            ORDER BY a.project_id,COALESCE(a.slug,a.id::text),a.inserted_at DESC,a.id DESC
        ), agreements AS (
            SELECT p.code AS project,r.title,r.state,r.notation_id,NULL::uuid AS document_id,r.envelope_id
            FROM runs r JOIN project p ON p.id=r.project_id
            UNION ALL
            SELECT p.code,COALESCE(f.filename,'Onboarding agreement'),'filed',NULL,f.id,f.metadata->>'docusign_envelope_id'
            FROM filings f JOIN project p ON p.id=f.project_id
            WHERE NOT EXISTS (SELECT 1 FROM runs r WHERE r.project_id=f.project_id AND
                (EXISTS (SELECT 1 FROM notation_document d WHERE d.notation_id=r.notation_id AND d.original_asset_id=f.id)
                 OR EXISTS (SELECT 1 FROM signature s WHERE s.notation_id=r.notation_id AND s.provider='docusign'
                            AND (s.provider_id=f.metadata->>'verified_onboarding_envelope_id' OR
                                 (f.source='docusign' AND s.provider_id=f.metadata->>'docusign_envelope_id')))))
        ) SELECT to_jsonb(a) FROM agreements a ORDER BY a.project,a.title,a.notation_id,a.document_id"
    ).bind(projects).fetch_all(pg.pool()).await?;
    Ok(rows.into_iter().map(|row| row.0).collect())
}

/// The latest unexecuted onboarding e-signature request per project, for projects that have one.
pub async fn pending_esign(
    pg: &PostgresDb,
    projects: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, EsignSummary>, sqlx::Error> {
    crate::postgres::history::require_scope(pg, crate::postgres::history::Family::Notation)
        .await
        .map_err(|error| sqlx::Error::Configuration(Box::new(error)))?;
    let rows: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        "SELECT DISTINCT ON (n.project_id) n.project_id, s.provider, s.provider_id, s.state
         FROM signature s JOIN notation n ON n.id=s.notation_id
         JOIN template t ON t.id=n.template_id
         WHERE n.project_id=ANY($1) AND t.kind='onboarding' AND s.state<>'completed'
         ORDER BY n.project_id, s.id DESC",
    )
    .bind(projects)
    .fetch_all(pg.pool())
    .await?;
    Ok(rows
        .into_iter()
        .map(|(project, provider, provider_id, state)| {
            (
                project,
                EsignSummary {
                    provider,
                    provider_id,
                    state,
                },
            )
        })
        .collect())
}

/// Filed opening and closing documents for batched lifecycle reads.
pub async fn filed_documents(
    pg: &PostgresDb,
    projects: &[Uuid],
) -> Result<(HashSet<Uuid>, HashSet<Uuid>), sqlx::Error> {
    crate::postgres::history::require_scope(pg, crate::postgres::history::Family::Notation)
        .await
        .map_err(|error| sqlx::Error::Configuration(Box::new(error)))?;
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT project_id,kind FROM asset
         WHERE project_id=ANY($1) AND deleted_at IS NULL AND kind IN ('onboarding','offboarding')
         UNION SELECT n.project_id,t.kind FROM notation n JOIN template t ON t.id=n.template_id
         JOIN notation_document d ON d.notation_id=n.id
         WHERE n.project_id=ANY($1) AND t.kind IN ('onboarding','offboarding')",
    )
    .bind(projects)
    .fetch_all(pg.pool())
    .await?;
    let mut opening = HashSet::new();
    let mut closing = HashSet::new();
    for (project, kind) in rows {
        if kind == "onboarding" {
            opening.insert(project);
        } else {
            closing.insert(project);
        }
    }
    Ok((opening, closing))
}

/// PostgreSQL revisions, ordered from the current head.
pub async fn revisions(
    pg: &PostgresDb,
    project: Uuid,
    slug: &str,
) -> Result<Vec<Asset>, AssetError> {
    crate::postgres::assets::rows(
        pg,
        "AND r.project_id=($1->>'project')::uuid AND r.slug=$1->>'slug' ORDER BY id DESC",
        serde_json::json!({"project":project,"slug":slug}),
    )
    .await
}

/// The Project's live PostgreSQL documents.
pub async fn documents(pg: &PostgresDb, project: Uuid) -> Result<Vec<Asset>, AssetError> {
    crate::postgres::assets::rows(
        pg,
        "AND r.project_id=($1->>'project')::uuid AND r.deleted_at IS NULL ORDER BY id DESC",
        serde_json::json!({"project":project}),
    )
    .await
}

/// File verified onboarding bytes through the canonical Asset transaction.
pub async fn file_document(
    pg: &PostgresDb,
    storage: &Arc<dyn StorageService>,
    args: &IngestArgs<'_>,
    identity: &DocumentIdentity<'_>,
    expected: Option<&ExpectedHead>,
    bytes: &[u8],
) -> Result<Filed, RevisionError> {
    crate::postgres::assets::file_revision(pg, storage, args, identity, None, expected, bytes).await
}

#[cfg(test)]
mod tests;
