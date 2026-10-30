//! Each house brand's public marketing-page copy, loaded from
//! `locales/en/<brand-key>/`.
//!
//! The words live in the English catalog. This module keeps the page
//! constructors the router calls and the advertising guards that read them.
//! Editing published copy is a YAML change; these functions stay as the
//! typed load.

use webapp::marketing_page::PageContent;

use crate::locales;

/// `/navigator` — the platform the firms the firm serves work on. See
/// `locales/en/neon/navigator.yaml`.
pub fn navigator(branding: &views::brand::Branding) -> PageContent {
    locales::navigator(branding)
}

/// `/legal-aid` — the workshop, self-hosted, and hosted paths for organizations.
pub fn legal_aid(branding: &views::brand::Branding) -> PageContent {
    locales::legal_aid(branding)
}

/// `/services` — the published schedule. See this brand's `services.yaml`.
pub fn legal_services(branding: &views::brand::Branding) -> PageContent {
    locales::legal_services(branding)
}

/// A brand's catalogued plain-language explainer.
pub fn explainer(branding: &views::brand::Branding, stem: &str) -> PageContent {
    locales::explainer(branding, stem)
}

/// `/delete-your-debt` — Neon's own gateway to the DeleteYourDebt.com
/// practice. See `locales/en/neon/gateway-delete-your-debt.yaml`.
pub fn delete_your_debt_gateway(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
) -> PageContent {
    locales::delete_your_debt_gateway(branding, deployment_host)
}

/// `/nautilus` — Neon Law Nautilus, the DeleteYourData.com subscription. See
/// `locales/en/neon/nautilus.yaml`.
pub fn nautilus(branding: &views::brand::Branding, deployment_host: Option<&str>) -> PageContent {
    locales::nautilus(branding, deployment_host)
}

/// `/delete-your-data` — Neon's own gateway to the DeleteYourData.com
/// practice. See `locales/en/neon/gateway-delete-your-data.yaml`.
pub fn delete_your_data_gateway(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
) -> PageContent {
    locales::delete_your_data_gateway(branding, deployment_host)
}

/// `/immigration` — Neon's own gateway to the Abhaya Immigration practice.
/// See `locales/en/neon/gateway-immigration.yaml`.
pub fn immigration_gateway(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
) -> PageContent {
    locales::immigration_gateway(branding, deployment_host)
}

/// `/estate-planning` — Neon's own gateway to the Vesta Estate Planning
/// practice. See `locales/en/neon/gateway-estate-planning.yaml`.
pub fn estate_planning_gateway(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
) -> PageContent {
    locales::estate_planning_gateway(branding, deployment_host)
}

/// `/accidents` — Neon's own gateway to the Misericordia Injury Law
/// practice. See `locales/en/neon/gateway-accidents.yaml`.
pub fn accidents_gateway(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
) -> PageContent {
    locales::accidents_gateway(branding, deployment_host)
}

/// `/divorce` — Neon's own gateway to the Daybridge Divorce Law practice.
/// See `locales/en/neon/gateway-divorce.yaml`.
pub fn divorce_gateway(
    branding: &views::brand::Branding,
    deployment_host: Option<&str>,
) -> PageContent {
    locales::divorce_gateway(branding, deployment_host)
}

/// The regulated claims on the firm's public pages.
///
/// `/navigator` and `/services` are the firm's, so the copy and the guards that
/// hold its claims in place live in the binary that publishes them rather than
/// in the application underneath.
#[cfg(test)]
mod firm_copy_tests {
    use webapp::marketing_page::{Band, Paragraph};

    #[test]
    fn legal_aid_reuses_the_canonical_hosted_card_and_usage_disclosure() {
        let pricing = |page: webapp::marketing_page::PageContent| {
            page.bands
                .into_iter()
                .find_map(|band| match band {
                    Band::Cards {
                        anchor,
                        description,
                        items,
                        ..
                    } if anchor == "pricing" => Some((description, items)),
                    _ => None,
                })
                .expect("published pricing band")
        };
        let branding = &views::brand::DEFAULT_BRANDING;
        let (canonical_disclosure, canonical_cards) = pricing(super::navigator(branding));
        let canonical_card = canonical_cards
            .into_iter()
            .find(|card| card.title == "Legal aid organizations")
            .expect("canonical legal-aid card");
        let (audience_disclosure, audience_cards) = pricing(super::legal_aid(branding));
        assert_eq!(audience_disclosure, canonical_disclosure);
        assert_eq!(audience_cards.len(), 1);
        assert!(audience_cards[0] == canonical_card);
    }

    /// Every string the services band puts in front of a reader: the
    /// heading, the search chrome, the empty state, and each service's name,
    /// category, fee, period, scope, and the two regulated badges.
    ///
    /// The keywords are absent because they are search metadata a reader never
    /// sees; `audience` is absent for the same reason. A guard reading this
    /// text is reading the page, not the data behind it.
    fn services_text(services: &webapp::services_search::ServicesBand) -> String {
        let entries = services
            .services
            .iter()
            .map(|service| {
                let mut badges = String::new();
                if service.members_only {
                    badges.push_str(&services.members_badge);
                    badges.push(' ');
                }
                if service.state_fee {
                    badges.push_str(&services.state_fee_badge);
                }
                let package = service.package.as_ref().map_or_else(String::new, |quote| {
                    format!("{} {}", services.package_badge, quote.members.join(" "))
                });
                let plan_price = service
                    .plan_price
                    .as_ref()
                    .map_or_else(String::new, |price| {
                        format!("{} {}", price.plan, price.amount)
                    });
                format!(
                    "{} {} {} {} {} {} {} {badges} {package} {plan_price}",
                    service.item,
                    service.name,
                    service.category,
                    service.fee,
                    service.period,
                    service.blurb,
                    service.includes.join(" "),
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        let description = services.description.clone().unwrap_or_default();
        format!(
            "{} {} {description} {} {} {} {} {} {} {} {} {} {entries}",
            services.overline,
            services.heading,
            services.search_label,
            services.search_placeholder,
            services.submit_label,
            services.fee_label,
            services.includes_label,
            services.package_badge,
            services.package_members_label,
            services.empty,
            services.empty_help,
        )
    }

    fn paragraphs(body: &[Paragraph]) -> String {
        body.iter()
            .flat_map(|p| p.iter().map(|r| r.text.clone()))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Read every published download field into the copy guards below.
    fn downloads_text(band: &Band) -> String {
        let Band::Downloads {
            overline,
            heading,
            description,
            version,
            archive_label,
            items,
            package,
            ..
        } = band
        else {
            unreachable!("downloads_text receives a downloads band")
        };
        let boxes = items
            .iter()
            .map(|d| format!("{} {} {}", d.label, d.detail, d.filename))
            .collect::<Vec<_>>()
            .join(" ");
        let package = package.as_ref().map_or_else(String::new, |p| {
            format!(
                "{} {} {}",
                p.heading,
                paragraphs(&p.body),
                p.commands.join(" ")
            )
        });
        let description = description.clone().unwrap_or_default();
        format!("{overline} {heading} {description} {version} {archive_label} {boxes} {package}")
    }

    /// Every word of prose a band renders, flattened. Titles, leads, overlines,
    /// descriptions, chips, and card bodies all count: a reader does not
    /// distinguish the struct field a claim arrived in, so a regulated claim (a
    /// rate, a turnaround promise, a comparative superlative) in any field must
    /// reach every guard in this module. A guard that reads only some of the
    /// page reports green on the half it cannot see.
    fn band_text(band: &Band) -> String {
        match band {
            Band::Enrollment { heading, body, steps, next_label, back_label, submit_label, booking_label, booking_body, .. } =>
                format!("{heading} {body} {} {next_label} {back_label} {submit_label} {booking_label} {booking_body}", steps.join(" ")),

            Band::Statement {
                heading,
                lead,
                body,
            } => format!("{heading} {lead} {}", paragraphs(body)),
            Band::Cards {
                overline,
                heading,
                description,
                items,
                ..
            } => {
                let cards = items
                    .iter()
                    .map(|c| format!("{} {} {}", c.title, c.chips.join(" "), paragraphs(&c.body)))
                    .collect::<Vec<_>>()
                    .join(" ");
                let description = description.clone().unwrap_or_default();
                format!("{overline} {heading} {description} {cards}")
            }
            Band::Video {
                overline,
                heading,
                description,
                label,
                ..
            } => format!(
                "{overline} {heading} {} {label}",
                description.clone().unwrap_or_default()
            ),
            Band::Photo { image, caption } => {
                // Alt text is published copy — a screen reader reads it —
                // so the guards scan it alongside the visible caption.
                let alt = image
                    .as_ref()
                    .map_or_else(String::new, |image| image.alt.clone());
                format!("{alt} {}", caption.clone().unwrap_or_default())
            }
            Band::Steps {
                overline,
                heading,
                description,
                items,
                ..
            } => {
                let steps = items
                    .iter()
                    .map(|s| format!("{} {}", s.title, paragraphs(&s.body)))
                    .collect::<Vec<_>>()
                    .join(" ");
                let description = description.clone().unwrap_or_default();
                format!("{overline} {heading} {description} {steps}")
            }
            Band::ProjectNetwork {
                overline,
                heading,
                description,
                left,
                right,
                mcp_tools,
                agentic_coding_tools,
                saas_tools,
                ..
            } => {
                let nodes = left
                    .iter()
                    .chain(right)
                    .map(|node| format!("{} {}", node.label, node.detail))
                    .collect::<Vec<_>>()
                    .join(" ");
                let description = description.clone().unwrap_or_default();
                format!(
                    "{overline} {heading} {description} {nodes} {} {} {}",
                    mcp_tools.join(" "),
                    agentic_coding_tools.join(" "),
                    saas_tools.join(" ")
                )
            }
            Band::AppPreview { .. } => app_preview_text(band),
            Band::Downloads { .. } => downloads_text(band),
            Band::Services(services) => services_text(services),
            Band::Cta { heading, body, .. } => {
                format!("{heading} {}", body.clone().unwrap_or_default())
            }
        }
    }

    fn app_preview_text(band: &Band) -> String {
        let Band::AppPreview {
            overline,
            heading,
            description,
            app_name,
            app_subtitle,
            price,
            checks_heading,
            checks,
            app_store_label,
            play_store_label,
            availability,
            ..
        } = band
        else {
            return String::new();
        };
        let checks = checks
            .iter()
            .map(|check| format!("{} {}", check.title, check.detail))
            .collect::<Vec<_>>()
            .join(" ");
        let description = description.clone().unwrap_or_default();
        format!(
            "{overline} {heading} {description} {app_name} {app_subtitle} {price} \
             {checks_heading} {checks} {app_store_label} {play_store_label} {availability}"
        )
    }

    fn page_text(bands: &[Band]) -> String {
        bands.iter().map(band_text).collect::<Vec<_>>().join(" ")
    }

    /// The fee schedule, resolved from the page rather than restated.
    ///
    /// Every guard below reads the rendered band, so adding a matter without
    /// scoping it — or shipping a placeholder in its price — fails here rather
    /// than passing against a list this file happened to keep in step.
    fn schedule(
        content: &webapp::marketing_page::PageContent,
    ) -> &webapp::services_search::ServicesBand {
        content
            .bands
            .iter()
            .find_map(Band::services)
            .expect("the Legal Services page renders its fee schedule as a services band")
    }

    /// The platform page offers free use to attorneys who co-counsel with the firm.
    #[test]
    fn the_navigator_page_invites_pro_bono_co_counsel() {
        let content = super::navigator(&views::brand::DEFAULT_BRANDING);
        let text = format!("{} {}", page_text(&content.bands), content.meta_description);
        assert_eq!(
            content.tagline,
            "A general-purpose agentic lawyering engine, built so your agent spends tokens on the legal work."
        );
        assert!(
            text.contains("Free use for those who co-counsel with us."),
            "the page offers free use to co-counseling attorneys: {text}"
        );
        assert!(
            text.contains("Anyone who co-counsels a case with us gets the software free for life for their own practices."),
            "the page states the lifetime software offer: {text}"
        );
        for removed in [
            "The manuals that go with the binary",
            "What a firm works with",
            "The licence, and the one thing we sell around it",
        ] {
            assert!(
                !text.contains(removed),
                "the retired band remains: {removed}: {text}"
            );
        }
        assert!(
            !text.to_lowercase().contains("fractional"),
            "the retired fractional offer must not remain: {text}"
        );
        match content.bands.last() {
            Some(Band::Cta { email, .. }) => {
                assert_eq!(email, views::brand::firm_email());
            }
            _ => panic!("the co-counsel invitation must be the page CTA"),
        }
    }

    /// The platform page is not a CTO/CISO or consulting advertisement.
    #[test]
    fn the_navigator_page_removes_the_cto_ciso_offer() {
        let content = super::navigator(&views::brand::DEFAULT_BRANDING);
        let text = format!("{} {}", page_text(&content.bands), content.meta_description);
        let words = || text.split(|character: char| !character.is_ascii_alphanumeric());
        assert!(
            !words().any(|word| word.eq_ignore_ascii_case("cto")),
            "no CTO offer reaches the page: {text}"
        );
        assert!(
            !words().any(|word| word.eq_ignore_ascii_case("ciso")),
            "no CISO offer reaches the page: {text}"
        );
        // `law-related service` is the RPC 5.7 term of art, and the licence
        // offer is required to use it — so the ban moves off the phrase and onto
        // the *subject* the retired copy attached it to. Banning the phrase
        // outright would mean this page could never make the one disclosure the
        // rule asks for, which is not what removing a consulting offer was for.
        for retired in ["technology function", "consulting"] {
            assert!(
                !text.to_lowercase().contains(retired),
                "the retired consulting offer must not return to this page \
                 (`{retired}`): {text}"
            );
        }
        assert_eq!(
            text.matches("law-related service").count(),
            text.matches("Licensing software is a law-related service")
                .count(),
            "every `law-related service` on this page must be the licence \
             disclosure; any other use is the consulting characterization \
             coming back: {text}"
        );
        assert!(
            !text.contains("Bring a case") && !text.contains("See it in practice"),
            "the sales-style card grid must not remain: {text}"
        );
        assert!(
            !text.contains("Navigator is the AI system we build")
                && !text.contains("everyone loves vibe-coding"),
            "the retired explanatory copy must not remain: {text}"
        );
    }

    /// The Legal Services page is a schedule of scoped matters.
    ///
    /// This is the shape the fee schedule will be published in, asserted before
    /// the figures land. It replaced a page held to the opposite rule — guarded
    /// against containing a `$` at all, because the firm quoted every
    /// engagement privately — so what matters here is that the structure
    /// survives: a list of named matters, each with the scope its future fee
    /// will buy. A card that lost its scope line would leave a bare price with
    /// no boundary the moment a number arrived beside it.
    #[test]
    fn the_schedule_lists_scoped_matters() {
        let content = super::legal_services(&views::brand::DEFAULT_BRANDING);
        let services = &schedule(&content).services;
        assert!(
            services.len() >= 5,
            "the schedule is the page; {} matters is not a schedule",
            services.len()
        );
        for service in services {
            assert!(
                !service.includes.is_empty(),
                "{} names no scope, which reads as covering everything",
                service.name
            );
            assert!(
                !service.blurb.trim().is_empty(),
                "{} says nothing about what the work is",
                service.name
            );
        }
    }

    /// A fee is either published properly or not published at all.
    ///
    /// Every entry is unset today and the firm sets them when it decides them,
    /// so this guards the transition rather than the current state: whatever
    /// appears in that column has to be a real figure. A blank string, a `TBD`,
    /// or a `—` would render as a price tag the reader cannot parse, and a
    /// placeholder shipped by accident is exactly the failure that guard is
    /// for.
    #[test]
    fn any_published_fee_is_a_real_figure() {
        let content = super::legal_services(&views::brand::DEFAULT_BRANDING);
        for service in &schedule(&content).services {
            let price = &service.fee;
            assert!(
                price.starts_with('$'),
                "{} publishes {price:?}, which is not a fee",
                service.name
            );
            assert!(
                price.chars().any(|c| c.is_ascii_digit()),
                "{} publishes {price:?}, which carries no amount",
                service.name
            );
            // One fee per matter, and it says what it is charged per. The
            // catalog refuses two fees on one record; this is the same rule
            // observed on the rendered page.
            assert!(
                !service.period.trim().is_empty(),
                "{} prints {price:?} with nothing saying what it buys",
                service.name
            );
        }
    }

    /// A fee that depends on a government body's own charge says so.
    ///
    /// The firm cannot control what the Secretary of State, the IRS, or the
    /// USPTO charges, and those change without asking us. A formation priced at
    /// a bare `$700` would be read as the whole cost of forming a company, and
    /// the state's invoice afterwards would land as a surprise charge from a
    /// firm that advertised a flat fee.
    #[test]
    fn a_fee_with_a_pass_through_names_it() {
        let content = super::legal_services(&views::brand::DEFAULT_BRANDING);
        for service in &schedule(&content).services {
            let printed = format!("{} {}", service.fee, service.period);
            if printed.contains('+') {
                assert!(
                    printed.contains("fee"),
                    "{} adds a pass-through without naming it: {printed}",
                    service.name
                );
            }
        }
    }

    /// Every matter whose fee depends on a government charge says so.
    ///
    /// The pass-through is a property of the work, not of the price, so it is
    /// stated whether or not a figure is set. A reader deciding whether they
    /// can afford a formation needs to know a second bill is coming.
    ///
    /// The disclosure now rides a structured flag rather than a sentence
    /// somebody remembered to write, so this reads the flag *and* the badge
    /// the band prints from it — a flag nothing renders would disclose
    /// nothing. The matters are named by catalog id, not by display title:
    /// the titles are draft copy pending attorney review, and a guard that
    /// pinned them would fail on a wording change rather than on a lost
    /// disclosure.
    #[test]
    fn a_matter_with_a_government_charge_discloses_it() {
        let content = super::legal_services(&views::brand::DEFAULT_BRANDING);
        let schedule = schedule(&content);
        assert!(
            !schedule.state_fee_badge.trim().is_empty()
                && schedule.state_fee_badge.to_lowercase().contains("fee"),
            "the pass-through badge must name a fee: {:?}",
            schedule.state_fee_badge
        );
        for matter in ["llc-file", "trademark"] {
            let service = schedule
                .services
                .iter()
                .find(|service| service.id == matter)
                .unwrap_or_else(|| panic!("{matter} is on the schedule"));
            assert!(
                service.state_fee,
                "{} carries a government charge the page must disclose",
                service.name
            );
        }
        // The badge reaches the reader, not just the data.
        let rendered = page_text(&content.bands);
        assert!(
            rendered.contains(&schedule.state_fee_badge),
            "the pass-through disclosure must reach the page: {rendered}"
        );
    }

    /// The page states the attorney review the work rests on.
    ///
    /// A priced list of legal documents is the shape a document mill takes, and
    /// the one thing separating this page from one is that a licensed attorney
    /// reads what goes out. That has to be on the page, not only in the footer.
    #[test]
    fn the_legal_services_page_names_attorney_review() {
        let content = super::legal_services(&views::brand::DEFAULT_BRANDING);
        let text = format!(
            "{} {} {} {}",
            content.title,
            content.tagline,
            content.meta_description,
            page_text(&content.bands)
        );
        assert!(
            text.to_lowercase().contains("attorney"),
            "the page states the attorney review the work rests on: {text}"
        );
    }

    #[test]
    fn the_services_page_explains_notation_pricing_and_separate_fees() {
        let content = super::legal_services(&views::brand::DEFAULT_BRANDING);
        let text = page_text(&content.bands).to_lowercase();
        assert!(!text.contains("a la carte"));
        let schedule = schedule(&content);
        let trademark = schedule
            .services
            .iter()
            .find(|service| service.id == "trademark")
            .expect("the trademark service remains available");
        assert_eq!(trademark.fee, "$100");
        assert_eq!(trademark.period, "+ government filing fees");
        assert!(
            !trademark.members_only,
            "a trademark Notation is available without a plan"
        );
        let family_plan = schedule
            .services
            .iter()
            .find(|service| service.id == "estate-package")
            .expect("the family plan is a Notation package");
        assert_eq!(family_plan.fee, "$5,000");
        assert_eq!(family_plan.period, "per package");
        // No plan price: it quoted against the retired consumer plan, so the
        // package now publishes its a la carte fee alone rather than a
        // discount nobody can obtain.
        assert_eq!(family_plan.plan_price, None);
        let setup = schedule
            .services
            .iter()
            .find(|service| service.id == "llc-launch")
            .expect("company setup is a Notation package");
        assert_eq!(setup.fee, "$100");
        assert_eq!(setup.plan_price, None);
    }

    /// The `/delete-your-debt` gateway names the destination and the firm
    /// behind it, and its CTA is a destination-named handoff — never a claim
    /// that Neon itself provides `PandA` Law's services.
    #[test]
    fn the_delete_your_debt_gateway_names_the_destination_and_the_firm() {
        let content = super::delete_your_debt_gateway(&views::brand::DEFAULT_BRANDING, None);
        let text = format!(
            "{} {} {} {} {}",
            content.title,
            content.tagline,
            content.hero_lead,
            content.meta_description,
            page_text(&content.bands)
        );
        assert!(
            text.contains("DeleteYourDebt.com"),
            "the gateway names the destination by its proper name: {text}"
        );
        assert!(
            text.contains("Guieb & Shook PC"),
            "the gateway names Guieb & Shook PC as the firm behind the destination: {text}"
        );
        let cta = content
            .hero_cta
            .as_ref()
            .expect("the gateway has a hero CTA");
        assert_eq!(cta.label, "Visit DeleteYourDebt.com");
    }

    /// The gateway introduces the service without promising a debt outcome.
    #[test]
    fn the_delete_your_debt_gateway_makes_no_outcome_promise() {
        let content = super::delete_your_debt_gateway(&views::brand::DEFAULT_BRANDING, None);
        let text = format!(
            "{} {} {} {} {}",
            content.title,
            content.tagline,
            content.hero_lead,
            content.meta_description,
            page_text(&content.bands)
        )
        .to_lowercase();
        for promise in [
            "we guarantee",
            "guaranteed result",
            "erase your debt",
            "erase debt",
            "delete your debt",
            "eliminate your debt",
            "reduce what you owe",
            "settle your debt",
            "negotiate your balance",
            "pennies on the dollar",
        ] {
            assert!(
                !text.contains(promise),
                "the gateway promises {promise:?}: {text}"
            );
        }
        assert!(
            text.contains("no result is promised"),
            "the outcome disclaimer survives: {text}"
        );
    }

    /// Litigation and fractional GC stay off the one-time-matter schedule.
    ///
    /// Litigation is quoted per engagement because its scope is not knowable
    /// in advance; a published litigation "price" would be a floor dressed as
    /// a fee, the misleading-fee-advertising problem the flat-fee schedule
    /// exists to avoid. Fractional GC is different: it now publishes its own
    /// figures, but on its own page (`/business`) as a retainer, not here
    /// — this schedule is for one-time matters, and a recurring retainer does
    /// not belong on it regardless of whether it has a public number.
    #[test]
    fn the_services_page_does_not_price_litigation_or_fractional_gc() {
        let content = super::legal_services(&views::brand::DEFAULT_BRANDING);
        let services = &schedule(&content).services;
        for quoted in ["litigation", "fractional"] {
            assert!(
                !services
                    .iter()
                    .any(|service| service.name.to_lowercase().contains(quoted)),
                "{quoted} is quoted per engagement and must not appear in the fee schedule"
            );
        }
    }
    /// The page states the token-efficiency thesis and names what carries it.
    ///
    /// Token efficiency is the page's argument: the agent spends tokens on the
    /// legal work because the context is already written down. Each named
    /// mechanism is something the repository ships (the glossary command, a
    /// Project's `navigator.yaml` and its Project Skills, Drafts and pages as
    /// text in Git, the CLI and MCP server), so dropping the names leaves
    /// the page asserting a preference with nothing under it.
    #[test]
    fn the_navigator_page_makes_the_token_efficiency_case() {
        let content = super::navigator(&views::brand::DEFAULT_BRANDING);
        let text = format!(
            "{} {} {}",
            content.tagline,
            page_text(&content.bands),
            content.meta_description
        );
        assert!(
            text.contains("Token efficiency") && text.contains("spends tokens on the legal work"),
            "the page leads with token efficiency: {text}"
        );
        for named in [
            "navigator glossary show",
            "navigator.yaml",
            "Project Skills",
            "Drafts and pages are text in Git",
            "navigator project gate",
            "MCP server",
            "Claude Code",
            "Codex",
        ] {
            assert!(
                text.contains(named),
                "the page names {named}, a mechanism the claim rests on: {text}"
            );
        }
        assert!(
            text.contains("MIT OR Apache-2.0"),
            "the page states the open-source licence: {text}"
        );
        let lowered = text.to_lowercase();
        for retired in ["vibe", "portal"] {
            assert!(
                !lowered.contains(retired),
                "Matter Pages are declared, so the page names no {retired}: {text}"
            );
        }
    }

    /// The declarative walk shows each step on a real sample Matter Page.
    #[test]
    fn the_navigator_page_shows_declared_matter_pages() {
        let content = super::navigator(&views::brand::DEFAULT_BRANDING);
        let steps = content
            .bands
            .iter()
            .find_map(|band| match band {
                Band::Steps { anchor, items, .. } if anchor == "declare" => Some(items),
                _ => None,
            })
            .expect("the Navigator page walks the declarative loop");
        let images: Vec<_> = steps
            .iter()
            .filter_map(|step| step.image.as_ref())
            .collect();
        let registered: Vec<String> = views::assets::NAVIGATOR_MATTER_PAGE_SCREENSHOTS
            .iter()
            .map(|key| views::assets::asset_url(key))
            .collect();
        assert_eq!(
            images
                .iter()
                .map(|image| image.src.clone())
                .collect::<Vec<_>>(),
            registered,
            "each screenshot is a registered public asset, in order"
        );
        assert!(images.iter().all(|image| !image.alt.is_empty()));
    }

    /// The connected-Project diagram names the Project's work surfaces.
    #[test]
    fn the_navigator_page_maps_connected_project_surfaces() {
        let content = super::navigator(&views::brand::DEFAULT_BRANDING);
        let diagram = content
            .bands
            .iter()
            .find_map(|band| match band {
                Band::ProjectNetwork {
                    left,
                    right,
                    mcp_tools,
                    agentic_coding_tools,
                    saas_tools,
                    ..
                } => Some((left, right, mcp_tools, agentic_coding_tools, saas_tools)),
                _ => None,
            })
            .expect("the Navigator page renders its connected-Project diagram");

        let left_labels: Vec<&str> = diagram.0.iter().map(|node| node.label.as_str()).collect();
        assert_eq!(
            left_labels,
            [
                "Internal Slack",
                "Internal Notion",
                "Project repository",
                "Matter Pages"
            ]
        );
        let right_labels: Vec<&str> = diagram.1.iter().map(|node| node.label.as_str()).collect();
        assert_eq!(
            right_labels,
            [
                "Shared Slack",
                "Per-Project Inbox",
                "Google Drive folder",
                "Shared Notion"
            ]
        );
        assert!(diagram.0[2].detail.contains("Git history"));
        assert!(diagram.0[3].detail.contains("declared in navigator.yaml"));
        assert_eq!(diagram.1[2].detail, "Large document intake");
        assert_eq!(
            diagram.1[3].detail,
            "Client collaboration when the Project uses it."
        );
        let mcp_tools: Vec<&str> = diagram.2.iter().map(String::as_str).collect();
        assert_eq!(
            mcp_tools,
            ["Court Listener", "Descrybe", "Exa", "Midpage", "Perplexity"]
        );
        let agentic_coding_tools: Vec<&str> = diagram.3.iter().map(String::as_str).collect();
        assert_eq!(
            agentic_coding_tools,
            ["Antigravity", "Claude Code", "Codex", "Cursor"]
        );
        let saas_tools: Vec<&str> = diagram.4.iter().map(String::as_str).collect();
        assert_eq!(
            saas_tools,
            [
                "Descript",
                "DocuSign",
                "Google Workspace",
                "Linear",
                "Mercury",
                "Restate",
                "SurrealDB",
                "Twilio",
                "Xero"
            ]
        );
    }

    /// The version-control case is argued without a claim the firm cannot
    /// substantiate.
    ///
    /// This is the guard the drafting of that section needed. The thesis it came
    /// from called version control "the most token-efficient way to write legal
    /// documents over time" — a superlative no one can defend under RPC 7.1, on
    /// a page that is lawyer advertising in California, Nevada, and Washington.
    /// The page makes the substantiable claim instead: a revision costs the
    /// change rather than the whole document.
    ///
    /// The banned list is a floor, not the whole rule. A superlative that is not
    /// spelled here is still a superlative.
    #[test]
    fn the_navigator_page_publishes_no_superlative_and_no_turnaround_promise() {
        let content = super::navigator(&views::brand::DEFAULT_BRANDING);
        let text = format!(
            "{} {} {}",
            content.tagline,
            page_text(&content.bands),
            content.meta_description
        );
        let lowered = text.to_lowercase();
        for banned in [
            "most token-efficient",
            "fastest",
            "cheapest",
            "world-class",
            "cutting-edge",
            "industry-leading",
            "best-in-class",
            "premier",
            "guarantee",
            "certified",
        ] {
            assert!(
                !lowered.contains(banned),
                "the platform page must not publish {banned:?}: {text}"
            );
        }
        // A turnaround on this page would be a service commitment about work the
        // platform does not do — the retired fractional-GC page was where a
        // redline turnaround belonged, and it is gone.
        assert!(
            !lowered.contains("business day") && !lowered.contains("turnaround"),
            "the platform page promises no turnaround: {text}"
        );
    }
}
