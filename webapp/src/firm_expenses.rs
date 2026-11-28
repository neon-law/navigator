//! The Firm's confirmed, unbilled Project Costs, limited to the viewer's matters.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmExpenses {
    unavailable: bool,
    matters: Vec<MatterExpenses>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MatterExpenses {
    code: String,
    name: String,
    groups: Vec<ExpenseGroup>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ExpenseGroup {
    category: String,
    currency: String,
    total: String,
    costs: Vec<ExpenseLine>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ExpenseLine {
    id: Uuid,
    date: String,
    description: String,
    amount: String,
    receipt_id: Option<Uuid>,
}

#[component]
pub fn UnbilledExpenses(view: FirmExpenses) -> Element {
    rsx! {
        section { id: "firm-unbilled-expenses", class: "nav-section",
            h2 { "Unbilled expenses" }
            if view.unavailable {
                p { "Unbilled expenses are unavailable." }
            } else if view.matters.is_empty() {
                p { "No confirmed, unbilled expenses on your matters." }
            } else {
                for matter in &view.matters {
                    article { key: "{matter.code}",
                        h3 { a { class: "nav-link", href: "/app/projects/{matter.code}/finances", "{matter.name}" } }
                        div { class: "nav-table-wrap",
                            table { class: "nav-table", aria_label: "Unbilled expenses for {matter.name}",
                                thead { tr {
                                    th { scope: "col", "Category" }
                                    th { scope: "col", "Date" }
                                    th { scope: "col", "Expense" }
                                    th { scope: "col", "Amount" }
                                    th { scope: "col", "Receipt" }
                                } }
                                for group in &matter.groups {
                                    tbody { key: "{group.category}-{group.currency}",
                                        for cost in &group.costs {
                                            tr { key: "{cost.id}",
                                                td { "{group.category}" }
                                                td { "{cost.date}" }
                                                th { scope: "row", "{cost.description}" }
                                                td { "{cost.amount}" }
                                                td {
                                                    if let Some(id) = cost.receipt_id {
                                                        a { class: "nav-link", href: "/app/projects/{matter.code}/documents/{id}",
                                                            aria_label: "View receipt for {cost.description}", "View receipt" }
                                                    } else {
                                                        "Receipt missing"
                                                    }
                                                }
                                            }
                                        }
                                        tr {
                                            th { scope: "row", colspan: "3", "{group.category} total" }
                                            td { "{group.total}" }
                                            td {}
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

pub(crate) fn format_expense(cents: i128, currency: &str) -> String {
    if let ("USD", Ok(cents)) = (currency, i64::try_from(cents)) {
        return crate::portal_project_detail::format_usd(cents);
    }
    format!("{currency} {}.{:02}", cents / 100, cents % 100)
}

#[cfg(feature = "server")]
pub(crate) async fn load(
    application_store: &store::application_store::ApplicationStore,
    person_id: Option<Uuid>,
    role: store::persons::Role,
    firm_id: Uuid,
) -> FirmExpenses {
    let result = load_scoped(application_store, person_id, role, firm_id).await;
    match result {
        Ok(matters) => FirmExpenses {
            unavailable: false,
            matters,
        },
        Err(_) => FirmExpenses {
            unavailable: true,
            ..FirmExpenses::default()
        },
    }
}

#[cfg(feature = "server")]
async fn load_scoped(
    application_store: &store::application_store::ApplicationStore,
    person_id: Option<Uuid>,
    role: store::persons::Role,
    firm_id: Uuid,
) -> Result<Vec<MatterExpenses>, String> {
    use std::collections::HashMap;
    let mut projects =
        store::access::visible_projects_as_lawyer(application_store, person_id, role).await?;
    projects.retain(|project| project.firm_id == Some(firm_id));
    projects.sort_by(|a, b| (&a.name, &a.code).cmp(&(&b.name, &b.code)));
    if projects.is_empty() {
        return Ok(Vec::new());
    }
    let project_ids: Vec<_> = projects.iter().map(|p| p.id).collect();
    let pg = store::postgres::PostgresDb::from_env()
        .await
        .map_err(|error| error.to_string())?;
    let rows = store::project_costs::for_expense_review(&pg, &project_ids, true)
        .await
        .map_err(|error| error.to_string())?;
    let mut by_project = HashMap::<_, Vec<_>>::new();
    for row in rows {
        by_project.entry(row.project_id).or_default().push(row);
    }
    Ok(projects
        .into_iter()
        .filter_map(|project| {
            let rows = by_project.remove(&project.id)?;
            Some(MatterExpenses {
                code: project.code,
                name: project.name,
                groups: group_expenses(rows),
            })
        })
        .collect())
}

#[cfg(feature = "server")]
fn group_expenses(rows: Vec<store::project_costs::ExpenseReviewRow>) -> Vec<ExpenseGroup> {
    use std::collections::BTreeMap;
    let mut groups = BTreeMap::<_, (i128, Vec<_>)>::new();
    for row in rows {
        let amount = format_expense(i128::from(row.amount_cents), &row.currency);
        let (sum, costs) = groups
            .entry((row.category.label(), row.currency))
            .or_default();
        *sum += i128::from(row.amount_cents);
        costs.push(ExpenseLine {
            id: row.id,
            date: row.occurred_on.to_string(),
            description: row.description,
            amount,
            receipt_id: row.receipt_id,
        });
    }
    groups
        .into_iter()
        .map(|((category, currency), (sum, costs))| ExpenseGroup {
            total: format_expense(sum, &currency),
            category: category.into(),
            currency,
            costs,
        })
        .collect()
}

#[cfg(all(feature = "server", test))]
mod tests {
    use super::*;
    use store::documents::ExpenseCategory;
    use store::project_costs::ExpenseReviewRow;

    #[test]
    fn expense_schedule_totals_by_category_and_currency_and_links_only_existing_receipts() {
        let receipt = Uuid::new_v4();
        let rows = [
            (ExpenseCategory::CourtFilingFee, "USD", 1200, Some(receipt)),
            (ExpenseCategory::CourtFilingFee, "USD", 150, None),
            (ExpenseCategory::CourtFilingFee, "CAD", 2100, None),
            (ExpenseCategory::Transcript, "USD", 900, None),
        ]
        .into_iter()
        .map(
            |(category, currency, amount_cents, receipt_id)| ExpenseReviewRow {
                id: Uuid::new_v4(),
                project_id: Uuid::nil(),
                occurred_on: chrono::NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
                payee: "Synthetic vendor".into(),
                description: "Synthetic expense".into(),
                amount_cents,
                currency: currency.into(),
                amount_status: "confirmed".into(),
                billing_status: "unbilled".into(),
                category,
                receipt_id,
            },
        )
        .collect();
        let groups = group_expenses(rows);
        assert_eq!(
            groups
                .iter()
                .map(|group| group.total.as_str())
                .collect::<Vec<_>>(),
            ["CAD 21.00", "$13.50", "$9.00"]
        );
        let view = FirmExpenses {
            matters: vec![MatterExpenses {
                code: "synthetic-expenses".into(),
                name: "Synthetic expenses".into(),
                groups,
            }],
            unavailable: false,
        };
        let mut dom = VirtualDom::new_with_props(UnbilledExpenses, UnbilledExpensesProps { view });
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        assert!(html.contains("/app/projects/synthetic-expenses/finances"));
        assert!(html.contains(&format!(
            "/app/projects/synthetic-expenses/documents/{receipt}"
        )));
        assert_eq!(html.matches("/documents/").count(), 1);
        assert!(html.contains("Receipt missing"));
        assert!(html.contains("Court filing fee total"));
    }
}
