//! `navigator site firm list|rename|theme|delete` and `navigator site brand
//! move|detach|delete`: administer Firms and the Brands they wear on a
//! deployment.
//!
//! Firms are named by slug and Brands by key; each verb resolves them from
//! the caller's authorized directory (`GET /app/api/firms`) and calls one
//! `/app/api` door, which is where authorization happens. Folding one Firm
//! into another is `brand move` for each of its Brands, then `firm delete`:
//! the deployment refuses to delete a Firm that still owns a Project or a
//! Brand.

use std::process::ExitCode;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

use crate::firm_forgejo::refusal;

/// One Firm as `GET /app/api/firms` lists it.
#[derive(Deserialize)]
struct Firm {
    id: uuid::Uuid,
    name: String,
    slug: String,
    brands: Vec<String>,
}

/// One administrative action against a deployment.
pub enum Action<'a> {
    List,
    Rename {
        slug: &'a str,
        name: &'a str,
    },
    /// Set the Firm's Portal Theme Brand, or clear it with `None`.
    Theme {
        slug: &'a str,
        key: Option<&'a str>,
    },
    Delete {
        slug: &'a str,
    },
    MoveBrand {
        key: &'a str,
        to: &'a str,
    },
    DetachBrand {
        key: &'a str,
    },
    DeleteBrand {
        key: &'a str,
    },
}

struct Site {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Site {
    async fn firms(&self) -> Result<Vec<Firm>> {
        let response = self
            .http
            .get(format!("{}/app/api/firms", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("GET /app/api/firms")?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("list Firms failed: {}", refusal(status, &body));
        }
        serde_json::from_str(&body).context("parse GET /app/api/firms")
    }

    async fn firm(&self, slug: &str) -> Result<Firm> {
        let slug = slug.trim();
        self.firms()
            .await?
            .into_iter()
            .find(|firm| firm.slug == slug)
            .ok_or_else(|| anyhow!("no Firm with the slug {slug:?} is visible to this caller"))
    }

    async fn wearer(&self, key: &str) -> Result<Firm> {
        self.firms()
            .await?
            .into_iter()
            .find(|firm| firm.brands.iter().any(|brand| brand == key))
            .ok_or_else(|| anyhow!("no Firm visible to this caller wears the Brand {key:?}"))
    }

    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &serde_json::Value,
        doing: &str,
    ) -> Result<()> {
        let response = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            // A body on every write, so the request states its length to the
            // ingress (ENG-1226).
            .json(body)
            .send()
            .await
            .with_context(|| doing.to_string())?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!("{doing} refused: {}", refusal(status, &body));
        }
        Ok(())
    }
}

fn listing(firms: &[Firm]) -> String {
    firms
        .iter()
        .map(|firm| {
            let brands = if firm.brands.is_empty() {
                "(no brands)".to_string()
            } else {
                firm.brands.join(", ")
            };
            format!("{}\t{}\t{brands}", firm.slug, firm.name)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn run_at(base: &str, token: &str, action: Action<'_>) -> Result<String> {
    use reqwest::Method;
    let site = Site {
        http: reqwest::Client::new(),
        base: base.to_string(),
        token: token.to_string(),
    };
    let empty = serde_json::json!({});
    match action {
        Action::List => Ok(listing(&site.firms().await?)),
        Action::Rename { slug, name } => {
            let firm = site.firm(slug).await?;
            let body = serde_json::json!({ "name": name });
            let path = format!("/app/api/firms/{}", firm.id);
            site.send(Method::PATCH, &path, &body, "rename Firm")
                .await?;
            Ok(format!(
                "{}: renamed {:?} to {name:?}",
                firm.slug, firm.name
            ))
        }
        Action::Theme { slug, key } => {
            let firm = site.firm(slug).await?;
            let body = serde_json::json!({ "portal_theme_brand_key": key });
            let path = format!("/app/api/firms/{}", firm.id);
            site.send(Method::PATCH, &path, &body, "set portal theme")
                .await?;
            Ok(match key {
                Some(key) => format!("{}: portal theme is now {key}", firm.slug),
                None => format!("{}: portal theme cleared", firm.slug),
            })
        }
        Action::Delete { slug } => {
            let firm = site.firm(slug).await?;
            let path = format!("/app/api/firms/{}", firm.id);
            site.send(Method::DELETE, &path, &empty, "delete Firm")
                .await?;
            Ok(format!("{}: deleted {:?}", firm.slug, firm.name))
        }
        Action::MoveBrand { key, to } => {
            let target = site.firm(to).await?;
            let body = serde_json::json!({ "firm_id": target.id });
            let path = format!("/app/api/brands/{key}/firm");
            site.send(Method::PUT, &path, &body, "move Brand").await?;
            Ok(format!(
                "{key}: now worn by {} ({})",
                target.name, target.slug
            ))
        }
        Action::DetachBrand { key } => {
            let firm = site.wearer(key).await?;
            let path = format!("/app/api/firms/{}/brands/{key}", firm.id);
            site.send(Method::DELETE, &path, &empty, "detach Brand")
                .await?;
            Ok(format!(
                "{key}: no longer worn by {} ({})",
                firm.name, firm.slug
            ))
        }
        Action::DeleteBrand { key } => {
            let path = format!("/app/api/brands/{key}");
            site.send(Method::DELETE, &path, &empty, "delete Brand")
                .await?;
            Ok(format!("{key}: deleted"))
        }
    }
}

/// Run one `navigator site firm` or `navigator site brand` administrative verb.
pub async fn run(host: Option<&str>, action: Action<'_>) -> ExitCode {
    let result = async {
        let (base, token) = crate::remote::resolve(host)?;
        run_at(&base, &token, action).await
    }
    .await;
    match result {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{run_at, Action};
    use serde_json::json;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SOURCE: &str = "0192f0a0-0000-7000-8000-000000000001";
    const TARGET: &str = "0192f0a0-0000-7000-8000-000000000002";

    async fn directory() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/firms"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": SOURCE, "name": "Example Duplicate", "slug": "example-duplicate",
                  "brands": ["example-brand"] },
                { "id": TARGET, "name": "Example Survivor", "slug": "example-survivor",
                  "brands": [] }
            ])))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn the_directory_lists_each_firm_with_its_brands() {
        let server = directory().await;
        let report = run_at(&server.uri(), "t", Action::List).await.unwrap();
        assert_eq!(
            report,
            "example-duplicate\tExample Duplicate\texample-brand\n\
             example-survivor\tExample Survivor\t(no brands)"
        );
    }

    #[tokio::test]
    async fn slugs_and_brand_keys_resolve_to_the_ids_each_door_takes() {
        let server = directory().await;
        Mock::given(method("PUT"))
            .and(path("/app/api/brands/example-brand/firm"))
            .and(body_json(json!({ "firm_id": TARGET })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(format!(
                "/app/api/firms/{SOURCE}/brands/example-brand"
            )))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(format!("/app/api/firms/{TARGET}")))
            .and(body_json(json!({ "name": "Example Survivor PC" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        let base = server.uri();
        let moved = Action::MoveBrand {
            key: "example-brand",
            to: "example-survivor",
        };
        assert_eq!(
            run_at(&base, "t", moved).await.unwrap(),
            "example-brand: now worn by Example Survivor (example-survivor)"
        );
        let detached = Action::DetachBrand {
            key: "example-brand",
        };
        run_at(&base, "t", detached).await.unwrap();
        let renamed = Action::Rename {
            slug: "example-survivor",
            name: "Example Survivor PC",
        };
        run_at(&base, "t", renamed).await.unwrap();
    }

    #[tokio::test]
    async fn theme_sets_or_clears_the_firm_s_portal_theme_brand() {
        let server = directory().await;
        Mock::given(method("PATCH"))
            .and(path(format!("/app/api/firms/{SOURCE}")))
            .and(body_json(
                json!({ "portal_theme_brand_key": "example-brand" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(format!("/app/api/firms/{SOURCE}")))
            .and(body_json(json!({ "portal_theme_brand_key": null })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        let base = server.uri();
        let set = Action::Theme {
            slug: "example-duplicate",
            key: Some("example-brand"),
        };
        assert_eq!(
            run_at(&base, "t", set).await.unwrap(),
            "example-duplicate: portal theme is now example-brand"
        );
        let cleared = Action::Theme {
            slug: "example-duplicate",
            key: None,
        };
        assert_eq!(
            run_at(&base, "t", cleared).await.unwrap(),
            "example-duplicate: portal theme cleared"
        );
    }

    #[tokio::test]
    async fn the_servers_refusal_is_reported_and_an_unknown_slug_writes_nothing() {
        let server = directory().await;
        Mock::given(method("DELETE"))
            .and(path(format!("/app/api/firms/{SOURCE}")))
            .respond_with(ResponseTemplate::new(409).set_body_json(json!({
                "error": "conflict", "message": "That firm still owns brands."
            })))
            .mount(&server)
            .await;
        let base = server.uri();
        let error = run_at(
            &base,
            "t",
            Action::Delete {
                slug: "example-duplicate",
            },
        )
        .await
        .unwrap_err()
        .to_string();
        assert_eq!(
            error,
            "delete Firm refused: 409 Conflict: That firm still owns brands."
        );
        let error = run_at(
            &base,
            "t",
            Action::Delete {
                slug: "no-such-firm",
            },
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("no Firm with the slug"), "{error}");
    }
}

use crate::{firm_forgejo, HostOpt};
use clap::Subcommand;
#[derive(Subcommand)]
pub enum SiteFirmCmd {
    /// Create or reconcile the Firm's Forgejo organization.
    ///
    /// The deployment's provisioning identity creates the private
    /// organization the Firm's slug names, and adds the account the Firm's
    /// own Forgejo access token authenticates to its owners team, so the
    /// Firm's Project repositories can be created or copied there without
    /// anyone holding site administration. Safe to re-run: an existing
    /// organization and owner are left as they are. Owner, or an Admin of
    /// the Firm.
    ProvisionForgejo {
        /// The Firm's slug, as `GET /app/api/firms` lists it.
        slug: String,
        #[command(flatten)]
        host: HostOpt,
    },
    /// List the Firms this login may administer: slug, name, and the Brands
    /// each wears, one Firm per tab-separated line.
    List {
        #[command(flatten)]
        host: HostOpt,
    },
    /// Rename a Firm. Its slug, and the Forgejo organization the slug names,
    /// is unchanged. Owner, or an Admin of the Firm.
    Rename {
        /// The Firm's slug, as `navigator site firm list` prints it.
        slug: String,
        /// The Firm's new display name.
        name: String,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Set the Firm's Portal Theme Brand: the Brand it owns whose colours,
    /// logo, and name every signed-in `/app` page reached through the
    /// Firm's hosts wears. Owner, or an Admin of the Firm.
    #[command(group(clap::ArgGroup::new("theme").required(true).args(["key", "clear"])))]
    Theme {
        /// The Firm's slug, as `navigator site firm list` prints it.
        slug: String,
        /// The key of a Brand this Firm owns.
        key: Option<String>,
        /// Clear the theme, so each host keeps its own Brand's look.
        #[arg(long)]
        clear: bool,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Delete a Firm and its memberships. Refused while the Firm owns any
    /// Project or Brand: move each Brand with `navigator site brand move`
    /// first. Owner, or an Admin of the Firm.
    Delete {
        /// The Firm's slug, as `navigator site firm list` prints it.
        slug: String,
        #[command(flatten)]
        host: HostOpt,
    },
}

#[derive(Subcommand)]
pub enum SiteBrandCmd {
    /// Move a Brand to another Firm, so its public pages name that Firm's
    /// legal entity. Owner only.
    Move {
        /// The Brand's key, as `navigator site firm list` prints it.
        key: String,
        /// The slug of the Firm the Brand moves to.
        #[arg(long)]
        to: String,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Stop the Firm that wears a Brand from wearing it. The Brand row
    /// remains. Owner, or an Admin of that Firm.
    Detach {
        /// The Brand's key, as `navigator site firm list` prints it.
        key: String,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Delete a Brand. Refused while any Firm wears it (`detach` it first)
    /// or any Project names it. Owner, or the owning Firm's Admin DRI.
    Delete {
        /// The Brand's key.
        key: String,
        #[command(flatten)]
        host: HostOpt,
    },
}

pub async fn run_firm(action: SiteFirmCmd) -> ExitCode {
    match action {
        SiteFirmCmd::ProvisionForgejo { slug, host } => {
            firm_forgejo::provision(host.host.as_deref(), &slug).await
        }
        SiteFirmCmd::List { host } => run(host.host.as_deref(), Action::List).await,
        SiteFirmCmd::Rename { slug, name, host } => {
            run(
                host.host.as_deref(),
                Action::Rename {
                    slug: &slug,
                    name: &name,
                },
            )
            .await
        }
        SiteFirmCmd::Theme {
            slug,
            key,
            clear: _,
            host,
        } => {
            run(
                host.host.as_deref(),
                Action::Theme {
                    slug: &slug,
                    key: key.as_deref(),
                },
            )
            .await
        }
        SiteFirmCmd::Delete { slug, host } => {
            run(host.host.as_deref(), Action::Delete { slug: &slug }).await
        }
    }
}

pub async fn run_brand(action: SiteBrandCmd) -> ExitCode {
    match action {
        SiteBrandCmd::Move { key, to, host } => {
            run(
                host.host.as_deref(),
                Action::MoveBrand { key: &key, to: &to },
            )
            .await
        }
        SiteBrandCmd::Detach { key, host } => {
            run(host.host.as_deref(), Action::DetachBrand { key: &key }).await
        }
        SiteBrandCmd::Delete { key, host } => {
            run(host.host.as_deref(), Action::DeleteBrand { key: &key }).await
        }
    }
}
