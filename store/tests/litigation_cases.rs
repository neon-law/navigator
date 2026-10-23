//! The litigation module (#686): many cases per matter, the generic
//! docket spine, and structured discovery where structure is earned.
//!
//! Two properties drive most of these tests. `entry_number` is **text**,
//! because real dockets use attachment sub-numbers like `29-1`. And an
//! entry with no document is **source pending** — a meaningful state, not
//! an error — derived from the link rather than a hand-maintained flag.

use chrono::{DateTime, Utc};
use store::cases::{
    add_item, answer_item, current_appearances_for_project, docket, for_project, is_source_pending,
    items, open_case, record_entry, record_integrity_check, record_stamp, serve_discovery,
    undated_hearings_and_trials, Device, EntryKind, NewCase, NewDiscoveryRequest, NewDocketEntry,
};
use store::cm_ecf_stamp::CmEcfStamp;
use store::surreal::test_support::mem;
use store::surreal::SurrealDb;
use uuid::Uuid;

/// The litigation tables link to `project`, so a case needs a real matter
/// row to hang off.
async fn open_matter(surreal: &SurrealDb, code: &str) -> Uuid {
    store::test_support::seed_project_surreal(surreal, code).await
}

fn a_case(project_id: Uuid, caption: &'static str) -> NewCase<'static> {
    NewCase {
        project_id,
        caption,
        forum: Some("Eighth Judicial District Court"),
        jurisdiction: Some("Nevada"),
        docket_number: Some("A-26-000001-C"),
        judge: Some("Hon. Example Judge"),
        posture: "plaintiff",
    }
}

fn at(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .expect("fixture RFC 3339")
        .with_timezone(&Utc)
}

fn an_entry(
    case_id: Uuid,
    entry_number: &'static str,
    kind: EntryKind,
    title: &'static str,
) -> NewDocketEntry<'static> {
    NewDocketEntry {
        case_id,
        entry_number,
        kind,
        title,
        party: None,
        filed_or_served_on: None,
        scheduled_on: None,
        supersedes: None,
        document_asset_id: None,
        notation_id: None,
    }
}

/// The premise of the whole module: one engagement carries several cases.
#[tokio::test]
async fn a_matter_carries_many_cases() {
    let db = &mem().await;
    let project_id = open_matter(db, "many-cases").await;

    open_case(db, &a_case(project_id, "Alpha v. Beta"))
        .await
        .expect("first");
    open_case(db, &a_case(project_id, "Alpha v. Gamma"))
        .await
        .expect("second");

    let cases = for_project(db, project_id).await.expect("list");
    assert_eq!(cases.len(), 2);
    assert_eq!(cases[0].caption, "Alpha v. Beta");
    assert_eq!(cases[1].caption, "Alpha v. Gamma");
}

/// The case masthead renders caption, court, docket number, **and
/// judge** — the column the original schema had no home for.
#[tokio::test]
async fn a_case_records_its_presiding_judge() {
    let db = &mem().await;
    let project_id = open_matter(db, "masthead").await;

    let c = open_case(db, &a_case(project_id, "Alpha v. Beta"))
        .await
        .expect("case");
    assert_eq!(c.judge.as_deref(), Some("Hon. Example Judge"));
    assert_eq!(c.forum.as_deref(), Some("Eighth Judicial District Court"));
    assert_eq!(c.docket_number.as_deref(), Some("A-26-000001-C"));
}

/// **`entry_number` is text.** Real dockets use attachment sub-numbers,
/// and the entry list must reference them exactly — an integer column
/// could not hold `29-1` at all.
#[tokio::test]
async fn a_docket_entry_number_round_trips_an_attachment_sub_number() {
    let db = &mem().await;
    let project_id = open_matter(db, "subnumber").await;
    let c = open_case(db, &a_case(project_id, "Alpha v. Beta"))
        .await
        .expect("case");

    for (number, title) in [
        ("29", "Motion for Summary Judgment"),
        ("29-1", "Exhibit A to Motion"),
        ("29-2", "Declaration in Support"),
    ] {
        record_entry(
            db,
            &NewDocketEntry {
                case_id: c.id,
                entry_number: number,
                kind: EntryKind::Motion,
                title,
                party: Some("Plaintiff"),
                filed_or_served_on: Some("2026-08-01T00:00:00Z"),
                scheduled_on: None,
                supersedes: None,
                document_asset_id: None,
                notation_id: None,
            },
        )
        .await
        .expect("entry");
    }

    let entries = docket(db, c.id).await.expect("docket");
    let numbers: Vec<&str> = entries.iter().map(|e| e.entry_number.as_str()).collect();
    assert_eq!(
        numbers,
        ["29", "29-1", "29-2"],
        "sub-numbers must survive verbatim"
    );
}

/// **Source pending is a state, not an error.** An entry with no asset
/// is legitimate, and the state is derived from the link so it cannot
/// disagree with reality.
#[tokio::test]
async fn an_entry_without_a_document_is_source_pending_not_invalid() {
    let db = &mem().await;
    let project_id = open_matter(db, "pending").await;
    let c = open_case(db, &a_case(project_id, "Alpha v. Beta"))
        .await
        .expect("case");

    let pending = record_entry(
        db,
        &NewDocketEntry {
            case_id: c.id,
            entry_number: "1",
            kind: EntryKind::Order,
            title: "Order Setting Trial",
            party: Some("Court"),
            filed_or_served_on: Some("2026-08-01T00:00:00Z"),
            scheduled_on: None,
            supersedes: None,
            document_asset_id: None,
            notation_id: None,
        },
    )
    .await
    .expect("the entry is valid without a document");

    assert!(
        is_source_pending(&pending),
        "no asset link means source pending"
    );
    assert_eq!(
        docket(db, c.id).await.expect("docket").len(),
        1,
        "and it is a real row on the docket"
    );
}

/// The spine carries every stage without a migration per stage.
#[tokio::test]
async fn the_spine_carries_entries_from_pleading_through_appeal() {
    let db = &mem().await;
    let project_id = open_matter(db, "stages").await;
    let c = open_case(db, &a_case(project_id, "Alpha v. Beta"))
        .await
        .expect("case");

    let stages = [
        (EntryKind::Pleading, "Complaint"),
        (EntryKind::Motion, "Motion to Dismiss"),
        (EntryKind::Opposition, "Opposition"),
        (EntryKind::Order, "Order Denying Motion"),
        (EntryKind::Subpoena, "Subpoena Duces Tecum"),
        (EntryKind::ExpertDisclosure, "Expert Disclosure"),
        (EntryKind::Hearing, "Hearing on Motions in Limine"),
        (EntryKind::Trial, "Trial Setting"),
        (EntryKind::Appeal, "Notice of Appeal"),
        (EntryKind::Settlement, "Stipulated Dismissal"),
    ];
    for (i, (kind, title)) in stages.iter().enumerate() {
        let entry_number = (i + 1).to_string();
        let scheduled_on = kind.is_appearance().then_some(at("2027-06-01T09:00:00Z"));
        record_entry(
            db,
            &NewDocketEntry {
                case_id: c.id,
                entry_number: &entry_number,
                kind: *kind,
                title,
                party: None,
                filed_or_served_on: None,
                scheduled_on,
                supersedes: None,
                document_asset_id: None,
                notation_id: None,
            },
        )
        .await
        .expect("entry");
    }

    assert_eq!(docket(db, c.id).await.expect("docket").len(), stages.len());
}

/// An entry number is unique within its case — a docket cannot have two
/// number 1s — but the same number on another case is fine.
#[tokio::test]
async fn entry_numbers_are_unique_within_a_case_only() {
    let db = &mem().await;
    let project_id = open_matter(db, "unique").await;
    let one = open_case(db, &a_case(project_id, "Alpha v. Beta"))
        .await
        .expect("case one");
    let two = open_case(db, &a_case(project_id, "Alpha v. Gamma"))
        .await
        .expect("case two");

    let entry = |case_id: Uuid| an_entry(case_id, "1", EntryKind::Pleading, "Complaint");

    record_entry(db, &entry(one.id)).await.expect("first");
    record_entry(db, &entry(two.id))
        .await
        .expect("the same number on a different case is fine");
    record_entry(db, &entry(one.id))
        .await
        .expect_err("a duplicate number on the same case must be refused");
}

/// Written discovery is the first device to earn its own tables: a set
/// of numbered items with per-item responses and objections.
#[tokio::test]
async fn a_served_discovery_set_carries_numbered_items_and_responses() {
    let db = &mem().await;
    let project_id = open_matter(db, "discovery").await;
    let c = open_case(db, &a_case(project_id, "Alpha v. Beta"))
        .await
        .expect("case");

    // Service is recorded on the spine, and the structured set points
    // back at it.
    let served_entry = record_entry(
        db,
        &NewDocketEntry {
            case_id: c.id,
            entry_number: "12",
            kind: EntryKind::DiscoveryRequest,
            title: "Defendant's First Set of Interrogatories",
            party: Some("Defendant"),
            filed_or_served_on: Some("2026-08-01T00:00:00Z"),
            scheduled_on: None,
            supersedes: None,
            document_asset_id: None,
            notation_id: None,
        },
    )
    .await
    .expect("spine entry");

    let set = serve_discovery(
        db,
        &NewDiscoveryRequest {
            case_id: c.id,
            docket_entry_id: Some(served_entry.id),
            device: Device::Interrogatories,
            set_number: 1,
            propounding_party: "Defendant",
            responding_party: "Plaintiff",
            served_on: Some("2026-08-01T00:00:00Z"),
            responses_due_on: Some("2026-08-31T00:00:00Z"),
        },
    )
    .await
    .expect("served");
    assert_eq!(set.status, "served");
    assert_eq!(set.docket_entry_id, Some(served_entry.id));

    let first = add_item(db, set.id, 1, "State each fact supporting your claim.")
        .await
        .expect("item one");
    add_item(db, set.id, 2, "Identify each witness.")
        .await
        .expect("item two");

    assert!(
        first.response_text.is_none(),
        "an item is unanswered until someone answers it"
    );

    answer_item(
        db,
        first.id,
        "Responding party incorporates its general objections.",
        Some("Overbroad; premature contention interrogatory."),
    )
    .await
    .expect("answer");

    let all = items(db, set.id).await.expect("items");
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].item_number, 1);
    assert!(all[0].response_text.is_some());
    assert!(all[0].objections.is_some());
    assert!(
        all[1].response_text.is_none(),
        "answering one item leaves the others alone"
    );
}

/// One set number per device per case, so "Set Two" is unambiguous — but
/// a different device may reuse the number.
#[tokio::test]
async fn a_set_number_is_unique_per_device_per_case() {
    let db = &mem().await;
    let project_id = open_matter(db, "sets").await;
    let c = open_case(db, &a_case(project_id, "Alpha v. Beta"))
        .await
        .expect("case");

    let set = |device: Device| NewDiscoveryRequest {
        case_id: c.id,
        docket_entry_id: None,
        device,
        set_number: 1,
        propounding_party: "Defendant",
        responding_party: "Plaintiff",
        served_on: None,
        responses_due_on: None,
    };

    serve_discovery(db, &set(Device::Interrogatories))
        .await
        .expect("rogs set one");
    serve_discovery(db, &set(Device::RequestsForProduction))
        .await
        .expect("a different device may reuse set one");
    serve_discovery(db, &set(Device::Interrogatories))
        .await
        .expect_err("the same device may not have two set ones");
}

/// The database refuses values outside either closed vocabulary, even
/// written around the store commands. The engine enforces this with an
/// `CHECK`; the Surreal schema enforces it with an `ASSERT`, and the
/// point of the test is that neither vocabulary is only a Rust-side
/// convention.
#[tokio::test]
async fn the_database_refuses_an_unknown_entry_kind_or_device() {
    let db = &mem().await;
    let project_id = open_matter(db, "checks").await;
    let c = open_case(db, &a_case(project_id, "Alpha v. Beta"))
        .await
        .expect("case");

    let kind_error = db
        .query(
            "CREATE type::record('case_docket_entry', rand::uuid::v7()) SET \
             case_id = $case, entry_number = '1', kind = 'interpretive_dance', title = 'Nope'",
        )
        .bind(("case", store::surreal::record_id("case", c.id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .expect_err("kind is ASSERT-constrained");
    assert!(kind_error.to_string().contains("kind"), "{kind_error}");

    let device_error = db
        .query(
            "CREATE type::record('discovery_request', rand::uuid::v7()) SET \
             case_id = $case, device = 'telepathy', set_number = 1, \
             propounding_party = 'Plaintiff', responding_party = 'Defendant'",
        )
        .bind(("case", store::surreal::record_id("case", c.id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .expect_err("device is ASSERT-constrained");
    assert!(
        device_error.to_string().contains("device"),
        "{device_error}"
    );
}

/// Deleting a case takes its docket and its discovery with it; deleting
/// a matter takes its cases.
#[tokio::test]
async fn litigation_records_are_scoped_to_their_case_and_matter() {
    let db = &mem().await;
    let project_id = open_matter(db, "cascade").await;
    let c = open_case(db, &a_case(project_id, "Alpha v. Beta"))
        .await
        .expect("case");
    record_entry(db, &an_entry(c.id, "1", EntryKind::Pleading, "Complaint"))
        .await
        .expect("entry");
    let set = serve_discovery(
        db,
        &NewDiscoveryRequest {
            case_id: c.id,
            docket_entry_id: None,
            device: Device::RequestsForAdmission,
            set_number: 1,
            propounding_party: "Plaintiff",
            responding_party: "Defendant",
            served_on: None,
            responses_due_on: None,
        },
    )
    .await
    .expect("set");
    add_item(db, set.id, 1, "Admit that…").await.expect("item");

    store::cases::delete_for_project(db, project_id)
        .await
        .expect("delete matter cases");

    assert!(for_project(db, project_id).await.expect("q").is_empty());
    assert!(docket(db, c.id).await.expect("q").is_empty());
    assert!(items(db, set.id).await.expect("q").is_empty());
    assert!(store::cases::discovery_requests(db, c.id)
        .await
        .expect("q")
        .is_empty());
}

/// A hearing or trial is an appearance: the schema refuses a row of those
/// kinds without `scheduled_on`.
#[tokio::test]
async fn a_hearing_without_scheduled_on_is_refused() {
    let db = &mem().await;
    let project_id = open_matter(db, "hearing-date").await;
    let c = open_case(db, &a_case(project_id, "Alpha v. Beta"))
        .await
        .expect("case");

    let refused = db
        .query(
            "CREATE type::record('case_docket_entry', rand::uuid::v7()) SET \
             case_id = $case, entry_number = '40', kind = 'hearing', \
             title = 'Motion hearing'",
        )
        .bind(("case", store::surreal::record_id("case", c.id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .expect_err("a hearing needs scheduled_on");
    assert!(
        refused.to_string().contains("scheduled_on") || refused.to_string().contains("ASSERT"),
        "{refused}"
    );
}

/// `scheduled_on` is only for appearances. A pleading that carries one is
/// refused, the same way a hearing without one is.
#[tokio::test]
async fn a_pleading_with_scheduled_on_is_refused() {
    let db = &mem().await;
    let project_id = open_matter(db, "pleading-date").await;
    let c = open_case(db, &a_case(project_id, "Alpha v. Beta"))
        .await
        .expect("case");

    let refused = db
        .query(
            "CREATE type::record('case_docket_entry', rand::uuid::v7()) SET \
             case_id = $case, entry_number = '1', kind = 'pleading', \
             title = 'Complaint', scheduled_on = d'2027-03-15T09:00:00Z'",
        )
        .bind(("case", store::surreal::record_id("case", c.id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .expect_err("a pleading must not carry scheduled_on");
    assert!(
        refused.to_string().contains("scheduled_on") || refused.to_string().contains("ASSERT"),
        "{refused}"
    );
}

/// A continuance is a new entry of the same kind. The earlier date stays
/// on the superseded row; the calendar follows the chain to the later one.
#[tokio::test]
async fn a_continuance_chain_resolves_to_the_later_date() {
    let db = &mem().await;
    let project_id = open_matter(db, "continuance").await;
    let c = open_case(db, &a_case(project_id, "Alpha v. Beta"))
        .await
        .expect("case");

    let original = record_entry(
        db,
        &NewDocketEntry {
            scheduled_on: Some(at("2027-03-15T09:00:00Z")),
            ..an_entry(c.id, "40", EntryKind::Hearing, "Motion hearing")
        },
    )
    .await
    .expect("original setting");
    record_entry(
        db,
        &NewDocketEntry {
            scheduled_on: Some(at("2027-04-12T09:00:00Z")),
            supersedes: Some(original.id),
            ..an_entry(c.id, "41", EntryKind::Hearing, "Motion hearing (continued)")
        },
    )
    .await
    .expect("continuance");

    let appearances = current_appearances_for_project(db, project_id)
        .await
        .expect("appearances");
    assert_eq!(appearances.len(), 1, "{appearances:?}");
    assert_eq!(appearances[0].title, "Motion hearing (continued)");
    assert_eq!(appearances[0].scheduled_on, at("2027-04-12T09:00:00Z"));
}

/// Rows written before `scheduled_on` existed are reported, never filled in.
#[tokio::test]
async fn undated_hearings_are_reported_and_never_backfilled() {
    let db = &mem().await;
    let project_id = open_matter(db, "undated-hearing").await;
    let c = open_case(db, &a_case(project_id, "Alpha v. Beta"))
        .await
        .expect("case");

    db.query(
        "DEFINE FIELD OVERWRITE kind ON case_docket_entry TYPE string \
         ASSERT $value IN ['pleading', 'motion', 'opposition', 'reply', 'order', 'notice', \
         'stipulation', 'discovery_request', 'discovery_response', 'subpoena', \
         'expert_disclosure', 'hearing', 'trial', 'appeal', 'settlement', 'other']; \
         DEFINE FIELD OVERWRITE scheduled_on ON case_docket_entry TYPE option<datetime>",
    )
    .await
    .unwrap()
    .check()
    .unwrap();
    let id = Uuid::now_v7();
    db.query(
        "CREATE $id SET case_id = $case, entry_number = '40', kind = 'hearing', \
         title = 'Legacy hearing'",
    )
    .bind(("id", store::surreal::record_id("case_docket_entry", id)))
    .bind(("case", store::surreal::record_id("case", c.id)))
    .await
    .unwrap()
    .check()
    .expect("historical hearing without scheduled_on");
    store::schema::apply(db)
        .await
        .expect("restore the write-time ASSERT");

    let missing = undated_hearings_and_trials(db).await.expect("report");
    assert_eq!(missing.len(), 1, "{missing:?}");
    assert_eq!(missing[0].id, id);
    assert_eq!(missing[0].title, "Legacy hearing");
    assert_eq!(missing[0].kind, "hearing");
    let still_missing = docket(db, c.id)
        .await
        .expect("docket")
        .into_iter()
        .find(|entry| entry.id == id)
        .expect("legacy row");
    assert!(
        still_missing.scheduled_on.is_none(),
        "the report never writes a date"
    );
}

/// The CM/ECF stamp is recorded **alongside** the hand-typed entry fields,
/// not over them — a reviewer can see both and notice when they disagree
/// (LAW-24, option 2).
#[tokio::test]
async fn recording_a_stamp_leaves_the_hand_typed_fields_alone() {
    let db = &mem().await;
    let project_id = open_matter(db, "stamp").await;
    let c = open_case(db, &a_case(project_id, "Alpha v. Beta"))
        .await
        .expect("case");
    let entry = record_entry(
        db,
        &NewDocketEntry {
            filed_or_served_on: Some("2026-08-01T00:00:00Z"),
            ..an_entry(c.id, "29", EntryKind::Motion, "Motion for Summary Judgment")
        },
    )
    .await
    .expect("entry");
    assert_eq!(entry.stamp_case_number, None, "no stamp recorded yet");

    let stamped = record_stamp(
        db,
        entry.id,
        &CmEcfStamp {
            case_number: "1:23-cv-04567-ABC".to_string(),
            entry_number: "29".to_string(),
            filed_on: "03/14/24".to_string(),
        },
    )
    .await
    .expect("record stamp");

    assert_eq!(
        stamped.stamp_case_number.as_deref(),
        Some("1:23-cv-04567-ABC")
    );
    assert_eq!(stamped.stamp_entry_number.as_deref(), Some("29"));
    assert_eq!(stamped.stamp_filed_on.as_deref(), Some("03/14/24"));
    // The hand-typed fields this entry was created with are untouched.
    assert_eq!(stamped.entry_number, "29");
    assert_eq!(
        stamped.filed_or_served_on.as_deref(),
        Some("2026-08-01T00:00:00Z")
    );
}

/// An integrity check records both the result and what was fetched, and
/// `None` before any check is distinct from `Some(false)`, a check that
/// ran and found a mismatch (LAW-24, option 1).
#[tokio::test]
async fn an_integrity_check_records_the_source_hash_and_the_result() {
    let db = &mem().await;
    let project_id = open_matter(db, "integrity").await;
    let c = open_case(db, &a_case(project_id, "Alpha v. Beta"))
        .await
        .expect("case");
    let entry = record_entry(db, &an_entry(c.id, "1", EntryKind::Order, "Order"))
        .await
        .expect("entry");
    assert_eq!(
        entry.integrity_matched, None,
        "unchecked is distinct from checked-and-mismatched"
    );

    let matching = record_integrity_check(db, entry.id, "abc123", true)
        .await
        .expect("record match");
    assert_eq!(matching.integrity_matched, Some(true));
    assert_eq!(
        matching.integrity_source_sha256_hex.as_deref(),
        Some("abc123")
    );
    assert!(matching.integrity_checked_at.is_some());

    let mismatching = record_integrity_check(db, entry.id, "def456", false)
        .await
        .expect("record mismatch");
    assert_eq!(
        mismatching.integrity_matched,
        Some(false),
        "a later check overwrites the earlier result"
    );
    assert_eq!(
        mismatching.integrity_source_sha256_hex.as_deref(),
        Some("def456")
    );
}

/// LAW-133: the project-scoped read and write path. An entry is recorded
/// source pending, its filed document is linked later, and a continuance
/// supersedes the appearance it moves; the project-wide docket reads every
/// case's entries back.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn an_entry_is_recorded_linked_and_superseded_through_the_project_docket() {
    use store::cases::{
        docket_for_project, find_case_by_docket_number, find_entry, link_document, supersede,
        Continuance,
    };

    let db = &mem().await;
    let project_id = open_matter(db, "project-docket").await;
    let case = open_case(db, &a_case(project_id, "Alpha v. Beta"))
        .await
        .expect("open");

    // The manifest may leave the judge's initials off the docket number.
    let found = find_case_by_docket_number(db, project_id, "a-26-000001-c-XYZ")
        .await
        .expect("lookup")
        .expect("the case is found by its docket number");
    assert_eq!(found.id, case.id);
    assert!(find_case_by_docket_number(db, project_id, "a-26-000002-c")
        .await
        .expect("lookup")
        .is_none());

    let filed = record_entry(
        db,
        &an_entry(case.id, "12", EntryKind::Motion, "Motion to dismiss"),
    )
    .await
    .expect("record");
    assert!(is_source_pending(&filed));

    let tmp = tempfile::tempdir().expect("tempdir");
    let storage: std::sync::Arc<dyn cloud::StorageService> = std::sync::Arc::new(
        cloud::FsStorage::new(tmp.path().to_path_buf())
            .await
            .expect("fs storage"),
    );
    let asset = store::documents::ingest_bytes_as(
        db,
        &storage,
        &store::documents::IngestArgs {
            project_id,
            source: store::documents::source::UPLOAD,
            filename: "di-12-motion.pdf",
            kind: "filing",
            content_type: "application/pdf",
            description: None,
            visibility: store::documents::visibility::INTERNAL,
            secondary_storage_key: None,
        },
        &store::documents::DocumentIdentity {
            slug: Some("docket/a-26-000001-c/di-12-motion.pdf"),
            ..Default::default()
        },
        b"%PDF-1.7 synthetic",
    )
    .await
    .expect("file the motion");
    let asset_id = asset.asset_id;
    let linked = link_document(db, filed.id, asset_id).await.expect("link");
    assert_eq!(linked.document_asset_id, Some(asset_id));
    assert!(!is_source_pending(&linked));
    assert_eq!(
        find_entry(db, filed.id)
            .await
            .expect("find")
            .map(|e| e.document_asset_id),
        Some(Some(asset_id))
    );

    let mut hearing = an_entry(
        case.id,
        "13",
        EntryKind::Hearing,
        "Hearing on motion to dismiss",
    );
    hearing.scheduled_on = Some(at("2027-03-15T09:00:00Z"));
    let hearing = record_entry(db, &hearing).await.expect("hearing");
    let continued = supersede(
        db,
        hearing.id,
        &Continuance {
            entry_number: "14",
            scheduled_on: at("2027-04-01T09:00:00Z"),
            title: None,
            party: None,
            filed_or_served_on: Some("2027-03-01"),
            document_asset_id: None,
        },
    )
    .await
    .expect("continuance");
    assert_eq!(continued.kind, "hearing");
    assert_eq!(continued.title, hearing.title);
    assert_eq!(continued.supersedes, Some(hearing.id));
    assert_eq!(continued.scheduled_on, Some(at("2027-04-01T09:00:00Z")));

    let appearances = current_appearances_for_project(db, project_id)
        .await
        .expect("appearances");
    assert_eq!(appearances.len(), 1);
    assert_eq!(appearances[0].id, continued.id);

    let project_docket = docket_for_project(db, project_id)
        .await
        .expect("project docket");
    let numbers: Vec<&str> = project_docket
        .iter()
        .map(|entry| entry.entry_number.as_str())
        .collect();
    assert_eq!(numbers, ["12", "13", "14"]);

    // Only an appearance can be continued, and only an entry that exists.
    let refused = supersede(
        db,
        filed.id,
        &Continuance {
            entry_number: "15",
            scheduled_on: at("2027-04-01T09:00:00Z"),
            title: None,
            party: None,
            filed_or_served_on: None,
            document_asset_id: None,
        },
    )
    .await
    .expect_err("a motion is not an appearance");
    assert!(
        refused.to_string().contains("not an appearance"),
        "{refused}"
    );
    assert!(link_document(db, Uuid::now_v7(), asset_id).await.is_err());
}
