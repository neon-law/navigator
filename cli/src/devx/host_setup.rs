//! `navigator ops host setup`: bring one registered brand host online.
//!
//! A host is registered in code first (`views::brand::BrandKey::hosts`). This
//! command then does the rest for one deployment:
//!
//! 1. **DNS** — an `A` record for the host's label in its zone, pointed at the
//!    deployment's gateway, through the configured DNS provider. Preview
//!    unless `--apply`.
//! 2. **Certificate** — every registered host is on the deployment's Firm
//!    `ManagedCertificate` and Ingress, which `ops ship` renders from the same
//!    registry. This checks the live TLS handshake and says when a ship or the
//!    certificate's issuance is still outstanding.
//! 3. **Sign-in** — a brand host signs in through the deployment's one
//!    registered callback and is handed back for its own session
//!    (`portal::host_handoff`), so no identity provider needs a new redirect
//!    URI. The host serves its own branded sign-in page; this checks that
//!    its provider path, `/auth/login/oidc`, detours correctly.

use std::net::ToSocketAddrs;

use anyhow::{bail, Context, Result};
use views::brand::{is_staging_host, registered_brand_key, BrandKey};

use super::brand_readiness::{self, CommandRunner, ProcessCommandRunner, Verdict};
use super::dns;
use super::ship::ShipConfig;
use super::DnsSetupProvider;

/// Where a host's `A` record lives: its DNS zone and the label inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostPlan {
    pub key: BrandKey,
    pub host: &'static str,
    pub zone: String,
    pub label: String,
}

/// Resolve `host` against the compiled registry and the deployment's
/// environment. Refuses an unregistered host, and a staging host on a
/// production deployment (or the reverse).
pub(crate) fn plan(host: &str, deployment_public_host: &str) -> Result<HostPlan> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let Some(key) = registered_brand_key(&host) else {
        bail!(
            "{host} is not a registered brand host; add it to `views::brand::BrandKey::hosts` \
             and ship that release first"
        );
    };
    let registered = key
        .hosts()
        .iter()
        .copied()
        .find(|candidate| candidate.eq_ignore_ascii_case(&host))
        .expect("registered_brand_key matched one of this key's hosts");
    if is_staging_host(registered) != is_staging_host(deployment_public_host) {
        bail!(
            "{registered} belongs to the {} environment, but this deployment serves \
             {deployment_public_host}",
            if is_staging_host(registered) {
                "staging"
            } else {
                "production"
            }
        );
    }
    let zone = key
        .apex()
        .map_or_else(|| registrable_domain(registered), str::to_owned);
    let label = registered
        .strip_suffix(&zone)
        .and_then(|rest| rest.strip_suffix('.'))
        .map_or_else(|| "@".to_owned(), str::to_owned);
    Ok(HostPlan {
        key,
        host: registered,
        zone,
        label,
    })
}

/// The last two labels of `host`: the zone a `neonlaw.com` subdomain brand's
/// record lives in.
fn registrable_domain(host: &str) -> String {
    let labels: Vec<&str> = host.split('.').collect();
    labels[labels.len().saturating_sub(2)..].join(".")
}

/// The brand host path that detours to the sign-in origin: the primary
/// provider's redirect. `/auth/login` itself renders the brand's own page.
const SIGN_IN_PROBE_PATH: &str = "/auth/login/oidc";

/// Whether a `curl -D -` of `https://<host>/auth/login/oidc` shows the
/// per-host sign-in detour: a 303 to another origin's same path that names
/// this host as `return_host`.
pub(crate) fn sign_in_detours(headers: &str, host: &str) -> Result<String, String> {
    let status = headers
        .lines()
        .find(|line| line.starts_with("HTTP/"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok());
    let location = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("location")
            .then(|| value.trim().to_owned())
    });
    let Some(location) = location else {
        return Err(format!(
            "expected a 303 to the sign-in origin, got status {status:?}"
        ));
    };
    let Ok(url) = url::Url::parse(&location) else {
        return Err(format!("unparseable redirect {location:?}"));
    };
    let names_host = url
        .query_pairs()
        .any(|(name, value)| name == "return_host" && value.eq_ignore_ascii_case(host));
    if status == Some(303) && url.path() == SIGN_IN_PROBE_PATH && names_host {
        Ok(format!(
            "{SIGN_IN_PROBE_PATH} detours to {} and returns here",
            url.origin().ascii_serialization()
        ))
    } else {
        Err(format!(
            "expected a 303 to the sign-in origin naming {host}, got status {status:?} to {location}"
        ))
    }
}

/// The deployment gateway's IPv4 address: `--gateway-ip` when given, else the
/// address the deployment's own public host resolves to.
fn gateway_ip(explicit: Option<String>, public_host: &str) -> Result<String> {
    if let Some(ip) = explicit.filter(|ip| !ip.trim().is_empty()) {
        return Ok(ip);
    }
    (public_host, 443)
        .to_socket_addrs()
        .with_context(|| format!("resolve {public_host} for its gateway address"))?
        .find(std::net::SocketAddr::is_ipv4)
        .map(|addr| addr.ip().to_string())
        .with_context(|| format!("{public_host} has no IPv4 address; pass --gateway-ip"))
}

/// `navigator ops host setup`.
pub(crate) fn run(
    cfg: &ShipConfig,
    host: &str,
    explicit_gateway_ip: Option<String>,
    apply: bool,
    timeout_secs: u32,
) -> Result<()> {
    let provider = dns::DnsimpleProvider::from_env()?;
    run_with(
        &provider,
        &mut ProcessCommandRunner,
        cfg,
        host,
        explicit_gateway_ip,
        apply,
        timeout_secs,
    )
}

/// [`run`] with the DNS provider and the process runner injected, so the
/// whole sequence is exercised without `DNSimple` or a network.
fn run_with<R: CommandRunner>(
    provider: &dyn DnsSetupProvider,
    runner: &mut R,
    cfg: &ShipConfig,
    host: &str,
    explicit_gateway_ip: Option<String>,
    apply: bool,
    timeout_secs: u32,
) -> Result<()> {
    let plan = plan(host, &cfg.public_host)?;
    eprintln!(
        "{} → {} ({}), zone {} label {}",
        plan.host,
        plan.key.as_str(),
        cfg.name,
        plan.zone,
        plan.label
    );

    // 1. DNS.
    let ip = gateway_ip(explicit_gateway_ip, &cfg.public_host)?;
    let config = dns::DnsSetupConfig {
        gateway_ip: Some(ip),
        hosts: vec![plan.label.clone()],
        ..dns::DnsSetupConfig::default()
    };
    let desired = dns::desired_records(&plan.zone, &config);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?
        .block_on(super::dns_setup_zone(
            provider, &plan.zone, &config, &desired, apply,
        ))?;

    // 2. Certificate and 3. sign-in, both read-only.
    let branding = plan.key.resolve_branding(&views::brand::DEFAULT_BRANDING);
    let tls = brand_readiness::check_host(
        runner,
        plan.key,
        plan.host,
        branding.firm.site_name,
        timeout_secs,
    );
    let mut ready = tls.verdict == Verdict::Ready;
    if ready {
        eprintln!("READY certificate: {}", tls.detail);
    } else {
        eprintln!(
            "NOT READY certificate ({:?}): {}\n  The Firm ManagedCertificate and Ingress carry every \
             registered host. Run `navigator ops ship --deployment {}` if this host's release has \
             not shipped, then allow about 15 minutes after DNS resolves for issuance.",
            tls.verdict, tls.detail, cfg.name
        );
    }
    let login = runner.run(
        "curl",
        &[
            "-sS".into(),
            "--max-time".into(),
            timeout_secs.to_string(),
            "-D".into(),
            "-".into(),
            "-o".into(),
            "/dev/null".into(),
            format!("https://{}{SIGN_IN_PROBE_PATH}", plan.host),
        ],
    );
    match sign_in_detours(&login.stdout, plan.host) {
        Ok(detail) => eprintln!("READY sign-in: {detail}"),
        Err(detail) => {
            ready = false;
            eprintln!("NOT READY sign-in: {detail}");
        }
    }
    if !apply {
        eprintln!("PREVIEW: pass --apply to write the DNS record");
    }
    if ready {
        Ok(())
    } else {
        bail!(
            "{} is not ready yet; rerun this command after the steps above",
            plan.host
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::super::brand_readiness::{CommandOutcome, CommandRunner};
    use super::super::dns;
    use super::super::ship::ShipConfig;
    use super::{gateway_ip, plan, run_with, sign_in_detours};
    use views::brand::BrandKey;

    /// A DNS zone holding `existing`, recording every write it is asked for.
    #[derive(Default)]
    struct FakeZone {
        existing: Vec<dns::ExistingRecord>,
        writes: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl dns::DnsProvider for FakeZone {
        async fn list_records(
            &self,
            _zone: &str,
        ) -> std::result::Result<Vec<dns::ExistingRecord>, dns::DnsError> {
            Ok(self.existing.clone())
        }

        async fn create_record(
            &self,
            zone: &str,
            record: &dns::DesiredRecord,
        ) -> std::result::Result<u64, dns::DnsError> {
            self.writes.lock().expect("writes lock").push(format!(
                "create:{zone}:{}:{}:{}",
                record.record_type.as_str(),
                record.name,
                record.content
            ));
            Ok(1)
        }

        async fn update_record(
            &self,
            zone: &str,
            record_id: u64,
            record: &dns::DesiredRecord,
        ) -> std::result::Result<(), dns::DnsError> {
            self.writes.lock().expect("writes lock").push(format!(
                "update:{zone}:{record_id}:{}:{}",
                record.name, record.content
            ));
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl dns::CertificateProvider for FakeZone {
        async fn latest_certificate(
            &self,
            _zone: &str,
        ) -> std::result::Result<Option<dns::ApexCertificate>, dns::DnsError> {
            unreachable!("host setup never reconciles an apex certificate")
        }

        async fn order_letsencrypt(
            &self,
            _zone: &str,
            _auto_renew: bool,
        ) -> std::result::Result<dns::ApexCertificate, dns::DnsError> {
            unreachable!("host setup never orders an apex certificate")
        }

        async fn issue_letsencrypt(
            &self,
            _zone: &str,
            _certificate_id: u64,
        ) -> std::result::Result<dns::ApexCertificateState, dns::DnsError> {
            unreachable!("host setup never issues an apex certificate")
        }
    }

    /// Answers the TLS check, then the sign-in check, in that order.
    struct Scripted {
        outcomes: std::collections::VecDeque<CommandOutcome>,
        urls: Vec<String>,
    }

    impl Scripted {
        fn new(outcomes: Vec<CommandOutcome>) -> Self {
            Self {
                outcomes: outcomes.into(),
                urls: Vec::new(),
            }
        }
    }

    impl CommandRunner for Scripted {
        fn run(&mut self, program: &str, args: &[String]) -> CommandOutcome {
            assert_eq!(program, "curl");
            self.urls
                .push(args.last().cloned().expect("curl is given a URL"));
            self.outcomes
                .pop_front()
                .expect("more curl calls than this test scripted")
        }
    }

    fn answered(stdout: &str) -> CommandOutcome {
        CommandOutcome {
            exit_code: Some(0),
            stdout: stdout.to_owned(),
            stderr: String::new(),
        }
    }

    fn branded_home(site_name: &str) -> CommandOutcome {
        answered(&format!(
            "HTTP/2 200 \r\ncontent-type: text/html\r\n\r\n\
             <meta property=\"og:site_name\" content=\"{site_name}\">"
        ))
    }

    fn detour(host: &str) -> CommandOutcome {
        answered(&format!(
            "HTTP/2 303 \r\nlocation: https://www.neonlaw.com/auth/login/oidc?return_host={host}&handoff_nonce=n\r\n"
        ))
    }

    fn deployment(public_host: &str) -> ShipConfig {
        ShipConfig {
            name: "test-deployment".into(),
            environment: store::DeploymentEnvironment::Dev,
            capabilities: store::deployment::DeploymentCapabilities::default(),
            project_id: "test-project".into(),
            location: "example-region".into(),
            cluster: "navigator".into(),
            registry: "ghcr.io/example/navigator".into(),
            namespace: "navigator".into(),
            web_image_name: "neon-server".into(),
            public_host: public_host.to_string(),
            asset_base_url: format!("https://{public_host}/assets"),
            google_service_account_id: "navigator-web".into(),
            primary_domain: "neonlaw.com".into(),
            secret_name: "navigator-web-secrets".into(),
            workflows_url: None,
            restate_admin_url: None,
            context: "gke_test-project_example-region_navigator".into(),
        }
    }

    #[test]
    fn apply_writes_the_label_record_and_succeeds_once_tls_and_sign_in_are_ready() {
        let zone = FakeZone::default();
        let mut runner = Scripted::new(vec![
            branded_home("IEP Defenders"),
            detour("iep.neonlaw.com"),
        ]);
        run_with(
            &zone,
            &mut runner,
            &deployment("www.neonlaw.com"),
            "iep.neonlaw.com",
            Some("192.0.2.10".into()),
            true,
            5,
        )
        .expect("a host whose TLS and sign-in both pass is ready");
        assert_eq!(
            *zone.writes.lock().unwrap(),
            vec!["create:neonlaw.com:A:iep:192.0.2.10".to_owned()]
        );
        assert_eq!(
            runner.urls,
            vec![
                "https://iep.neonlaw.com/".to_owned(),
                "https://iep.neonlaw.com/auth/login/oidc".to_owned()
            ]
        );
    }

    #[test]
    fn preview_writes_nothing_and_an_unissued_certificate_is_not_ready() {
        let zone = FakeZone::default();
        let mut runner = Scripted::new(vec![
            CommandOutcome {
                exit_code: Some(60),
                stdout: String::new(),
                stderr: "SSL: no alternative certificate subject name matches".into(),
            },
            detour("staging-iep.neonlaw.com"),
        ]);
        let err = run_with(
            &zone,
            &mut runner,
            &deployment("staging.neonlaw.com"),
            "staging-iep.neonlaw.com",
            Some("192.0.2.20".into()),
            false,
            5,
        )
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("staging-iep.neonlaw.com is not ready yet"));
        assert!(
            zone.writes.lock().unwrap().is_empty(),
            "a preview never writes to the zone"
        );
    }

    #[test]
    fn a_sign_in_without_the_detour_is_not_ready_even_with_a_valid_certificate() {
        let zone = FakeZone {
            existing: vec![dns::ExistingRecord {
                id: 9,
                record_type: dns::RecordType::A,
                name: "lex-tecnica".into(),
                content: "192.0.2.30".into(),
                priority: None,
            }],
            ..FakeZone::default()
        };
        let mut runner = Scripted::new(vec![
            branded_home("Lex Tecnica"),
            answered("HTTP/2 303 \r\nlocation: https://lex-tecnica.neonlaw.com/app\r\n"),
        ]);
        let err = run_with(
            &zone,
            &mut runner,
            &deployment("www.neonlaw.com"),
            "lex-tecnica.neonlaw.com",
            Some("192.0.2.30".into()),
            true,
            5,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not ready yet"));
        assert!(
            zone.writes.lock().unwrap().is_empty(),
            "a record already pointing at the gateway is left unchanged"
        );
    }

    #[test]
    fn a_refused_host_touches_neither_dns_nor_the_network() {
        let zone = FakeZone::default();
        let mut runner = Scripted::new(Vec::new());
        let err = run_with(
            &zone,
            &mut runner,
            &deployment("www.neonlaw.com"),
            "staging-iep.neonlaw.com",
            Some("192.0.2.40".into()),
            true,
            5,
        )
        .unwrap_err();
        assert!(err.to_string().contains("staging environment"));
        assert!(zone.writes.lock().unwrap().is_empty());
        assert!(runner.urls.is_empty());
    }

    #[test]
    fn an_explicit_gateway_ip_wins_and_a_blank_one_falls_back_to_resolution() {
        assert_eq!(
            gateway_ip(Some("192.0.2.50".into()), "unused.invalid").unwrap(),
            "192.0.2.50"
        );
        assert_eq!(
            gateway_ip(Some("  ".into()), "127.0.0.1").unwrap(),
            "127.0.0.1"
        );
        assert!(
            gateway_ip(None, "::1").is_err(),
            "an IPv6-only host has no IPv4 gateway"
        );
    }

    #[test]
    fn an_unparseable_or_missing_redirect_is_not_a_detour() {
        let garbled = "HTTP/2 303 \r\nlocation: not a url\r\n";
        assert!(sign_in_detours(garbled, "iep.neonlaw.com")
            .unwrap_err()
            .contains("unparseable redirect"));
        let wrong_path =
            "HTTP/2 303 \r\nlocation: https://www.neonlaw.com/app?return_host=iep.neonlaw.com\r\n";
        assert!(sign_in_detours(wrong_path, "iep.neonlaw.com").is_err());
        let found =
            "HTTP/2 302 \r\nlocation: https://www.neonlaw.com/auth/login/oidc?return_host=iep.neonlaw.com\r\n";
        assert!(sign_in_detours(found, "iep.neonlaw.com").is_err());
    }

    #[test]
    fn a_neonlaw_subdomain_brand_gets_a_label_in_the_neonlaw_zone() {
        let production = plan("iep.neonlaw.com", "www.neonlaw.com").unwrap();
        assert_eq!(production.key, BrandKey::IepDefenders);
        assert_eq!(production.zone, "neonlaw.com");
        assert_eq!(production.label, "iep");
        let staging = plan("staging-lex-tecnica.neonlaw.com", "staging.neonlaw.com").unwrap();
        assert_eq!(staging.key, BrandKey::LexTecnica);
        assert_eq!(staging.zone, "neonlaw.com");
        assert_eq!(staging.label, "staging-lex-tecnica");
    }

    #[test]
    fn a_brand_on_its_own_domain_uses_that_zone() {
        let plan = plan("STAGING.guiebshook.com.", "staging.neonlaw.com").unwrap();
        assert_eq!(plan.key, BrandKey::GuiebShook);
        assert_eq!(plan.zone, "guiebshook.com");
        assert_eq!(plan.label, "staging");
    }

    #[test]
    fn unregistered_and_cross_environment_hosts_are_refused() {
        let unknown = plan("other.neonlaw.com", "www.neonlaw.com").unwrap_err();
        assert!(unknown.to_string().contains("not a registered brand host"));
        let crossed = plan("staging-iep.neonlaw.com", "www.neonlaw.com").unwrap_err();
        assert!(crossed.to_string().contains("staging environment"));
        let reversed = plan("iep.neonlaw.com", "staging.neonlaw.com").unwrap_err();
        assert!(reversed.to_string().contains("production environment"));
    }

    #[test]
    fn sign_in_detour_requires_a_303_naming_the_host() {
        let ok = "HTTP/2 303 \r\nlocation: https://www.neonlaw.com/auth/login/oidc?return_host=iep.neonlaw.com&handoff_nonce=x\r\n";
        assert!(sign_in_detours(ok, "iep.neonlaw.com").is_ok());
        let shared = "HTTP/2 303 \r\nlocation: https://www.neonlaw.com/auth/login/oidc\r\n";
        assert!(sign_in_detours(shared, "iep.neonlaw.com").is_err());
        let missing = "HTTP/2 404 \r\n";
        assert!(sign_in_detours(missing, "iep.neonlaw.com").is_err());
    }
}
