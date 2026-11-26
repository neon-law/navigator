use super::*;
use crate::persons::{NewPerson, Role};
use crate::projects::{self, NewProject};
use crate::schema::apply;
use crate::surreal::test_support::unmigrated;
use crate::test_support::{mem_surreal, seed_entity};

async fn practice(db: &SurrealDb, name: &str) -> Firm {
    practice_with_admin(db, name, admin_dri_person(db).await).await
}

/// A fresh `person.role = admin` person, fit to name as a Firm's
/// `admin_dri_person_id`. Its own identity is opaque to callers that do
/// not need to name it — only [`practice`] uses this directly.
async fn admin_dri_person(db: &SurrealDb) -> Uuid {
    crate::persons::create(
        db,
        &NewPerson::with_role(
            "Admin DRI",
            format!("admin-dri-{}@example.com", Uuid::now_v7()),
            Role::Admin,
        ),
    )
    .await
    .unwrap()
    .id
}

async fn practice_with_admin(db: &SurrealDb, name: &str, admin_dri_person_id: Uuid) -> Firm {
    let entity_id = seed_entity(db).await;
    create(
        db,
        &NewFirm {
            name: name.to_string(),
            status: "active".to_string(),
            entity_id,
            admin_dri_person_id,
        },
    )
    .await
    .unwrap()
}

/// `user_message` names the refused rule rather than restating the debug
/// form — the two refusals ENG-585's create form must show by name.
#[test]
fn user_message_names_the_rule_for_the_create_form_s_two_refusals() {
    let person_id = Uuid::now_v7();
    assert_eq!(
        FirmError::IneligibleAdminDriTier(person_id).user_message(),
        "The Admin DRI must be a person whose role is admin or owner."
    );
    assert_eq!(
        FirmError::NoSuchEntity(person_id).user_message(),
        "Pick an existing entity to open the firm against."
    );
    assert_eq!(
        FirmError::NotAuthorized.user_message(),
        "You may not make this change."
    );
}
/// ENG-499: creation is atomic with the first Admin DRI. There is no
/// setup state — the moment the Firm row exists, so does exactly one
/// `person_firm_role` row carrying `membership = admin, is_dri = true`.
/// Creation refuses every ineligible tier: Lawyer, Clerk, and Client all
/// fail the same way a dangling person id would.
/// An Owner may hold the Admin DRI designation: opened as a Firm's first
/// DRI, or appointed onto a Firm they hold no membership on — which grants
/// the `admin` membership in the same transaction. Either way the
/// invariant report finds nothing to flag.
#[tokio::test]
async fn an_owner_may_be_the_admin_dri() {
    let db = mem_surreal().await;
    let owner = crate::persons::create(
        &db,
        &NewPerson::with_role("Owner DRI", "owner-dri@example.com", Role::Owner),
    )
    .await
    .unwrap();

    let opened = practice_with_admin(&db, "Owner-Opened Practice", owner.id).await;
    let row = membership_for_person(&db, owner.id, opened.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.membership, FirmMembership::Admin);
    assert!(row.is_dri);

    let firm = practice(&db, "Owner-Appointed Practice").await;
    assert!(membership_for_person(&db, owner.id, firm.id)
        .await
        .unwrap()
        .is_none());
    let appointed = appoint_admin_dri(&db, Role::Owner, Some(owner.id), firm.id, owner.id)
        .await
        .unwrap();
    assert_eq!(appointed.membership, FirmMembership::Admin);
    assert!(appointed.is_dri);
    let dris: Vec<Uuid> = memberships_for_firm(&db, firm.id)
        .await
        .unwrap()
        .into_iter()
        .filter(|row| row.is_dri)
        .map(|row| row.person_id)
        .collect();
    assert_eq!(dris, vec![owner.id], "the outgoing Admin DRI is cleared");

    let report = admin_dri_invariant_report(&db).await.unwrap();
    for firm_id in [opened.id, firm.id] {
        let status = report.iter().find(|s| s.firm_id == firm_id).unwrap();
        assert_eq!(status.problem, None, "{status:?}");
    }
}

/// Owner appoints and transfers the designation; every other tier,
/// including the Firm's own Admin, is refused. The transfer clears the
/// outgoing DRI and sets the incoming one atomically.
#[derive(Clone)]
struct TelemetryBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
impl std::io::Write for TelemetryBuf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for TelemetryBuf {
    type Writer = TelemetryBuf;
    fn make_writer(&'a self) -> TelemetryBuf {
        self.clone()
    }
}

/// `appoint_admin_dri` emits its own outcome event — distinct from
/// `firm_capability.resolve`'s own, which the same call also fires —
/// naming the Firm, the appointee, the actor, and a stable reason code
/// for both success and every typed refusal (ENG-464). Never carries an
/// email or name.
#[tokio::test]
async fn appoint_admin_dri_emits_a_reasoned_outcome_event_for_success_and_every_refusal() {
    crate::test_tracing::ensure_callsite_interest();
    let db = mem_surreal().await;
    let first_admin = admin_dri_person(&db).await;
    let firm = practice_with_admin(&db, "Telemetry Transfer Practice", first_admin).await;
    let second_admin = crate::persons::create(
        &db,
        &NewPerson::with_role(
            "Telemetry Second Admin",
            "telemetry-second-admin@example.com",
            Role::Admin,
        ),
    )
    .await
    .unwrap();
    add_membership(
        &db,
        &NewPersonFirmRole {
            person_id: second_admin.id,
            firm_id: firm.id,
            membership: FirmMembership::Admin,
            is_dri: false,
        },
    )
    .await
    .unwrap();
    let lawyer = crate::persons::create(
        &db,
        &NewPerson::with_role(
            "Telemetry DRI Lawyer",
            "telemetry-dri-lawyer@example.com",
            Role::Lawyer,
        ),
    )
    .await
    .unwrap();

    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::INFO)
        .with_writer(TelemetryBuf(buf.clone()))
        .finish();
    {
        let _guard = tracing::subscriber::set_default(subscriber);
        // Not authorized: an Admin, not Owner.
        appoint_admin_dri(
            &db,
            Role::Admin,
            Some(first_admin),
            firm.id,
            second_admin.id,
        )
        .await
        .unwrap_err();
        // Ineligible tier: a Lawyer named as the proposed DRI.
        appoint_admin_dri(&db, Role::Owner, None, firm.id, lawyer.id)
            .await
            .unwrap_err();
        // Success.
        appoint_admin_dri(&db, Role::Owner, None, firm.id, second_admin.id)
            .await
            .unwrap();
    }

    let logged = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    let events: Vec<&str> = logged
        .lines()
        .filter(|line| line.contains("admin DRI appointment/transfer"))
        .collect();
    assert_eq!(events.len(), 3, "one event per attempt: {logged}");

    assert!(
        events.iter().any(
            |line| line.contains(&format!("\"firm_id\":\"{}\"", firm.id))
                && line.contains("\"reason\":\"not_authorized\"")
                && line.contains("\"succeeded\":false")
        ),
        "{logged}"
    );
    assert!(
        events
            .iter()
            .any(|line| line.contains("\"reason\":\"ineligible_tier\"")
                && line.contains(&format!("\"person_id\":\"{}\"", lawyer.id))),
        "{logged}"
    );
    assert!(
        events
            .iter()
            .any(|line| line.contains("\"reason\":\"appointed\"")
                && line.contains("\"succeeded\":true")
                && line.contains(&format!("\"person_id\":\"{}\"", second_admin.id))),
        "{logged}"
    );

    assert!(
        !logged.contains("@example.com"),
        "no email may reach telemetry: {logged}"
    );
    assert!(
        !logged.contains("Telemetry Second Admin") && !logged.contains("Telemetry DRI Lawyer"),
        "no name may reach telemetry: {logged}"
    );
}

/// Appointment refuses ineligible tiers, a person with no membership on
/// this Firm at all (cross-Firm), and a person whose membership on this
/// Firm is not `admin` — three distinct typed refusals.
/// The guard used by membership-removal doors: removing or demoting the
/// current sole DRI on an active Firm is refused; anything else is a
/// no-op that never errors.
#[tokio::test]
async fn refuse_admin_dri_orphaning_only_blocks_the_active_firm_s_sole_dri() {
    let db = mem_surreal().await;
    let admin = admin_dri_person(&db).await;
    let firm = practice_with_admin(&db, "Guarded Practice", admin).await;

    let err = refuse_admin_dri_orphaning(&db, firm.id, admin, None)
        .await
        .unwrap_err();
    assert!(matches!(err, FirmError::WouldLeaveFirmWithoutAdminDri(id) if id == firm.id));
    let err = refuse_admin_dri_orphaning(&db, firm.id, admin, Some(FirmMembership::Lawyer))
        .await
        .unwrap_err();
    assert!(matches!(err, FirmError::WouldLeaveFirmWithoutAdminDri(id) if id == firm.id));

    // Reassigning to `admin` is not an orphan and is not refused.
    refuse_admin_dri_orphaning(&db, firm.id, admin, Some(FirmMembership::Admin))
        .await
        .unwrap();

    // A non-DRI row on the same Firm is never refused.
    let lawyer = crate::persons::create(
        &db,
        &NewPerson::with_role("Lawyer", "guard-lawyer@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    add_membership(
        &db,
        &NewPersonFirmRole {
            person_id: lawyer.id,
            firm_id: firm.id,
            membership: FirmMembership::Lawyer,
            is_dri: false,
        },
    )
    .await
    .unwrap();
    refuse_admin_dri_orphaning(&db, firm.id, lawyer.id, None)
        .await
        .unwrap();
}

/// The deployment-wide report names missing and multiple designations,
/// and never writes anything — the two invalid states a direct database
/// edit could still produce underneath the atomic doors above.
#[tokio::test]
async fn admin_dri_invariant_report_names_missing_and_multiple() {
    let db = mem_surreal().await;
    let admin = admin_dri_person(&db).await;
    let firm = practice_with_admin(&db, "Reported Practice", admin).await;

    // Multiple: a second Admin membership is force-marked `is_dri` by a
    // direct write underneath `appoint_admin_dri` — exactly the kind of
    // state this report exists to catch rather than silently repair.
    let second_admin = crate::persons::create(
        &db,
        &NewPerson::with_role("Second", "second-reported@example.com", Role::Admin),
    )
    .await
    .unwrap();
    add_membership(
        &db,
        &NewPersonFirmRole {
            person_id: second_admin.id,
            firm_id: firm.id,
            membership: FirmMembership::Admin,
            is_dri: true,
        },
    )
    .await
    .unwrap();
    let report = admin_dri_invariant_report(&db).await.unwrap();
    let status = report.iter().find(|s| s.firm_id == firm.id).unwrap();
    assert!(
        matches!(&status.problem, Some(AdminDriProblem::Multiple(ids)) if ids.len() == 2),
        "{status:?}"
    );

    // Missing: an empty Firm reports it, never guesses one.
    let empty_entity = seed_entity(&db).await;
    let empty_admin = admin_dri_person(&db).await;
    let empty_firm = create(
        &db,
        &NewFirm {
            name: "Empty Practice".to_string(),
            status: "active".to_string(),
            entity_id: empty_entity,
            admin_dri_person_id: empty_admin,
        },
    )
    .await
    .unwrap();
    // Clear the only DRI with a direct write — the same underneath-the-door
    // edit the report exists to catch.
    db.query("UPDATE person_firm_role SET is_dri = false WHERE firm_id = $firm_id")
        .bind(("firm_id", record_id(TABLE, empty_firm.id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    let report = admin_dri_invariant_report(&db).await.unwrap();
    let status = report.iter().find(|s| s.firm_id == empty_firm.id).unwrap();
    assert_eq!(status.problem, Some(AdminDriProblem::Missing));
}
#[tokio::test]
async fn add_membership_refuses_a_client_but_allows_every_firm_tier() {
    let db = mem_surreal().await;
    let firm = practice(&db, "Tier Practice").await;

    for (tag, role, membership) in [
        ("owner", Role::Owner, FirmMembership::Admin),
        ("admin", Role::Admin, FirmMembership::Admin),
        ("lawyer", Role::Lawyer, FirmMembership::Lawyer),
        ("clerk", Role::Clerk, FirmMembership::Clerk),
    ] {
        let person = crate::persons::create(
            &db,
            &NewPerson::with_role(format!("{tag} Person"), format!("{tag}@example.com"), role),
        )
        .await
        .unwrap();
        add_membership(
            &db,
            &NewPersonFirmRole {
                person_id: person.id,
                firm_id: firm.id,
                membership,
                is_dri: false,
            },
        )
        .await
        .unwrap_or_else(|error| panic!("{tag} should be allowed to join a firm: {error}"));
    }

    let client = crate::persons::create(
        &db,
        &NewPerson::with_role("Client Person", "client-tier@example.com", Role::Client),
    )
    .await
    .unwrap();
    let err = add_membership(
        &db,
        &NewPersonFirmRole {
            person_id: client.id,
            firm_id: firm.id,
            membership: FirmMembership::Lawyer,
            is_dri: false,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, FirmError::ClientCannotJoinFirm(id) if id == client.id));
    assert!(membership_for_person(&db, client.id, firm.id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn ensure_membership_refuses_a_client() {
    let db = mem_surreal().await;
    let firm = practice(&db, "Ensure Practice").await;
    let client = crate::persons::create(
        &db,
        &NewPerson::with_role("Client Person", "ensure-client@example.com", Role::Client),
    )
    .await
    .unwrap();
    let err = ensure_membership(
        &db,
        &NewPersonFirmRole {
            person_id: client.id,
            firm_id: firm.id,
            membership: FirmMembership::Clerk,
            is_dri: false,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, FirmError::ClientCannotJoinFirm(id) if id == client.id));
}

#[tokio::test]
async fn membership_refuses_a_dangling_person_or_firm() {
    let db = mem_surreal().await;
    let firm = practice(&db, "Practice Two").await;
    let missing_person = Uuid::now_v7();
    let err = add_membership(
        &db,
        &NewPersonFirmRole {
            person_id: missing_person,
            firm_id: firm.id,
            membership: FirmMembership::Clerk,
            is_dri: false,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, FirmError::NoSuchPerson(id) if id == missing_person));

    let person = crate::persons::create(
        &db,
        &NewPerson::with_role("Kim Clerk", "kim@example.com", Role::Clerk),
    )
    .await
    .unwrap();
    let missing_firm = Uuid::now_v7();
    let err = add_membership(
        &db,
        &NewPersonFirmRole {
            person_id: person.id,
            firm_id: missing_firm,
            membership: FirmMembership::Clerk,
            is_dri: false,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, FirmError::NoSuchFirm(id) if id == missing_firm));
}

#[tokio::test]
async fn project_firm_id_round_trips() {
    let db = mem_surreal().await;
    let firm = practice(&db, "Practice Three").await;
    let entity_id = seed_entity(&db).await;
    let created = projects::create(
        &db,
        &NewProject {
            code: "owned-matter".to_string(),
            name: "Owned Matter".to_string(),
            status: "open".to_string(),
            entity_id,
            firm_id: Some(firm.id),
            ..NewProject::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(created.firm_id, Some(firm.id));
    let reloaded = projects::find_by_id(&db, created.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reloaded.firm_id, Some(firm.id));
}

#[tokio::test]
async fn create_refuses_a_dangling_firm_id() {
    let db = mem_surreal().await;
    let entity_id = seed_entity(&db).await;
    let missing = Uuid::now_v7();
    let err = projects::create(
        &db,
        &NewProject {
            code: "orphan-matter".to_string(),
            name: "Orphan Matter".to_string(),
            status: "open".to_string(),
            entity_id,
            firm_id: Some(missing),
            ..NewProject::default()
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err,
        projects::ProjectStoreError::NoSuchFirm(id) if id == missing
    ));
}

/// Drop the definition, write the row, put the definition back — the
/// same historical-row shape `project.brand` uses. An absent `firm_id`
/// reads as `None` rather than failing deserialize.
#[tokio::test]
async fn reads_a_project_row_written_before_firm_id_was_defined() {
    let db = unmigrated().await;
    apply(&db).await.unwrap();
    db.query("REMOVE FIELD firm_id ON project").await.unwrap();
    let id = Uuid::now_v7();
    let entity_id = Uuid::now_v7();
    db.query(
        "CREATE $id SET code = 'pre-firm-matter', name = 'Pre-Firm Matter', \
         status = 'open', entity_id = $entity_id, brand = 'neon', \
         inserted_at = '2026-09-04T00:00:00Z', updated_at = '2026-09-04T00:00:00Z'",
    )
    .bind(("id", record_id("project", id)))
    .bind(("entity_id", record_id("entity", entity_id)))
    .await
    .unwrap()
    .check()
    .unwrap();
    db.query("DEFINE FIELD OVERWRITE firm_id ON project TYPE option<record<firm>>")
        .await
        .unwrap();

    let project = projects::find_by_id(&db, id).await.unwrap().unwrap();
    assert_eq!(project.firm_id, None);
}

#[tokio::test]
async fn create_requires_a_live_entity_and_refuses_a_second_firm_on_it() {
    let db = mem_surreal().await;
    let admin = admin_dri_person(&db).await;
    let missing = Uuid::now_v7();
    let err = create(
        &db,
        &NewFirm {
            name: "Ghost Practice".to_string(),
            status: "active".to_string(),
            entity_id: missing,
            admin_dri_person_id: admin,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, FirmError::NoSuchEntity(id) if id == missing));

    let entity_id = seed_entity(&db).await;
    create(
        &db,
        &NewFirm {
            name: "First".to_string(),
            status: "active".to_string(),
            entity_id,
            admin_dri_person_id: admin,
        },
    )
    .await
    .unwrap();
    let duplicate = create(
        &db,
        &NewFirm {
            name: "Second".to_string(),
            status: "active".to_string(),
            entity_id,
            admin_dri_person_id: admin,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(duplicate, FirmError::DuplicateEntity));
}

#[tokio::test]
async fn reads_a_firm_row_written_before_entity_id_was_defined() {
    let db = unmigrated().await;
    apply(&db).await.unwrap();
    db.query("REMOVE FIELD entity_id ON firm").await.unwrap();
    let id = Uuid::now_v7();
    db.query(
        "CREATE $id SET name = 'Pre-Entity Firm', slug = 'pre-entity-firm', status = 'active', \
         inserted_at = '2026-09-04T00:00:00Z', updated_at = '2026-09-04T00:00:00Z'",
    )
    .bind(("id", record_id(TABLE, id)))
    .await
    .unwrap()
    .check()
    .unwrap();
    db.query("DEFINE FIELD OVERWRITE entity_id ON firm TYPE option<record<entity>>")
        .await
        .unwrap();

    let firm = find_by_id(&db, id).await.unwrap().unwrap();
    assert_eq!(firm.name, "Pre-Entity Firm");
    assert_eq!(firm.entity_id, None);
}

#[tokio::test]
async fn moves_a_brand_between_firms_and_names_the_new_owner() {
    let db = mem_surreal().await;
    let from = practice(&db, "Moving Out").await;
    let to = practice(&db, "Moving In").await;
    attach_brand(&db, Role::Owner, None, from.id, "neon")
        .await
        .unwrap();

    move_brand(&db, Role::Owner, None, "neon", to.id)
        .await
        .unwrap();

    assert_eq!(
        firm_id_for_brand_key(&db, "neon").await.unwrap(),
        Some(to.id)
    );
    assert_eq!(
        brand_keys_for_firm(&db, to.id).await.unwrap(),
        vec!["neon".to_string()]
    );
    assert!(brand_keys_for_firm(&db, from.id).await.unwrap().is_empty());
    // The brand row's owning firm re-pointed with the seat, so the
    // live Firm→Entity resolution follows the move too.
    let brand = crate::brands::find_by_key(&db, "neon")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(brand.firm_id, Some(to.id));
}

/// A refused replacement seat must roll back the preceding deletion and
/// owner update, so a failed move leaves the complete old attribution.
#[tokio::test]
async fn a_failed_replacement_seat_rolls_back_the_whole_brand_move() {
    let db = mem_surreal().await;
    let from = practice(&db, "Rollback Source").await;
    let to = practice(&db, "Rollback Target").await;
    attach_brand(&db, Role::Owner, None, from.id, "neon")
        .await
        .unwrap();
    let before = crate::brands::find_by_key(&db, "neon")
        .await
        .unwrap()
        .unwrap();

    // Refuse only the final CREATE, after DELETE and UPDATE ran. The
    // existing seat remains readable under this write-time assertion.
    db.query(
        "DEFINE FIELD OVERWRITE brand_key ON firm_brand TYPE string \
         ASSERT $value != 'neon'",
    )
    .await
    .unwrap()
    .check()
    .unwrap();
    assert!(move_brand(&db, Role::Owner, None, "neon", to.id)
        .await
        .is_err());

    assert_eq!(
        firm_id_for_brand_key(&db, "neon").await.unwrap(),
        Some(from.id)
    );
    assert_eq!(
        brand_keys_for_firm(&db, from.id).await.unwrap(),
        vec!["neon".to_string()]
    );
    assert!(brand_keys_for_firm(&db, to.id).await.unwrap().is_empty());
    let after = crate::brands::find_by_key(&db, "neon")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.firm_id, before.firm_id);
    assert_eq!(after.updated_at, before.updated_at);
}

#[tokio::test]
async fn only_owner_moves_or_attaches() {
    let db = mem_surreal().await;
    let admin = admin_dri_person(&db).await;
    let firm = practice_with_admin(&db, "Admin DRI Practice", admin).await;
    let wearer = practice(&db, "Wearer").await;
    attach_brand(&db, Role::Owner, None, wearer.id, "neon")
        .await
        .unwrap();

    // An Admin-DRI may detach a brand but never repoint the seat: a
    // move changes whose legal entity the public copyright names.
    let refused = move_brand(&db, Role::Admin, Some(admin), "neon", firm.id)
        .await
        .unwrap_err();
    assert!(matches!(refused, FirmError::NotAuthorized));
    // An attach without an Owner behind it is the same hole, refused.
    let refused = attach_brand(&db, Role::Admin, Some(admin), firm.id, "delete-your-data")
        .await
        .unwrap_err();
    assert!(matches!(refused, FirmError::NotAuthorized));
    // A non-Owner role with no person behind it is refused outright.
    let refused = move_brand(&db, Role::Client, None, "neon", firm.id)
        .await
        .unwrap_err();
    assert!(matches!(refused, FirmError::NotAuthorized));
    // No refusal wrote anything: the seat still names the old firm.
    assert_eq!(
        firm_id_for_brand_key(&db, "neon").await.unwrap(),
        Some(wearer.id)
    );
}

#[tokio::test]
async fn move_noops_retry_and_refuses_unknowns_without_losing_the_seat() {
    let db = mem_surreal().await;
    let from = practice(&db, "Wearer").await;
    let to = practice(&db, "Target").await;
    attach_brand(&db, Role::Owner, None, from.id, "neon")
        .await
        .unwrap();

    // A move to the firm already wearing the brand is a no-op success.
    move_brand(&db, Role::Owner, None, "neon", from.id)
        .await
        .unwrap();
    assert_eq!(
        firm_id_for_brand_key(&db, "neon").await.unwrap(),
        Some(from.id)
    );

    // An unknown target firm is a typed error before any write runs —
    // the retried-or-failed move leaves the old association intact.
    let unknown_firm = move_brand(&db, Role::Owner, None, "neon", Uuid::now_v7())
        .await
        .unwrap_err();
    assert!(matches!(unknown_firm, FirmError::NoSuchFirm(_)));
    let unknown_brand = move_brand(&db, Role::Owner, None, "not-a-brand", to.id)
        .await
        .unwrap_err();
    assert!(matches!(unknown_brand, FirmError::UnknownBrand(key) if key == "not-a-brand"));
    assert_eq!(
        firm_id_for_brand_key(&db, "neon").await.unwrap(),
        Some(from.id)
    );

    move_brand(&db, Role::Owner, None, "neon", to.id)
        .await
        .unwrap();
    // Retrying the completed move is a no-op success, and the seat
    // still resolves to exactly one firm.
    move_brand(&db, Role::Owner, None, "neon", to.id)
        .await
        .unwrap();
    assert_eq!(
        firm_id_for_brand_key(&db, "neon").await.unwrap(),
        Some(to.id)
    );
}

#[tokio::test]
async fn attaches_closed_brand_keys_and_refuses_an_unknown_or_taken_key() {
    let db = mem_surreal().await;
    let firm = practice(&db, "Brand Holder").await;
    // `mem_surreal` already seeds every `CLOSED_BRAND_KEYS` entry as a
    // system-wide brand row, so "neon" exists before this test creates
    // anything.
    attach_brand(&db, crate::persons::Role::Owner, None, firm.id, "neon")
        .await
        .unwrap();
    assert_eq!(
        brand_keys_for_firm(&db, firm.id).await.unwrap(),
        vec!["neon".to_string()]
    );
    ensure_brand(&db, firm.id, "neon").await.unwrap();
    let unknown = attach_brand(
        &db,
        crate::persons::Role::Owner,
        None,
        firm.id,
        "not-a-brand",
    )
    .await
    .unwrap_err();
    assert!(matches!(unknown, FirmError::UnknownBrand(key) if key == "not-a-brand"));

    let other = practice(&db, "Other Practice").await;
    let taken = attach_brand(&db, crate::persons::Role::Owner, None, other.id, "neon")
        .await
        .unwrap_err();
    assert!(matches!(taken, FirmError::DuplicateBrand));

    detach_brand(&db, Role::Owner, None, firm.id, "neon")
        .await
        .unwrap();
    assert!(brand_keys_for_firm(&db, firm.id).await.unwrap().is_empty());
    // Detaching a key the firm never wore is a no-op, not an error.
    detach_brand(&db, Role::Owner, None, firm.id, "neon")
        .await
        .unwrap();
}

#[tokio::test]
async fn two_firms_each_list_only_their_own_brands() {
    let db = mem_surreal().await;
    // `mem_surreal` already seeds every `CLOSED_BRAND_KEYS` entry as a
    // system-wide brand row, so "neon", "delete-your-data", and
    // "guieb-shook" all exist before this test attaches any of them.
    let firm_a = practice(&db, "Practice A").await;
    let firm_b = practice(&db, "Practice B").await;
    attach_brand(&db, crate::persons::Role::Owner, None, firm_a.id, "neon")
        .await
        .unwrap();
    attach_brand(
        &db,
        crate::persons::Role::Owner,
        None,
        firm_a.id,
        "delete-your-data",
    )
    .await
    .unwrap();
    attach_brand(
        &db,
        crate::persons::Role::Owner,
        None,
        firm_b.id,
        "guieb-shook",
    )
    .await
    .unwrap();

    assert_eq!(
        firm_id_for_brand_key(&db, "neon").await.unwrap(),
        Some(firm_a.id)
    );
    assert_eq!(
        firm_id_for_brand_key(&db, "guieb-shook").await.unwrap(),
        Some(firm_b.id)
    );
    assert_eq!(
        brand_keys_for_firm(&db, firm_a.id).await.unwrap(),
        vec!["neon".to_string(), "delete-your-data".to_string()]
    );
    assert_eq!(
        brand_keys_for_firm(&db, firm_b.id).await.unwrap(),
        vec!["guieb-shook".to_string()]
    );
}

/// ENG-587: a Firm can wear a brand created at runtime through
/// `store::brands::create`, not just one of the nine compiled house
/// brands — `attach_brand` validates against live `brand` rows, not a
/// closed list, against the fully applied schema.
#[tokio::test]
async fn attach_brand_accepts_a_runtime_created_brand_key() {
    let db = mem_surreal().await;
    let admin = admin_dri_person(&db).await;
    let firm = practice_with_admin(&db, "Runtime Brand Wearer", admin).await;
    crate::brands::create(
        &db,
        Role::Admin,
        Some(admin),
        &crate::brands::NewBrand {
            name: "Runtime Brand".to_string(),
            key: "runtime-brand".to_string(),
            firm_id: Some(firm.id),
            ..crate::brands::NewBrand::default()
        },
    )
    .await
    .unwrap();

    attach_brand(
        &db,
        crate::persons::Role::Owner,
        None,
        firm.id,
        "runtime-brand",
    )
    .await
    .unwrap();

    assert_eq!(
        brand_keys_for_firm(&db, firm.id).await.unwrap(),
        vec!["runtime-brand".to_string()]
    );
    assert_eq!(
        firm_id_for_brand_key(&db, "runtime-brand").await.unwrap(),
        Some(firm.id)
    );
}

/// `attach_own_brand` reads the Firm from the `brand` row, admits only
/// Owner or that Firm's own Admin, and is idempotent.
#[tokio::test]
async fn attach_own_brand_wears_the_rows_own_firm_only() {
    let db = mem_surreal().await;
    let admin = admin_dri_person(&db).await;
    let firm = practice_with_admin(&db, "Own Brand Wearer", admin).await;
    let stranger = admin_dri_person(&db).await;
    practice_with_admin(&db, "Stranger Practice", stranger).await;
    crate::brands::create(
        &db,
        Role::Admin,
        Some(admin),
        &crate::brands::NewBrand {
            name: "Own Brand".to_string(),
            key: "own-brand".to_string(),
            firm_id: Some(firm.id),
            ..crate::brands::NewBrand::default()
        },
    )
    .await
    .unwrap();

    let refused = attach_own_brand(&db, Role::Admin, Some(stranger), "own-brand")
        .await
        .unwrap_err();
    assert!(matches!(refused, FirmError::NotAuthorized), "{refused:?}");
    assert!(brand_keys_for_firm(&db, firm.id).await.unwrap().is_empty());

    let attached = attach_own_brand(&db, Role::Admin, Some(admin), "own-brand")
        .await
        .unwrap();
    assert_eq!(attached, firm.id);
    attach_own_brand(&db, Role::Owner, None, "own-brand")
        .await
        .unwrap();
    assert_eq!(
        brand_keys_for_firm(&db, firm.id).await.unwrap(),
        vec!["own-brand".to_string()]
    );

    let unknown = attach_own_brand(&db, Role::Owner, None, "no-such-brand")
        .await
        .unwrap_err();
    assert!(matches!(unknown, FirmError::UnknownBrand(_)), "{unknown:?}");
}

/// ENG-494: `update` edits name/status/entity_id and leaves the Admin
/// DRI and every membership row untouched. `archived` is now an
/// admitted status (the schema `ASSERT` grew a third value alongside
/// `active`/`suspended`).
#[tokio::test]
async fn update_edits_firm_fields_and_leaves_membership_alone() {
    let db = mem_surreal().await;
    let admin = admin_dri_person(&db).await;
    let firm = practice_with_admin(&db, "Editable Practice", admin).await;
    let new_entity = seed_entity(&db).await;

    let edited = update(
        &db,
        Role::Owner,
        None,
        firm.id,
        &FirmEdit {
            name: Some("Renamed Practice".to_string()),
            status: Some("archived".to_string()),
            entity_id: Some(new_entity),
            portal_theme_brand_key: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(edited.name, "Renamed Practice");
    assert_eq!(edited.status, "archived");
    assert_eq!(edited.entity_id, Some(new_entity));

    let membership = membership_for_person(&db, admin, firm.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(membership.membership, FirmMembership::Admin);
    assert!(membership.is_dri, "editing the firm must not touch the DRI");

    // A partial edit touches only the named fields.
    let partial = update(
        &db,
        Role::Owner,
        None,
        firm.id,
        &FirmEdit {
            status: Some("active".to_string()),
            ..FirmEdit::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(partial.name, "Renamed Practice");
    assert_eq!(partial.status, "active");

    let missing = Uuid::now_v7();
    let err = update(&db, Role::Owner, None, missing, &FirmEdit::default())
        .await
        .unwrap_err();
    assert!(matches!(err, FirmError::NoSuchFirm(id) if id == missing));

    let missing_entity = Uuid::now_v7();
    let err = update(
        &db,
        Role::Owner,
        None,
        firm.id,
        &FirmEdit {
            entity_id: Some(missing_entity),
            ..FirmEdit::default()
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, FirmError::NoSuchEntity(id) if id == missing_entity));
}

fn theme(key: Option<&str>) -> FirmEdit {
    FirmEdit {
        portal_theme_brand_key: Some(key.map(str::to_string)),
        ..FirmEdit::default()
    }
}

/// The Portal Theme Brand: only Owner or the Firm's own Admin sets it,
/// only to a Brand the Firm owns, and `Some(None)` clears it.
#[tokio::test]
#[allow(clippy::too_many_lines)] // One fixture walks every refusal, then the set and clear.
async fn portal_theme_is_a_brand_the_firm_owns_set_by_its_admin() {
    let db = mem_surreal().await;
    let admin = admin_dri_person(&db).await;
    let firm = practice_with_admin(&db, "Themed Practice", admin).await;
    let other_admin = admin_dri_person(&db).await;
    let other = practice_with_admin(&db, "Other Practice", other_admin).await;
    for (owner, admin_id, key) in [
        (firm.id, admin, "themed-house"),
        (other.id, other_admin, "other-house"),
    ] {
        crate::brands::create(
            &db,
            Role::Admin,
            Some(admin_id),
            &crate::brands::NewBrand {
                name: format!("Brand {key}"),
                key: key.to_string(),
                firm_id: Some(owner),
                primary_color: None,
                accent_color: None,
                typeface: None,
            },
        )
        .await
        .unwrap();
    }
    let lawyer = crate::persons::create(
        &db,
        &NewPerson::with_role("Lawyer", "theme-lawyer@example.com", Role::Lawyer),
    )
    .await
    .unwrap()
    .id;
    add_membership(
        &db,
        &NewPersonFirmRole {
            person_id: lawyer,
            firm_id: firm.id,
            membership: FirmMembership::Lawyer,
            is_dri: false,
        },
    )
    .await
    .unwrap();

    // Neither a Lawyer of the Firm nor another Firm's Admin may set it.
    for (role, person) in [(Role::Lawyer, lawyer), (Role::Admin, other_admin)] {
        let refused = update(
            &db,
            role,
            Some(person),
            firm.id,
            &theme(Some("themed-house")),
        )
        .await
        .unwrap_err();
        assert!(matches!(refused, FirmError::NotAuthorized), "{refused:?}");
    }
    // A Brand another Firm owns, or none at all, is refused.
    for key in ["other-house", "no-such-brand"] {
        let refused = update(&db, Role::Admin, Some(admin), firm.id, &theme(Some(key)))
            .await
            .unwrap_err();
        assert!(
            matches!(&refused, FirmError::ThemeBrandNotOwned(id, k) if *id == firm.id && k == key),
            "{refused:?}"
        );
    }
    assert_eq!(
        find_by_id(&db, firm.id)
            .await
            .unwrap()
            .unwrap()
            .portal_theme_brand_key,
        None,
        "no refusal wrote anything"
    );

    let themed = update(
        &db,
        Role::Admin,
        Some(admin),
        firm.id,
        &theme(Some("themed-house")),
    )
    .await
    .unwrap();
    assert_eq!(
        themed.portal_theme_brand_key.as_deref(),
        Some("themed-house")
    );
    // An edit that does not name the theme leaves it alone.
    let renamed = update(
        &db,
        Role::Owner,
        None,
        firm.id,
        &FirmEdit {
            name: Some("Themed Practice PC".to_string()),
            ..FirmEdit::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        renamed.portal_theme_brand_key.as_deref(),
        Some("themed-house")
    );
    let cleared = update(&db, Role::Owner, None, firm.id, &theme(None))
        .await
        .unwrap();
    assert_eq!(cleared.portal_theme_brand_key, None);
}

/// `delete` refuses a Firm that still owns Projects, and otherwise
/// removes it along with its membership and brand rows.
#[tokio::test]
async fn delete_refuses_when_projects_reference_the_firm_else_cascades_memberships() {
    let db = mem_surreal().await;
    let admin = admin_dri_person(&db).await;
    let firm = practice_with_admin(&db, "Deletable Practice", admin).await;
    // `mem_surreal` already seeds every `CLOSED_BRAND_KEYS` entry as a
    // system-wide brand row, so "neon" exists before this test attaches it.
    attach_brand(&db, crate::persons::Role::Owner, None, firm.id, "neon")
        .await
        .unwrap();

    let entity_id = seed_entity(&db).await;
    let project = projects::create(
        &db,
        &NewProject {
            code: "owning-matter".to_string(),
            name: "Owning Matter".to_string(),
            status: "open".to_string(),
            entity_id,
            firm_id: Some(firm.id),
            ..NewProject::default()
        },
    )
    .await
    .unwrap();
    let err = delete(&db, Role::Owner, None, firm.id).await.unwrap_err();
    assert!(matches!(err, FirmError::FirmOwnsProjects(id) if id == firm.id));

    // Once the matter no longer exists, deletion proceeds and cascades
    // the membership and brand rows with it.
    projects::delete_project_with_surreal(&db, project.id)
        .await
        .unwrap();
    delete(&db, Role::Owner, None, firm.id).await.unwrap();
    assert!(find_by_id(&db, firm.id).await.unwrap().is_none());
    assert!(membership_for_person(&db, admin, firm.id)
        .await
        .unwrap()
        .is_none());
    assert!(brand_keys_for_firm(&db, firm.id).await.unwrap().is_empty());

    let missing = Uuid::now_v7();
    let err = delete(&db, Role::Owner, None, missing).await.unwrap_err();
    assert!(matches!(err, FirmError::NoSuchFirm(id) if id == missing));
}

/// `update_membership` changes only `membership`, never `is_dri`, and is
/// refused for the current sole DRI unless the new membership is still
/// `admin`.
/// `remove_membership` deletes the row and is refused for the current
/// sole DRI.
/// The authorization matrix ENG-494 asks for: Owner passes on every
/// Firm; an Admin who is a member of *this* Firm passes; an Admin who
/// is not (a member elsewhere or nowhere), and every Lawyer/Clerk,
/// is refused — for every write door this issue adds.
#[tokio::test]
async fn write_doors_admit_owner_and_the_firm_s_own_admin_only() {
    let db = mem_surreal().await;
    let admin_on_firm = admin_dri_person(&db).await;
    let firm = practice_with_admin(&db, "Matrix Practice", admin_on_firm).await;
    let other_firm = practice(&db, "Other Matrix Practice").await;

    let admin_off_firm = crate::persons::create(
        &db,
        &NewPerson::with_role("Admin Off Firm", "admin-off-firm@example.com", Role::Admin),
    )
    .await
    .unwrap();
    add_membership(
        &db,
        &NewPersonFirmRole {
            person_id: admin_off_firm.id,
            firm_id: other_firm.id,
            membership: FirmMembership::Admin,
            is_dri: false,
        },
    )
    .await
    .unwrap();
    let lawyer = crate::persons::create(
        &db,
        &NewPerson::with_role("Matrix Lawyer", "matrix-lawyer@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    add_membership(
        &db,
        &NewPersonFirmRole {
            person_id: lawyer.id,
            firm_id: firm.id,
            membership: FirmMembership::Lawyer,
            is_dri: false,
        },
    )
    .await
    .unwrap();
    let clerk = crate::persons::create(
        &db,
        &NewPerson::with_role("Matrix Clerk", "matrix-clerk@example.com", Role::Clerk),
    )
    .await
    .unwrap();
    add_membership(
        &db,
        &NewPersonFirmRole {
            person_id: clerk.id,
            firm_id: firm.id,
            membership: FirmMembership::Clerk,
            is_dri: false,
        },
    )
    .await
    .unwrap();

    // Every actor below asks the same question: may they change
    // `lawyer`'s membership on `firm`? Owner and the Firm's own Admin
    // may; an Admin scoped to a different Firm, a Lawyer, and a Clerk
    // may not.
    let allowed = [(Role::Owner, None), (Role::Admin, Some(admin_on_firm))];
    let forbidden = [
        (Role::Admin, Some(admin_off_firm.id)),
        (Role::Lawyer, Some(lawyer.id)),
        (Role::Clerk, Some(clerk.id)),
    ];

    for (role, person_id) in allowed {
        update_membership(
            &db,
            role,
            person_id,
            lawyer.id,
            firm.id,
            FirmMembership::Lawyer,
        )
        .await
        .unwrap_or_else(|error| panic!("{role:?} should be allowed: {error}"));
        update(&db, role, person_id, firm.id, &FirmEdit::default())
            .await
            .unwrap_or_else(|error| panic!("{role:?} should be allowed to edit: {error}"));
    }
    for (role, person_id) in forbidden {
        let err = update_membership(
            &db,
            role,
            person_id,
            lawyer.id,
            firm.id,
            FirmMembership::Lawyer,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, FirmError::NotAuthorized),
            "{role:?} must be refused: {err}"
        );
        let err = update(&db, role, person_id, firm.id, &FirmEdit::default())
            .await
            .unwrap_err();
        assert!(
            matches!(err, FirmError::NotAuthorized),
            "{role:?} must be refused to edit: {err}"
        );
    }
}

#[tokio::test]
async fn admin_visibility_stays_inside_the_admin_s_firms() {
    let db = mem_surreal().await;
    let admin_a = crate::persons::create(
        &db,
        &NewPerson::with_role("Admin A", "admin-a@example.com", Role::Admin),
    )
    .await
    .unwrap();
    let firm_a = practice_with_admin(&db, "Practice A", admin_a.id).await;
    let firm_b = practice(&db, "Practice B").await;
    let lawyer_a = crate::persons::create(
        &db,
        &NewPerson::with_role("Lawyer A", "lawyer-a@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    let lawyer_b = crate::persons::create(
        &db,
        &NewPerson::with_role("Lawyer B", "lawyer-b@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    add_membership(
        &db,
        &NewPersonFirmRole {
            person_id: lawyer_a.id,
            firm_id: firm_a.id,
            membership: FirmMembership::Lawyer,
            is_dri: false,
        },
    )
    .await
    .unwrap();
    add_membership(
        &db,
        &NewPersonFirmRole {
            person_id: lawyer_b.id,
            firm_id: firm_b.id,
            membership: FirmMembership::Lawyer,
            is_dri: false,
        },
    )
    .await
    .unwrap();

    let visible = visible_person_ids(&db, admin_a.id).await.unwrap();
    assert!(visible.contains(&admin_a.id));
    assert!(visible.contains(&lawyer_a.id));
    assert!(!visible.contains(&lawyer_b.id));
}

#[tokio::test]
async fn visible_person_ids_never_surfaces_a_client_through_a_firm_row() {
    let db = mem_surreal().await;
    let admin = crate::persons::create(
        &db,
        &NewPerson::with_role("Admin Only", "admin-only@example.com", Role::Admin),
    )
    .await
    .unwrap();
    let firm = practice_with_admin(&db, "No Client Members Practice", admin.id).await;

    let client = crate::persons::create(
        &db,
        &NewPerson::with_role(
            "Would Be Member",
            "would-be-member@example.com",
            Role::Client,
        ),
    )
    .await
    .unwrap();
    add_membership(
        &db,
        &NewPersonFirmRole {
            person_id: client.id,
            firm_id: firm.id,
            membership: FirmMembership::Lawyer,
            is_dri: false,
        },
    )
    .await
    .unwrap_err();

    let visible = visible_person_ids(&db, admin.id).await.unwrap();
    assert!(visible.contains(&admin.id));
    assert!(!visible.contains(&client.id));
}

/// The SurrealDB directory reads one keyset page at a time: a walk covers
/// every matching row once, forward and back, through tied names, and an
/// Admin's scope and Firm names apply to each page.
#[tokio::test]
async fn people_directory_pages_walk_every_row_once_on_surreal() {
    let db = mem_surreal().await;
    let mut created = Vec::new();
    for name in [
        "Row 1", "Row 2", "Row 2", "Row 2", "Row 3", "Row 4", "Row 5",
    ] {
        let email = format!("{}@page-walk.test", Uuid::now_v7());
        created.push(
            crate::persons::create(&db, &NewPerson::new(name, email))
                .await
                .unwrap(),
        );
    }
    let parse =
        |query: String| crate::listing::ListParams::parse(&query, &PEOPLE_DIRECTORY).unwrap();
    for sort in ["name", "-name", "email", "name,-email"] {
        let base = format!("filter[email]=page-walk&sort={sort}&page[size]=3");
        let (mut seen, mut sizes, mut query) = (Vec::new(), Vec::new(), base.clone());
        let last = loop {
            let page = people_directory(&db, &parse(query.clone()), DirectoryScope::Everyone)
                .await
                .unwrap();
            seen.extend(page.items.iter().map(|entry| entry.person.id));
            sizes.push(page.items.len());
            match page.next.clone() {
                Some(next) => query = format!("{base}&page[after]={next}"),
                None => break page,
            }
        };
        assert_eq!(sizes, vec![3, 3, 1], "{sort}");
        let mut unique = seen.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), created.len(), "{sort}");
        let prev = last.prev.expect("the last page links back");
        let back = people_directory(
            &db,
            &parse(format!("{base}&page[before]={prev}")),
            DirectoryScope::Everyone,
        )
        .await
        .unwrap();
        let back: Vec<_> = back.items.iter().map(|entry| entry.person.id).collect();
        assert_eq!(back, seen[3..6].to_vec(), "{sort}");
    }

    let admin = crate::persons::create(
        &db,
        &NewPerson::with_role("Page Admin", "admin@page-scope.test", Role::Admin),
    )
    .await
    .unwrap();
    let firm = practice_with_admin(&db, "Paging Practice", admin.id).await;
    let page = people_directory(
        &db,
        &parse("filter[email]=page-".into()),
        DirectoryScope::VisibleToAdmin(admin.id),
    )
    .await
    .unwrap();
    let rows: Vec<_> = page
        .items
        .iter()
        .map(|entry| (entry.person.id, entry.firm_names.clone()))
        .collect();
    assert_eq!(rows, vec![(admin.id, vec![firm.name])]);
}

#[tokio::test]
async fn backfill_points_unowned_projects_at_the_firm_once() {
    let db = mem_surreal().await;
    let firm = practice(&db, "Backfill Practice").await;
    let entity_id = seed_entity(&db).await;
    let project = projects::create(
        &db,
        &NewProject {
            code: "unowned-matter".to_string(),
            name: "Unowned Matter".to_string(),
            status: "open".to_string(),
            entity_id,
            ..NewProject::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(project.firm_id, None);
    assert_eq!(
        backfill_unowned_projects(&db, Some(firm.id)).await.unwrap(),
        1
    );
    assert_eq!(
        projects::find_by_id(&db, project.id)
            .await
            .unwrap()
            .unwrap()
            .firm_id,
        Some(firm.id)
    );
    assert_eq!(
        backfill_unowned_projects(&db, Some(firm.id)).await.unwrap(),
        0
    );
}

/// An Entity carrying `firm_anchor_key`, wrapped in a Firm — the shape
/// [`anchor_firm`] resolves. `name` is exactly [`crate::seed::FIRM_ENTITY_NAME`]
/// in these tests, because no test sets `NAVIGATOR_BOOTSTRAP_COMPANY` (a
/// process-wide environment variable every parallel test would race on),
/// so `anchor_firm` always falls back to that default.
async fn anchor_practice(db: &SurrealDb, name: &str) -> Firm {
    let entity_id = crate::entities::create(
        db,
        &crate::entities::NewEntity {
            name: name.to_string(),
            entity_type_id: crate::test_support::SEED_ENTITY_TYPE_ID,
            jurisdiction_id: crate::test_support::SEED_ENTITY_JURISDICTION_ID,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: Some(name.trim().to_lowercase()),
        },
    )
    .await
    .unwrap()
    .id;
    create(
        db,
        &NewFirm {
            name: name.to_string(),
            status: "active".to_string(),
            entity_id,
            admin_dri_person_id: admin_dri_person(db).await,
        },
    )
    .await
    .unwrap()
}

/// ENG-495: the default a newly created Lawyer or Clerk joins when no
/// other Firm is named — resolved through the two-hop
/// `firm_anchor_holder` → `find_by_entity_id` the issue asked to be
/// verified before anything is built on it.
#[tokio::test]
async fn anchor_firm_resolves_the_firm_wearing_the_bootstrap_entity() {
    let db = mem_surreal().await;
    let anchor = anchor_practice(&db, crate::seed::FIRM_ENTITY_NAME).await;

    assert_eq!(anchor_firm(&db).await.unwrap(), Some(anchor));
}

/// No Entity carries the anchor key at all — an ordinary Firm alone
/// resolves to no default, rather than an arbitrary one.
#[tokio::test]
async fn anchor_firm_is_none_when_no_entity_holds_the_anchor_key() {
    let db = mem_surreal().await;
    practice(&db, "Ordinary Practice").await;

    assert_eq!(anchor_firm(&db).await.unwrap(), None);
}

/// The two-firm case: a deployment holding several Firms still resolves
/// the one wearing the anchor Entity, not merely the first or the last
/// one created — this is the assertion that would pass vacuously against
/// a single seeded firm and is the whole point of ENG-495.
#[tokio::test]
async fn anchor_firm_picks_the_anchor_among_several_firms() {
    let db = mem_surreal().await;
    let _ordinary_first = practice(&db, "Ordinary Practice One").await;
    let anchor = anchor_practice(&db, crate::seed::FIRM_ENTITY_NAME).await;
    let _ordinary_second = practice(&db, "Ordinary Practice Two").await;

    assert_eq!(anchor_firm(&db).await.unwrap(), Some(anchor));
}

#[tokio::test]
async fn create_derives_the_slug_and_a_rename_never_moves_it() {
    let db = mem_surreal().await;
    let firm = practice(&db, "Example Låw, P.C.").await;
    assert_eq!(firm.slug, "example-law-p-c");

    let renamed = update(
        &db,
        Role::Owner,
        None,
        firm.id,
        &FirmEdit {
            name: Some("Entirely Different Practice".to_string()),
            ..FirmEdit::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(renamed.name, "Entirely Different Practice");
    assert_eq!(renamed.slug, "example-law-p-c");

    // Immutability is the engine's, not only the absence of a handler.
    let direct = db
        .query("UPDATE $id SET slug = 'moved-practice'")
        .bind(("id", record_id(TABLE, firm.id)))
        .await
        .unwrap()
        .check();
    assert!(direct.is_err(), "a direct slug write must be refused");
    let read = find_by_id(&db, firm.id).await.unwrap().unwrap();
    assert_eq!(read.slug, "example-law-p-c");
}

#[tokio::test]
async fn create_refuses_an_invalid_reserved_or_taken_slug() {
    let db = mem_surreal().await;
    let admin = admin_dri_person(&db).await;
    let attempt = |name: &'static str| {
        let db = db.clone();
        async move {
            let entity_id = seed_entity(&db).await;
            create(
                &db,
                &NewFirm {
                    name: name.to_string(),
                    status: "active".to_string(),
                    entity_id,
                    admin_dri_person_id: admin,
                },
            )
            .await
        }
    };
    assert!(matches!(
        attempt("Admin").await,
        Err(FirmError::InvalidSlug(slug)) if slug == "admin"
    ));
    assert!(matches!(
        attempt("!!!").await,
        Err(FirmError::InvalidSlug(slug)) if slug.is_empty()
    ));
    attempt("Example Law PC").await.unwrap();
    let duplicate = attempt("EXAMPLE  law -- pc").await.unwrap_err();
    assert!(
        matches!(&duplicate, FirmError::DuplicateSlug(slug) if slug == "example-law-pc"),
        "{duplicate:?}"
    );
    assert!(duplicate.user_message().contains("example-law-pc"));
}

/// A row written before `slug` existed gets the slug its name derives on
/// the next apply; a second apply changes nothing.
#[tokio::test]
async fn apply_backfills_a_historical_firm_and_then_holds_it() {
    let db = mem_surreal().await;
    // This fixture represents a database created before SurrealDB 4.
    db.query(
        "ALTER DATABASE COMPATIBILITY 3; REMOVE INDEX firm_slug ON firm; \
         REMOVE FIELD slug ON firm;",
    )
    .await
    .unwrap()
    .check()
    .unwrap();
    let (first, second) = (Uuid::now_v7(), Uuid::now_v7());
    for (id, name) in [(first, "Historical Practice"), (second, "Ünïcode Practice")] {
        db.query(
            "CREATE $id SET name = $name, status = 'active', \
             inserted_at = '2026-09-04T00:00:00Z', updated_at = '2026-09-04T00:00:00Z'",
        )
        .bind(("id", record_id(TABLE, id)))
        .bind(("name", name))
        .await
        .unwrap()
        .check()
        .unwrap();
    }

    apply(&db).await.unwrap();
    let slugs = |db: SurrealDb| async move {
        (
            find_by_id(&db, first).await.unwrap().unwrap().slug,
            find_by_id(&db, second).await.unwrap().unwrap().slug,
        )
    };
    assert_eq!(
        slugs(db.clone()).await,
        ("historical-practice".into(), "unicode-practice".into())
    );

    update(
        &db,
        Role::Owner,
        None,
        first,
        &FirmEdit {
            name: Some("Renamed Historical".to_string()),
            ..FirmEdit::default()
        },
    )
    .await
    .unwrap();
    apply(&db).await.unwrap();
    assert_eq!(
        slugs(db.clone()).await,
        ("historical-practice".into(), "unicode-practice".into())
    );
}

/// Two historical Firms whose names derive one slug stop the apply; no
/// suffix is invented and neither row is written.
#[tokio::test]
async fn apply_fails_loudly_on_a_slug_collision() {
    let db = mem_surreal().await;
    // This fixture represents a database created before SurrealDB 4.
    db.query(
        "ALTER DATABASE COMPATIBILITY 3; REMOVE INDEX firm_slug ON firm; \
         REMOVE FIELD slug ON firm;",
    )
    .await
    .unwrap()
    .check()
    .unwrap();
    for name in ["Example Law PC", "Example Law, PC"] {
        db.query(
            "CREATE $id SET name = $name, status = 'active', \
             inserted_at = '2026-09-04T00:00:00Z', updated_at = '2026-09-04T00:00:00Z'",
        )
        .bind(("id", record_id(TABLE, Uuid::now_v7())))
        .bind(("name", name))
        .await
        .unwrap()
        .check()
        .unwrap();
    }

    let error = apply(&db).await.unwrap_err();
    assert!(
        matches!(
            &error,
            crate::schema::SchemaError::FirmSlugBackfill(slug::BackfillRefusal::Collision { slug, .. })
                if slug == "example-law-pc"
        ),
        "{error:?}"
    );
    let mut response = db
        .query("SELECT VALUE id FROM firm WHERE slug IS NOT NONE")
        .await
        .unwrap();
    let written: Vec<surrealdb::types::RecordId> = response.take(0).unwrap();
    assert!(written.is_empty(), "a refused backfill writes nothing");
}

#[tokio::test]
async fn the_forgejo_repository_path_is_the_firm_slug_then_the_project_code() {
    let db = mem_surreal().await;
    let firm = practice(&db, "Example Law PC").await;
    let other = practice(&db, "Other Example Practice").await;
    let entity_id = seed_entity(&db).await;
    let owned = projects::create(
        &db,
        &NewProject {
            code: "sample-forge-matter".to_string(),
            name: "Sample Forge Matter".to_string(),
            status: "open".to_string(),
            entity_id,
            firm_id: Some(firm.id),
            ..NewProject::default()
        },
    )
    .await
    .unwrap();

    let path = forgejo::repository_path(&firm, &owned).unwrap();
    assert_eq!(path.owner, "example-law-pc");
    assert_eq!(path.repository, "sample-forge-matter");
    assert_eq!(path.to_string(), "example-law-pc/sample-forge-matter");

    assert_eq!(
        forgejo::repository_path(&other, &owned),
        Err(forgejo::RepositoryPathError::NotOwned {
            project_id: owned.id,
            firm_id: other.id,
        })
    );
    let unowned = projects::create(
        &db,
        &NewProject {
            code: "sample-unowned-matter".to_string(),
            name: "Sample Unowned Matter".to_string(),
            status: "open".to_string(),
            entity_id,
            ..NewProject::default()
        },
    )
    .await
    .unwrap();
    assert!(forgejo::repository_path(&firm, &unowned).is_err());

    let renamed = update(
        &db,
        Role::Owner,
        None,
        firm.id,
        &FirmEdit {
            name: Some("Renamed Example Practice".to_string()),
            ..FirmEdit::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(forgejo::repository_path(&renamed, &owned).unwrap(), path);
}

#[test]
fn consumer_gmail_is_not_a_google_workspace_mailbox() {
    for domain in ["gmail.com", "GMAIL.com", "googlemail.com"] {
        assert!(is_consumer_gmail(&format!("ada@{domain}")), "{domain}");
    }
    for domain in ["example.com", "gmail.example.com"] {
        assert!(!is_consumer_gmail(&format!("ada@{domain}")), "{domain}");
    }
    assert!(!is_consumer_gmail("gmail.com"));
}
