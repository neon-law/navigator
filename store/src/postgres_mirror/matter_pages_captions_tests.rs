use std::process::Command;

use rules::pages::PageEntry;
use word::matter::Matter;

use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema, synthetic_entity, test_postgres};
use crate::{matter_captions, matter_pages};

fn page(slug: &str, client: Option<&[&str]>) -> PageEntry {
    let mut lenses = BTreeMap::new();
    lenses.insert(
        "lawyer".to_string(),
        ["deadline_board", "boundary_note", "provenance_statement"]
            .map(String::from)
            .to_vec(),
    );
    if let Some(client) = client {
        lenses.insert(
            "client".to_string(),
            client.iter().map(|s| (*s).to_string()).collect(),
        );
    }
    PageEntry {
        slug: slug.to_string(),
        kind: "docket_deadline_board".to_string(),
        title: format!("Page {slug}"),
        draft: Some(format!("drafts/{slug}.md")),
        lenses,
    }
}

const CLIENT: &[&str] = &["key_dates_strip", "boundary_note", "provenance_statement"];

fn pages() -> Vec<PageEntry> {
    vec![page("timeline", Some(CLIENT)), page("deadlines", None)]
}

fn matter(case_number: &str) -> Matter {
    Matter {
        court: vec!["SUPERIOR COURT".into(), "COUNTY OF SAMPLE".into()],
        case_number: case_number.into(),
        judicial_officer: "Hon. Sample Jurist".into(),
        department: "12".into(),
        filed_on: "2026-02-02".into(),
        trial_on: None,
        parties: vec![
            Party {
                name: "A CORP".into(),
                role: "Plaintiff".into(),
            },
            Party {
                name: "B CORP".into(),
                role: "Defendant".into(),
            },
        ],
        counsel: Counsel {
            name: "Pat Sample".into(),
            bar_jurisdiction: "California".into(),
            bar_number: "000000".into(),
            firm: "SAMPLE LAW".into(),
            address: vec!["1 Example Way".into(), "Sample City".into()],
            telephone: "(000) 555-0100".into(),
            email: "pat@example.invalid".into(),
            appears_for: "Plaintiff A CORP".into(),
            pro_per: false,
        },
    }
}

/// A Firm with its Admin DRI and Entity, written through the store.
async fn source_firm(surreal: &SurrealDb) -> Uuid {
    let entity_id = synthetic_entity(surreal).await;
    let admin = crate::persons::create(
        surreal,
        &crate::persons::NewPerson::with_role(
            "Synthetic Admin",
            format!("matter-admin-{}@example.com", Uuid::now_v7()),
            crate::persons::Role::Admin,
        ),
    )
    .await
    .unwrap()
    .id;
    crate::firms::create(
        surreal,
        &crate::firms::NewFirm {
            name: format!("Synthetic Matter Practice {}", Uuid::now_v7().simple()),
            status: "active".into(),
            entity_id,
            admin_dri_person_id: admin,
        },
    )
    .await
    .unwrap()
    .id
}

async fn source_project(surreal: &SurrealDb, firm_id: Option<Uuid>) -> Uuid {
    let entity_id = synthetic_entity(surreal).await;
    let id = Uuid::now_v7();
    crate::projects::create(
        surreal,
        &crate::projects::NewProject {
            code: crate::projects::code_from_name("Synthetic matter pages", id),
            name: "Synthetic matter pages".into(),
            status: "open".into(),
            entity_id,
            firm_id,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .id
}

async fn stored_pages(pg: &PostgresDb, project_id: Uuid) -> Vec<PageImage> {
    target_pages(pg, Some(project_id))
        .await
        .unwrap()
        .remove(&project_id)
        .unwrap_or_default()
}

fn pages_key(project_id: Uuid) -> MirrorKey {
    MirrorKey::MatterPages(project_id)
}

fn caption_key(project_id: Uuid) -> MirrorKey {
    MirrorKey::MatterCaption(project_id)
}

#[test]
fn keys_name_the_project_and_never_page_or_caption_values() {
    let id = Uuid::nil();
    assert_eq!(pages_key(id).table(), "matter_page");
    assert_eq!(caption_key(id).table(), "matter_caption");
    assert_eq!(pages_key(id).to_string(), format!("matter_page:{id}"));
    assert_eq!(caption_key(id).to_string(), format!("matter_caption:{id}"));
}

#[test]
fn an_unreadable_sync_timestamp_is_refused() {
    assert!(matches!(
        synced("yesterday"),
        Err(MirrorError::SourceTimestamp)
    ));
    assert_eq!(
        synced("2026-10-18T12:00:00.123456789+00:00").unwrap(),
        "2026-10-18T12:00:00.123456Z"
            .parse::<DateTime<Utc>>()
            .unwrap()
    );
}

#[tokio::test]
async fn a_whole_set_sync_converges_replays_and_drops_removed_pages() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = crate::test_support::mem_surreal().await;
    let firm_id = source_firm(&surreal).await;
    let project_id = source_project(&surreal, Some(firm_id)).await;

    let synced = matter_pages::replace_for_project(&surreal, project_id, &pages())
        .await
        .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, &pages_key(project_id)).await.unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        mirror(&surreal, &pg, &pages_key(project_id)).await.unwrap(),
        Mirrored::Unchanged,
        "a replay writes nothing"
    );
    let stored = stored_pages(&pg, project_id).await;
    assert_eq!(
        stored.iter().map(|page| page.id).collect::<Vec<_>>(),
        synced.iter().map(|page| page.id).collect::<Vec<_>>(),
        "source identities and declared order survive"
    );
    assert_eq!(stored[0].slug, "timeline");
    assert_eq!(stored[0].position, 0);
    assert_eq!(stored[1].position, 1);
    assert_eq!(stored[0].firm_id, Some(firm_id));
    assert_eq!(stored[0].draft.as_deref(), Some("drafts/timeline.md"));
    assert_eq!(
        stored[0].client_sections.as_deref(),
        Some(
            CLIENT
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>()
                .as_slice()
        )
    );
    assert_eq!(
        stored[1].client_sections, None,
        "an undeclared lens stays undeclared"
    );
    assert_eq!(stored[1].clerk_sections, None);
    assert_eq!(
        stored[0].synced_at,
        synced_at(&synced[0].synced_at),
        "the sync timestamp is preserved"
    );
    assert!(parity(&surreal, &pg).await.unwrap().is_clean());

    // A re-sync that reorders and drops a page replaces the whole set.
    let resynced =
        matter_pages::replace_for_project(&surreal, project_id, &[page("deadlines", None)])
            .await
            .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, &pages_key(project_id)).await.unwrap(),
        Mirrored::Written
    );
    let stored = stored_pages(&pg, project_id).await;
    assert_eq!(stored.len(), 1, "the removed page leaves PostgreSQL");
    assert_eq!(stored[0].id, resynced[0].id);
    assert_eq!(
        (stored[0].slug.as_str(), stored[0].position),
        ("deadlines", 0)
    );

    // The matter-delete cascade empties the set.
    matter_pages::delete_for_project(&surreal, project_id)
        .await
        .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, &pages_key(project_id)).await.unwrap(),
        Mirrored::Deleted
    );
    assert!(stored_pages(&pg, project_id).await.is_empty());
    assert!(parity(&surreal, &pg).await.unwrap().is_clean());
    pg.0.close().await;
}

fn synced_at(text: &str) -> DateTime<Utc> {
    synced(text).unwrap()
}

#[tokio::test]
async fn a_caption_round_trips_its_nested_fields_and_an_explicit_empty_block_removes_it() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = crate::test_support::mem_surreal().await;
    let firm_id = source_firm(&surreal).await;
    let project_id = source_project(&surreal, Some(firm_id)).await;

    let first = matter_captions::replace_for_project(&surreal, project_id, Some(&matter("00CV1")))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, &caption_key(project_id))
            .await
            .unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        mirror(&surreal, &pg, &caption_key(project_id))
            .await
            .unwrap(),
        Mirrored::Unchanged
    );
    let stored = target_captions(&pg)
        .await
        .unwrap()
        .remove(&project_id)
        .unwrap();
    assert_eq!(stored.id, first.id);
    assert_eq!(stored.firm_id, Some(firm_id));
    assert_eq!(
        stored.parties,
        matter("00CV1").parties,
        "party order survives"
    );
    assert_eq!(stored.counsel, matter("00CV1").counsel);
    assert_eq!(stored.trial_on, None);
    assert_eq!(stored.synced_at, synced_at(&first.synced_at));

    // A re-sync replaces the Project's one caption under its new id.
    let second = matter_captions::replace_for_project(&surreal, project_id, Some(&matter("00CV2")))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, &caption_key(project_id))
            .await
            .unwrap(),
        Mirrored::Written
    );
    let captions = target_captions(&pg).await.unwrap();
    assert_eq!(captions.len(), 1);
    assert_eq!(captions[&project_id].id, second.id);
    assert_eq!(captions[&project_id].case_number, "00CV2");

    // `matter: {}` removes the caption from both stores.
    assert!(
        matter_captions::replace_for_project(&surreal, project_id, Some(&Matter::default()))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        mirror(&surreal, &pg, &caption_key(project_id))
            .await
            .unwrap(),
        Mirrored::Deleted
    );
    assert!(target_captions(&pg).await.unwrap().is_empty());
    assert!(parity(&surreal, &pg).await.unwrap().is_clean());
    pg.0.close().await;
}

#[tokio::test]
async fn parity_names_divergent_projects_and_reconcile_repairs_them() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = crate::test_support::mem_surreal().await;
    let firm_id = source_firm(&surreal).await;
    let project_id = source_project(&surreal, Some(firm_id)).await;
    matter_pages::replace_for_project(&surreal, project_id, &pages())
        .await
        .unwrap();
    matter_captions::replace_for_project(&surreal, project_id, Some(&matter("00CV3")))
        .await
        .unwrap();
    for key in [pages_key(project_id), caption_key(project_id)] {
        mirror(&surreal, &pg, &key).await.unwrap();
    }

    // Changed, reordered, and extra pages, and a changed nested caption field.
    sqlx::query("UPDATE matter_page SET title = 'Tampered' WHERE project_id = $1 AND position = 0")
        .bind(project_id)
        .execute(&pg.0)
        .await
        .unwrap();
    sqlx::query("UPDATE matter_page SET position = 1 - position WHERE project_id = $1")
        .bind(project_id)
        .execute(&pg.0)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO matter_page (id, project_id, firm_id, slug, kind, title, position, synced_at) \
         VALUES ($1, $2, $3, 'stray', 'docket_deadline_board', 'Stray', 2, now())",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(firm_id)
    .execute(&pg.0)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE matter_caption SET counsel = jsonb_set(counsel, '{pro_per}', 'true') \
         WHERE project_id = $1",
    )
    .bind(project_id)
    .execute(&pg.0)
    .await
    .unwrap();

    let drift = parity(&surreal, &pg).await.unwrap();
    assert_eq!(drift.tables[0].divergent, vec![pages_key(project_id)]);
    assert_eq!(drift.tables[1].divergent, vec![caption_key(project_id)]);
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());
    let stored = stored_pages(&pg, project_id).await;
    assert_eq!(
        stored
            .iter()
            .map(|page| page.slug.as_str())
            .collect::<Vec<_>>(),
        ["timeline", "deadlines"]
    );
    assert_eq!(stored[0].title, "Page timeline");
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());
    pg.0.close().await;
}

#[tokio::test]
async fn rows_outside_their_projects_firm_are_refused() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = crate::test_support::mem_surreal().await;
    let owner = source_firm(&surreal).await;
    let other = source_firm(&surreal).await;
    let project_id = source_project(&surreal, Some(owner)).await;
    matter_pages::replace_for_project(&surreal, project_id, &pages())
        .await
        .unwrap();
    mirror(&surreal, &pg, &pages_key(project_id)).await.unwrap();
    crate::postgres_mirror::tenancy::mirror(&surreal, &pg, &MirrorKey::Firm(other))
        .await
        .unwrap();

    let cross_firm = sqlx::query(
        "INSERT INTO matter_page (id, project_id, firm_id, slug, kind, title, position, synced_at) \
         VALUES ($1, $2, $3, 'elsewhere', 'docket_deadline_board', 'Elsewhere', 5, now())",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(other)
    .execute(&pg.0)
    .await
    .unwrap_err();
    assert_eq!(
        MirrorError::from(cross_firm).to_string(),
        "PostgreSQL refused the row (SQLSTATE 23503)"
    );
    let cross_caption = sqlx::query(
        "INSERT INTO matter_caption (id, project_id, firm_id, court, case_number, \
         judicial_officer, department, filed_on, parties, counsel, synced_at) \
         VALUES ($1, $2, $3, '{}', '', '', '', '', '[]', '{}', now())",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(other)
    .execute(&pg.0)
    .await
    .unwrap_err();
    assert_eq!(
        MirrorError::from(cross_caption).to_string(),
        "PostgreSQL refused the row (SQLSTATE 23503)"
    );
    assert!(
        sqlx::query("UPDATE matter_page SET firm_id = $1 WHERE project_id = $2")
            .bind(other)
            .bind(project_id)
            .execute(&pg.0)
            .await
            .is_err()
    );

    // A Project with no Firm has no owner to prove: refused and reported.
    let unowned = source_project(&surreal, None).await;
    matter_pages::replace_for_project(&surreal, unowned, &pages())
        .await
        .unwrap();
    assert!(matches!(
        mirror(&surreal, &pg, &pages_key(unowned)).await,
        Err(MirrorError::Source)
    ));
    let report = reconcile(&surreal, &pg).await.unwrap();
    assert_eq!(report.tables[0].divergent, vec![pages_key(unowned)]);
    assert!(stored_pages(&pg, unowned).await.is_empty());
    pg.0.close().await;
}

/// The hook reads the process environment and the process-wide pool, so each
/// case runs the probe below in its own child process.
#[test]
fn matter_sync_hooks_are_isolated_from_disabled_and_unreachable_postgres() {
    let mut cases = vec!["disabled", "unreachable"];
    let database = test_postgres().map(|_| {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(fresh_schema())
            .unwrap()
    });
    if database.is_some() {
        cases.push("mirrored");
    }
    for case in cases {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "postgres_mirror::matter_pages_captions::tests::matter_sync_hook_probe",
                "--nocapture",
            ])
            .env("NAVIGATOR_MATTER_HOOK_TEST_CASE", case)
            .env(crate::postgres_mirror::MIRROR_ENV, "1")
            .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
            .env("NAVIGATOR_POSTGRES_PORT", "1")
            .env("NAVIGATOR_POSTGRES_USER", "postgres")
            .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
            .env("NAVIGATOR_POSTGRES_DATABASE", "synthetic")
            .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
            .env_remove("NAVIGATOR_POSTGRES_ROOT_CERT");
        match case {
            "disabled" => {
                child.env_remove("NAVIGATOR_POSTGRES_HOST");
            }
            "mirrored" => {
                let (options, name) = database.as_ref().unwrap();
                child
                    .env("NAVIGATOR_POSTGRES_PORT", options.get_port().to_string())
                    .env("NAVIGATOR_POSTGRES_DATABASE", name);
            }
            _ => {}
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "matter hook case {case}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[tokio::test]
async fn matter_sync_hook_probe() {
    let Ok(case) = std::env::var("NAVIGATOR_MATTER_HOOK_TEST_CASE") else {
        return;
    };
    let surreal = crate::test_support::mem_surreal().await;
    let firm_id = source_firm(&surreal).await;
    let project_id = source_project(&surreal, Some(firm_id)).await;

    // Every write succeeds in SurrealDB whatever PostgreSQL does.
    matter_pages::replace_for_project(&surreal, project_id, &pages())
        .await
        .unwrap();
    let kept = matter_pages::replace_for_project(&surreal, project_id, &[page("deadlines", None)])
        .await
        .unwrap();
    matter_captions::replace_for_project(&surreal, project_id, Some(&matter("00CV4")))
        .await
        .unwrap();
    let cleared_project = source_project(&surreal, Some(firm_id)).await;
    matter_captions::replace_for_project(&surreal, cleared_project, Some(&matter("00CV5")))
        .await
        .unwrap();
    matter_captions::replace_for_project(&surreal, cleared_project, None)
        .await
        .unwrap();
    assert_eq!(
        matter_pages::list_for_project(&surreal, project_id)
            .await
            .unwrap(),
        kept
    );

    if case == "mirrored" {
        let port: u16 = std::env::var("NAVIGATOR_POSTGRES_PORT")
            .unwrap()
            .parse()
            .unwrap();
        let options = sqlx::postgres::PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port)
            .username("postgres")
            .password("synthetic")
            .ssl_mode(sqlx::postgres::PgSslMode::Disable);
        let pg = connect(
            &options,
            &std::env::var("NAVIGATOR_POSTGRES_DATABASE").unwrap(),
        )
        .await;
        let stored = stored_pages(&pg, project_id).await;
        assert_eq!(
            stored.iter().map(|page| page.id).collect::<Vec<_>>(),
            [kept[0].id],
            "the hook mirrored the whole replacement"
        );
        let captions = target_captions(&pg).await.unwrap();
        assert_eq!(captions[&project_id].case_number, "00CV4");
        assert!(
            !captions.contains_key(&cleared_project),
            "the hook mirrored the explicit removal"
        );
        assert!(parity(&surreal, &pg).await.unwrap().is_clean());
        pg.0.close().await;
    }
}
