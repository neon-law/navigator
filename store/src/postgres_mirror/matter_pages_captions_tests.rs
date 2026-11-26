use std::process::Command;

use rules::pages::PageEntry;
use word::matter::Matter;

use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema, synthetic_entity};
use crate::{matter_captions, matter_pages::MatterPage};

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
        document_revision: None,
        media: Vec::new(),
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
        signed_on: None,
        in_re: String::new(),
        page_count: false,
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

async fn captured_project(surreal: &SurrealDb, owned: bool) -> (Uuid, Option<Uuid>) {
    let entity = synthetic_entity(surreal).await;
    let firm = if owned {
        let id = Uuid::now_v7();
        let now = Utc::now().to_rfc3339();
        surreal.query("CREATE $id SET name='Captured Firm',slug=$slug,status='active',entity_id=$entity,inserted_at=$now,updated_at=$now")
            .bind(("id",record_id("firm",id))).bind(("slug",id.to_string())).bind(("entity",record_id("entity",entity))).bind(("now",now)).await.unwrap().check().unwrap();
        Some(id)
    } else {
        None
    };
    let id = Uuid::now_v7();
    let now = Utc::now().to_rfc3339();
    surreal.query("CREATE $id SET code=$code,name='Captured Pages',brand='neon',status='open',entity_id=$entity,firm_id=$firm,inserted_at=$now,updated_at=$now")
        .bind(("id",record_id("project",id))).bind(("code",id.to_string())).bind(("entity",record_id("entity",entity))).bind(("firm",firm.map(|id|record_id("firm",id)))).bind(("now",now)).await.unwrap().check().unwrap();
    (id, firm)
}
async fn captured_pages(
    surreal: &SurrealDb,
    project: Uuid,
    pages: &[PageEntry],
) -> Vec<MatterPage> {
    let mut rows = Vec::new();
    for (position, entry) in pages.iter().enumerate() {
        let id = Uuid::now_v7();
        let now = Utc::now().to_rfc3339();
        let lens = |key: &str| entry.lenses.get(key).cloned();
        surreal.query("CREATE $id SET project_id=$project,slug=$slug,kind=$kind,title=$title,draft=$draft,document_revision=$revision,media=$media,position=$position,client_sections=$client,lawyer_sections=$lawyer,clerk_sections=$clerk,synced_at=$now")
            .bind(("id",record_id("matter_page",id))).bind(("project",record_id("project",project))).bind(("slug",entry.slug.clone())).bind(("kind",entry.kind.clone())).bind(("title",entry.title.clone())).bind(("draft",entry.draft.clone())).bind(("revision",entry.document_revision.clone())).bind(("media",serde_json::json!(entry.media))).bind(("position",i64::try_from(position).unwrap())).bind(("client",lens("client"))).bind(("lawyer",lens("lawyer"))).bind(("clerk",lens("clerk"))).bind(("now",now.clone())).await.unwrap().check().unwrap();
        rows.push(MatterPage {
            id,
            project_id: project,
            entry: entry.clone(),
            position: i64::try_from(position).unwrap(),
            synced_at: now,
        });
    }
    rows
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
async fn captured_pages_keep_identities_order_and_actual_row_counts() {
    super::super::from_source(super::super::during_initial_copy(async {
        let Some((options, database)) = fresh_schema().await else {
            return;
        };
        let pg = connect(&options, &database).await;
        let source = crate::test_support::mem_surreal().await;
        let (project, firm) = captured_project(&source, true).await;
        let mut declared = pages();
        let revision = Uuid::now_v7().to_string();
        declared[1].kind = "whatsapp_timeline".into();
        declared[1].draft = None;
        declared[1].document_revision = Some(revision.clone());
        let capture = captured_pages(&source, project, &declared).await;
        let missing = scoped_page_references(&source, &pg).await.unwrap();
        assert_eq!(missing.len(), 2);
        assert!(missing.iter().all(|row| row.source_orphans == 0
            && row.postgres_missing == 2
            && row.newly_target_missing == 2));
        assert_eq!(
            mirror(&source, &pg, &pages_key(project)).await.unwrap(),
            Mirrored::Written
        );
        assert_eq!(
            mirror(&source, &pg, &pages_key(project)).await.unwrap(),
            Mirrored::Unchanged
        );
        sqlx::query("UPDATE project SET name='Native parent' WHERE id=$1")
            .bind(project)
            .execute(pg.pool())
            .await
            .unwrap();
        assert_eq!(
            mirror(&source, &pg, &pages_key(project)).await.unwrap(),
            Mirrored::Unchanged
        );
        let name: String = sqlx::query_scalar("SELECT name FROM project WHERE id=$1")
            .bind(project)
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert_eq!(name, "Native parent");
        assert!(scoped_page_references(&source, &pg)
            .await
            .unwrap()
            .is_empty());
        let stored = stored_pages(&pg, project).await;
        assert_eq!(
            stored.iter().map(|row| row.id).collect::<Vec<_>>(),
            capture.iter().map(|row| row.id).collect::<Vec<_>>()
        );
        assert_eq!(stored[0].firm_id, firm);
        assert_eq!(
            stored[0].client_sections.as_deref(),
            Some(
                CLIENT
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect::<Vec<_>>()
                    .as_slice()
            )
        );
        assert!(stored[1].client_sections.is_none());
        assert_eq!(
            stored[1].document_revision.as_deref(),
            Some(revision.as_str())
        );
        assert_eq!(stored[0].synced_at, synced_at(&capture[0].synced_at));
        let report = scoped_page_parity(&source, &pg).await.unwrap();
        assert!(report.divergent.is_empty());
        assert_eq!((report.source_rows, report.target_rows), (2, 2));
        source
            .query("DELETE matter_page WHERE project_id=$project")
            .bind(("project", record_id("project", project)))
            .await
            .unwrap()
            .check()
            .unwrap();
        assert_eq!(
            mirror(&source, &pg, &pages_key(project)).await.unwrap(),
            Mirrored::Unchanged
        );
        let report = scoped_page_parity(&source, &pg).await.unwrap();
        assert_eq!((report.source_rows, report.target_rows), (0, 2));
        assert_eq!(report.divergent, vec![pages_key(project)]);
        assert_eq!(stored_pages(&pg, project).await, stored);
        pg.pool().close().await;
    }))
    .await;
}

#[tokio::test]
async fn page_refresh_preserves_target_only_rows_and_refuses_changed_ownership() {
    super::super::from_source(super::super::during_initial_copy(async {
        let Some((options,database))=fresh_schema().await else{return;};let pg=connect(&options,&database).await;
        let source=crate::test_support::mem_surreal().await;let(project,_)=captured_project(&source,true).await;let capture=captured_pages(&source,project,&pages()).await;
        mirror(&source,&pg,&pages_key(project)).await.unwrap();
        sqlx::query("INSERT INTO matter_page(id,project_id,firm_id,slug,kind,title,position,synced_at) SELECT $1,id,firm_id,'native','docket_deadline_board','Native',2,now() FROM project WHERE id=$2").bind(Uuid::now_v7()).bind(project).execute(pg.pool()).await.unwrap();
        source.query("UPDATE $id SET title='Captured refresh'").bind(("id",record_id("matter_page",capture[0].id))).await.unwrap().check().unwrap();
        // Initial copy cannot overwrite a target version. Authorized refresh updates captured identities only.
        mirror(&source,&pg,&pages_key(project)).await.unwrap();assert_eq!(stored_pages(&pg,project).await[0].title,"Page timeline");
        let images=source_pages(&source,Some(project)).await.unwrap().remove(&project).unwrap();
        write_pages(&source,&pg,project,&images,ImportMode::Refresh).await.unwrap();let stored=stored_pages(&pg,project).await;assert_eq!(stored.len(),3);assert_eq!(stored[0].title,"Captured refresh");
        let report=scoped_page_parity(&source,&pg).await.unwrap();assert_eq!((report.source_rows,report.target_rows),(2,3));assert_eq!(report.divergent,vec![pages_key(project)]);
        let (_,other_firm)=captured_project(&source,true).await;let other_firm=other_firm.unwrap();
        super::super::mirror(&source,&pg,&MirrorKey::Firm(other_firm)).await.unwrap();
        source.query("UPDATE $id SET firm_id=$firm").bind(("id",record_id("project",project))).bind(("firm",record_id("firm",other_firm))).await.unwrap().check().unwrap();
        let changed=source_pages(&source,Some(project)).await.unwrap().remove(&project).unwrap();
        assert!(write_pages(&source,&pg,project,&changed,ImportMode::Refresh).await.is_err());assert_eq!(stored_pages(&pg,project).await,stored);
        let refs=scoped_page_references(&source,&pg).await.unwrap();assert_eq!(refs.len(),1);assert_eq!(refs[0].field,"firm_id");assert_eq!((refs[0].source_orphans,refs[0].postgres_missing,refs[0].newly_target_missing),(0,2,2));
        pg.pool().close().await;
    })).await;
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
async fn parity_preserves_unknown_page_drift_while_reconciling_captions() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = crate::test_support::mem_surreal().await;
    let firm_id = source_firm(&surreal).await;
    let project_id = source_project(&surreal, Some(firm_id)).await;
    captured_pages(&surreal, project_id, &pages()).await;
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
    assert!(!reconcile(&surreal, &pg).await.unwrap().is_clean());
    let stored = stored_pages(&pg, project_id).await;
    assert_eq!(
        stored
            .iter()
            .map(|page| page.slug.as_str())
            .collect::<Vec<_>>(),
        ["deadlines", "timeline", "stray"]
    );
    assert_eq!(stored[1].title, "Tampered");
    assert!(!reconcile(&surreal, &pg).await.unwrap().is_clean());
    pg.0.close().await;
}

#[tokio::test]
async fn rows_outside_their_projects_firm_are_refused() {
    super::super::from_source(super::super::during_initial_copy(async {
        let Some((options, database)) = fresh_schema().await else {
            return;
        };
        let pg = connect(&options, &database).await;
        let surreal = crate::test_support::mem_surreal().await;
        let (project_id, _) = captured_project(&surreal, true).await;
        let (_, other) = captured_project(&surreal, true).await;
        let other = other.unwrap();
        captured_pages(&surreal, project_id, &pages()).await;
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
        let (unowned, _) = captured_project(&surreal, false).await;
        captured_pages(&surreal, unowned, &pages()).await;
        assert!(matches!(
            mirror(&surreal, &pg, &pages_key(unowned)).await,
            Err(MirrorError::Source)
        ));
        let report = reconcile(&surreal, &pg).await.unwrap();
        assert_eq!(report.tables[0].divergent, vec![pages_key(unowned)]);
        assert!(stored_pages(&pg, unowned).await.is_empty());
        pg.0.close().await;
    }))
    .await;
}

/// The hook reads the process environment and the process-wide pool, so each
/// case runs the probe below in its own child process.
#[test]
fn matter_sync_hooks_are_isolated_from_disabled_and_unreachable_postgres() {
    let database = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(fresh_schema())
        .unwrap();
    for case in ["disabled", "unreachable", "mirrored"] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "postgres_mirror::matter_pages_captions::tests::matter_sync_hook_probe",
                "--nocapture",
            ])
            .env("NAVIGATOR_MATTER_HOOK_TEST_CASE", case)
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
                let (options, name) = &database;
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
