//! Estate planning and custom trust administration, with a shared contact path.
use super::sibling_practice::{SiblingPractice, SiblingPractices};
use super::HomeContent;
use crate::components::aster::{AsterArtwork, AsterPose};
use crate::components::is_external_href;
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

/// Copy loaded from the brand home catalog.
///
/// This mirrors `views::locales::EstateCopy` rather than naming it, for the
/// same reason [`super::CompanyContent`] does: `views` is gated behind the
/// `server` feature, and this type is a field of [`HomeContent`], which the
/// wasm client build compiles. `neon::locales::home` maps the catalog shape
/// onto this one at the one seam that already reads the YAML.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EstateContent {
    pub eyebrow: String,
    pub process_link: String,
    pub plan_label: String,
    pub plan_description: String,
    pub features: Vec<String>,
    pub fee_note: String,
    pub nevada_label: String,
    pub nevada_heading: String,
    pub nevada_body: String,
    pub nevada_points: Vec<[String; 2]>,
    pub nevada_note: String,
    pub nevada_details_label: String,
    pub administration_label: String,
    pub administration_heading: String,
    pub administration_body: String,
    pub administration_services: Vec<[String; 2]>,
    pub administration_fee_label: String,
    pub administration_fee_note: String,
    pub administration_contact_label: String,
    pub administration_details_label: String,
    pub video_updates_label: String,
    pub video_updates_body: String,
    pub video_updates_blog_label: String,
    pub testimonials_label: String,
    pub video_label: String,
    pub video_src: String,
    pub photos: Vec<[String; 2]>,
    pub astrology_label: String,
    pub astrology_heading: String,
    pub astrology_body: String,
    pub cosmos_label: String,
    pub cosmos_note: String,
    pub aster_name: String,
    pub aster_image: String,
    pub aster_description: String,
    pub cosmos_themes: Vec<[String; 4]>,
    pub probate_label: String,
    pub probate_heading: String,
    pub probate_body: String,
    pub probate_details_label: String,
    pub process_label: String,
    pub process_heading: String,
    pub steps: Vec<[String; 2]>,
    pub record_label: String,
    pub record_unit: String,
    pub record_status: String,
    pub record_heading: String,
    pub record_body: String,
    pub record_note: String,
    pub closing_heading: String,
    pub closing_body: String,
    /// Sibling-practice cards, resolved for this deployment.
    #[serde(default)]
    pub sibling_practices: Vec<SiblingPractice>,
}

#[component]
pub(super) fn EstateHome(
    content: HomeContent,
    estate: EstateContent,
    #[props(default)] video_posts: Vec<super::HomeVideoPost>,
    #[props(default)] testimonials: Vec<crate::components::TestimonialCard>,
) -> Element {
    let contact_external = is_external_href(&content.contact_href);
    rsx! {
        div { class: "vesta-home",
            section { class: "vesta-hero", "aria-labelledby": "vesta-title",
                div {
                    p { class: "vesta-eyebrow", "{estate.eyebrow}" }
                    h1 { id: "vesta-title", "{content.heading}" }
                    p { class: "vesta-lead", "{content.lead}" }
                    div { class: "vesta-actions",
                        a {
                            class: "nav-btn nav-btn--primary",
                            href: "{content.contact_href}",
                            target: if contact_external { Some("_blank") } else { None },
                            rel: if contact_external { Some("noopener noreferrer") } else { None },
                            "{content.contact_label}"
                        }
                        a { class: "vesta-text-link", href: "#estate-plan", "{estate.process_link}" }
                        a { class: "vesta-text-link", href: "#astrology", "{estate.astrology_label}" }
                    }
                }
                div { class: "vesta-cosmos", "aria-labelledby": "vesta-cosmos-label",
                    p { id: "vesta-cosmos-label", class: "vesta-eyebrow", "{estate.cosmos_label}" }
                    div { class: "vesta-orrery", "aria-hidden": "true",
                        div { class: "vesta-orbit vesta-orbit--outer", span { class: "vesta-planet vesta-planet--jupiter" } }
                        div { class: "vesta-orbit vesta-orbit--middle", span { class: "vesta-planet vesta-planet--mars" } }
                        div { class: "vesta-orbit vesta-orbit--inner", span { class: "vesta-planet vesta-planet--venus" } }
                        AsterArtwork { class: "vesta-aster vesta-aster--hero", src: estate.aster_image.clone(), pose: AsterPose::Standing }
                        span { class: "vesta-zodiac vesta-zodiac--north", "♋" }
                        span { class: "vesta-zodiac vesta-zodiac--east", "♎" }
                        span { class: "vesta-zodiac vesta-zodiac--south", "♑" }
                        span { class: "vesta-zodiac vesta-zodiac--west", "♈" }
                    }
                    p { class: "vesta-aster__name", "{estate.aster_name}" }
                    p { class: "vesta-cosmos-note", "{estate.aster_description}" }
                    p { class: "vesta-cosmos-note vesta-cosmos-note--sky", "{estate.cosmos_note}" }
                }
            }
            section { id: "astrology", class: "vesta-reflection", "aria-labelledby": "vesta-astrology-title",
                AsterArtwork { class: "vesta-aster vesta-aster--illustrate", src: estate.aster_image.clone(), pose: AsterPose::Illustrate }
                p { class: "vesta-eyebrow", "{estate.astrology_label}" }
                h2 { id: "vesta-astrology-title", "{estate.astrology_heading}" }
                p { "{estate.astrology_body}" }
                div { class: "vesta-themes",
                    for theme in &estate.cosmos_themes {
                        details { class: "vesta-theme", "name": "vesta-reflections",
                            summary {
                                span { class: "vesta-theme__symbol", "aria-hidden": "true", "{theme[0]}" }
                                span { "{theme[1]}" }
                            }
                            div { class: "vesta-theme__body",
                                h3 { "{theme[2]}" }
                                p { "{theme[3]}" }
                            }
                        }
                    }
                }
            }
            section { id: "estate-plan", class: "vesta-offer", "aria-label": "{estate.plan_label}",
                aside { class: "vesta-plan", "aria-label": "{estate.plan_label}",
                    img { class: "vesta-plan__mark", src: "/public/brand/vesta.svg", alt: "", width: "36", height: "50" }
                    p { class: "vesta-eyebrow", "{estate.plan_label}" }
                    p { class: "vesta-plan__term", "{estate.plan_description}" }
                    ul { for feature in &estate.features { li { "{feature}" } } }
                    p { class: "vesta-note", "{estate.fee_note}" }
                    div { class: "vesta-actions",
                        a { class: "vesta-text-link", href: "#nevada-trusts", "{estate.nevada_label}" }
                        a { class: "vesta-text-link", href: "#trust-administration", "{estate.administration_label}" }
                        a { class: "vesta-text-link", href: "#probate", "{estate.probate_label}" }
                    }
                }
            }
            section { id: "your-plan", class: "vesta-process", "aria-labelledby": "vesta-process-title",
                AsterArtwork { class: "vesta-aster vesta-aster--explore", src: estate.aster_image.clone(), pose: AsterPose::Explore }
                p { class: "vesta-eyebrow", "{estate.process_label}" }
                h2 { id: "vesta-process-title", "{estate.process_heading}" }
                ol { class: "vesta-steps",
                    for step in &estate.steps {
                        li {
                            h3 { "{step[0]}" }
                            p { "{step[1]}" }
                        }
                    }
                }
            }
            section { id: "nevada-trusts", class: "vesta-nevada", "aria-labelledby": "vesta-nevada-title",
                div { class: "vesta-nevada__intro",
                    p { class: "vesta-eyebrow", "{estate.nevada_label}" }
                    h2 { id: "vesta-nevada-title", "{estate.nevada_heading}" }
                    p { "{estate.nevada_body}" }
                    a { class: "vesta-text-link", href: "/services#nevada-trusts", "{estate.nevada_details_label}" }
                }
                div {
                    ul { class: "vesta-nevada__points",
                        for point in &estate.nevada_points {
                            li {
                                h3 { "{point[0]}" }
                                p { "{point[1]}" }
                            }
                        }
                    }
                    p { class: "vesta-note", "{estate.nevada_note}" }
                }
            }
            section { class: "vesta-video", "aria-labelledby": "vesta-video-title",
                h2 { id: "vesta-video-title", "{estate.video_label}" }
                video {
                    controls: true,
                    playsinline: true,
                    preload: "metadata",
                    poster: "/public/brand/vesta.svg",
                    src: "{estate.video_src}",
                    "aria-label": "{estate.video_label}",
                    a { href: "{estate.video_src}", "{estate.video_label}" }
                }
            }
            section { id: "trust-administration", class: "vesta-administration", "aria-labelledby": "vesta-administration-title",
                div { class: "vesta-administration__intro",
                    p { class: "vesta-eyebrow", "{estate.administration_label}" }
                    h2 { id: "vesta-administration-title", "{estate.administration_heading}" }
                    p { "{estate.administration_body}" }
                    a { class: "vesta-text-link", href: "/services#trust-administration", "{estate.administration_details_label}" }
                }
                div {
                    ul { class: "vesta-administration__services",
                        for service in &estate.administration_services {
                            li {
                                h3 { "{service[0]}" }
                                p { "{service[1]}" }
                            }
                        }
                    }
                    div { class: "vesta-administration__contact",
                        p { class: "vesta-eyebrow", "{estate.administration_fee_label}" }
                        p { class: "vesta-note", "{estate.administration_fee_note}" }
                        a {
                            class: "nav-btn nav-btn--primary",
                            href: "{content.contact_href}",
                            target: if contact_external { Some("_blank") } else { None },
                            rel: if contact_external { Some("noopener noreferrer") } else { None },
                            "{estate.administration_contact_label}"
                        }
                    }
                }
            }
            section { id: "probate", class: "vesta-probate", "aria-labelledby": "vesta-probate-title",
                div {
                    p { class: "vesta-eyebrow", "{estate.probate_label}" }
                    h2 { id: "vesta-probate-title", "{estate.probate_heading}" }
                }
                div {
                    p { "{estate.probate_body}" }
                    a { class: "vesta-text-link", href: "/probate", "{estate.probate_details_label}" }
                }
            }
            section { class: "vesta-record", "aria-labelledby": "vesta-record-title",
                div {
                    p { class: "vesta-eyebrow", "{estate.record_label}" }
                    p { "{estate.record_unit}" }
                    span { class: "vesta-status", "{estate.record_status}" }
                }
                div {
                    h2 { id: "vesta-record-title", "{estate.record_heading}" }
                    p { "{estate.record_body}" }
                    p { class: "vesta-note", "{estate.record_note}" }
                }
            }
            SiblingPractices { cards: estate.sibling_practices.clone() }
            section { class: "vesta-photos", "aria-label": "Memorials and places of remembrance",
                for [src, alt] in &estate.photos {
                    img { src: "{src}", alt: "{alt}", loading: "lazy", decoding: "async" }
                }
            }
            if !video_posts.is_empty() {
                section { class: "vesta-updates", id: "video-updates", "aria-labelledby": "vesta-updates-title",
                    div { class: "vesta-updates__heading",
                        h2 { id: "vesta-updates-title", "{estate.video_updates_label}" }
                        p { "{estate.video_updates_body}" }
                        a { class: "vesta-text-link", href: "/blog", "{estate.video_updates_blog_label}" }
                    }
                    div { class: "vesta-updates__feed nav-video-rail", role: "region", tabindex: "0", "aria-labelledby": "vesta-updates-title",
                        for update in video_posts.iter().take(5) {
                            article { class: "vesta-update",
                                if let Some(src) = &update.video_url {
                                    video { controls: true, playsinline: true, preload: "none", "aria-label": "{update.post.title}",
                                        source { src: "{src}", r#type: "video/mp4" }
                                    }
                                }
                                p { class: "vesta-eyebrow", "{update.post.date}" }
                                h3 { a { href: "/blog/{update.post.slug}", "{update.post.title}" } }
                                p { "{update.post.description}" }
                                nav { class: "vesta-update__links", "aria-label": "Watch {update.post.title}",
                                    for link in &update.social_links {
                                        a { class: "vesta-update__link", href: "{link.href}", target: "_blank",
                                            rel: "noopener noreferrer", "aria-label": "Watch on {link.label}",
                                            title: "Watch on {link.label}",
                                            crate::components::SocialMarkGlyph { label: link.label.clone() }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            crate::components::TestimonialSection {
                heading: estate.testimonials_label.clone(), lead: String::new(), cards: testimonials,
            }
            section { class: "vesta-closing", "aria-labelledby": "vesta-closing-title",
                AsterArtwork { class: "vesta-aster vesta-aster--closing", src: estate.aster_image.clone(), pose: AsterPose::Guide }
                div {
                    h2 { id: "vesta-closing-title", "{estate.closing_heading}" }
                    p { "{estate.closing_body}" }
                }
                a {
                    class: "nav-btn nav-btn--primary",
                    href: "{content.contact_href}",
                    target: if contact_external { Some("_blank") } else { None },
                    rel: if contact_external { Some("noopener noreferrer") } else { None },
                    "{content.contact_label}"
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_updates_group_platforms_and_render_all_supplied_testimonials() {
        fn app() -> Element {
            rsx! {
                EstateHome {
                    content: HomeContent::default(),
                    estate: EstateContent {
                        video_updates_label: "Video updates".into(),
                        testimonials_label: "Testimonials".into(),
                        ..EstateContent::default()
                    },
                    video_posts: vec![super::super::HomeVideoPost {
                        post: crate::blog_index::BlogPostSummary {
                            slug: "synthetic-update".into(), title: "Synthetic video".into(),
                            ..Default::default()
                        },
                        social_links: vec![
                            crate::blog_post::SocialLinkContent { label: "Facebook".into(), href: "https://www.facebook.com/reel/123/".into() },
                            crate::blog_post::SocialLinkContent { label: "TikTok".into(), href: "https://www.tiktok.com/@synthetic/video/123".into() },
                        ],
                        ..Default::default()
                    }],
                    testimonials: (0..8).map(|index| crate::components::TestimonialCard {
                        quote: format!("Synthetic quote {index}"), attribution: String::new(),
                        detail: None, profile_image_url: None, product_label: None,
                    }).collect::<Vec<_>>(),
                }
            }
        }
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        assert_eq!(html.matches("class=\"vesta-update\"").count(), 1);
        assert!(html.contains("href=\"/blog/synthetic-update\""));
        assert!(html.contains("Watch on Facebook"));
        assert!(html.contains("Watch on TikTok"));
        assert!(html.contains(r#"data-social-mark="Facebook""#));
        assert!(html.contains(r#"data-social-mark="TikTok""#));
        for index in 0..8 {
            assert!(html.contains(&format!("Synthetic quote {index}")));
        }
        assert!(!html.contains("<iframe"));
    }

    #[test]
    fn video_feed_shows_only_five_updates_in_supplied_publication_order() {
        fn app() -> Element {
            rsx! {
                EstateHome {
                    content: HomeContent::default(),
                    estate: EstateContent::default(),
                    video_posts: (0..7).map(|index| super::super::HomeVideoPost {
                        post: crate::blog_index::BlogPostSummary {
                            slug: format!("update-{index}"), title: format!("Update {index}"),
                            ..Default::default()
                        },
                        video_url: Some(format!("/assets/synthetic-{index}.mp4")),
                        ..Default::default()
                    }).collect::<Vec<_>>(),
                }
            }
        }
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        assert_eq!(html.matches("class=\"vesta-update\"").count(), 5);
        let feed = html
            .split_once("class=\"vesta-updates__feed nav-video-rail\"")
            .unwrap()
            .1;
        let feed = feed.split_once("</section>").unwrap().0;
        assert_eq!(feed.matches("<video").count(), 5);
        for index in 0..5 {
            assert!(html.contains(&format!("/assets/synthetic-{index}.mp4")));
        }
        assert!(!html.contains("synthetic-5.mp4"));
        assert!(!html.contains("synthetic-6.mp4"));
        assert!(html.find("synthetic-0.mp4") < html.find("synthetic-4.mp4"));
        assert!(html.contains("tabindex=\"0\""));
        assert!(html.contains("aria-labelledby=\"vesta-updates-title\""));
        assert!(!html.contains("autoplay"));
    }

    #[test]
    fn estate_gallery_renders_accessible_lazy_photos() {
        fn app() -> Element {
            rsx! {
                EstateHome {
                    content: HomeContent::default(),
                    estate: EstateContent {
                        photos: vec![[
                            "/public/img/vesta-home/cherry-blossoms.jpg".into(),
                            "Cherry blossoms above memorials".into(),
                        ]],
                        ..EstateContent::default()
                    },
                }
            }
        }
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        assert!(!html.contains("vesta-price"));
        assert!(html.contains("src=\"/public/img/vesta-home/cherry-blossoms.jpg\""));
        assert!(html.contains("alt=\"Cherry blossoms above memorials\""));
        assert!(html.contains("loading=\"lazy\""));
        assert!(html.contains("aria-label=\"Memorials and places of remembrance\""));
    }
}
