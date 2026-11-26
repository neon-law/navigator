#![allow(clippy::doc_markdown)]
//! The draft contract prepares a synthetic PDF and reads signer progress by envelope ID.
//! CI runs the contract against a loopback HTTP mock. The ignored live test runs the same
//! contract using explicit DocuSign demo credentials. The embedded-signing test sends to a captive sandbox recipient
//! when `NAVIGATOR_RUN_LIVE_SANDBOX=1`. Completing the signing ceremony and
//! obtaining a real Certificate of Completion require a human signer.
//!
//! Live credentials use `DOCUSIGN_SANDBOX_*` or their corresponding `DOCUSIGN_*` names:
//! - `DOCUSIGN_SANDBOX_INTEGRATION_KEY` / `DOCUSIGN_INTEGRATION_KEY`,
//!   `DOCUSIGN_SANDBOX_USER_ID` / `DOCUSIGN_USER_ID`,
//!   `DOCUSIGN_SANDBOX_RSA_KEY` / `DOCUSIGN_PRIVATE_KEY` (JWT grant; see
//!   `portal::docusign_auth`),
//! - `DOCUSIGN_SANDBOX_BASE_URL` / `DOCUSIGN_BASE_URL`
//!   (e.g. `https://demo.docusign.net/restapi`),
//! - `DOCUSIGN_SANDBOX_ACCOUNT_ID` / `DOCUSIGN_ACCOUNT_ID`,
//!   `DOCUSIGN_SANDBOX_SIGNER_EMAIL` / `DOCUSIGN_SIGNER_EMAIL`.

use std::time::{SystemTime, UNIX_EPOCH};

use uuid::Uuid;

use portal::docusign_auth::DocuSignJwtAuth;
use portal::signature::{
    DocuSignSignatureProvider, RecipientView, SendMode, SignatureManifest, SignatureProvider,
    SignatureRecipient,
};
use portal::signature_render::expand_signatures;

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.is_empty())
}

/// Read a sandbox-scoped var, falling back to the canonical `DOCUSIGN_*`
/// name. This lets a developer simply `source .env` (which uses the
/// runtime scheme) to drive the live test, instead of exporting a
/// parallel `DOCUSIGN_SANDBOX_*` set.
fn sandbox_or_canonical(sandbox_key: &str, canonical_key: &str) -> Option<String> {
    env(sandbox_key).or_else(|| env(canonical_key))
}

fn demo_provider() -> Option<DocuSignSignatureProvider> {
    assert!(
        std::env::var_os("GITHUB_ACTIONS").is_none(),
        "CI uses the local DocuSign mock"
    );
    let base = sandbox_or_canonical("DOCUSIGN_SANDBOX_BASE_URL", "DOCUSIGN_BASE_URL")?;
    assert_eq!(
        base.trim_end_matches('/'),
        "https://demo.docusign.net/restapi"
    );
    let oauth = sandbox_or_canonical("DOCUSIGN_SANDBOX_OAUTH_BASE", "DOCUSIGN_OAUTH_BASE");
    assert!(
        matches!(
            oauth.as_deref().map(|url| url.trim_end_matches('/')),
            None | Some("https://account-d.docusign.com" | "account-d.docusign.com")
        ),
        "sandbox tests require the DocuSign demo OAuth host"
    );
    let auth = DocuSignJwtAuth::new(
        sandbox_or_canonical(
            "DOCUSIGN_SANDBOX_INTEGRATION_KEY",
            "DOCUSIGN_INTEGRATION_KEY",
        )?,
        sandbox_or_canonical("DOCUSIGN_SANDBOX_USER_ID", "DOCUSIGN_USER_ID")?,
        "https://account-d.docusign.com",
        sandbox_or_canonical("DOCUSIGN_SANDBOX_RSA_KEY", "DOCUSIGN_PRIVATE_KEY")?.into_bytes(),
    );
    Some(DocuSignSignatureProvider::with_jwt_auth(
        base,
        sandbox_or_canonical("DOCUSIGN_SANDBOX_ACCOUNT_ID", "DOCUSIGN_ACCOUNT_ID")?,
        auth,
        "sandbox@example.com",
        "Sandbox Signer",
    ))
}

#[tokio::test]
#[ignore = "creates an unsent draft in DocuSign demo using explicit sandbox credentials"]
async fn sandbox_reads_an_unregistered_draft_by_id() {
    let provider = demo_provider().expect("configure DocuSign demo JWT credentials");
    let id = prepare_and_read_draft(&provider).await;
    eprintln!("prepared DocuSign demo draft {id}");
}

/// LAW-501/LAW-430 live: read by ID, void a sent envelope, delete a draft.
/// The recipients are embedded (`clientUserId`), so DocuSign emails no one.
#[tokio::test]
#[ignore = "sends to embedded sandbox recipients in DocuSign demo, then voids and deletes"]
async fn sandbox_voids_a_sent_envelope_and_deletes_a_draft() {
    let provider = demo_provider().expect("configure DocuSign demo JWT credentials");
    let sent = prepare_and_read_draft(&provider).await;
    provider
        .dispatch_prepared(&portal::signature::SignatureRequestId(sent.to_string()))
        .await
        .expect("send the sandbox envelope");
    assert_eq!(provider.read_external_envelope(sent).await.unwrap().status, "sent");
    provider
        .void_envelope(sent, "Navigator sandbox void check")
        .await
        .expect("void the sent envelope");
    assert_eq!(provider.read_external_envelope(sent).await.unwrap().status, "voided");
    eprintln!("voided DocuSign demo envelope {sent}");

    let draft = prepare_and_read_draft(&provider).await;
    provider.delete_draft(draft).await.expect("delete the draft");
    let after = provider.read_external_envelope(draft).await;
    eprintln!(
        "deleted DocuSign demo draft {draft}; a read now answers {:?}",
        after.map(|envelope| envelope.status)
    );
}

async fn prepare_and_read_draft(provider: &DocuSignSignatureProvider) -> Uuid {
    let (body, fields) = expand_signatures(
        "Navigator sandbox consent.\n\n{{client.signature}}\n\n{{firm.signature}}",
    );
    let pdf = pdf::render(&body).expect("render synthetic consent PDF");
    let recipients = [("firm", 2), ("client", 1)].map(|(role, routing_order)| SignatureRecipient {
        role: role.into(),
        email: format!("{role}@example.com"),
        name: format!("Sandbox {role}"),
        routing_order,
        client_user_id: Some(role.into()),
    });
    let manifest = SignatureManifest {
        recipients: recipients.to_vec(),
        fields,
    };
    let draft = provider
        .send_for_signature(Uuid::new_v4(), &pdf, &manifest, SendMode::Prepare)
        .await
        .expect("prepare an unsent DocuSign demo envelope");
    let id = Uuid::parse_str(&draft.0).expect("DocuSign assigned an envelope UUID");
    for _ in 0..2 {
        let evidence = provider
            .read_external_envelope(id)
            .await
            .expect("read the unregistered draft");
        assert_eq!(evidence.envelope_id, id);
        assert_eq!(evidence.status, "created");
        assert!(evidence.sent_date_time.is_none());
        assert!(evidence
            .envelope_documents
            .iter()
            .any(|doc| doc.name == "Document"));
        assert_eq!(evidence.signers.len(), 2);
        for (signer, (role, order)) in evidence
            .signers
            .iter()
            .zip([("client", "1"), ("firm", "2")])
        {
            assert_eq!(signer.email, format!("{role}@example.com"));
            assert_eq!(signer.routing_order.as_deref(), Some(order));
            assert!(signer.signed_date_time.is_none());
        }
    }
    id
}

/// Run the same PDF preparation and repeated draft reads as the live demo test in CI.
#[tokio::test]
async fn draft_contract_uses_mock_docusign_http() {
    use serde_json::json;
    use wiremock::{
        matchers::{body_partial_json, header, method, path, query_param},
        Mock, MockServer, ResponseTemplate,
    };
    let http = MockServer::start().await;
    let provider = DocuSignSignatureProvider::new(
        http.uri(),
        "account",
        "synthetic-token",
        "firm@example.com",
        "Test Firm",
    );
    let collection = "/v2.1/accounts/account/envelopes";
    let id = Uuid::new_v4();
    let path_to_draft = format!("{collection}/{id}");
    let auth = || header("authorization", "Bearer synthetic-token");
    Mock::given(method("POST"))
        .and(path(collection))
        .and(auth())
        .and(body_partial_json(json!({"status":"created"})))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(json!({"envelopeId":id,"status":"created"})),
        )
        .expect(1)
        .mount(&http)
        .await;
    Mock::given(method("GET"))
        .and(path(path_to_draft.as_str()))
        .and(auth())
        .and(query_param("include", "documents"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"envelopeId":id,"status":"created","envelopeDocuments":[{"name":"Document"}]}),
        ))
        .expect(2)
        .mount(&http)
        .await;
    Mock::given(method("GET")).and(path(format!("{path_to_draft}/recipients"))).and(auth())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"signers":[
            {"name":"Sandbox firm","email":"firm@example.com","status":"created","routingOrder":"2"},
            {"name":"Sandbox client","email":"client@example.com","status":"created","routingOrder":"1"}
        ]}))).expect(2).mount(&http).await;
    assert_eq!(prepare_and_read_draft(&provider).await, id);
    assert_eq!(http.received_requests().await.unwrap().len(), 5);
}

#[tokio::test]
async fn sandbox_accepts_a_retainer_envelope_with_anchor_tabs() {
    if env("NAVIGATOR_RUN_LIVE_SANDBOX").as_deref() != Some("1") {
        return;
    }
    let Some(provider) = demo_provider() else {
        eprintln!("skipping: no DocuSign demo credentials");
        return;
    };
    let Some(signer_email) =
        sandbox_or_canonical("DOCUSIGN_SANDBOX_SIGNER_EMAIL", "DOCUSIGN_SIGNER_EMAIL")
    else {
        eprintln!("skipping: sandbox signer email is missing");
        return;
    };

    // Render a tiny retainer whose body anchors a client signature.
    let (typst, fields) =
        expand_signatures("Sandbox retainer.\n\nSign below:\n\n{{client.signature}}\n");
    assert!(!fields.is_empty(), "the body must place a signature field");
    let pdf = pdf::render(&typst).expect("signature blocks compile to a PDF");

    // One real signer (the dev account's signer email) keyed to the
    // anchored field. Mark them CAPTIVE (`client_user_id`) so this also
    // grounds the embedded-signing path: DocuSign suppresses the email
    // and we drive `createRecipientView` below.
    let client_user_id = "sandbox-embedded-client".to_string();
    let manifest = SignatureManifest {
        recipients: vec![SignatureRecipient {
            role: "client".into(),
            email: signer_email.clone(),
            name: "Sandbox Signer".into(),
            routing_order: 1,
            client_user_id: Some(client_user_id.clone()),
        }],
        fields,
    };
    let id = provider
        .send_for_signature(Uuid::new_v4(), &pdf, &manifest, SendMode::Send)
        .await
        .expect("DocuSign demo must accept our envelope + anchored tabs");
    assert!(!id.0.is_empty(), "a real envelope id comes back");
    eprintln!("created sandbox envelope {}", id.0);

    // Embedded-signing grounding (Phase 0.3): request a recipient view
    // for the captive signer. This proves the `views/recipient` path,
    // the clientUserId match, and that DocuSign issues a real signing
    // URL — the URL a human then opens to actually sign.
    let signing_url = provider
        .create_recipient_view(
            &id,
            &RecipientView {
                return_url: "https://www.neonlaw.com/portal".into(),
                email: signer_email,
                name: "Sandbox Signer".into(),
                client_user_id,
            },
        )
        .await
        .expect("DocuSign demo must issue an embedded recipient view URL");
    assert!(
        signing_url.starts_with("https://"),
        "the demo recipient view uses HTTPS"
    );

    // Ground the scheme-less OAuth-base hardening against the REAL demo
    // OAuth server (not just the unit test in `docusign_auth`). Prod's
    // `DOCUSIGN_OAUTH_BASE` was once configured without a scheme
    // (`account.docusign.com`), which made `mint` build a relative token
    // URL and fail. Build an auth whose base is deliberately scheme-less
    // and confirm it still mints a real token — i.e. `normalize_oauth_base`
    // produces a URL DocuSign actually accepts.
    let (Some(ik), Some(user), Some(pem)) = (
        sandbox_or_canonical(
            "DOCUSIGN_SANDBOX_INTEGRATION_KEY",
            "DOCUSIGN_INTEGRATION_KEY",
        ),
        sandbox_or_canonical("DOCUSIGN_SANDBOX_USER_ID", "DOCUSIGN_USER_ID"),
        sandbox_or_canonical("DOCUSIGN_SANDBOX_RSA_KEY", "DOCUSIGN_PRIVATE_KEY"),
    ) else {
        eprintln!("skipping scheme-less grounding: JWT grant env not fully present");
        return;
    };
    // `account-d.docusign.com` — the demo OAuth host, WITHOUT a scheme.
    let scheme_less = DocuSignJwtAuth::new(ik, user, "account-d.docusign.com", pem.into_bytes());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let token2 = scheme_less
        .mint_access_token(now)
        .await
        .expect("a scheme-less demo OAuth base must still mint a real token");
    assert!(
        !token2.is_empty(),
        "normalize_oauth_base yields a token URL DocuSign accepts"
    );
    eprintln!(
        "scheme-less OAuth base minted a live demo token ({} chars)",
        token2.len()
    );

    // NOTE — remaining grounding that needs a HUMAN (cannot be automated
    // here; DocuSign has no API to programmatically click "sign"):
    //   1. Open `signing_url`, sign, and let the envelope reach
    //      `completed`. Then `provider.fetch_completed_documents(&id)`
    //      should return the combined signed PDF + the Certificate of
    //      Completion (the `/documents/combined` + `/documents/certificate`
    //      paths). Before completion DocuSign returns 409/404 for those,
    //      so we do not assert the download here.
    //   2. From the sandbox Connect log, capture a real `completed` and a
    //      real `declined` payload and pin them as fixtures, then assert
    //      `ConnectPayload::is_completed` / `is_declined` (the hermetic
    //      tests in `esignature_webhook` currently use hand-written shapes).
    //   3. Configure a sandbox Connect webhook with an HMAC key, capture
    //      the `X-DocuSign-Signature-1` header on a real delivery, and
    //      confirm `portal::webhook_auth::verify_hmac_sha256_b64` accepts it
    //      — the single most important thing to ground, since the prod
    //      HMAC key is still a placeholder.
}
