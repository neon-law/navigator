use chrono::{Duration, SubsecRound, Utc};
use uuid::Uuid;

use super::*;
use crate::persons::Role;
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::postgres_mirror::{MirrorKey, Mirrored};
use crate::posts::{self, AccountAlias, ImportChange, SocialLink, SourceCaptions, SourcePost};
use crate::social_accounts::{
    self, ConnectSocialAccount, RefreshSocialAccount, SocialAccountError, SocialProvider,
};
use crate::social_publications::{self as publications, Admission};
use crate::surreal::SurrealDb;
use crate::test_support::mem_surreal;
use crate::{social_account_selections as selections, social_handles};

/// A Firm, its Admin, and a Brand, written to SurrealDB (which still answers
/// Brand authority) and copied into PostgreSQL, whose social tables name
/// them by foreign key.
async fn practice(surreal: &SurrealDb, pg: &PostgresDb) -> (Uuid, String) {
    let admin = crate::persons::create(
        surreal,
        &crate::persons::NewPerson::with_role(
            "Synthetic Social Admin",
            format!("{}@example.test", Uuid::now_v7()),
            Role::Admin,
        ),
    )
    .await
    .unwrap();
    let entity = crate::postgres_mirror::from_source(async {
        let entity_type = crate::entity_types::create(surreal, "Synthetic social entity type")
            .await
            .unwrap();
        let jurisdiction = crate::jurisdictions::create(
            surreal,
            &crate::jurisdictions::NewJurisdiction::new(
                format!("Synthetic social jurisdiction {}", Uuid::now_v7()),
                format!("S{}", &Uuid::now_v7().simple().to_string()[..7]),
                "state",
            ),
        )
        .await
        .unwrap();
        crate::entities::create(
            surreal,
            &crate::entities::NewEntity {
                name: format!("Synthetic social entity {}", Uuid::now_v7()),
                entity_type_id: entity_type.id,
                jurisdiction_id: jurisdiction.id,
                phone: None,
                url: None,
                xero_id: None,
                firm_anchor_key: None,
            },
        )
        .await
        .unwrap()
    })
    .await;
    let firm = crate::firms::create(
        surreal,
        &crate::firms::NewFirm {
            name: format!("Synthetic social firm {}", Uuid::now_v7()),
            status: "active".into(),
            entity_id: entity.id,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .unwrap();
    let brand = crate::brands::create(
        surreal,
        Role::Admin,
        Some(admin.id),
        &crate::brands::NewBrand {
            name: format!("Synthetic social brand {}", Uuid::now_v7()),
            key: format!("social-{}", Uuid::now_v7().simple()),
            firm_id: Some(firm.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    crate::postgres_mirror::during_initial_copy(async {
        for key in [
            MirrorKey::Person(admin.id),
            MirrorKey::Firm(firm.id),
            MirrorKey::Brand(brand.id),
        ] {
            crate::postgres_mirror::mirror(surreal, pg, &key)
                .await
                .unwrap();
        }
    })
    .await;
    (admin.id, brand.key)
}

/// A fresh PostgreSQL schema and a practice in it. SurrealDB still answers
/// Brand and Firm authority, so the practice is written there too.
async fn fixture() -> Option<(PostgresDb, SurrealDb, Uuid, String)> {
    let (options, database) = fresh_schema().await?;
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let (admin, brand) = Box::pin(practice(&surreal, &pg)).await;
    Some((pg, surreal, admin, brand))
}

async fn surreal_count(surreal: &SurrealDb, table: &str) -> usize {
    let rows: Vec<serde_json::Value> = surreal
        .query(format!("SELECT VALUE id FROM {table}"))
        .await
        .unwrap()
        .take(0)
        .unwrap();
    rows.len()
}

/// No social row reached SurrealDB: PostgreSQL held every write.
async fn assert_nothing_in_surreal(surreal: &SurrealDb) {
    for table in [
        "post",
        "blog_video",
        "social_handle",
        "social_publication",
        "social_account",
        "social_account_selection",
    ] {
        assert_eq!(surreal_count(surreal, table).await, 0, "{table}");
    }
}

fn source<'a>(brand_key: &'a str, slug: &'a str, status: &'a str) -> SourcePost<'a> {
    SourcePost {
        brand_key,
        slug,
        title: "Synthetic Post",
        description: "A synthetic Post.",
        body_html: "<p>Synthetic</p>",
        body_markdown: None,
        status,
        // Whole seconds, as an editorial source writes them.
        publish_at: (Utc::now() + Duration::days(1)).trunc_subsecs(0),
        video_asset: None,
        vertical_video: None,
        tiktok_ai_generated: None,
        captions: SourceCaptions::default(),
        accounts: &[],
        social_links: &[],
    }
}

fn connection<'a>(
    actor: Uuid,
    brand_key: &'a str,
    provider: SocialProvider,
    account: &'a str,
    scopes: &'a [String],
    access_token: &'a str,
) -> ConnectSocialAccount<'a> {
    ConnectSocialAccount {
        actor_role: Role::Admin,
        actor_person_id: actor,
        brand_key,
        provider,
        provider_account_id: account,
        display_name: Some("Synthetic Account"),
        scopes,
        expires_at: Some(Utc::now() + Duration::days(7)),
        access_token,
        refresh_token: Some("synthetic-refresh"),
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one connection walks connect, reads, refresh, and revocation"
)]
async fn social_accounts_live_in_postgres() {
    let Some((pg, surreal, admin, brand)) = Box::pin(fixture()).await else {
        return;
    };
    let kms = cloud::FakeKms::new("synthetic-social-postgres");
    let scopes = vec!["pages_manage_posts".to_string()];
    with_test_db(
        pg.clone(),
        Box::pin(async {
            // Two callbacks for one account converge on one row.
            let (first, second) = tokio::join!(
                social_accounts::connect(
                    &surreal,
                    connection(
                        admin,
                        &brand,
                        SocialProvider::Facebook,
                        "page-1",
                        &scopes,
                        "a"
                    ),
                    &kms
                ),
                social_accounts::connect(
                    &surreal,
                    connection(
                        admin,
                        &brand,
                        SocialProvider::Facebook,
                        "page-1",
                        &scopes,
                        "b"
                    ),
                    &kms
                )
            );
            let (first, second) = (first.unwrap(), second.unwrap());
            assert_eq!(first.id, second.id);
            let account = social_accounts::connected_account(
                &surreal,
                &brand,
                SocialProvider::Facebook,
                "page-1",
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(account.id, first.id);
            assert_eq!(account.scopes, scopes);
            assert_eq!(account.status, "connected");
            let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM social_account")
                .fetch_one(pg.pool())
                .await
                .unwrap();
            assert_eq!(rows, 1);
            let stored: String =
                sqlx::query_scalar("SELECT ciphertext FROM social_account WHERE id = $1")
                    .bind(&account.id)
                    .fetch_one(pg.pool())
                    .await
                    .unwrap();
            assert!(!stored.is_empty() && !stored.contains("synthetic-refresh"));

            assert_eq!(
                social_accounts::for_actor(&surreal, Role::Admin, admin)
                    .await
                    .unwrap(),
                vec![account.clone()]
            );
            let identity = social_accounts::for_provider_identity(
                &surreal,
                SocialProvider::Facebook,
                "page-1",
            )
            .await
            .unwrap();
            assert_eq!(identity, vec![account.clone()]);
            assert!(social_accounts::for_provider_identity(
                &surreal,
                SocialProvider::Facebook,
                "missing"
            )
            .await
            .unwrap()
            .is_empty());
            let selected = social_accounts::selected_for_brand(
                &surreal,
                &brand,
                std::slice::from_ref(&account.id),
            )
            .await
            .unwrap();
            assert_eq!(selected, vec![account.clone()]);
            assert!(social_accounts::selected_for_brand(
                &surreal,
                "another-brand",
                std::slice::from_ref(&account.id)
            )
            .await
            .unwrap()
            .is_empty());
            assert_eq!(
                social_accounts::connected_account(
                    &surreal,
                    &brand,
                    SocialProvider::Facebook,
                    "page-1"
                )
                .await
                .unwrap(),
                Some(account.clone())
            );

            social_accounts::update_refreshed_tokens(
                &surreal,
                RefreshSocialAccount {
                    brand_key: &brand,
                    provider: SocialProvider::Facebook,
                    provider_account_id: "page-1",
                    expires_at: Utc::now() + Duration::days(14),
                    access_token: "rotated-access",
                    refresh_token: Some("rotated-refresh"),
                },
                &kms,
            )
            .await
            .unwrap();
            let tokens = social_accounts::resolve_tokens_for_brand(
                &surreal,
                &brand,
                SocialProvider::Facebook,
                "page-1",
                &kms,
            )
            .await
            .unwrap();
            assert_eq!(tokens.access_token().unwrap(), "rotated-access");
            assert_eq!(tokens.refresh_token().unwrap(), Some("rotated-refresh"));

            social_accounts::revoke(
                &surreal,
                Role::Admin,
                admin,
                &brand,
                SocialProvider::Facebook,
                "page-1",
            )
            .await
            .unwrap();
            assert!(matches!(
                social_accounts::revoke(
                    &surreal,
                    Role::Admin,
                    admin,
                    &brand,
                    SocialProvider::Facebook,
                    "page-1",
                )
                .await,
                Err(SocialAccountError::NotFound)
            ));
            assert!(matches!(
                social_accounts::resolve_tokens(
                    &surreal,
                    &brand,
                    SocialProvider::Facebook,
                    "page-1",
                    &kms
                )
                .await,
                Err(SocialAccountError::NotFound)
            ));
            assert!(matches!(
                social_accounts::update_refreshed_tokens(
                    &surreal,
                    RefreshSocialAccount {
                        brand_key: &brand,
                        provider: SocialProvider::Facebook,
                        provider_account_id: "page-1",
                        expires_at: Utc::now(),
                        access_token: "too-late",
                        refresh_token: None,
                    },
                    &kms,
                )
                .await,
                Err(SocialAccountError::NotFound)
            ));
            let revoked = social_accounts::for_actor(&surreal, Role::Admin, admin)
                .await
                .unwrap();
            assert_eq!(revoked[0].status, "revoked");
            assert_eq!(revoked[0].revoked_by_person_id, Some(admin));
            assert!(social_accounts::connected_account(
                &surreal,
                &brand,
                SocialProvider::Facebook,
                "page-1"
            )
            .await
            .unwrap()
            .is_none());

            // Reconnecting restores the connection under the same key.
            let again = social_accounts::connect(
                &surreal,
                connection(
                    admin,
                    &brand,
                    SocialProvider::Facebook,
                    "page-1",
                    &scopes,
                    "c",
                ),
                &kms,
            )
            .await
            .unwrap();
            assert_eq!((again.id, again.revoked_at), (account.id, None));

            selections::save(
                &surreal,
                Role::Admin,
                admin,
                &brand,
                SocialProvider::Threads,
                "threads-1",
            )
            .await
            .unwrap();
            selections::save(
                &surreal,
                Role::Admin,
                admin,
                &brand,
                SocialProvider::Threads,
                "threads-2",
            )
            .await
            .unwrap();
            let saved = selections::for_brand(&surreal, &brand).await.unwrap();
            assert_eq!(saved.len(), 1);
            assert_eq!(saved[0].provider_account_id, "threads-2");
        }),
    )
    .await;
    assert_nothing_in_surreal(&surreal).await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one Brand walks handles, videos, imports, links, and cancellation"
)]
async fn posts_handles_and_videos_live_in_postgres() {
    let Some((pg, surreal, admin, brand)) = Box::pin(fixture()).await else {
        return;
    };
    let kms = cloud::FakeKms::new("synthetic-posts-postgres");
    with_test_db(
        pg.clone(),
        Box::pin(async {
            assert!(social_handles::find(&surreal, &brand, "x")
                .await
                .unwrap()
                .is_none());
            social_handles::set(&surreal, &brand, "x", "SyntheticOne")
                .await
                .unwrap();
            social_handles::set(&surreal, &brand, "x", "SyntheticTwo")
                .await
                .unwrap();
            let handle = social_handles::find(&surreal, &brand, "x")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(handle.handle, "SyntheticTwo");

            let video = posts::record_video(&surreal, &brand, "videos/a.mp4", 1080, 1920)
                .await
                .unwrap();
            assert_eq!(video.orientation, "vertical");
            assert_eq!(
                posts::find_video(&surreal, "videos/a.mp4").await.unwrap(),
                Some(video)
            );
            // One upload registers once.
            assert!(
                posts::record_video(&surreal, &brand, "videos/a.mp4", 10, 10)
                    .await
                    .is_err()
            );
            assert!(posts::find_video(&surreal, "videos/missing.mp4")
                .await
                .unwrap()
                .is_none());

            // Two first imports of one Brand+slug converge on one Post.
            let draft = source(&brand, "first", "draft");
            let (a, b) = tokio::join!(
                posts::import_source(&surreal, &draft, Utc::now()),
                posts::import_source(&surreal, &draft, Utc::now())
            );
            let (a, b) = (a.unwrap(), b.unwrap());
            assert_eq!(a.post_id, b.post_id);
            assert!([a.change, b.change].contains(&ImportChange::Created));
            assert_eq!(
                posts::import_source(&surreal, &draft, Utc::now())
                    .await
                    .unwrap()
                    .change,
                ImportChange::Unchanged
            );
            let post = posts::find_by_id(&surreal, a.post_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                (post.slug.as_str(), post.status.as_str()),
                ("first", "draft")
            );
            assert!(posts::find_by_id(&surreal, Uuid::now_v7())
                .await
                .unwrap()
                .is_none());

            // A connected account resolves; a Post links only its Brand's handle.
            let scopes = Vec::new();
            let account = social_accounts::connect(
                &surreal,
                connection(admin, &brand, SocialProvider::X, "x-1", &scopes, "token"),
                &kms,
            )
            .await
            .unwrap();
            let aliases = [
                AccountAlias {
                    provider: "x".into(),
                    provider_account_id: "x-1".into(),
                },
                AccountAlias {
                    provider: "threads".into(),
                    provider_account_id: "threads-unknown".into(),
                },
            ];
            let links = [SocialLink {
                provider: "x".into(),
                account: "SyntheticTwo".into(),
                post_id: None,
                permalink: None,
            }];
            let scheduled = SourcePost {
                accounts: &aliases,
                social_links: &links,
                ..source(&brand, "scheduled", "scheduled")
            };
            let imported = posts::import_source(&surreal, &scheduled, Utc::now())
                .await
                .unwrap();
            assert!(imported.social_scheduled);
            assert_eq!(imported.unresolved_accounts, ["threads:threads-unknown"]);
            let post = posts::find_by_brand_slug(&surreal, &brand, "scheduled")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(post.selected_social_accounts, [account.id]);
            assert_eq!(post.social_links, links);
            let wrong_handle = [SocialLink {
                account: "Someone".into(),
                ..links[0].clone()
            }];
            assert!(posts::import_source(
                &surreal,
                &SourcePost {
                    social_links: &wrong_handle,
                    ..scheduled.clone()
                },
                Utc::now()
            )
            .await
            .is_err());

            let link = posts::encode_social_link(&surreal, post.id, "x", "1850000000", None)
                .await
                .unwrap();
            assert_eq!(
                link.permalink.as_deref(),
                Some("https://x.com/SyntheticTwo/status/1850000000")
            );
            assert_eq!(
                posts::import_source(
                    &surreal,
                    &SourcePost {
                        social_links: std::slice::from_ref(&link),
                        ..scheduled.clone()
                    },
                    Utc::now()
                )
                .await
                .unwrap()
                .change,
                ImportChange::Unchanged
            );

            assert_eq!(
                posts::slugs_for_brand(&surreal, &brand).await.unwrap(),
                ["first".to_string(), "scheduled".to_string()]
                    .into_iter()
                    .collect::<std::collections::HashSet<_>>()
            );
            assert_eq!(
                posts::for_actor(&surreal, Role::Admin, admin)
                    .await
                    .unwrap()
                    .len(),
                2
            );
            assert_eq!(
                publications::due_or_future_scheduled(&surreal, 10)
                    .await
                    .unwrap()
                    .iter()
                    .map(|(id, _)| *id)
                    .collect::<Vec<_>>(),
                [post.id]
            );
            assert!(
                posts::published_for_brand(&surreal, &brand, Utc::now() + Duration::days(2))
                    .await
                    .unwrap()
                    .iter()
                    .any(|live| live.id == post.id)
            );
            assert!(posts::published_for_brand(&surreal, &brand, Utc::now())
                .await
                .unwrap()
                .is_empty());
            posts::cancel(&surreal, Role::Admin, admin, post.id)
                .await
                .unwrap();
            assert_eq!(
                posts::find_by_id(&surreal, post.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                "cancelled"
            );
        }),
    )
    .await;
    assert_nothing_in_surreal(&surreal).await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one publication walks every state the publisher records"
)]
async fn social_publications_live_in_postgres() {
    let Some((pg, surreal, _, brand)) = Box::pin(fixture()).await else {
        return;
    };
    with_test_db(
        pg.clone(),
        Box::pin(async {
            let mut ids = Vec::new();
            for slug in ["one", "two", "three"] {
                ids.push(
                    posts::import_source(&surreal, &source(&brand, slug, "draft"), Utc::now())
                        .await
                        .unwrap()
                        .post_id,
                );
            }
            let (post, other, third) = (ids[0], ids[1], ids[2]);
            assert!(matches!(
                publications::admit(&surreal, post, "myspace").await,
                Err(publications::SocialPublicationError::UnsupportedProvider)
            ));

            // The unique (post, provider) index admits one sender.
            let (a, b) = tokio::join!(
                publications::admit(&surreal, post, "TikTok"),
                publications::admit(&surreal, post, "tiktok")
            );
            let admitted = [a.unwrap(), b.unwrap()];
            assert_eq!(
                admitted
                    .iter()
                    .filter(|a| matches!(a, Admission::Admitted(_)))
                    .count(),
                1
            );
            assert!(admitted
                .iter()
                .any(|a| matches!(a, Admission::AlreadyInFlight(_))));
            assert!(publications::begin_provider_write(&surreal, post, "tiktok")
                .await
                .unwrap());
            assert!(
                !publications::begin_provider_write(&surreal, post, "tiktok")
                    .await
                    .unwrap()
            );
            publications::record_provider_task(&surreal, post, "tiktok", "task-1")
                .await
                .unwrap();
            publications::record_provider_task(&surreal, post, "tiktok", "task-1")
                .await
                .unwrap();
            assert!(
                publications::record_provider_task(&surreal, post, "tiktok", "task-2")
                    .await
                    .is_err()
            );
            assert!(matches!(
                publications::admit(&surreal, post, "tiktok").await.unwrap(),
                Admission::ReconciliationRequired(p)
                    if p.provider_task_id.as_deref() == Some("task-1")
                        && p.id == publications::publication_key(post, "tiktok")
            ));
            let confirmed = publications::mark_confirmed(&surreal, post, "tiktok", "video-1", None)
                .await
                .unwrap();
            assert_eq!(
                confirmed.last_error.as_deref(),
                Some(publications::PERMALINK_PENDING)
            );
            assert!(matches!(
                publications::admit(&surreal, post, "tiktok").await.unwrap(),
                Admission::AlreadyConfirmed(_)
            ));
            assert_eq!(
                publications::pending_links(&surreal, "tiktok", 10)
                    .await
                    .unwrap()
                    .len(),
                1
            );
            let linked = publications::record_permalink(
                &surreal,
                post,
                "tiktok",
                "https://www.tiktok.com/@synthetic/video/1",
            )
            .await
            .unwrap();
            assert_eq!(linked.last_error, None);
            assert!(publications::record_permalink(
                &surreal,
                post,
                "tiktok",
                "https://www.tiktok.com/@synthetic/video/2"
            )
            .await
            .is_err());
            assert!(publications::pending_links(&surreal, "tiktok", 10)
                .await
                .unwrap()
                .is_empty());
            assert_eq!(
                publications::confirmed_links(&surreal, "tiktok", 10)
                    .await
                    .unwrap(),
                std::slice::from_ref(&linked)
            );

            // A known failure retries; an ambiguous one waits.
            publications::admit(&surreal, other, "youtube")
                .await
                .unwrap();
            publications::mark_failed(&surreal, other, "youtube", "temporary")
                .await
                .unwrap();
            assert!(matches!(
                publications::admit(&surreal, other, "youtube").await.unwrap(),
                Admission::Admitted(p) if p.attempt_count == 2 && p.last_error.is_none()
            ));
            publications::mark_unknown(&surreal, other, "youtube", "timed out")
                .await
                .unwrap();
            assert!(matches!(
                publications::admit(&surreal, other, "youtube")
                    .await
                    .unwrap(),
                Admission::ReconciliationRequired(_)
            ));
            assert!(
                publications::mark_failed_before_send(&surreal, other, "youtube", "revoked")
                    .await
                    .is_err()
            );
            publications::admit(&surreal, third, "facebook")
                .await
                .unwrap();
            let failed =
                publications::mark_failed_before_send(&surreal, third, "facebook", "revoked")
                    .await
                    .unwrap();
            assert_eq!(failed.state, publications::FAILED);

            let all = publications::for_posts(&surreal, &[post, other, third])
                .await
                .unwrap();
            assert_eq!(all.len(), 3);
            assert!(publications::for_posts(&surreal, &[])
                .await
                .unwrap()
                .is_empty());
            assert_eq!(
                publications::find(&surreal, post, "TIKTOK").await.unwrap(),
                Some(linked)
            );
            assert!(publications::find(&surreal, third, "x")
                .await
                .unwrap()
                .is_none());
        }),
    )
    .await;
    assert_nothing_in_surreal(&surreal).await;
}

/// SurrealDB rows are no evidence once PostgreSQL holds these tables: the
/// write hook and reconcile write nothing, parity names only the rows
/// PostgreSQL lacks until a Mirror Backfill completes, and only that first
/// backfill copies them.
#[tokio::test]
async fn the_mirror_leaves_postgres_alone() {
    let Some((pg, surreal, _, brand)) = Box::pin(fixture()).await else {
        return;
    };
    let legacy = crate::postgres_mirror::from_source(posts::import_source(
        &surreal,
        &source(&brand, "legacy", "draft"),
        Utc::now(),
    ))
    .await
    .unwrap()
    .post_id;
    surreal
        .query("CREATE social_handle SET brand_key=$brand, provider='x', handle='Legacy'")
        .bind(("brand", brand.clone()))
        .await
        .unwrap()
        .check()
        .unwrap();
    let key = MirrorKey::Post(legacy);
    with_test_db(
        pg.clone(),
        Box::pin(async {
            assert_eq!(
                crate::postgres_mirror::mirror(&surreal, &pg, &key)
                    .await
                    .unwrap(),
                Mirrored::Unchanged
            );
            assert!(posts::find_by_id(&surreal, legacy).await.unwrap().is_none());
            let before = crate::postgres_mirror::reconcile(&surreal, &pg)
                .await
                .unwrap();
            let missing = before.divergent().cloned().collect::<Vec<_>>();
            assert_eq!(
                missing,
                [
                    key.clone(),
                    MirrorKey::SocialHandle {
                        brand_key: brand.clone(),
                        provider: "x".into(),
                    },
                ]
            );
            // A row only PostgreSQL holds is its own.
            social_handles::set(&surreal, &brand, "threads", "Native")
                .await
                .unwrap();
            assert_eq!(
                crate::postgres_mirror::parity(&surreal, &pg)
                    .await
                    .unwrap()
                    .divergent()
                    .count(),
                2
            );

            crate::postgres_mirror::during_initial_copy(crate::postgres_mirror::reconcile(
                &surreal, &pg,
            ))
            .await
            .unwrap();
            assert!(posts::find_by_id(&surreal, legacy).await.unwrap().is_some());
            assert_eq!(
                social_handles::find(&surreal, &brand, "threads")
                    .await
                    .unwrap()
                    .unwrap()
                    .handle,
                "Native"
            );

            sqlx::query(
                "INSERT INTO mirror_backfill_run (id, batch_size, status, completed_at) \
                 VALUES ($1, 1, 'completed', now())",
            )
            .bind(Uuid::new_v4())
            .execute(pg.pool())
            .await
            .unwrap();
            for parity in [
                crate::postgres_mirror::parity(&surreal, &pg).await.unwrap(),
                crate::postgres_mirror::social_accounts::parity(&surreal, &pg)
                    .await
                    .unwrap(),
                crate::postgres_mirror::social_account_selections::parity(&surreal, &pg)
                    .await
                    .unwrap(),
                crate::postgres_mirror::blog_videos::parity(&surreal, &pg)
                    .await
                    .unwrap(),
            ] {
                assert!(parity.tables.is_empty(), "{parity:?}");
            }
        }),
    )
    .await;
}
