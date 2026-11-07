//! The JSON wire shape of every collection read: the request parsed by
//! [`store::listing::ListParams`] and one page answered as
//! `{ "data": [...], "links": { "self", "next", "prev" }, "meta": { "page": { "size" } } }`.
//! `next` and `prev` are absent at either end of the list.

use axum::Json;
use serde::Serialize;
use store::listing::{ListParams, ListSpec, Page};

use crate::api::ApiError;

/// Parse a collection request's raw query string against its spec.
///
/// # Errors
///
/// [`ApiError::ListInvalid`] (a 400) for an unadvertised filter or sort
/// field, an out-of-range `page[size]`, or a stale cursor.
pub fn parse(raw: Option<&str>, spec: &ListSpec) -> Result<ListParams, ApiError> {
    ListParams::parse(raw.unwrap_or(""), spec).map_err(ApiError::ListInvalid)
}

/// The page as a JSON:API-shaped document whose links repeat the request's
/// filters, sort, and size.
pub fn document<T: Serialize>(
    path: &str,
    params: &ListParams,
    page: &Page<T>,
) -> Json<serde_json::Value> {
    let link = |cursor: Option<(&str, &str)>| {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        for (member, value) in &params.filters {
            query.append_pair(&format!("filter[{member}]"), value);
        }
        query.append_pair("sort", &params.sort_param());
        query.append_pair("page[size]", &params.size.to_string());
        if let Some((member, value)) = cursor {
            query.append_pair(member, value);
        }
        format!("{path}?{}", query.finish())
    };
    let mut links = serde_json::Map::new();
    links.insert("self".into(), link(None).into());
    if let Some(next) = &page.next {
        links.insert("next".into(), link(Some(("page[after]", next))).into());
    }
    if let Some(prev) = &page.prev {
        links.insert("prev".into(), link(Some(("page[before]", prev))).into());
    }
    Json(serde_json::json!({
        "data": &page.items,
        "links": links,
        "meta": { "page": { "size": params.size } },
    }))
}
