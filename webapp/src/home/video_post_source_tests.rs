//! The home page's video updates read from seeded Blog Posts on the Source
//! engine. Its own module, so the engine-free `home` tests stay selectable.

use super::home_video_post;

#[tokio::test]
async fn published_video_posts_group_social_links_and_skip_articles() {
    let db = store::test_support::mem_surreal().await;
    store::seed::seed_brand_blog(&db, store::seed::BrandSeed::Neon)
        .await
        .unwrap();
    let posts = store::posts::published_for_brand(&db, "vesta", chrono::Utc::now())
        .await
        .unwrap();
    let updates: Vec<_> = posts.into_iter().filter_map(home_video_post).collect();
    assert_eq!(updates.len(), 9);
    assert_eq!(updates[0].post.date, "October 20, 2026");
    assert_eq!(updates[1].post.date, "October 19, 2026");
    assert!(updates[..2]
        .iter()
        .all(|update| update.social_links.is_empty()));
    assert!(updates[2..].iter().all(|update| update
        .social_links
        .iter()
        .map(|link| link.label.as_str())
        .eq(["Facebook", "Instagram", "TikTok"])));
    assert_eq!(updates[2].post.date, "October 18, 2026");
    assert_eq!(updates[3].post.date, "October 17, 2026");
    assert_eq!(updates[4].post.date, "October 16, 2026");
    let article = store::posts::find_by_brand_slug(&db, "neon", "thanks-apple")
        .await
        .unwrap()
        .unwrap();
    assert!(home_video_post(article.clone()).is_none());
    let mut video = article;
    video.video_asset = Some("synthetic.mp4".into());
    video.social_links = vec![
        store::posts::SocialLink {
            provider: "instagram".into(),
            account: "example".into(),
            post_id: Some("example".into()),
            permalink: Some("https://www.instagram.com/reel/example/".into()),
        },
        store::posts::SocialLink {
            provider: "threads".into(),
            account: "example".into(),
            post_id: Some("example".into()),
            permalink: Some("https://www.threads.com/@example/post/example".into()),
        },
    ];
    let video = home_video_post(video).unwrap();
    assert!(video.video_url.is_some());
    assert_eq!(
        video
            .social_links
            .iter()
            .map(|link| link.label.as_str())
            .collect::<Vec<_>>(),
        ["Instagram", "Threads"]
    );
}
