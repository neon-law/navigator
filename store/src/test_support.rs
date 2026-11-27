//! Shared synthetic infrastructure for workspace tests and explicit development smoke harnesses.
//!
//! PostgreSQL actor tests use [`with_postgres`]: each walk owns a freshly
//! created database admitted through the production fresh-native creation and
//! migration path before fixture data. [`PostgresFixture`] exposes that same lifecycle to a harness
//! which must pass explicit connection options or share its database with a
//! child process. Ordinary application startup never calls these constructors.
//!
//! The remaining helpers build synthetic domain records for each test.

use anyhow::Context as _;
use uuid::Uuid;

/// Load the bundled Questions and Notation templates into a synthetic store.
pub async fn seed_notation_catalog(
    application_store: &crate::application_store::ApplicationStore,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
) -> anyhow::Result<()> {
    crate::seed::seed_notation_catalog(application_store, storage).await
}

/// Seed validated completed enrollment evidence for a synthetic PostgreSQL fixture.
/// This writes the same scoped ledger rows unit fixtures use; accessors still
/// validate the receipt rather than skipping their readiness checks.
pub async fn seed_postgres_family(
    pg: &crate::postgres::PostgresDb,
    family: crate::postgres::history::Family,
) {
    crate::postgres::history::activate_fixture(pg, family).await;
}

/// An inert store handle for tests that prove a PostgreSQL accessor never
/// touches the legacy source engine.
#[must_use]
pub fn uninitialized_store() -> crate::application_store::ApplicationStore {
    crate::application_store::ApplicationStore::uninitialized()
}

/// Synthetic Twilio registry and credential fixtures shared by route tests.
pub mod twilio;

/// A synthetic 40-hex commit sha for a project-scoped template version in a
/// test. `store::templates::save_version` refuses one without a commit.
pub const SYNTHETIC_COMMIT_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

/// An asset row holding `bytes`' digest, for a test that needs a template
/// body to pin but never reads the bytes back. Nothing is written to object
/// storage, so [`crate::templates::body`] on it fails; use
/// [`crate::assets::ingest_content`] when the test renders the body.
pub async fn seed_body_asset(
    _application_store: &crate::application_store::ApplicationStore,
    bytes: &[u8],
) -> Uuid {
    let sha = crate::assets::sha256_hex(bytes);
    let id = Uuid::now_v7();
    let pg = crate::postgres::PostgresDb::application()
        .await
        .expect("admit fixture database");
    sqlx::query(
        "INSERT INTO asset (id,storage_key,content_type,byte_size,sha256_hex,visibility)
                 VALUES ($1,$2,'text/markdown',$3,$4,'internal')",
    )
    .bind(id)
    .bind(format!("blobs/{sha}"))
    .bind(i64::try_from(bytes.len()).unwrap_or(i64::MAX))
    .bind(sha)
    .execute(pg.pool())
    .await
    .expect("seed body asset");
    id
}

/// The `Uuid` behind the `brand` row carrying `key`, if the database holds
/// one. Test fixtures that dress a brand with their own presentation fields
/// look the row up first: the compiled catalog (`seed_compiled_brands`,
/// seeded by both `mem` and `server_surreal`) pre-registers every closed
/// key, and `brand_key_unique` rejects a second CREATE with the same key.
pub async fn brand_id_with_key(
    _application_store: &crate::application_store::ApplicationStore,
    key: &str,
) -> Option<Uuid> {
    let pg = crate::postgres::PostgresDb::application()
        .await
        .expect("admit fixture database");
    sqlx::query_scalar("SELECT id FROM brand WHERE brand_key = $1")
        .bind(key)
        .fetch_optional(pg.pool())
        .await
        .expect("look up the fixture Brand by key")
}

/// Seed a current Project-scoped Template with no body or source commit.
/// This bodyless row exercises retained-data reads; ordinary `save_version`
/// requires a pinned body and commit for a Project-scoped version.
/// PostgreSQL uses normal Template authority admission. Explicit Source reads
/// retain the Source fixture; failed PostgreSQL admission never falls back.
pub async fn seed_unpinned_project_template(
    application_store: &crate::application_store::ApplicationStore,
    project_id: Uuid,
    code: &str,
    title: &str,
    respondent_type: &str,
    kind: Option<&str>,
) -> crate::templates::Template {
    let id = Uuid::now_v7();
    let pg = crate::postgres::matter::authority_for(crate::templates::TABLE)
        .await
        .expect("admit the fixture Template authority");
    sqlx::query("INSERT INTO template(id,code,title,respondent_type,project_id,is_current,kind,current_key,inserted_at,updated_at) VALUES($1,$2,$3,$4,$5,true,$6,$7,now(),now())")
        .bind(id).bind(code).bind(title).bind(respondent_type).bind(project_id)
        .bind(kind).bind(crate::templates::current_key(Some(project_id), code))
        .execute(pg.pool()).await.expect("seed unpinned project template in PostgreSQL");
    crate::templates::find_by_id(application_store, id)
        .await
        .expect("read unpinned project template")
        .expect("unpinned project template exists")
}

/// The synthetic `jurisdiction_id` every [`seed_entity`] row carries.
///
/// The entity's `jurisdiction_id` is a `record<jurisdiction>` link now
/// (ENG-120), and the engine does **not** validate a link, so this
/// fixture still deliberately points it at no row: nothing here needs
/// the jurisdiction resolved, and seeding one would cost every
/// project-focused test a reference row it never reads. A test that
/// *renders* jurisdiction names must seed its own row through
/// `store::jurisdictions` and use that id instead.
pub const SEED_ENTITY_JURISDICTION_ID: Uuid =
    Uuid::from_u128(0x0199_0000_0000_7000_8000_0000_0000_0075);

/// The synthetic `entity_type_id` every [`seed_entity`] row carries.
/// Same contract as [`SEED_ENTITY_JURISDICTION_ID`].
pub const SEED_ENTITY_TYPE_ID: Uuid = Uuid::from_u128(0x0199_0000_0000_7000_8000_0000_0000_0076);

/// Create a minimal entity and return its id. `project.entity_id` is
/// required, so every test that opens a matter needs a pre-existing
/// entity; this is the one-liner that supplies it.
///
/// Its Entity Type and Jurisdiction are shared synthetic rows it creates on
/// first use, so the PostgreSQL foreign keys hold. It never carries a firm-anchor key — a fixture that minted one
/// would make the *second* fixture in the same database fail on the
/// `entity_firm_anchor` index.
pub async fn seed_entity(application_store: &crate::application_store::ApplicationStore) -> Uuid {
    let (entity_type_id, jurisdiction_id) = seed_entity_parents(application_store).await;
    crate::entities::create(
        application_store,
        &crate::entities::NewEntity {
            name: format!("Test Entity {}", Uuid::now_v7()),
            entity_type_id,
            jurisdiction_id,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        },
    )
    .await
    .expect("seed entity")
    .id
}

/// An open synthetic Project and its owning Firm for PostgreSQL calendar and archive tests.
pub async fn seed_calendar_project() -> (uuid::Uuid, uuid::Uuid) {
    let pg = crate::postgres::PostgresDb::application().await.unwrap();
    sqlx::query_as::<_, (uuid::Uuid, uuid::Uuid)>(
        "WITH j AS (INSERT INTO jurisdiction (name) VALUES ('Calendar jurisdiction') RETURNING id),
              t AS (INSERT INTO entity_type (name) VALUES ('Calendar entity type') RETURNING id),
              e AS (INSERT INTO entity (name, entity_type_id, jurisdiction_id)
                    SELECT 'Calendar entity', t.id, j.id FROM t,j RETURNING id),
              f AS (INSERT INTO firm (name, slug, status, entity_id)
                    SELECT 'Calendar Firm', 'calendar-firm', 'active', id FROM e RETURNING id)
         INSERT INTO project (code, name, status, brand, entity_id, firm_id)
         SELECT 'calendar-test', 'Calendar test', 'open', 'neon', e.id, f.id FROM e,f RETURNING id, firm_id"
    ).fetch_one(pg.pool()).await.unwrap()
}

/// The shared synthetic Entity Type and Jurisdiction ids [`seed_entity`]
/// uses, created on first use.
pub async fn seed_entity_parents(
    application_store: &crate::application_store::ApplicationStore,
) -> (Uuid, Uuid) {
    let kind = crate::entity_types::find_or_create(application_store, "Synthetic fixture kind")
        .await
        .expect("seed entity type");
    let jurisdiction = match crate::jurisdictions::find_by_code(application_store, "ZZ")
        .await
        .expect("find fixture jurisdiction")
    {
        Some(found) => found,
        None => crate::jurisdictions::create(
            application_store,
            &crate::jurisdictions::NewJurisdiction::new("Synthetic fixture", "ZZ", "state"),
        )
        .await
        .expect("seed jurisdiction"),
    };
    (kind.id, jurisdiction.id)
}

/// Register a `brand` row for every `CLOSED_BRAND_KEYS` entry under one
/// synthetic Firm, the state a deployment reaches once the seed has run, so a
/// test that opens a Project or a social handle can name a Brand. Idempotent.
pub async fn seed_compiled_brands_postgres(db: &crate::application_store::ApplicationStore) {
    let first = crate::firms::CLOSED_BRAND_KEYS[0];
    if crate::brands::find_by_key(db, first)
        .await
        .expect("look up the fixture brand")
        .is_some()
    {
        return;
    }
    let unique = Uuid::new_v4();
    let admin = crate::persons::create(
        db,
        &crate::persons::NewPerson::with_role(
            "Synthetic brand administrator",
            format!("brands-{unique}@example.test"),
            crate::persons::Role::Admin,
        ),
    )
    .await
    .expect("fixture admin");
    let firm = crate::firms::create(
        db,
        &crate::firms::NewFirm {
            name: format!("Synthetic brand firm {unique}"),
            status: "active".into(),
            entity_id: seed_entity(db).await,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .expect("fixture firm");
    for key in crate::firms::CLOSED_BRAND_KEYS {
        crate::brands::create(
            db,
            crate::persons::Role::Admin,
            Some(admin.id),
            &crate::brands::NewBrand {
                name: (*key).to_string(),
                key: (*key).to_string(),
                firm_id: Some(firm.id),
                ..Default::default()
            },
        )
        .await
        .expect("fixture brand");
        crate::firms::attach_brand(db, crate::persons::Role::Owner, None, firm.id, key)
            .await
            .expect("attach fixture brand");
    }
}

/// Create a synthetic Entity and its lookup parents through the active accessors.
pub async fn seed_entity_with_parents(db: &crate::application_store::ApplicationStore) -> Uuid {
    let id = Uuid::new_v4();
    let jurisdiction = crate::jurisdictions::create(
        db,
        &crate::jurisdictions::NewJurisdiction::new(
            format!("Synthetic jurisdiction {id}"),
            id.to_string(),
            "state",
        ),
    )
    .await
    .unwrap();
    let kind = crate::entity_types::create(db, &format!("Synthetic kind {id}"))
        .await
        .unwrap();
    crate::entities::create(
        db,
        &crate::entities::NewEntity {
            name: format!("Synthetic entity {id}"),
            entity_type_id: kind.id,
            jurisdiction_id: jurisdiction.id,
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

/// Create a synthetic Firm, administrator, and owned Brand through canonical accessors.
pub async fn seed_firm_with_new_brand(
    db: &crate::application_store::ApplicationStore,
    brand: &str,
) -> (Uuid, Uuid) {
    let admin = crate::persons::create(
        db,
        &crate::persons::NewPerson::with_role(
            "Synthetic administrator",
            format!("synthetic-{}@example.com", Uuid::new_v4()),
            crate::persons::Role::Admin,
        ),
    )
    .await
    .unwrap();
    let firm = crate::firms::create(
        db,
        &crate::firms::NewFirm {
            name: format!("Synthetic firm {}", Uuid::new_v4()),
            status: "active".to_owned(),
            entity_id: seed_entity_with_parents(db).await,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .unwrap();
    crate::brands::create(
        db,
        crate::persons::Role::Admin,
        Some(admin.id),
        &crate::brands::NewBrand {
            name: brand.to_string(),
            key: brand.to_string(),
            firm_id: Some(firm.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    crate::firms::attach_brand(db, crate::persons::Role::Owner, None, firm.id, brand)
        .await
        .unwrap();
    (firm.id, admin.id)
}

/// Seed a synthetic Firm that wears the named brand so matter-open fixtures
/// exercise the same Firm/brand ownership invariant as production.
pub async fn seed_firm_with_brand(
    application_store: &crate::application_store::ApplicationStore,
    brand: &str,
) -> Uuid {
    let admin = crate::persons::create(
        application_store,
        &crate::persons::NewPerson::with_role(
            "Test Firm Admin",
            format!("test-firm-admin-{}@example.com", Uuid::now_v7()),
            crate::persons::Role::Admin,
        ),
    )
    .await
    .expect("seed Firm Admin");
    let firm = crate::firms::create(
        application_store,
        &crate::firms::NewFirm {
            name: format!("Test Firm {}", Uuid::now_v7()),
            status: "active".to_string(),
            entity_id: seed_entity(application_store).await,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .expect("seed Firm");
    crate::firms::attach_brand(
        application_store,
        crate::persons::Role::Owner,
        None,
        firm.id,
        brand,
    )
    .await
    .expect("attach Firm brand");
    firm.id
}

/// Rename the firm anchor aside and surrender its `firm_anchor_key`,
/// opening the "white-label window" in which the protected name exists in
/// configuration but no row carries it.
///
/// This deliberately goes around `store::entities::update`, which refuses
/// to rename a row carrying the key — that refusal is the invariant the
/// tests using this helper are there to prove, so a fixture that went
/// through the seam could not set the scene. It lives here rather than in
/// a test file so there is exactly one documented way around the guard.
///
/// It drops the `firm_anchor` claim as well as the column. Clearing only
/// the column would leave the claim standing, and the window it is
/// opening would be shut to every later mint.
///
/// # Panics
///
/// If the write fails, or the entity does not exist.
pub async fn release_firm_anchor(
    _application_store: &crate::application_store::ApplicationStore,
    entity_id: Uuid,
    aside_name: &str,
) {
    let pg = crate::postgres::PostgresDb::application().await.unwrap();
    let mut tx = pg.pool().begin().await.unwrap();
    sqlx::query("DELETE FROM firm_anchor WHERE entity_id = $1")
        .bind(entity_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE entity SET name = $2, firm_anchor_key = NULL WHERE id = $1")
        .bind(entity_id)
        .bind(aside_name)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

/// Create a minimal open matter in the projects cluster. Tests that need
/// a different lifecycle state can call
/// [`crate::projects::create`] directly.
///
/// # Panics
///
/// If seeding the entity or the project fails.
pub async fn seed_project(
    application_store: &crate::application_store::ApplicationStore,
    name: &str,
) -> crate::projects::Project {
    seed_project_row(application_store, name).await
}

/// Give a test Admin authority over the Project's Firm or its Brand's Firm.
///
/// # Panics
///
/// If the Project is missing or the Firm lookup or membership write fails.
pub async fn grant_project_firm_admin(
    application_store: &crate::application_store::ApplicationStore,
    project_id: Uuid,
    person_id: Uuid,
) {
    let project = crate::projects::find_by_id(application_store, project_id)
        .await
        .expect("read the seeded Project")
        .expect("the seeded Project exists");
    let firm_id = match project.firm_id {
        Some(firm_id) => Some(firm_id),
        None => crate::firms::firm_id_for_brand_key(application_store, &project.brand)
            .await
            .expect("read the Brand's Firm"),
    };
    if let Some(firm_id) = firm_id {
        crate::firms::ensure_membership(
            application_store,
            &crate::firms::NewPersonFirmRole {
                person_id,
                firm_id,
                membership: crate::firms::FirmMembership::Admin,
                is_dri: false,
            },
        )
        .await
        .expect("grant the test Admin's Firm membership");
    }
}

/// [`seed_project`] returning just the id — what most fixtures want.
///
/// # Panics
///
/// If seeding the entity or the project fails.
pub async fn seed_project_id(
    application_store: &crate::application_store::ApplicationStore,
    name: &str,
) -> Uuid {
    seed_project_row(application_store, name).await.id
}

/// [`seed_project`] attached to a firm — what the inbox/triage fixtures
/// need. `None` seeds an unattached matter.
///
/// # Panics
///
/// If seeding the entity or the project fails.
pub async fn seed_project_on_firm(
    application_store: &crate::application_store::ApplicationStore,
    firm_id: Option<Uuid>,
    name: &str,
) -> crate::projects::Project {
    seed_compiled_brands_postgres(application_store).await;
    let entity_id = seed_entity(application_store).await;
    let code_id = Uuid::now_v7();
    crate::projects::create(
        application_store,
        &crate::projects::NewProject {
            code: crate::projects::code_from_name(name, code_id),
            name: name.to_string(),
            status: "open".to_string(),
            entity_id,
            firm_id,
            ..Default::default()
        },
    )
    .await
    .expect("seed project on firm")
}

async fn seed_project_row(
    application_store: &crate::application_store::ApplicationStore,
    name: &str,
) -> crate::projects::Project {
    let entity_id = seed_entity(application_store).await;
    let code_id = Uuid::now_v7();
    crate::projects::create(
        application_store,
        &crate::projects::NewProject {
            code: crate::projects::code_from_name(name, code_id),
            name: name.to_string(),
            status: "open".to_string(),
            entity_id,
            ..Default::default()
        },
    )
    .await
    .expect("seed project")
}

/// Find-or-create a single throwaway person to stand as a project's
/// lawyer and client DRI in tests. Every matter needs a real `person` id
/// on each side; tests that don't exercise DRI semantics point both at
/// this one fixture row. Idempotent (keyed on a fixed email) so repeated
/// calls in one test return the same id instead of colliding on
/// `person_email_lower`.
///
/// Find-or-create the person `input` describes, keyed on its email.
///
/// A scenario that says "there is a client called Libra" wants the row to
/// exist, not to be *created*. The difference matters wherever one engine
/// outlives one test: the cucumber suites share a single process-wide
/// engine (dropping one per scenario panics inside the async runtime), so
/// the second scenario to blind-insert the same mailbox is refused by the
/// `person_email_lower` index — a collision that says nothing about the
/// behaviour under test.
///
/// [`crate::persons::find_or_create`] settles identity, including the
/// case where two scenarios race to create the same mailbox. On top of
/// that, this brings the name and role up to date when an existing row
/// differs, so a caller gets the person it asked for rather than whatever
/// an earlier scenario happened to seed. [`dri_person`] is the same
/// shape, fixed to one well-known fixture identity.
///
/// # Panics
///
/// If the lookup or the write fails.
pub async fn ensure_person(
    application_store: &crate::application_store::ApplicationStore,
    input: &crate::persons::NewPerson,
) -> crate::persons::Person {
    use crate::persons::{self, PersonEdit};

    let created = persons::find_by_email_ci(application_store, &input.email)
        .await
        .expect("person lookup")
        .is_none();
    let mut person = persons::find_or_create(application_store, input)
        .await
        .expect("seed the person");
    if created {
        return person;
    }

    if person.role != input.role {
        person = persons::set_role(application_store, person.id, input.role)
            .await
            .expect("align the seeded person's role")
            .expect("the seeded person is still there");
    }
    if person.name != input.name {
        person = persons::edit(
            application_store,
            person.id,
            &PersonEdit {
                name: Some(input.name.clone()),
                ..PersonEdit::default()
            },
        )
        .await
        .expect("align the seeded person's name")
        .expect("the seeded person is still there");
    }
    person
}

pub async fn dri_person(application_store: &crate::application_store::ApplicationStore) -> Uuid {
    use crate::persons::{self, NewPerson};

    const EMAIL: &str = "dri-fixture@test.invalid";
    // `find_or_create`, because the cucumber suites share one engine and
    // run their scenarios concurrently: look-then-create interleaves and
    // the loser is refused by the unique email index.
    persons::find_or_create(application_store, &NewPerson::new("DRI Fixture", EMAIL))
        .await
        .expect("seed dri fixture person")
        .id
}

/// Seed one notation (with its template, person, and project) and
/// return the notation id. Shared by the helper-module tests that need
/// a matter to hang rows off (`review_documents`, `document_comments`).
/// The template declares `kind: onboarding`. The code is a unique synthetic
/// id (`sitting__transcript-…`), not a catalog template — see [`seed_notation_with_kind`] for a
/// test that needs a different (or absent) declared kind.
///
/// See [`seed_project`] for the same shape.
pub async fn seed_notation(application_store: &crate::application_store::ApplicationStore) -> Uuid {
    seed_notation_fixture(application_store, Some("onboarding")).await
}

/// Same as [`seed_notation`], but the template's declared `kind:` is
/// `kind` instead of the default `onboarding` — `None` seeds a template
/// with no declared kind at all, for the "generate_pdf on a kindless
/// template errors" case.
pub async fn seed_notation_with_kind(
    application_store: &crate::application_store::ApplicationStore,
    kind: Option<&str>,
) -> Uuid {
    seed_notation_fixture(application_store, kind).await
}

async fn seed_notation_fixture(
    application_store: &crate::application_store::ApplicationStore,
    kind: Option<&str>,
) -> Uuid {
    use crate::persons::{self, NewPerson};
    use crate::projects::{designate_dri, DriSide, NewProject};

    seed_compiled_brands_postgres(application_store).await;
    let entity_id = seed_entity(application_store).await;

    // A fixed mailbox on an engine that outlives one test, so this seeds
    // the same way [`dri_person`] does — see [`ensure_person`].
    let person = persons::find_or_create(
        application_store,
        &NewPerson::new("Libra", "libra@example.com"),
    )
    .await
    .expect("seed person");
    let dri = dri_person(application_store).await;
    let proj = crate::projects::create(
        application_store,
        &NewProject {
            code: format!("libra-estate-{}", Uuid::now_v7()),
            name: "Libra estate plan".into(),
            status: "open".into(),
            entity_id,
            ..Default::default()
        },
    )
    .await
    .expect("seed project");
    designate_dri(application_store, proj.id, dri, DriSide::Lawyer)
        .await
        .expect("designate lawyer DRI");
    designate_dri(application_store, proj.id, dri, DriSide::Client)
        .await
        .expect("designate client DRI");
    seed_project_notation(application_store, proj.id, person.id, kind).await
}

async fn seed_project_notation(
    application_store: &crate::application_store::ApplicationStore,
    project_id: Uuid,
    person_id: Uuid,
    kind: Option<&str>,
) -> Uuid {
    let tmpl = crate::templates::save_version(
        application_store,
        None,
        &format!("sitting__transcript-{}", Uuid::now_v7()),
        crate::templates::Version {
            title: "Estate Plan".into(),
            respondent_type: "person".into(),
            asset_id: None,
            form_code: None,
            kind: kind.map(str::to_string),
            source_commit_sha: None,
        },
    )
    .await
    .expect("seed template")
    .into_model();
    crate::notations::create(
        application_store,
        &crate::notations::NewNotation::new(tmpl.id, person_id, project_id, "BEGIN"),
    )
    .await
    .expect("seed notation")
    .id
}

/// File a completed onboarding signature for an existing synthetic Project.
pub async fn seed_signed_onboarding(
    application_store: &crate::application_store::ApplicationStore,
    project_id: Uuid,
    person_id: Uuid,
) {
    let notation =
        seed_project_notation(application_store, project_id, person_id, Some("onboarding")).await;
    let provider = crate::signatures::SignatureProvider::DocuSign;
    let envelope_id = Uuid::now_v7().to_string();
    crate::signatures::record_request(application_store, notation, provider, &envelope_id)
        .await
        .expect("seed onboarding envelope");
    let stamped = crate::signatures::stamp_signed(
        application_store,
        provider,
        &envelope_id,
        &chrono::Utc::now().to_rfc3339(),
    )
    .await
    .expect("complete onboarding envelope");
    assert!(stamped, "fixture envelope must be stamped");
}

/// An in-memory object store whose writes can fail before storing, fail
/// after storing (an unknown provider outcome), or store damaged bytes, and
/// which supports create-only generations like GCS. For storage fault tests.
#[derive(Default)]
pub struct MemoryStorage {
    objects: std::sync::Mutex<std::collections::BTreeMap<String, (Vec<u8>, u64)>>,
    pub next_generation: std::sync::atomic::AtomicUsize,
    pub puts: std::sync::atomic::AtomicUsize,
    pub fail_before_write: std::sync::atomic::AtomicUsize,
    pub fail_after_write: std::sync::atomic::AtomicUsize,
    pub corrupt_after_write: std::sync::atomic::AtomicUsize,
}

impl MemoryStorage {
    pub fn keys(&self, prefix: &str) -> Vec<String> {
        self.objects
            .lock()
            .unwrap()
            .keys()
            .filter(|key| key.starts_with(prefix))
            .cloned()
            .collect()
    }

    pub fn corrupt(&self, key: &str) {
        let mut objects = self.objects.lock().unwrap();
        let entry = objects.get_mut(key).expect("object to corrupt");
        entry.0[0] ^= 0xff;
    }

    pub fn remove(&self, key: &str) {
        self.objects.lock().unwrap().remove(key);
    }

    fn take(counter: &std::sync::atomic::AtomicUsize) -> bool {
        counter
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |left| left.checked_sub(1),
            )
            .is_ok()
    }
}

#[async_trait::async_trait]
impl cloud::StorageService for MemoryStorage {
    async fn put(&self, key: &str, bytes: &[u8], _: &str) -> Result<(), cloud::StorageError> {
        self.puts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if Self::take(&self.fail_before_write) {
            return Err(cloud::StorageError::S3 {
                key: key.into(),
                message: "injected failure before write".into(),
            });
        }
        let generation = self
            .next_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst) as u64
            + 1;
        let mut stored = bytes.to_vec();
        if Self::take(&self.corrupt_after_write) {
            stored[0] ^= 0xff;
        }
        self.objects
            .lock()
            .unwrap()
            .insert(key.into(), (stored, generation));
        if Self::take(&self.fail_after_write) {
            return Err(cloud::StorageError::S3 {
                key: key.into(),
                message: "injected timeout after write".into(),
            });
        }
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<cloud::StoredObject, cloud::StorageError> {
        self.objects
            .lock()
            .unwrap()
            .get(key)
            .map(|(bytes, _)| cloud::StoredObject {
                key: key.into(),
                bytes: bytes.clone(),
                content_type: "application/octet-stream".into(),
            })
            .ok_or_else(|| cloud::StorageError::NotFound(key.into()))
    }

    async fn put_immutable(
        &self,
        key: &str,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<cloud::ImmutableObject, cloud::StorageError> {
        if self.objects.lock().unwrap().contains_key(key) {
            return Err(cloud::StorageError::S3 {
                key: key.into(),
                message: "exists".into(),
            });
        }
        self.put(key, bytes, content_type).await?;
        let generation = self.objects.lock().unwrap()[key].1;
        Ok(cloud::ImmutableObject {
            generation,
            size_bytes: bytes.len() as u64,
        })
    }

    async fn get_generation(
        &self,
        key: &str,
        generation: u64,
    ) -> Result<cloud::VersionedObject, cloud::StorageError> {
        let object = self.get(key).await?;
        let stored = self.objects.lock().unwrap()[key].1;
        if stored != generation {
            return Err(cloud::StorageError::NotFound(key.into()));
        }
        Ok(cloud::VersionedObject { object, generation })
    }

    async fn delete(&self, key: &str) -> Result<(), cloud::StorageError> {
        self.remove(key);
        Ok(())
    }

    async fn list(&self, prefix: &str) -> Result<Vec<cloud::ObjectListing>, cloud::StorageError> {
        Ok(self
            .objects
            .lock()
            .unwrap()
            .iter()
            .filter(|(key, _)| key.starts_with(prefix))
            .map(|(key, (bytes, _))| cloud::ObjectListing {
                key: key.clone(),
                size_bytes: bytes.len() as u64,
            })
            .collect())
    }

    async fn head(&self, key: &str) -> Result<Option<cloud::ObjectHead>, cloud::StorageError> {
        Ok(self
            .objects
            .lock()
            .unwrap()
            .get(key)
            .map(|(bytes, _)| cloud::ObjectHead {
                size_bytes: bytes.len() as u64,
            }))
    }

    async fn signed_url(
        &self,
        _: &str,
        _: std::time::Duration,
    ) -> Result<String, cloud::StorageError> {
        Err(cloud::StorageError::Unsupported("signed url"))
    }
}

/// Ingest one Fi row, retrying the instance-wide import lock's `Busy`.
///
/// `fi_import::lock_on` try-locks `google_fi:import:v1` — an advisory key
/// PostgreSQL scopes to the whole server, not the caller's database — and
/// returns `Busy` rather than queueing, so competing coordinators release
/// instead of exhausting the pool. Tests share one PostgreSQL instance with
/// concurrently scheduled processes (nextest runs every test as its own
/// process), including the fencing fixture that deliberately holds this key,
/// so a bare unwrap turns legal contention into a failure. Bounded retries
/// mirror the production contract — the API answers `Busy` with 503 and
/// "retry the unchanged batch", assignments answer "retry assignment" — and
/// leave every assertion exactly as strict.
pub async fn ingest_fi_message_on(
    pg: &crate::postgres::PostgresDb,
    scope: &crate::fi_import::Scope,
    row: &crate::fi_import::Row,
) -> Result<crate::inbox::IngestedInbound, crate::fi_import::FiImportError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        match crate::fi_import::ingest_on(pg, scope, row).await {
            Err(crate::fi_import::FiImportError::Busy) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            result => return result,
        }
    }
}

/// An exclusively owned, freshly created synthetic PostgreSQL database.
///
/// Default construction creates and initializes a genuine fresh-native Database
/// Origin through the production creation capability, without captured-history
/// receipts. `create_unactivated` leaves history absent for admission tests.
/// Construction never accepts an existing target database. Call `close`
/// after all consumers have drained; it drops only this resource's minted name.
pub struct PostgresFixture {
    admin: crate::postgres::PostgresDb,
    pg: crate::postgres::PostgresDb,
    database: String,
}

enum FixtureInitialization {
    CaptureTarget,
    SchemaOnly,
}

impl PostgresFixture {
    /// Create a unique database using explicit administrative connection options.
    ///
    /// # Errors
    /// Returns connection, creation, or canonical fixture initialization failures.
    pub async fn create(options: sqlx::postgres::PgConnectOptions) -> anyhow::Result<Self> {
        Self::create_with_connections(options.clone(), options).await
    }

    /// Create a fresh fixture with independently routed administration and data connections.
    ///
    /// Both options must address the same PostgreSQL server. Separate transports keep
    /// closing a fixture connection from interrupting its administrative cleanup.
    ///
    /// # Errors
    /// Returns connection, creation, or canonical fixture initialization failures.
    pub async fn create_with_connections(
        admin_options: sqlx::postgres::PgConnectOptions,
        fixture_options: sqlx::postgres::PgConnectOptions,
    ) -> anyhow::Result<Self> {
        let admin = crate::postgres::PostgresDb::connect(admin_options.clone())
            .await
            .context("connect synthetic fixture administration")?;
        let operation = Uuid::now_v7();
        let database = crate::postgres::native_origin::database_name(operation)?;
        let tier = crate::postgres::native_origin::TierBinding {
            tier_id: Uuid::new_v4(),
            namespace_uid: Uuid::new_v4(),
            cluster_name: "synthetic-native-fixture".into(),
        };
        let prepared = crate::postgres::native_origin::fixture::create(
            &admin,
            admin_options,
            fixture_options,
            operation,
            tier,
        )
        .await
        .with_context(|| format!("initialize owned synthetic fixture {database}"));
        match prepared {
            Ok(native) => Ok(Self {
                admin,
                pg: native.postgres().clone(),
                database,
            }),
            Err(error) => {
                admin.pool().close().await;
                Err(error)
            }
        }
    }

    /// Create a fresh application fixture without native object ownership, for
    /// exercising the real retained-source handover rather than bypassing it.
    ///
    /// # Errors
    /// Returns exclusive resource construction or application fixture failures.
    pub async fn create_capture_target(
        options: sqlx::postgres::PgConnectOptions,
    ) -> anyhow::Result<Self> {
        Self::create_owned(
            options.clone(),
            options,
            FixtureInitialization::CaptureTarget,
        )
        .await
    }

    /// Create an exclusively owned canonical schema without application enrollment.
    ///
    /// This fixture exercises admission refusal before any global, family, or
    /// native object history has been installed.
    ///
    /// # Errors
    /// Returns exclusive resource construction or canonical schema failures.
    pub async fn create_unactivated(
        options: sqlx::postgres::PgConnectOptions,
    ) -> anyhow::Result<Self> {
        Self::create_owned(options.clone(), options, FixtureInitialization::SchemaOnly).await
    }

    async fn create_owned(
        admin_options: sqlx::postgres::PgConnectOptions,
        fixture_options: sqlx::postgres::PgConnectOptions,
        initialization: FixtureInitialization,
    ) -> anyhow::Result<Self> {
        let admin = crate::postgres::PostgresDb::connect(admin_options)
            .await
            .context("connect synthetic fixture administration")?;
        let database = format!("native_test_{}", Uuid::now_v7().simple());
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
            .execute(admin.pool())
            .await
            .context("create owned synthetic fixture database")?;
        let prepared =
            prepare_postgres_fixture(fixture_options.database(&database), initialization)
                .await
                .with_context(|| format!("initialize owned synthetic fixture {database}"));
        match prepared {
            Ok(pg) => Ok(Self {
                admin,
                pg,
                database,
            }),
            Err(error) => {
                let cleanup = drop_postgres_fixture(&admin, &database).await;
                admin.pool().close().await;
                Err(fixture_setup_failure(error, cleanup))
            }
        }
    }

    /// The fixture's native PostgreSQL connection.
    #[must_use]
    pub const fn postgres(&self) -> &crate::postgres::PostgresDb {
        &self.pg
    }

    /// The generated database name, for a child process sharing this fixture.
    #[must_use]
    pub fn database(&self) -> &str {
        &self.database
    }

    /// Drain the fixture's pool and delete its exclusively owned database.
    ///
    /// Consuming the resource prevents a second cleanup through this handle.
    ///
    /// # Errors
    /// Returns a drop failure when another consumer has not drained.
    pub async fn close(self) -> anyhow::Result<()> {
        self.pg.pool().close().await;
        let dropped = drop_postgres_fixture(&self.admin, &self.database).await;
        self.admin.pool().close().await;
        dropped
    }
}

/// Cleanup must not replace the failure which caused initialization to unwind.
fn fixture_setup_failure(error: anyhow::Error, cleanup: anyhow::Result<()>) -> anyhow::Error {
    match cleanup {
        Ok(()) => error,
        Err(cleanup) => error.context(format!("owned fixture cleanup also failed: {cleanup:#}")),
    }
}

async fn drop_postgres_fixture(
    admin: &crate::postgres::PostgresDb,
    database: &str,
) -> anyhow::Result<()> {
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE {database}")))
        .execute(admin.pool())
        .await
        .context("drop owned synthetic fixture database")?;
    Ok(())
}

async fn prepare_postgres_fixture(
    options: sqlx::postgres::PgConnectOptions,
    initialization: FixtureInitialization,
) -> anyhow::Result<crate::postgres::PostgresDb> {
    let pg = crate::postgres::PostgresDb::connect(options)
        .await
        .context("connect newly created synthetic fixture")?;
    let initialized = async {
        sqlx::raw_sql("CREATE EXTENSION IF NOT EXISTS age; LOAD 'age';")
            .execute(pg.pool()).await.context("initialize synthetic fixture AGE extension")?;
        sqlx::raw_sql(crate::postgres_schema::canonical_schema())
            .execute(pg.pool()).await.context("apply synthetic fixture canonical schema")?;
        if matches!(initialization, FixtureInitialization::SchemaOnly) {
            return anyhow::Ok(());
        }
        sqlx::query("INSERT INTO mirror_backfill_run(id,batch_size,status,scope,completed_at) VALUES ($1,1,'completed','global',now())")
            .bind(Uuid::new_v4()).execute(pg.pool()).await
            .context("initialize fresh synthetic fixture global history")?;
        for family in crate::postgres::history::Family::all() {
            seed_postgres_family(&pg, *family).await;
        }
        anyhow::Ok(())
    }.await;
    if let Err(error) = initialized {
        pg.pool().close().await;
        return Err(error);
    }
    Ok(pg)
}

/// The port of the disposable PostgreSQL test server.
///
/// Every PostgreSQL-backed test requires it; none skips without it.
/// `docs/test-database.md` starts the server the way CI does.
///
/// # Panics
/// Panics when `NAVIGATOR_TEST_POSTGRES_PORT` is unset or not a port number.
#[must_use]
pub fn postgres_test_port() -> u16 {
    let port = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT").unwrap_or_else(|_| {
        panic!(
            "NAVIGATOR_TEST_POSTGRES_PORT is unset: PostgreSQL-backed tests require the \
             disposable test server; start it as docs/test-database.md describes"
        )
    });
    port.parse().unwrap_or_else(|_| {
        panic!("NAVIGATOR_TEST_POSTGRES_PORT={port:?} is not a port; see docs/test-database.md")
    })
}

/// Each actor walk uses a genuine fresh-native Origin, with no Source handle.
pub fn with_postgres(
    test: impl std::future::Future<Output = ()>,
) -> impl std::future::Future<Output = ()> {
    with_postgres_history(test, FixtureHistory::Native)
}

/// Run a legacy store behavior test with PostgreSQL as its sole database
/// authority and expose both handles for native fixture setup.
pub async fn with_native_application_fixture<F, Fut>(test: F)
where
    F: FnOnce(crate::application_store::ApplicationStore, crate::postgres::PostgresDb) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    with_postgres(async move {
        let pg = crate::postgres::PostgresDb::application()
            .await
            .expect("required PostgreSQL application fixture");
        let fixture_pg = pg.clone();
        let db = crate::application_store::ApplicationStore::from(pg.clone());
        seed_compiled_brands_postgres(&db).await;
        crate::postgres::PostgresDb::with_application_db(pg, test(db, fixture_pg))
            .await
            .expect("native PostgreSQL application fixture is admitted");
    })
    .await;
}

/// [`with_postgres`] under the name `hudson ops surreal-free-tests` reads: an
/// integration test that calls it keeps its file in the coverage measurement.
///
/// # Panics
/// Panics when the local test server is unspecified or the fixture fails.
pub fn with_required_postgres(
    test: impl std::future::Future<Output = ()>,
) -> impl std::future::Future<Output = ()> {
    with_postgres(test)
}

/// Exercise captured-enrollment validation using explicit synthetic history.
/// No Database Origin or native object ownership is installed. The same owned
/// scope, panic cleanup, and required-PostgreSQL rules apply as [`with_postgres`].
pub fn with_captured_postgres(
    test: impl std::future::Future<Output = ()>,
) -> impl std::future::Future<Output = ()> {
    with_postgres_history(test, FixtureHistory::Captured)
}

/// Run an owned, genuinely migrated schema-only target for historical input
/// parser/transaction tests. No capture enrollment or Database Origin is minted.
pub fn with_unactivated_postgres(
    test: impl std::future::Future<Output = ()>,
) -> impl std::future::Future<Output = ()> {
    with_postgres_history(test, FixtureHistory::SchemaOnly)
}

enum FixtureHistory {
    SchemaOnly,
    Native,
    Captured,
}

fn with_postgres_history(
    test: impl std::future::Future<Output = ()>,
    history: FixtureHistory,
) -> impl std::future::Future<Output = ()> {
    let test = Box::pin(test);
    async move {
        use crate::postgres::PostgresDb;
        use futures::FutureExt as _;
        use sqlx::postgres::{PgConnectOptions, PgSslMode};
        let port = postgres_test_port();
        let database =
            std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into());
        let options = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port)
            .username("postgres")
            .password("synthetic")
            .database(&database)
            .ssl_mode(PgSslMode::Disable)
            .options([("session_preload_libraries", "age")]);
        let fixture = match history {
            FixtureHistory::SchemaOnly => PostgresFixture::create_unactivated(options).await,
            FixtureHistory::Native => PostgresFixture::create(options).await,
            FixtureHistory::Captured => PostgresFixture::create_capture_target(options).await,
        }
        .unwrap();
        let result = std::panic::AssertUnwindSafe(PostgresDb::with_test_db(
            fixture.postgres().clone(),
            test,
        ))
        .catch_unwind()
        .await;
        fixture.close().await.unwrap();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}

#[cfg(test)]
mod postgres_tests;

/// Produce a genuine future upgrade intent stopped after its fixed SQLx migration
/// commits, before completion. The fixture owns an actual newly created database;
/// current-release admission must refuse this future metadata during transport tests.
///
/// # Errors
/// Invalid origin, failed migration, missing barrier evidence or interrupted cleanup.
pub async fn native_upgrade_pending(
    native: &crate::postgres::native_origin::NativeDatabase,
) -> Result<Uuid, crate::postgres::native_origin::OriginError> {
    crate::postgres::native_origin::fixture_upgrade::pending(native).await
}

/// Complete only the same fixed future transport operation. This does not admit
/// the future schema under the current release or fabricate a backfill receipt.
///
/// # Errors
/// The operation, origin, intent or applied checksum prefix does not match.
pub async fn native_upgrade_complete(
    native: &crate::postgres::native_origin::NativeDatabase,
    operation: Uuid,
) -> Result<(), crate::postgres::native_origin::OriginError> {
    crate::postgres::native_origin::fixture_upgrade::complete(native, operation).await
}
