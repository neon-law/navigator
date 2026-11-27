//! Bind a court-date citation to the archived version and an immutable private Forgejo blob.
use crate::{
    application_store::ApplicationStore, assets::Asset, deadline_proposals::VerifiedForgejo,
};
use cloud::forgejo_forge::{CitationFile, CitationRepository};
use model::deadline_proposals::{Draft, Evidence, ForgejoRepository, Role};
use model::document_pointers::DocumentPointer;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum Error {
    #[error("the pinned Forgejo source differs from the archived original")]
    Invalid,
    #[error("the pinned source was not found")]
    NotFound,
    #[error("this source requires lawyer access")]
    Forbidden,
    #[error("the owning Firm’s Forgejo could not verify the citation")]
    Unavailable,
}

fn forge_error(error: &cloud::forge::ForgeError) -> Error {
    use cloud::forge::ForgeError;
    match error {
        ForgeError::Api { status: 404, .. } => Error::NotFound,
        ForgeError::InvalidConfig(_)
        | ForgeError::NotPrivate { .. }
        | ForgeError::ForeignRepository { .. } => Error::Invalid,
        _ => Error::Unavailable,
    }
}

async fn forge(
    db: &ApplicationStore,
    project: Uuid,
    kms: &dyn cloud::RuntimeKms,
) -> Result<cloud::ForgejoForge, Error> {
    crate::project_surfaces::forgejo_for_project(db, project, kms, |key| std::env::var(key).ok())
        .await
        .map_err(|_| Error::Unavailable)
}

/// A Project pointer names its exact filed asset; an Authority pointer also names the global Authority.
fn matches_archive(bytes: &[u8], asset: &Asset, evidence: &Evidence) -> bool {
    if i64::try_from(bytes.len()).ok() == Some(asset.byte_size)
        && crate::assets::sha256_hex(bytes) == asset.sha256_hex
    {
        return true;
    }
    if bytes.len() > 256 * 1024 {
        return false;
    }
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| DocumentPointer::from_yaml(text).ok())
        .is_some_and(|p| {
            p.current_version.asset_id == asset.id
                && p.current_version.sha256 == asset.sha256_hex
                && p.current_version.size_bytes == asset.byte_size
                && p.authority_id == (evidence.role == Role::Authority).then_some(evidence.id)
        })
}

async fn archive(
    db: &ApplicationStore,
    project: Uuid,
    source: &Evidence,
    can_read_authorities: bool,
) -> Result<Asset, Error> {
    let id = if source.role == Role::Authority {
        if !can_read_authorities {
            return Err(Error::Forbidden);
        }
        crate::authorities::find(db, crate::authorities::AuthorityLookup::Id(source.id))
            .await
            .map_err(|_| Error::Unavailable)?
            .and_then(|a| a.archived_asset_id)
            .ok_or(Error::NotFound)?
    } else {
        source.id
    };
    let asset = crate::assets::find_by_id(db, id)
        .await
        .map_err(|_| Error::Unavailable)?
        .ok_or(Error::NotFound)?;
    if asset.project_id != (source.role != Role::Authority).then_some(project)
        || asset.deleted_at.is_some()
        || asset.purged_at.is_some()
    {
        return Err(Error::NotFound);
    }
    if asset.sha256_hex != source.sha256_hex {
        return Err(Error::Invalid);
    }
    Ok(asset)
}

async fn read(
    forge: &cloud::ForgejoForge,
    repo: CitationRepository<'_>,
    source: &Evidence,
    asset: &Asset,
) -> Result<CitationFile, Error> {
    let pin = source.forgejo.as_ref().ok_or(Error::Invalid)?;
    let file = forge
        .citation_file(repo, pin)
        .await
        .map_err(|e| forge_error(&e))?;
    if !matches_archive(&file.bytes, asset, source) {
        return Err(Error::Invalid);
    }
    Ok(file)
}

/// Calls only the owning Firm's configured Forgejo with its own scoped credential.
/// The caller enforces participation before checking any submitted source.
pub async fn verify(
    db: &ApplicationStore,
    project: Uuid,
    draft: &Draft,
    can_read_authorities: bool,
    kms: &dyn cloud::RuntimeKms,
) -> Result<Vec<VerifiedForgejo>, Error> {
    if !draft.evidence.iter().any(|e| e.forgejo.is_some()) {
        return Ok(Vec::new());
    }
    let forge = forge(db, project, kms).await?;
    verify_with(db, project, draft, can_read_authorities, &forge).await
}

/// Verify with the authenticated Project provider selected by the caller.
pub async fn verify_with(
    db: &ApplicationStore,
    project: Uuid,
    draft: &Draft,
    can_read_authorities: bool,
    forge: &cloud::ForgejoForge,
) -> Result<Vec<VerifiedForgejo>, Error> {
    draft.validate().map_err(|_| Error::Invalid)?;
    if !can_read_authorities
        && draft
            .evidence
            .iter()
            .any(|e| e.forgejo.is_some() && e.role == Role::Authority)
    {
        return Err(Error::Forbidden);
    }
    let project_row = crate::projects::find_by_id(db, project)
        .await
        .map_err(|_| Error::Unavailable)?
        .ok_or(Error::NotFound)?;

    let mut verified = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    for source in &draft.evidence {
        let Some(pin) = source.forgejo.as_ref() else {
            continue;
        };
        if verified.iter().any(|v: &VerifiedForgejo| {
            v.source_id == source.id && v.sha256_hex == source.sha256_hex && &v.pin == pin
        }) {
            continue;
        }
        let asset = archive(db, project, source, can_read_authorities).await?;
        let name = match pin.repository {
            ForgejoRepository::Project => &project_row.code,
            ForgejoRepository::FirmLibrary => crate::firm_library::REPOSITORY,
        };
        let file = tokio::time::timeout_at(
            deadline,
            read(forge, CitationRepository::Named(name), source, &asset),
        )
        .await
        .map_err(|_| Error::Unavailable)??;
        verified.push(VerifiedForgejo {
            project_id: project,
            firm_id: project_row.firm_id.ok_or(Error::NotFound)?,
            source_id: source.id,
            asset_id: asset.id,
            sha256_hex: source.sha256_hex.clone(),
            pin: pin.clone(),
            origin: file.origin,
            owner: forge.owner().to_owned(),
            name: file.name,
            repository_id: file.repository_id,
        });
    }
    Ok(verified)
}

/// Read a previously attested repository by numeric identity. The caller authorizes the
/// recorded version and asset before calling, including a lawyer-tier check for Authority.
pub async fn reopen(
    db: &ApplicationStore,
    project: Uuid,
    source: &Evidence,
    asset: &Asset,
    saved: &VerifiedForgejo,
    forge: &cloud::ForgejoForge,
) -> Result<String, Error> {
    if saved.project_id != project
        || saved.source_id != source.id
        || saved.asset_id != asset.id
        || saved.sha256_hex != source.sha256_hex
        || source.forgejo.as_ref() != Some(&saved.pin)
        || asset.project_id != (source.role != Role::Authority).then_some(project)
        || asset.sha256_hex != source.sha256_hex
        || asset.deleted_at.is_some()
        || asset.purged_at.is_some()
    {
        return Err(Error::Invalid);
    }
    let project_row = crate::projects::find_by_id(db, project)
        .await
        .map_err(|_| Error::Unavailable)?
        .ok_or(Error::NotFound)?;
    if project_row.firm_id != Some(saved.firm_id) {
        return Err(Error::NotFound);
    }
    let file = read(
        forge,
        CitationRepository::Recorded {
            origin: &saved.origin,
            id: saved.repository_id,
        },
        source,
        asset,
    )
    .await?;
    Ok(file.browser_url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn fixture(project: Uuid, id: Uuid, bytes: &[u8]) -> (Asset, Evidence) {
        let digest = crate::assets::sha256_hex(bytes);
        let asset=serde_json::from_value(json!({
            "id":id, "project_id":project, "storage_key":"fixture", "content_type":"application/pdf",
            "byte_size":bytes.len(), "sha256_hex":digest, "kind":"order", "visibility":"internal",
            "inserted_at":"2026-01-01T00:00:00Z", "updated_at":"2026-01-01T00:00:00Z"
        })).unwrap();
        let evidence = serde_json::from_value(
            json!({"id":id,"role":"signed_order","page":1,"sha256_hex":digest}),
        )
        .unwrap();
        (asset, evidence)
    }

    fn pointer(asset: &Asset) -> Value {
        json!({
            "kind":"order", "visibility":"internal",
            "current_version":{
                "version":1, "asset_id":asset.id, "created_at":"2026-01-01T00:00:00Z",
                "sha256":asset.sha256_hex, "size_bytes":asset.byte_size
            }
        })
    }

    #[test]
    fn the_exact_project_pointer_or_original_bytes_bind_to_the_citation() {
        let original = b"synthetic filed original";
        let (asset, source) = fixture(Uuid::now_v7(), Uuid::now_v7(), original);
        assert!(matches_archive(original, &asset, &source));
        assert!(!matches_archive(b"different", &asset, &source));
        let document = pointer(&asset);
        let matches = |value: &Value| {
            matches_archive(
                serde_yaml::to_string(value).unwrap().as_bytes(),
                &asset,
                &source,
            )
        };
        assert!(matches(&document));
        for (field, value) in [
            ("asset_id", json!(Uuid::now_v7())),
            ("sha256", json!("a".repeat(64))),
            ("size_bytes", json!(1)),
        ] {
            let mut changed = document.clone();
            changed["current_version"][field] = value;
            assert!(!matches(&changed));
        }
        let mut authority = source.clone();
        authority.role = Role::Authority;
        authority.id = Uuid::now_v7();
        let mut pointer = document;
        pointer["kind"] = json!("");
        pointer["authority_id"] = json!(authority.id);
        assert!(matches_archive(
            serde_yaml::to_string(&pointer).unwrap().as_bytes(),
            &asset,
            &authority
        ));
        pointer["authority_id"] = json!(Uuid::now_v7());
        assert!(!matches_archive(
            serde_yaml::to_string(&pointer).unwrap().as_bytes(),
            &asset,
            &authority
        ));
    }

    #[tokio::test]
    async fn archived_project_scope_and_lawyer_authority_access_are_checked_before_a_lookup() {
        crate::test_support::with_postgres(check_archive_scope()).await;
    }

    async fn check_archive_scope() {
        let db = ApplicationStore::uninitialized();
        let pg = crate::postgres::PostgresDb::application().await.unwrap();
        let (project, _) = crate::test_support::seed_calendar_project().await;
        let (asset, mut source) = fixture(project, Uuid::now_v7(), b"fixture");
        sqlx::query("INSERT INTO asset(id,project_id,kind,source,storage_key,content_type,byte_size,sha256_hex)
            VALUES($1,$2,'order','upload','synthetic','application/pdf',$3,$4)")
            .bind(asset.id).bind(project).bind(asset.byte_size).bind(&source.sha256_hex)
            .execute(pg.pool()).await.unwrap();
        assert_eq!(
            archive(&db, project, &source, true).await.unwrap().id,
            asset.id
        );
        assert!(matches!(
            archive(&db, Uuid::now_v7(), &source, true).await,
            Err(Error::NotFound)
        ));
        source.sha256_hex = "0".repeat(64);
        assert!(matches!(
            archive(&db, project, &source, true).await,
            Err(Error::Invalid)
        ));
        source.role = Role::Authority;
        assert!(matches!(
            archive(&db, project, &source, false).await,
            Err(Error::Forbidden)
        ));
    }
}
