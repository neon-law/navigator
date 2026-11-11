//! Cucumber runner for `tests/features/offer_letter_signing.feature`.
//!
//! The generalized send flow driven by a template that declares its own
//! signers: the employee offer letter names the company's officer
//! (embedded, the bound client) and the employee (emailed, email bound
//! to a questionnaire answer) in its `signers:` frontmatter, and the
//! envelope routes company-first because the template says so — not
//! because the send path assumes a retainer's shape.

// Cucumber's step-attribute macros require `async fn`, so assertion
// steps that don't await anything still have to be declared async.
#![allow(clippy::unused_async)]

use cucumber::{gherkin::Step, given, then, when, World};
use features::journey::Journey;
use uuid::Uuid;

const TEMPLATE_CODE: &str = "employee_offer_letter";
/// The employee's email — the value the walk types into the
/// `custom_text__employee_email` answer the signer declaration binds to.
const BOUND_EMPLOYEE_EMAIL: &str = "libra.prime@example.com";

#[derive(Default, World)]
#[world(init = Self::default)]
struct OfferWorld {
    journey: Option<Journey>,
    notation_id: Option<Uuid>,
    project_code: Option<String>,
    client: Option<store::persons::Person>,
}

impl std::fmt::Debug for OfferWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfferWorld")
            .field("notation_id", &self.notation_id)
            .finish_non_exhaustive()
    }
}

impl OfferWorld {
    fn journey(&self) -> &Journey {
        self.journey.as_ref().expect("journey not built")
    }
    fn client(&self) -> &store::persons::Person {
        self.client.as_ref().expect("client not resolved")
    }
    fn notation_id(&self) -> Uuid {
        self.notation_id.expect("notation not captured")
    }
    fn intake_path(&self) -> String {
        format!(
            "/app/projects/{}/intake/{}",
            self.project_code.as_deref().expect("project"),
            self.notation_id(),
        )
    }
}

#[given(regex = r#"^an offer-letter matter opened for "[^"]+" <([^>]+)>$"#)]
async fn open_matter(world: &mut OfferWorld, email: String) {
    let journey = Journey::open("offer-letter-signing").await;
    let body = format!(
        "client_email={}&retainer_template_code={TEMPLATE_CODE}",
        features::form_encode(&email),
    );
    let resp = journey.lawyer_post("/app/lawyer/retainers/new", body).await;
    let location = resp
        .location
        .unwrap_or_else(|| panic!("matter-open did not redirect (status {})", resp.status));
    let id = location
        .strip_prefix("/app/lawyer/notations/")
        .and_then(|s| s.strip_suffix("/step"))
        .unwrap_or_else(|| panic!("unexpected redirect target: {location}"));
    let notation_id = Uuid::parse_str(id).expect("notation id is a UUID");
    let notation = store::notations::find_by_id(&journey.surreal, notation_id)
        .await
        .expect("query notation")
        .expect("notation row");
    let person = store::persons::find_by_email_ci(&journey.surreal, &email)
        .await
        .expect("query person")
        .expect("matter-open created the client person");

    let project = store::projects::find_by_id(&journey.surreal, notation.project_id)
        .await
        .expect("query project")
        .expect("the matter the notation belongs to");
    world.project_code = Some(project.code);
    world.notation_id = Some(notation_id);
    world.client = Some(person);
    world.journey = Some(journey);
}

#[when("the client answers the company question:")]
async fn client_answers_company(world: &mut OfferWorld, step: &Step) {
    let table = step.table.as_ref().expect("scenario has a data table");
    let path = world.intake_path();
    let client = world.client().clone();
    for row in table.rows.iter().skip(1) {
        let value = row.first().expect("each row carries one cell").as_str();
        let resp = world
            .journey()
            .client_post(
                &client,
                &path,
                &format!("value={}", features::form_encode(value)),
            )
            .await;
        assert!(
            resp.status.is_success() || resp.status.is_redirection(),
            "client answer returned {} — body:\n{}",
            resp.status,
            resp.body,
        );
    }
}

#[when("the lawyer walks the rest of the offer questionnaire:")]
async fn lawyer_walks(world: &mut OfferWorld, step: &Step) {
    let table = step.table.as_ref().expect("scenario has a data table");
    let path = format!("/app/lawyer/notations/{}/step", world.notation_id());
    for row in table.rows.iter().skip(1) {
        let value = row.first().expect("each row carries one cell").as_str();
        // The `people_list` widget posts its row parts (`p{row}_{part}`)
        // instead of one free-typed `value` — the contacts row spells its
        // fields inline so the same table drives the whole walk.
        let body = match value.strip_prefix("contacts:") {
            Some(rest) => {
                let mut parts = rest.split(';');
                let name = parts.next().unwrap_or_default();
                let email = parts.next().unwrap_or_default();
                format!(
                    "p0_name={}&p0_email={}",
                    features::form_encode(name),
                    features::form_encode(email),
                )
            }
            None => format!("value={}", features::form_encode(value)),
        };
        let resp = world.journey().lawyer_post(&path, body).await;
        assert!(
            resp.status.is_success() || resp.status.is_redirection(),
            "walking {value:?} returned {} — body:\n{}",
            resp.status,
            resp.body,
        );
    }
}

#[when("the attorney approves and sends the document")]
async fn attorney_approves_and_sends(world: &mut OfferWorld) {
    // Approve renders + parks the PDF at generate_pdf__offer_letter_pdf;
    // the separate send dispatches the envelope once the PDF is present.
    let id = world.notation_id();
    for action in ["approve-send", "send"] {
        let resp = world
            .journey()
            .lawyer_post(
                &format!("/app/lawyer/notations/{id}/{action}"),
                String::new(),
            )
            .await;
        assert!(
            resp.status.is_success() || resp.status.is_redirection(),
            "{action} returned {} — body:\n{}",
            resp.status,
            resp.body,
        );
    }
}

#[then("the matter has a signature request")]
async fn has_signature_request(world: &mut OfferWorld) {
    assert!(
        store::signatures::request_id_for_notation(&world.journey().surreal, world.notation_id())
            .await
            .unwrap()
            .is_some(),
        "the approved offer letter must have been sent for signature"
    );
}

#[then("the envelope routes the company before the employee")]
async fn envelope_routing(world: &mut OfferWorld) {
    let calls = world.journey().signature.calls();
    assert_eq!(
        calls.len(),
        1,
        "expected exactly one envelope, got {calls:?}"
    );
    let recipients = &calls[0].manifest.recipients;
    let roles: Vec<_> = recipients.iter().map(|r| r.role.as_str()).collect();
    assert_eq!(
        roles,
        ["client", "employee"],
        "the envelope carries the template's declared signers, not the retainer default"
    );
    let client = &recipients[0];
    let employee = &recipients[1];
    assert_eq!(client.routing_order, 1, "the company signs first");
    assert_eq!(employee.routing_order, 2, "the employee signs second");
    // The company's officer signs embedded — the bound client is captive;
    // the employee is emailed, so no embedded view exists for them.
    assert!(
        client.client_user_id.is_some(),
        "the client is captive (embedded signing)"
    );
    assert!(
        employee.client_user_id.is_none(),
        "the employee is emailed, not embedded"
    );
}

#[then("the employee recipient is emailed at the bound address")]
async fn employee_email_binding(world: &mut OfferWorld) {
    let calls = world.journey().signature.calls();
    let recipients = &calls[0].manifest.recipients;
    let employee = recipients
        .iter()
        .find(|r| r.role == "employee")
        .expect("envelope has an employee recipient");
    assert_eq!(
        employee.email, BOUND_EMPLOYEE_EMAIL,
        "the employee's email resolves from the questionnaire answer the declaration binds to"
    );
}

#[tokio::main]
async fn main() {
    OfferWorld::cucumber()
        .run_and_exit("tests/features/offer_letter_signing.feature")
        .await;
}
