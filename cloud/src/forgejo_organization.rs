//! A Firm's organization on the installation's Forgejo, reconciled by
//! Navigator's provisioning identity.
//!
//! The installation sets `DISABLE_REGULAR_ORG_CREATION`, so only a site
//! administrator may create an organization. Navigator holds one dedicated
//! provisioning account for that, whose token is
//! [`NAVIGATOR_FORGEJO_PROVISIONER_TOKEN_ENV`] on the web process and never
//! on anyone's workstation. [`ForgejoOrganizations`] is the only client that
//! uses it, and it does two things: make sure an organization of a given name
//! exists and is private, and make sure a given account sits in that
//! organization's owners team. Both converge: a re-run against an existing
//! organization, or a member already present, writes nothing.
//!
//! Every call is bounded by a request timeout, and the token never appears in
//! a `Debug` rendering or an error.

use std::time::Duration;

use serde::Deserialize;

use crate::forge::ForgeError;
use crate::forgejo_forge::NAVIGATOR_FORGEJO_ROOT_URL_ENV;

/// The provisioning account's access token: a Forgejo site administrator
/// that exists only for Navigator, scoped to `write:organization` and
/// `read:user`.
pub const NAVIGATOR_FORGEJO_PROVISIONER_TOKEN_ENV: &str = "NAVIGATOR_FORGEJO_PROVISIONER_TOKEN";
/// Every Firm organization's visibility.
pub const ORGANIZATION_VISIBILITY: &str = "private";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const USER_AGENT: &str = concat!("neon-law-navigator/", env!("CARGO_PKG_VERSION"));

/// Why an organization could not be reconciled.
#[derive(Debug, thiserror::Error)]
pub enum OrganizationError {
    /// The forge itself failed, or a setting is missing or malformed.
    #[error(transparent)]
    Forge(#[from] ForgeError),
    /// The provisioning account may not create organizations: it is not a
    /// site administrator, or its token lacks `write:organization`.
    #[error(
        "the Forgejo provisioning account may not create organizations; it must be a site \
         administrator with a write:organization token"
    )]
    NotPermitted,
    /// Forgejo refused the name, and no organization holds it: a user
    /// account or a reserved name already claims it.
    #[error(
        "Forgejo refuses the organization name {name}; a user account or reserved name holds it"
    )]
    NameUnavailable { name: String },
    /// The organization has no team with owner permission to add to.
    #[error("organization {name} has no owners team")]
    NoOwnersTeam { name: String },
    /// The forge answered for an organization of another name.
    #[error("Forgejo answered for organization {found}, not {expected}")]
    Mismatch { expected: String, found: String },
}

/// An organization as the forge reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Organization {
    /// Forgejo's numeric id for the organization. A deleted and recreated
    /// organization of the same name has a new one.
    pub id: i64,
    /// The organization's name.
    pub name: String,
    /// Whether this call created it.
    pub created: bool,
    /// Whether this call changed its visibility to private.
    pub made_private: bool,
}

#[derive(Deserialize)]
struct OrganizationBody {
    id: i64,
    #[serde(alias = "name")]
    username: String,
    visibility: Option<String>,
}

#[derive(Deserialize)]
struct TeamBody {
    id: i64,
    permission: Option<String>,
}

#[derive(Deserialize)]
struct UserBody {
    login: String,
}

/// Navigator's provisioning identity on one Forgejo installation.
#[derive(Clone)]
pub struct ForgejoOrganizations {
    api_base: String,
    token: String,
    http: reqwest::Client,
}

impl std::fmt::Debug for ForgejoOrganizations {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ForgejoOrganizations")
            .field("api_base", &self.api_base)
            .field("token", &"[redacted]")
            .finish_non_exhaustive()
    }
}

/// The API base of `root_url`, refusing anything but `https` (or `http` on a
/// loopback test host).
fn api_base(root_url: &str) -> Result<String, ForgeError> {
    let root = root_url.trim().trim_end_matches('/');
    let loopback = root.starts_with("http://127.0.0.1") || root.starts_with("http://localhost");
    if !(root.starts_with("https://") || loopback) {
        return Err(ForgeError::InvalidConfig(NAVIGATOR_FORGEJO_ROOT_URL_ENV));
    }
    Ok(format!("{root}/api/v1"))
}

fn checked(
    response: Result<reqwest::Response, reqwest::Error>,
    action: &'static str,
) -> Result<reqwest::Response, ForgeError> {
    let response = response.map_err(|source| ForgeError::Request { action, source })?;
    match response.status().as_u16() {
        200..=299 => Ok(response),
        401 | 403 => Err(ForgeError::Authentication),
        status => Err(ForgeError::Api { action, status }),
    }
}

async fn json<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
    action: &'static str,
) -> Result<T, ForgeError> {
    response
        .json::<T>()
        .await
        .map_err(|source| ForgeError::Response { action, source })
}

fn request(token: &str, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    builder
        .header(reqwest::header::AUTHORIZATION, format!("token {token}"))
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .timeout(REQUEST_TIMEOUT)
}

/// The login of the account `token` authenticates, on the installation at
/// `root_url`. This is how a Firm's own access token names the account that
/// should own the Firm's organization.
///
/// # Errors
/// [`ForgeError`] for a malformed root URL, a blank or rejected token, or a
/// forge failure.
pub async fn token_login(root_url: &str, token: &str) -> Result<String, ForgeError> {
    let action = "reading the authenticated account";
    if token.trim().is_empty() {
        return Err(ForgeError::MissingConfig("Firm Forgejo access token"));
    }
    let url = format!("{}/user", api_base(root_url)?);
    let response = checked(
        request(token.trim(), reqwest::Client::new().get(url))
            .send()
            .await,
        action,
    )?;
    Ok(json::<UserBody>(response, action).await?.login)
}

impl ForgejoOrganizations {
    /// The provisioning client for the installation at `root_url`.
    ///
    /// # Errors
    /// [`ForgeError::InvalidConfig`] for a root URL that is not `https` (or
    /// `http` on a loopback test host); [`ForgeError::MissingConfig`] for a
    /// blank token.
    pub fn new(root_url: &str, token: &str) -> Result<Self, ForgeError> {
        let api_base = api_base(root_url)?;
        if token.trim().is_empty() {
            return Err(ForgeError::MissingConfig(
                NAVIGATOR_FORGEJO_PROVISIONER_TOKEN_ENV,
            ));
        }
        Ok(Self {
            api_base,
            token: token.trim().to_string(),
            http: reqwest::Client::new(),
        })
    }

    fn org_url(name: &str, api_base: &str) -> Result<String, ForgeError> {
        if !crate::workspace::is_valid_slug(name) {
            return Err(ForgeError::InvalidConfig("Forgejo organization name"));
        }
        Ok(format!("{api_base}/orgs/{name}"))
    }

    async fn find(&self, name: &str) -> Result<Option<OrganizationBody>, ForgeError> {
        let action = "reading the organization";
        let url = Self::org_url(name, &self.api_base)?;
        let response = request(&self.token, self.http.get(url)).send().await;
        let response = match response {
            Ok(response) if response.status().as_u16() == 404 => return Ok(None),
            other => checked(other, action)?,
        };
        json(response, action).await.map(Some)
    }

    /// Make sure the private organization `name` exists, creating it when it
    /// does not and making it private when it is not.
    ///
    /// A concurrent run that creates the organization between this call's
    /// read and its create is adopted, not reported as a failure.
    ///
    /// # Errors
    /// [`OrganizationError::NotPermitted`] when the provisioning account may
    /// not create organizations; [`OrganizationError::NameUnavailable`] when a
    /// user account or reserved name holds `name`; [`OrganizationError::Forge`]
    /// for any other forge failure.
    pub async fn ensure(&self, name: &str) -> Result<Organization, OrganizationError> {
        let (body, created) = match self.find(name).await? {
            Some(existing) => (existing, false),
            None => match self.create(name).await? {
                Some(created) => (created, true),
                None => match self.find(name).await? {
                    // Another run created it first.
                    Some(raced) => (raced, false),
                    None => {
                        return Err(OrganizationError::NameUnavailable {
                            name: name.to_string(),
                        })
                    }
                },
            },
        };
        if body.username != name {
            return Err(OrganizationError::Mismatch {
                expected: name.to_string(),
                found: body.username,
            });
        }
        let made_private = if body.visibility.as_deref() == Some(ORGANIZATION_VISIBILITY) {
            false
        } else {
            self.make_private(name).await?;
            true
        };
        tracing::info!(
            created,
            made_private,
            "Forgejo organization reconciliation completed"
        );
        Ok(Organization {
            id: body.id,
            name: body.username,
            created,
            made_private,
        })
    }

    /// Create the organization. `None` when Forgejo refuses the name as
    /// already taken, which [`Self::ensure`] resolves by reading again.
    async fn create(&self, name: &str) -> Result<Option<OrganizationBody>, OrganizationError> {
        let action = "creating the organization";
        let response = request(
            &self.token,
            self.http.post(format!("{}/orgs", self.api_base)),
        )
        .json(&serde_json::json!({
            "username": name,
            "visibility": ORGANIZATION_VISIBILITY,
        }))
        .send()
        .await
        .map_err(|source| ForgeError::Request { action, source })?;
        match response.status().as_u16() {
            200..=299 => Ok(Some(json(response, action).await?)),
            403 => Err(OrganizationError::NotPermitted),
            401 => Err(ForgeError::Authentication.into()),
            409 | 422 => Ok(None),
            status => Err(ForgeError::Api { action, status }.into()),
        }
    }

    async fn make_private(&self, name: &str) -> Result<(), ForgeError> {
        let action = "making the organization private";
        let url = Self::org_url(name, &self.api_base)?;
        checked(
            request(&self.token, self.http.patch(url))
                .json(&serde_json::json!({ "visibility": ORGANIZATION_VISIBILITY }))
                .send()
                .await,
            action,
        )?;
        Ok(())
    }

    /// Make sure `login` belongs to the owners team of organization `name`.
    /// Returns whether this call added it.
    ///
    /// # Errors
    /// [`OrganizationError::NoOwnersTeam`] when the organization has no team
    /// with owner permission; [`OrganizationError::Forge`] for a malformed
    /// login or a forge failure.
    pub async fn ensure_owner(&self, name: &str, login: &str) -> Result<bool, OrganizationError> {
        if !is_valid_login(login) {
            return Err(ForgeError::InvalidConfig("Forgejo account login").into());
        }
        let action = "listing the organization's teams";
        let url = format!("{}/teams", Self::org_url(name, &self.api_base)?);
        let response = checked(
            request(&self.token, self.http.get(url)).send().await,
            action,
        )?;
        let teams: Vec<TeamBody> = json(response, action).await?;
        let owners = teams
            .into_iter()
            .find(|team| team.permission.as_deref() == Some("owner"))
            .ok_or_else(|| OrganizationError::NoOwnersTeam {
                name: name.to_string(),
            })?;
        let member = format!("{}/teams/{}/members/{login}", self.api_base, owners.id);
        let action = "reading the owners team membership";
        let response = request(&self.token, self.http.get(&member)).send().await;
        match response {
            Ok(response) if response.status().as_u16() == 404 => {}
            other => {
                checked(other, action)?;
                return Ok(false);
            }
        }
        checked(
            request(&self.token, self.http.put(&member)).send().await,
            "adding the owners team member",
        )?;
        tracing::info!("Forgejo organization owner added");
        Ok(true)
    }
}

/// Whether `login` has the shape of a Forgejo account name, so it can be
/// placed in a URL path: ASCII letters, digits, `-`, `_`, and `.`, starting
/// with a letter or digit, at most 40 characters.
fn is_valid_login(login: &str) -> bool {
    !login.is_empty()
        && login.len() <= 40
        && login
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
        && login
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

#[cfg(test)]
mod tests {
    use super::{token_login, ForgejoOrganizations, OrganizationError};
    use crate::forge::ForgeError;
    use serde_json::json;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const ORG: &str = "example-law-pc";

    fn org(id: i64, visibility: &str) -> serde_json::Value {
        json!({ "id": id, "username": ORG, "visibility": visibility })
    }

    fn client(server: &MockServer) -> ForgejoOrganizations {
        ForgejoOrganizations::new(&server.uri(), "provisioner-token").unwrap()
    }

    #[test]
    fn configuration_is_refused_before_any_request() {
        assert!(ForgejoOrganizations::new("http://forge.example.test", "t").is_err());
        assert!(ForgejoOrganizations::new("https://forge.example.test", " ").is_err());
        let client = ForgejoOrganizations::new("https://forge.example.test/", "secret").unwrap();
        let rendered = format!("{client:?}");
        assert!(!rendered.contains("secret"), "{rendered}");
        assert!(rendered.contains("https://forge.example.test/api/v1"));
    }

    #[tokio::test]
    async fn a_missing_organization_is_created_private_by_the_provisioning_account() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/orgs/{ORG}")))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/orgs"))
            .and(header("authorization", "token provisioner-token"))
            .and(body_json(
                json!({ "username": ORG, "visibility": "private" }),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(org(7, "private")))
            .expect(1)
            .mount(&server)
            .await;

        let organization = client(&server).ensure(ORG).await.unwrap();
        assert_eq!(organization.id, 7);
        assert_eq!(organization.name, ORG);
        assert!(organization.created);
        assert!(!organization.made_private);
    }

    #[tokio::test]
    async fn an_existing_private_organization_converges_without_a_write() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/orgs/{ORG}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(org(7, "private")))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let client = client(&server);
        for _ in 0..2 {
            let organization = client.ensure(ORG).await.unwrap();
            assert_eq!(organization.id, 7);
            assert!(!organization.created && !organization.made_private);
        }
    }

    #[tokio::test]
    async fn a_public_organization_is_made_private() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/orgs/{ORG}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(org(7, "public")))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(format!("/api/v1/orgs/{ORG}")))
            .and(body_json(json!({ "visibility": "private" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(org(7, "private")))
            .expect(1)
            .mount(&server)
            .await;

        let organization = client(&server).ensure(ORG).await.unwrap();
        assert!(organization.made_private);
        assert!(!organization.created);
    }

    #[tokio::test]
    async fn a_concurrent_create_is_adopted() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/orgs/{ORG}")))
            .respond_with(ResponseTemplate::new(404))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/orgs/{ORG}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(org(9, "private")))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/orgs"))
            .respond_with(ResponseTemplate::new(422))
            .expect(1)
            .mount(&server)
            .await;

        let organization = client(&server).ensure(ORG).await.unwrap();
        assert_eq!(organization.id, 9);
        assert!(!organization.created);
    }

    #[tokio::test]
    async fn a_name_held_by_a_user_account_is_refused() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/orgs/{ORG}")))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/orgs"))
            .respond_with(ResponseTemplate::new(422))
            .mount(&server)
            .await;

        let error = client(&server).ensure(ORG).await.unwrap_err();
        assert!(
            matches!(&error, OrganizationError::NameUnavailable { name } if name == ORG),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_provisioning_account_without_site_administration_is_named() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/orgs/{ORG}")))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/orgs"))
            .respond_with(
                ResponseTemplate::new(403).set_body_json(json!({ "message": "%!s(<nil>)" })),
            )
            .mount(&server)
            .await;

        let error = client(&server).ensure(ORG).await.unwrap_err();
        assert!(matches!(error, OrganizationError::NotPermitted), "{error}");
        assert!(error.to_string().contains("site administrator"));
    }

    #[tokio::test]
    async fn an_invalid_organization_name_never_reaches_the_forge() {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let client = client(&server);
        for name in ["Example Law", "../admin", "", "a/b"] {
            assert!(matches!(
                client.ensure(name).await,
                Err(OrganizationError::Forge(ForgeError::InvalidConfig(_)))
            ));
        }
        assert!(matches!(
            client.ensure_owner(ORG, "../x").await,
            Err(OrganizationError::Forge(ForgeError::InvalidConfig(_)))
        ));
    }

    #[tokio::test]
    async fn the_owner_is_added_once_to_the_owners_team() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/orgs/{ORG}/teams")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": 3, "name": "Readers", "permission": "read" },
                { "id": 4, "name": "Owners", "permission": "owner" },
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/teams/4/members/firm-account"))
            .respond_with(ResponseTemplate::new(404))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/teams/4/members/firm-account"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "login": "firm-account" })),
            )
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/api/v1/teams/4/members/firm-account"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let client = client(&server);
        assert!(client.ensure_owner(ORG, "firm-account").await.unwrap());
        assert!(!client.ensure_owner(ORG, "firm-account").await.unwrap());
    }

    #[tokio::test]
    async fn an_organization_without_an_owners_team_is_reported() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/orgs/{ORG}/teams")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        assert!(matches!(
            client(&server).ensure_owner(ORG, "firm-account").await,
            Err(OrganizationError::NoOwnersTeam { .. })
        ));
    }

    #[tokio::test]
    async fn a_firm_token_names_its_account() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user"))
            .and(header("authorization", "token firm-token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "login": "firm-account" })),
            )
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            token_login(&server.uri(), "firm-token").await.unwrap(),
            "firm-account"
        );
        assert!(matches!(
            token_login(&server.uri(), " ").await,
            Err(ForgeError::MissingConfig(_))
        ));
    }
}
