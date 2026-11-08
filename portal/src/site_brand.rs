//! The serving site's brand, as JSON, for Project portals.
//!
//! `GET /site-brand.json` answers with the brand the request's `Host:` resolved
//! to (the same [`views::brand::BrandKey`] the rest of the site scopes), so a
//! portal mounted on `daybridgedivorce.com` wears that site's identity and one
//! on `www.neonlaw.com` wears Neon Law's. The portal is branded "Navigator" plus
//! this brand; the palette arrives as the `tokens_href` stylesheet, never as a
//! colour repainted inside the portal.

use axum::http::header::{HeaderValue, CACHE_CONTROL, CONTENT_TYPE, VARY};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// The line every portal footer carries after the host's own legal line.
pub const PLATFORM_LINE: &str = "Powered by Navigator";

/// Identity of the site serving the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteBrand {
    pub key: String,
    pub name: String,
    pub logo_href: String,
    pub tokens_href: String,
    pub legal: String,
    pub home_href: String,
    pub platform: &'static str,
}

/// Read the brand scoped to the current request.
#[must_use]
pub fn current() -> SiteBrand {
    let brand = &views::brand::FIRM_BRAND;
    let key = views::brand::brand_key();
    SiteBrand {
        key: key.as_str().to_string(),
        name: brand.site_name.to_string(),
        logo_href: brand.logo_href.to_string(),
        tokens_href: webapp::brand_style::brand_tokens_href(key.as_str()),
        legal: brand.legal_entity.to_string(),
        home_href: brand.home_href.to_string(),
        platform: PLATFORM_LINE,
    }
}

/// `GET /site-brand.json`.
pub async fn site_brand() -> Response {
    let mut response = serde_json::to_string(&current())
        .unwrap_or_else(|_| "{}".to_string())
        .into_response();
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    // The answer depends on the host, so a shared cache must key on it.
    headers.insert(VARY, HeaderValue::from_static("Host"));
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use views::brand::{scope, DAYBRIDGE_BRANDING, DEFAULT_BRANDING};

    #[tokio::test]
    async fn daybridge_host_shows_its_own_brand() {
        let brand = scope(&DAYBRIDGE_BRANDING, async { current() }).await;
        assert_eq!(brand.key, "daybridge");
        assert_eq!(brand.name, "Daybridge Divorce Law");
        assert_eq!(brand.tokens_href, "/public/css/brand-daybridge-tokens.css");
        assert_eq!(brand.platform, "Powered by Navigator");
    }

    #[tokio::test]
    async fn neon_law_host_shows_neon_law() {
        let brand = scope(&DEFAULT_BRANDING, async { current() }).await;
        assert_eq!(brand.key, "neon");
        assert_eq!(brand.name, DEFAULT_BRANDING.firm.site_name);
        assert_eq!(brand.tokens_href, "/public/css/brand-neon-tokens.css");
        assert_ne!(brand.name, "Daybridge Divorce Law");
    }

    #[tokio::test]
    async fn the_route_serves_json_keyed_on_host() {
        let response = scope(&DAYBRIDGE_BRANDING, site_brand()).await;
        assert_eq!(response.headers()[VARY], "Host");
        assert_eq!(
            response.headers()[CONTENT_TYPE],
            "application/json; charset=utf-8"
        );
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["name"], "Daybridge Divorce Law");
        assert_eq!(json["tokensHref"], "/public/css/brand-daybridge-tokens.css");
    }
}
