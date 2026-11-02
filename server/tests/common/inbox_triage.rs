//! Private-message review through the real local OIDC and form routes.

use std::time::Duration;

use fantoccini::Locator;
use features::webdriver::{
    base_url, login_as_lawyer, new_client_or_skip, scroll_and_js_click, wait_for_text,
};
use store::inbox_triage::{NewInboxTriage, PrivilegeLabel, TriageDisposition, RULES_ONLY_PROMPT};

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn private_social_assignment_waits_for_human_privilege_review() {
    let Some(browser) = new_client_or_skip().await else {
        return;
    };
    login_as_lawyer(&browser).await;
    let db = store::surreal::connect_from_env()
        .await
        .expect("local store");
    let firm = store::firms::firm_id_for_brand_key(&db, "neon")
        .await
        .expect("Brand lookup")
        .expect("local Neon Brand");
    let project = store::projects::find_by_code(&db, "sample-litigation")
        .await
        .expect("Project lookup")
        .expect("participating Lawyer's sample Project")
        .id;
    let provider_id = format!("private-review-{}", uuid::Uuid::now_v7());
    let sender = format!("Synthetic review {}", &provider_id[provider_id.len() - 6..]);
    let message = store::inbox::ingest_inbound(
        &db,
        &store::inbox::IngestInbound {
            firm_id: firm,
            brand_key: Some("neon"),
            interaction_type: Some("direct_message"),
            provider_post_id: None,
            parent_provider_message_id: None,
            visibility: Some("private"),
            channel: "x",
            provider_account: "synthetic-browser-review",
            provider_message_id: &provider_id,
            provider_thread_ref: None,
            external_address: "reviewer@example.com",
            external_name: Some(&sender),
            subject: None,
            body: "Synthetic private message for local review.",
            source_page: None,
            source_channel: None,
            matter_consent: false,
            promotional_opt_in: false,
            email_conversation_id: None,
            attachments: Vec::new(),
            project_id: None,
            occurred_at: "2026-11-02T05:00:00Z",
        },
    )
    .await
    .expect("synthetic private Message");
    let triage = store::inbox_triage::record(
        &db,
        &NewInboxTriage {
            message_id: message.message_id,
            firm_id: firm,
            suggested_project_id: Some(project),
            project_confidence: None,
            privilege: PrivilegeLabel::Uncertain,
            privilege_confidence: None,
            rationale: None,
            model_id: None,
            prompt_version: RULES_ONLY_PROMPT.into(),
            raw_output: None,
            disposition: TriageDisposition::NeedsHuman,
            labeled_example: false,
        },
    )
    .await
    .expect("human review row");
    let selector = format!("#triage-row-{}", triage.id);
    browser
        .goto(&format!("{}/app/inbox/triage", base_url()))
        .await
        .expect("review queue");
    wait_for_text(&browser, "Triage review", Duration::from_secs(15)).await;
    let text = browser
        .find(Locator::Css(&selector))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        text.contains("uncertain") && text.contains("needs human"),
        "{text}"
    );
    capture(&browser, "00-uncertain").await;
    scroll_and_js_click(
        &browser,
        &format!("{selector} form[action*='/confirm/'] button"),
    )
    .await;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let conversation = store::inbox::conversation_by_id(&db, message.conversation_id)
            .await
            .unwrap()
            .unwrap();
        if conversation.project_id == Some(project) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "confirm did not assign: {}",
            browser.current_url().await.unwrap()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let assigned = store::inbox_triage::find_by_id(&db, triage.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(assigned.privilege, PrivilegeLabel::Uncertain);
    assert_eq!(assigned.disposition, TriageDisposition::NeedsHuman);
    browser
        .goto(&format!("{}/app/inbox/triage", base_url()))
        .await
        .unwrap();
    wait_for_text(&browser, "Triage review", Duration::from_secs(15)).await;
    browser
        .find(Locator::Css(&selector))
        .await
        .expect("assignment retains review row");
    capture(&browser, "01-assigned-still-uncertain").await;
    let reason = browser
        .find(Locator::Css(&format!(
            "{selector} form[action$='/resolve'] textarea[name='rationale']"
        )))
        .await
        .unwrap();
    browser.execute(
        "const field=document.querySelector(arguments[0]); field.scrollIntoView({block:'center',behavior:'instant'}); field.focus(); field.value=arguments[1]; field.dispatchEvent(new Event('input',{bubbles:true})); field.dispatchEvent(new Event('change',{bubbles:true}));",
        vec![serde_json::json!(format!("{selector} form[action$='/resolve'] textarea[name='rationale']")), serde_json::json!("Human reviewed this synthetic private message.")],
    ).await.unwrap();
    assert_eq!(
        reason.prop("value").await.unwrap().as_deref(),
        Some("Human reviewed this synthetic private message.")
    );
    capture(&browser, "01b-human-review").await;
    scroll_and_js_click(
        &browser,
        &format!("{selector} form[action$='/resolve'] button"),
    )
    .await;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !browser
        .find_all(Locator::Css(&selector))
        .await
        .unwrap()
        .is_empty()
    {
        assert!(
            std::time::Instant::now() < deadline,
            "resolve did not clear the row: {}",
            browser.current_url().await.unwrap()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let resolved = store::inbox_triage::find_by_id(&db, triage.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.privilege, PrivilegeLabel::Privileged);
    assert_eq!(resolved.disposition, TriageDisposition::Confirmed);
    capture(&browser, "02-resolved").await;
    browser
        .goto(&format!("{}/app/inbox?brand=neon", base_url()))
        .await
        .unwrap();
    wait_for_text(&browser, "Showing Brand neon", Duration::from_secs(15)).await;
    scroll_and_js_click(
        &browser,
        "nav[aria-label='Filter inbox by channel'] a[href*='channel=x']",
    )
    .await;
    wait_for_text(&browser, "Showing x", Duration::from_secs(15)).await;
    wait_for_text(&browser, "Showing Brand neon", Duration::from_secs(15)).await;
    capture(&browser, "03-brand-filter").await;
    browser.close().await.unwrap();
}

async fn capture(browser: &fantoccini::Client, state: &str) {
    if let Ok(directory) = std::env::var("NAVIGATOR_TRIAGE_CAPTURE_DIR") {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            std::path::Path::new(&directory).join(format!("{state}.png")),
            browser.screenshot().await.unwrap(),
        )
        .unwrap();
    }
}
