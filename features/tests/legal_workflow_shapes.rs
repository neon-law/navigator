//! Cucumber runner for `features/legal_workflow_shapes.feature`.
//!
//! Loads each remaining bundled template from disk and pins down its
//! questionnaire shape. Complements `workflows/tests/workflow_integrity.rs`,
//! which enforces generic invariants; these scenarios are template-specific
//! so a recomposition shows up as a named failing scenario in the BDD suite.

#![allow(clippy::unused_async)]

use cucumber::{gherkin::Step, given, then, World};
use features::template_shapes::{strip_workflow_end, templates_root, walk_chain};
use workflows::{
    questionnaire_spec_from_template, workflow_spec_from_template, StateName, WorkflowSpec,
    WorkflowSpecError,
};

#[derive(Default, World)]
#[world(init = Self::default)]
struct ShapeWorld {
    markdown: Option<String>,
    workflow_before_omission: Option<WorkflowSpec>,
}

impl std::fmt::Debug for ShapeWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShapeWorld")
            .field("has_markdown", &self.markdown.is_some())
            .field("workflow_before_omission", &self.workflow_before_omission)
            .finish()
    }
}

#[given(regex = r#"^the bundled template "([^"]+)"$"#)]
async fn load_template(world: &mut ShapeWorld, relpath: String) {
    let path = templates_root().join(&relpath);
    world.markdown = Some(
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display())),
    );
    world.workflow_before_omission = None;
}

#[given(regex = r#"^the bundled template "([^"]+)" with the workflow END declaration removed$"#)]
async fn load_template_without_end(world: &mut ShapeWorld, relpath: String) {
    let path = templates_root().join(&relpath);
    let original =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    world.markdown = Some(strip_workflow_end(&original));
    world.workflow_before_omission =
        Some(workflow_spec_from_template(&original).expect("original workflow parses"));
}

#[then("the questionnaire transitions, in BEGIN-first order, are:")]
async fn assert_questionnaire_chain(world: &mut ShapeWorld, step: &Step) {
    let md = world.markdown.as_ref().expect("template loaded");
    let q = questionnaire_spec_from_template(md).expect("questionnaire frontmatter parses");
    assert_chain_matches(q.inner(), step);
}

#[then("the workflow transitions, in BEGIN-first order, are:")]
async fn assert_workflow_chain(world: &mut ShapeWorld, step: &Step) {
    let md = world.markdown.as_ref().expect("template loaded");
    let w = workflow_spec_from_template(md).expect("workflow frontmatter parses");
    assert_chain_matches(&w, step);
}

#[then("the authored workflow preserves every transition and supplies a terminal END")]
async fn assert_implicit_end(world: &mut ShapeWorld) {
    let md = world.markdown.as_ref().expect("template loaded");
    let expected = world
        .workflow_before_omission
        .as_ref()
        .expect("original workflow recorded before omitting END");
    let spec = workflow_spec_from_template(md).expect("authored workflow supplies implicit END");
    assert_eq!(
        &spec, expected,
        "omitting END must preserve the entire workflow"
    );
    assert!(spec.is_terminal(&StateName::end()));
}

#[then("the typed workflow still rejects a missing END declaration")]
async fn assert_typed_missing_end(world: &mut ShapeWorld) {
    let md = world.markdown.as_ref().expect("template loaded");
    let mut spec = workflow_spec_from_template(md).expect("authored workflow parses");
    assert!(spec.states.remove(&StateName::end()).is_some());
    assert!(matches!(
        spec.validate(),
        Err(WorkflowSpecError::MissingEnd)
    ));
}

fn assert_chain_matches(spec: &WorkflowSpec, step: &Step) {
    let table = step.table.as_ref().expect("scenario has a data table");
    let expected: Vec<(&str, &str)> = table
        .rows
        .iter()
        .skip(1)
        .map(|row| {
            (
                row.first().expect("from cell").as_str(),
                row.get(1).expect("to cell").as_str(),
            )
        })
        .collect();
    let chain = walk_chain(spec);
    let actual: Vec<(&str, &str)> = chain
        .iter()
        .map(|(f, t)| (f.as_str(), t.as_str()))
        .collect();
    assert_eq!(actual, expected, "transition chain mismatch");
}

#[tokio::main]
async fn main() {
    ShapeWorld::cucumber()
        .run_and_exit("tests/features/legal_workflow_shapes.feature")
        .await;
}
