use super::*;

fn catalog() -> views::locales::services::ServicesCatalog {
    views::locales::services::ServicesCatalog::parse(include_str!(
        "../../neon/locales/en/neon/services-catalog.yaml"
    ))
    .expect("the shipped service catalog parses")
}

/// The shipped catalog with the offered service mapped to the onboarding
/// letter, the way a Firm's Library would publish it. The start door maps
/// the service id to its template and ignores any client-named template,
/// so the loop test reaches an onboarding-kind notation through the same
/// service mapping production uses.
pub(super) fn onboarding_catalog() -> views::locales::services::ServicesCatalog {
    let mut catalog = catalog();
    let service = catalog
        .services
        .iter_mut()
        .find(|s| s.id == "llc-file")
        .expect("the offered service is in the catalog");
    service.template = Some("onboarding__letter".to_string());
    catalog
}

pub(super) async fn client(
    application_store: &store::application_store::ApplicationStore,
    name: &str,
    email: &str,
) -> store::persons::Person {
    let person = store::persons::create(
        application_store,
        &store::persons::NewPerson::with_role(name, email, store::persons::Role::Client),
    )
    .await
    .expect("create synthetic client");
    store::persons::set_admitted(application_store, person.id, true)
        .await
        .expect("admit synthetic client");
    store::persons::find_by_id(application_store, person.id)
        .await
        .expect("read synthetic client")
        .expect("synthetic client remains present")
}

pub(super) fn session(person: &store::persons::Person) -> SessionData {
    let mut session = SessionData::fresh(person.email.clone(), person.role);
    session.email = Some(person.email.clone());
    session.person_id = Some(person.id);
    session
}

pub(super) fn location(response: &Response) -> String {
    response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
        .unwrap_or_default()
}
