//! Build one `/notations/{slug}` show-page document from a template's own
//! Markdown — the content [`crate::dioxus_app::notation_preview_router`]
//! serves.
//!
//! Two callers assemble the same thing from different sources and must not
//! drift: the firm's public site, which compiles the bundled notations in
//! with `include_str!`, and `navigator notation preview`, which reads one
//! file off disk in whatever repository the author is standing in. A page
//! that renders differently depending on which of those produced it would
//! make the preview useless as a check on the published page, so the
//! projection lives here once and both call it.
//!
//! Everything is read from the template itself. The body becomes the
//! paragraph-stepping stage [`views::harvard_outline`] builds for every
//! notation; the declared `questionnaire:` becomes the "Try answering this"
//! demo's ordered steps; the declared `workflow:` becomes the sample-run
//! diagram's graph. All three readers are pure functions over the file — no
//! store, no runtime, no Notation — which is what lets a template be
//! previewed before it has ever been imported, let alone bound to a matter.

use views::questionnaire_preview::ParseFailure;
use webapp::notation_demo::DemoQuestion;
use webapp::notation_preview::PreviewDoc;
use webapp::notation_workflow::WorkflowStateView;

/// The template's declared questionnaire, in order, ready for the "Try
/// answering this" demo — parsed with no live Notation and no domain-crate
/// dependency (see `views::questionnaire_preview`'s own doc comment for why
/// that reader exists separately from `workflows::notation_session`).
///
/// # Errors
///
/// Returns the parse failure when the template declares a questionnaire
/// that cannot be read — the caller logs it and renders the visible panel,
/// never a silently empty demo.
pub fn demo_questions(frontmatter: &str) -> Result<Vec<DemoQuestion>, ParseFailure> {
    let questions = views::questionnaire_preview::parse(frontmatter)?;
    Ok(questions
        .into_iter()
        .map(|q| {
            let interactive = q.is_interactive();
            DemoQuestion {
                code: q.code,
                answer_type: q.answer_type,
                prompt: q.prompt,
                choices: q.choices,
                interactive,
                role_label: q.role_label,
                group: q.group,
                help: q.help,
                columns: q
                    .columns
                    .into_iter()
                    .map(|column| webapp::notation_demo::DemoColumn {
                        name: column.name,
                        label: column.label,
                        value_type: column.value_type,
                    })
                    .collect(),
            }
        })
        .collect())
}

/// The template's declared `workflow:` state machine, ready for the sample
/// "Workflow" runs — parsed with no live Restate invocation and no
/// domain-crate dependency (see `views::workflow_preview`).
#[must_use]
pub fn demo_workflow(frontmatter: &str) -> Vec<WorkflowStateView> {
    views::workflow_preview::parse(frontmatter)
        .into_iter()
        .map(|s| WorkflowStateView {
            name: s.name,
            transitions: s.transitions.into_iter().map(|t| (t.event, t.to)).collect(),
        })
        .collect()
}

/// Project one notation's Markdown onto its show page.
///
/// `slug` is the `{slug}` path segment the router matches, and `source_href`
/// is where "View source" points — a GitHub blob URL for a bundled notation,
/// the local path for a file being previewed. `origin_url` comes from the
/// template's own frontmatter, so a government form links to the government's
/// blank and a letter, which declares none, links nowhere.
#[must_use]
pub fn from_markdown(slug: &str, source_href: &str, src: &str) -> PreviewDoc {
    let (composed, composition_error) = match rules::clauses::compile(src) {
        Ok(composed) => (composed, None),
        Err(error) => {
            // Loud, not silent: the degraded page still renders (the
            // composition error becomes the document), and the structured
            // warning carries the slug so logs name the template.
            tracing::warn!(
                template_slug = %slug,
                error = %error,
                "clause compilation degraded; the preview renders the composition error"
            );
            (
                format!("---\ntitle: Invalid Clause reference\n---\n\n{error}"),
                Some(error),
            )
        }
    };
    let doc = views::harvard_outline::parse(&composed);
    let frontmatter = doc.frontmatter.clone().unwrap_or_default();
    let origin_url = views::harvard_outline::frontmatter_field(&frontmatter, "origin_url");
    let (demo_questions, questionnaire_error) = match demo_questions(&frontmatter) {
        Ok(questions) => (questions, None),
        Err(failure) => {
            tracing::warn!(
                template_slug = %slug,
                error = %failure,
                "questionnaire preview degraded; the page renders the failure panel"
            );
            (Vec::new(), Some(failure.to_string()))
        }
    };
    let questionnaire_error = composition_error
        .map(|error| format!("Clause composition failed: {error}"))
        .or(questionnaire_error);
    PreviewDoc {
        slug: slug.to_string(),
        title: doc.title.clone(),
        source_href: source_href.to_string(),
        demo_questions,
        demo_workflow: demo_workflow(&frontmatter),
        frontmatter,
        stage_html: views::harvard_outline::stage_html(&doc),
        origin_url,
        questionnaire_error,
    }
}

#[cfg(test)]
mod tests {
    use super::from_markdown;

    /// A cloneable in-memory writer that captures the structured warnings
    /// `from_markdown` emits, so a test can assert the trace a
    /// stalled-preview investigation starts from.
    #[derive(Clone, Default)]
    struct LogBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for LogBuf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log buffer lock").extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuf {
        type Writer = LogBuf;
        fn make_writer(&'a self) -> LogBuf {
            self.clone()
        }
    }

    /// Install a JSON subscriber writing into `buf` as this thread's
    /// default. Same pattern as `a2a`'s audit-capture tests.
    fn capture_into(
        buf: &std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    ) -> tracing::subscriber::DefaultGuard {
        crate::test_tracing::ensure_callsite_interest();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::INFO)
            .with_writer(LogBuf(buf.clone()))
            .finish();
        tracing::subscriber::set_default(subscriber)
    }

    fn logged(buf: &std::sync::Arc<std::sync::Mutex<Vec<u8>>>) -> String {
        String::from_utf8(buf.lock().expect("log buffer lock").clone())
            .expect("captured log is UTF-8")
    }

    #[test]
    fn clause_references_reach_the_preview_as_prose_and_questions() {
        let source = "---\nkind: agreement\ntitle: Sample\nquestionnaire: [entity__company]\nclauses:\n  law:\n    source: dispute_resolution@1\n    inputs:\n      law: jurisdiction__law\n      seat: jurisdiction__seat\n---\n## I. Administration\n\n{{> law}}";
        let doc = from_markdown("sample", "", source);
        assert!(doc.stage_html.contains("governs this agreement"));
        assert!(!doc.stage_html.contains("{{&gt;"));
        assert!(doc
            .demo_questions
            .iter()
            .any(|question| question.code == "jurisdiction__law"));
        assert!(doc
            .demo_questions
            .iter()
            .any(|question| question.code == "jurisdiction__seat"));
    }

    #[test]
    fn retired_clause_source_shows_a_composition_error() {
        let source = "---\nkind: agreement\nclauses:\n  law:\n    source: governing_law@1\n    inputs:\n      jurisdiction: jurisdiction__law\n      forum: custom_text__forum\n---\n{{> law}}";
        let doc = from_markdown("sample", "", source);
        assert_eq!(doc.title, "Invalid Clause reference");
        assert!(doc.demo_questions.is_empty());
        assert!(doc.stage_html.contains("retired"));
        assert!(!doc.stage_html.contains("Governing law:"));
        let error = doc
            .questionnaire_error
            .expect("the retired reference degrades loudly, not silently");
        assert!(error.contains("Clause composition failed"), "{error}");
        assert!(error.contains("retired"), "{error}");
    }

    #[test]
    fn invalid_clause_source_shows_a_composition_error() {
        let source = "---\nkind: agreement\nclauses:\n  law:\n    source: absent@1\n---\n{{> law}}";
        let doc = from_markdown("sample", "", source);
        assert_eq!(doc.title, "Invalid Clause reference");
        assert!(doc.demo_questions.is_empty());
        assert!(doc.stage_html.contains("unknown Clause version"));
        assert!(doc.questionnaire_error.is_some());
    }

    /// The brief's malformed-template contract: a template whose declared
    /// questionnaire cannot be read emits the structured warning — carrying
    /// the template slug and the parse error — and the doc surfaces the
    /// panel error, where HEAD's parse used to return a silently empty
    /// demo.
    #[test]
    fn a_malformed_questionnaire_warns_with_the_slug_and_the_error() {
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let _guard = capture_into(&buf);
        let source =
            "---\ntitle: Broken\nquestionnaire:\n  BEGIN:\n    _: END\n  END: {}\n---\n\nBody.\n";
        let doc = from_markdown("sample-will", "", source);

        let log = logged(&buf);
        assert!(
            log.contains("questionnaire preview degraded"),
            "warning event: {log}"
        );
        assert!(
            log.contains("sample-will"),
            "warning carries the slug: {log}"
        );
        assert!(
            log.contains("no steps resolve"),
            "warning carries the parse error: {log}"
        );

        let error = doc
            .questionnaire_error
            .expect("a declared-but-unreadable questionnaire fails loud");
        assert!(error.contains("no steps resolve"), "{error}");
        assert!(doc.demo_questions.is_empty());
    }

    /// The clause-compile path warns with the same shape: slug + error.
    #[test]
    fn a_degraded_clause_composition_warns_with_the_slug_and_the_error() {
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let _guard = capture_into(&buf);
        let source = "---\nkind: agreement\nclauses:\n  law:\n    source: absent@1\n---\n{{> law}}";
        let doc = from_markdown("sample-form", "", source);

        let log = logged(&buf);
        assert!(
            log.contains("clause compilation degraded"),
            "warning event: {log}"
        );
        assert!(
            log.contains("sample-form"),
            "warning carries the slug: {log}"
        );
        assert!(
            log.contains("unknown Clause version"),
            "warning carries the compile error: {log}"
        );

        let error = doc
            .questionnaire_error
            .expect("a composition failure degrades loudly");
        assert!(error.contains("Clause composition failed"), "{error}");
    }

    /// A template with both blocks projects onto a page carrying both
    /// sections: the questionnaire's declared order and the workflow's
    /// declared graph, each read from this file alone.
    #[test]
    fn reads_both_declared_blocks_out_of_one_template() {
        let src = "---\ntitle: Sample Letter\ncode: sample__letter\nquestionnaire:\n  BEGIN:\n    \
                   _: custom_text__client_name\n  custom_text__client_name:\n    _: END\n  END: \
                   {}\nprompts:\n  client_name: What is your \
                   name?\nworkflow:\n  BEGIN:\n    intake_submitted: \
                   lawyer_review\n  lawyer_review:\n    approved: END\n  END: {}\n---\n\n# Sample \
                   Letter\n\nBody prose.\n";

        let doc = from_markdown("sample-letter", "/tmp/sample.md", src);

        assert_eq!(doc.slug, "sample-letter");
        assert_eq!(doc.source_href, "/tmp/sample.md");
        assert_eq!(
            doc.demo_questions
                .iter()
                .map(|q| q.code.as_str())
                .collect::<Vec<_>>(),
            vec!["custom_text__client_name"],
            "the demo walks the template's own declared question order"
        );
        assert_eq!(doc.demo_questions[0].prompt, "What is your name?");
        assert!(
            doc.demo_workflow.iter().any(|s| s.name == "lawyer_review"),
            "the diagram carries every reachable declared workflow state"
        );
        assert!(doc.frontmatter.contains("code: sample__letter"));
        assert!(
            !doc.stage_html.is_empty(),
            "every notation body has a stage"
        );
        assert!(
            doc.origin_url.is_none(),
            "a letter declares no government blank to link to"
        );
    }

    /// A form's `origin_url` reaches the page, and a template with neither
    /// declared block renders neither section rather than an empty one.
    #[test]
    fn carries_origin_url_and_omits_undeclared_sections() {
        let src = "---\ntitle: Some Form\ncode: us__some_form\norigin_url: \
                   https://example.gov/f.pdf\n---\n\n# Some Form\n\nCover prose.\n";

        let doc = from_markdown("some-form", "blob://x", src);

        assert_eq!(doc.origin_url.as_deref(), Some("https://example.gov/f.pdf"));
        assert!(doc.demo_questions.is_empty());
        assert!(doc.demo_workflow.is_empty());
    }
}
