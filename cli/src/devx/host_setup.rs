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
//!    URI. This checks that `/auth/login` on the host detours correctly.

use std::net::ToSocketAddrs;

use anyhow::{bail, Context, Result};
use views::brand::{is_staging_host, registered_brand_key, BrandKey};

use super::brand_readiness::{self, CommandRunner, ProcessCommandRunner, Verdict};
use super::dns;
use super::ship::ShipConfig;

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

/// Whether a `curl -D -` of `https://<host>/auth/login` shows the per-host
/// sign-in detour: a 303 to another origin's `/auth/login` that names this
/// host as `return_host`.
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
    if status == Some(303) && url.path() == "/auth/login" && names_host {
        Ok(format!(
            "/auth/login detours to {} and returns here",
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
        .block_on(async {
            let provider = dns::DnsimpleProvider::from_env()?;
            super::dns_setup_zone(&provider, &plan.zone, &config, &desired, apply).await
        })?;

    // 2. Certificate and 3. sign-in, both read-only.
    let mut runner = ProcessCommandRunner;
    let branding = plan.key.resolve_branding(&views::brand::DEFAULT_BRANDING);
    let tls = brand_readiness::check_host(
        &mut runner,
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
            format!("https://{}/auth/login", plan.host),
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
    use super::{plan, sign_in_detours};
    use views::brand::BrandKey;

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
        let plan = plan("STAGING.guiebandshook.com.", "staging.neonlaw.com").unwrap();
        assert_eq!(plan.key, BrandKey::GuiebShook);
        assert_eq!(plan.zone, "guiebandshook.com");
        assert_eq!(plan.label, "staging");
    }

    #[test]
    fn unregistered_and_cross_environment_hosts_are_refused() {
        let unknown = plan("other.neonlaw.com", "www.neonlaw.com").unwrap_err();
        assert!(unknown.to_string().contains("not a registered brand host"));
        let crossed = plan("staging-iep.neonlaw.com", "www.neonlaw.com").unwrap_err();
        assert!(crossed.to_string().contains("staging environment"));
    }

    #[test]
    fn sign_in_detour_requires_a_303_naming_the_host() {
        let ok = "HTTP/2 303 \r\nlocation: https://www.neonlaw.com/auth/login?return_host=iep.neonlaw.com&handoff_nonce=x\r\n";
        assert!(sign_in_detours(ok, "iep.neonlaw.com").is_ok());
        let shared = "HTTP/2 303 \r\nlocation: https://www.neonlaw.com/auth/login\r\n";
        assert!(sign_in_detours(shared, "iep.neonlaw.com").is_err());
        let missing = "HTTP/2 404 \r\n";
        assert!(sign_in_detours(missing, "iep.neonlaw.com").is_err());
    }
}
