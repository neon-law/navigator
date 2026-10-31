//! The participation ledger: who is assigned to a matter, their system tier,
//! the DRI accountability markers, and the admin controls that govern them —
//! shared by the lawyer workbench and the admin-unassigned rendering
//! ([`crate::admin_unassigned_project_detail`]).

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

#[cfg(feature = "server")]
use crate::lawyer_project_detail::server_error;

/// One participation-ledger row: who is assigned, their system tier, the
/// participation derived from it, and the row id for the edit/remove actions.
/// Both columns render because the tier is what *explains* the participation —
/// and a legacy row where the two disagree is worth seeing.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct ParticipationRow {
    pub id: String,
    pub person_name: String,
    pub person_email: String,
    pub person_role: String,
    pub participation: String,
    /// The accountability markers this row carries. Each side is a set, so any
    /// number of rows may hold one — the lawyer markers name everyone who
    /// answers for the matter, and any of them may close it.
    pub is_lawyer_dri: bool,
    pub is_client_dri: bool,
    /// A same-origin avatar route when the person has an image; the stored
    /// object key never crosses the server/client view boundary.
    pub avatar_url: Option<String>,
}

/// Project a matter's raw participation rows and their linked people into the
/// rendered ledger shape. Pure (no I/O) so a caller that already holds both
/// batches — [`get_lawyer_project_detail`] fetches them for the DRI names
/// anyway — pays no second query to build the table too.
#[cfg(feature = "server")]
pub(crate) fn to_participation_rows(
    participation_rows: &[store::projects::PersonProjectRole],
    people: &[store::persons::Person],
) -> Vec<ParticipationRow> {
    let people_by_id: std::collections::HashMap<_, _> = people.iter().map(|p| (p.id, p)).collect();
    participation_rows
        .iter()
        .filter_map(|row| {
            people_by_id.get(&row.person_id).map(|p| ParticipationRow {
                id: row.id.to_string(),
                person_name: p.name.clone(),
                person_email: p.email.clone(),
                person_role: p.role.as_str().to_string(),
                participation: row.participation.clone(),
                is_lawyer_dri: row.is_lawyer_dri,
                is_client_dri: row.is_client_dri,
                avatar_url: p
                    .profile_image_url
                    .as_ref()
                    .map(|_| format!("/app/people/{}/avatar", p.id)),
            })
        })
        .collect()
}

/// The participation ledger for one matter, in the same shape the lawyer
/// workbench renders — the rows plus the linked people in one batched query,
/// so the system tier is visible without conflating it with participation.
///
/// Shared with [`crate::admin_unassigned_project_detail`], which renders the
/// same ledger for an Owner/Admin who has no row on the matter: the ledger
/// itself carries no confidential matter content, so both callers may read it
/// once each resolves that its own caller may.
#[cfg(feature = "server")]
pub(crate) async fn participation_rows_for(
    surreal: &store::surreal::SurrealDb,
    project_id: uuid::Uuid,
) -> Result<Vec<ParticipationRow>, ServerFnError> {
    let participation_rows = store::projects::participations_for_project(surreal, project_id)
        .await
        .map_err(server_error)?;
    let people = store::persons::find_by_ids(
        surreal,
        &participation_rows
            .iter()
            .map(|r| r.person_id)
            .collect::<Vec<_>>(),
    )
    .await
    .map_err(server_error)?;
    Ok(to_participation_rows(&participation_rows, &people))
}

/// Whether the workbench should offer lawyer-side DRI controls.
///
/// Mirrors `store::participation::authorize` for a viewer already on this
/// matter. `may_bootstrap` is the empty-set case: a lawyer-tier participant
/// already on the matter may name the first DRI because there is no current
/// holder to admit the designation.
pub(crate) fn may_govern_lawyer_dri(
    is_admin: bool,
    viewer_is_lawyer_dri: bool,
    may_bootstrap: bool,
) -> bool {
    is_admin || viewer_is_lawyer_dri || may_bootstrap
}

/// The matter's "Matter people" ledger table: everyone assigned, their system
/// tier, the derived participation, and the accountability controls — with
/// admin add/edit/remove.
///
/// Shared by [`LawyerProjectDetail`] (a participant's own workbench) and
/// [`crate::admin_unassigned_project_detail::AdminUnassignedProjectDetail`]
/// (an Owner/Admin who holds no row on this matter, and so may still manage
/// who does). `is_admin` is always `true` for the second caller — only
/// Owner/Admin reach that page — and `may_govern_lawyer_side` /
/// `may_govern_client_side` are always `true` there too, mirroring
/// `store::participation::authorize`'s unconditional Owner/Admin pass.
#[component]
pub fn ParticipationTable(
    code: String,
    csrf: String,
    participations: Vec<ParticipationRow>,
    is_admin: bool,
    may_govern_lawyer_side: bool,
    may_govern_client_side: bool,
) -> Element {
    rsx! {
        section { class: "lawyer-detail__section project-participations",
            div { class: "lawyer-detail__section-head",
                h2 { "Project people" }
                if is_admin {
                    a { class: "nav-btn nav-btn--primary", href: "/app/projects/{code}/people/new", "Add person" }
                }
            }
            p { class: "nav-muted", "Participation records who is assigned to this project and follows each person's system tier. Adding or removing someone here does not change that tier." }
            if participations.is_empty() {
                p { class: "projects-empty", "No people are assigned to this project yet." }
            } else {
                div { class: "nav-table-wrap",
                    table { class: "nav-table",
                        thead {
                            tr {
                                th { scope: "col", "Person" }
                                th { scope: "col", "System tier" }
                                th { scope: "col", "Participation" }
                                th { scope: "col", "Accountability" }
                                if is_admin {
                                    th { scope: "col", class: "nav-table__end", "" }
                                }
                            }
                        }
                        tbody {
                            for row in participations.iter() {
                                tr {
                                    td {
                                        strong { "{row.person_name}" }
                                        " "
                                        span { class: "nav-muted", "<" "{row.person_email}" ">" }
                                    }
                                    td { span { class: "status-chip", "{row.person_role}" } }
                                    td { code { "{row.participation}" } }
                                    // The accountability marker rides the
                                    // participation row, so the ledger is
                                    // where it belongs on the page — and so
                                    // is the control that moves it. Each
                                    // side is a set, so the control adds and
                                    // removes rather than reassigning.
                                    td { class: "matter-dri-cell",
                                        if row.is_lawyer_dri {
                                            span { class: "status-chip", "Lawyer DRI" }
                                        } else if row.is_client_dri {
                                            span { class: "status-chip", "Client DRI" }
                                        } else {
                                            span { class: "nav-muted", "—" }
                                        }
                                        {
                                            // A firm-side row answers to the
                                            // lawyer rule, a client-side row
                                            // to the client rule; the tier on
                                            // the row is what decides which.
                                            let firm_side = !row.participation.eq("client");
                                            let may = if firm_side { may_govern_lawyer_side } else { may_govern_client_side };
                                            let holds = row.is_lawyer_dri || row.is_client_dri;
                                            rsx! {
                                                if may {
                                                    " "
                                                    form {
                                                        class: "lawyer-detail__inline-form",
                                                        method: "post",
                                                        action: if holds {
                                                            format!("/app/projects/{code}/people/{}/dri/remove", row.id)
                                                        } else {
                                                            format!("/app/projects/{code}/people/{}/dri", row.id)
                                                        },
                                                        input { r#type: "hidden", name: "_csrf", value: "{csrf}" }
                                                        button {
                                                            class: "nav-btn nav-btn--secondary matter-dri-toggle",
                                                            r#type: "submit",
                                                            if holds { "Remove DRI" } else { "Make DRI" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    if is_admin {
                                        td { class: "nav-table__end",
                                            a {
                                                class: "nav-btn nav-btn--secondary",
                                                href: "/app/projects/{code}/people/{row.id}/edit",
                                                "Edit"
                                            }
                                            " "
                                            form {
                                                class: "lawyer-detail__inline-form",
                                                method: "post",
                                                action: "/app/projects/{code}/people/{row.id}/delete",
                                                input { r#type: "hidden", name: "_csrf", value: "{csrf}" }
                                                button { class: "nav-btn nav-btn--danger", r#type: "submit", "Remove" }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, lawyer_dri: bool) -> ParticipationRow {
        ParticipationRow {
            id: id.to_string(),
            person_name: "Aries Attorney".to_string(),
            person_email: "aries@neonlaw.com".to_string(),
            person_role: "lawyer".to_string(),
            participation: "lawyer".to_string(),
            is_lawyer_dri: lawyer_dri,
            is_client_dri: false,
            avatar_url: None,
        }
    }

    fn render(
        participations: Vec<ParticipationRow>,
        may_govern_lawyer_side: bool,
        is_admin: bool,
    ) -> String {
        render_for_code(
            "sample-litigation",
            participations,
            may_govern_lawyer_side,
            is_admin,
        )
    }

    fn render_for_code(
        code: &str,
        participations: Vec<ParticipationRow>,
        may_govern_lawyer_side: bool,
        is_admin: bool,
    ) -> String {
        dioxus_ssr::render_element(rsx! {
            ParticipationTable {
                code: code.to_string(),
                csrf: "TOK".to_string(),
                participations,
                is_admin,
                may_govern_lawyer_side,
                may_govern_client_side: true,
            }
        })
    }

    #[test]
    fn a_governable_row_posts_make_dri_with_the_session_csrf() {
        let html = render(
            vec![row("00000000-0000-0000-0000-0000000000aa", false)],
            true,
            false,
        );
        assert!(html.contains("Make DRI"), "{html}");
        assert!(
            html.contains(
                r#"action="/app/projects/sample-litigation/people/00000000-0000-0000-0000-0000000000aa/dri""#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"name="_csrf" value="TOK""#),
            "the workbench control must echo the session token: {html}"
        );
    }

    #[test]
    fn a_held_row_posts_remove_dri() {
        let html = render(
            vec![row("00000000-0000-0000-0000-0000000000aa", true)],
            true,
            false,
        );
        assert!(html.contains("Remove DRI"), "{html}");
        assert!(
            html.contains(
                r#"action="/app/projects/sample-litigation/people/00000000-0000-0000-0000-0000000000aa/dri/remove""#
            ),
            "{html}"
        );
        assert!(html.contains(">Lawyer DRI<"), "{html}");
    }

    #[test]
    fn a_viewer_who_cannot_govern_sees_the_label_and_no_control() {
        let html = render(
            vec![row("00000000-0000-0000-0000-0000000000aa", true)],
            false,
            false,
        );
        assert!(html.contains(">Lawyer DRI<"), "{html}");
        assert!(!html.contains("Make DRI"), "{html}");
        assert!(!html.contains("Remove DRI"), "{html}");
    }

    #[test]
    #[cfg(feature = "server")]
    fn participation_projection_joins_people_and_omits_unresolved_rows() {
        let person_id = uuid::Uuid::now_v7();
        let project_id = uuid::Uuid::now_v7();
        let person = store::persons::Person {
            id: person_id,
            name: "Aries Attorney".to_string(),
            given_name: None,
            family_name: None,
            middle_name: None,
            email: "aries@example.test".to_string(),
            oidc_subject: None,
            microsoft_subject: None,
            apple_subject: None,
            role: store::persons::Role::Lawyer,
            title: None,
            phone: None,
            xero_contact_id: None,
            profile_image_url: Some("https://example.test/avatar.png".to_string()),
            linkedin_url: None,
            email_confirmed: false,
            inserted_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let participation = |person_id| store::projects::PersonProjectRole {
            id: uuid::Uuid::now_v7(),
            person_id,
            project_id,
            participation: "lawyer".to_string(),
            is_lawyer_dri: true,
            is_client_dri: false,
            inserted_at: "2026-09-25T00:00:00Z".to_string(),
            updated_at: "2026-09-25T00:00:00Z".to_string(),
        };
        let rows = to_participation_rows(
            &[
                participation(person_id),
                participation(uuid::Uuid::now_v7()),
            ],
            &[person],
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].person_name, "Aries Attorney");
        assert_eq!(rows[0].person_email, "aries@example.test");
        assert_eq!(rows[0].person_role, "lawyer");
        assert_eq!(rows[0].participation, "lawyer");
        assert!(rows[0].is_lawyer_dri);
        assert!(!rows[0].is_client_dri);
        assert_eq!(
            rows[0].avatar_url.as_deref(),
            Some(format!("/app/people/{person_id}/avatar").as_str())
        );
    }

    #[test]
    fn an_empty_lawyer_set_lets_a_lawyer_on_the_matter_govern() {
        assert!(may_govern_lawyer_dri(false, false, true));
        assert!(!may_govern_lawyer_dri(false, false, false));
        assert!(may_govern_lawyer_dri(true, false, true));
        assert!(may_govern_lawyer_dri(false, true, false));
    }
}
