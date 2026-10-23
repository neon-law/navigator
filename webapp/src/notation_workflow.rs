//! `/notations/{slug}`'s "Workflow" section — a client-side-only graph of
//! the template's declared `workflow:` state machine. Every state becomes a
//! node and every declared event becomes a labelled edge.
//!
//! **This is not live data.** There is no Restate invocation, no
//! `store::notation_events` row, and no real matter behind the graph shown
//! here — the same structural guarantee [`crate::notation_demo`] gives the
//! questionnaire section, for the same reason: this public page cannot
//! depend on `workflows` or `store` (`cli/tests/brand_crate_dependencies.rs`),
//! and a firm's real client activity has no business on a public marketing
//! page regardless. The topology below is computed from the template's own
//! declared graph — never read from anywhere real.
//!
//! [`WorkflowStateView`] is the plain-data mirror of
//! `views::workflow_preview::WorkflowState` that crosses the `neon` →
//! `webapp` boundary, the same pattern [`crate::notation_demo::DemoQuestion`]
//! uses for the questionnaire section.

use std::collections::{HashMap, VecDeque};

use dioxus::prelude::*;

/// One state in the declared workflow, with its own outgoing `(event, to)`
/// transitions — the plain-data mirror of
/// `views::workflow_preview::WorkflowState`.
#[derive(Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct WorkflowStateView {
    pub name: String,
    pub transitions: Vec<(String, String)>,
}

#[derive(Debug)]
struct GraphNode {
    name: String,
    x: f64,
    y: f64,
    terminal: bool,
}

#[derive(Debug)]
struct GraphEdge {
    event: String,
    from_x: f64,
    from_y: f64,
    to_x: f64,
    to_y: f64,
    return_lane: Option<f64>,
    label_offset: u32,
}

#[derive(Debug)]
struct GraphLayout {
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
    width: f64,
    height: f64,
}

fn graph_layout(states: &[WorkflowStateView]) -> Option<GraphLayout> {
    let begin = states.iter().position(|state| state.name == "BEGIN")?;
    let by_name: HashMap<&str, usize> = states
        .iter()
        .enumerate()
        .map(|(index, state)| (state.name.as_str(), index))
        .collect();
    let depths = graph_depths(states, begin, &by_name);
    let levels = graph_levels(&depths);
    let (width, height) = graph_dimensions(&levels)?;
    let positions = graph_positions(&levels, width, states.len())?;
    let nodes = graph_nodes(states, &positions);
    let edges = graph_edges(states, &positions, &by_name, &depths, width)?;
    Some(GraphLayout {
        nodes,
        edges,
        width,
        height,
    })
}

fn graph_depths(
    states: &[WorkflowStateView],
    begin: usize,
    by_name: &HashMap<&str, usize>,
) -> Vec<usize> {
    let mut depths = vec![usize::MAX; states.len()];
    depths[begin] = 0;
    let mut queue = VecDeque::from([begin]);
    while let Some(index) = queue.pop_front() {
        let next_depth = depths[index].saturating_add(1);
        for (_, target) in &states[index].transitions {
            if let Some(&target_index) = by_name.get(target.as_str()) {
                if depths[target_index] > next_depth {
                    depths[target_index] = next_depth;
                    queue.push_back(target_index);
                }
            }
        }
    }
    let fallback = depths
        .iter()
        .copied()
        .filter(|depth| *depth != usize::MAX)
        .max()
        .unwrap_or(0)
        + 1;
    for depth in &mut depths {
        if *depth == usize::MAX {
            *depth = fallback;
        }
    }
    let terminal_depth = depths.iter().copied().max().unwrap_or(0) + 1;
    for (index, state) in states.iter().enumerate() {
        if state.is_terminal() {
            depths[index] = terminal_depth;
        }
    }
    depths
}

fn graph_levels(depths: &[usize]) -> Vec<Vec<usize>> {
    let max_depth = depths.iter().copied().max().unwrap_or(0);
    let mut levels = vec![Vec::new(); max_depth + 1];
    for (index, depth) in depths.iter().copied().enumerate() {
        levels[depth].push(index);
    }
    levels
}

fn graph_dimensions(levels: &[Vec<usize>]) -> Option<(f64, f64)> {
    let max_across = levels.iter().map(Vec::len).max().unwrap_or(1);
    let width = (f64::from(u32::try_from(max_across).ok()?) * 330.0 + 320.0).max(490.0);
    let height = (f64::from(u32::try_from(levels.len()).ok()?) * 160.0).max(100.0);
    Some((width, height))
}

fn graph_positions(
    levels: &[Vec<usize>],
    width: f64,
    state_count: usize,
) -> Option<Vec<(f64, f64)>> {
    let mut positions = vec![(0.0, 0.0); state_count];
    for (depth, level) in levels.iter().enumerate() {
        let level_width = f64::from(u32::try_from(level.len()).ok()?) * 330.0;
        let offset = (width - level_width) / 2.0;
        for (column, index) in level.iter().copied().enumerate() {
            positions[index] = (
                offset + f64::from(u32::try_from(column).ok()?) * 330.0 + 165.0,
                f64::from(u32::try_from(depth).ok()?) * 160.0 + 55.0,
            );
        }
    }
    Some(positions)
}

fn graph_nodes(states: &[WorkflowStateView], positions: &[(f64, f64)]) -> Vec<GraphNode> {
    states
        .iter()
        .enumerate()
        .map(|(index, state)| GraphNode {
            name: state.name.clone(),
            x: positions[index].0,
            y: positions[index].1,
            terminal: state.is_terminal(),
        })
        .collect()
}

fn graph_edges(
    states: &[WorkflowStateView],
    positions: &[(f64, f64)],
    by_name: &HashMap<&str, usize>,
    depths: &[usize],
    width: f64,
) -> Option<Vec<GraphEdge>> {
    let mut edges = Vec::new();
    for (index, state) in states.iter().enumerate() {
        for (event_index, (event, target)) in state.transitions.iter().enumerate() {
            let Some(&target_index) = by_name.get(target.as_str()) else {
                continue;
            };
            let label_offset = u32::try_from(event_index).ok()?;
            let event_lane = f64::from(label_offset);
            let source_lane = f64::from(u32::try_from(index).ok()?);
            let has_parallel_target = state
                .transitions
                .iter()
                .filter(|(_, to)| to == target)
                .count()
                > 1;
            edges.push(GraphEdge {
                event: event.clone(),
                from_x: positions[index].0,
                from_y: positions[index].1 + 30.0,
                to_x: positions[target_index].0,
                to_y: positions[target_index].1 - 30.0,
                return_lane: (depths[target_index] != depths[index] + 1 || has_parallel_target)
                    .then_some(width - 20.0 - source_lane * 12.0 - event_lane * 24.0),
                label_offset,
            });
        }
    }
    Some(edges)
}

impl WorkflowStateView {
    fn is_terminal(&self) -> bool {
        self.transitions.is_empty()
    }
}

/// The client-side-only workflow definition graph. Renders nothing for a
/// notation with no declared `workflow:` block.
#[component]
pub fn WorkflowDiagram(states: Vec<WorkflowStateView>) -> Element {
    let Some(graph) = graph_layout(&states) else {
        return rsx! {};
    };
    let view_box = format!("0 0 {} {}", graph.width, graph.height);
    rsx! {
        section { class: "notation-workflow", "aria-label": "Workflow graph",
            p { class: "nav-muted",
                "Definition graph — generated directly from this notation's workflow."
            }
            div { class: "notation-workflow__viewport",
            svg {
                class: "notation-workflow__graph",
                style: "min-width: {graph.width}px",
                view_box,
                role: "img",
                "aria-label": "Workflow states and transitions",
                defs {
                    marker {
                        id: "notation-workflow-arrow",
                        view_box: "0 0 10 10",
                        ref_x: "9",
                        ref_y: "5",
                        marker_width: "6",
                        marker_height: "6",
                        orient: "auto-start-reverse",
                        path { d: "M 0 0 L 10 5 L 0 10 z", class: "notation-workflow__arrow" }
                    }
                }
                for edge in graph.edges.iter() {
                    g { class: "notation-workflow__edge",
                        if let Some(lane) = edge.return_lane {
                            path {
                                d: "M {edge.from_x} {edge.from_y} H {lane} V {edge.to_y} H {edge.to_x}",
                                marker_end: "url(#notation-workflow-arrow)",
                            }
                            text { x: lane - 6.0, y: edge.from_y + 18.0 + f64::from(edge.label_offset) * 22.0, text_anchor: "end", "{edge.event}" }
                        } else {
                            path {
                                d: "M {edge.from_x} {edge.from_y} V {edge.from_y.midpoint(edge.to_y)} H {edge.to_x} V {edge.to_y}",
                                marker_end: "url(#notation-workflow-arrow)",
                            }
                            text {
                                x: edge.from_x.midpoint(edge.to_x) + 6.0,
                                y: edge.from_y.midpoint(edge.to_y) - 6.0,
                                "{edge.event}"
                            }
                        }
                    }
                }
                for node in graph.nodes.iter() {
                    g {
                        class: if node.name == "BEGIN" { "notation-workflow__node notation-workflow__node--start" } else if node.terminal { "notation-workflow__node notation-workflow__node--end" } else { "notation-workflow__node" },
                        transform: "translate({node.x}, {node.y})",
                        rect { x: -150, y: -30, width: 300, height: 60, rx: 12 }
                        text { x: 0, y: 5, text_anchor: "middle", "{node.name}" }
                        if node.name == "BEGIN" {
                            text { class: "notation-workflow__node-role", x: 0, y: -40, text_anchor: "middle", "Start" }
                        } else if node.terminal {
                            text { class: "notation-workflow__node-role", x: 0, y: -40, text_anchor: "middle", "End" }
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

    fn state(name: &str, transitions: &[(&str, &str)]) -> WorkflowStateView {
        WorkflowStateView {
            name: name.to_string(),
            transitions: transitions
                .iter()
                .map(|(e, t)| (e.to_string(), t.to_string()))
                .collect(),
        }
    }

    /// The naturalization workflow, in the shape
    /// `views::workflow_preview::parse` would hand back.
    fn naturalization() -> Vec<WorkflowStateView> {
        vec![
            state(
                "BEGIN",
                &[("intake_submitted", "intake_persisted__applicant")],
            ),
            state(
                "intake_persisted__applicant",
                &[("application_rendered", "lawyer_review")],
            ),
            state(
                "lawyer_review",
                &[
                    ("approved", "generate_pdf__n400_summary"),
                    ("rejected", "END"),
                ],
            ),
            state(
                "generate_pdf__n400_summary",
                &[("pdf_persisted", "sent_for_signature__pending")],
            ),
            state(
                "sent_for_signature__pending",
                &[
                    ("signature_received", "e_filing__uscis"),
                    ("signature_declined", "END"),
                ],
            ),
            state("e_filing__uscis", &[("filed", "END")]),
            state("END", &[]),
        ]
    }

    fn render(states: Vec<WorkflowStateView>) -> String {
        let mut dom = VirtualDom::new_with_props(WorkflowDiagram, WorkflowDiagramProps { states });
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    #[test]
    fn a_notation_with_no_workflow_renders_nothing() {
        assert!(graph_layout(&[]).is_none());
        assert!(!render(Vec::new()).contains("notation-workflow"));
    }

    #[test]
    fn terminal_nodes_follow_the_work_and_parallel_events_have_separate_lanes() {
        let graph = graph_layout(&[
            state("BEGIN", &[("submit", "review")]),
            state("review", &[("approved", "sign"), ("rejected", "END")]),
            state("sign", &[("received", "END"), ("declined", "END")]),
            state("END", &[]),
        ])
        .expect("graph");
        let end = graph
            .nodes
            .iter()
            .find(|node| node.name == "END")
            .expect("END");
        assert!(graph
            .nodes
            .iter()
            .filter(|node| !node.terminal)
            .all(|node| node.y < end.y));
        let received = graph
            .edges
            .iter()
            .find(|edge| edge.event == "received")
            .expect("received");
        let declined = graph
            .edges
            .iter()
            .find(|edge| edge.event == "declined")
            .expect("declined");
        assert_ne!(received.return_lane, declined.return_lane);
        assert_ne!(received.label_offset, declined.label_offset);
    }

    #[test]
    fn graph_contains_every_definition_node_and_transition() {
        let workflow = naturalization();
        let graph = graph_layout(&workflow).expect("BEGIN makes a graph");
        assert_eq!(graph.nodes.len(), workflow.len());
        assert_eq!(
            graph.edges.len(),
            workflow
                .iter()
                .map(|state| state.transitions.len())
                .sum::<usize>()
        );
        let out = render(workflow);
        assert!(out.contains("notation-workflow__graph"), "{out}");
        assert!(out.contains("notation-workflow__node--start"), "{out}");
        assert!(out.contains("notation-workflow__node--end"), "{out}");
        assert!(out.contains("lawyer_review"), "{out}");
        assert!(out.contains("signature_declined"), "{out}");
        assert!(out.contains("generated directly"), "{out}");
    }
}
