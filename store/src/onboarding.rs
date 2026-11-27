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
/// Two sources count. A completed e-signature request from any provider in
/// [`crate::signatures::SignatureProvider`] counts. So does a live `onboarding` asset whose
/// server-stamped `verified_onboarding_envelope_id` equals its DocuSign envelope UUID. A committed
/// pointer alone is not evidence: anyone with repository access can write one.
pub async fn official_projects(
    pg: &PostgresDb,
    projects: &[Uuid],
) -> Result<HashSet<Uuid>, sqlx::Error> {
    Ok(verified_providers(pg, projects)
        .await?
        .into_keys()
        .collect())
}

/// Signing methods backed by completed provider evidence, keyed by Project.
pub async fn verified_providers(
    pg: &PostgresDb,
    projects: &[Uuid],
) -> Result<HashMap<Uuid, BTreeSet<SigningMethod>>, sqlx::Error> {
    crate::postgres::history::require_scope(pg, crate::postgres::history::Family::Notation)
        .await
        .map_err(|error| sqlx::Error::Configuration(Box::new(error)))?;
    let providers: Vec<&str> = crate::signatures::SignatureProvider::ALL
        .iter()
        .map(crate::signatures::SignatureProvider::as_str)
        .collect();
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT n.project_id, s.provider, s.provider_id FROM signature s
         JOIN notation n ON n.id=s.notation_id JOIN template t ON t.id=n.template_id
         WHERE n.project_id=ANY($1) AND t.kind='onboarding'
           AND s.provider=ANY($2) AND s.state='completed' AND s.signed_at IS NOT NULL
         UNION
         SELECT project_id, 'docusign', metadata->>'docusign_envelope_id' FROM asset
         WHERE project_id=ANY($1) AND kind='onboarding' AND deleted_at IS NULL
           AND metadata->>'verified_onboarding_envelope_id'=metadata->>'docusign_envelope_id'",
    )
    .bind(projects)
    .bind(&providers)
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

/// Whether a provider's request id can stand as execution evidence. DocuSign envelope ids are
/// non-nil UUIDs. Another provider's id need only be present.
fn evidence_id_is_valid(provider: &str, id: &str) -> bool {
    if provider == "docusign" {
        Uuid::parse_str(id).is_ok_and(|id| !id.is_nil())
    } else {
        !id.trim().is_empty()
    }
}

#[cfg(test)]
mod evidence_tests {
    use super::evidence_id_is_valid;

    #[test]
    fn a_docusign_id_must_be_a_real_uuid_and_another_providers_id_must_be_present() {
        assert!(evidence_id_is_valid(
            "docusign",
            "be7c549b-29fc-8c65-8185-fd527ff4e506"
        ));
        assert!(!evidence_id_is_valid("docusign", "x"));
        assert!(!evidence_id_is_valid(
            "docusign",
            "00000000-0000-0000-0000-000000000000"
        ));
        assert!(evidence_id_is_valid("dropbox_sign", "c3f1a9e0d2b4"));
        assert!(!evidence_id_is_valid("dropbox_sign", "  "));
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
