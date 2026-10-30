//! Google Contacts for a Person's phone numbers.
//!
//! When a Person's phone changes in Navigator, the same number belongs on
//! the Firm's Google contact for that person, so a lawyer dialing from
//! their phone sees what Navigator knows. This adapter updates one
//! contact's `phoneNumbers` through the People API, matched by email.
//!
//! It sits beside [`crate::calendar::GoogleCalendar`] on purpose: the same
//! Workspace service account and delegated user authenticate both, and the
//! account's domain-wide delegation must also grant the Contacts scope
//! (`auth/contacts`) before a live call succeeds. That grant is an account
//! fact the repository cannot supply, so live verification is external;
//! the code and its mocked tests build without it.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const CONTACTS_SCOPE: &str = "https://www.googleapis.com/auth/contacts";
const PEOPLE_BASE_URL: &str = "https://people.googleapis.com/v1";

/// The environment variables a contacts provider needs. They are Drive's
/// Workspace coordinates, because the same delegated user owns the firm
/// directory.
pub const CONTACTS_CONFIGURATION_KEYS: [&str; 2] = [
    "NAVIGATOR_DRIVE_NEON_LAW_DELEGATED_USER",
    "NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT_JSON",
];

/// One phone number as a Google contact carries it: the E.164 value and a
/// free-form type label. The value is never attached to a tracing span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactPhone {
    /// Canonical E.164 spelling, normalized by the store before it gets here.
    pub value: String,
    /// A People API `type`, conventionally `mobile`, `work`, `home`, or a
    /// custom label.
    pub label: String,
}

#[derive(Debug, Error)]
pub enum ContactsError {
    #[error("missing required Contacts configuration: {0}")]
    MissingConfig(String),
    #[error("Contacts authentication failed")]
    Authentication,
    #[error("Contacts request failed while {action}")]
    Request {
        action: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("Contacts API returned {status} while {action}")]
    Api { action: &'static str, status: u16 },
    #[error("Contacts API returned an invalid response while {action}")]
    Response {
        action: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("no Google contact answers to the email {0}")]
    NoContact(String),
    #[error("several Google contacts answer to the email {0}; refusing to guess")]
    AmbiguousContact(String),
}

/// Update a Google contact's phone numbers, matched by email.
#[async_trait]
pub trait ContactsService: Send + Sync {
    /// Replace the contact's stored number for `label` with `phone`,
    /// creating the entry when the contact has none of that label. The
    /// number must already be canonical E.164: Navigator normalizes at
    /// the store boundary, and this adapter does not guess spelling.
    ///
    /// # Errors
    /// [`ContactsError::NoContact`] when no directory contact carries the
    /// email; [`ContactsError::AmbiguousContact`] when more than one does.
    async fn set_phone(
        &self,
        email: &str,
        label: &str,
        phone: &ContactPhone,
    ) -> Result<(), ContactsError>;
}

/// People API client authenticated as the Workspace service account with
/// domain-wide delegation.
pub struct GoogleContacts {
    token_source: Arc<dyn google_cloud_token::TokenSource>,
    http: reqwest::Client,
    base_url: String,
}

impl GoogleContacts {
    /// Build from the delegated user and the service-account key.
    ///
    /// # Errors
    /// [`ContactsError::Authentication`] when the key does not parse or no
    /// token source can be built.
    pub async fn new(
        delegated_user: &str,
        service_account_json: &str,
    ) -> Result<Self, ContactsError> {
        let credentials =
            google_cloud_auth::credentials::CredentialsFile::new_from_str(service_account_json)
                .await
                .map_err(|_| ContactsError::Authentication)?;
        let scopes = [CONTACTS_SCOPE];
        let auth_config = google_cloud_auth::project::Config::default()
            .with_scopes(&scopes)
            .with_sub(delegated_user);
        let provider = google_cloud_auth::token::DefaultTokenSourceProvider::new_with_credentials(
            auth_config,
            Box::new(credentials),
        )
        .await
        .map_err(|_| ContactsError::Authentication)?;
        Ok(Self::from_parts(
            google_cloud_token::TokenSourceProvider::token_source(&provider),
            PEOPLE_BASE_URL,
        ))
    }

    /// Build from [`CONTACTS_CONFIGURATION_KEYS`], naming every missing key.
    ///
    /// # Errors
    /// [`ContactsError::MissingConfig`] or an authentication failure.
    pub async fn from_env() -> Result<Self, ContactsError> {
        Self::from_lookup(|key| std::env::var(key).ok()).await
    }

    async fn from_lookup<F>(get: F) -> Result<Self, ContactsError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let [user_key, json_key] = CONTACTS_CONFIGURATION_KEYS;
        let value = |key: &str| get(key).filter(|value| !value.trim().is_empty());
        match (value(user_key), value(json_key)) {
            (Some(user), Some(json)) => Self::new(&user, &json).await,
            (user, json) => {
                let missing: Vec<&str> = [(user_key, user.is_none()), (json_key, json.is_none())]
                    .into_iter()
                    .filter_map(|(key, absent)| absent.then_some(key))
                    .collect();
                Err(ContactsError::MissingConfig(missing.join(", ")))
            }
        }
    }

    fn from_parts(token_source: Arc<dyn google_cloud_token::TokenSource>, base_url: &str) -> Self {
        Self {
            token_source,
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    async fn token(&self) -> Result<String, ContactsError> {
        let raw = self
            .token_source
            .token()
            .await
            .map_err(|_| ContactsError::Authentication)?;
        Ok(raw.strip_prefix("Bearer ").unwrap_or(&raw).to_string())
    }

    fn checked(
        response: Result<reqwest::Response, reqwest::Error>,
        action: &'static str,
    ) -> Result<reqwest::Response, ContactsError> {
        let response = response.map_err(|source| ContactsError::Request { action, source })?;
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(ContactsError::Api {
                action,
                status: response.status().as_u16(),
            })
        }
    }

    /// Find the directory contact whose `emailAddresses` carry exactly
    /// `email` (case-insensitively). The search API is prefix-fuzzy, so the
    /// adapter filters its results and refuses an ambiguous match.
    async fn find_contact(
        &self,
        email: &str,
    ) -> Result<Option<(String, String, Vec<ContactPhone>)>, ContactsError> {
        let action = "searching contacts";
        let response = self
            .http
            .get(format!("{}/people:searchContacts", self.base_url))
            .bearer_auth(self.token().await?)
            .query(&[
                ("query", email),
                ("readMask", "emailAddresses,phoneNumbers,metadata"),
            ])
            .send()
            .await;
        let body: SearchResponse = Self::checked(response, action)?
            .json()
            .await
            .map_err(|source| ContactsError::Response { action, source })?;

        let wanted = email.trim().to_ascii_lowercase();
        let matches: Vec<&SearchPerson> = body
            .results
            .iter()
            .filter_map(|found| found.person.as_ref())
            .filter(|person| {
                person
                    .email_addresses
                    .iter()
                    .any(|address| address.value.trim().to_ascii_lowercase() == wanted)
            })
            .collect();
        match matches.as_slice() {
            [person] => Ok(Some((
                person.resource_name.clone(),
                person.etag.clone(),
                person
                    .phone_numbers
                    .iter()
                    .map(|phone| ContactPhone {
                        value: phone.value.clone(),
                        label: phone
                            .type_field
                            .clone()
                            .unwrap_or_else(|| "other".to_string()),
                    })
                    .collect(),
            ))),
            [] => Ok(None),
            _ => Err(ContactsError::AmbiguousContact(email.to_string())),
        }
    }
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    results: Vec<SearchResult>,
}

#[derive(Deserialize)]
struct SearchResult {
    #[serde(default)]
    person: Option<SearchPerson>,
}

#[derive(Deserialize)]
struct SearchPerson {
    /// The `people/{id}` path the update call addresses.
    #[serde(rename = "resourceName")]
    resource_name: String,
    /// Optimistic-concurrency token; the update carries it back.
    etag: String,
    #[serde(default, rename = "emailAddresses")]
    email_addresses: Vec<EmailAddress>,
    #[serde(default, rename = "phoneNumbers")]
    phone_numbers: Vec<ApiPhone>,
}

#[derive(Deserialize)]
struct EmailAddress {
    value: String,
}

#[derive(Deserialize)]
struct ApiPhone {
    value: String,
    #[serde(rename = "type")]
    type_field: Option<String>,
}

#[derive(Serialize)]
struct UpdateContactBody<'a> {
    etag: &'a str,
    #[serde(rename = "phoneNumbers")]
    phone_numbers: Vec<ApiPhoneOut<'a>>,
}

#[derive(Serialize)]
struct ApiPhoneOut<'a> {
    value: &'a str,
    #[serde(rename = "type")]
    type_field: &'a str,
}

#[async_trait]
impl ContactsService for GoogleContacts {
    async fn set_phone(
        &self,
        email: &str,
        label: &str,
        phone: &ContactPhone,
    ) -> Result<(), ContactsError> {
        let Some((resource_name, etag, mut phones)) = self.find_contact(email).await? else {
            return Err(ContactsError::NoContact(email.to_string()));
        };

        // Replace this label's entry, keep every other label's untouched:
        // the write is scoped to what changed so the directory never
        // loses a fax because a mobile moved.
        phones.retain(|existing| existing.label != label);
        phones.push(ContactPhone {
            value: phone.value.clone(),
            label: label.to_string(),
        });

        let action = "updating the contact";
        let body = UpdateContactBody {
            etag: &etag,
            phone_numbers: phones
                .iter()
                .map(|p| ApiPhoneOut {
                    value: &p.value,
                    type_field: &p.label,
                })
                .collect(),
        };
        let response = self
            .http
            .patch(format!("{}/{resource_name}", self.base_url))
            .bearer_auth(self.token().await?)
            .query(&[("updatePersonFields", "phoneNumbers")])
            .json(&body)
            .send()
            .await;
        Self::checked(response, action)?;
        Ok(())
    }
}

/// An in-memory directory for tests: no network, no credentials, and a
/// deterministic record of what was written.
#[derive(Default)]
pub struct FakeContacts {
    /// email -> (etag, phones). Emails are matched case-insensitively,
    /// like the real directory.
    directory: Mutex<BTreeMap<String, (u64, Vec<ContactPhone>)>>,
}

impl FakeContacts {
    /// Seed the directory with a contact at `email` carrying `phones`.
    pub fn with_contact(&self, email: &str, phones: Vec<ContactPhone>) {
        self.directory
            .lock()
            .expect("fake contacts lock")
            .insert(email.to_ascii_lowercase(), (1, phones));
    }

    /// The phones the directory now holds for `email`.
    pub fn phones(&self, email: &str) -> Vec<ContactPhone> {
        self.directory
            .lock()
            .expect("fake contacts lock")
            .get(&email.to_ascii_lowercase())
            .map(|(_, phones)| phones.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl ContactsService for FakeContacts {
    async fn set_phone(
        &self,
        email: &str,
        label: &str,
        phone: &ContactPhone,
    ) -> Result<(), ContactsError> {
        let mut directory = self.directory.lock().expect("fake contacts lock");
        let entry = directory
            .get_mut(&email.to_ascii_lowercase())
            .ok_or_else(|| ContactsError::NoContact(email.to_string()))?;
        entry.0 += 1;
        entry.1.retain(|existing| existing.label != label);
        entry.1.push(ContactPhone {
            value: phone.value.clone(),
            label: label.to_string(),
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{ContactPhone, ContactsError, ContactsService, FakeContacts, GoogleContacts};

    use async_trait::async_trait;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn missing_configuration_names_every_key() {
        let error = GoogleContacts::from_lookup(|_| None)
            .await
            .err()
            .expect("no configuration is refused");
        let message = error.to_string();
        assert!(
            message.contains("NAVIGATOR_DRIVE_NEON_LAW_DELEGATED_USER"),
            "{message}"
        );
        assert!(
            message.contains("NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT_JSON"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn an_unknown_contact_is_refused_not_created() {
        let fake = FakeContacts::default();
        let error = fake
            .set_phone(
                "stranger@example.com",
                "mobile",
                &ContactPhone {
                    value: "+15550142".to_string(),
                    label: "mobile".to_string(),
                },
            )
            .await
            .expect_err("an unknown contact is refused");
        assert!(matches!(error, ContactsError::NoContact(_)), "{error}");
    }

    #[tokio::test]
    async fn a_write_replaces_only_its_own_label() {
        let fake = FakeContacts::default();
        fake.with_contact(
            "client@example.com",
            vec![
                ContactPhone {
                    value: "+15550142".to_string(),
                    label: "mobile".to_string(),
                },
                ContactPhone {
                    value: "+15550100".to_string(),
                    label: "work".to_string(),
                },
            ],
        );
        fake.set_phone(
            "Client@example.com",
            "mobile",
            &ContactPhone {
                value: "+15550199".to_string(),
                label: "mobile".to_string(),
            },
        )
        .await
        .expect("a known contact accepts the write");
        let phones = fake.phones("client@example.com");
        assert_eq!(phones.len(), 2, "the work number survives");
        assert!(phones.contains(&ContactPhone {
            value: "+15550199".to_string(),
            label: "mobile".to_string(),
        }));
        assert!(phones.contains(&ContactPhone {
            value: "+15550100".to_string(),
            label: "work".to_string(),
        }));
    }

    #[derive(Debug)]
    struct StaticToken;

    #[async_trait]
    impl google_cloud_token::TokenSource for StaticToken {
        async fn token(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok("Bearer test-token".into())
        }
    }

    fn google(server: &MockServer) -> GoogleContacts {
        GoogleContacts::from_parts(Arc::new(StaticToken), &server.uri())
    }

    fn directory_person(resource_name: &str, email: &str) -> serde_json::Value {
        serde_json::json!({
            "resourceName": resource_name,
            "etag": "%etag-1",
            "emailAddresses": [{ "value": email }],
            "phoneNumbers": [{ "value": "+15550100", "type": "work" }]
        })
    }

    #[tokio::test]
    async fn a_live_write_searches_by_email_then_patches_the_phones() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/people:searchContacts"))
            .and(query_param("query", "client@example.com"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [
                    { "person": directory_person("people/123", "other@example.com") },
                    { "person": directory_person("people/456", "client@example.com") }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        // The patch carries the exact previous phones for every other
        // label, the new mobile, and the etag for optimistic concurrency.
        Mock::given(method("PATCH"))
            .and(path("/people/456"))
            .and(query_param("updatePersonFields", "phoneNumbers"))
            .respond_with(move |request: &wiremock::Request| {
                let body: serde_json::Value =
                    serde_json::from_slice(&request.body).expect("request body is json");
                let etag = body["etag"].as_str().expect("etag present");
                let phones = body["phoneNumbers"].as_array().expect("phones present");
                let values: Vec<&str> = phones.iter().filter_map(|p| p["value"].as_str()).collect();
                if etag != "%etag-1" || values != ["+15550100", "+15550199"] {
                    ResponseTemplate::new(400)
                } else {
                    ResponseTemplate::new(200)
                }
            })
            .mount(&server)
            .await;

        google(&server)
            .set_phone(
                "client@example.com",
                "mobile",
                &ContactPhone {
                    value: "+15550199".to_string(),
                    label: "mobile".to_string(),
                },
            )
            .await
            .expect("the mock directory accepts the write");
    }

    #[tokio::test]
    async fn a_fuzzy_search_hit_that_misses_the_exact_email_is_no_contact() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/people:searchContacts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                // The People search is prefix-fuzzy: `client` also returns
                // `clientco@example.com`, which is not the person asked for.
                "results": [
                    { "person": directory_person("people/789", "clientco@example.com") }
                ]
            })))
            .mount(&server)
            .await;
        let error = google(&server)
            .set_phone(
                "client@example.com",
                "mobile",
                &ContactPhone {
                    value: "+15550199".to_string(),
                    label: "mobile".to_string(),
                },
            )
            .await
            .expect_err("a fuzzy-only match is refused");
        assert!(matches!(error, ContactsError::NoContact(_)), "{error}");
    }

    #[tokio::test]
    async fn two_directory_hits_for_one_email_are_refused() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/people:searchContacts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [
                    { "person": directory_person("people/456", "client@example.com") },
                    { "person": directory_person("people/999", "Client@example.com") }
                ]
            })))
            .mount(&server)
            .await;
        let error = google(&server)
            .set_phone(
                "client@example.com",
                "mobile",
                &ContactPhone {
                    value: "+15550199".to_string(),
                    label: "mobile".to_string(),
                },
            )
            .await
            .expect_err("two contacts for one email are refused");
        assert!(
            matches!(error, ContactsError::AmbiguousContact(_)),
            "{error}"
        );
    }

    #[tokio::test]
    async fn an_api_failure_names_its_action_and_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/people:searchContacts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [
                    { "person": directory_person("people/456", "client@example.com") }
                ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let error = google(&server)
            .set_phone(
                "client@example.com",
                "mobile",
                &ContactPhone {
                    value: "+15550199".to_string(),
                    label: "mobile".to_string(),
                },
            )
            .await
            .expect_err("a refused patch surfaces its error");
        assert!(
            error.to_string().contains("updating the contact"),
            "{error}"
        );
    }
}
