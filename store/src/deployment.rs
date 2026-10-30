//! Typed deployment-invariant metadata shared by service startup and
//! `navigator ops ship` preflight.

use crate::DeploymentEnvironment;

pub const CREDENTIAL_ENVIRONMENT: &str = "NAVIGATOR_CREDENTIAL_ENVIRONMENT";
pub const CI_HARNESS: &str = "NAVIGATOR_CI_HARNESS";
/// The mounted configuration flag for the deployment that consumes the
/// global engineering webhook.
pub const GITHUB_AUTOMATION_HOME_FLAG: &str = "NAVIGATOR_GITHUB_AUTOMATION_HOME";
pub const SMS_TEST_RECIPIENTS_FLAG: &str = "NAVIGATOR_SMS_TEST_RECIPIENTS_DEPLOYMENT";

/// The mounted configuration flag for the deployment that sends test SMS.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeploymentCapabilities {
    pub github_automation_home: bool,
    pub sms_test_recipients_home: bool,
}

impl DeploymentCapabilities {
    pub fn from_lookup<F: Fn(&str) -> Option<String>>(get: &F) -> anyhow::Result<Self> {
        Ok(Self {
            github_automation_home: parse_capability_flag(GITHUB_AUTOMATION_HOME_FLAG, get)?,
            sms_test_recipients_home: parse_capability_flag(SMS_TEST_RECIPIENTS_FLAG, get)?,
        })
    }
}

fn parse_capability_flag<F: Fn(&str) -> Option<String>>(
    key: &str,
    get: &F,
) -> anyhow::Result<bool> {
    match get(key).as_deref() {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(other) => anyhow::bail!(
            "{key} is `{other}`, but it must be exactly `true` or `false`. Declare both capability \
             flags as quoted strings under the deployment's `[env]`:\n\
             \n    {GITHUB_AUTOMATION_HOME_FLAG} = \"false\"\
             \n    {SMS_TEST_RECIPIENTS_FLAG} = \"false\"\n\
             \nand set `\"true\"` only on the one deployment that holds that role"
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RequirementScope {
    #[default]
    EveryDeployment,
    GithubAutomationHome,
    SmsTestRecipientsHome,
}

impl RequirementScope {
    #[must_use]
    pub const fn applies(self, capabilities: DeploymentCapabilities) -> bool {
        match self {
            Self::EveryDeployment => true,
            Self::GithubAutomationHome => capabilities.github_automation_home,
            Self::SmsTestRecipientsHome => capabilities.sms_test_recipients_home,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Requirement {
    pub any_of: &'static [&'static [&'static str]],
    pub trigger: Option<&'static str>,
    pub integration: bool,
    /// Restrict this requirement to a deployment explicitly marked with the
    /// matching capability in its mounted configuration.
    pub scope: RequirementScope,
}

macro_rules! required {
    ($key:literal) => {
        Requirement {
            any_of: &[&[$key]],
            trigger: None,
            integration: false,
            scope: RequirementScope::EveryDeployment,
        }
    };
    (integration $key:literal) => {
        Requirement {
            any_of: &[&[$key]],
            trigger: None,
            integration: true,
            scope: RequirementScope::EveryDeployment,
        }
    };
    // An integration a deployment may decline entirely, but may not
    // half-configure: required only once `$trigger` declares the integration
    // present.
    (integration $key:literal if $trigger:literal) => {
        Requirement {
            any_of: &[&[$key]],
            trigger: Some($trigger),
            integration: true,
            scope: RequirementScope::EveryDeployment,
        }
    };
}

/// Web boot requirements whose key shape is also consumed by `ops ship`.
pub static WEB_REQUIREMENTS: &[Requirement] = &[
    required!("RESTATE_BROKER_URL"),
    required!("NAVIGATOR_STORAGE_BACKEND"),
    // The private bucket each Project's published Portal bundle lives in,
    // streamed same-origin through `/app/projects/{code}/portal`. Mandatory like
    // the other object-storage lanes: `ops gcp setup` provisions it for every
    // deployment, so a deployment missing the coordinate is a real gap `ops ship`
    // must refuse rather than a lane it may decline.
    required!("NAVIGATOR_APPLICATIONS_BUCKET"),
    // Operational SurrealDB exports have a separate confidentiality and
    // retention boundary from client documents. A missing dedicated bucket
    // must stop boot and `ops ship`; it must never degrade to a generic
    // storage or documents bucket.
    required!("NAVIGATOR_SURREAL_ARCHIVES_BUCKET"),
    required!("NAVIGATOR_CLAMD_ADDR"),
    required!(integration "NAVIGATOR_EMAIL_BACKEND"),
    required!(integration "SENDGRID_API_KEY"),
    required!(integration "SENDGRID_FROM_EMAIL"),
    required!(integration "SENDGRID_INBOUND_SECRET"),
    required!(integration "SENDGRID_EVENTS_SECRET"),
    required!(integration "SENDGRID_EVENTS_PUBLIC_KEY"),
    // Dash0 is declared by the endpoint coordinate and is otherwise absent.
    // The endpoint and dataset are plaintext coordinates; the token is
    // projected from the deployment's encrypted Secret. A declared exporter
    // must carry all three values so the renderer cannot silently turn a
    // half-configured row into a Google-only pipeline.
    required!(integration "DASH0_DATASET" if "DASH0_ENDPOINT"),
    required!(integration "DASH0_TOKEN" if "DASH0_ENDPOINT"),
    // DocuSign is declared by `DOCUSIGN_BASE_URL` and is otherwise absent.
    //
    // A deployment that executes no documents supplies none of these and runs
    // `StubSignatureProvider`. The stub is reachable only through genuine
    // absence: `portal::signature::DocuSignSignatureProvider::from_env`
    // returns `Some` for any non-empty value, so a placeholder would boot the
    // *real* provider holding a credential DocuSign will reject — a green
    // deploy that fails on the first signature request. Requiring the keys
    // unconditionally left a placeholder as the only way to ship, which is why
    // this is a trigger rather than five plain requirements.
    //
    // Declaring the integration still demands every key the real provider
    // reads, so a half-configured DocuSign remains a hard failure. Both
    // deployments set `DOCUSIGN_BASE_URL`, so nothing relaxes for them.
    required!(integration "DOCUSIGN_ACCOUNT_ID" if "DOCUSIGN_BASE_URL"),
    Requirement {
        any_of: &[
            &[
                "DOCUSIGN_INTEGRATION_KEY",
                "DOCUSIGN_USER_ID",
                "DOCUSIGN_PRIVATE_KEY",
            ],
            &["DOCUSIGN_ACCESS_TOKEN"],
        ],
        trigger: Some("DOCUSIGN_BASE_URL"),
        integration: true,
        scope: RequirementScope::EveryDeployment,
    },
    required!(integration "DOCUSIGN_HMAC_KEY" if "DOCUSIGN_BASE_URL"),
    required!(integration "DOCUSIGN_WEBHOOK_SECRET" if "DOCUSIGN_BASE_URL"),
    // The two remaining keys the real provider reads, modelled under the same
    // trigger so the integration is described in one place rather than
    // half here and half in a provider-side check.
    //
    // `DOCUSIGN_OAUTH_BASE` was already enforced for a production JWT
    // credential by `portal::config::enforce_deployment_invariants`, which
    // rejects an empty value once `DOCUSIGN_INTEGRATION_KEY` is set. Stating it
    // here makes that conditional readable from the table every other gate
    // reads. `DOCUSIGN_SIGNER_EMAIL` has no such backstop: `portal` defaults it
    // to a hard-coded address, so a deployment that declares DocuSign and omits
    // it sends envelopes from whatever that default happens to be. A declared
    // integration names its own signer.
    required!(integration "DOCUSIGN_OAUTH_BASE" if "DOCUSIGN_BASE_URL"),
    required!(integration "DOCUSIGN_SIGNER_EMAIL" if "DOCUSIGN_BASE_URL"),
    // The Project-repo organization: read by Forge's repository governance
    // (`ops github setup`) and by Project-repository reconciliation, neither of
    // which is GitHub-App-specific. Integration-tier, so the staging CI
    // harness may skip it.
    required!(integration "NAVIGATOR_GITHUB_ORG"),
    // The Restate ingress endpoint + bearer that every `*-trigger` CronJob
    // submits a one-way invocation through. Still project-scoped to the
    // automation home here, matching this row's pre-existing scope; other
    // deployments are not preflighted for it by this table. Integration-tier:
    // the staging CI harness runs no trigger.
    Requirement {
        any_of: &[&["RESTATE_INGRESS_URL", "RESTATE_AUTH_TOKEN"]],
        trigger: None,
        integration: true,
        scope: RequirementScope::GithubAutomationHome,
    },
    required!(integration "NAVIGATOR_CREDENTIAL_ENVIRONMENT"),
    // Restate Cloud signs requests to the production worker. The public key
    // is deployment configuration, not secret material, and KIND remains
    // keyless through the CI-harness integration relaxation.
    required!(integration "RESTATE_IDENTITY_KEY"),
    // Target-aware Slack Web API delivery for per-Project private channels.
    // The bot token is required for both staging and production once this
    // feature is enabled; local/KIND uses the capturing backend when absent.
    required!(integration "SLACK_BOT_TOKEN"),
    // Runtime KMS stands alone: it seals Firm integration secrets and Brand
    // social account tokens, and `web` and `workflows-service` each build it
    // from this one key. The Notion lane needs it to decrypt its token, so a
    // Notion database demands the key; the key never demands Notion.
    required!(integration "NAVIGATOR_RUNTIME_KMS_KEY" if "NAVIGATOR_NOTION_DATABASE_ID"),
    // Brand social accounts. The public app id is a `config.toml` coordinate
    // and declares the provider; its secret is production-only key material
    // (`deployments/sops-inventory.yaml`). A declared app needs its secret,
    // the callback `ship` renders inline, and runtime KMS to seal the account
    // tokens; a secret without its app id is half a configuration. Meta also
    // receives the Page and Instagram webhooks through `portal::social_webhooks`.
    // Supplying neither leaves `portal::social_oauth` unconfigured.
    required!(integration "NAVIGATOR_META_APP_SECRET" if "NAVIGATOR_META_APP_ID"),
    required!(integration "NAVIGATOR_META_APP_ID" if "NAVIGATOR_META_APP_SECRET"),
    required!(integration "NAVIGATOR_META_WEBHOOK_VERIFY_TOKEN" if "NAVIGATOR_META_APP_ID"),
    required!(integration "NAVIGATOR_META_WEBHOOK_APP_SECRET" if "NAVIGATOR_META_APP_ID"),
    required!(integration "NAVIGATOR_SOCIAL_OAUTH_REDIRECT_URI" if "NAVIGATOR_META_APP_ID"),
    required!(integration "NAVIGATOR_RUNTIME_KMS_KEY" if "NAVIGATOR_META_APP_ID"),
    required!(integration "NAVIGATOR_YOUTUBE_CLIENT_SECRET" if "NAVIGATOR_YOUTUBE_CLIENT_ID"),
    required!(integration "NAVIGATOR_YOUTUBE_CLIENT_ID" if "NAVIGATOR_YOUTUBE_CLIENT_SECRET"),
    required!(integration "NAVIGATOR_SOCIAL_OAUTH_REDIRECT_URI" if "NAVIGATOR_YOUTUBE_CLIENT_ID"),
    required!(integration "NAVIGATOR_RUNTIME_KMS_KEY" if "NAVIGATOR_YOUTUBE_CLIENT_ID"),
    required!(integration "NAVIGATOR_X_CLIENT_SECRET" if "NAVIGATOR_X_CLIENT_ID"),
    required!(integration "NAVIGATOR_X_CLIENT_ID" if "NAVIGATOR_X_CLIENT_SECRET"),
    required!(integration "NAVIGATOR_SOCIAL_OAUTH_REDIRECT_URI" if "NAVIGATOR_X_CLIENT_ID"),
    required!(integration "NAVIGATOR_RUNTIME_KMS_KEY" if "NAVIGATOR_X_CLIENT_ID"),
    required!(integration "NAVIGATOR_TIKTOK_BUSINESS_AUTHORIZATION_URL" if "NAVIGATOR_TIKTOK_BUSINESS_APP_ID"),
    required!(integration "NAVIGATOR_TIKTOK_BUSINESS_APP_ID" if "NAVIGATOR_TIKTOK_BUSINESS_AUTHORIZATION_URL"),
    required!(integration "NAVIGATOR_TIKTOK_BUSINESS_APP_SECRET" if "NAVIGATOR_TIKTOK_BUSINESS_APP_ID"),
    required!(integration "NAVIGATOR_TIKTOK_BUSINESS_APP_ID" if "NAVIGATOR_TIKTOK_BUSINESS_APP_SECRET"),
    required!(integration "NAVIGATOR_SOCIAL_OAUTH_REDIRECT_URI" if "NAVIGATOR_TIKTOK_BUSINESS_APP_ID"),
    required!(integration "NAVIGATOR_RUNTIME_KMS_KEY" if "NAVIGATOR_TIKTOK_BUSINESS_APP_ID"),
    required!(integration "NAVIGATOR_LINKEDIN_CLIENT_SECRET" if "NAVIGATOR_LINKEDIN_CLIENT_ID"),
    required!(integration "NAVIGATOR_LINKEDIN_CLIENT_ID" if "NAVIGATOR_LINKEDIN_CLIENT_SECRET"),
    required!(integration "NAVIGATOR_SOCIAL_OAUTH_REDIRECT_URI" if "NAVIGATOR_LINKEDIN_CLIENT_ID"),
    required!(integration "NAVIGATOR_RUNTIME_KMS_KEY" if "NAVIGATOR_LINKEDIN_CLIENT_ID"),
    required!(integration "NAVIGATOR_THREADS_APP_SECRET" if "NAVIGATOR_THREADS_APP_ID"),
    required!(integration "NAVIGATOR_THREADS_APP_ID" if "NAVIGATOR_THREADS_APP_SECRET"),
    required!(integration "NAVIGATOR_SOCIAL_OAUTH_REDIRECT_URI" if "NAVIGATOR_THREADS_APP_ID"),
    required!(integration "NAVIGATOR_RUNTIME_KMS_KEY" if "NAVIGATOR_THREADS_APP_ID"),
    required!("SESSION_SECRET"),
    Requirement {
        any_of: &[&["OIDC_AUDIENCE"]],
        trigger: Some("OIDC_JWKS_URL"),
        integration: false,
        scope: RequirementScope::EveryDeployment,
    },
    Requirement {
        any_of: &[&["OIDC_ISSUER"]],
        trigger: Some("OIDC_JWKS_URL"),
        integration: false,
        scope: RequirementScope::EveryDeployment,
    },
    // The OAuth client allowlist Gemini Enterprise's tokens are validated
    // against. Unset, `portal::google_oauth` degrades to a pass-through, so
    // `portal::mcp_principal` never injects a `Principal` — and every Navigator MCP tool
    // that scopes on the authenticated actor (`create_notation`'s
    // project ACL) silently stops checking. Integration-tier: the explicit
    // staging CI harness has no real Google tenant to validate against.
    required!(integration "GOOGLE_OAUTH_CLIENT_IDS"),
    // Sign in with Microsoft, declared by `OAUTH_MICROSOFT_CLIENT_ID` and
    // otherwise absent — the same shape as DocuSign above.
    // `portal::oauth::Provider::microsoft_from_env` reads an unset client id
    // as "no second provider" (`Ok(None)`), so a deployment that has not
    // registered an Entra app supplies neither key. But once the id is set,
    // the client secret is not optional: `microsoft_from_env` returns
    // `OAuthSetupError::Missing` without it, crash-looping the pod on every
    // boot. `OAUTH_MICROSOFT_CLIENT_ID` itself rides inline Deployment env
    // (`ship::INLINE_ENV_WEB_KEYS`), not the Secret rail, so it is not a
    // requirement in its own right here — only the trigger for its secret.
    required!(integration "OAUTH_MICROSOFT_CLIENT_SECRET" if "OAUTH_MICROSOFT_CLIENT_ID"),
    // Sign in with Apple follows the same optional-provider shape. The
    // Services ID is inline; once it is declared, boot needs all three
    // signing values from the Secret rail.
    required!(integration "OAUTH_APPLE_TEAM_ID" if "OAUTH_APPLE_CLIENT_ID"),
    required!(integration "OAUTH_APPLE_KEY_ID" if "OAUTH_APPLE_CLIENT_ID"),
    required!(integration "OAUTH_APPLE_PRIVATE_KEY" if "OAUTH_APPLE_CLIENT_ID"),
    // hCaptcha on the public lead forms: the sitekey is public and rides
    // inline env; once a deployment names one, `portal::hcaptcha` refuses to
    // boot without the secret that redeems a solved challenge.
    required!(integration "HCAPTCHA_SECRET" if "HCAPTCHA_SITEKEY"),
    // Email/password sign-in through Identity Platform. The browser key is
    // the whole declaration: `portal::oauth::IdentityPasswordConfig::from_env`
    // reads an absent key as "SSO only", and there is no public coordinate
    // to trigger on, so the key is its own trigger. A row without it declines
    // the door and `ops ship` renders its Secret Manager reference out.
    required!(integration "NAVIGATOR_IDENTITY_PLATFORM_API_KEY" if "NAVIGATOR_IDENTITY_PLATFORM_API_KEY"),
    // The social dashboard's leads by platform read Plausible's Stats API.
    // `webapp::lead_sources` reads an absent key as "not connected", so the
    // key is its own trigger and a row without it declines the section.
    required!(integration "PLAUSIBLE_STATS_API_KEY" if "PLAUSIBLE_STATS_API_KEY"),
    // DriDigest on DBOS posts through this incoming webhook. A row that does
    // not host it declines the key, so it is its own trigger; `ops ship`
    // demands it when `NAVIGATOR_DBOS_DRI_DIGEST_ENABLED` is on.
    required!(integration "SLACK_WEBHOOK_URL" if "SLACK_WEBHOOK_URL"),
    // Twilio SMS intake: the number rides inline env, and once a deployment
    // names one, `portal::twilio_sms` refuses to boot without the token that
    // authenticates each text.
    required!(integration "TWILIO_AUTH_TOKEN" if "NAVIGATOR_SMS_NUMBER"),
    // Texting a person first or replying from the admin inbox calls Twilio's
    // Messages API as the account, so the same number also demands the SID.
    required!(integration "TWILIO_ACCOUNT_SID" if "NAVIGATOR_SMS_NUMBER"),
    // Simulated matters refuse every text, so staging texts only the personal
    // numbers listed here. They are people's numbers, so they ride the Secret
    // rail, and production (real matters, no allowlist) is never asked for them.
    Requirement {
        any_of: &[&["NAVIGATOR_SMS_TEST_RECIPIENTS"]],
        trigger: Some("NAVIGATOR_SMS_NUMBER"),
        integration: true,
        scope: RequirementScope::SmsTestRecipientsHome,
    },
    // The SurrealDB coordinates. `web` fails closed on a missing endpoint —
    // `portal::hosting` calls `store::surreal::connect_from_env` with no
    // fallback, and the person directory lives in that engine, so a
    // deployment without one does not degrade: it cannot authenticate anyone.
    //
    // These are coordinates, not key material, so each deployment supplies
    // them from its plaintext `config.toml` rather than the Secret rail. They
    // are listed here so `ops ship` refuses a deployment that is missing one,
    // instead of the absence surfacing as a crash-looping pod — which is what
    // happened while `web` required the handle and this list did not mention
    // it. The root credentials are a separate entry and land with the database
    // user (ENG-18).
    required!("NAVIGATOR_SURREAL_ENDPOINT"),
    required!("NAVIGATOR_SURREAL_NAMESPACE"),
    required!("NAVIGATOR_SURREAL_DATABASE"),
    // The sign-in credentials. A managed engine rejects an anonymous
    // connection, so these are as fatal as the endpoint — and the
    // failure is worse, because `connect` succeeds and the first query
    // is what fails. Required as a pair: half a login is not a login.
    // `NAVIGATOR_SURREAL_AUTH_SCOPE` is deliberately absent — it
    // defaults to `root`, and a deployment naming a different scope is
    // choosing, not satisfying an invariant.
    Requirement {
        any_of: &[&["NAVIGATOR_SURREAL_USER", "NAVIGATOR_SURREAL_PASSWORD"]],
        trigger: None,
        integration: false,
        scope: RequirementScope::EveryDeployment,
    },
];

#[must_use]
pub fn ci_harness_enabled<F: Fn(&str) -> Option<String>>(get: &F) -> bool {
    get(CI_HARNESS).as_deref() == Some("1")
}

/// Whether `NAVIGATOR_CI_HARNESS=1` may relax anything for this profile.
///
/// The flag authorizes in-process fakes for the automated dev harness and
/// nothing beyond it. `k8s/base/web/web.yaml` only ever pairs it with
/// `NAVIGATOR_ENVIRONMENT=dev`, and `docs/third-party-integrations.md` states
/// that production rejects it outright.
///
/// The scoping lives here, in one predicate, rather than at each call site.
/// A caller that tests the raw flag instead grants production the same
/// relaxation the dev harness gets — for a credential-shaped check that means
/// a deployment keeps booting green while pointed at a provider's test
/// environment, which is exactly what these checks exist to prevent.
#[must_use]
pub fn harness_relaxations_apply<F: Fn(&str) -> Option<String>>(
    environment: DeploymentEnvironment,
    get: &F,
) -> bool {
    environment == DeploymentEnvironment::Dev && ci_harness_enabled(get)
}

pub fn applicable_web_requirements<F: Fn(&str) -> Option<String>>(
    environment: DeploymentEnvironment,
    get: &F,
) -> anyhow::Result<Vec<Requirement>> {
    let harness = harness_relaxations_apply(environment, get);
    let capabilities = DeploymentCapabilities::from_lookup(get)?;
    Ok(WEB_REQUIREMENTS
        .iter()
        .filter(|requirement| !harness || !requirement.integration)
        .filter(|requirement| requirement.scope.applies(capabilities))
        .filter(|requirement| {
            requirement
                .trigger
                .is_none_or(|key| get(key).is_some_and(|value| !value.is_empty()))
        })
        .copied()
        .collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_malformed_capability_flag_names_the_value_and_the_fix() {
        let get = |key: &str| (key == super::GITHUB_AUTOMATION_HOME_FLAG).then(|| "yes".to_owned());
        let error = super::DeploymentCapabilities::from_lookup(&get)
            .unwrap_err()
            .to_string();
        assert!(error.contains("NAVIGATOR_GITHUB_AUTOMATION_HOME is `yes`"));
        assert!(error.contains("    NAVIGATOR_GITHUB_AUTOMATION_HOME = \"false\"\n"));
        assert!(error.contains("    NAVIGATOR_SMS_TEST_RECIPIENTS_DEPLOYMENT = \"false\"\n"));
    }

    use super::{applicable_web_requirements, DeploymentEnvironment};

    /// Every key any alternative of any applicable requirement names.
    fn demanded(pairs: &[(&str, &str)]) -> Vec<String> {
        let get = |key: &str| -> Option<String> {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, value)| (*value).to_owned())
        };
        applicable_web_requirements(DeploymentEnvironment::Production, &get)
            .expect("synthetic capability flags are valid")
            .iter()
            .flat_map(|requirement| requirement.any_of.iter().copied())
            .flatten()
            .map(|key| (*key).to_owned())
            .collect()
    }

    const DOCUSIGN_KEYS: &[&str] = &[
        "DOCUSIGN_ACCOUNT_ID",
        "DOCUSIGN_INTEGRATION_KEY",
        "DOCUSIGN_USER_ID",
        "DOCUSIGN_PRIVATE_KEY",
        "DOCUSIGN_ACCESS_TOKEN",
        "DOCUSIGN_HMAC_KEY",
        "DOCUSIGN_WEBHOOK_SECRET",
    ];

    /// A deployment that signs nothing must be able to say so. Without this,
    /// `ops ship` demands DocuSign credentials, and the placeholder an
    /// operator would reach for is worse than the absence:
    /// `DocuSignSignatureProvider::from_env` returns `Some` for any non-empty
    /// value, so the *real* provider boots holding a fake credential and the
    /// first signature request fails at runtime. `StubSignatureProvider` only
    /// engages when DocuSign is genuinely unset.
    #[test]
    fn a_deployment_that_declares_no_docusign_is_asked_for_none() {
        let demanded = demanded(&[("NAVIGATOR_GCP_PROJECT_ID", "example-project")]);
        for key in DOCUSIGN_KEYS {
            assert!(
                !demanded.contains(&(*key).to_owned()),
                "{key} must not be required of a deployment that declares no DOCUSIGN_BASE_URL"
            );
        }
    }

    /// The other half, and the reason this is a trigger rather than a
    /// deletion: declaring DocuSign still demands everything the real
    /// provider needs. A half-configured integration stays a hard failure.
    #[test]
    fn declaring_docusign_demands_every_key_the_real_provider_reads() {
        let demanded = demanded(&[
            ("NAVIGATOR_GCP_PROJECT_ID", "neon-law"),
            ("DOCUSIGN_BASE_URL", "https://na4.docusign.net/restapi"),
        ]);
        for key in DOCUSIGN_KEYS {
            assert!(
                demanded.contains(&(*key).to_owned()),
                "{key} must be required once a deployment declares DOCUSIGN_BASE_URL"
            );
        }
    }

    /// A deployment that has not registered an Entra app must be able to say
    /// so without being asked for a client secret it has no reason to hold.
    #[test]
    fn a_deployment_that_declares_no_microsoft_oauth_is_asked_for_none() {
        let demanded = demanded(&[("NAVIGATOR_GCP_PROJECT_ID", "example-project")]);
        assert!(
            !demanded.contains(&"OAUTH_MICROSOFT_CLIENT_SECRET".to_owned()),
            "OAUTH_MICROSOFT_CLIENT_SECRET must not be required of a deployment that declares no \
             OAUTH_MICROSOFT_CLIENT_ID"
        );
    }

    /// The other half, and the reason this is a trigger rather than a plain
    /// requirement: once a deployment sets `OAUTH_MICROSOFT_CLIENT_ID`,
    /// `portal::oauth::Provider::microsoft_from_env` fails without the
    /// matching secret — the staging crash loop this guards, where the
    /// SecretProviderClass projected the id's inline env but never the
    /// secret.
    #[test]
    fn declaring_microsoft_oauth_demands_the_client_secret() {
        let demanded = demanded(&[
            ("NAVIGATOR_GCP_PROJECT_ID", "example-project"),
            ("OAUTH_MICROSOFT_CLIENT_ID", "some-entra-app-id"),
        ]);
        assert!(
            demanded.contains(&"OAUTH_MICROSOFT_CLIENT_SECRET".to_owned()),
            "OAUTH_MICROSOFT_CLIENT_SECRET must be required once a deployment declares \
             OAUTH_MICROSOFT_CLIENT_ID"
        );
    }

    #[test]
    fn naming_an_sms_number_demands_the_twilio_token() {
        let quiet = demanded(&[("NAVIGATOR_GCP_PROJECT_ID", "example-project")]);
        assert!(!quiet.contains(&"TWILIO_AUTH_TOKEN".to_owned()));
        let texting = demanded(&[
            ("NAVIGATOR_GCP_PROJECT_ID", "example-project"),
            ("NAVIGATOR_SMS_NUMBER", "+15550100000"),
        ]);
        assert!(texting.contains(&"TWILIO_AUTH_TOKEN".to_owned()));
        assert!(!quiet.contains(&"TWILIO_ACCOUNT_SID".to_owned()));
        assert!(texting.contains(&"TWILIO_ACCOUNT_SID".to_owned()));
    }

    #[test]
    fn only_a_marked_deployment_with_a_number_is_asked_for_sms_test_recipients() {
        let texting = [("NAVIGATOR_SMS_NUMBER", "+15550100000")];
        let marked = demanded(&[(super::SMS_TEST_RECIPIENTS_FLAG, "true"), texting[0]]);
        assert!(marked.contains(&"NAVIGATOR_SMS_TEST_RECIPIENTS".to_owned()));
        let ordinary = demanded(&[("NAVIGATOR_GCP_PROJECT_ID", "example-project"), texting[0]]);
        assert!(!ordinary.contains(&"NAVIGATOR_SMS_TEST_RECIPIENTS".to_owned()));
        let quiet = demanded(&[(super::SMS_TEST_RECIPIENTS_FLAG, "true")]);
        assert!(!quiet.contains(&"NAVIGATOR_SMS_TEST_RECIPIENTS".to_owned()));
    }

    #[test]
    fn only_a_marked_deployment_is_asked_for_webhook_credentials() {
        let marked = demanded(&[(super::GITHUB_AUTOMATION_HOME_FLAG, "true")]);
        assert!(marked.contains(&"RESTATE_INGRESS_URL".to_owned()));
        assert!(marked.contains(&"RESTATE_AUTH_TOKEN".to_owned()));
        let ordinary = demanded(&[("NAVIGATOR_GCP_PROJECT_ID", "example-project")]);
        assert!(!ordinary.contains(&"RESTATE_INGRESS_URL".to_owned()));
    }

    #[test]
    fn capability_flags_must_be_exact_booleans() {
        let get = |key: &str| (key == super::GITHUB_AUTOMATION_HOME_FLAG).then(|| "yes".to_owned());
        let error = applicable_web_requirements(DeploymentEnvironment::Production, &get)
            .expect_err("invalid capability flags must fail closed");
        assert!(error
            .to_string()
            .contains(super::GITHUB_AUTOMATION_HOME_FLAG));
    }

    #[test]
    fn explicit_false_capability_flags_keep_requirements_off() {
        let demanded = demanded(&[
            (super::GITHUB_AUTOMATION_HOME_FLAG, "false"),
            (super::SMS_TEST_RECIPIENTS_FLAG, "false"),
        ]);
        assert!(!demanded.contains(&"RESTATE_INGRESS_URL".to_owned()));
        assert!(!demanded.contains(&"NAVIGATOR_SMS_TEST_RECIPIENTS".to_owned()));
    }

    #[test]
    fn a_deployment_that_declares_no_apple_oauth_is_asked_for_none() {
        let demanded = demanded(&[("NAVIGATOR_GCP_PROJECT_ID", "example-project")]);
        for key in [
            "OAUTH_APPLE_TEAM_ID",
            "OAUTH_APPLE_KEY_ID",
            "OAUTH_APPLE_PRIVATE_KEY",
        ] {
            assert!(
                !demanded.contains(&key.to_owned()),
                "{key} must not be required of a deployment that declares no OAUTH_APPLE_CLIENT_ID"
            );
        }
    }

    #[test]
    fn declaring_apple_oauth_demands_every_signing_value() {
        let demanded = demanded(&[
            ("NAVIGATOR_GCP_PROJECT_ID", "example-project"),
            ("OAUTH_APPLE_CLIENT_ID", "com.example.navigator"),
        ]);
        for key in [
            "OAUTH_APPLE_TEAM_ID",
            "OAUTH_APPLE_KEY_ID",
            "OAUTH_APPLE_PRIVATE_KEY",
        ] {
            assert!(
                demanded.contains(&key.to_owned()),
                "{key} must be required once OAUTH_APPLE_CLIENT_ID is declared"
            );
        }
    }

    #[test]
    fn a_deployment_that_declares_no_dash0_is_asked_for_none() {
        let demanded = demanded(&[("NAVIGATOR_GCP_PROJECT_ID", "example-project")]);
        for key in ["DASH0_DATASET", "DASH0_TOKEN"] {
            assert!(
                !demanded.contains(&key.to_owned()),
                "{key} must not be required of a deployment that declares no DASH0_ENDPOINT"
            );
        }
    }

    #[test]
    fn a_deployment_that_declares_no_firm_integrations_is_asked_for_none() {
        let demanded = demanded(&[("NAVIGATOR_GCP_PROJECT_ID", "example-project")]);
        for key in ["NAVIGATOR_RUNTIME_KMS_KEY", "NAVIGATOR_NOTION_DATABASE_ID"] {
            assert!(
                !demanded.contains(&key.to_owned()),
                "{key} must not be required of a deployment that declares neither coordinate"
            );
        }
    }

    #[test]
    fn a_runtime_kms_key_alone_demands_no_notion_database() {
        let demanded = demanded(&[
            ("NAVIGATOR_GCP_PROJECT_ID", "example-project"),
            ("NAVIGATOR_RUNTIME_KMS_KEY", "set"),
        ]);
        assert!(
            !demanded.contains(&"NAVIGATOR_NOTION_DATABASE_ID".to_owned()),
            "runtime KMS seals social and Firm credentials without any Notion database"
        );
    }

    #[test]
    fn a_notion_database_demands_the_runtime_kms_key() {
        let demanded = demanded(&[
            ("NAVIGATOR_GCP_PROJECT_ID", "example-project"),
            ("NAVIGATOR_NOTION_DATABASE_ID", "set"),
        ]);
        assert!(
            demanded.contains(&"NAVIGATOR_RUNTIME_KMS_KEY".to_owned()),
            "the runtime KMS key is required once the Notion database id is set"
        );
    }

    const META_KEYS: &[&str] = &[
        "NAVIGATOR_META_APP_SECRET",
        "NAVIGATOR_META_WEBHOOK_VERIFY_TOKEN",
        "NAVIGATOR_META_WEBHOOK_APP_SECRET",
        "NAVIGATOR_SOCIAL_OAUTH_REDIRECT_URI",
        "NAVIGATOR_RUNTIME_KMS_KEY",
    ];

    const THREADS_KEYS: &[&str] = &[
        "NAVIGATOR_THREADS_APP_SECRET",
        "NAVIGATOR_SOCIAL_OAUTH_REDIRECT_URI",
        "NAVIGATOR_RUNTIME_KMS_KEY",
    ];

    #[test]
    fn a_deployment_that_declares_no_social_app_is_asked_for_none() {
        let demanded = demanded(&[("NAVIGATOR_GCP_PROJECT_ID", "example-project")]);
        for key in META_KEYS
            .iter()
            .chain(THREADS_KEYS)
            .chain(&["NAVIGATOR_META_APP_ID", "NAVIGATOR_THREADS_APP_ID"])
        {
            assert!(
                !demanded.contains(&(*key).to_owned()),
                "{key} must not be required of a deployment that declares no social app"
            );
        }
    }

    #[test]
    fn declaring_the_meta_app_demands_its_secret_webhook_and_runtime_kms() {
        let demanded = demanded(&[
            ("NAVIGATOR_GCP_PROJECT_ID", "example-project"),
            ("NAVIGATOR_META_APP_ID", "1000000000000001"),
        ]);
        for key in META_KEYS {
            assert!(
                demanded.contains(&(*key).to_owned()),
                "{key} must be required once NAVIGATOR_META_APP_ID is declared"
            );
        }
        assert!(!demanded.contains(&"NAVIGATOR_THREADS_APP_SECRET".to_owned()));
    }

    #[test]
    fn declaring_the_threads_app_demands_its_secret_and_runtime_kms() {
        let demanded = demanded(&[
            ("NAVIGATOR_GCP_PROJECT_ID", "example-project"),
            ("NAVIGATOR_THREADS_APP_ID", "2000000000000002"),
        ]);
        for key in THREADS_KEYS {
            assert!(
                demanded.contains(&(*key).to_owned()),
                "{key} must be required once NAVIGATOR_THREADS_APP_ID is declared"
            );
        }
        assert!(!demanded.contains(&"NAVIGATOR_META_APP_SECRET".to_owned()));
    }

    #[test]
    fn a_social_app_secret_without_its_app_id_is_half_configured() {
        for (secret, id) in [
            ("NAVIGATOR_META_APP_SECRET", "NAVIGATOR_META_APP_ID"),
            ("NAVIGATOR_THREADS_APP_SECRET", "NAVIGATOR_THREADS_APP_ID"),
        ] {
            let demanded = demanded(&[
                ("NAVIGATOR_GCP_PROJECT_ID", "example-project"),
                (secret, "set"),
            ]);
            assert!(
                demanded.contains(&id.to_owned()),
                "{id} must be required once {secret} is supplied"
            );
        }
    }

    #[test]
    fn declaring_dash0_demands_the_dataset_and_token() {
        let demanded = demanded(&[
            ("NAVIGATOR_GCP_PROJECT_ID", "example-project"),
            ("DASH0_ENDPOINT", "https://dash0.example"),
        ]);
        for key in ["DASH0_DATASET", "DASH0_TOKEN"] {
            assert!(
                demanded.contains(&key.to_owned()),
                "{key} must be required once DASH0_ENDPOINT is declared"
            );
        }
    }
}
