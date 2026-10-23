//! The `relationship_graph` section: the matter's parties and the
//! [Conflict-Check Graph](../../../docs/glossary/conflict-check-graph.md)'s
//! edges among them.
//!
//! The diagram is inline SVG laid out on the server, so it needs no client
//! JavaScript. Every edge it draws is also a row of the table beneath it,
//! which is the accessible reading of the same graph and the one a screen
//! reader lands on. The section is the firm's conflicts record, so the
//! loader fills it on the lawyer lens only.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{Badge, BadgeTone, Empty, Panel};

/// One party on the graph.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphNode {
    pub label: String,
    /// `Person` or `Entity`.
    pub kind: String,
    /// How the party is on the matter: `Client`, `Lawyer`, or a reference
    /// reason.
    pub role: String,
}

/// One edge between two nodes, by index into [`GraphView::nodes`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphEdge {
    pub from: usize,
    pub to: usize,
    /// The relationship in plain words, e.g. `Adverse to`, `Manages`.
    pub kind: String,
    /// Whether the edge records adversity: drawn dashed.
    pub adverse: bool,
    pub confidence_pct: i32,
}

/// The `relationship_graph` section's data. No nodes is the empty state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphView {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

const WIDTH: f64 = 640.0;
const HEIGHT: f64 = 400.0;
const RADIUS: f64 = 140.0;
const LABEL_CHARS: usize = 22;

/// Where node `index` of `count` sits: evenly round a circle, the first at
/// the top. A single node sits at the centre.
fn position(index: usize, count: usize) -> (f64, f64, f64) {
    let (cx, cy) = (WIDTH / 2.0, HEIGHT / 2.0);
    if count <= 1 {
        return (cx, cy, 0.0);
    }
    let step = std::f64::consts::TAU / f64::from(u32::try_from(count).unwrap_or(u32::MAX));
    let angle = step * f64::from(u32::try_from(index).unwrap_or(0)) - std::f64::consts::FRAC_PI_2;
    (
        cx + RADIUS * angle.cos(),
        cy + RADIUS * angle.sin(),
        angle.cos(),
    )
}

fn short(label: &str) -> String {
    if label.chars().count() <= LABEL_CHARS {
        label.to_string()
    } else {
        let cut: String = label.chars().take(LABEL_CHARS - 1).collect();
        format!("{cut}…")
    }
}

/// The `relationship_graph` section.
#[component]
pub fn RelationshipGraphSection(view: GraphView) -> Element {
    let count = view.nodes.len();
    let points: Vec<(f64, f64, f64)> = (0..count).map(|i| position(i, count)).collect();
    let view_box = format!("0 0 {WIDTH} {HEIGHT}");
    let summary = format!(
        "{count} parties and {} relationships among them. The table below lists each one.",
        view.edges.len()
    );
    let unlinked: Vec<&GraphNode> = view
        .nodes
        .iter()
        .enumerate()
        .filter(|(i, _)| !view.edges.iter().any(|e| e.from == *i || e.to == *i))
        .map(|(_, node)| node)
        .collect();
    rsx! {
        section {
            class: "matter-section",
            "data-section": "relationship_graph",
            aria_labelledby: "section-relationship_graph-title",
            Panel {
                id: "section-relationship_graph".to_string(),
                title: "Relationships".to_string(),
                note: "The matter's parties and the conflict-check graph's ties among them. Only a lawyer can clear a conflict.".to_string(),
                if view.nodes.is_empty() {
                    div { class: "matter-section__empty",
                        Empty { title: "No parties are recorded on this matter yet.".to_string() }
                    }
                } else {
                    figure { class: "matter-graph",
                        svg {
                            class: "matter-graph__svg",
                            view_box: "{view_box}",
                            role: "img",
                            "aria-labelledby": "relationship-graph-title relationship-graph-desc",
                            title { id: "relationship-graph-title", "Relationship graph" }
                            desc { id: "relationship-graph-desc", "{summary}" }
                            for edge in view.edges.iter() {
                                if let (Some(a), Some(b)) = (points.get(edge.from), points.get(edge.to)) {
                                    line {
                                        class: if edge.adverse { "matter-graph__edge matter-graph__edge--adverse" } else { "matter-graph__edge" },
                                        x1: "{a.0:.1}",
                                        y1: "{a.1:.1}",
                                        x2: "{b.0:.1}",
                                        y2: "{b.1:.1}",
                                    }
                                }
                            }
                            for (node , point) in view.nodes.iter().zip(points.iter()) {
                                {
                                    let anchor = if point.2 > 0.2 { "start" } else if point.2 < -0.2 { "end" } else { "middle" };
                                    let label_x = point.0 + 14.0 * point.2;
                                    let label_y = if point.2.abs() <= 0.2 && point.1 < HEIGHT / 2.0 { point.1 - 14.0 } else if point.2.abs() <= 0.2 { point.1 + 22.0 } else { point.1 + 4.0 };
                                    let node_class = if node.kind == "Entity" { "matter-graph__node matter-graph__node--entity" } else { "matter-graph__node" };
                                    rsx! {
                                        g {
                                            circle { class: node_class, cx: "{point.0:.1}", cy: "{point.1:.1}", r: "8" }
                                            text {
                                                class: "matter-graph__label",
                                                x: "{label_x:.1}",
                                                y: "{label_y:.1}",
                                                text_anchor: anchor,
                                                "{short(&node.label)}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        figcaption { class: "matter-graph__caption", "{summary}" }
                    }
                    div { class: "nav-table-wrap",
                        table { class: "nav-table",
                            caption { "Relationships among the matter's parties" }
                            thead {
                                tr {
                                    th { scope: "col", "From" }
                                    th { scope: "col", "Relationship" }
                                    th { scope: "col", "To" }
                                    th { scope: "col", "Confidence" }
                                }
                            }
                            tbody {
                                if view.edges.is_empty() {
                                    tr {
                                        td { colspan: "4", "No relationships are recorded among these parties." }
                                    }
                                }
                                for edge in view.edges.iter() {
                                    if let (Some(from), Some(to)) = (view.nodes.get(edge.from), view.nodes.get(edge.to)) {
                                        tr { "data-adverse": if edge.adverse { "true" } else { "false" },
                                            td { "{from.label} " span { class: "nav-muted", "({from.role})" } }
                                            td {
                                                Badge {
                                                    tone: if edge.adverse { BadgeTone::Blocked } else { BadgeTone::Default },
                                                    "{edge.kind}"
                                                }
                                            }
                                            td { "{to.label} " span { class: "nav-muted", "({to.role})" } }
                                            td { "{edge.confidence_pct}%" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if !unlinked.is_empty() {
                        p { class: "matter-graph__unlinked",
                            "No recorded ties: "
                            for (i , node) in unlinked.iter().enumerate() {
                                if i > 0 { ", " }
                                "{node.label} ({node.role})"
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The server half: the matter's parties and the graph's edges among them.
#[cfg(feature = "server")]
pub mod derive {
    use super::{GraphEdge, GraphNode, GraphView};
    use store::conflicts::{Edge, NodeRef};
    use store::relationships::{Endpoint, KIND_ADVERSE_TO};

    fn words(raw: &str) -> String {
        let spaced = raw.replace(['_', '-'], " ");
        let mut chars = spaced.chars();
        chars.next().map_or_else(String::new, |first| {
            first.to_uppercase().collect::<String>() + chars.as_str()
        })
    }

    /// The graph from the matter's parties (each a node, a label, and a
    /// role) and the edges among them. Repeated nodes keep their first
    /// role; an edge to a node outside the list is dropped.
    #[must_use]
    pub fn graph(parties: &[(NodeRef, String, String)], edges: &[Edge]) -> GraphView {
        let mut seen: Vec<NodeRef> = Vec::new();
        let mut nodes = Vec::new();
        for (node, label, role) in parties {
            if seen.contains(node) {
                continue;
            }
            seen.push(*node);
            nodes.push(GraphNode {
                label: label.clone(),
                kind: match node.kind {
                    Endpoint::Person => "Person",
                    Endpoint::Entity => "Entity",
                }
                .to_string(),
                role: role.clone(),
            });
        }
        let index = |node: &NodeRef| seen.iter().position(|n| n == node);
        let edges = edges
            .iter()
            .filter_map(|edge| {
                Some(GraphEdge {
                    from: index(&edge.from)?,
                    to: index(&edge.to)?,
                    kind: words(&edge.kind),
                    adverse: edge.kind == KIND_ADVERSE_TO,
                    confidence_pct: edge.confidence_pct,
                })
            })
            .collect();
        GraphView { nodes, edges }
    }

    /// Read the matter's client Entity, its participants, and its Project
    /// References, then the graph's edges among exactly those parties.
    ///
    /// # Errors
    /// Any store error, as text.
    pub async fn load(
        surreal: &store::surreal::SurrealDb,
        project: &store::projects::Project,
    ) -> Result<GraphView, String> {
        let mut parties: Vec<(NodeRef, String, String)> = Vec::new();
        if let Some(entity) = store::entities::find_by_id(surreal, project.entity_id)
            .await
            .map_err(|e| e.to_string())?
        {
            parties.push((
                NodeRef::entity(entity.id),
                entity.name,
                "Client".to_string(),
            ));
        }
        let roles = store::projects::participations_for_project(surreal, project.id)
            .await
            .map_err(|e| e.to_string())?;
        let ids: Vec<uuid::Uuid> = roles.iter().map(|row| row.person_id).collect();
        let people = store::persons::find_by_ids(surreal, &ids)
            .await
            .map_err(|e| e.to_string())?;
        for row in &roles {
            if let Some(person) = people.iter().find(|p| p.id == row.person_id) {
                parties.push((
                    NodeRef::person(person.id),
                    person.name.clone(),
                    words(&row.participation),
                ));
            }
        }
        let references = store::project_references::for_project(surreal, project.id)
            .await
            .map_err(|e| e.to_string())?;
        let ids = |kind: Endpoint| -> Vec<uuid::Uuid> {
            references
                .iter()
                .filter(|row| row.party.kind == kind)
                .map(|row| row.party.id)
                .collect()
        };
        let ref_people = store::persons::find_by_ids(surreal, &ids(Endpoint::Person))
            .await
            .map_err(|e| e.to_string())?;
        let ref_entities = store::entities::find_by_ids(surreal, &ids(Endpoint::Entity))
            .await
            .map_err(|e| e.to_string())?;
        for row in &references {
            let name = match row.party.kind {
                Endpoint::Person => ref_people
                    .iter()
                    .find(|p| p.id == row.party.id)
                    .map(|p| p.name.clone()),
                Endpoint::Entity => ref_entities
                    .iter()
                    .find(|e| e.id == row.party.id)
                    .map(|e| e.name.clone()),
            };
            if let Some(name) = name {
                let node = match row.party.kind {
                    Endpoint::Person => NodeRef::person(row.party.id),
                    Endpoint::Entity => NodeRef::entity(row.party.id),
                };
                parties.push((node, name, words(&row.reason)));
            }
        }
        let nodes: Vec<NodeRef> = parties.iter().map(|(node, _, _)| *node).collect();
        let edges = store::conflicts::edges_among(surreal, &nodes).await?;
        Ok(graph(&parties, &edges))
    }

    #[cfg(test)]
    mod tests {
        use super::graph;
        use store::conflicts::{Edge, NodeRef};
        use uuid::Uuid;

        #[test]
        fn edges_index_the_parties_and_adversity_is_marked() {
            let client = NodeRef::entity(Uuid::now_v7());
            let other = NodeRef::person(Uuid::now_v7());
            let stranger = NodeRef::person(Uuid::now_v7());
            let view = graph(
                &[
                    (client, "Synthetic Client LLC".into(), "Client".into()),
                    (other, "Synthetic Opponent".into(), "Adverse party".into()),
                    (client, "Synthetic Client LLC".into(), "Duplicate".into()),
                ],
                &[
                    Edge {
                        from: client,
                        to: other,
                        kind: "adverse_to".into(),
                        confidence_pct: 100,
                    },
                    Edge {
                        from: other,
                        to: stranger,
                        kind: "related_party".into(),
                        confidence_pct: 90,
                    },
                ],
            );
            assert_eq!(view.nodes.len(), 2);
            assert_eq!(view.nodes[0].kind, "Entity");
            assert_eq!(view.edges.len(), 1, "an edge off the list is dropped");
            assert_eq!(view.edges[0].kind, "Adverse to");
            assert!(view.edges[0].adverse);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GraphEdge, GraphNode, GraphView};
    use crate::sections::tests::render;
    use crate::sections::SectionView;

    fn node(label: &str, kind: &str, role: &str) -> GraphNode {
        GraphNode {
            label: label.into(),
            kind: kind.into(),
            role: role.into(),
        }
    }

    #[test]
    fn the_graph_is_an_svg_with_a_table_of_every_edge_and_no_script() {
        let html = render(SectionView::RelationshipGraph(GraphView {
            nodes: vec![
                node("Synthetic Client LLC", "Entity", "Client"),
                node("Synthetic Counsel", "Person", "Lawyer"),
                node("Synthetic Opponent", "Person", "Adverse party"),
                node("Synthetic Bystander", "Person", "Witness"),
            ],
            edges: vec![
                GraphEdge {
                    from: 0,
                    to: 2,
                    kind: "Adverse to".into(),
                    adverse: true,
                    confidence_pct: 100,
                },
                GraphEdge {
                    from: 1,
                    to: 0,
                    kind: "Manages".into(),
                    adverse: false,
                    confidence_pct: 100,
                },
            ],
        }));
        assert!(html.contains("<svg"), "{html}");
        assert!(html.contains("role=\"img\""), "{html}");
        assert!(
            html.contains("<title id=\"relationship-graph-title\""),
            "{html}"
        );
        assert!(html.contains("matter-graph__edge--adverse"), "{html}");
        assert!(html.contains("<table"), "{html}");
        assert!(html.contains("data-adverse=\"true\""), "{html}");
        assert!(html.contains("Adverse to"), "{html}");
        assert!(html.contains("Manages"), "{html}");
        assert!(
            html.contains("No recorded ties: Synthetic Bystander (Witness)"),
            "{html}"
        );
        assert!(!html.contains("<script"), "{html}");
    }

    #[test]
    fn a_long_label_is_shortened_in_the_diagram_and_whole_in_the_table() {
        let long = "A Synthetic Party With A Very Long Registered Name";
        let html = render(SectionView::RelationshipGraph(GraphView {
            nodes: vec![
                node(long, "Entity", "Client"),
                node("Short", "Person", "Lawyer"),
            ],
            edges: vec![GraphEdge {
                from: 1,
                to: 0,
                kind: "Manages".into(),
                adverse: false,
                confidence_pct: 100,
            }],
        }));
        assert!(html.contains("A Synthetic Party Wit…"), "{html}");
        assert!(html.contains(long), "{html}");
    }
}
