//! `/notations/{slug}`'s "Try answering this" section (ENG-452) — a
//! client-side-only walk through the template's real declared questionnaire
//! order, rendered with Navigator's real [`Field`] controls.
//!
//! Preview answers and the current question are saved in the browser under a
//! versioned, per-path `/notations` key. No real records or workflow instances
//! are created. Record questions collect display values for the template.

use std::collections::BTreeMap;

use dioxus::prelude::*;

use crate::components::Progress;
use crate::notation_preview_storage::{PreviewSnapshot, PreviewStorage};

/// A clearly-synthetic sample matter, named in the demo's chrome so the walk
/// never reads as scoped to nothing — every real Notation has a Project.
pub const SAMPLE_PROJECT_LABEL: &str = "Sample matter: Acme LLC";

/// One step of the demo — the plain-data mirror of
/// `views::questionnaire_preview::PreviewQuestion` that crosses the `neon` →
/// `webapp` boundary, the same pattern
/// [`crate::notation_preview::PreviewDoc`] uses for the rest of this page's
/// content.
#[derive(Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct DemoQuestion {
    pub code: String,
    pub answer_type: String,
    pub prompt: String,
    /// `(value, label)` options — populated only when [`Self::interactive`]
    /// is `true` and the answer type is `custom_single_choice`.
    pub choices: Vec<(String, String)>,
    /// Whether this step has a real control to demo, or is a record/reference
    /// state rendered as a short explanation instead.
    pub interactive: bool,
    /// Title-case role from the state name.
    #[serde(default)]
    pub role_label: String,
    /// Questionnaire group heading, when the author declared one.
    #[serde(default)]
    pub group: String,
    /// Extra guidance under the prompt.
    #[serde(default)]
    pub help: String,
    /// Typed cells when `answer_type` is `schedule`.
    #[serde(default)]
    pub columns: Vec<DemoColumn>,
}

/// One schedule cell the public demo can collect.
#[derive(Clone, PartialEq, Eq, Default, Debug, serde::Serialize, serde::Deserialize)]
pub struct DemoColumn {
    pub name: String,
    pub label: String,
    pub value_type: String,
}

/// The client-side-only questionnaire stepper. Renders nothing for a
/// notation with no declared questionnaire (a template whose frontmatter
/// carries none, or one `views::questionnaire_preview::parse` couldn't read).
#[component]
pub fn QuestionnaireDemo(
    questions: Vec<DemoQuestion>,
    #[props(default)] template_html: String,
    /// The public preview puts the live document below the questionnaire. A draft
    /// page already renders that document in its own section.
    #[props(default = true)]
    embed_document: bool,
) -> Element {
    let Some(total) = std::num::NonZeroUsize::new(questions.len()) else {
        return rsx! {};
    };
    let total = total.get();
    let mut step = use_signal(|| 0_usize);
    let mut answers = use_signal(BTreeMap::<String, String>::new);
    let mut storage = use_signal(PreviewStorage::default);
    let mut restored = use_signal(|| false);
    let mut saved = use_signal(|| false);
    let restore_template = template_html.clone();
    let restore_questions = questions.clone();
    use_effect(move || {
        if *restored.peek() {
            return;
        }
        let adapter = PreviewStorage::for_current_page();
        if let Some(mut snapshot) = adapter.load(&restore_template) {
            snapshot.answers.retain(|code, _| {
                restore_questions
                    .iter()
                    .any(|question| question.code == *code)
            });
            step.set(snapshot.step.min(total - 1));
            answers.set(snapshot.answers);
        }
        storage.set(adapter);
        restored.set(true);
    });
    let save_template = template_html.clone();
    use_effect(move || {
        if !*restored.read() {
            return;
        }
        let snapshot = PreviewSnapshot {
            template: save_template.clone(),
            step: *step.read(),
            answers: answers.read().clone(),
        };
        saved.set(storage.peek().save(&snapshot));
    });
    let index = *step.read();
    let question = &questions[index];
    let position = index + 1;
    let show_document = embed_document && !template_html.is_empty();

    rsx! {
        section { class: "notation-demo", "aria-label": "Questionnaire",
            h2 { "Questionnaire" }
            p { "Try answering this" }
            p { class: "nav-muted",
                "{SAMPLE_PROJECT_LABEL} — preview only. "
                if *saved.read() {
                    "Answers and your place are saved in this browser for this notation."
                } else {
                    "Answers remain available on this page; browser storage is unavailable."
                }
            }
            div {
                class: "notation-demo__workspace notation-demo__workspace--stacked",
                div { class: "notation-demo__question",
                    p { class: "notation-demo__count", "Question {position} of {total}" }
                    Progress {
                        label: "Demo question progress".to_string(),
                        value: Some(position),
                        max: total,
                    }
                    if !question.group.is_empty() {
                        p { class: "notation-demo__group", "{question.group}" }
                    }
                    if !question.role_label.is_empty() {
                        p { class: "notation-demo__role", "{question.role_label}" }
                    }
                    div {
                        class: "notation-demo__step",
                        key: "{question.code}",
                        {demo_field(question, answers)}
                    }
                    {question_outline(&questions, index)}
                    {demo_actions(index, position, total, step)}
                    button {
                        class: "nav-btn nav-btn--secondary",
                        r#type: "button",
                        onclick: move |_| { answers.set(BTreeMap::new()); step.set(0); },
                        "Reset answers"
                    }
                }
            }
        }
        if show_document {
            section { class: "notation-template-body", "aria-label": "Template body",
                h2 { "Body" }
                div { class: "notation-live-template",
                    dangerous_inner_html: "{render_live_template(&template_html, &questions, &answers.read())}"
                }
            }
        }
    }
}

fn question_outline(questions: &[DemoQuestion], index: usize) -> Element {
    rsx! {
        details { class: "notation-demo__outline",
            summary { "All questions" }
            ol {
                for (number, question) in questions.iter().enumerate() {
                    li {
                        aria_current: if number == index { "step" } else { "" },
                        "{question.prompt}"
                    }
                }
            }
        }
    }
}

fn demo_actions(index: usize, position: usize, total: usize, mut step: Signal<usize>) -> Element {
    rsx! {
        div { class: "notation-demo__actions",
            if index > 0 {
                button {
                    class: "nav-btn nav-btn--secondary",
                    r#type: "button",
                    onclick: move |_| step.set(index - 1),
                    "Back"
                }
            }
            if position < total {
                button {
                    class: "nav-btn nav-btn--primary",
                    r#type: "button",
                    onclick: move |_| step.set(index + 1),
                    "Continue"
                }
            } else {
                span { class: "nav-muted", "That is every question this notation asks." }
            }
        }
    }
}

fn answer_label(question: &DemoQuestion, value: &str) -> String {
    if question.answer_type == "custom_yes_no" {
        return match value {
            "true" => "Yes".to_string(),
            "false" => "No".to_string(),
            _ => value.to_string(),
        };
    }
    question
        .choices
        .iter()
        .find(|(candidate, _)| candidate == value)
        .map_or_else(|| value.to_string(), |(_, label)| label.clone())
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn replace_unanswered(mut html: String, questions: &[DemoQuestion]) -> String {
    for question in questions {
        let token = format!("{{{{{}}}}}", question.code);
        let blank = format!(
            "<span class=\"notation-live-template__blank\" data-placeholder=\"{}\">{}</span>",
            escape_html(&question.code),
            escape_html(&question.prompt)
        );
        html = html.replace(&token, &blank);
    }
    while let Some(start) = html.find("{{") {
        let Some(end) = html[start + 2..].find("}}") else {
            break;
        };
        html.replace_range(
            start..start + end + 4,
            "<span class=\"notation-live-template__blank\"></span>",
        );
    }
    html
}

/// Render the same template on every answer change. Answer text is escaped
/// before it reaches the template's already-rendered HTML.
pub(crate) fn render_live_template(
    template_html: &str,
    questions: &[DemoQuestion],
    answers: &BTreeMap<String, String>,
) -> String {
    let context = preview_context(questions, answers);
    let mut display: BTreeMap<String, String> = questions
        .iter()
        .filter_map(|question| {
            answers.get(&question.code).map(|answer| {
                (
                    question.code.clone(),
                    escape_html(&answer_label(question, answer)),
                )
            })
        })
        .collect();
    for (key, value) in &context {
        display
            .entry(key.clone())
            .or_insert_with(|| escape_html(value));
    }
    replace_unanswered(
        forms::notation::fill_with_display(template_html, &context, &display),
        questions,
    )
}

/// Render one interactive demo step, or the explanation for a record state.
fn preview_context(
    questions: &[DemoQuestion],
    answers: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut context: BTreeMap<String, String> = answers
        .iter()
        .map(|(code, answer)| {
            let value = questions
                .iter()
                .find(|question| question.code == *code && question.answer_type == "schedule")
                .and_then(|_| escaped_schedule_json(answer))
                .unwrap_or_else(|| answer.clone());
            (code.clone(), value)
        })
        .collect();
    for question in questions {
        if matches!(
            question.answer_type.as_str(),
            "person" | "entity" | "project"
        ) {
            if let Some(value) = answers.get(&question.code) {
                context.insert(format!("{}.name", question.code), value.clone());
            }
        }
    }
    context
}

/// Schedule rows are inserted into the live document by the loop renderer,
/// which copies string cells as written. Escape them here so the public demo
/// cannot treat a cell as markup.
fn escaped_schedule_json(answer: &str) -> Option<String> {
    let mut rows: Vec<BTreeMap<String, serde_json::Value>> = serde_json::from_str(answer).ok()?;
    for row in &mut rows {
        for value in row.values_mut() {
            if let serde_json::Value::String(text) = value {
                *text = escape_html(text);
            }
        }
    }
    serde_json::to_string(&rows).ok()
}

fn prompt_copy(question: &DemoQuestion) -> Element {
    rsx! {
        span { class: "notation-demo__prompt", "{question.prompt}" }
        if !question.help.is_empty() {
            span { class: "notation-demo__help", "{question.help}" }
        }
    }
}

/// Render one interactive demo step, or the explanation for a record state.
fn demo_field(question: &DemoQuestion, mut answers: Signal<BTreeMap<String, String>>) -> Element {
    if !question.interactive
        && !matches!(
            question.answer_type.as_str(),
            "person" | "entity" | "project" | "address"
        )
    {
        return rsx! {
            p { class: "notation-demo__explanation", "{question.prompt}" }
        };
    }
    if question.answer_type == "schedule" {
        return schedule_field(question, answers);
    }
    let code = question.code.clone();
    let current = answers.read().get(&code).cloned().unwrap_or_default();
    let input_type = match question.answer_type.as_str() {
        "custom_datetime" => "date",
        "custom_usd" => "number",
        "custom_phone" => "tel",
        _ => "text",
    };
    if matches!(
        question.answer_type.as_str(),
        "custom_yes_no" | "custom_single_choice"
    ) {
        let choices = if question.answer_type == "custom_yes_no" {
            vec![
                ("true".to_string(), "Yes".to_string()),
                ("false".to_string(), "No".to_string()),
            ]
        } else {
            question.choices.clone()
        };
        return rsx! {
            fieldset { class: "nav-field nav-choice-group",
                legend { {prompt_copy(question)} }
                for (value, label) in choices {
                    label { class: "nav-choice-card",
                        input {
                            class: "nav-control",
                            r#type: "radio",
                            name: "{question.code}",
                            value: "{value}",
                            checked: current == value,
                            onchange: {
                                let code = code.clone();
                                let value = value.clone();
                                move |_| { answers.write().insert(code.clone(), value.clone()); }
                            },
                        }
                        span { "{label}" }
                    }
                }
            }
        };
    }
    if question.answer_type == "custom_text" {
        return rsx! {
            label { class: "nav-field",
                {prompt_copy(question)}
                textarea {
                    class: "nav-control",
                    rows: 4,
                    value: "{current}",
                    oninput: move |event| { answers.write().insert(code.clone(), event.value()); },
                }
            }
        };
    }
    rsx! {
        label { class: "nav-field",
            {prompt_copy(question)}
            input {
                class: "nav-control",
                r#type: input_type,
                value: "{current}",
                step: if question.answer_type == "custom_usd" { "0.01" } else { "" },
                oninput: move |event| { answers.write().insert(code.clone(), event.value()); },
            }
        }
    }
}

fn schedule_field(
    question: &DemoQuestion,
    mut answers: Signal<BTreeMap<String, String>>,
) -> Element {
    let code = question.code.clone();
    let current = answers.read().get(&code).cloned().unwrap_or_default();
    let rows = schedule_rows(&current, &question.columns);
    rsx! {
        div { class: "notation-demo__schedule",
            {prompt_copy(question)}
            for (row_index, row) in rows.iter().enumerate() {
                div { class: "notation-demo__row", key: "{question.code}-{row_index}",
                    for column in question.columns.iter() {
                        label { class: "nav-field",
                            span { "{column.label}" }
                            input {
                                class: "nav-control",
                                r#type: schedule_input_type(&column.value_type),
                                value: "{row.get(&column.name).cloned().unwrap_or_default()}",
                                oninput: {
                                    let code = code.clone();
                                    let name = column.name.clone();
                                    let columns = question.columns.clone();
                                    move |event| {
                                        let mut next = schedule_rows(
                                            answers.read().get(&code).map_or("", String::as_str),
                                            &columns,
                                        );
                                        if let Some(row) = next.get_mut(row_index) {
                                            row.insert(name.clone(), event.value());
                                        }
                                        if let Ok(json) = serde_json::to_string(&next) {
                                            answers.write().insert(code.clone(), json);
                                        }
                                    }
                                },
                            }
                        }
                    }
                    if rows.len() > 1 {
                        button {
                            class: "nav-btn nav-btn--secondary",
                            r#type: "button",
                            onclick: {
                                let code = code.clone();
                                let columns = question.columns.clone();
                                move |_| {
                                    let mut next = schedule_rows(
                                        answers.read().get(&code).map_or("", String::as_str),
                                        &columns,
                                    );
                                    if row_index < next.len() {
                                        next.remove(row_index);
                                    }
                                    if let Ok(json) = serde_json::to_string(&next) {
                                        answers.write().insert(code.clone(), json);
                                    }
                                }
                            },
                            "Remove row"
                        }
                    }
                }
            }
            button {
                class: "nav-btn nav-btn--secondary",
                r#type: "button",
                onclick: {
                    let code = code.clone();
                    let columns = question.columns.clone();
                    move |_| {
                        let mut next = schedule_rows(
                            answers.read().get(&code).map_or("", String::as_str),
                            &columns,
                        );
                        next.push(blank_row(&columns));
                        if let Ok(json) = serde_json::to_string(&next) {
                            answers.write().insert(code.clone(), json);
                        }
                    }
                },
                "Add row"
            }
        }
    }
}

fn schedule_input_type(value_type: &str) -> &'static str {
    match value_type {
        "number" => "number",
        "date" => "date",
        _ => "text",
    }
}

fn blank_row(columns: &[DemoColumn]) -> BTreeMap<String, String> {
    columns
        .iter()
        .map(|column| (column.name.clone(), String::new()))
        .collect()
}

fn schedule_rows(answer: &str, columns: &[DemoColumn]) -> Vec<BTreeMap<String, String>> {
    let parsed: Vec<BTreeMap<String, String>> = serde_json::from_str(answer).unwrap_or_default();
    if parsed.is_empty() {
        vec![blank_row(columns)]
    } else {
        parsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_preview_names_fill_dotted_placeholders_and_escape_markup() {
        let questions = vec![DemoQuestion {
            code: "person__client".into(),
            answer_type: "person".into(),
            interactive: true,
            ..DemoQuestion::default()
        }];
        let answers = BTreeMap::from([("person__client".into(), "<Acme & Co>".into())]);
        let rendered = render_live_template("Dear {{person__client.name}}", &questions, &answers);
        assert_eq!(rendered, "Dear &lt;Acme &amp; Co&gt;");
    }

    fn interactive(answer_type: &str, prompt: &str, choices: Vec<(&str, &str)>) -> DemoQuestion {
        DemoQuestion {
            code: format!("{answer_type}__demo"),
            answer_type: answer_type.to_string(),
            prompt: prompt.to_string(),
            choices: choices
                .into_iter()
                .map(|(v, l)| (v.to_string(), l.to_string()))
                .collect(),
            interactive: true,
            ..DemoQuestion::default()
        }
    }

    fn explanation(prompt: &str) -> DemoQuestion {
        DemoQuestion {
            code: "person__client".to_string(),
            answer_type: "person".to_string(),
            prompt: prompt.to_string(),
            choices: Vec::new(),
            interactive: false,
            ..DemoQuestion::default()
        }
    }

    fn render(questions: Vec<DemoQuestion>) -> String {
        let mut dom = VirtualDom::new_with_props(
            QuestionnaireDemo,
            QuestionnaireDemoProps {
                questions,
                template_html: String::new(),
                embed_document: true,
            },
        );
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    #[test]
    fn a_notation_with_no_questionnaire_renders_nothing() {
        let out = render(Vec::new());
        assert!(!out.contains("Try answering this"), "{out}");
    }

    #[test]
    fn the_filled_template_is_not_titled_live_document() {
        let questions = vec![interactive("custom_text", "Client name", vec![])];
        let mut dom = VirtualDom::new_with_props(
            QuestionnaireDemo,
            QuestionnaireDemoProps {
                questions,
                template_html: "<p>Hello</p>".to_string(),
                embed_document: true,
            },
        );
        dom.rebuild_in_place();
        let out = dioxus_ssr::render(&dom);
        assert!(out.contains("Hello"), "{out}");
        assert!(!out.contains("Live document"), "{out}");
    }

    #[test]
    fn names_the_sample_matter_and_reports_storage_status() {
        let out = render(vec![interactive(
            "custom_text",
            "What's the scope?",
            vec![],
        )]);
        assert!(out.contains(SAMPLE_PROJECT_LABEL), "{out}");
        assert!(out.contains("browser storage is unavailable"), "{out}");
    }

    #[test]
    fn custom_text_renders_a_textarea() {
        let out = render(vec![interactive(
            "custom_text",
            "Describe the scope.",
            vec![],
        )]);
        assert!(out.contains("<textarea"), "{out}");
        assert!(out.contains("Describe the scope."), "{out}");
    }

    #[test]
    fn custom_single_choice_renders_the_templates_own_radio_options() {
        let out = render(vec![interactive(
            "custom_single_choice",
            "Which state's law governs?",
            vec![("nevada", "Nevada"), ("california", "California")],
        )]);
        assert!(out.contains(r#"type="radio""#), "{out}");
        assert!(
            out.contains("Nevada") && out.contains("California"),
            "{out}"
        );
        // ENG-506: cards, not a compact radio list — matching the real
        // walkers' own ENG-504 treatment.
        assert!(out.contains("nav-choice-group"), "{out}");
    }

    #[test]
    fn custom_yes_no_renders_a_two_card_yes_no_choice() {
        let out = render(vec![interactive(
            "custom_yes_no",
            "Do you have counsel?",
            vec![],
        )]);
        assert!(out.contains("nav-choice-group"), "{out}");
        assert!(out.contains(">Yes<") && out.contains(">No<"), "{out}");
    }

    #[test]
    fn the_outline_lists_every_question_behind_a_closed_disclosure() {
        let out = render(vec![
            interactive("custom_text", "First?", vec![]),
            interactive("custom_text", "Second?", vec![]),
        ]);
        assert!(out.contains("Question 1 of 2"), "{out}");
        assert!(out.contains("notation-demo__outline"), "{out}");
        assert!(!out.contains("nav-steps__item"), "{out}");
        assert!(out.contains(r#"aria-current="step""#), "{out}");
        assert!(out.contains("All questions"), "{out}");
        assert!(out.contains("First?") && out.contains("Second?"), "{out}");
        assert!(
            !out.contains("notation-demo__outline\" open"),
            "the outline starts closed: {out}"
        );
    }

    #[test]
    fn a_record_state_collects_a_local_display_value() {
        let out = render(vec![explanation("This step records the client.")]);
        assert!(out.contains("This step records the client."), "{out}");
        assert!(
            out.contains("<input") && out.contains("type=\"text\""),
            "a preview name control: {out}"
        );
    }

    #[test]
    fn the_progress_indicator_names_the_first_of_several_steps() {
        let out = render(vec![
            interactive("custom_text", "First?", vec![]),
            interactive("custom_text", "Second?", vec![]),
        ]);
        assert!(out.contains(r#"aria-valuenow="1""#), "{out}");
        assert!(out.contains(r#"aria-valuemax="2""#), "{out}");
    }

    #[test]
    fn a_single_step_notation_has_no_back_button_and_names_the_end() {
        let out = render(vec![interactive("custom_text", "Only question?", vec![])]);
        assert!(!out.contains(">Back<"), "{out}");
        assert!(
            out.contains("That is every question this notation asks."),
            "{out}"
        );
    }

    #[test]
    fn no_form_element_or_action_attribute_exists_anywhere_in_the_demo() {
        let out = render(vec![
            interactive("custom_text", "Free text?", vec![]),
            explanation("This step records the client."),
        ]);
        assert!(!out.contains("<form"), "no <form> in the demo: {out}");
        assert!(!out.contains("action="), "no action= in the demo: {out}");
    }

    #[test]
    fn live_template_hides_tokens_escapes_answers_and_evaluates_clauses() {
        let questions = vec![interactive("custom_text", "Client name", vec![])];
        let code = questions[0].code.clone();
        let token = format!("{{{{{code}}}}}");
        let template = ["Hello ", &token, ". {{#if ", &code, "}}Named.{{/if}}"].concat();
        let empty = render_live_template(&template, &questions, &BTreeMap::new());
        assert!(!empty.contains("{{"), "{empty}");
        assert!(!empty.contains("Named."), "{empty}");
        assert!(empty.contains("Client name"), "{empty}");
        let filled = render_live_template(
            &template,
            &questions,
            &BTreeMap::from([(code, "<Acme>".to_string())]),
        );
        assert!(filled.contains("&lt;Acme&gt;"), "{filled}");
        assert!(filled.contains("Named."), "{filled}");
    }

    #[test]
    fn a_schedule_renders_typed_rows_and_escapes_cells_in_the_document() {
        let mut question = interactive("schedule", "List each license.", vec![]);
        question.columns = vec![
            DemoColumn {
                name: "product".to_string(),
                label: "Product".to_string(),
                value_type: "text".to_string(),
            },
            DemoColumn {
                name: "seats".to_string(),
                label: "Seats".to_string(),
                value_type: "number".to_string(),
            },
        ];
        question.help = "Add one row per item. Each cell is text, a number, or a date.".to_string();
        let out = render(vec![question.clone()]);
        assert!(out.contains("Add row"), "{out}");
        assert!(out.contains(r#"type="number""#), "{out}");
        assert!(out.contains("Add one row per item"), "{out}");
        let template = format!(
            "{{{{#for row in {}}}}}{{{{row.product}}}}{{{{/for}}}}",
            question.code
        );
        let filled = render_live_template(
            &template,
            &[question.clone()],
            &BTreeMap::from([(
                question.code,
                r#"[{"product":"<script>","seats":"2"}]"#.to_string(),
            )]),
        );
        assert!(filled.contains("&lt;script&gt;"), "{filled}");
        assert!(!filled.contains("<script>"), "{filled}");
    }
}
