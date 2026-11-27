//! Browser sign-in uses the one registered callback origin, and a brand host
//! gets the sign-in handed back for its own session.

/// Run against the generated worktree environment and the browser harness.
/// Distinct loopback names exercise actual host-only cookie isolation: the
/// sign-in passes through the primary callback and ends on the brand listener
/// with that listener's own session.
#[tokio::test]
async fn browser_login_from_secondary_loopback_host_is_handed_back() {
    use features::webdriver::{base_url, new_client_or_skip, require_harness, wait_for_path};
    use std::time::Duration;

    let Ok(port) = std::env::var("NAVIGATOR_LOCAL_DELETE_YOUR_DATA_PORT") else {
        assert!(
            !require_harness(),
            "source the worktree environment for the secondary listener"
        );
        return;
    };
    let Some(browser) = new_client_or_skip().await else {
        return;
    };
    let primary = url::Url::parse(&base_url()).unwrap();
    assert_eq!(primary.host_str(), Some("localhost"));
    let secondary = format!("http://127.0.0.1:{port}");
    let secondary_origin = url::Url::parse(&secondary).unwrap().origin();
    login_with_optional_capture(&browser, &secondary).await;
    wait_for_path(&browser, "/app/projects", Duration::from_secs(20)).await;
    assert_eq!(
        browser.current_url().await.unwrap().origin(),
        secondary_origin
    );
    let cookies = browser.get_all_cookies().await.unwrap();
    assert!(cookies
        .iter()
        .any(|cookie| cookie.name() == "navigator_session"));
    assert!(cookies
        .iter()
        .all(|cookie| cookie.name() != "navigator_handoff_nonce"));
    browser
        .goto(&format!("{secondary}/app/projects"))
        .await
        .unwrap();
    wait_for_path(&browser, "/app/projects", Duration::from_secs(20)).await;
    assert_eq!(
        browser.current_url().await.unwrap().origin(),
        secondary_origin
    );
    assert!(!browser
        .source()
        .await
        .unwrap()
        .contains("could not be completed"));
    browser.close().await.unwrap();
}

async fn login_with_optional_capture(browser: &fantoccini::Client, secondary: &str) {
    let password = ["pass", "word"].concat();
    let login = features::webdriver::login_as_at(
        browser,
        secondary,
        "lawyer@neonlaw.com",
        &password,
        "/app/projects",
    );
    if std::env::var("NAV_OAUTH_SHOTS").as_deref() != Ok("1") {
        login.await;
        return;
    }
    let directory = std::path::Path::new("/tmp/navigator-screenshots/oauth-host");
    std::fs::create_dir_all(directory).unwrap();
    tokio::pin!(login);
    let mut frame = 0;
    loop {
        let complete = tokio::select! {
            () = &mut login => true,
            () = tokio::time::sleep(std::time::Duration::from_millis(400)) => false,
        };
        std::fs::write(
            directory.join(format!("frame-{frame:04}.png")),
            browser.screenshot().await.unwrap(),
        )
        .unwrap();
        frame += 1;
        if complete {
            break;
        }
    }
}

// --- Per-host sign-in -------------------------------------------------------
