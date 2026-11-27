//! Public testimonial reads and the replay-safe canonical seeder seam.

use crate::postgres::matter;
use serde_json::json;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::persons::{self, PersonError};
use crate::projects;

/// A testimonial row returned to an authenticated matter surface.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct Testimonial {
    pub id: Uuid,
    pub project_id: Uuid,
    pub person_id: Uuid,
    pub quote: String,
    pub attribution_label: Option<String>,
    pub consented_at: Option<String>,
    pub published_at: Option<String>,
    pub display_order: i32,
}

/// The homepage projection of a published testimonial. Identity stays on the
/// private row: public output is the quote plus the client's chosen
/// attribution, and nothing inferred from the Person record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedTestimonial {
    pub id: Uuid,
    pub quote: String,
    pub attribution_label: Option<String>,
}

#[derive(Clone, Debug)]
pub struct NewTestimonial<'a> {
    pub project_id: Uuid,
    pub person_id: Uuid,
    pub quote: &'a str,
    pub attribution_label: Option<String>,
    pub consented_at: Option<String>,
    pub published_at: Option<String>,
    pub display_order: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum TestimonialError {
    #[error(transparent)]
    Postgres(#[from] matter::MatterAuthorityError),
    #[error("resolve the testimonial senders: {0}")]
    Person(#[from] PersonError),
    #[error(transparent)]
    Project(#[from] projects::ProjectStoreError),
    #[error("writing a testimonial returned no usable row")]
    WriteReturnedNothing,
    #[error("the caller is not authorized to change this testimonial")]
    NotAuthorized,
    #[error("testimonial not found")]
    NotFound,
    #[error("a testimonial must have public consent before publication")]
    NotConsented,
}

/// The client-side fields submitted by the accountable client. The project and
/// person are deliberately not part of this input: both come from the route
/// and authenticated session, respectively.
#[derive(Clone, Debug)]
pub struct TestimonialSubmission<'a> {
    pub quote: &'a str,
    pub attribution_label: Option<String>,
    pub request_public: bool,
}

/// Read the current testimonial for one person on one project.
pub async fn for_person_project(
    _application_store: &ApplicationStore,
    person_id: Uuid,
    project_id: Uuid,
) -> Result<Option<Testimonial>, TestimonialError> {
    let pg = matter::authority_for("testimonial").await?;
    Ok(matter::select(
        &pg,
        "testimonial",
        json!({"project_id": project_id, "person_id": person_id}),
        &["updated_at DESC"],
    )
    .await?
    .into_iter()
    .next())
}

/// Create or edit the testimonial owned by the project's client DRI.
///
/// The participation row is the authorization boundary. A client request can
/// grant consent, but it can never publish: every client write clears
/// `published_at`, requiring a fresh lawyer/admin approval after an edit.
pub async fn save_for_client_dri(
    application_store: &ApplicationStore,
    person_id: Uuid,
    project_id: Uuid,
    input: &TestimonialSubmission<'_>,
) -> Result<Testimonial, TestimonialError> {
    let Some(participation) =
        projects::participation_for_person(application_store, person_id, project_id).await?
    else {
        return Err(TestimonialError::NotAuthorized);
    };
    if !participation.is_client_dri
        || !projects::PARTICIPATION_CLIENT_SIDE.contains(&participation.participation.as_str())
    {
        return Err(TestimonialError::NotAuthorized);
    }
    let quote = input.quote.trim();
    if quote.is_empty() {
        return Err(TestimonialError::WriteReturnedNothing);
    }
    let attribution_label = input
        .attribution_label
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let existing = for_person_project(application_store, person_id, project_id).await?;
    let id = existing
        .as_ref()
        .map_or_else(Uuid::now_v7, |testimonial| testimonial.id);
    let now = chrono::Utc::now().to_rfc3339();
    let consented_at = input.request_public.then(|| now.clone());
    let pg = matter::authority_for("testimonial").await?;
    if existing.is_some() {
        matter::update(
            &pg,
            "testimonial",
            json!({"id": id}),
            json!({"quote": quote,
                "attribution_label": attribution_label, "consented_at": consented_at,
                "published_at": null, "updated_at": now}),
        )
        .await?
        .into_iter()
        .next()
        .ok_or(TestimonialError::WriteReturnedNothing)
    } else {
        Ok(matter::upsert(
            &pg,
            "testimonial",
            &json!({"id": id, "project_id": project_id,
                "person_id": person_id, "quote": quote, "attribution_label": attribution_label,
                "consented_at": consented_at, "published_at": null, "display_order": 0,
                "inserted_at": now, "updated_at": now}),
        )
        .await?)
    }
}

async fn firm_actor_can_publish(
    application_store: &ApplicationStore,
    actor_person_id: Option<Uuid>,
    actor_role: persons::Role,
    project_id: Uuid,
) -> Result<bool, TestimonialError> {
    if !actor_role.is_lawyer_tier() {
        return Ok(false);
    }
    let Some(actor_person_id) = actor_person_id else {
        return Ok(false);
    };
    Ok(
        projects::participation_for_person(application_store, actor_person_id, project_id)
            .await?
            .is_some_and(|row| {
                !projects::PARTICIPATION_CLIENT_SIDE.contains(&row.participation.as_str())
            }),
    )
}

/// Approve a consented testimonial for the public homepage.
pub async fn publish(
    application_store: &ApplicationStore,
    actor_person_id: Option<Uuid>,
    actor_role: persons::Role,
    testimonial_id: Uuid,
) -> Result<Testimonial, TestimonialError> {
    let Some(current) = find_by_id(application_store, testimonial_id).await? else {
        return Err(TestimonialError::NotFound);
    };
    if !firm_actor_can_publish(
        application_store,
        actor_person_id,
        actor_role,
        current.project_id,
    )
    .await?
    {
        return Err(TestimonialError::NotAuthorized);
    }
    if current.consented_at.is_none() {
        return Err(TestimonialError::NotConsented);
    }
    update_publication(
        application_store,
        testimonial_id,
        Some(chrono::Utc::now().to_rfc3339()),
    )
    .await
}

/// Remove a testimonial from the public homepage while preserving the client's
/// consent record for a later re-approval.
pub async fn unpublish(
    application_store: &ApplicationStore,
    actor_person_id: Option<Uuid>,
    actor_role: persons::Role,
    testimonial_id: Uuid,
) -> Result<Testimonial, TestimonialError> {
    let Some(current) = find_by_id(application_store, testimonial_id).await? else {
        return Err(TestimonialError::NotFound);
    };
    if !firm_actor_can_publish(
        application_store,
        actor_person_id,
        actor_role,
        current.project_id,
    )
    .await?
    {
        return Err(TestimonialError::NotAuthorized);
    }
    update_publication(application_store, testimonial_id, None).await
}

async fn find_by_id(
    _application_store: &ApplicationStore,
    testimonial_id: Uuid,
) -> Result<Option<Testimonial>, TestimonialError> {
    let pg = matter::authority_for("testimonial").await?;
    Ok(matter::find(&pg, "testimonial", json!({"id": testimonial_id})).await?)
}

async fn update_publication(
    _application_store: &ApplicationStore,
    testimonial_id: Uuid,
    published_at: Option<String>,
) -> Result<Testimonial, TestimonialError> {
    let pg = matter::authority_for("testimonial").await?;
    let value: Option<serde_json::Value> = sqlx::query_scalar("UPDATE testimonial AS t SET published_at = $2, updated_at = now() WHERE id = $1 AND ($2::timestamptz IS NULL OR consented_at IS NOT NULL) RETURNING to_jsonb(t)")
            .bind(testimonial_id).bind(published_at.as_deref().map(chrono::DateTime::parse_from_rfc3339).transpose().map_err(|_| TestimonialError::WriteReturnedNothing)?)
            .fetch_optional(pg.pool()).await.map_err(matter::MatterAuthorityError::from)?;
    value
        .map(serde_json::from_value)
        .transpose()
        .map_err(matter::MatterAuthorityError::from)?
        .ok_or(TestimonialError::NotConsented)
}

/// Create the testimonial identified by its natural seed key, or return the
/// existing row after a competing canonical seed won its unique index race.
pub async fn find_or_create(
    application_store: &ApplicationStore,
    input: &NewTestimonial<'_>,
) -> Result<(), TestimonialError> {
    let pg = matter::authority_for("testimonial").await?;
    let filter =
        json!({"project_id": input.project_id, "person_id": input.person_id, "quote": input.quote});
    if matter::find::<Testimonial>(&pg, "testimonial", filter.clone())
        .await?
        .is_some()
    {
        return Ok(());
    }
    if projects::find_by_id(application_store, input.project_id)
        .await?
        .is_none()
    {
        return Err(projects::ProjectStoreError::NoSuchProject(input.project_id).into());
    }
    let now = chrono::Utc::now();
    let result = matter::upsert::<Testimonial>(&pg, "testimonial", &json!({"id": Uuid::now_v7(),
            "project_id": input.project_id, "person_id": input.person_id, "quote": input.quote,
            "attribution_label": input.attribution_label, "consented_at": input.consented_at,
            "published_at": input.published_at, "display_order": input.display_order, "inserted_at": now, "updated_at": now})).await;
    match result {
        Ok(_) => Ok(()),
        Err(matter::MatterAuthorityError::Conflict(_)) => {
            matter::find::<Testimonial>(&pg, "testimonial", filter)
                .await?
                .ok_or(TestimonialError::WriteReturnedNothing)
                .map(|_| ())
        }
        Err(error) => Err(error.into()),
    }
}

/// Published testimonials for the homepage, ordered exactly as the former SQL
/// read: display order then the lawyer publication timestamp.
pub async fn published_for_home(
    _application_store: &ApplicationStore,
    limit: u64,
) -> Result<Vec<PublishedTestimonial>, TestimonialError> {
    let pg = matter::authority_for("testimonial").await?;
    matter::published_testimonials(&pg, None, Some(limit))
        .await
        .map_err(TestimonialError::from)
}

/// Published testimonials for one house brand, ordered exactly as the public
/// home and testimonials pages display them.
/// An absent limit includes every eligible testimonial for the brand.
pub async fn published_for_brand(
    _application_store: &ApplicationStore,
    brand: &str,
    limit: Option<u64>,
) -> Result<Vec<PublishedTestimonial>, TestimonialError> {
    let pg = matter::authority_for("testimonial").await?;
    matter::published_testimonials(&pg, Some(brand), limit)
        .await
        .map_err(TestimonialError::from)
}

#[cfg(test)]
#[path = "testimonials/native_tests.rs"]
mod native_tests;

#[cfg(test)]
#[path = "testimonials/compatibility_tests.rs"]
mod compatibility_tests;
