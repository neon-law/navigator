//! The `authority_table` section: each [Authority](../../../docs/glossary/authority.md)
//! this matter uses, with its citation, its class, which side relies on it,
//! what the firm did with it, and the quoted passages its Citations pin.
//!
//! Every value here is display-ready and wasm-safe; the server half
//! ([`derive`]) builds the cards from `authority`, `authority_use`,
//! `citation`, and `verification`, already scoped to the lens.
//!
//! Links follow the `legal-authority` skill: on the lawyer lens every
//! citation carries the Authority's canonical link, and a missing one is
//! flagged rather than hidden. A client page carries the citation as text
//! and makes no off-origin request, the same rule a built Portal
//! keeps; the link lives in the Authority record. A client also never sees
//! a disposition that records firm reasoning, why a passage was relied on,
//! or the verification record.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use super::PageLens;
use crate::components::{Badge, BadgeTone, Empty, ExternalLink, Panel};

/// One quoted passage a Citation pins.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityQuote {
    /// The relied-on text, verbatim.
    pub quote: String,
    /// Why the passage supports the point. Empty off the lawyer lens.
    pub why: String,
    /// `Verified`, `Unverified`, `Stale`, `Rejected`, or `Not verified`.
    /// Empty off the lawyer lens.
    pub verification: String,
}

/// One Authority as this matter uses it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityCard {
    /// The use id.
    pub key: String,
    pub citation: String,
    pub title: String,
    /// `Case`, `Statute`, `Regulation`, `Administrative`, or
    /// `Secondary source`.
    pub class: String,
    /// The court or publisher, and the date issued, as one line.
    pub source: String,
    /// Which side relies on it, in plain words.
    pub position: String,
    /// What the firm did with it, in plain words.
    pub disposition: String,
    /// The use's role note. Empty off the lawyer lens.
    pub role: String,
    /// The canonical URL. Always `None` off the lawyer lens.
    pub link: Option<String>,
    pub quotes: Vec<AuthorityQuote>,
}

/// The `authority_table` section's data, already scoped to the lens.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityView {
    pub lens: Option<PageLens>,
    pub cards: Vec<AuthorityCard>,
}

fn verification_tone(status: &str) -> BadgeTone {
    match status {
        "Verified" => BadgeTone::Ready,
        "Rejected" => BadgeTone::Blocked,
        "Stale" | "Unverified" | "Not verified" => BadgeTone::Review,
        _ => BadgeTone::Default,
    }
}

/// The `authority_table` section.
#[component]
pub fn AuthorityTableSection(view: AuthorityView) -> Element {
    let lawyer = view.lens == Some(PageLens::Lawyer);
    let note = if lawyer {
        "Each citation links to its source. A citation with no link recorded is flagged."
    } else {
        "Citations appear as text. Your legal team keeps each source on file."
    };
    rsx! {
        section {
            class: "matter-section",
            "data-section": "authority_table",
            aria_labelledby: "section-authority_table-title",
            Panel {
                id: "section-authority_table".to_string(),
                title: "Authorities".to_string(),
                note: note.to_string(),
                if view.cards.is_empty() {
                    div { class: "matter-section__empty",
                        Empty { title: "No authorities are recorded for this matter yet.".to_string() }
                    }
                } else {
                    ul { class: "matter-authorities",
                        for card in view.cards.iter() {
                            AuthorityCardView { key: "{card.key}", card: card.clone(), lawyer }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn AuthorityCardView(card: AuthorityCard, lawyer: bool) -> Element {
    rsx! {
        li { class: "matter-authorities__item",
            article { class: "matter-authorities__card",
                div { class: "matter-authorities__head",
                    p { class: "matter-authorities__citation",
                        if let Some(href) = card.link.clone() {
                            ExternalLink { href, class: "nav-link".to_string(), "{card.citation}" }
                        } else {
                            cite { "{card.citation}" }
                        }
                    }
                    Badge { tone: BadgeTone::Source, "{card.class}" }
                    if !card.position.is_empty() {
                        Badge { "{card.position}" }
                    }
                    if !card.disposition.is_empty() {
                        Badge { tone: BadgeTone::Next, "{card.disposition}" }
                    }
                    if lawyer && card.link.is_none() {
                        Badge { tone: BadgeTone::Blocked, "No link recorded" }
                    }
                }
                if !card.title.is_empty() {
                    p { class: "matter-authorities__title", "{card.title}" }
                }
                if !card.source.is_empty() {
                    p { class: "matter-authorities__source", "{card.source}" }
                }
                if !card.role.is_empty() {
                    p { class: "matter-authorities__role", "{card.role}" }
                }
                for quote in card.quotes.iter() {
                    figure { class: "matter-authorities__quote",
                        blockquote { "{quote.quote}" }
                        if !quote.why.is_empty() || !quote.verification.is_empty() {
                            figcaption {
                                if !quote.verification.is_empty() {
                                    Badge { tone: verification_tone(&quote.verification), "{quote.verification}" }
                                    " "
                                }
                                "{quote.why}"
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The server half: cards from the citation apparatus.
#[cfg(feature = "server")]
pub mod derive {
    use super::{AuthorityCard, AuthorityQuote};
    use crate::sections::PageLens;
    use rules::citation::{AuthorityClass, Disposition};
    use store::authorities::{Authority, AuthorityUse, Citation};
    use store::verifications::Verification;

    /// One use, its Authority, and each Citation with its verifications
    /// (newest first), as the loader read them.
    pub struct UseRecord {
        pub authority_use: AuthorityUse,
        pub authority: Authority,
        pub citations: Vec<(Citation, Vec<Verification>)>,
    }

    fn class_label(raw: &str) -> String {
        match AuthorityClass::parse(raw) {
            Some(AuthorityClass::CaseLaw) => "Case",
            Some(AuthorityClass::Statute) => "Statute",
            Some(AuthorityClass::Regulation) => "Regulation",
            Some(AuthorityClass::Administrative) => "Administrative",
            Some(AuthorityClass::Secondary) => "Secondary source",
            None => "Authority",
        }
        .to_string()
    }

    fn position_label(raw: &str) -> String {
        match raw {
            "ours" => "We rely on it",
            "adverse" => "The other side relies on it",
            "neutral" => "Neutral",
            _ => "",
        }
        .to_string()
    }

    fn disposition_label(raw: &str) -> String {
        let spaced = raw.replace('-', " ");
        let mut chars = spaced.chars();
        chars.next().map_or_else(String::new, |first| {
            first.to_uppercase().collect::<String>() + chars.as_str()
        })
    }

    /// The newest verification's three axes, as one word.
    fn verification(rows: &[Verification]) -> String {
        let Some(latest) = rows.first() else {
            return "Not verified".to_string();
        };
        let axes = [
            latest.status_citation.as_str(),
            latest.status_quote.as_str(),
            latest.status_proposition.as_str(),
        ];
        if axes.contains(&"rejected") {
            "Rejected"
        } else if axes.contains(&"stale") {
            "Stale"
        } else if axes.iter().all(|axis| *axis == "verified") {
            "Verified"
        } else {
            "Unverified"
        }
        .to_string()
    }

    /// A link only when the stored URL is an absolute web address.
    fn link(raw: Option<&String>) -> Option<String> {
        raw.map(|url| url.trim().to_string())
            .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
    }

    /// The cards for `lens`, ours first, then by citation. A client never
    /// receives a use whose disposition records firm reasoning, even when
    /// the caller passed one.
    #[must_use]
    pub fn cards(records: &[UseRecord], lens: PageLens) -> Vec<AuthorityCard> {
        let lawyer = lens == PageLens::Lawyer;
        let mut out: Vec<(u8, AuthorityCard)> = records
            .iter()
            .filter(|record| {
                lawyer
                    || Disposition::parse(&record.authority_use.disposition)
                        .is_some_and(|d| !d.is_firm_reasoning())
            })
            .map(|record| {
                let authority = &record.authority;
                let use_ = &record.authority_use;
                let source = [authority.publisher.clone(), authority.issued_on.clone()]
                    .into_iter()
                    .flatten()
                    .filter(|part| !part.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join(" · ");
                let rank = match use_.position.as_str() {
                    "ours" => 0,
                    "adverse" => 1,
                    _ => 2,
                };
                (
                    rank,
                    AuthorityCard {
                        key: use_.id.to_string(),
                        citation: authority.citation.clone(),
                        title: authority.title.clone(),
                        class: class_label(&authority.class),
                        source,
                        position: position_label(&use_.position),
                        disposition: disposition_label(&use_.disposition),
                        role: if lawyer {
                            use_.role.clone().unwrap_or_default()
                        } else {
                            String::new()
                        },
                        link: if lawyer {
                            link(authority.canonical_url.as_ref())
                        } else {
                            None
                        },
                        quotes: record
                            .citations
                            .iter()
                            .map(|(citation, verifications)| AuthorityQuote {
                                quote: citation.quote.clone(),
                                why: if lawyer {
                                    citation.why.clone()
                                } else {
                                    String::new()
                                },
                                verification: if lawyer {
                                    verification(verifications)
                                } else {
                                    String::new()
                                },
                            })
                            .collect(),
                    },
                )
            })
            .collect();
        out.sort_by(|a, b| (a.0, &a.1.citation).cmp(&(b.0, &b.1.citation)));
        out.into_iter().map(|(_, card)| card).collect()
    }

    /// Read this matter's uses for `lens` (the client-visible ones on a
    /// client page) with their Authorities, Citations, and, on the lawyer
    /// lens, verifications, then build the cards.
    ///
    /// # Errors
    /// Any store error, as text.
    pub async fn load(
        surreal: &store::surreal::SurrealDb,
        project_id: uuid::Uuid,
        lens: PageLens,
    ) -> Result<Vec<AuthorityCard>, String> {
        let lawyer = lens == PageLens::Lawyer;
        let uses = if lawyer {
            store::authorities::uses_for_project(surreal, project_id).await
        } else {
            store::authorities::client_visible_uses(surreal, project_id).await
        }
        .map_err(|e| e.to_string())?;
        let mut records = Vec::new();
        for authority_use in uses {
            let Some(authority) = store::authorities::find(
                surreal,
                store::authorities::AuthorityLookup::Id(authority_use.authority_id),
            )
            .await
            .map_err(|e| e.to_string())?
            else {
                continue;
            };
            let mut citations = Vec::new();
            for citation in store::authorities::citations_for_use(surreal, authority_use.id)
                .await
                .map_err(|e| e.to_string())?
            {
                let verifications = if lawyer {
                    store::verifications::for_citation(surreal, citation.id)
                        .await
                        .map_err(|e| e.to_string())?
                } else {
                    Vec::new()
                };
                citations.push((citation, verifications));
            }
            records.push(UseRecord {
                authority_use,
                authority,
                citations,
            });
        }
        Ok(cards(&records, lens))
    }

    #[cfg(test)]
    mod tests {
        use super::{cards, UseRecord};
        use crate::sections::PageLens;
        use store::authorities::{Authority, AuthorityUse, Citation};
        use store::verifications::Verification;
        use uuid::Uuid;

        fn record(
            citation: &str,
            position: &str,
            disposition: &str,
            url: Option<&str>,
        ) -> UseRecord {
            let now = chrono::Utc::now();
            let authority_id = Uuid::now_v7();
            let use_id = Uuid::now_v7();
            let citation_id = Uuid::now_v7();
            UseRecord {
                authority_use: AuthorityUse {
                    id: use_id,
                    project_id: Uuid::now_v7(),
                    authority_id,
                    position: position.into(),
                    disposition: disposition.into(),
                    role: Some("Synthetic role note".into()),
                    inserted_at: now,
                    updated_at: now,
                },
                authority: Authority {
                    id: authority_id,
                    class: "case_law".into(),
                    citation: citation.into(),
                    short_cite: None,
                    title: format!("Synthetic title for {citation}"),
                    publisher: Some("Synthetic Supreme Court".into()),
                    issued_on: Some("2020-01-02".into()),
                    canonical_url: url.map(str::to_string),
                    checked_on: None,
                    archived_asset_id: None,
                    inserted_at: now,
                    updated_at: now,
                },
                citations: vec![(
                    Citation {
                        id: citation_id,
                        authority_use_id: use_id,
                        quote: "Synthetic verbatim passage.".into(),
                        why: "Synthetic reason it supports us.".into(),
                        source_pin: None,
                        draft_pin: None,
                        inserted_at: now,
                        updated_at: now,
                    },
                    vec![Verification {
                        id: Uuid::now_v7(),
                        citation_id,
                        revision_sha: "abc123".into(),
                        status_citation: "verified".into(),
                        status_quote: "verified".into(),
                        status_proposition: "verified".into(),
                        verifier_person_id: Uuid::now_v7(),
                        inserted_at: now,
                        updated_at: now,
                    }],
                )],
            }
        }

        fn records() -> Vec<UseRecord> {
            vec![
                record("Zeta v. Synthetic", "adverse", "relied-on", None),
                record(
                    "Alpha v. Synthetic",
                    "ours",
                    "relied-on",
                    Some("https://example.com/alpha"),
                ),
                record("Hidden v. Synthetic", "ours", "reviewed-not-used", None),
            ]
        }

        #[test]
        fn the_lawyer_lens_links_each_citation_and_keeps_the_firms_record() {
            let out = cards(&records(), PageLens::Lawyer);
            let cites: Vec<&str> = out.iter().map(|c| c.citation.as_str()).collect();
            assert_eq!(
                cites,
                [
                    "Alpha v. Synthetic",
                    "Hidden v. Synthetic",
                    "Zeta v. Synthetic"
                ]
            );
            assert_eq!(out[0].link.as_deref(), Some("https://example.com/alpha"));
            assert_eq!(out[0].class, "Case");
            assert_eq!(out[0].position, "We rely on it");
            assert_eq!(out[0].disposition, "Relied on");
            assert_eq!(out[0].quotes[0].verification, "Verified");
            assert!(!out[0].quotes[0].why.is_empty());
            assert!(out[2].link.is_none(), "no URL recorded");
        }

        #[test]
        fn the_client_lens_has_no_link_no_firm_reasoning_and_no_verification() {
            let out = cards(&records(), PageLens::Client);
            assert!(out.iter().all(|c| c.citation != "Hidden v. Synthetic"));
            for card in &out {
                assert!(card.link.is_none(), "{card:?}");
                assert!(card.role.is_empty(), "{card:?}");
                for quote in &card.quotes {
                    assert!(quote.why.is_empty() && quote.verification.is_empty());
                    assert_eq!(quote.quote, "Synthetic verbatim passage.");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AuthorityCard, AuthorityQuote, AuthorityView};
    use crate::sections::tests::render;
    use crate::sections::{PageLens, SectionView};

    fn card(link: Option<&str>, why: &str, verification: &str) -> AuthorityCard {
        AuthorityCard {
            key: "use-1".into(),
            citation: "Alpha v. Synthetic, 1 Syn. 2d 1 (2020)".into(),
            title: "Synthetic title".into(),
            class: "Case".into(),
            source: "Synthetic Supreme Court · 2020-01-02".into(),
            position: "We rely on it".into(),
            disposition: "Relied on".into(),
            role: String::new(),
            link: link.map(str::to_string),
            quotes: vec![AuthorityQuote {
                quote: "Synthetic verbatim passage.".into(),
                why: why.into(),
                verification: verification.into(),
            }],
        }
    }

    #[test]
    fn a_lawyer_card_links_the_citation_itself_off_site() {
        let html = render(SectionView::AuthorityTable(AuthorityView {
            lens: Some(PageLens::Lawyer),
            cards: vec![card(
                Some("https://example.com/alpha"),
                "Synthetic reason.",
                "Verified",
            )],
        }));
        assert!(
            html.contains("href=\"https://example.com/alpha\""),
            "{html}"
        );
        assert!(html.contains("rel=\"noopener noreferrer\""), "{html}");
        assert!(html.contains("Alpha v. Synthetic"), "{html}");
        assert!(html.contains("Synthetic verbatim passage."), "{html}");
        assert!(html.contains("Verified"), "{html}");
        assert!(!html.contains("No link recorded"), "{html}");
    }

    #[test]
    fn a_lawyer_card_without_a_link_is_flagged() {
        let html = render(SectionView::AuthorityTable(AuthorityView {
            lens: Some(PageLens::Lawyer),
            cards: vec![card(None, "", "Not verified")],
        }));
        assert!(html.contains("No link recorded"), "{html}");
        assert!(html.contains("<cite>"), "{html}");
    }

    #[test]
    fn a_client_card_carries_the_citation_as_text_with_no_off_site_link() {
        let html = render(SectionView::AuthorityTable(AuthorityView {
            lens: Some(PageLens::Client),
            cards: vec![card(None, "", "")],
        }));
        assert!(html.contains("Alpha v. Synthetic"), "{html}");
        assert!(!html.contains("href=\"http"), "{html}");
        assert!(!html.contains("No link recorded"), "{html}");
        assert!(!html.contains("figcaption"), "{html}");
    }
}
