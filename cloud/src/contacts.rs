//! Each firm member's own Google Contacts, kept in step with their matters.
//!
//! Google Voice and Google Fi both name an incoming caller from the signed-in
//! Workspace user's own Google Contacts. So the contacts that matter belong to
//! each lawyer and clerk, not to a shared mailbox: a [`ContactBooks`] opens one
//! member's book by acting as that member through the Workspace service account
//! ([`crate::delegation`]), and the account's domain-wide delegation must grant
//! the Contacts scope (`auth/contacts`).
//!
//! Navigator owns only the contacts it created. Each carries a `clientData`
//! entry keyed [`MANAGED_KEY`] holding the Person id. A member's own contacts
//! have no such entry, so a sync never reads, changes, or removes them.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const CONTACTS_SCOPE: &str = "https://www.googleapis.com/auth/contacts";
const PEOPLE_BASE_URL: &str = "https://people.googleapis.com/v1";
const PERSON_FIELDS: &str = "names,emailAddresses,phoneNumbers,clientData";
const UPDATE_FIELDS: &str = "names,emailAddresses,phoneNumbers";

/// The `clientData` key that marks a contact as Navigator's, holding the
/// Person id it mirrors.
pub const MANAGED_KEY: &str = "navigator_person_id";

/// The environment variables a contacts provider needs: the Workspace
/// delegation Drive and Calendar use.
pub const CONTACTS_CONFIGURATION_KEYS: [&str; 2] = crate::delegation::WORKSPACE_DELEGATION_KEYS;

/// One phone number as a Google contact carries it: the E.164 value and its
/// label. The value is never attached to a tracing span.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContactPhone {
    pub value: String,
    pub label: String,
}

/// What a member's book should hold for one Person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedContact {
    pub person_id: String,
    pub name: String,
    pub email: Option<String>,
    pub phones: Vec<ContactPhone>,
}

/// A Navigator contact as the book holds it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredContact {
    pub resource_name: String,
    pub etag: String,
    pub contact: ManagedContact,
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
}

/// One member's contact book, limited to the contacts Navigator manages.
#[async_trait]
pub trait ContactBook: Send + Sync {
    /// Every contact carrying [`MANAGED_KEY`].
    async fn managed_contacts(&self) -> Result<Vec<StoredContact>, ContactsError>;
    async fn create_contact(&self, contact: &ManagedContact) -> Result<(), ContactsError>;
    async fn update_contact(
        &self,
        stored: &StoredContact,
        contact: &ManagedContact,
    ) -> Result<(), ContactsError>;
    async fn delete_contact(&self, stored: &StoredContact) -> Result<(), ContactsError>;
}

/// Opens a firm member's contact book.
#[async_trait]
pub trait ContactBooks: Send + Sync {
    async fn book(&self, member_email: &str) -> Result<Arc<dyn ContactBook>, ContactsError>;
}

/// Opens books through the People API, acting as each member.
pub struct GoogleContactBooks {
    service_account: String,
    base_url: String,
}

impl GoogleContactBooks {
    /// Build from [`CONTACTS_CONFIGURATION_KEYS`], naming every missing key.
    ///
    /// # Errors
    /// [`ContactsError::MissingConfig`].
    pub fn from_env() -> Result<Self, ContactsError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    fn from_lookup<F>(get: F) -> Result<Self, ContactsError>
    where
        F: Fn(&str) -> Option<String>,
    {
        match crate::delegation::WorkspaceDelegation::from_lookup(get) {
            Ok(delegation) => Ok(Self {
                service_account: delegation.service_account,
                base_url: PEOPLE_BASE_URL.to_string(),
            }),
            Err(crate::delegation::DelegationError::MissingConfig(missing)) => {
                Err(ContactsError::MissingConfig(missing))
            }
            Err(crate::delegation::DelegationError::Authentication) => {
                Err(ContactsError::Authentication)
            }
        }
    }
}

#[async_trait]
impl ContactBooks for GoogleContactBooks {
    async fn book(&self, member_email: &str) -> Result<Arc<dyn ContactBook>, ContactsError> {
        let delegation = crate::delegation::WorkspaceDelegation {
            service_account: self.service_account.clone(),
            delegated_user: member_email.to_string(),
        };
        let token_source = delegation
            .token_source(&[CONTACTS_SCOPE])
            .await
            .map_err(|_| ContactsError::Authentication)?;
        Ok(Arc::new(GoogleContactBook::from_parts(
            token_source,
            &self.base_url,
        )))
    }
}

/// One member's book through the People API.
pub struct GoogleContactBook {
    token_source: Arc<dyn google_cloud_token::TokenSource>,
    http: reqwest::Client,
    base_url: String,
}

impl GoogleContactBook {
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

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        action: &'static str,
    ) -> Result<reqwest::Response, ContactsError> {
        let response = request
            .bearer_auth(self.token().await?)
            .send()
            .await
            .map_err(|source| ContactsError::Request { action, source })?;
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(ContactsError::Api {
                action,
                status: response.status().as_u16(),
            })
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionsPage {
    #[serde(default)]
    connections: Vec<ApiPerson>,
    next_page_token: Option<String>,
}

#[derive(Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct ApiPerson {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    resource_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    etag: String,
    #[serde(default)]
    names: Vec<ApiName>,
    #[serde(default)]
    email_addresses: Vec<ApiValue>,
    #[serde(default)]
    phone_numbers: Vec<ApiPhone>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    client_data: Vec<ApiClientData>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiName {
    #[serde(default)]
    unstructured_name: String,
}

#[derive(Deserialize, Serialize)]
struct ApiValue {
    value: String,
}

#[derive(Deserialize, Serialize)]
struct ApiPhone {
    value: String,
    #[serde(rename = "type", default)]
    label: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct ApiClientData {
    key: String,
    value: String,
}

impl ApiPerson {
    fn from_contact(contact: &ManagedContact) -> Self {
        Self {
            names: vec![ApiName {
                unstructured_name: contact.name.clone(),
            }],
            email_addresses: contact
                .email
                .iter()
                .map(|value| ApiValue {
                    value: value.clone(),
                })
                .collect(),
            phone_numbers: contact
                .phones
                .iter()
                .map(|phone| ApiPhone {
                    value: phone.value.clone(),
                    label: Some(phone.label.clone()),
                })
                .collect(),
            client_data: vec![ApiClientData {
                key: MANAGED_KEY.to_string(),
                value: contact.person_id.clone(),
            }],
            ..Self::default()
        }
    }

    /// The contact this entry mirrors, or `None` for one Navigator did not
    /// create.
    fn into_stored(self) -> Option<StoredContact> {
        let person_id = self
            .client_data
            .iter()
            .find(|entry| entry.key == MANAGED_KEY)?
            .value
            .clone();
        Some(StoredContact {
            resource_name: self.resource_name,
            etag: self.etag,
            contact: ManagedContact {
                person_id,
                name: self
                    .names
                    .into_iter()
                    .next()
                    .map(|name| name.unstructured_name)
                    .unwrap_or_default(),
                email: self.email_addresses.into_iter().next().map(|e| e.value),
                phones: self
                    .phone_numbers
                    .into_iter()
                    .map(|phone| ContactPhone {
                        value: phone.value,
                        label: phone.label.unwrap_or_else(|| "other".to_string()),
                    })
                    .collect(),
            },
        })
    }
}

#[async_trait]
impl ContactBook for GoogleContactBook {
    async fn managed_contacts(&self) -> Result<Vec<StoredContact>, ContactsError> {
        let action = "listing contacts";
        let mut managed = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut query = vec![("personFields", PERSON_FIELDS), ("pageSize", "1000")];
            if let Some(token) = page_token.as_deref() {
                query.push(("pageToken", token));
            }
            let request = self
                .http
                .get(format!("{}/people/me/connections", self.base_url))
                .query(&query);
            let page: ConnectionsPage = self
                .send(request, action)
                .await?
                .json()
                .await
                .map_err(|source| ContactsError::Response { action, source })?;
            managed.extend(
                page.connections
                    .into_iter()
                    .filter_map(ApiPerson::into_stored),
            );
            match page.next_page_token {
                Some(token) if !token.is_empty() => page_token = Some(token),
                _ => return Ok(managed),
            }
        }
    }

    async fn create_contact(&self, contact: &ManagedContact) -> Result<(), ContactsError> {
        let request = self
            .http
            .post(format!("{}/people:createContact", self.base_url))
            .json(&ApiPerson::from_contact(contact));
        self.send(request, "creating a contact").await.map(drop)
    }

    async fn update_contact(
        &self,
        stored: &StoredContact,
        contact: &ManagedContact,
    ) -> Result<(), ContactsError> {
        let mut body = ApiPerson::from_contact(contact);
        body.resource_name.clone_from(&stored.resource_name);
        body.etag.clone_from(&stored.etag);
        let request = self
            .http
            .patch(format!(
                "{}/{}:updateContact",
                self.base_url, stored.resource_name
            ))
            .query(&[("updatePersonFields", UPDATE_FIELDS)])
            .json(&body);
        self.send(request, "updating a contact").await.map(drop)
    }

    async fn delete_contact(&self, stored: &StoredContact) -> Result<(), ContactsError> {
        let request = self.http.delete(format!(
            "{}/{}:deleteContact",
            self.base_url, stored.resource_name
        ));
        self.send(request, "deleting a contact").await.map(drop)
    }
}

/// In-memory books for tests: each member's Navigator contacts by Person id.
#[derive(Debug, Default, Clone)]
pub struct FakeContactBooks {
    books: Arc<Mutex<BTreeMap<String, BTreeMap<String, ManagedContact>>>>,
}

impl FakeContactBooks {
    /// The Navigator contacts in `member_email`'s book, by Person id.
    #[must_use]
    pub fn contacts(&self, member_email: &str) -> BTreeMap<String, ManagedContact> {
        self.books
            .lock()
            .expect("fake contacts lock")
            .get(member_email)
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl ContactBooks for FakeContactBooks {
    async fn book(&self, member_email: &str) -> Result<Arc<dyn ContactBook>, ContactsError> {
        Ok(Arc::new(FakeContactBook {
            books: Arc::clone(&self.books),
            member: member_email.to_string(),
        }))
    }
}

struct FakeContactBook {
    books: Arc<Mutex<BTreeMap<String, BTreeMap<String, ManagedContact>>>>,
    member: String,
}

impl FakeContactBook {
    fn with_book<T>(&self, apply: impl FnOnce(&mut BTreeMap<String, ManagedContact>) -> T) -> T {
        let mut books = self.books.lock().expect("fake contacts lock");
        apply(books.entry(self.member.clone()).or_default())
    }
}

#[async_trait]
impl ContactBook for FakeContactBook {
    async fn managed_contacts(&self) -> Result<Vec<StoredContact>, ContactsError> {
        Ok(self.with_book(|book| {
            book.values()
                .map(|contact| StoredContact {
                    resource_name: format!("people/{}", contact.person_id),
                    etag: "etag".to_string(),
                    contact: contact.clone(),
                })
                .collect()
        }))
    }

    async fn create_contact(&self, contact: &ManagedContact) -> Result<(), ContactsError> {
        self.with_book(|book| book.insert(contact.person_id.clone(), contact.clone()));
        Ok(())
    }

    async fn update_contact(
        &self,
        _stored: &StoredContact,
        contact: &ManagedContact,
    ) -> Result<(), ContactsError> {
        self.create_contact(contact).await
    }

    async fn delete_contact(&self, stored: &StoredContact) -> Result<(), ContactsError> {
        self.with_book(|book| book.remove(&stored.contact.person_id));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use wiremock::matchers::{body_partial_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{
        ContactBook, ContactPhone, ContactsError, GoogleContactBook, GoogleContactBooks,
        ManagedContact, StoredContact, MANAGED_KEY, UPDATE_FIELDS,
    };

    #[derive(Debug)]
    struct StaticToken;

    #[async_trait]
    impl google_cloud_token::TokenSource for StaticToken {
        async fn token(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok("Bearer test-token".into())
        }
    }

    fn book(server: &MockServer) -> GoogleContactBook {
        GoogleContactBook::from_parts(Arc::new(StaticToken), &server.uri())
    }

    fn contact() -> ManagedContact {
        ManagedContact {
            person_id: "person-1".into(),
            name: "Ada Client".into(),
            email: Some("ada@example.com".into()),
            phones: vec![ContactPhone {
                value: "+15555550100".into(),
                label: "mobile".into(),
            }],
        }
    }

    #[test]
    fn missing_configuration_names_every_key() {
        let Err(ContactsError::MissingConfig(missing)) = GoogleContactBooks::from_lookup(|_| None)
        else {
            panic!("no configuration is refused");
        };
        assert!(missing.contains("NAVIGATOR_DRIVE_NEON_LAW_DELEGATED_USER"));
        assert!(missing.contains("NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT"));
    }

    #[tokio::test]
    async fn lists_only_navigator_contacts_across_pages() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/people/me/connections"))
            .and(query_param("pageToken", "next"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "connections": [{
                    "resourceName": "people/c2",
                    "etag": "e2",
                    "names": [{ "unstructuredName": "Ada Client" }],
                    "clientData": [{ "key": MANAGED_KEY, "value": "person-1" }]
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/people/me/connections"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "connections": [{
                    "resourceName": "people/c1",
                    "etag": "e1",
                    "names": [{ "unstructuredName": "A personal friend" }]
                }],
                "nextPageToken": "next"
            })))
            .mount(&server)
            .await;

        let managed = book(&server).managed_contacts().await.unwrap();
        assert_eq!(managed.len(), 1, "a contact without the marker is not ours");
        assert_eq!(managed[0].resource_name, "people/c2");
        assert_eq!(managed[0].contact.person_id, "person-1");
        assert_eq!(managed[0].contact.name, "Ada Client");
    }

    #[tokio::test]
    async fn creates_a_marked_contact() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/people:createContact"))
            .and(body_partial_json(serde_json::json!({
                "names": [{ "unstructuredName": "Ada Client" }],
                "phoneNumbers": [{ "value": "+15555550100", "type": "mobile" }],
                "clientData": [{ "key": MANAGED_KEY, "value": "person-1" }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        book(&server).create_contact(&contact()).await.unwrap();
    }

    #[tokio::test]
    async fn updates_with_the_etag_and_deletes_by_resource_name() {
        let server = MockServer::start().await;
        let stored = StoredContact {
            resource_name: "people/c2".into(),
            etag: "e2".into(),
            contact: contact(),
        };
        Mock::given(method("PATCH"))
            .and(path("/people/c2:updateContact"))
            .and(query_param("updatePersonFields", UPDATE_FIELDS))
            .and(body_partial_json(serde_json::json!({ "etag": "e2" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/people/c2:deleteContact"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        let book = book(&server);
        book.update_contact(&stored, &contact()).await.unwrap();
        book.delete_contact(&stored).await.unwrap();
    }

    #[tokio::test]
    async fn a_refused_call_names_its_action() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let error = book(&server).create_contact(&contact()).await.unwrap_err();
        assert!(matches!(
            error,
            ContactsError::Api {
                action: "creating a contact",
                status: 403
            }
        ));
    }
}
