//! `navigator site firm` and `navigator site brand` against a deployment's
//! Firm directory.

use serde_json::json;

use crate::site::Site;

const DUPLICATE: &str = "0192f0a0-0000-7000-8000-000000000001";
const SURVIVOR: &str = "0192f0a0-0000-7000-8000-000000000002";

async fn directory() -> Site {
    let site = Site::start().await;
    let firms = json!([
        { "id": DUPLICATE, "name": "Example Duplicate", "slug": "example-duplicate",
          "brands": ["example-brand"] },
        { "id": SURVIVOR, "name": "Example Survivor", "slug": "example-survivor", "brands": [] }
    ]);
    site.answer("GET", "/app/api/firms", 200, firms).await;
    site
}

#[tokio::test]
async fn firm_and_brand_verbs_name_the_door_each_one_calls() {
    let site = directory().await;
    let duplicate = format!("/app/api/firms/{DUPLICATE}");
    let brand = "/app/api/brands/example-brand";
    for (method, path) in [
        ("PATCH", duplicate.clone()),
        ("DELETE", duplicate.clone()),
        ("PUT", format!("{brand}/firm")),
        ("DELETE", format!("{duplicate}/brands/example-brand")),
        ("DELETE", brand.to_string()),
    ] {
        site.answer(method, &path, 200, json!({})).await;
    }
    let cases: Vec<(Vec<&str>, &str, String, serde_json::Value, &str)> = vec![
        (
            vec!["firm", "rename", "example-duplicate", "Example Renamed"],
            "PATCH",
            duplicate.clone(),
            json!({ "name": "Example Renamed" }),
            "example-duplicate: renamed \"Example Duplicate\" to \"Example Renamed\"",
        ),
        (
            vec!["firm", "theme", "example-duplicate", "example-brand"],
            "PATCH",
            duplicate.clone(),
            json!({ "portal_theme_brand_key": "example-brand" }),
            "example-duplicate: portal theme is now example-brand",
        ),
        (
            vec!["firm", "theme", "example-duplicate", "--clear"],
            "PATCH",
            duplicate.clone(),
            json!({ "portal_theme_brand_key": null }),
            "example-duplicate: portal theme cleared",
        ),
        (
            vec!["brand", "move", "example-brand", "--to", "example-survivor"],
            "PUT",
            format!("{brand}/firm"),
            json!({ "firm_id": SURVIVOR }),
            "example-brand: now worn by Example Survivor (example-survivor)",
        ),
        (
            vec!["brand", "detach", "example-brand"],
            "DELETE",
            format!("{duplicate}/brands/example-brand"),
            json!({}),
            "example-brand: no longer worn by Example Duplicate (example-duplicate)",
        ),
        (
            vec!["brand", "delete", "example-brand"],
            "DELETE",
            brand.to_string(),
            json!({}),
            "example-brand: deleted",
        ),
        (
            vec!["firm", "delete", "example-duplicate"],
            "DELETE",
            duplicate.clone(),
            json!({}),
            "example-duplicate: deleted \"Example Duplicate\"",
        ),
    ];
    for (verb, method, path, body, printed) in cases {
        let ran = site.navigator(&[&["site"][..], &verb].concat()).await;
        assert_eq!(ran.succeeded().trim(), printed, "{verb:?}");
        assert_eq!(
            site.last_request().await,
            (method.to_string(), path, Some(body)),
            "{verb:?}"
        );
    }

    let listed = site.navigator(&["site", "firm", "list"]).await;
    assert_eq!(
        listed.succeeded().trim(),
        "example-duplicate\tExample Duplicate\texample-brand\n\
         example-survivor\tExample Survivor\t(no brands)"
    );
    site.navigator(&["site", "firm", "delete", "example-unknown"])
        .await
        .refused(1, "no Firm with the slug \"example-unknown\"");
    site.navigator(&["site", "brand", "detach", "example-unworn"])
        .await
        .refused(1, "wears the Brand \"example-unworn\"");
}

#[tokio::test]
async fn firm_secret_verbs_store_list_and_revoke_a_stripe_key() {
    let site = directory().await;
    let secrets = format!("/app/api/firms/{DUPLICATE}/secrets");
    let stored = json!({
        "mode": "sandbox",
        "keys": [
            { "account": "operating", "kind": "secret", "mode": "sandbox",
              "updated_at": "2099-01-02T00:00:00Z" },
            { "account": "nevada", "kind": "publishable", "mode": null,
              "updated_at": "2099-01-03T00:00:00Z" }
        ]
    });
    site.answer("GET", &secrets, 200, stored).await;
    let key = format!("{secrets}/stripe/operating/secret/sandbox");
    for method in ["PUT", "DELETE"] {
        wiremock::Mock::given(wiremock::matchers::method(method))
            .and(wiremock::matchers::path(key.clone()))
            .respond_with(wiremock::ResponseTemplate::new(204))
            .mount(&site.server)
            .await;
    }
    let firm = ["--firm", "example-duplicate"];
    let target = |verb: &'static str, mode: &'static str| {
        let mut args = vec!["site", "firm", "secret", verb];
        args.extend(firm);
        args.extend(["--provider", "stripe", "--account", "operating"]);
        args.extend(["--kind", "secret", "--mode", mode]);
        args
    };

    let listed = site
        .navigator(&[&["site", "firm", "secret", "list"][..], &firm].concat())
        .await;
    assert_eq!(
        listed.succeeded(),
        "stripe\toperating\tsecret\tsandbox\t2099-01-02T00:00:00+00:00\n\
         stripe\tnevada\tpublishable\tunrecorded\t2099-01-03T00:00:00+00:00\n"
    );

    let set = target("set", "sandbox");
    site.navigator_reading(&set, "rk_test_example\n")
        .await
        .succeeded();
    let requests = site.server.received_requests().await.unwrap();
    let put = requests.last().unwrap();
    assert_eq!(
        (put.method.as_str(), put.url.path(), &put.body[..]),
        ("PUT", key.as_str(), &b"rk_test_example"[..])
    );

    site.navigator(&target("delete", "sandbox"))
        .await
        .succeeded();
    let (method, path, _) = site.last_request().await;
    assert_eq!((method.as_str(), path), ("DELETE", key));

    site.navigator_reading(&set, "rk_live_example\n")
        .await
        .refused(1, "does not match the selected kind and mode");
    site.navigator(&target("delete", "live"))
        .await
        .refused(1, "this deployment accepts only sandbox Stripe keys");
}

#[tokio::test]
async fn firm_secret_verbs_report_what_the_deployment_refused() {
    let site = directory().await;
    let secrets = format!("/app/api/firms/{DUPLICATE}/secrets");
    let list = ["site", "firm", "secret", "list", "--firm"];
    site.navigator(&[&list[..], &["example-duplicate"]].concat())
        .await
        .refused(1, "read Firm secret metadata refused (404 Not Found)");
    site.navigator(&[&list[..], &["example-unknown"]].concat())
        .await
        .refused(1, "no Firm with the slug \"example-unknown\"");

    let empty = json!({ "mode": "sandbox", "keys": [] });
    site.answer("GET", &secrets, 200, empty).await;
    let mut delete = vec!["site", "firm", "secret", "delete", "--firm"];
    delete.extend(["example-duplicate", "--provider", "stripe"]);
    delete.extend(["--account", "nevada", "--kind", "publishable"]);
    delete.extend(["--mode", "sandbox"]);
    site.navigator(&delete)
        .await
        .refused(1, "Firm secret operation refused (404 Not Found)");
}
