//! Layout of the public notation preview. The page is local: this server has
//! no store, and the demo has no form action. Chromedriver is optional; without
//! it the test skips unless `NAV_REQUIRE_HARNESS=1`.

use std::net::SocketAddr;
use std::time::Duration;

use axum::body::Body;
use axum::extract::Path as PathParam;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use fantoccini::Client;
use features::webdriver::new_client_or_skip;
use tokio::net::TcpListener;
use webapp::notation_demo::{DemoColumn, DemoQuestion};

const VIEWPORTS: [(u32, u32); 4] = [(320, 640), (375, 700), (768, 900), (1280, 800)];

#[tokio::test]
async fn public_preview_fits_questions_fields_and_the_document() {
    let Some(client) = new_client_or_skip().await else {
        return;
    };
    let addr = serve().await;
    let url = format!("http://{addr}/notations/synthetic-preview");
    std::fs::create_dir_all("/tmp/navigator-screenshots").expect("screenshot directory");
    let cursor_artifacts = std::path::Path::new("/opt/cursor/artifacts");
    let capture = if std::fs::create_dir_all(cursor_artifacts).is_ok() {
        cursor_artifacts
    } else {
        std::path::Path::new("/tmp/navigator-screenshots")
    };

    for (width, height) in VIEWPORTS {
        set_viewport(&client, width, height).await;
        client.goto(&url).await.expect("preview");
        client
            .wait()
            .for_element(fantoccini::Locator::Css(".notation-demo__count"))
            .await
            .expect("question count");
        assert_no_horizontal_overflow(&client, width).await;
        assert_descendants_fit(&client).await;
        let png = client.screenshot().await.expect("screenshot");
        let name = format!("notation-preview-{width}.png");
        std::fs::write(capture.join(&name), &png).expect("write artifact");
        std::fs::write(format!("/tmp/navigator-screenshots/{name}"), &png).expect("write shot");
    }

    set_viewport(&client, 1280, 800).await;
    client.goto(&url).await.expect("preview");
    let count = client
        .find(fantoccini::Locator::Css(".notation-demo__count"))
        .await
        .expect("count")
        .text()
        .await
        .expect("count text");
    assert!(count.starts_with("Question 1 of "), "{count}");
    let outline = client
        .find(fantoccini::Locator::Css(".notation-demo__outline"))
        .await
        .expect("outline");
    let open = outline.attr("open").await.expect("open attr");
    assert!(open.is_none(), "outline starts closed");
    outline
        .find(fantoccini::Locator::Css("summary"))
        .await
        .expect("summary")
        .click()
        .await
        .expect("open outline");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_descendants_fit(&client).await;

    inject_corner_widget(&client).await;
    let button = client
        .find(fantoccini::Locator::Css(
            ".notation-demo__actions .nav-btn--primary",
        ))
        .await
        .expect("continue");
    let _ = button;
    client
        .execute(
            "document.querySelector('.notation-demo__actions .nav-btn--primary').scrollIntoView({block:'center'});",
            vec![],
        )
        .await
        .expect("scroll continue");
    let hit = client
        .execute(
            "const el = document.querySelector('.notation-demo__actions .nav-btn--primary');\
             const r = el.getBoundingClientRect();\
             const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);\
             return {reachable: el.contains(hit) || hit === el, top: r.top, bottom: r.bottom, vh: window.innerHeight};",
            vec![],
        )
        .await
        .expect("hit test");
    assert!(
        hit["reachable"].as_bool().unwrap_or(false),
        "continue stays reachable beside a corner widget: {hit}"
    );

    let html = client.source().await.expect("source");
    assert!(!html.contains("<form"), "public demo has no form: {html}");
    assert!(
        html.contains("Answers remain available on this page")
            || html.contains("saved in this browser for this notation"),
        "preview names where answers live: {html}"
    );
    assert!(
        html.contains("example.com"),
        "synthetic document is present"
    );
    client.close().await.ok();
}

/// The basic will's public preview, composed exactly the way the published
/// site composes it — `notation_preview_doc::from_markdown` over the bundled
/// template — walked once per state the will serves.
///
/// This server renders server-side only: the demo's Continue button is a
/// hydrated `onclick`, and no wasm bundle exists here, so the walk asserts
/// what the SSR page must carry — the full declared chain as the count and
/// outline, the state question as a real radio choice for each of the four
/// states (not the non-interactive explanation the `jurisdiction__law` state
/// rendered before it became a declared choice), and a body that composes
/// without leaking raw template markers. The hydrated stepping through the
/// same chain is the matter-bound walk's job in `browser_e2e.rs`.
#[tokio::test]
async fn basic_will_preview_offers_every_states_question() {
    const WILL_STATES: [(&str, &str); 4] = [
        ("CA", "California"),
        ("NV", "Nevada"),
        ("NY", "New York"),
        ("WA", "Washington"),
    ];

    let Some(client) = new_client_or_skip().await else {
        return;
    };
    let addr = serve_doc(basic_will_doc()).await;
    let url = format!("http://{addr}/notations/basic-will");

    for (key, label) in WILL_STATES {
        client.goto(&url).await.expect("preview");
        client
            .wait()
            .for_element(fantoccini::Locator::Css(".notation-demo__count"))
            .await
            .expect("demo question count");
        let count = client
            .find(fantoccini::Locator::Css(".notation-demo__count"))
            .await
            .expect("count element")
            .text()
            .await
            .expect("count text");
        assert_eq!(
            count, "Question 1 of 8",
            "{label}: the demo must offer the will's full declared chain — an empty or truncated questionnaire is the reported symptom, at the browser layer"
        );

        // The state question is interactive: a radio card per declared
        // choice, including the state under test. Before the will's state
        // question declared its choices, this step rendered as an
        // explanation and no state could be walked in the demo.
        let state_radio =
            format!("input[type='radio'][name='custom_single_choice__law'][value='{key}']");
        client
            .find(fantoccini::Locator::Css(&state_radio))
            .await
            .unwrap_or_else(|_| {
                panic!("{label}: the demo's first question must render the state choice as a radio group")
            });
        let state_label = client
            .execute(
                &format!(
                    "const el = document.querySelector('{state_radio}'); \
                     return el ? el.closest('label').textContent.trim() : null;"
                ),
                vec![],
            )
            .await
            .expect("state choice label");
        assert!(
            state_label
                .as_str()
                .is_some_and(|text| text.contains(label)),
            "{label}: the state card's label must name the state, got {state_label:?}"
        );

        // The outline lists every declared question, so a parse that drops
        // questions cannot pass silently.
        let outline_items = client
            .find_all(fantoccini::Locator::Css(".notation-demo__outline ol li"))
            .await
            .expect("outline items");
        assert_eq!(
            outline_items.len(),
            8,
            "{label}: the outline must list all 8 declared questions, got {}",
            outline_items.len()
        );

        // The composed page must carry no raw template markers: the preview
        // is fail-loud, so composition failures may not silently degrade
        // into `{{#if` leakage or an unanswered-blank scaffold.
        let html = client.source().await.expect("source");
        assert!(
            !html.contains("{{"),
            "{label}: raw template markers leaked into the composed preview: {html:.1000}",
        );
        assert!(
            !html.contains("notation-demo__explanation"),
            "{label}: the first question rendered as a non-interactive explanation instead of a control"
        );
        assert_no_horizontal_overflow(&client, 1280).await;
    }

    client.close().await.ok();
}

fn basic_will_doc() -> webapp::notation_preview::PreviewDoc {
    let source = include_str!("../../templates/notations/neon_law/basic_will.md");
    portal::notation_preview_doc::from_markdown("basic-will", "", source)
}

async fn serve() -> SocketAddr {
    serve_doc(webapp::notation_preview::PreviewDoc {
        slug: "synthetic-preview".to_string(),
        title: "Synthetic preview".to_string(),
        source_href: String::new(),
        frontmatter: "title: Synthetic preview\ncode: synthetic_preview\n".to_string(),
        stage_html: stage_html(),
        origin_url: Some(
            "https://example.com/forms/very-long-government-form-location".to_string(),
        ),
        demo_questions: questions(),
        demo_workflow: vec![webapp::notation_workflow::WorkflowStateView {
            name: "BEGIN".to_string(),
            transitions: vec![("submit".to_string(), "END".to_string())],
        }],
        questionnaire_error: None,
    })
    .await
}

async fn serve_doc(doc: webapp::notation_preview::PreviewDoc) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = router(doc);
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    addr
}

fn router(doc: webapp::notation_preview::PreviewDoc) -> Router {
    let app = portal::dioxus_app::notation_preview_router(
        vec![doc],
        webapp::notation_preview::NotationPreviewMode::Local,
        None,
    );
    app.route("/public/css/{*path}", get(css_asset))
}

async fn css_asset(PathParam(path): PathParam<String>) -> Response {
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("public/css")
        .join(&path);
    match std::fs::read(&file) {
        Ok(bytes) => ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], bytes).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, Body::from("no such asset")).into_response(),
    }
}

fn questions() -> Vec<DemoQuestion> {
    let long = "Which option applies when the label itself is a long URL https://example.com/questions/option-with-a-very-long-path-that-must-wrap-inside-the-column?";
    let mut questions = vec![schedule_question(), choice_question(long)];
    for number in 3..=12 {
        questions.push(DemoQuestion {
            code: format!("custom_text__item_{number}"),
            answer_type: "custom_text".to_string(),
            prompt: format!("Question {number}: describe item {number} in enough words that the prompt wraps on a narrow screen without leaving the column."),
            choices: Vec::new(),
            interactive: true,
            role_label: format!("Item {number}"),
            help: "Answer in plain language. This preview stores nothing.".to_string(),
            ..DemoQuestion::default()
        });
    }
    questions
}

fn schedule_question() -> DemoQuestion {
    DemoQuestion {
        code: "schedule__licenses".to_string(),
        answer_type: "schedule".to_string(),
        prompt: "List each license, including a long product name if you have one.".to_string(),
        interactive: true,
        role_label: "Licenses".to_string(),
        group: "Schedules".to_string(),
        help: "Add one row per item. Each cell is text, a number, or a date.".to_string(),
        columns: vec![
            DemoColumn {
                name: "product".to_string(),
                label: "Product name that can be quite long".to_string(),
                value_type: "text".to_string(),
            },
            DemoColumn {
                name: "seats".to_string(),
                label: "Seats".to_string(),
                value_type: "number".to_string(),
            },
            DemoColumn {
                name: "renews".to_string(),
                label: "Renews".to_string(),
                value_type: "date".to_string(),
            },
        ],
        ..DemoQuestion::default()
    }
}

fn choice_question(prompt: &str) -> DemoQuestion {
    DemoQuestion {
        code: "custom_single_choice__source".to_string(),
        answer_type: "custom_single_choice".to_string(),
        prompt: prompt.to_string(),
        choices: vec![
            (
                "example".to_string(),
                "https://example.com/options/a-label-long-enough-to-wrap-beside-the-control"
                    .to_string(),
            ),
            ("other".to_string(), "Other published page".to_string()),
        ],
        interactive: true,
        role_label: "Source".to_string(),
        ..DemoQuestion::default()
    }
}

fn stage_html() -> String {
    let cell = "<td>https://example.com/documents/wide-preview-cell-that-must-wrap-inside-a-fixed-table</td>";
    let mut row = String::new();
    for _ in 0..6 {
        row.push_str(cell);
    }
    format!(
        "<article class=\"harvard-stage\" data-harvard-outline><table><tr>{row}</tr></table><p>Synthetic document for example.com.</p></article>"
    )
}

async fn set_viewport(client: &Client, width: u32, height: u32) {
    client
        .set_window_size(width, height)
        .await
        .expect("viewport");
}

async fn assert_no_horizontal_overflow(client: &Client, width: u32) {
    let value = client
        .execute(
            "return [document.documentElement.scrollWidth, document.documentElement.clientWidth];",
            vec![],
        )
        .await
        .expect("widths");
    let widths = value.as_array().expect("pair");
    let scroll = widths[0].as_u64().expect("scroll");
    let client_width = widths[1].as_u64().expect("client");
    assert!(
        scroll <= client_width + 1,
        "viewport {width}: scrollWidth {scroll} exceeds clientWidth {client_width}"
    );
}

/// The workflow graph is wider than a phone column and scrolls inside
/// `.notation-workflow__viewport`. Those SVG boxes sit past the preview
/// edge on purpose; page-level overflow is the `scrollWidth` assertion.
async fn assert_descendants_fit(client: &Client) {
    let value = client
        .execute(
            "const root = document.querySelector('.notation-preview');\
             const box = root.getBoundingClientRect();\
             const nodes = root.querySelectorAll('*');\
             const offenders = [];\
             for (const node of nodes) {\
               if (node.closest('.notation-workflow__viewport')) continue;\
               const style = getComputedStyle(node);\
               if (style.display === 'none' || style.visibility === 'hidden') continue;\
               const rect = node.getBoundingClientRect();\
               if (rect.width === 0 || rect.height === 0) continue;\
               if (rect.right > box.right + 1 || rect.left < box.left - 1) {\
                 offenders.push(node.className || node.tagName);\
               }\
             }\
             return offenders.slice(0, 8);",
            vec![],
        )
        .await
        .expect("boxes");
    let offenders = value.as_array().cloned().unwrap_or_default();
    assert!(
        offenders.is_empty(),
        "descendants outside the preview: {offenders:?}"
    );
}

async fn inject_corner_widget(client: &Client) {
    client
        .execute(
            "const widget = document.createElement('div');\
             widget.id = 'synthetic-chat-widget';\
             widget.style.cssText = 'position:fixed;right:0;bottom:0;width:56px;height:56px;z-index:1000;background:#000';\
             document.body.appendChild(widget);",
            vec![],
        )
        .await
        .expect("widget");
}
