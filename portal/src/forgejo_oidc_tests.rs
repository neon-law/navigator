//! Which Forgejo Actions runs the deployment trusts: configuration it
//! refuses, run claims it refuses, and tokens it refuses, each against a
//! loopback discovery document on an ephemeral port.

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{
    decode_claims, validate_claim_times, ForgejoActionsClaims, ForgejoOidc, ForgejoOidcError,
    ForgejoRun,
};
use crate::auth::JwksDocument;
use crate::session::now_unix_secs;
use crate::test_support::{sign_rs256_claims, test_oidc_jwks_rsa, TEST_OIDC_KID};

const ISSUER: &str = "https://forgejo.example/api/actions";
const AUDIENCE: &str = "navigator-ci";
const GATE: &str = ".forgejo/workflows/project-gate.yml";
const CD: &str = ".forgejo/workflows/cd.yml";

fn trusted() -> ForgejoOidc {
    ForgejoOidc::configured_with_cd(ISSUER, AUDIENCE, GATE, CD).unwrap()
}

/// A gate run for a push to `main` of `firm/acme`, signed moments ago.
fn push(issuer: &str) -> ForgejoActionsClaims {
    let now = now_unix_secs();
    ForgejoActionsClaims {
        sub: "repo:firm/acme:ref:refs/heads/main".into(),
        iss: issuer.into(),
        aud: AUDIENCE.into(),
        exp: now + 300,
        iat: now - 10,
        nbf: now - 10,
        actor: "synthetic".into(),
        base_ref: String::new(),
        event_name: "push".into(),
        head_ref: String::new(),
        git_ref: "refs/heads/main".into(),
        ref_protected: "true".into(),
        ref_type: "branch".into(),
        repository: "firm/acme".into(),
        repository_owner: "firm".into(),
        run_attempt: "1".into(),
        run_id: "17".into(),
        run_number: "4".into(),
        sha: "9f3ba2c7114e0d5689c0b1a2f4e5d6c7b8a9f0e1".into(),
        workflow: "project-gate.yml".into(),
        workflow_ref: format!("firm/acme/{GATE}@refs/heads/main"),
    }
}

fn cd_push() -> ForgejoActionsClaims {
    ForgejoActionsClaims {
        workflow: "cd.yml".into(),
        workflow_ref: format!("firm/acme/{CD}@refs/heads/main"),
        ..push(ISSUER)
    }
}

fn pull_request() -> ForgejoActionsClaims {
    ForgejoActionsClaims {
        sub: "repo:firm/acme:pull_request".into(),
        base_ref: "main".into(),
        event_name: "pull_request".into(),
        git_ref: "refs/pull/7/head".into(),
        workflow_ref: format!("firm/acme/{GATE}@refs/pull/7/head"),
        ..push(ISSUER)
    }
}

fn refused<T: std::fmt::Debug>(result: Result<T, ForgejoOidcError>, why: &str) {
    match result {
        Err(ForgejoOidcError::Invalid(_)) => {}
        other => panic!("{why}: {other:?}"),
    }
}

#[test]
fn trust_configuration_outside_the_forgejo_contract_is_refused() {
    type Case = (
        &'static str,
        &'static str,
        &'static str,
        Option<&'static str>,
    );
    let cases: [Case; 14] = [
        ("not a url", AUDIENCE, GATE, None),
        (ISSUER, "  ", GATE, None),
        (ISSUER, AUDIENCE, ".github/workflows/gate.yml", None),
        (ISSUER, AUDIENCE, ".forgejo/workflows/../gate.yml", None),
        (ISSUER, AUDIENCE, ".forgejo/workflows/gate.yml@main", None),
        (ISSUER, AUDIENCE, ".forgejo/workflows/", None),
        (ISSUER, AUDIENCE, GATE, Some(GATE)),
        (ISSUER, AUDIENCE, GATE, Some("cd.yml")),
        (
            "https://user@forgejo.example/api/actions",
            AUDIENCE,
            GATE,
            None,
        ),
        (
            "https://forgejo.example/api/actions?x=1",
            AUDIENCE,
            GATE,
            None,
        ),
        (
            "https://forgejo.example/api/actions#x",
            AUDIENCE,
            GATE,
            None,
        ),
        ("https://forgejo.example/api/actions/", AUDIENCE, GATE, None),
        ("https://forgejo.example/api", AUDIENCE, GATE, None),
        ("http://127.0.0.1:3000/api/actions", AUDIENCE, GATE, None),
    ];
    for (issuer, audience, gate, cd) in cases {
        let built = match cd {
            Some(cd) => ForgejoOidc::configured_with_cd(issuer, audience, gate, cd),
            None => ForgejoOidc::configured(issuer, audience, gate),
        };
        refused(
            built.map(|_| ()),
            &format!("{issuer} {audience:?} {gate} {cd:?}"),
        );
    }
    // Plain HTTP is a loopback-with-port test concession, never a host name.
    for issuer in [
        "http://forgejo.example:3000/api/actions",
        "http://127.0.0.1/api/actions",
    ] {
        refused(
            ForgejoOidc::local_test_issuer(issuer, AUDIENCE, GATE).map(|_| ()),
            issuer,
        );
    }
    assert!(
        ForgejoOidc::local_test_issuer("http://127.0.0.1:3000/api/actions", AUDIENCE, GATE).is_ok()
    );
}

#[test]
fn deployment_variables_enable_trust_only_when_complete_and_valid() {
    let from = |pairs: &'static [(&'static str, &'static str)]| {
        ForgejoOidc::from_variables(|name| {
            pairs
                .iter()
                .find(|(key, _)| name.ends_with(key))
                .map(|(_, value)| (*value).to_owned())
        })
    };
    let is_rejecting = |oidc: &ForgejoOidc| {
        matches!(
            oidc.authorize_run(&push(ISSUER)),
            Err(ForgejoOidcError::Rejected)
        )
    };
    assert!(is_rejecting(&from(&[])));
    assert!(is_rejecting(&from(&[
        ("OIDC_ISSUER", ISSUER),
        ("OIDC_AUDIENCE", AUDIENCE)
    ])));
    assert!(is_rejecting(&from(&[
        ("OIDC_ISSUER", "http://forgejo.example/api/actions"),
        ("OIDC_AUDIENCE", AUDIENCE),
        ("OIDC_WORKFLOW", GATE),
    ])));

    let gate_only = from(&[
        ("OIDC_ISSUER", ISSUER),
        ("OIDC_AUDIENCE", AUDIENCE),
        ("OIDC_WORKFLOW", GATE),
        ("OIDC_CD_WORKFLOW", " "),
    ]);
    assert_eq!(
        gate_only.authorize_run(&push(ISSUER)).unwrap(),
        ForgejoRun::MainGate
    );
    refused(
        gate_only.authorize_run(&cd_push()),
        "no CD workflow is trusted",
    );

    let with_cd = from(&[
        ("OIDC_ISSUER", ISSUER),
        ("OIDC_AUDIENCE", AUDIENCE),
        ("OIDC_WORKFLOW", GATE),
        ("OIDC_CD_WORKFLOW", CD),
    ]);
    assert_eq!(
        with_cd.authorize_run(&cd_push()).unwrap(),
        ForgejoRun::MainCd
    );
}

#[test]
fn each_trusted_run_is_named_and_only_cd_on_main_may_write() {
    let oidc = trusted();
    assert_eq!(
        oidc.authorize_run(&push(ISSUER)).unwrap(),
        ForgejoRun::MainGate
    );
    assert_eq!(oidc.authorize_run(&cd_push()).unwrap(), ForgejoRun::MainCd);
    assert_eq!(
        oidc.authorize_run(&pull_request()).unwrap(),
        ForgejoRun::PullRequest
    );
    let dispatched = ForgejoActionsClaims {
        event_name: "workflow_dispatch".into(),
        ..cd_push()
    };
    assert_eq!(oidc.authorize_run(&dispatched).unwrap(), ForgejoRun::MainCd);

    // A pull request naming the CD workflow is held to the gate path.
    let cd_from_a_pull_request = ForgejoActionsClaims {
        workflow: "cd.yml".into(),
        workflow_ref: format!("firm/acme/{CD}@refs/pull/7/head"),
        ..pull_request()
    };
    refused(
        oidc.authorize_run(&cd_from_a_pull_request),
        "a pull request never runs CD",
    );
    assert!(matches!(
        ForgejoOidc::rejecting().authorize_run(&push(ISSUER)),
        Err(ForgejoOidcError::Rejected)
    ));
}

#[test]
fn a_run_outside_the_trusted_workflow_contract_is_refused() {
    type Edit = fn(&mut ForgejoActionsClaims);
    let on_main: [(&str, Edit); 12] = [
        ("repository with a path", |c| {
            c.repository = "firm/acme/extra".into();
        }),
        ("another owner", |c| c.repository_owner = "other".into()),
        ("another workflow file", |c| c.workflow = "other.yml".into()),
        ("another workflow path", |c| {
            c.workflow_ref = "firm/acme/.forgejo/workflows/other.yml@refs/heads/main".into();
        }),
        ("another repository's workflow", |c| {
            c.workflow_ref = format!("other/acme/{GATE}@refs/heads/main");
        }),
        ("short sha", |c| c.sha = "9f3ba2c".into()),
        ("non-hex sha", |c| c.sha = "z".repeat(40)),
        ("run id", |c| c.run_id = "17a".into()),
        ("attempt zero", |c| c.run_attempt = "0".into()),
        ("a branch other than main", |c| {
            c.git_ref = "refs/heads/feature".into();
            c.sub = "repo:firm/acme:ref:refs/heads/feature".into();
            c.workflow_ref = format!("firm/acme/{GATE}@refs/heads/feature");
        }),
        ("a tag", |c| c.ref_type = "tag".into()),
        ("another repository's subject", |c| {
            c.sub = "repo:firm/other:ref:refs/heads/main".into();
        }),
    ];
    for (why, edit) in on_main {
        let mut claims = push(ISSUER);
        edit(&mut claims);
        refused(trusted().authorize_run(&claims), why);
    }

    let on_a_pull_request: [(&str, Edit); 7] = [
        ("another base", |c| c.base_ref = "release".into()),
        ("a merge ref", |c| {
            c.git_ref = "refs/pull/7/merge".into();
            c.workflow_ref = format!("firm/acme/{GATE}@refs/pull/7/merge");
        }),
        ("pull request zero", |c| {
            c.git_ref = "refs/pull/0/head".into();
            c.workflow_ref = format!("firm/acme/{GATE}@refs/pull/0/head");
        }),
        ("an unnumbered pull request", |c| {
            c.git_ref = "refs/pull//head".into();
            c.workflow_ref = format!("firm/acme/{GATE}@refs/pull//head");
        }),
        ("a push subject", |c| {
            c.sub = "repo:firm/acme:ref:refs/pull/7/head".into();
        }),
        ("a workflow from another ref", |c| {
            c.workflow_ref = format!("firm/acme/{GATE}@refs/heads/main");
        }),
        ("an unlisted event", |c| c.event_name = "schedule".into()),
    ];
    for (why, edit) in on_a_pull_request {
        let mut claims = pull_request();
        edit(&mut claims);
        refused(trusted().authorize_run(&claims), why);
    }
}

#[test]
fn a_token_is_spent_once_until_its_expiry() {
    let oidc = trusted();
    let now = now_unix_secs();
    oidc.spend_token("first.signed.token", now + 300).unwrap();
    assert!(matches!(
        oidc.spend_token("first.signed.token", now + 300),
        Err(ForgejoOidcError::Replayed)
    ));
    // A renewed token from the same run is a different token.
    oidc.spend_token("renewed.signed.token", now + 300).unwrap();
    // An expired entry no longer guards anything; signature time checks do.
    oidc.spend_token("stale.signed.token", now - 1).unwrap();
    oidc.spend_token("stale.signed.token", now + 300).unwrap();
}

#[test]
fn timestamps_outside_the_accepted_window_are_refused() {
    type Edit = fn(&mut ForgejoActionsClaims, i64);
    assert!(validate_claim_times(&push(ISSUER)).is_ok());
    let cases: [(&str, Edit); 6] = [
        ("expired", |c, now| c.exp = now),
        ("no issue time", |c, _| c.iat = 0),
        ("issued in the future", |c, now| c.iat = now + 120),
        ("not yet valid", |c, now| c.nbf = now + 120),
        ("expires before issue", |c, now| {
            c.iat = now + 20;
            c.exp = now + 10;
        }),
        ("longer than an hour", |c, now| c.exp = now + 3600),
    ];
    for (why, edit) in cases {
        let mut claims = push(ISSUER);
        edit(&mut claims, now_unix_secs());
        refused(validate_claim_times(&claims), why);
    }
}

#[test]
fn a_published_key_must_be_a_complete_rs256_rsa_key() {
    let token = sign_rs256_claims(TEST_OIDC_KID, &push(ISSUER));
    let with = |edit: fn(&mut crate::auth::JwksKey)| {
        let mut key = test_oidc_jwks_rsa();
        edit(&mut key);
        decode_claims(
            &token,
            ISSUER,
            AUDIENCE,
            TEST_OIDC_KID,
            &JwksDocument { keys: vec![key] },
        )
    };
    assert_eq!(with(|_| {}).unwrap().repository, "firm/acme");
    refused(with(|key| key.kty = "EC".into()), "not an RSA key");
    refused(with(|key| key.alg = Some("RS512".into())), "another alg");
    refused(with(|key| key.n = None), "no modulus");
    refused(with(|key| key.e = None), "no exponent");
    refused(with(|key| key.n = Some("!".into())), "unreadable modulus");
}

/// A loopback Forgejo whose discovery document and key set are `discovery`
/// and `keys`. Returns the issuer the deployment would pin.
async fn forgejo(
    discovery: impl FnOnce(&str) -> serde_json::Value,
    keys: Vec<serde_json::Value>,
) -> (MockServer, String) {
    let server = MockServer::start().await;
    let issuer = format!("{}/api/actions", server.uri());
    Mock::given(method("GET"))
        .and(path("/api/actions/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(discovery(&issuer)))
        .mount(&server)
        .await;
    for (index, body) in keys.iter().enumerate() {
        let mock = Mock::given(method("GET"))
            .and(path("/api/actions/.well-known/keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body));
        if index + 1 < keys.len() {
            mock.up_to_n_times(1).mount(&server).await;
        } else {
            mock.mount(&server).await;
        }
    }
    (server, issuer)
}

fn bound(issuer: &str) -> serde_json::Value {
    serde_json::json!({ "issuer": issuer, "jwks_uri": format!("{issuer}/.well-known/keys") })
}

fn published() -> serde_json::Value {
    serde_json::json!({ "keys": [test_oidc_jwks_rsa()] })
}

async fn key_fetches(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path().ends_with("/keys"))
        .count()
}

#[tokio::test]
async fn a_signed_run_token_verifies_against_the_pinned_issuer_and_caches_its_keys() {
    let (server, issuer) = forgejo(bound, vec![published()]).await;
    let oidc = ForgejoOidc::local_test_issuer(&issuer, AUDIENCE, GATE).unwrap();
    let token = sign_rs256_claims(TEST_OIDC_KID, &push(&issuer));

    for _ in 0..2 {
        let claims = oidc.verify(&token).await.unwrap();
        assert_eq!(claims.repository, "firm/acme");
        assert_eq!(oidc.authorize_run(&claims).unwrap(), ForgejoRun::MainGate);
    }
    assert_eq!(key_fetches(&server).await, 1, "the key set is cached");

    assert!(matches!(
        oidc.verify("  ").await,
        Err(ForgejoOidcError::Missing)
    ));
    assert!(matches!(
        ForgejoOidc::rejecting().verify(&token).await,
        Err(ForgejoOidcError::Rejected)
    ));
}

#[tokio::test]
async fn tokens_the_issuer_did_not_sign_for_this_deployment_are_refused() {
    let (server, issuer) = forgejo(bound, vec![published()]).await;
    let oidc = ForgejoOidc::local_test_issuer(&issuer, AUDIENCE, GATE).unwrap();
    let segment = |json: &str| {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
    };

    refused(oidc.verify("not-a-jwt").await, "unreadable header");
    let symmetric = format!(
        "{}.{}.c2ln",
        segment(r#"{"alg":"HS256","kid":"k"}"#),
        segment("{}")
    );
    refused(oidc.verify(&symmetric).await, "HS256");
    let anonymous = format!("{}.{}.c2ln", segment(r#"{"alg":"RS256"}"#), segment("{}"));
    refused(oidc.verify(&anonymous).await, "no kid");
    assert_eq!(
        key_fetches(&server).await,
        0,
        "header refusals fetch nothing"
    );

    // A stale token is correctly signed, so no key refresh is spent on it.
    let stale = ForgejoActionsClaims {
        iat: now_unix_secs() - 7200,
        ..push(&issuer)
    };
    refused(
        oidc.verify(&sign_rs256_claims(TEST_OIDC_KID, &stale)).await,
        "older than the accepted lifetime",
    );

    // The first failed signature check spends the one allowed key refresh;
    // later failures are refused without another fetch.
    let other_audience = ForgejoActionsClaims {
        aud: "another-deployment".into(),
        ..push(&issuer)
    };
    refused(
        oidc.verify(&sign_rs256_claims(TEST_OIDC_KID, &other_audience))
            .await,
        "another audience",
    );
    let fetched = key_fetches(&server).await;
    let other_issuer = ForgejoActionsClaims {
        iss: ISSUER.into(),
        ..push(&issuer)
    };
    for (kid, claims) in [(TEST_OIDC_KID, &other_issuer), ("rotated-away", &stale)] {
        match oidc.verify(&sign_rs256_claims(kid, claims)).await {
            Err(ForgejoOidcError::Invalid(reason)) => {
                assert!(reason.contains("throttled"), "{reason}");
            }
            other => panic!("expected a throttled refusal, got {other:?}"),
        }
    }
    assert_eq!(key_fetches(&server).await, fetched);
}

#[tokio::test]
async fn a_rotated_signing_key_is_picked_up_by_one_refetch() {
    let (server, issuer) =
        forgejo(bound, vec![serde_json::json!({ "keys": [] }), published()]).await;
    let oidc = ForgejoOidc::local_test_issuer(&issuer, AUDIENCE, GATE).unwrap();
    let token = sign_rs256_claims(TEST_OIDC_KID, &push(&issuer));
    assert_eq!(oidc.verify(&token).await.unwrap().run_id, "17");
    assert_eq!(key_fetches(&server).await, 2);
}

#[tokio::test]
async fn discovery_metadata_must_be_bound_to_the_pinned_issuer() {
    type Discovery = fn(&str) -> serde_json::Value;
    let unbound: [(&str, Discovery); 5] = [
        (
            "another issuer",
            |issuer| serde_json::json!({ "issuer": ISSUER, "jwks_uri": format!("{issuer}/.well-known/keys") }),
        ),
        (
            "keys on another origin",
            |issuer| serde_json::json!({ "issuer": issuer, "jwks_uri": format!("{ISSUER}/.well-known/keys") }),
        ),
        (
            "keys on another path",
            |issuer| serde_json::json!({ "issuer": issuer, "jwks_uri": format!("{issuer}/keys") }),
        ),
        (
            "an unreadable key address",
            |issuer| serde_json::json!({ "issuer": issuer, "jwks_uri": "not a url" }),
        ),
        (
            "an incomplete document",
            |issuer| serde_json::json!({ "issuer": issuer }),
        ),
    ];
    for (why, discovery) in unbound {
        let (server, issuer) = forgejo(discovery, vec![published()]).await;
        let oidc = ForgejoOidc::local_test_issuer(&issuer, AUDIENCE, GATE).unwrap();
        let token = sign_rs256_claims(TEST_OIDC_KID, &push(&issuer));
        refused(oidc.verify(&token).await, why);
        assert_eq!(key_fetches(&server).await, 0, "{why}");
    }

    // An issuer that answers neither document, then one whose key set fails.
    let silent = MockServer::start().await;
    let issuer = format!("{}/api/actions", silent.uri());
    let oidc = ForgejoOidc::local_test_issuer(&issuer, AUDIENCE, GATE).unwrap();
    let token = sign_rs256_claims(TEST_OIDC_KID, &push(&issuer));
    refused(oidc.verify(&token).await, "no discovery document");
    Mock::given(method("GET"))
        .and(path("/api/actions/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(bound(&issuer)))
        .mount(&silent)
        .await;
    refused(oidc.verify(&token).await, "no key set");
}
