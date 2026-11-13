//! The frame a Project's letter is rendered in: the compiled registry's
//! letterhead for a Brand, or the Brand's runtime letterhead row read over
//! the deployment's authenticated HTTPS letterhead door. Which Brand a
//! Project wears is [`cli::letterhead::resolve_brand`]'s answer, re-exported
//! here beside the frame sources.
//!
//! The runtime row comes from one of two sources. When the signed-in
//! deployment answers, the Brand's runtime letterhead row — the same one
//! `/app/admin/brands` edits, read over HTTPS from
//! `GET /app/api/brands/{key}/letterhead` with the `site login` bearer
//! (LAW-341) — supplies the wordmark, the contact lines, the postal
//! address, the letterhead font, and the mark. When the deployment cannot
//! be asked, the compiled registry supplies the frame instead, and the
//! caller announces the fallback; see `main.rs::project_letterhead`. A
//! template re-render therefore reflects the letterhead its Brand
//! currently publishes, while executed and filed artifacts keep their
//! stored bytes (ENG-1148).
//!
//! The row read is a pinned render input (LAW-19): a successful fetch is
//! cached locally keyed on the deployment base and the Brand key, and under
//! `--ci` only that cache is read — a miss refuses rather than letting a
//! render's bytes drift between runs on a live deployment.

use std::path::PathBuf;

use store::brand_letterheads::{BrandLetterhead, LetterheadMark as StoreMark};
use views::brand::{BrandKey, DEFAULT_BRANDING};

pub use cli::letterhead::{resolve_brand, LetterheadError};

/// Cache-root override, so CI and test harnesses can pin the cache to a
/// known directory instead of the user's `XDG_CACHE_HOME`.
const CACHE_ROOT_ENV: &str = "NAVIGATOR_LETTERHEAD_CACHE";

/// The letterhead `key` wears: its display name, its Firm's legal name,
/// its production host, `contact@` that host's registrable domain, and its
/// phone. Only the default Brand draws the compiled mark, which is its own.
///
/// # Errors
/// [`LetterheadError::InvalidBrandHost`] when the compiled host is not a
/// hostname, which the registry's own tests rule out.
pub fn for_brand(key: BrandKey) -> Result<pdf::Letterhead, LetterheadError> {
    let branding = key.resolve_branding(&DEFAULT_BRANDING);
    let mut letterhead = pdf::Letterhead::from_brand(
        branding.firm.site_name,
        key.canonical_host(),
        branding.firm_phone,
        "",
    )
    .map_err(|_| LetterheadError::InvalidBrandHost(key))?;
    letterhead.entity = branding.firm.legal_entity.to_string();
    letterhead.mark = if key == BrandKey::Neon {
        pdf::LetterheadMark::Compiled
    } else {
        pdf::LetterheadMark::Wordmark
    };
    Ok(letterhead)
}

/// The letterhead for a Project declaring `brand` and `host`.
///
/// # Errors
/// [`LetterheadError`] when the host has no Brand or the declaration contradicts it.
pub fn for_project(
    brand: Option<&str>,
    host: Option<&str>,
) -> Result<pdf::Letterhead, LetterheadError> {
    for_brand(resolve_brand(brand, host)?)
}

/// The frame read from the deployment's stores, with the provenance a
/// render reports beside it.
pub struct RuntimeLetterhead {
    /// The frame the letter wears.
    pub letterhead: pdf::Letterhead,
    /// The stored row's `updated_at` — the revision the render used.
    pub revision: Option<String>,
    /// The letterhead font's uploaded object, named by the provenance line.
    pub font_object_key: Option<String>,
    /// Non-fatal degradations a render announces on stderr: a mark asked
    /// for but unfetchable prints the wordmark, loudly.
    pub warnings: Vec<String>,
}

/// The runtime letterhead `key` wears, read from the signed-in deployment's
/// letterhead door — the same rows `/app/admin/brands` edits, served by
/// `GET /app/api/brands/{key}/letterhead` (LAW-341). `None` when the
/// deployment knows no such Brand, so the caller falls back to the compiled
/// registry and says so.
///
/// The mark follows the row: `wordmark` prints the admin's wordmark, and
/// `logo` draws the Brand's uploaded mark, fetched from the deployment's
/// public assets route at `api_base`. A logo the fetch cannot produce
/// degrades to the wordmark with a warning — never a refusal, and never
/// the compiled mark, which belongs to the default Brand alone.
///
/// # Errors
/// [`LetterheadError::Store`] when the row fetch fails — transport, status,
/// parse, or a `--ci` cache miss; the caller decides between refusing and
/// falling back.
pub async fn for_brand_runtime(
    key: BrandKey,
    api_base: &str,
    token: Option<&str>,
    ci: bool,
) -> Result<Option<RuntimeLetterhead>, LetterheadError> {
    let Some(resolved) = fetch_row(api_base, token, key.as_str(), ci)
        .await
        .map_err(LetterheadError::Store)?
    else {
        return Ok(None);
    };
    let (mark, warnings) = match resolved.mark {
        StoreMark::Wordmark => (pdf::LetterheadMark::Wordmark, Vec::new()),
        StoreMark::Logo => match fetch_png(api_base, resolved.logo_object_key.as_deref()).await {
            Ok(bytes) => (pdf::LetterheadMark::Png(bytes), Vec::new()),
            Err(reason) => (
                pdf::LetterheadMark::Wordmark,
                vec![format!(
                    "the Brand's uploaded mark could not be drawn ({reason}); the letter \
                     opens on its wordmark"
                )],
            ),
        },
    };
    Ok(Some(RuntimeLetterhead {
        revision: resolved.updated_at.clone(),
        font_object_key: resolved.font_object_key.clone(),
        letterhead: frame_from_row(&resolved, mark),
        warnings,
    }))
}

/// The resolved row behind `GET {api_base}/app/api/brands/{key}/letterhead`,
/// or `None` for a door that answers 404 — the deployment knows no such
/// Brand. The raw JSON is cached on success, and `--ci` reads the cache
/// alone (LAW-19): a letter render's frame must not drift between runs on
/// a live deployment, so pinned bytes or a refusal — never a silent fresh
/// read.
pub async fn fetch_row(
    api_base: &str,
    token: Option<&str>,
    key: &str,
    ci: bool,
) -> Result<Option<BrandLetterhead>, String> {
    if ci {
        let cached = cache_get(api_base, key).ok_or_else(|| {
            format!(
                "--ci letterhead cache miss for brand `{key}`: render once without --ci \
                 to warm the cache from the deployment, then re-render"
            )
        })?;
        return serde_json::from_str(&cached)
            .map_err(|error| format!("parsing the cached letterhead row for `{key}`: {error}"));
    }
    let url = format!(
        "{}/app/api/brands/{}/letterhead",
        api_base.trim_end_matches('/'),
        key
    );
    let mut request = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(10));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("GET {url}: {error}"))?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(format!("GET {url}: {status}"));
    }
    let body = response
        .text()
        .await
        .map_err(|error| format!("GET {url}: {error}"))?;
    let row: BrandLetterhead = serde_json::from_str(&body).map_err(|error| {
        format!(
            "GET {url}: the letterhead door answered a body that is not a resolved \
             letterhead row: {error}"
        )
    })?;
    cache_put(api_base, key, &body)?;
    Ok(Some(row))
}

/// The frame a resolved runtime row maps onto: every render line is the
/// row's own resolved line, and the mark is the row's choice. The door's
/// resolve has already applied its fallbacks (wordmark → the Brand's
/// name, mailbox → `contact@` the site's registrable domain); the frame
/// carries them through untouched.
#[must_use]
pub fn frame_from_row(resolved: &BrandLetterhead, mark: pdf::LetterheadMark) -> pdf::Letterhead {
    pdf::Letterhead {
        name: resolved.wordmark.clone(),
        entity: resolved.firm_legal_name.clone().unwrap_or_default(),
        phone: resolved.phone.clone().unwrap_or_default(),
        email: resolved.email.clone().unwrap_or_default(),
        web: resolved.web.clone().unwrap_or_default(),
        postal_address: resolved.postal_address.clone().unwrap_or_default(),
        font_family: resolved.font_family.clone().unwrap_or_default(),
        mark,
    }
}

/// The cache key: the deployment base the row was read from plus the Brand
/// key. A row re-pointed at a different deployment, or read for another
/// Brand, misses and re-reads rather than serving another Brand's frame.
fn cache_path(api_base: &str, key: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(CACHE_ROOT_ENV).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path).join(cache_key(api_base, key)));
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(
        base.join("navigator")
            .join("letterhead")
            .join(cache_key(api_base, key)),
    )
}

fn cache_key(api_base: &str, key: &str) -> PathBuf {
    PathBuf::from(format!(
        "{}.json",
        store::assets::sha256_hex(format!("{api_base}\n{key}").as_bytes())
    ))
}

fn cache_get(api_base: &str, key: &str) -> Option<String> {
    let path = cache_path(api_base, key)?;
    std::fs::read_to_string(path).ok()
}

fn cache_put(api_base: &str, key: &str, body: &str) -> Result<(), String> {
    let Some(path) = cache_path(api_base, key) else {
        // No HOME and no override: the row is used un-cached rather than
        // refusing a render over a cache that cannot exist.
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    std::fs::write(&path, body).map_err(|error| format!("write {}: {error}", path.display()))
}

/// A Brand's uploaded mark, fetched from the deployment's public assets
/// route — the same route the site itself draws the mark from. The message
/// names the object so a missing upload is diagnosable from the warning.
async fn fetch_png(base: &str, object_key: Option<&str>) -> Result<Vec<u8>, String> {
    let Some(key) = object_key.map(str::trim).filter(|key| !key.is_empty()) else {
        return Err("the Brand has no uploaded mark object".to_string());
    };
    let url = format!("{}/assets/{key}", base.trim_end_matches('/'));
    let response = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|error| format!("GET {url}: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("GET {url}: {status}"));
    }
    response
        .bytes()
        .await
        .map(Into::into)
        .map_err(|error| format!("GET {url}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{for_brand, for_project, LetterheadError};
    use views::brand::BrandKey;

    #[test]
    fn an_unknown_brand_host_or_neither_is_refused() {
        assert_eq!(
            for_project(Some("sample-brand"), None),
            Err(LetterheadError::UnknownBrand("sample-brand".to_string()))
        );
        let refused = for_project(None, Some("navigator.example.test")).unwrap_err();
        assert!(refused.to_string().contains("navigator.example.test"));
        assert_eq!(
            for_project(Some(" "), Some("")),
            Err(LetterheadError::Undeclared)
        );
    }

    #[test]
    fn only_the_default_brand_draws_the_compiled_mark() {
        for key in BrandKey::ALL.iter().copied() {
            let letterhead = for_brand(key).expect("every compiled Brand has a letterhead");
            let expected_mark = if key == BrandKey::Neon {
                pdf::LetterheadMark::Compiled
            } else {
                pdf::LetterheadMark::Wordmark
            };
            assert_eq!(letterhead.mark, expected_mark, "{}", key.as_str());
            assert!(letterhead.email.starts_with("contact@"));
            assert!(
                !letterhead.entity.is_empty(),
                "{} names its Firm",
                key.as_str()
            );
        }
    }

    #[tokio::test]
    async fn the_mark_fetch_serves_the_uploaded_bytes() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/assets/brands/vesta/mark.png"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_bytes(b"png-bytes".to_vec()),
            )
            .mount(&server)
            .await;
        let bytes = super::fetch_png(&server.uri(), Some("brands/vesta/mark.png"))
            .await
            .expect("the mark fetches");
        assert_eq!(bytes, b"png-bytes");
    }

    #[tokio::test]
    async fn the_mark_fetch_names_the_url_on_a_miss() {
        let server = wiremock::MockServer::start().await;
        let reason = super::fetch_png(&server.uri(), Some("brands/vesta/mark.png"))
            .await
            .expect_err("a miss is an error");
        assert!(reason.contains("/assets/brands/vesta/mark.png"), "{reason}");
        assert!(reason.contains("404"), "{reason}");
    }

    #[tokio::test]
    async fn the_mark_fetch_refuses_an_unset_object() {
        assert!(super::fetch_png("http://127.0.0.1:1", None).await.is_err());
    }

    /// A row in the exact shape the door answers — the admin-set fields
    /// plus the resolve's revision stamp.
    fn served_row() -> String {
        serde_json::json!({
            "brand_key": "vesta",
            "wordmark": "Vesta Estate Planning",
            "mark": "wordmark",
            "updated_at": "2026-11-05T22:41:19+00:00"
        })
        .to_string()
    }

    #[tokio::test]
    async fn the_row_fetch_sends_the_bearer_and_parses_the_row() {
        let server = wiremock::MockServer::start().await;
        // The row mock matches only a request carrying the site-login
        // bearer; the catch-all 500 mounted after it catches every other
        // shape, so a missing or wrong header fails the fetch rather than
        // passing the test.
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/app/api/brands/vesta/letterhead"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer site-token",
            ))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(served_row()))
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let row = super::fetch_row(&server.uri(), Some("site-token"), "vesta", false)
            .await
            .expect("the door answers");
        let row = row.expect("the double serves a row");
        assert_eq!(row.wordmark, "Vesta Estate Planning");
        assert_eq!(row.updated_at.as_deref(), Some("2026-11-05T22:41:19+00:00"));
    }

    #[tokio::test]
    async fn a_missing_brand_answers_none() {
        let server = wiremock::MockServer::start().await;
        // No mock mounted: the double answers 404, the door's own
        // unknown-brand answer.
        let row = super::fetch_row(&server.uri(), None, "ghost", false)
            .await
            .expect("a 404 door is an answer, not a failure");
        assert!(row.is_none());
    }

    #[tokio::test]
    async fn a_failing_door_names_the_url_and_status() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/app/api/brands/vesta/letterhead"))
            .respond_with(wiremock::ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let reason = super::fetch_row(&server.uri(), None, "vesta", false)
            .await
            .expect_err("a 500 door is a failure");
        assert!(
            reason.contains("/app/api/brands/vesta/letterhead"),
            "{reason}"
        );
        assert!(reason.contains("500"), "{reason}");
    }

    #[tokio::test]
    async fn a_malformed_row_is_a_refusal_that_names_the_door() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/app/api/brands/vesta/letterhead"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("<html/>"))
            .mount(&server)
            .await;
        let reason = super::fetch_row(&server.uri(), None, "vesta", false)
            .await
            .expect_err("a non-row body is a refusal");
        assert!(reason.contains("not a resolved letterhead row"), "{reason}");
    }
}
