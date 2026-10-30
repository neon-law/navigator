//! Firm content catalog for Brand-scoped Blog Posts.
//!
//! The catalog, dashboard, and calendar read the `post` and
//! `social_publication` tables. The native form schedules approved Posts through
//! the same command as the CLI. These pages show the
//! declared video and orientation, the measured orientation, and why each
//! provider would refuse the Post. The schedule on each page is read
//! from those tables alone, so it does not depend on which durable workflow
//! engine sends the Posts. A cancellation holds a future schedule.

use chrono::Utc;
#[cfg(any(feature = "server", test))]
use chrono::{Datelike, NaiveDate};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{Choice, Field, FormCard, Heading};
use crate::people::ViewerRole;

pub const APP_PATH: &str = "/app/admin/blog";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlogPostRow {
    pub id: String,
    pub brand: String,
    pub slug: String,
    pub title: String,
    pub status: String,
    pub publish_at: String,
    pub video_asset: Option<String>,
    #[serde(default)]
    pub video_url: Option<String>,
    #[serde(default)]
    pub vertical_video: bool,
    #[serde(default)]
    pub tiktok_ai_generated: Option<bool>,
    #[serde(default)]
    pub video_orientation: Option<String>,
    #[serde(default)]
    pub eligibility: Vec<ProviderEligibility>,
    pub facebook_caption: Option<String>,
    pub instagram_caption: Option<String>,
    pub threads_caption: Option<String>,
    pub youtube_caption: Option<String>,
    pub selected_social_accounts: Vec<String>,
    /// The Post's provider links. `link` is `None` until the provider post is
    /// known.
    #[serde(default)]
    pub social_links: Vec<SocialLinkRow>,
}

/// One provider link a Post carries.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialLinkRow {
    pub provider: String,
    pub handle: String,
    pub link: Option<String>,
}

fn disclosure_label(disclosure: Option<bool>) -> &'static str {
    match disclosure {
        Some(true) => "AI generated",
        Some(false) => "Not AI generated",
        None => "Not declared; publishing blocked",
    }
}

/// Whether one provider could receive a Post, and why not.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderEligibility {
    pub provider: String,
    pub refusal: Option<String>,
}

/// The providers whose eligibility the catalog explains.
#[cfg(feature = "server")]
const ELIGIBILITY_PROVIDERS: &[(&str, &str)] = &[
    ("facebook", "Facebook"),
    ("instagram", "Instagram"),
    ("threads", "Threads"),
    ("youtube", "YouTube"),
    ("linkedin", "LinkedIn"),
    ("x", "X"),
    ("tiktok", "TikTok for Business"),
];

/// A video upload's measured shape and the frontmatter lines that use it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadedVideo {
    pub key: String,
    pub width: u32,
    pub height: u32,
    pub orientation: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialAccountChoice {
    pub id: String,
    #[serde(default)]
    pub provider_account_id: String,
    pub brand: String,
    pub provider: String,
    pub display_name: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialPublicationRow {
    pub post_id: String,
    pub provider: String,
    pub state: String,
    pub provider_post_id: Option<String>,
    pub permalink: Option<String>,
    pub last_error: Option<String>,
    /// When the publication last changed, RFC 3339 in UTC.
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BlogCatalogPage {
    pub role: ViewerRole,
    pub brands: Vec<String>,
    pub rows: Vec<BlogPostRow>,
    pub social_accounts: Vec<SocialAccountChoice>,
    pub publications: Vec<SocialPublicationRow>,
    pub csrf_token: String,
    pub error: Option<String>,
    pub uploaded_video: Option<UploadedVideo>,
    pub brand_filter: Option<String>,
    pub tokens_href: String,
    pub firm_name: String,
    #[serde(default)]
    pub calendar: CalendarMonth,
}

/// A UTC calendar month carried with the authorized catalog response.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CalendarMonth {
    label: String,
    month: String,
    previous: String,
    next: String,
    today: String,
    days: Vec<Option<String>>,
    compose_at: String,
}

#[cfg(any(feature = "server", test))]
fn calendar_month(month: Option<&str>, day: Option<&str>, today: NaiveDate) -> CalendarMonth {
    let first = month
        .and_then(|value| {
            NaiveDate::parse_from_str(&format!("{value}-01"), "%Y-%m-%d")
                .ok()
                .filter(|date| date.format("%Y-%m").to_string() == value)
        })
        .filter(|date| (1900..=9998).contains(&date.year()))
        .unwrap_or(today.with_day(1).unwrap_or(today));
    let previous = first
        .checked_sub_months(chrono::Months::new(1))
        .unwrap_or(first);
    let next = first
        .checked_add_months(chrono::Months::new(1))
        .unwrap_or(first);
    let mut days = vec![None; first.weekday().num_days_from_sunday() as usize];
    let mut date = first;
    while date < next {
        days.push(Some(date.to_string()));
        let Some(next_day) = date.succ_opt() else {
            break;
        };
        date = next_day;
    }
    while !days.len().is_multiple_of(7) {
        days.push(None);
    }
    let compose_at = day
        .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
        .filter(|date| *date >= first && *date < next)
        .map_or_else(String::new, |date| format!("{date}T09:00:00+00:00"));
    CalendarMonth {
        label: first.format("%B %Y").to_string(),
        month: first.format("%Y-%m").to_string(),
        previous: previous.format("%Y-%m").to_string(),
        next: next.format("%Y-%m").to_string(),
        today: today.to_string(),
        days,
        compose_at,
    }
}

fn calendar_href(page: &BlogCatalogPage, month: &str, day: Option<&str>) -> String {
    let mut url = format!("/app/social/calendar?month={month}");
    if let Some(brand) = page.brand_filter.as_ref() {
        let encoded =
            percent_encoding::utf8_percent_encode(brand, percent_encoding::NON_ALPHANUMERIC);
        url = format!("{url}&brand_key={encoded}");
    }
    if let Some(day) = day {
        url = format!("{url}&day={day}#compose-post");
    }
    url
}

fn post_utc_date(row: &BlogPostRow) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(&row.publish_at)
        .ok()
        .map(|date| date.with_timezone(&Utc).format("%Y-%m-%d").to_string())
}

fn post_utc_time(row: &BlogPostRow) -> String {
    chrono::DateTime::parse_from_rfc3339(&row.publish_at)
        .map(|date| date.with_timezone(&Utc).format("%H:%M UTC").to_string())
        .unwrap_or_default()
}

fn post_providers(page: &BlogCatalogPage, row: &BlogPostRow) -> String {
    let providers = page
        .social_accounts
        .iter()
        .filter(|account| row.selected_social_accounts.contains(&account.id))
        .map(|account| account.provider.as_str())
        .chain(row.social_links.iter().map(|link| link.provider.as_str()))
        .chain(
            page.publications
                .iter()
                .filter(|item| item.post_id == row.id)
                .map(|item| item.provider.as_str()),
        )
        .collect::<std::collections::BTreeSet<_>>();
    if providers.is_empty() {
        return "Website only".into();
    }
    providers
        .into_iter()
        .map(provider_label)
        .collect::<Vec<_>>()
        .join(" · ")
}

fn month_calendar(page: &BlogCatalogPage) -> Element {
    let calendar = &page.calendar;
    rsx! {
        section { class: "social-month", aria_label: "Monthly publishing calendar",
            header { class: "social-month__toolbar",
                h2 { "{calendar.label}" }
                nav { aria_label: "Calendar month",
                    a { href: calendar_href(page, &calendar.previous, None), aria_label: "Previous month", "← Previous" }
                    a { href: calendar_href(page, calendar.today.get(..7).unwrap_or(&calendar.month), None), "Today" }
                    a { href: calendar_href(page, &calendar.next, None), aria_label: "Next month", "Next →" }
                }
            }
            p { "All calendar dates and times are UTC. Choose a day to prepare a Post; publishing still requires approval." }
            div { class: "social-month__scroll", role: "region", aria_label: "Calendar days; scroll horizontally on narrow screens", tabindex: "0",
                div { class: "social-month__grid",
                    for weekday in ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"] {
                        div { class: "social-month__weekday", "{weekday}" }
                    }
                    for (index, day) in calendar.days.iter().enumerate() {
                        if let Some(day) = day {
                            section { key: "{day}", class: "social-month__day", aria_label: "{day}",
                                a { class: "social-month__date", href: calendar_href(page, &calendar.month, Some(day)),
                                    aria_label: "Prepare a Post for {day} UTC",
                                    time { datetime: "{day}", "{day.get(8..).unwrap_or(day)}" }
                                    if day == &calendar.today { " · Today" }
                                    span { " +" }
                                }
                                for row in page.rows.iter().filter(|row| post_utc_date(row).as_ref() == Some(day)) {
                                    details { class: "social-month__post", key: "{row.id}",
                                        summary {
                                            strong { "{row.title}" }
                                            span { "{row.brand} · {row.status}" }
                                            time { datetime: "{row.publish_at}", "{post_utc_time(row)}" }
                                            span { "{post_providers(page, row)}" }
                                        }
                                        {media_and_eligibility(row)}
                                        {social_links_list(row)}
                                        for publication in page.publications.iter().filter(|item| item.post_id == row.id) {
                                            p { "{provider_label(&publication.provider)}: {publication.state}" }
                                        }
                                        if row.status == "scheduled" {
                                            a { href: "#scheduled-post-{row.id}", "Review schedule" }
                                        } else {
                                            a { href: "/app/social", "Review publication" }
                                        }
                                    }
                                }
                            }
                        } else {
                            div { key: "blank-{index}", class: "social-month__blank", aria_hidden: "true" }
                        }
                    }
                }
            }
        }
    }
}

#[server]
#[allow(clippy::too_many_lines)]
pub async fn blog_catalog_page() -> Result<BlogCatalogPage, ServerFnError> {
    let role = crate::admin_listing::require_admin().await?;
    let surreal = consume_context::<store::surreal::SurrealDb>();
    let person = crate::admin_listing::injected_person_id()
        .await
        .ok_or_else(|| ServerFnError::new("admin access required"))?;
    let store_role = match role {
        ViewerRole::Owner => store::persons::Role::Owner,
        ViewerRole::Admin => store::persons::Role::Admin,
        _ => return Err(ServerFnError::new("admin access required")),
    };
    let brands = store::posts::manageable_brand_keys(&surreal, store_role, person)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?;
    let posts = store::posts::for_actor(&surreal, store_role, person)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?;
    let query =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Query<ErrorQuery>, _>()
            .await
            .ok()
            .map(|axum::extract::Query(query)| query)
            .unwrap_or_default();
    let brand_filter = query.brand_key.filter(|brand| brands.contains(brand));
    let uploaded_video = match query.video_asset.as_deref() {
        Some(key) => store::posts::find_video(&surreal, key)
            .await
            .map_err(|error| ServerFnError::new(error.to_string()))?
            .filter(|video| brands.contains(&video.brand_key))
            .map(|video| UploadedVideo {
                key: video.object_key,
                width: video.width,
                height: video.height,
                orientation: video.orientation,
            }),
        None => None,
    };
    let posts = posts
        .into_iter()
        .filter(|post| {
            brand_filter
                .as_ref()
                .is_none_or(|brand| brand == &post.brand_key)
        })
        .collect::<Vec<_>>();
    let social_accounts = store::social_accounts::for_actor(&surreal, store_role, person)
        .await
        .map_err(|error| ServerFnError::new(error.to_string()))?
        .into_iter()
        .filter(|account| account.status == "connected")
        .map(|account| SocialAccountChoice {
            id: account.id,
            provider_account_id: account.provider_account_id,
            brand: account.brand_key,
            provider: account.provider.as_str().to_string(),
            display_name: account.display_name,
        })
        .collect();
    let publication_rows = store::social_publications::for_posts(
        &surreal,
        &posts.iter().map(|post| post.id).collect::<Vec<_>>(),
    )
    .await
    .map_err(|error| ServerFnError::new(error.to_string()))?;
    let publications = publication_rows
        .into_iter()
        .map(|publication| SocialPublicationRow {
            post_id: publication.post_id.to_string(),
            provider: publication.provider,
            state: publication.state,
            provider_post_id: publication.provider_post_id,
            permalink: publication.permalink,
            last_error: publication.last_error,
            updated_at: publication.updated_at.to_rfc3339(),
        })
        .collect();
    let csrf_token = dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<crate::csrf::CsrfToken>,
        _,
    >()
    .await
    .map(|axum::Extension(token)| token.0)
    .unwrap_or_default();
    let calendar = calendar_month(
        query.month.as_deref(),
        query.day.as_deref(),
        Utc::now().date_naive(),
    );
    Ok(BlogCatalogPage {
        role,
        brands,
        rows: posts
            .into_iter()
            .map(|post| BlogPostRow {
                id: post.id.to_string(),
                brand: post.brand_key,
                slug: post.slug,
                title: post.title,
                status: post.status,
                publish_at: post
                    .publish_at
                    .map_or_else(|| "Immediately".into(), |at| at.to_rfc3339()),
                video_url: post
                    .video_asset
                    .as_deref()
                    .map(views::assets::bucket_asset_url),
                eligibility: ELIGIBILITY_PROVIDERS
                    .iter()
                    .map(|(provider, label)| ProviderEligibility {
                        provider: (*label).to_string(),
                        refusal: store::posts::provider_eligibility(
                            provider,
                            post.video_asset.as_deref(),
                            post.vertical_video,
                            post.video_orientation.as_deref(),
                        )
                        .err()
                        .map(str::to_string),
                    })
                    .collect(),
                vertical_video: post.vertical_video,
                tiktok_ai_generated: post.tiktok_ai_generated,
                video_orientation: post.video_orientation,
                video_asset: post.video_asset,
                facebook_caption: post.facebook_caption,
                instagram_caption: post.instagram_caption,
                threads_caption: post.threads_caption,
                youtube_caption: post.youtube_caption,
                selected_social_accounts: post.selected_social_accounts,
                social_links: post
                    .social_links
                    .into_iter()
                    .map(|link| SocialLinkRow {
                        provider: link.provider,
                        handle: link.account,
                        link: link.permalink,
                    })
                    .collect(),
            })
            .collect(),
        social_accounts,
        publications,
        csrf_token,
        error: query.error,
        uploaded_video,
        brand_filter,
        tokens_href: crate::app_chrome::app_tokens_href_from_context().await,
        firm_name: crate::app_chrome::firm_name_from_context().await,
        calendar,
    })
}

/// One line of the publishing schedule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleEntry {
    /// RFC 3339 in UTC, so entries sort by their text.
    pub when: String,
    pub brand: String,
    pub title: String,
    /// What is happening or happened: `Scheduled`, `Published on the site`,
    /// `Sent to Facebook`, and so on.
    pub what: String,
    pub link: Option<String>,
}

fn provider_label(provider: &str) -> String {
    ELIGIBILITY_LABELS
        .iter()
        .find(|(name, _)| *name == provider)
        .map_or_else(|| provider.to_string(), |(_, label)| (*label).to_string())
}

const ELIGIBILITY_LABELS: &[(&str, &str)] = &[
    ("facebook", "Facebook"),
    ("instagram", "Instagram"),
    ("threads", "Threads"),
    ("youtube", "YouTube"),
    ("linkedin", "LinkedIn"),
    ("x", "X"),
    ("tiktok", "TikTok for Business"),
];

/// The schedule split into what will go out (soonest first) and what went out
/// (most recent first). It reads only the page's rows and publication states.
#[must_use]
pub fn schedule_entries(page: &BlogCatalogPage) -> (Vec<ScheduleEntry>, Vec<ScheduleEntry>) {
    let mut upcoming = Vec::new();
    let mut sent = Vec::new();
    for row in &page.rows {
        let entry = |when: &str, what: String, link: Option<String>| ScheduleEntry {
            when: when.to_string(),
            brand: row.brand.clone(),
            title: row.title.clone(),
            what,
            link,
        };
        match row.status.as_str() {
            "scheduled" => upcoming.push(entry(&row.publish_at, "Scheduled".into(), None)),
            "published" => sent.push(entry(&row.publish_at, "Published on the site".into(), None)),
            _ => {}
        }
        for publication in page
            .publications
            .iter()
            .filter(|item| item.post_id == row.id)
        {
            let provider = provider_label(&publication.provider);
            let what = match publication.state.as_str() {
                "confirmed" => format!("Sent to {provider}"),
                "sending" => format!("Sending to {provider}"),
                "failed" => format!("Could not send to {provider}"),
                "unknown" => format!("{provider}: outcome unknown, needs reconciliation"),
                other => format!("{provider}: {other}"),
            };
            sent.push(entry(
                &publication.updated_at,
                what,
                publication.permalink.clone(),
            ));
        }
    }
    upcoming.sort_by(|a, b| a.when.cmp(&b.when).then_with(|| a.title.cmp(&b.title)));
    sent.sort_by(|a, b| b.when.cmp(&a.when).then_with(|| a.title.cmp(&b.title)));
    (upcoming, sent)
}

fn schedule_table(
    label: &'static str,
    entries: Vec<ScheduleEntry>,
    empty: &'static str,
) -> Element {
    rsx! {
        if entries.is_empty() {
            p { "{empty}" }
        } else {
            table { class: "schedule-table",
                caption { "{label}" }
                thead { tr { th { scope: "col", "When" } th { scope: "col", "Brand" }
                    th { scope: "col", "Post" } th { scope: "col", "What" } } }
                tbody {
                    for (index, entry) in entries.into_iter().enumerate() {
                        tr { key: "{index}",
                            td { time { datetime: "{entry.when}", "{entry.when}" } }
                            td { "{entry.brand}" }
                            td { "{entry.title}" }
                            td { "{entry.what}"
                                if let Some(link) = entry.link.as_ref() { " · " a { href: "{link}", "Open post" } }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The publishing schedule shown at the top of the social pages: what will go
/// out, and what went out. Sending itself is a durable workflow; this only
/// reads the outcome tables.
fn schedule_section(page: &BlogCatalogPage) -> Element {
    let (upcoming, sent) = schedule_entries(page);
    rsx! {
        section { id: "publishing-schedule", "aria-labelledby": "publishing-schedule-heading",
            h2 { id: "publishing-schedule-heading", "Publishing schedule" }
            p { "Times are UTC." }
            {schedule_table("Going out", upcoming, "Nothing is scheduled.")}
            {schedule_table("Went out", sent, "Nothing has gone out yet.")}
        }
    }
}

/// A Post's provider links, with the handle each one names.
fn social_links_list(row: &BlogPostRow) -> Element {
    rsx! {
        if !row.social_links.is_empty() {
            ul { class: "social-links", "aria-label": "Social links",
                for link in row.social_links.iter() {
                    li {
                        "{provider_label(&link.provider)} (@{link.handle}): "
                        if let Some(href) = link.link.as_ref() {
                            a { href: "{href}", "Open post" }
                        } else {
                            "link pending"
                        }
                    }
                }
            }
        }
    }
}

/// The Post's video, its declared and measured orientation, and each
/// provider's eligibility. Eligibility explains; it never selects an account.
fn media_and_eligibility(row: &BlogPostRow) -> Element {
    let declared = if row.vertical_video {
        "vertical"
    } else {
        "not vertical"
    };
    let measured = row
        .video_orientation
        .as_deref()
        .unwrap_or("not measured here");
    rsx! {
        if let Some(video) = row.video_url.as_ref() {
            video { controls: true, preload: "metadata", src: "{video}" }
            p { "Video: declared {declared}; measured {measured}." }
        } else {
            p { "Text-only Post." }
        }
        ul { class: "social-eligibility", "aria-label": "Provider eligibility",
            for item in row.eligibility.iter() {
                li {
                    if let Some(refusal) = item.refusal.as_ref() {
                        "{item.provider}: not eligible, because {refusal}."
                    } else {
                        "{item.provider}: eligible when selected."
                    }
                }
            }
        }
    }
}

#[component]
pub fn SocialDashboard() -> Element {
    let resource = use_server_future(blog_catalog_page)?;
    let Some(Ok(page)) = &*resource.read() else {
        return rsx! { main { id: "social-dashboard", p { "Loading social publishing…" } } };
    };
    social_dashboard_body(page)
}

fn social_dashboard_body(page: &BlogCatalogPage) -> Element {
    let rows = page.rows.iter().map(|row| {
        rsx! {
            article { class: "team-home__card", key: "{row.id}",
                h2 { "{row.title}" }
                p { "Brand: {row.brand}. Status: {row.status}. {row.publish_at}." }
                {media_and_eligibility(row)}
                {social_links_list(row)}
                if row.selected_social_accounts.is_empty() {
                    p { "No social accounts selected." }
                } else {
                    p { "Selected channels: {row.selected_social_accounts.len()}" }
                }
                p { "Facebook: {row.facebook_caption.as_deref().unwrap_or(\"No caption\")}" }
                p { "Instagram: {row.instagram_caption.as_deref().unwrap_or(\"No caption\")}" }
                p { "Threads: {row.threads_caption.as_deref().unwrap_or(\"No caption\")}" }
                p { "YouTube: {row.youtube_caption.as_deref().unwrap_or(\"No caption\")}" }
                p { "TikTok AI disclosure: {disclosure_label(row.tiktok_ai_generated)}" }
                for publication in page.publications.iter().filter(|item| item.post_id == row.id) {
                    p { "{publication.provider}: {publication.state}. "
                        if let Some(link) = publication.permalink.as_ref() { a { href: "{link}", "Open post" } }
                        if let Some(provider_id) = publication.provider_post_id.as_ref() { " · Provider ID: {provider_id}" }
                        if let Some(_error) = publication.last_error.as_ref() {
                            " · Details are available to an authorized administrator."
                        }
                    }
                }
            }
        }
    });
    rsx! {
        document::Title { "{page.firm_name} | Social" }
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{page.tokens_href}" }
        crate::components::AppNavbar { destinations: crate::app_chrome::app_destinations(page.role), logo: None }
        main { id: "social-dashboard", class: "nav-theme",
            header { class: "page-header", h1 { "Social publishing" },
                p { "Review Brand content and publication status." }
                p { a { href: "/app/social/calendar", "Open calendar" } " · "
                    a { href: "/app/admin/blog", "Manage Blog Posts" } }
            }
            {schedule_section(page)}
            crate::lead_sources::LeadSourcesSection {}
            div { class: "team-home__cards", "aria-label": "Brand publications", {rows} }
        }
    }
}

#[component]
pub fn SocialCalendar() -> Element {
    let resource = use_server_future(blog_catalog_page)?;
    let Some(Ok(page)) = &*resource.read() else {
        return rsx! { main { id: "social-calendar", p { "Loading publishing calendar…" } } };
    };
    social_calendar_body(page)
}

fn social_calendar_body(page: &BlogCatalogPage) -> Element {
    let rows = page
        .rows
        .iter()
        .filter(|row| row.status == "scheduled")
        .map(|row| {
            let cancel_action = format!("/app/social/calendar/{}/cancel", row.id);
            let cancel_token = page.csrf_token.clone();
            let title = row.title.clone();
            let brand = row.brand.clone();
            let publish_at = row.publish_at.clone();
            rsx! {
                article { id: "scheduled-post-{row.id}", class: "team-home__card", key: "{row.id}",
                    h2 { "{title}" }
                    p { "Brand: {brand}. Scheduled: {publish_at}." }
                    {media_and_eligibility(row)}
                    FormCard {
                        title: "Cancel {title} schedule".to_string(),
                        action: cancel_action,
                        submit_label: "Cancel schedule".to_string(),
                        heading: Heading::H2,
                        csrf_token: Some(cancel_token),
                        fields: vec![],
                    }
                }
            }
        });
    rsx! {
        document::Title { "{page.firm_name} | Social calendar" }
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{page.tokens_href}" }
        crate::components::AppNavbar { destinations: crate::app_chrome::app_destinations(page.role), logo: None }
        main { id: "social-calendar", class: "nav-theme",
            header { class: "page-header", h1 { "Publishing calendar" },
                p { "When Brand content goes out and when it went out. Cancel holds a scheduled Post before it is sent." }
                nav { aria_label: "Filter calendar by Brand",
                    a { href: "/app/social/calendar?month={page.calendar.month}", "All Brands" }
                    for brand in page.brands.iter() {
                        " · " a { href: "/app/social/calendar?month={page.calendar.month}&brand_key={brand}", "{brand}" }
                    }
                }
                p { a { href: "/app/social", "Back to social" } }
            }
            if let Some(error) = page.error.as_ref() { p { class: "page-subtitle", "{error}" } }
            {month_calendar(page)}
            {schedule_section(page)}
            {schedule_post_form(page)}
            div { class: "team-home__cards", "aria-label": "Scheduled publications", {rows} }
        }
    }
}

/// Render retained schedule copy with the same controls as the composer.
#[cfg(feature = "server")]
pub fn schedule_refusal_html(
    brand: &str,
    csrf_token: &str,
    message: &str,
    fields: Vec<Field>,
) -> String {
    let content = dioxus_ssr::render_element(rsx! {
        main { class: "nav-theme",
            h1 { "Review Post schedule" }
            p { role: "alert", "{message}" }
            p { a { href: "/app/social/calendar", "Review calendar" } }
            p { a { href: "/app/admin/brands/{brand}/edit", target: "_blank", rel: "noopener", "Connect or review social accounts in a new tab" } }
            FormCard {
                title: "Review Post schedule".to_string(),
                action: "/app/api/blog/schedule".to_string(),
                submit_label: "Approve and schedule".to_string(),
                heading: Heading::H2,
                csrf_token: Some(csrf_token.to_string()),
                fields,
            }
        }
    });
    format!("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>Review Post schedule</title><link rel=\"stylesheet\" href=\"{}\"></head><body>{content}</body></html>", crate::components::THEME_STYLESHEET_HREF)
}

fn schedule_post_form(page: &BlogCatalogPage) -> Element {
    let brands = page.brands.iter().filter(|brand| {
        page.brand_filter
            .as_ref()
            .is_none_or(|selected| selected == *brand)
    });
    rsx! {
        section { id: "compose-post", aria_label: "Schedule a Post",
            h2 { "Schedule a Post" }
            p { "Choose connected destinations and approve the copy before scheduling. A blank time queues publication now. Times must include the UTC offset." }
            p { "Provider captions are optional and preserve longer copy separately from the website description. TikTok, LinkedIn and X currently use the website description as their caption." }
            p { a { href: "/app/inbox", "Answer comments and questions in the inbox" }
                ". Facebook and Instagram comments and messages require configured provider permissions and verified webhook subscriptions. TikTok public comment replies require comment permissions and a verified webhook subscription." }
            for brand in brands {
                p { a { href: "/app/admin/brands/{brand}/edit", "Connect or review {brand} social accounts" } }
                {
                    let mut fields = vec![
                        Field::select("Brand", "brand", vec![Choice::new(brand, brand)], Some(brand.clone())).required(),
                        Field::text("Slug", "slug", "").required(),
                        Field::text("Title", "title", "").required(),
                        Field::text("Website description", "description", "").help("Maximum 200 characters. Used as the social caption when no provider caption is supplied.").required(),
                        Field::textarea("Facebook caption", "facebook_caption", "", 4),
                        Field::textarea("Instagram caption", "instagram_caption", "", 4),
                        Field::textarea("Threads caption", "threads_caption", "", 4),
                        Field::textarea("YouTube caption", "youtube_caption", "", 4),
                        Field::textarea("Website Post", "body_markdown", "", 5).required(),
                        Field::text("Publication time (with UTC offset)", "publish_at", &page.calendar.compose_at).help("For example: 2026-10-19T09:00:00-05:00. Leave blank to publish now."),
                        Field::text("Uploaded video asset", "video_asset", page.uploaded_video.as_ref().filter(|video| video.key.starts_with(&format!("img/{brand}/"))).map_or("", |video| video.key.as_str())),
                        Field::select("AI-generated video", "tiktok_ai_generated", vec![Choice::new("", "Choose for TikTok"), Choice::new("false", "No"), Choice::new("true", "Yes")], None),
                    ];
                    for provider in ["facebook", "instagram", "threads", "youtube", "tiktok", "linkedin", "x"] {
                        let mut choices = vec![Choice::new("", "Do not publish here")];
                        choices.extend(page.social_accounts.iter().filter(|account| account.brand == *brand && account.provider == provider).map(|account| Choice::new(&account.provider_account_id, account.display_name.as_deref().unwrap_or(&account.provider_account_id))));
                        if choices.len() > 1 {
                            fields.push(Field::select(format!("{provider} destination"), provider, choices, None));
                        }
                    }
                    rsx! { FormCard {
                        title: format!("Schedule for {brand}"),
                        action: "/app/api/blog/schedule".to_string(),
                        submit_label: "Approve and schedule".to_string(),
                        heading: Heading::H2,
                        csrf_token: Some(page.csrf_token.clone()),
                        fields: fields.into_iter().enumerate().map(|(index, field)| field.id(format!("schedule-{brand}-{index}"))).collect::<Vec<_>>(),
                    } }
                }
            }
        }
    }
}

#[derive(Deserialize, Default)]
pub struct ErrorQuery {
    pub error: Option<String>,
    pub video_asset: Option<String>,
    pub brand_key: Option<String>,
    pub month: Option<String>,
    pub day: Option<String>,
}

#[component]
pub fn BrandBlogCatalog() -> Element {
    let resource = use_server_future(blog_catalog_page)?;
    let Some(Ok(page)) = &*resource.read() else {
        return rsx! { main { id: "brand-blog-catalog", p { "Loading content catalog…" } } };
    };
    brand_blog_catalog_body(page)
}

fn brand_blog_catalog_body(page: &BlogCatalogPage) -> Element {
    let upload_brand_choices: Vec<Choice> = page
        .brands
        .iter()
        .map(|brand| Choice::new(brand.clone(), brand.clone()))
        .collect();
    let rows = page.rows.iter().map(|row| rsx! {
        article { class: "team-home__card", key: "{row.brand}-{row.slug}",
            h2 { "{row.title}" }
            p { "Brand: {row.brand}. Slug: {row.slug}. Status: {row.status}. Publish: {row.publish_at}." }
            {media_and_eligibility(row)}
            {social_links_list(row)}
            p { "Facebook preview: {row.facebook_caption.as_deref().unwrap_or(\"No caption\")}" }
            p { "Instagram preview: {row.instagram_caption.as_deref().unwrap_or(\"No caption\")}" }
            p { "Threads preview: {row.threads_caption.as_deref().unwrap_or(\"No caption\")}" }
            p { "YouTube preview: {row.youtube_caption.as_deref().unwrap_or(\"No caption\")}" }
            p { "TikTok AI disclosure: {disclosure_label(row.tiktok_ai_generated)}" }
            if !row.selected_social_accounts.is_empty() {
                p { "Selected accounts: "
                    for id in row.selected_social_accounts.iter() {
                        if let Some(account) = page.social_accounts.iter().find(|account| &account.id == id) {
                            "{account.brand} {account.provider} · {account.display_name.as_deref().unwrap_or(\"Connected account\")}. "
                        }
                    }
                }
            }
            for publication in page.publications.iter().filter(|item| item.post_id == row.id) {
                p { "{publication.provider}: {publication.state}. "
                    if let Some(link) = publication.permalink.as_ref() { a { href: "{link}", "Open post" } }
                    if let Some(provider_id) = publication.provider_post_id.as_ref() { " · Provider ID: {provider_id}" }
                    if let Some(error) = publication.last_error.as_ref() { " · {error}" }
                }
            }
        }
    });
    rsx! {
        document::Title { "{page.firm_name} | Blog content" }
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{page.tokens_href}" }
        crate::components::AppNavbar { destinations: crate::app_chrome::app_destinations(page.role), logo: None }
        main { id: "brand-blog-catalog", class: "nav-theme",
            header { class: "page-header", h1 { "Blog content" },
                p { "Posts for the Brands you manage, as stored in the database." }
            }
            if let Some(error) = page.error.as_ref() { p { class: "page-subtitle", "{error}" } }
            {schedule_section(page)}
            {schedule_post_form(page)}
            section { "aria-label": "Social links",
                h2 { "Social links" }
                p { "A Post may link to a social account only after the Brand's handle is recorded with "
                    code { "navigator site blog handle" } ". Encode the link once the post exists with "
                    code { "navigator site blog link" } "." }
            }
            FormCard {
                title: "Upload a Brand video".to_string(),
                action: "/app/admin/blog/video".to_string(),
                submit_label: "Upload video".to_string(),
                heading: Heading::H2,
                multipart: true,
                csrf_token: Some(page.csrf_token.clone()),
                fields: vec![
                    Field::select("Brand", "brand_key", upload_brand_choices, None).required(),
                    Field::file("Original video (MP4, up to 64 MiB)", "file").required(),
                ],
            }
            if let Some(video) = page.uploaded_video.as_ref() {
                section { "aria-label": "Uploaded video",
                    p { "Uploaded {video.width}×{video.height}, measured {video.orientation}. Use on a Post:" }
                    pre { code {
                        "video_asset: {video.key}\nvertical_video: {video.orientation == \"vertical\"}"
                    } }
                }
            }
            div { class: "team-home__cards", "aria-label": "Blog Posts", {rows} }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::too_many_lines)]
    fn page() -> BlogCatalogPage {
        BlogCatalogPage {
            role: ViewerRole::Owner,
            brands: vec!["neon".into(), "vesta".into()],
            rows: vec![
                BlogPostRow {
                    id: "post-1".into(),
                    brand: "neon".into(),
                    slug: "welcome".into(),
                    title: "A welcome post".into(),
                    status: "scheduled".into(),
                    publish_at: "2026-10-12T09:00:00-07:00".into(),
                    video_asset: Some("img/neon/social/intro.mp4".into()),
                    video_url: Some("https://assets.example.test/intro.mp4".into()),
                    vertical_video: false,
                    tiktok_ai_generated: None,
                    video_orientation: Some("landscape".into()),
                    eligibility: vec![
                        ProviderEligibility {
                            provider: "Facebook".into(),
                            refusal: None,
                        },
                        ProviderEligibility {
                            provider: "Instagram".into(),
                            refusal: Some("it needs vertical_video: true".into()),
                        },
                    ],
                    facebook_caption: Some("Facebook copy".into()),
                    instagram_caption: Some("Instagram copy".into()),
                    threads_caption: Some("Threads copy".into()),
                    youtube_caption: Some("YouTube copy".into()),
                    selected_social_accounts: vec!["account-fb".into(), "missing".into()],
                    social_links: vec![SocialLinkRow {
                        provider: "x".into(),
                        handle: "NeonLawUSA".into(),
                        link: None,
                    }],
                },
                BlogPostRow {
                    id: "post-2".into(),
                    brand: "vesta".into(),
                    slug: "draft".into(),
                    title: "A draft".into(),
                    status: "draft".into(),
                    publish_at: "Immediately".into(),
                    video_asset: None,
                    video_url: None,
                    vertical_video: false,
                    tiktok_ai_generated: None,
                    video_orientation: None,
                    eligibility: vec![ProviderEligibility {
                        provider: "TikTok".into(),
                        refusal: Some("it needs a video".into()),
                    }],
                    facebook_caption: None,
                    instagram_caption: None,
                    threads_caption: None,
                    youtube_caption: None,
                    selected_social_accounts: vec![],
                    social_links: vec![],
                },
            ],
            social_accounts: vec![
                SocialAccountChoice {
                    id: "account-fb".into(),
                    provider_account_id: "synthetic-fb".into(),
                    brand: "neon".into(),
                    provider: "facebook".into(),
                    display_name: Some("Neon Page".into()),
                },
                SocialAccountChoice {
                    id: "account-ig".into(),
                    provider_account_id: "synthetic-ig".into(),
                    brand: "vesta".into(),
                    provider: "instagram".into(),
                    display_name: None,
                },
            ],
            publications: vec![
                SocialPublicationRow {
                    post_id: "post-1".into(),
                    provider: "facebook".into(),
                    state: "confirmed".into(),
                    provider_post_id: Some("remote-123".into()),
                    permalink: Some("https://social.example.test/remote-123".into()),
                    last_error: None,
                    updated_at: "2026-10-01T16:00:00+00:00".into(),
                },
                SocialPublicationRow {
                    post_id: "post-1".into(),
                    provider: "instagram".into(),
                    state: "failed".into(),
                    provider_post_id: None,
                    permalink: None,
                    last_error: Some("The provider could not publish this post.".into()),
                    updated_at: "2026-10-02T16:00:00+00:00".into(),
                },
            ],
            csrf_token: "csrf-example".into(),
            error: Some("Review the scheduled publication.".into()),
            uploaded_video: Some(UploadedVideo {
                key: "img/neon/social/portrait.mp4".into(),
                width: 1080,
                height: 1920,
                orientation: "vertical".into(),
            }),
            brand_filter: Some("neon".into()),
            tokens_href: "/assets/brand.css".into(),
            firm_name: "Neon Law".into(),
            calendar: calendar_month(
                Some("2026-10"),
                None,
                NaiveDate::from_ymd_opt(2026, 10, 28).unwrap(),
            ),
        }
    }

    #[test]
    fn calendar_query_and_catalog_remain_compatible_with_older_fields() {
        let query: ErrorQuery = serde_json::from_str(r#"{"brand_key":"neon"}"#).unwrap();
        assert!(query.month.is_none());
        assert!(query.day.is_none());
        let mut payload = serde_json::to_value(page()).unwrap();
        payload.as_object_mut().unwrap().remove("calendar");
        let old: BlogCatalogPage = serde_json::from_value(payload).unwrap();
        let html = dioxus_ssr::render_element(month_calendar(&old));
        assert!(html.contains("Monthly publishing calendar"));
    }

    #[test]
    fn month_grid_handles_leap_days_and_year_boundaries() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 28).unwrap();
        let leap = calendar_month(Some("2028-02"), Some("2028-02-29"), today);
        assert_eq!(leap.days.iter().flatten().count(), 29);
        assert_eq!(leap.days.len() % 7, 0);
        assert_eq!(leap.days[2].as_deref(), Some("2028-02-01"));
        assert_eq!(leap.compose_at, "2028-02-29T09:00:00+00:00");
        let january = calendar_month(Some("2027-01"), Some("2027-02-01"), today);
        assert_eq!(january.previous, "2026-12");
        assert_eq!(january.next, "2027-02");
        assert_eq!(calendar_month(Some("2026-1"), None, today).month, "2026-10");
        assert!(january.compose_at.is_empty());
        assert_eq!(
            calendar_month(Some("invalid"), None, today).month,
            "2026-10"
        );
    }

    #[test]
    fn calendar_groups_posts_by_utc_day_and_day_links_preserve_brand() {
        let mut calendar = page();
        calendar.rows[0].publish_at = "2026-10-12T23:30:00-07:00".into();
        assert_eq!(
            post_utc_date(&calendar.rows[0]).as_deref(),
            Some("2026-10-13")
        );
        let html = dioxus_ssr::render_element(month_calendar(&calendar));
        for expected in [
            "October 2026",
            "Previous month",
            "Next month",
            "Today",
            "month=2026-11",
            "brand_key=neon",
            "day=2026-10-13#compose-post",
            "A welcome post",
            "06:30 UTC",
            "Facebook",
            "Instagram: failed",
        ] {
            assert!(html.contains(expected), "missing {expected}: {html}");
        }
        assert!(!html.contains("A draft"));
        assert!(
            !html.contains("<form"),
            "browsing the month never schedules a Post"
        );
    }

    #[test]
    fn choosing_a_day_prefills_explicit_utc_time_without_selecting_providers() {
        let mut calendar = page();
        calendar.calendar = calendar_month(
            Some("2026-11"),
            Some("2026-11-15"),
            NaiveDate::from_ymd_opt(2026, 10, 28).unwrap(),
        );
        let html = dioxus_ssr::render_element(schedule_post_form(&calendar));
        assert!(html.contains("2026-11-15T09:00:00+00:00"), "{html}");
        assert!(html.contains("Approve and schedule"));
        assert!(html.contains("Do not publish here"));
        assert!(html.contains("name=\"_csrf\""));
    }

    #[test]
    fn website_schedule_respects_the_active_brand_filter() {
        let mut filtered = page();
        filtered.brand_filter = Some("vesta".into());
        let html = dioxus_ssr::render_element(schedule_post_form(&filtered));
        crate::components::assert_forms_accessible(&html, "filtered schedule_post_form");
        assert!(html.contains("Schedule for vesta"), "{html}");
        assert!(html.contains("synthetic-ig"), "{html}");
        assert!(!html.contains("Schedule for neon"), "{html}");
        assert!(!html.contains("synthetic-fb"), "{html}");
        assert!(
            !html.contains("Connect or review neon social accounts"),
            "{html}"
        );
    }

    #[test]
    fn website_schedule_requires_explicit_brand_destinations_and_disclosure() {
        let mut unfiltered = page();
        unfiltered.brand_filter = None;
        let html = dioxus_ssr::render_element(schedule_post_form(&unfiltered));
        crate::components::assert_forms_accessible(&html, "schedule_post_form");
        assert!(html.contains("/app/api/blog/schedule"), "{html}");
        assert!(html.contains("name=\"_csrf\""), "{html}");
        assert!(html.contains("name=\"publish_at\""), "{html}");
        assert!(html.contains("name=\"tiktok_ai_generated\""), "{html}");
        assert!(html.contains("synthetic-fb"), "{html}");
        let neon_form = html
            .split("aria-label=\"Schedule for neon\"")
            .nth(1)
            .unwrap()
            .split("</form>")
            .next()
            .unwrap();
        assert!(neon_form.contains("synthetic-fb"), "{html}");
        assert!(!neon_form.contains("synthetic-ig"), "{html}");
        let vesta_form = html
            .split("aria-label=\"Schedule for vesta\"")
            .nth(1)
            .unwrap()
            .split("</form>")
            .next()
            .unwrap();
        assert!(vesta_form.contains("synthetic-ig"), "{html}");
        assert!(!vesta_form.contains("synthetic-fb"), "{html}");
        assert!(html.contains("Do not publish here"), "{html}");
        assert!(html.contains("Approve and schedule"), "{html}");
        assert!(html.contains("/app/inbox"), "{html}");
    }

    #[test]
    fn social_dashboard_shows_brand_media_copy_and_publication_outcomes() {
        let html = dioxus_ssr::render_element(social_dashboard_body(&page()));
        for expected in [
            "Social publishing",
            "Review Brand content and publication status.",
            "A welcome post",
            "Brand: neon. Status: scheduled.",
            "https://assets.example.test/intro.mp4",
            "Video: declared not vertical; measured landscape.",
            "Instagram: not eligible, because it needs vertical_video: true.",
            "Facebook: eligible when selected.",
            "Text-only Post.",
            "TikTok: not eligible, because it needs a video.",
            "Selected channels: 2",
            "Facebook: Facebook copy",
            "Instagram: Instagram copy",
            "Threads: Threads copy",
            "YouTube: YouTube copy",
            "confirmed",
            "https://social.example.test/remote-123",
            "Provider ID: remote-123",
            "Details are available to an authorized administrator.",
            "No social accounts selected.",
            "No caption",
        ] {
            assert!(html.contains(expected), "missing {expected:?}: {html}");
        }
        assert!(html.contains("Open calendar"), "{html}");
        assert!(html.contains("Manage Blog Posts"), "{html}");
    }

    #[test]
    fn social_calendar_shows_scheduled_media_and_only_the_cancel_hold() {
        let html = dioxus_ssr::render_element(social_calendar_body(&page()));
        assert!(html.contains("Publishing calendar"), "{html}");
        assert!(html.contains("Review the scheduled publication."), "{html}");
        assert!(html.contains("A welcome post"), "{html}");
        assert!(!html.contains("A draft"), "{html}");
        assert!(
            html.contains("Video: declared not vertical; measured landscape."),
            "{html}"
        );
        assert!(
            html.contains("Instagram: not eligible, because it needs vertical_video: true."),
            "{html}"
        );
        assert!(
            html.contains("/app/social/calendar/post-1/cancel"),
            "{html}"
        );
        assert!(!html.contains("reschedule"), "{html}");
        assert!(!html.contains("Markdown source"), "{html}");
        assert!(html.contains("name=\"_csrf\""), "{html}");
        assert!(html.contains("All Brands"), "{html}");
        assert!(html.contains("brand_key=neon"), "{html}");
        assert!(html.contains("brand_key=vesta"), "{html}");
        assert!(html.contains("Back to social"), "{html}");
        for field in [
            "facebook_caption",
            "instagram_caption",
            "threads_caption",
            "youtube_caption",
        ] {
            assert!(html.contains(&format!("name=\"{field}\"")), "{html}");
        }
        assert!(html.contains("Maximum 200 characters"), "{html}");
    }

    #[test]
    fn blog_catalog_is_a_projection_with_an_upload_and_its_frontmatter() {
        let html = dioxus_ssr::render_element(brand_blog_catalog_body(&page()));
        for expected in [
            "Blog content",
            "Review the scheduled publication.",
            "stored in the database",
            "navigator site blog handle",
            "navigator site blog link",
            "NeonLawUSA",
            "link pending",
            "Upload a Brand video",
            "Original video (MP4, up to 64 MiB)",
            "Uploaded 1080×1920, measured vertical.",
            "video_asset: img/neon/social/portrait.mp4",
            "vertical_video: true",
            "A welcome post",
            "Instagram: not eligible, because it needs vertical_video: true.",
            "Open post",
            "remote-123",
            "The provider could not publish this post.",
        ] {
            assert!(html.contains(expected), "missing {expected:?}: {html}");
        }
        assert!(!html.contains("Create a Blog Post"), "{html}");
        assert!(!html.contains("body_html"), "{html}");
        assert!(html.contains("name=\"_csrf\""), "{html}");
        assert!(html.contains("multipart/form-data"), "{html}");
    }

    #[test]
    fn the_schedule_precedes_the_detailed_cards_on_every_social_page() {
        for html in [
            dioxus_ssr::render_element(social_dashboard_body(&page())),
            dioxus_ssr::render_element(social_calendar_body(&page())),
            dioxus_ssr::render_element(brand_blog_catalog_body(&page())),
        ] {
            let schedule = html.find("Publishing schedule").expect("schedule heading");
            let first_card = html.find("team-home__cards").expect("post cards");
            assert!(
                schedule < first_card,
                "the schedule precedes the cards: {html}"
            );
            assert!(html.contains("Going out"), "{html}");
            assert!(html.contains("Went out"), "{html}");
        }
    }

    #[test]
    fn the_schedule_orders_upcoming_soonest_first_and_sent_most_recent_first() {
        let (upcoming, sent) = schedule_entries(&page());
        assert_eq!(upcoming.len(), 1);
        assert_eq!(upcoming[0].title, "A welcome post");
        assert_eq!(upcoming[0].what, "Scheduled");
        let whats: Vec<&str> = sent.iter().map(|entry| entry.what.as_str()).collect();
        assert_eq!(
            whats,
            vec!["Could not send to Instagram", "Sent to Facebook"]
        );
    }
}
