//! Shared test infrastructure for every workspace crate that exercises
//! the `store` schema.
//!
//! # The pattern
//!
//! Each test opens its own embedded, memory-backed SurrealDB inside the
//! test process — [`mem_surreal`] — and applies the `DEFINE` schema to it.
//! Nothing is shared between tests and nothing outlives the process, so
//! the whole suite runs in parallel with no cross-test collisions, no
//! container, and no cleanup. `cargo test` needs no configuration and no
//! Docker.
//!
//! The one exception is a test that spawns the `navigator` binary: a
//! subprocess cannot reach an in-process engine, so [`server_surreal`]
//! opens a *server-mode* database instead and hands back the coordinates
//! to point that process at. It skips when no endpoint is configured,
//! which `NAV_REQUIRE_SURREAL=1` turns into a failure so CI stays honest.
//!
//! The rest of this module is seeds: the small fixtures — an entity, a
//! matter, a person, a notation — that most tests need before they can
//! assert anything.

use uuid::Uuid;

/// A synthetic 40-hex commit sha for a project-scoped template version in a
/// test. `store::templates::save_version` refuses one without a commit.
pub const SYNTHETIC_COMMIT_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

/// An asset row holding `bytes`' digest, for a test that needs a template
/// body to pin but never reads the bytes back. Nothing is written to object
/// storage, so [`crate::templates::body`] on it fails; use
/// [`crate::assets::ingest_content`] when the test renders the body.
pub async fn seed_body_asset(surreal: &crate::surreal::SurrealDb, bytes: &[u8]) -> Uuid {
    let sha = crate::assets::sha256_hex(bytes);
    let id = Uuid::now_v7();
    surreal
        .query(
            "CREATE $id SET storage_key = $key, content_type = 'text/markdown', \
             byte_size = $size, sha256_hex = $sha, visibility = 'internal', metadata = NONE",
        )
        .bind(("id", crate::surreal::record_id(crate::assets::TABLE, id)))
        .bind(("key", format!("blobs/{sha}")))
        .bind(("size", i64::try_from(bytes.len()).unwrap_or(i64::MAX)))
        .bind(("sha", sha))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .expect("seed body asset");
    id
}

/// The current project-scoped template version for `code` with no body and
/// no commit — the shape of a row written before
/// [`crate::templates::save_version`] required both, which it now refuses
/// to write. For a test that covers how a body-less version reads.
pub async fn seed_unpinned_project_template(
    surreal: &crate::surreal::SurrealDb,
    project_id: Uuid,
    code: &str,
    title: &str,
    respondent_type: &str,
    kind: Option<&str>,
) -> crate::templates::Template {
    let id = Uuid::now_v7();
    surreal
        .query(
            "CREATE $id SET code = $code, title = $title, respondent_type = $respondent, \
             project_id = $project, is_current = true, kind = $kind, current_key = $key",
        )
        .bind(("id", crate::surreal::record_id(crate::templates::TABLE, id)))
        .bind(("code", code.to_string()))
        .bind(("title", title.to_string()))
        .bind(("respondent", respondent_type.to_string()))
        .bind((
            "project",
            crate::surreal::record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .bind(("kind", kind.map(str::to_string)))
        .bind(("key", crate::templates::current_key(Some(project_id), code)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .expect("seed unpinned project template");
    crate::templates::find_by_id(surreal, id)
        .await
        .expect("read unpinned project template")
        .expect("unpinned project template exists")
}

/// A private embedded SurrealDB with the `DEFINE` schema applied — one
/// engine per test, inside the test process.
pub async fn mem_surreal() -> crate::surreal::SurrealDb {
    crate::surreal::test_support::mem().await
}

/// A SurrealDB reachable from *another process*, plus the coordinates to
/// point that process at it — see [`server_surreal`].
pub struct ServerSurreal {
    /// The connected handle, on a database of the caller's own naming,
    /// with the `DEFINE` schema applied.
    pub db: crate::surreal::SurrealDb,
    /// The endpoint, namespace, database, and any credentials, ready to
    /// be handed to a subprocess as environment variables.
    pub config: crate::surreal::SurrealConfig,
}

/// Open a *server-mode* SurrealDB for a test that spawns a subprocess.
///
/// [`mem_surreal`] lives inside the test process, so a `navigator`
/// invocation cannot reach it. A test that runs the binary — and every
/// local-store CLI command touches the person directory, if only through
/// the canonical seed — needs a real server instead:
///
/// - **`NAVIGATOR_SURREAL_ENDPOINT` set** → connect, on `database` so two
///   tests on one engine cannot collide, and apply the schema.
/// - **unset** → return `None`, keeping `cargo test` zero-config on a
///   laptop with no dependency tier running.
///
/// `NAV_REQUIRE_SURREAL=1` turns that skip into a failure, which is what
/// keeps CI honest. A half-configured environment (an endpoint with no
/// namespace) always panics: that is a mistake, not an opt-out.
///
/// # Panics
///
/// When the environment is half-configured, when `NAV_REQUIRE_SURREAL=1`
/// and no endpoint is set, or when connecting or applying the schema
/// fails.
pub async fn server_surreal(database: &str) -> Option<ServerSurreal> {
    let config = match crate::surreal::SurrealConfig::from_env() {
        Ok(config) => crate::surreal::SurrealConfig {
            database: database.to_string(),
            ..config
        },
        Err(crate::surreal::SurrealConfigError::MissingEnv(name)) => {
            assert!(
                std::env::var("NAV_REQUIRE_SURREAL").as_deref() != Ok("1"),
                "NAV_REQUIRE_SURREAL=1 but {name} is unset: this test spawns `navigator`, \
                 which resolves the person directory in SurrealDB and cannot reach an \
                 in-process engine.",
            );
            eprintln!("skipping the server-mode SurrealDB lane: {name} is unset");
            return None;
        }
        Err(err) => panic!("the SurrealDB environment is half-configured: {err}"),
    };
    let db = crate::surreal::connect(&config)
        .await
        .expect("connect to the person store");
    crate::schema::apply(&db).await.expect("apply the schema");
    Some(ServerSurreal { db, config })
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
/// Its references are the
/// synthetic [`SEED_ENTITY_JURISDICTION_ID`] and [`SEED_ENTITY_TYPE_ID`],
/// and it never carries a firm-anchor key — a fixture that minted one
/// would make the *second* fixture in the same database fail on the
/// `entity_firm_anchor` index.
pub async fn seed_entity(surreal: &crate::surreal::SurrealDb) -> Uuid {
    crate::entities::create(
        surreal,
        &crate::entities::NewEntity {
            name: format!("Test Entity {}", Uuid::now_v7()),
            entity_type_id: SEED_ENTITY_TYPE_ID,
            jurisdiction_id: SEED_ENTITY_JURISDICTION_ID,
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

/// Seed a synthetic Firm that wears the named brand so matter-open fixtures
/// exercise the same Firm/brand ownership invariant as production.
pub async fn seed_firm_with_brand(surreal: &crate::surreal::SurrealDb, brand: &str) -> Uuid {
    let admin = crate::persons::create(
        surreal,
        &crate::persons::NewPerson::with_role(
            "Test Firm Admin",
            format!("test-firm-admin-{}@example.com", Uuid::now_v7()),
            crate::persons::Role::Admin,
        ),
    )
    .await
    .expect("seed Firm Admin");
    let firm = crate::firms::create(
        surreal,
        &crate::firms::NewFirm {
            name: format!("Test Firm {}", Uuid::now_v7()),
            status: "active".to_string(),
            entity_id: seed_entity(surreal).await,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .expect("seed Firm");
    crate::firms::attach_brand(surreal, crate::persons::Role::Owner, None, firm.id, brand)
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
    surreal: &crate::surreal::SurrealDb,
    entity_id: Uuid,
    aside_name: &str,
) {
    surreal
        .query(
            "BEGIN TRANSACTION; \
             DELETE firm_anchor WHERE entity_id = $id; \
             UPDATE $id SET name = $name, firm_anchor_key = NONE; \
             COMMIT TRANSACTION;",
        )
        .bind(("id", crate::surreal::record_id("entity", entity_id)))
        .bind(("name", aside_name.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .expect("move the seeded anchor aside");
}

/// Create a minimal open matter in the projects cluster. Tests that need
/// a different lifecycle state can call
/// [`crate::projects::create`] directly.
///
/// # Panics
///
/// If seeding the entity or the project fails.
pub async fn seed_project(
    surreal: &crate::surreal::SurrealDb,
    name: &str,
) -> crate::projects::Project {
    seed_project_row(surreal, name).await
}

/// [`seed_project`] returning just the id — what most fixtures want.
///
/// # Panics
///
/// If seeding the entity or the project fails.
pub async fn seed_project_surreal(surreal: &crate::surreal::SurrealDb, name: &str) -> Uuid {
    seed_project_row(surreal, name).await.id
}

async fn seed_project_row(
    surreal: &crate::surreal::SurrealDb,
    name: &str,
) -> crate::projects::Project {
    let entity_id = seed_entity(surreal).await;
    let code_id = Uuid::now_v7();
    crate::projects::create(
        surreal,
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
    surreal: &crate::surreal::SurrealDb,
    input: &crate::persons::NewPerson,
) -> crate::persons::Person {
    use crate::persons::{self, PersonEdit};

    let created = persons::find_by_email_ci(surreal, &input.email)
        .await
        .expect("person lookup")
        .is_none();
    let mut person = persons::find_or_create(surreal, input)
        .await
        .expect("seed the person");
    if created {
        return person;
    }

    if person.role != input.role {
        person = persons::set_role(surreal, person.id, input.role)
            .await
            .expect("align the seeded person's role")
            .expect("the seeded person is still there");
    }
    if person.name != input.name {
        person = persons::edit(
            surreal,
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

/// Write a `person_project_role` row carrying a participation word the
/// schema `ASSERT` (ENG-478) rejects, for a test proving a legacy row
/// survives the migration that closed the vocabulary.
///
/// This deliberately goes around [`crate::projects::add_participation`],
/// which cannot write such a word once the schema refuses it — that refusal
/// is the invariant ENG-478 exists to add. It loosens the field, writes the
/// row, then restores the real definition by re-running [`crate::schema::apply`]
/// rather than repeating the `ASSERT`'s word list here, so the two can never
/// quietly say something different from one another. That is exactly what a
/// live migration does to a row already on disk: `ASSERT` validates a write,
/// never a value already stored, so a row written before this line shipped
/// keeps reading — it is only ever reported, never rewritten.
///
/// # Panics
///
/// If loosening the field, writing the row, or restoring the schema fails.
pub async fn seed_legacy_participation(
    surreal: &crate::surreal::SurrealDb,
    project_id: Uuid,
    person_id: Uuid,
    participation: &str,
) {
    surreal
        .query("DEFINE FIELD OVERWRITE participation ON person_project_role TYPE string;")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .expect("loosen the participation field");
    let now = chrono::Utc::now().to_rfc3339();
    surreal
        .query(
            "CREATE $id SET person_id = $person_id, project_id = $project_id, \
             participation = $participation, inserted_at = $now, updated_at = $now",
        )
        .bind((
            "id",
            crate::surreal::record_id("person_project_role", Uuid::now_v7()),
        ))
        .bind(("person_id", crate::surreal::record_id("person", person_id)))
        .bind((
            "project_id",
            crate::surreal::record_id("project", project_id),
        ))
        .bind(("participation", participation.to_string()))
        .bind(("now", now))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .expect("write the legacy participation row");
    crate::schema::apply(surreal)
        .await
        .expect("restore the participation field's schema");
}

pub async fn dri_person(surreal: &crate::surreal::SurrealDb) -> Uuid {
    use crate::persons::{self, NewPerson};

    const EMAIL: &str = "dri-fixture@test.invalid";
    // `find_or_create`, because the cucumber suites share one engine and
    // run their scenarios concurrently: look-then-create interleaves and
    // the loser is refused by the unique email index.
    persons::find_or_create(surreal, &NewPerson::new("DRI Fixture", EMAIL))
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
pub async fn seed_notation(surreal: &crate::surreal::SurrealDb) -> Uuid {
    seed_notation_with_kind_surreal(surreal, Some("onboarding")).await
}

/// Same as [`seed_notation`], but the template's declared `kind:` is
/// `kind` instead of the default `onboarding` — `None` seeds a template
/// with no declared kind at all, for the "generate_pdf on a kindless
/// template errors" case.
pub async fn seed_notation_with_kind(
    surreal: &crate::surreal::SurrealDb,
    kind: Option<&str>,
) -> Uuid {
    seed_notation_with_kind_surreal(surreal, kind).await
}

async fn seed_notation_with_kind_surreal(
    surreal: &crate::surreal::SurrealDb,
    kind: Option<&str>,
) -> Uuid {
    use crate::persons::{self, NewPerson};
    use crate::projects::{designate_dri_in_surreal, DriSide, NewProject};

    let entity_id = seed_entity(surreal).await;

    let tmpl = crate::templates::save_version(
        surreal,
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
    // A fixed mailbox on an engine that outlives one test, so this seeds
    // the same way [`dri_person`] does — see [`ensure_person`].
    let person = persons::find_or_create(surreal, &NewPerson::new("Libra", "libra@example.com"))
        .await
        .expect("seed person");
    let dri = dri_person(surreal).await;
    let proj = crate::projects::create(
        surreal,
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
    designate_dri_in_surreal(surreal, proj.id, dri, DriSide::Lawyer)
        .await
        .expect("designate lawyer DRI");
    designate_dri_in_surreal(surreal, proj.id, dri, DriSide::Client)
        .await
        .expect("designate client DRI");
    crate::notations::create(
        surreal,
        &crate::notations::NewNotation::new(tmpl.id, person.id, proj.id, "BEGIN"),
    )
    .await
    .expect("seed notation")
    .id
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

/// Copy a Firm row and a client Entity under their original ids into an empty
/// target store: the only prerequisites a Project Recovery Archive restore
/// expects.
///
/// # Panics
/// If either write fails.
pub async fn seed_restore_prerequisites(
    target: &crate::surreal::SurrealDb,
    firm: &crate::firms::Firm,
    entity_id: Uuid,
) {
    target
        .query(
            "CREATE $id SET name = $name, status = $status, slug = $slug, \
             inserted_at = $at, updated_at = $at",
        )
        .bind(("id", crate::surreal::record_id("firm", firm.id)))
        .bind(("name", firm.name.clone()))
        .bind(("status", firm.status.clone()))
        .bind(("slug", firm.slug.clone()))
        .bind(("at", firm.inserted_at.clone()))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .expect("seed the Firm row");
    target
        .query(
            "CREATE $id SET name = 'Synthetic Client', entity_type_id = $type, \
             jurisdiction_id = $jurisdiction",
        )
        .bind(("id", crate::surreal::record_id("entity", entity_id)))
        .bind((
            "type",
            crate::surreal::record_id("entity_type", SEED_ENTITY_TYPE_ID),
        ))
        .bind((
            "jurisdiction",
            crate::surreal::record_id("jurisdiction", SEED_ENTITY_JURISDICTION_ID),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .expect("seed the client Entity row");
}
