//! Daily manual billing and a Project-grouped review of a lawyer's time.
#[cfg(any(feature = "server", test))]
use std::collections::BTreeMap;

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{people::ViewerRole, portal_project_detail::format_usd};

pub const PATH: &str = "/app/lawyer/billing";

#[derive(Clone, Serialize, Deserialize)]
struct ProjectOption {
    code: String,
    name: String,
    rate: Option<i32>,
    entries: Group,
}

#[derive(Clone, Serialize, Deserialize)]
struct Line {
    description: String,
    minutes: i32,
    rate: i32,
    cents: i64,
    status: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Group {
    lines: Vec<Line>,
    minutes: i64,
    cents: i64,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum BillingPage {
    ChooseDay { attorney: Option<Uuid> },
    Day(Box<BillingDay>),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct BillingDay {
    day: chrono::NaiveDate,
    actor: Uuid,
    selected: Uuid,
    attorneys: Vec<(Uuid, String)>,
    projects: Vec<ProjectOption>,
    role: ViewerRole,
    logo: Option<crate::components::AppLogo>,
    tokens: String,
    csrf: String,
    flash: Option<String>,
    error: Option<String>,
}

#[cfg(feature = "server")]
#[derive(Deserialize)]
struct BillingFilter {
    attorney: Option<Uuid>,
    day: Option<chrono::NaiveDate>,
    flash: Option<String>,
    error: Option<String>,
}

#[server]
pub async fn get_billing_day() -> Result<BillingPage, ServerFnError> {
    use crate::lawyer_project_detail::server_error;
    use dioxus_fullstack_core::FullstackContext;
    let role = crate::admin_listing::require_lawyer().await?;
    let actor = crate::admin_listing::injected_person_id()
        .await
        .ok_or_else(|| ServerFnError::new("A signed-in attorney is required."))?;
    let axum::extract::Query(query) =
        FullstackContext::extract::<axum::extract::Query<BillingFilter>, _>().await?;
    let Some(day) = query.day else {
        return Ok(BillingPage::ChooseDay {
            attorney: query.attorney,
        });
    };
    let db = consume_context::<store::surreal::SurrealDb>();
    let brand = crate::app_chrome::request_brand().await;
    let mut projects = store::access::visible_lawyer_projects_for_brand(
        &db,
        Some(actor),
        super::store_role(role),
        brand.as_str(),
    )
    .await
    .map_err(server_error)?;
    projects.sort_by(|a, b| (&a.name, &a.code).cmp(&(&b.name, &b.code)));
    let pg = store::postgres::PostgresDb::from_env()
        .await
        .map_err(server_error)?;
    let ids: Vec<_> = projects.iter().map(|p| p.id).collect();
    let entries = store::time_entries::for_project_day(&pg, &ids, day)
        .await
        .map_err(server_error)?;
    let attorneys = store::time_entries::project_timekeepers(&pg, &ids, actor)
        .await
        .map_err(server_error)?;
    let selected = query
        .attorney
        .filter(|id| attorneys.iter().any(|(attorney, _)| attorney == id))
        .unwrap_or(actor);
    let mut groups = BTreeMap::new();
    for entry in entries.into_iter().filter(|e| e.attorney_id == selected) {
        let written_off = matches!(
            entry.billing_status,
            store::time_entries::BillingStatus::WrittenOff { .. }
        );
        let cents = if entry.billable && !written_off {
            store::time_entries::entry_amount_cents(entry.minutes, entry.rate_cents_per_hour)
        } else {
            0
        };
        add_line(
            &mut groups,
            entry.project_id,
            Line {
                description: entry.description,
                minutes: entry.minutes,
                rate: entry.rate_cents_per_hour,
                cents,
                status: if written_off {
                    "Written off"
                } else if !entry.billable {
                    "No charge"
                } else {
                    ""
                }
                .into(),
            },
        );
    }
    let rates = store::attorney_rates::resolve_hourly_rates_cents(&pg, actor, &ids, day)
        .await
        .map_err(server_error)?;
    let projects = projects
        .into_iter()
        .map(|p| ProjectOption {
            rate: rates.get(&p.id).copied(),
            code: p.code,
            name: p.name,
            entries: groups.remove(&p.id).unwrap_or_default(),
        })
        .collect();
    let axum::Extension(csrf) =
        FullstackContext::extract::<axum::Extension<crate::csrf::CsrfToken>, _>().await?;
    Ok(BillingPage::Day(Box::new(BillingDay {
        actor,
        selected,
        day,
        role,
        projects,
        attorneys,
        csrf: csrf.0,
        flash: query.flash,
        error: query.error,
        tokens: crate::app_chrome::app_tokens_href_from_context().await,
        logo: crate::app_chrome::app_logo_from_context().await,
    })))
}

#[cfg(any(feature = "server", test))]
fn add_line(groups: &mut BTreeMap<Uuid, Group>, project: Uuid, line: Line) {
    let group = groups.entry(project).or_default();
    group.minutes += i64::from(line.minutes);
    group.cents += line.cents;
    group.lines.push(line);
}

fn hours(minutes: i64) -> String {
    if minutes % 6 == 0 {
        format!("{}.{}", minutes / 60, (minutes % 60) / 6)
    } else {
        // Existing timers and manual entries can contain individual minutes.
        format!("{}h {}m", minutes / 60, minutes % 60)
    }
}

#[component]
pub fn DailyBilling() -> Element {
    let result = use_server_future(get_billing_day)?;
    let day = result.read();
    match &*day {
        Some(Ok(BillingPage::Day(view))) => billing_body(view),
        Some(Ok(BillingPage::ChooseDay { attorney })) => rsx! {
            document::Title { "Daily billing" }
            document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
            document::Script { src: "/public/js/billing-work-date.js", defer: true }
            main { class: "nav-theme daily-billing",
                h1 { "Daily billing" }
                form { method: "get", action: PATH,
                    if let Some(id) = attorney { input { r#type: "hidden", name: "attorney", value: "{id}" } }
                    label { class: "nav-label", "Work date"
                        input { class: "nav-input", r#type: "date", name: "day", required: true }
                    }
                    button { class: "nav-btn nav-btn--secondary", r#type: "submit", "View entries" }
                }
            }
        },
        Some(Err(_)) => rsx! { main { p { role: "alert", "Billing is unavailable." } } },
        None => rsx! { main { p { "Loading billing…" } } },
    }
}

fn billing_body(view: &BillingDay) -> Element {
    let selected = view.selected;
    rsx! {
        document::Title { "Daily billing" }
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{view.tokens}" }
        crate::components::AppNavbar { destinations: crate::app_chrome::app_destinations(view.role), logo: view.logo.clone() }
        main { class: "nav-theme daily-billing",
            a { class: "nav-link", href: "/app/lawyer", "Lawyer workbench" }
            h1 { "Daily billing" }
            form { class: "daily-billing__filters", method: "get", action: PATH,
                label { class: "nav-label", "Attorney"
                    select { class: "nav-select", name: "attorney",
                        for (id, label) in &view.attorneys {
                            option { value: "{id}", selected: *id == selected, "{label}" }
                        }
                    }
                }
                label { class: "nav-label", "Work date"
                    input { class: "nav-input", r#type: "date", name: "day", value: "{view.day}", required: true }
                }
                button { class: "nav-btn nav-btn--secondary", r#type: "submit", "View entries" }
            }
            if let Some(message) = &view.error { p { role: "alert", class: "nav-form-error", "{message}" } }
            if let Some(message) = &view.flash { p { role: "status", "{message}" } }
            if selected == view.actor {
                if view.projects.is_empty() {
                    p { "You have no Projects available for time entry." }
                } else {
                    form { class: "daily-billing__entry", method: "post", action: PATH, aria_label: "Add my time",
                        input { r#type: "hidden", name: "_csrf", value: "{view.csrf}" }
                        input { r#type: "hidden", name: "performed_on", value: "{view.day}" }
                        label { class: "nav-label daily-billing__project", "Project · hourly rate"
                            select { class: "nav-select", name: "project", required: true,
                                option { value: "", "Choose a Project" }
                                for p in &view.projects {
                                    option { value: "{p.code}",
                                        "{p.name} ({p.code}) · "
                                        if let Some(rate) = p.rate { "{format_usd(i64::from(rate))}/hr" }
                                        else { "No hourly rate · No-charge work only" }
                                    }
                                }
                            }
                        }
                        label { class: "nav-label", "Work performed"
                            textarea { class: "nav-input", name: "description", rows: 2, maxlength: 2000, required: true }
                        }
                        label { class: "nav-label", "Minutes"
                            input { class: "nav-input", r#type: "number", name: "minutes", min: "1", max: "1440", step: "1", inputmode: "numeric", required: true }
                        }
                        label { class: "nav-label",
                            input { class: "nav-checkbox", r#type: "checkbox", name: "billable", checked: true }
                            "Billable"
                        }
                        button { class: "nav-btn nav-btn--primary", r#type: "submit", "Add entry" }
                    }
                }
            } else {
                a { class: "nav-link", href: "{PATH}?day={view.day}", "Enter my time" }
            }
            {ledger(&view.projects)}
        }
    }
}

fn ledger(projects: &[ProjectOption]) -> Element {
    let groups: Vec<_> = projects
        .iter()
        .filter(|p| !p.entries.lines.is_empty())
        .collect();
    let minutes = groups.iter().map(|p| p.entries.minutes).sum();
    let cents = groups.iter().map(|p| p.entries.cents).sum();
    rsx! {
        div { class: "nav-table-wrap",
            table { class: "nav-table daily-billing__ledger", aria_label: "Entries grouped by Project",
                thead { tr { th { scope: "col", "Project / work performed" } th { scope: "col", "Hours" }
                    th { scope: "col", "Rate" } th { scope: "col", "Amount" } } }
                if groups.is_empty() { tbody { tr { td { colspan: 4, "No entries for this attorney on this date." } } } }
                for project in groups {
                    tbody {
                        tr { class: "daily-billing__group", th { scope: "rowgroup", colspan: 4,
                            a { class: "nav-link", href: "/app/projects/{project.code}/billing", "{project.name} ({project.code})" }
                        } }
                        for line in &project.entries.lines {
                            tr { td { "{line.description}" if !line.status.is_empty() { " · {line.status}" } }
                                td { "{hours(i64::from(line.minutes))}" }
                                td { "{format_usd(i64::from(line.rate))}" } td { "{format_usd(line.cents)}" }
                            }
                        }
                        tr { class: "daily-billing__subtotal", th { scope: "row", "Project subtotal" }
                            td { "{hours(project.entries.minutes)}" } td {} td { "{format_usd(project.entries.cents)}" } }
                    }
                }
                tfoot { tr { th { scope: "row", "Billing total" } td { "{hours(minutes)}" } td {} td { "{format_usd(cents)}" } } }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_billing_only_offers_entry_for_the_signed_in_attorney() {
        let actor = Uuid::from_u128(1);
        let other = Uuid::from_u128(2);
        let mut day = BillingDay {
            day: chrono::NaiveDate::from_ymd_opt(2026, 11, 23).unwrap(),
            actor,
            selected: actor,
            attorneys: vec![
                (actor, "First attorney".into()),
                (other, "Second attorney".into()),
            ],
            projects: vec![ProjectOption {
                code: "sample".into(),
                name: "Sample matter".into(),
                rate: Some(32500),
                entries: Group::default(),
            }],
            role: ViewerRole::Lawyer,
            logo: None,
            tokens: String::new(),
            csrf: "test".into(),
            flash: None,
            error: None,
        };
        let own = dioxus_ssr::render_element(billing_body(&day));
        assert!(own.contains("method=\"post\""));
        assert!(own.contains("name=\"minutes\""), "enter actual minutes");
        assert!(own.contains("name=\"billable\""), "choose whether to bill");
        assert!(own.contains("$325.00/hr"));
        day.projects[0].rate = None;
        let unrated = dioxus_ssr::render_element(billing_body(&day));
        let option = unrated.split("value=\"sample\"").nth(1).unwrap();
        assert!(!option.split('>').next().unwrap().contains("disabled"));
        day.selected = other;
        let review = dioxus_ssr::render_element(billing_body(&day));
        assert!(!review.contains("aria-label=\"Add my time\""));
        assert!(review.contains("Enter my time"));
    }

    #[test]
    fn daily_billing_groups_interleaved_entries_and_sums_line_amounts() {
        let mut groups = BTreeMap::new();
        for (project, mins, cents) in [(1, 30, 5000), (2, 36, 7200), (1, 6, 1000)] {
            add_line(
                &mut groups,
                Uuid::from_u128(project),
                Line {
                    description: "Reviewed correspondence".into(),
                    minutes: mins,
                    rate: 10000,
                    cents,
                    status: String::new(),
                },
            );
        }
        assert_eq!(groups.len(), 2);
        let first = &groups[&Uuid::from_u128(1)];
        assert_eq!(
            (first.lines.len(), first.minutes, first.cents),
            (2, 36, 6000)
        );
        let projects: Vec<_> = groups
            .into_iter()
            .map(|(id, entries)| ProjectOption {
                code: id.to_string(),
                name: "Sample".into(),
                rate: Some(10000),
                entries,
            })
            .collect();
        let html = dioxus_ssr::render_element(ledger(&projects));
        assert!(html.contains("$132.00"));
        assert!(html.contains("1.2"));
        assert_eq!(hours(61), "1h 1m");
    }
}
