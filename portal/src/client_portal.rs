//! Client access to Project pages on the serving Brand.

use axum::{
    extract::{MatchedPath, Path, Request},
    http::StatusCode,
    RequestExt,
};
use store::{access::ClientPortal, application_store::ApplicationStore, projects};

pub(crate) fn access() -> ClientPortal<'static> {
    ClientPortal {
        brand: views::brand::brand_key().as_str(),
    }
}

pub(crate) use views::brand::project_origin;

/// The Firm theme may already have been resolved by the outer request layer.
pub(crate) async fn apply_host_mark(request: &mut Request) {
    use webapp::app_chrome::{resolve_app_brand_mark, AppBrandMark};
    let host = views::brand::brand_key();
    let tokens = webapp::brand_style::brand_tokens_href(host.as_str());
    if request
        .extensions()
        .get::<AppBrandMark>()
        .is_some_and(|mark| mark.tokens_href != tokens)
    {
        if let Some(db) = request.extensions().get::<ApplicationStore>() {
            let mark = resolve_app_brand_mark(db, host).await;
            request.extensions_mut().insert(mark);
        }
    }
}

/// All reads and writes beneath a Project code share this boundary after route admission.
/// The retired portal redirect only echoes the caller's own path and reads no Project.
pub(crate) async fn admits_page(request: &mut Request) -> Result<bool, StatusCode> {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or("", MatchedPath::as_str);
    if !route.starts_with("/app/projects/{")
        || route.starts_with(crate::project_portal::PROJECT_PORTAL_PATH)
    {
        return Ok(true);
    }
    let Path(params) = request
        .extract_parts::<Path<Vec<String>>>()
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let code = params.first().ok_or(StatusCode::BAD_REQUEST)?;
    let db = request
        .extensions()
        .get::<ApplicationStore>()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let project = projects::find_by_code(db, code)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let Some(project) = project.filter(|project| access().contains(project)) else {
        return Ok(false);
    };
    let person = request
        .extensions()
        .get::<crate::SessionData>()
        .and_then(|session| session.person_id);
    store::access::can_see_project_as_client(db, person, project.id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[cfg(test)]
mod tests {
    use super::project_origin;

    #[test]
    fn client_links_use_the_project_host_in_the_same_deployment() {
        for (base, expected) in [
            (
                "https://www.neonlaw.com",
                Some("https://www.deleteyourdata.com"),
            ),
            (
                "https://staging.neonlaw.com",
                Some("https://staging.deleteyourdata.com"),
            ),
            ("https://portal.example.org/", None),
        ] {
            assert_eq!(
                project_origin("delete-your-data", base).as_deref(),
                expected
            );
        }
    }
}
