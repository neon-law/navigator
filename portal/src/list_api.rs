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

#[cfg(test)]
mod tests {
    use super::{document, parse};
    use crate::api::ApiError;
    use store::listing::{ListError, ListSpec, Page};

    const SPEC: ListSpec = ListSpec {
        filters: &["name"],
        sorts: &["name", "email"],
        default_sort: &[("name", false)],
    };

    #[test]
    fn an_unadvertised_filter_or_an_out_of_range_page_is_refused() {
        for (query, expected) in [
            (
                "filter[role]=owner",
                ListError::UnknownFilter("role".into()),
            ),
            ("sort=role", ListError::UnknownSort("role".into())),
            ("page[size]=0", ListError::PageSize),
            ("page[after]=stale", ListError::Cursor),
        ] {
            match parse(Some(query), &SPEC) {
                Err(ApiError::ListInvalid(error)) => assert_eq!(error, expected, "{query}"),
                other => panic!("{query} must be refused, got {:?}", other.map(|_| ())),
            }
        }
        assert!(parse(None, &SPEC).unwrap().filters.is_empty());
    }

    #[test]
    fn every_link_repeats_the_request_and_only_existing_neighbours_are_linked() {
        let params = parse(Some("filter[name]=Ada+L&sort=-email&page[size]=2"), &SPEC).unwrap();
        let middle = Page {
            items: vec!["a", "b"],
            next: Some("n1".into()),
            prev: Some("p1".into()),
        };
        let body = document("/app/api/people", &params, &middle).0;
        let repeated = "/app/api/people?filter%5Bname%5D=Ada+L&sort=-email&page%5Bsize%5D=2";
        assert_eq!(body["data"], serde_json::json!(["a", "b"]));
        assert_eq!(body["meta"]["page"]["size"], 2);
        assert_eq!(body["links"]["self"], repeated);
        assert_eq!(
            body["links"]["next"],
            format!("{repeated}&page%5Bafter%5D=n1")
        );
        assert_eq!(
            body["links"]["prev"],
            format!("{repeated}&page%5Bbefore%5D=p1")
        );

        let only = Page::<&str> {
            items: Vec::new(),
            next: None,
            prev: None,
        };
        let links = &document("/app/api/people", &params, &only).0["links"];
        assert_eq!(links.as_object().unwrap().len(), 1, "{links}");
    }
}
