//! Source pages for court-date review, resolved from the recorded proposal revision.
use std::{
    collections::BTreeSet,
    io::Cursor,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use axum::{
    extract::{Path, Query, State},
    http::header,
    response::{IntoResponse, Redirect, Response},
};
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
use model::deadline_proposals::{Draft, Evidence, Proposal, Role};
use serde::Deserialize;
use store::deadline_proposals::{SourceReceipt, VerifiedForgejo};
use store::{assets::Asset, surreal::SurrealDb};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::api::{ApiError, ApiState, AuthedSession};

const MAX_SOURCE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_PAGE_EDGE: u16 = 1600;
static PAGE_WORKERS: Semaphore = Semaphore::const_new(2);

fn invalid(message: &'static str) -> ApiError {
    ApiError::DeadlineInvalid(message.into())
}

#[derive(Deserialize)]
pub(crate) struct PageQuery {
    revision: i64,
}

pub(crate) async fn page(
    State(state): State<ApiState>,
    AuthedSession(session): AuthedSession,
    Path((project, proposal, index)): Path<(Uuid, Uuid, usize)>,
    Query(query): Query<PageQuery>,
) -> Result<Response, ApiError> {
    let (source, asset, _) =
        recorded_source(&state, &session, project, proposal, index, query.revision).await?;
    let png = original_page(state.storage.as_ref(), &asset, &source).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "private, no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; sandbox",
            ),
        ],
        png,
    )
        .into_response())
}

pub(crate) async fn forgejo_source(
    State(state): State<ApiState>,
    AuthedSession(session): AuthedSession,
    Path((project, proposal, index)): Path<(Uuid, Uuid, usize)>,
    Query(query): Query<PageQuery>,
) -> Result<Response, ApiError> {
    let (source, asset, receipt) =
        recorded_source(&state, &session, project, proposal, index, query.revision).await?;
    let saved = receipt.forgejo.ok_or(ApiError::NotFound)?;
    let forge = state
        .integration_providers
        .forgejo(&state.surreal, project)
        .await
        .map_err(|_| forge_error(store::court_date_forgejo::Error::Unavailable))?;
    let url =
        store::court_date_forgejo::reopen(&state.surreal, project, &source, &asset, &saved, &forge)
            .await
            .map_err(forge_error)?;
    Ok((
        [
            (header::CACHE_CONTROL, "private, no-store"),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        Redirect::to(&url),
    )
        .into_response())
}

async fn recorded_source(
    state: &ApiState,
    session: &crate::SessionData,
    project: Uuid,
    proposal: Uuid,
    index: usize,
    revision: i64,
) -> Result<(Evidence, Asset, SourceReceipt), ApiError> {
    crate::court_dates_api::project_reviewer(state, session, project).await?;
    let pg = crate::court_dates_api::database().await?;
    let record = store::deadline_proposals::get_revision(&pg, project, proposal, revision)
        .await
        .map_err(crate::court_dates_api::map_error)?
        .ok_or(ApiError::NotFound)?;
    let source = record
        .draft
        .evidence
        .into_iter()
        .nth(index)
        .ok_or(ApiError::NotFound)?;
    if source.role == Role::Authority && !session.role.is_lawyer_tier() {
        return Err(ApiError::Forbidden);
    }
    let receipt =
        store::deadline_proposals::evidence_receipt(&pg, project, proposal, revision, index)
            .await
            .map_err(crate::court_dates_api::map_error)?
            .ok_or(ApiError::NotFound)?;
    let asset = store::assets::find_by_id(&state.surreal, receipt.asset_id)
        .await
        .map_err(|_| ApiError::Db("could not read archived source".into()))?
        .ok_or(ApiError::NotFound)?;
    validate_asset(project, &asset, &source)?;
    Ok((source, asset, receipt))
}

fn forge_error(error: store::court_date_forgejo::Error) -> ApiError {
    use store::court_date_forgejo::Error;
    match error {
        Error::Invalid => ApiError::CourtDateConflict,
        Error::NotFound => ApiError::NotFound,
        Error::Forbidden => ApiError::Forbidden,
        Error::Unavailable => ApiError::CourtDateSourceUnavailable,
    }
}

/// Confirm that the requested page of every active original can actually be opened.
async fn verify_originals(
    state: &ApiState,
    project: Uuid,
    draft: &Draft,
    can_read_authorities: bool,
) -> Result<(), ApiError> {
    draft.validate().map_err(invalid)?;
    let mut rendered = BTreeSet::new();
    for evidence in &draft.evidence {
        let asset = source_asset(
            &state.surreal,
            project,
            draft,
            evidence,
            can_read_authorities,
        )
        .await?;
        if rendered.insert((asset.id, evidence.page)) {
            original_page(state.storage.as_ref(), &asset, evidence).await?;
        }
    }
    Ok(())
}

pub(crate) async fn verify(
    state: &ApiState,
    project: Uuid,
    draft: &Draft,
    can_read_authorities: bool,
) -> Result<Vec<VerifiedForgejo>, ApiError> {
    verify_originals(state, project, draft, can_read_authorities).await?;
    if !draft.evidence.iter().any(|e| e.forgejo.is_some()) {
        return Ok(Vec::new());
    }
    let forge = state
        .integration_providers
        .forgejo(&state.surreal, project)
        .await
        .map_err(|_| forge_error(store::court_date_forgejo::Error::Unavailable))?;
    store::court_date_forgejo::verify_with(
        &state.surreal,
        project,
        draft,
        can_read_authorities,
        &forge,
    )
    .await
    .map_err(forge_error)
}

pub(crate) async fn verify_confirmation(
    state: &ApiState,
    project: Uuid,
    proposal: &Proposal,
) -> Result<(), ApiError> {
    verify_originals(state, project, &proposal.draft, true).await?;
    if !proposal.draft.evidence.iter().any(|e| e.forgejo.is_some()) {
        return Ok(());
    }
    let pg = crate::court_dates_api::database().await?;
    let forge = state
        .integration_providers
        .forgejo(&state.surreal, project)
        .await
        .map_err(|_| forge_error(store::court_date_forgejo::Error::Unavailable))?;
    let mut checked = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    for (index, source) in proposal
        .draft
        .evidence
        .iter()
        .enumerate()
        .filter(|(_, e)| e.forgejo.is_some())
    {
        let receipt = store::deadline_proposals::evidence_receipt(
            &pg,
            project,
            proposal.id,
            proposal.revision,
            index,
        )
        .await
        .map_err(crate::court_dates_api::map_error)?
        .ok_or(ApiError::CourtDateConflict)?;
        let saved = receipt.forgejo.ok_or(ApiError::CourtDateConflict)?;
        if checked.contains(&saved) {
            continue;
        }
        let asset = store::assets::find_by_id(&state.surreal, receipt.asset_id)
            .await
            .map_err(|_| ApiError::Db("could not read filed source".into()))?
            .ok_or(ApiError::NotFound)?;
        tokio::time::timeout_at(
            deadline,
            store::court_date_forgejo::reopen(
                &state.surreal,
                project,
                source,
                &asset,
                &saved,
                &forge,
            ),
        )
        .await
        .map_err(|_| ApiError::CourtDateSourceUnavailable)?
        .map_err(forge_error)?;
        checked.push(saved);
    }
    Ok(())
}

async fn source_asset(
    db: &SurrealDb,
    project: Uuid,
    draft: &Draft,
    evidence: &Evidence,
    can_read_authorities: bool,
) -> Result<Asset, ApiError> {
    evidence.validate().map_err(invalid)?;
    let id = if evidence.role == Role::Authority {
        if !can_read_authorities {
            return Err(ApiError::Forbidden);
        }
        let authority =
            store::authorities::find(db, store::authorities::AuthorityLookup::Id(evidence.id))
                .await
                .map_err(|_| ApiError::Db("could not read filed authority".into()))?
                .ok_or(ApiError::NotFound)?;
        if !matches!(
            authority.class.as_str(),
            "statute" | "regulation" | "case_law"
        ) || draft
            .calculation
            .as_ref()
            .is_some_and(|calc| calc.authority_class() != Some(authority.class.as_str()))
        {
            return Err(invalid(
                "the filed authority class does not support this calculation",
            ));
        }
        authority.archived_asset_id.ok_or(ApiError::NotFound)?
    } else {
        evidence.id
    };
    let asset = store::assets::find_by_id(db, id)
        .await
        .map_err(|_| ApiError::Db("could not read filed source".into()))?
        .ok_or(ApiError::NotFound)?;
    validate_asset(project, &asset, evidence)?;
    if evidence.role != Role::Authority {
        if let Some(slug) = asset.slug.as_deref() {
            let current = store::assets::current(db, project, slug, store::assets::Lens::Lawyer)
                .await
                .map_err(|_| ApiError::Db("could not check the source revision".into()))?;
            if current.is_none_or(|head| head.id != id) {
                return Err(invalid("the cited document revision has been superseded"));
            }
        }
    }
    if evidence.role == Role::SignedOrder {
        let filings = store::efiling_transactions::list(db, project)
            .await
            .map_err(|_| ApiError::Db("could not check the entered order".into()))?;
        if filings.iter().any(|row| {
            row.needs_entered_order
                && (row.submitted_document_id == Some(id) || row.conformed_document_id == Some(id))
        }) {
            return Err(invalid("the filing is awaiting the judge’s entered order"));
        }
    }
    Ok(asset)
}

fn validate_asset(project: Uuid, asset: &Asset, evidence: &Evidence) -> Result<(), ApiError> {
    let expected_project = (evidence.role != Role::Authority).then_some(project);
    if asset.project_id != expected_project
        || asset.deleted_at.is_some()
        || asset.purged_at.is_some()
    {
        return Err(ApiError::NotFound);
    }
    if !matches!(
        asset.content_type.as_str(),
        "application/pdf" | "image/png" | "image/jpeg" | "application/octet-stream"
    ) {
        return Err(invalid(
            "court-date evidence requires a filed PDF, PNG, or JPEG",
        ));
    }
    if asset.source.as_deref() == Some(store::documents::source::GENERATED)
        || asset.derived_from.is_some()
        || asset.kind.as_deref() == Some("transcript")
    {
        return Err(invalid("court-date evidence requires the filed original"));
    }
    if evidence.role == Role::SignedOrder && asset.kind.as_deref() != Some("order") {
        return Err(invalid("the filed source must be classified as an order"));
    }
    if evidence.sha256_hex != asset.sha256_hex {
        return Err(invalid("the source digest differs from the filed revision"));
    }
    Ok(())
}

fn storage_error(error: &cloud::StorageError) -> ApiError {
    match error {
        cloud::StorageError::NotFound(_) => ApiError::NotFound,
        _ => ApiError::Db("could not read filed source bytes".into()),
    }
}

async fn original_page(
    storage: &dyn cloud::StorageService,
    asset: &Asset,
    source: &Evidence,
) -> Result<Vec<u8>, ApiError> {
    let size = u64::try_from(asset.byte_size)
        .ok()
        .filter(|n| (1..=MAX_SOURCE_BYTES).contains(n))
        .ok_or_else(|| invalid("the source is empty or exceeds the 32 MiB review limit"))?;
    let permit = PAGE_WORKERS
        .acquire()
        .await
        .map_err(|_| ApiError::Db("page renderer unavailable".into()))?;
    let head = storage
        .head(&asset.storage_key)
        .await
        .map_err(|error| storage_error(&error))?
        .ok_or(ApiError::NotFound)?;
    if head.size_bytes != size {
        return Err(invalid("the source size differs from the filed revision"));
    }
    let bytes = storage
        .get(&asset.storage_key)
        .await
        .map_err(|error| storage_error(&error))?
        .bytes;
    let digest = source.sha256_hex.clone();
    let page = source.page;
    tokio::task::spawn_blocking(move || {
        // Keep the permit while the renderer runs even if the HTTP request is cancelled.
        let _permit = permit;
        if bytes.len() as u64 != size || store::assets::sha256_hex(&bytes) != digest {
            return Err("the source bytes differ from the filed revision");
        }
        render(bytes, page)
    })
    .await
    .map_err(|_| invalid("the original page could not be rendered"))?
    .map_err(invalid)
}

/// Hayro draws authored normal appearance streams. A viewer-generated field
/// or stamp without one may carry a judge's signature, so refuse that page.
fn displayable_annotations(page: &hayro::hayro_syntax::page::Page<'_>) -> bool {
    use hayro::hayro_syntax::object::{Array, Dict, Name, Rect, Stream};
    let Some(items) = page.raw().get::<Array<'_>>(b"Annots") else {
        return true;
    };
    items.iter::<Dict<'_>>().all(|annotation| {
        let flags = annotation.get::<u32>(b"F").unwrap_or(0);
        if flags & 2 != 0
            || annotation
                .get::<Rect>(b"Rect")
                .is_some_and(|r| r.width() == 0. || r.height() == 0.)
        {
            return true;
        }
        // Hyperlinks commonly have no painted appearance of their own.
        annotation
            .get::<Name<'_>>(b"Subtype")
            .is_some_and(|name| name.as_str() == "Link")
            || annotation
                .get::<Dict<'_>>(b"AP")
                .and_then(|ap| ap.get::<Stream<'_>>(b"N"))
                .is_some()
    })
}

fn render(bytes: Vec<u8>, page_number: u32) -> Result<Vec<u8>, &'static str> {
    const UNREADABLE: &str = "the filed original is not a readable PDF, PNG, or JPEG";
    const MISSING_PAGE: &str = "the cited page is outside the filed original";
    let index = page_number.checked_sub(1).ok_or(MISSING_PAGE)? as usize;
    if bytes.starts_with(b"%PDF-") {
        let document = hayro::hayro_syntax::Pdf::new(bytes).map_err(|_| UNREADABLE)?;
        let page = document.pages().get(index).ok_or(MISSING_PAGE)?;
        if !displayable_annotations(page) {
            return Err("the filed page contains an annotation the renderer cannot reproduce");
        }
        let (width, height) = page.render_dimensions();
        if !width.is_finite() || !height.is_finite() || width < 1. || height < 1. {
            return Err(UNREADABLE);
        }
        let scale = f32::from(MAX_PAGE_EDGE) / width.max(height);
        if (width.min(height) * scale) < 1. {
            return Err(UNREADABLE);
        }
        let warned = Arc::new(AtomicBool::new(false));
        let warning_flag = warned.clone();
        let settings = hayro::hayro_interpret::InterpreterSettings {
            warning_sink: Arc::new(move |_| warning_flag.store(true, Ordering::Relaxed)),
            ..Default::default()
        };
        let pixels = hayro::render(
            page,
            &hayro::RenderCache::new(),
            &settings,
            &hayro::RenderSettings::default(),
            &hayro::PixmapSettings {
                x_scale: scale,
                y_scale: scale,
                bg_color: hayro::vello_cpu::color::palette::css::WHITE,
            },
        );
        if warned.load(Ordering::Relaxed) {
            return Err("the renderer could not reproduce all of the filed page");
        }
        return pixels.into_png().map_err(|_| UNREADABLE);
    }
    let format = image::guess_format(&bytes).map_err(|_| UNREADABLE)?;
    if !matches!(format, ImageFormat::Png | ImageFormat::Jpeg) {
        return Err(UNREADABLE);
    }
    if index != 0 {
        return Err(MISSING_PAGE);
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(96 * 1024 * 1024);
    limits.max_image_width = Some(12000);
    limits.max_image_height = Some(12000);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|_| UNREADABLE)?;
    if decoder.total_bytes() > 96 * 1024 * 1024 {
        return Err(UNREADABLE);
    }
    let orientation = decoder.orientation().map_err(|_| UNREADABLE)?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(|_| UNREADABLE)?;
    image.apply_orientation(orientation);
    if image.width().max(image.height()) > u32::from(MAX_PAGE_EDGE) {
        image = image.thumbnail(u32::from(MAX_PAGE_EDGE), u32::from(MAX_PAGE_EDGE));
    }
    let mut png = Cursor::new(Vec::new());
    image
        .write_to(&mut png, ImageFormat::Png)
        .map_err(|_| UNREADABLE)?;
    Ok(png.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloud::StorageService;
    use serde_json::json;

    fn fixture(project: Uuid, bytes: &[u8]) -> (Asset, Evidence) {
        let id = Uuid::new_v4();
        let digest = store::assets::sha256_hex(bytes);
        let asset = serde_json::from_value(json!({
            "id":id, "project_id":project,"storage_key":"fixture", "content_type":"application/pdf",
            "byte_size":bytes.len(), "sha256_hex":digest, "kind":"order","source":"upload","visibility":"internal",
            "inserted_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z",
            "signing":{"method":"wet_ink","signed_on":"2026-01-01"}
        })).unwrap();
        (
            asset,
            Evidence {
                role: Role::SignedOrder,
                forgejo: None,
                id,
                sha256_hex: digest,
                page: 1,
                rects: vec![],
            },
        )
    }

    #[test]
    fn rasterizes_the_selected_archived_page_with_its_own_content_and_dimensions() {
        let bytes = pdf::render("#page(width: 144pt, height: 72pt, margin: 0pt, fill: rgb(\"#ff0000\"))[]\n#page(width: 72pt, height: 144pt, margin: 0pt, fill: rgb(\"#0000ff\"))[]").unwrap();
        for (page, landscape, channel) in [(1, true, 0usize), (2, false, 2)] {
            let png = render(bytes.clone(), page).unwrap();
            let page = image::load_from_memory_with_format(&png, ImageFormat::Png)
                .unwrap()
                .to_rgb8();
            let (w, h) = page.dimensions();
            assert_eq!(w > h, landscape);
            let pixel = page.get_pixel(w / 2, h / 2);
            assert!(pixel[channel] > 200 && pixel[1] < 100);
        }
        assert!(render(bytes, 3).is_err());
        assert!(render(b"%PDF-1.4 broken file".to_vec(), 1).is_err());
    }

    #[test]
    fn refuses_a_visible_signature_widget_without_a_renderable_appearance() {
        use std::fmt::Write as _;
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 120 100] /Annots [4 0 R] /Resources <<>> >>",
            "<< /Type /Annot /Subtype /Widget /FT /Sig /Rect [10 20 90 40] >>",
        ];
        let mut pdf = String::from("%PDF-1.4\n");
        let mut offsets = vec![];
        for (ix, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            writeln!(&mut pdf, "{} 0 obj\n{object}\nendobj", ix + 1).unwrap();
        }
        let xref = pdf.len();
        pdf.push_str("xref\n0 5\n0000000000 65535 f \n");
        for offset in offsets {
            writeln!(&mut pdf, "{offset:010} 00000 n ").unwrap();
        }
        writeln!(
            &mut pdf,
            "trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF"
        )
        .unwrap();
        assert!(render(pdf.into_bytes(), 1)
            .unwrap_err()
            .contains("annotation"));
    }

    #[test]
    fn accepts_an_original_scan_only_on_its_first_page() {
        let mut png = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(image::RgbImage::from_pixel(4, 2, image::Rgb([72, 16, 220])))
            .write_to(&mut png, ImageFormat::Png)
            .unwrap();
        let bytes = png.into_inner();
        let rendered = render(bytes.clone(), 1).unwrap();
        assert_eq!(
            image::load_from_memory(&rendered)
                .unwrap()
                .to_rgb8()
                .get_pixel(0, 0)
                .0,
            [72, 16, 220]
        );
        assert!(render(bytes, 2).is_err());
        assert!(render(b"reconstructed text".to_vec(), 1).is_err());
    }

    #[test]
    fn validates_source_scope_order_classification_and_version_from_asset_metadata() {
        let project = Uuid::new_v4();
        let (asset, mut evidence) = fixture(project, b"source");
        assert!(validate_asset(project, &asset, &evidence).is_ok());
        assert!(matches!(
            validate_asset(Uuid::new_v4(), &asset, &evidence),
            Err(ApiError::NotFound)
        ));
        let mut modified = asset.clone();
        // The lawyer attests to the court’s signature on the image during confirmation.
        modified.signing = None;
        assert!(validate_asset(project, &modified, &evidence).is_ok());
        modified = asset.clone();
        modified.kind = Some("filing".into());
        assert!(validate_asset(project, &modified, &evidence).is_err());
        evidence.role = Role::Trigger;
        assert!(validate_asset(project, &modified, &evidence).is_ok());
        modified.source = Some(store::documents::source::GENERATED.into());
        assert!(validate_asset(project, &modified, &evidence).is_err());
        modified = asset.clone();
        modified.derived_from = Some(json!({"id":"original"}));
        assert!(validate_asset(project, &modified, &evidence).is_err());
        modified = asset.clone();
        modified.deleted_at = Some(chrono::Utc::now());
        assert!(matches!(
            validate_asset(project, &modified, &evidence),
            Err(ApiError::NotFound)
        ));
        evidence.role = Role::Authority;
        assert!(matches!(
            validate_asset(project, &asset, &evidence),
            Err(ApiError::NotFound)
        ));
        modified = asset;
        modified.project_id = None;
        assert!(validate_asset(project, &modified, &evidence).is_ok());
        evidence.sha256_hex = "0".repeat(64);
        assert!(validate_asset(project, &modified, &evidence).is_err());
    }

    #[tokio::test]
    async fn resolves_only_current_entered_orders_and_permitted_archived_authorities() {
        store::test_support::with_postgres(async {
            let db = SurrealDb::uninitialized();
            let pg = store::postgres::PostgresDb::application().await.unwrap();
            let (project, _) = store::test_support::seed_calendar_project().await;
            let mut draft: Draft = serde_json::from_value(json!({"kind":"signed_order", "title":"Fixture",
                "timezone":"America/New_York"})).unwrap();
            let (_, mut evidence) = fixture(project,b"fixture source");
            let first = Uuid::now_v7();
            let latest = Uuid::now_v7();
            let archive = Uuid::new_v4();
            for (id,scope,slug) in [(first,Some(project),Some("order")), (latest,Some(project),Some("order")), (archive,None,None)] {
                sqlx::query("INSERT INTO asset(id,project_id,slug,kind,source,storage_key,content_type,byte_size,sha256_hex)
                    VALUES($1,$2,$3,'order','upload','fixture','application/pdf',14,$4)").bind(id).bind(scope).bind(slug).bind(&evidence.sha256_hex)
                    .execute(pg.pool()).await.unwrap();
            }
            evidence.id = first;
            assert!(matches!(source_asset(&db,project,&draft,&evidence,true).await,Err(ApiError::DeadlineInvalid(msg)) if msg.contains("superseded")));
            evidence.id = latest;
            assert!(source_asset(&db, project, &draft, &evidence, true).await.is_ok());
            sqlx::query("INSERT INTO efiling_transactions(id,project_id,transaction_id,case_number,court,efsp,status,
                       note,needs_entered_order,submitted_document_id,inserted_at,updated_at) VALUES(gen_random_uuid(),
                       $1,'fixture','fixture','fixture','fixture','accepted','',true,$2,now(),now())")
                .bind(project).bind(latest).execute(pg.pool()).await.unwrap();
            assert!(matches!(source_asset(&db,project,&draft,&evidence,true).await,Err(ApiError::DeadlineInvalid(msg)) if msg.contains("entered order")));
            assert!(matches!(source_asset(&db,Uuid::new_v4(),&draft,&evidence,true).await,Err(ApiError::NotFound)));
            evidence.role = Role::Authority;
            evidence.id = sqlx::query_scalar("INSERT INTO authority(class,citation,title,archived_asset_id)
                VALUES('statute','Fixture statute','Fixture',$1) RETURNING id").bind(archive)
                .fetch_one(pg.pool()).await.unwrap();
            assert!(matches!(source_asset(&db,project,&draft,&evidence,false).await,Err(ApiError::Forbidden)));
            draft.calculation = Some(serde_json::from_value(json!({"trigger":"Receipt","trigger_on":"2026-11-01",
                "offset_days":2,"counting_basis":"calendar_days","authority_kind":"statute"})).unwrap());
            assert_eq!(source_asset(&db,project,&draft,&evidence,true).await.unwrap().id,archive);
            draft.calculation.as_mut().unwrap().authority_kind = model::project_deadlines::AuthorityKind::CourtRule;
            assert!(matches!(source_asset(&db,project,&draft,&evidence,true).await,Err(ApiError::DeadlineInvalid(msg)) if msg.contains("authority class")));
        }).await;
    }

    #[tokio::test]
    async fn missing_changed_and_oversized_originals_never_produce_an_evidence_image() {
        let temp = tempfile::tempdir().unwrap();
        let storage = cloud::FsStorage::new(temp.path()).await.unwrap();
        let bytes = b"same length";
        let (mut asset, source) = fixture(Uuid::new_v4(), bytes);
        assert!(matches!(
            original_page(&storage, &asset, &source).await,
            Err(ApiError::NotFound)
        ));
        storage
            .put(&asset.storage_key, b"bad content", "application/pdf")
            .await
            .unwrap();
        assert!(
            matches!(original_page(&storage,&asset,&source).await, Err(ApiError::DeadlineInvalid(reason)) if reason.contains("bytes differ"))
        );
        asset.byte_size += 1;
        assert!(
            matches!(original_page(&storage,&asset,&source).await, Err(ApiError::DeadlineInvalid(reason)) if reason.contains("size differs"))
        );
        asset.byte_size = i64::try_from(MAX_SOURCE_BYTES).unwrap() + 1;
        assert!(
            matches!(original_page(&storage,&asset,&source).await, Err(ApiError::DeadlineInvalid(reason)) if reason.contains("limit"))
        );
    }
}
