//! Injectable HTTP fetch — the only network seam a site adapter's parsing
//! touches. Adapters receive a `&dyn HttpFetcher` and never open a connection
//! themselves, so every parse path runs against a recorded body in tests and
//! against the portal in production.

/// The maximum response body the fetcher will buffer. Court notices and
/// their PDFs are small; a response this large is a wrong link, not a
/// document, and buffering it anyway would only make the mistake expensive.
// Adapters pass response sizes through `fetch_into`'s guard.
#[cfg_attr(not(test), allow(dead_code))]
const MAX_RESPONSE_BYTES: usize = 256 * 1024 * 1024;

/// One HTTP GET an adapter needs: the URL, optional request headers (the
/// portal's JSON endpoints key off a `Provider` header, for one), and, when
/// resuming a partial download, the byte offset the server should answer
/// from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))] // adapters build these; the base registers none yet
pub(crate) struct FetchRequest {
    pub url: String,
    /// `Range: bytes=<start>-`. `None` on a first fetch.
    pub range_start: Option<u64>,
    /// Request headers, applied verbatim. A `BTreeMap` so a request's
    /// serialization — and any test record of it — is deterministic.
    #[cfg_attr(not(test), allow(dead_code))] // the adapter sets the Provider header
    pub headers: std::collections::BTreeMap<String, String>,
}

/// A completed fetch: the status the server answered with and its body.
///
/// The status is handed to the adapter rather than folded into an error so
/// the adapter — which knows the portal — decides that a `5xx` is transient
/// while a `404` means the flow moved.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))] // adapters receive these from `fetch`
pub(crate) struct FetchResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Why a fetch could not be completed. The URL is safe to name; bodies are
/// never carried.
#[derive(Debug, thiserror::Error)]
pub(crate) enum FetchError {
    #[error("the HTTP fetch failed")]
    Request(#[from] reqwest::Error),
    #[error("the response body from {url} exceeded {limit} bytes")]
    #[cfg_attr(not(test), allow(dead_code))] // `fetch_into` raises this past the size bound
    TooLarge { url: String, limit: usize },
}

/// The fetch seam adapters parse through.
#[async_trait::async_trait]
pub(crate) trait HttpFetcher: Send + Sync {
    #[cfg_attr(not(test), allow(dead_code))] // adapters fetch through this seam
    async fn fetch(&self, request: &FetchRequest) -> Result<FetchResponse, FetchError>;
}

/// The reqwest-backed fetcher production adapters use: a single client with
/// a bounded request timeout, honoring `Range` when the caller asks.
pub(crate) struct HttpFetcherOverReqwest {
    #[cfg_attr(not(test), allow(dead_code))] // read by the fetch implementation
    client: reqwest::Client,
}

impl HttpFetcherOverReqwest {
    /// # Errors
    ///
    /// When the underlying client cannot be built.
    pub(crate) fn new() -> Result<Self, FetchError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(FetchError::Request)?;
        Ok(Self { client })
    }
}

#[async_trait::async_trait]
impl HttpFetcher for HttpFetcherOverReqwest {
    async fn fetch(&self, request: &FetchRequest) -> Result<FetchResponse, FetchError> {
        let mut builder = self.client.get(&request.url);
        if let Some(start) = request.range_start {
            builder = builder.header(reqwest::header::RANGE, format!("bytes={start}-"));
        }
        for (name, value) in &request.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        let response = builder.send().await.map_err(FetchError::Request)?;
        let status = response.status().as_u16();
        // Bound the buffered body: refuse early when the server declares a
        // length over the cap, and again after reading in case it did not.
        if let Some(declared) = response.content_length() {
            if declared > MAX_RESPONSE_BYTES as u64 {
                return Err(FetchError::TooLarge {
                    url: request.url.clone(),
                    limit: MAX_RESPONSE_BYTES,
                });
            }
        }
        let body = response.bytes().await.map_err(FetchError::Request)?;
        if body.len() > MAX_RESPONSE_BYTES {
            return Err(FetchError::TooLarge {
                url: request.url.clone(),
                limit: MAX_RESPONSE_BYTES,
            });
        }
        Ok(FetchResponse {
            status,
            body: body.to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::HttpFetcherOverReqwest;

    #[test]
    fn the_reqwest_fetcher_builds_without_network() {
        assert!(HttpFetcherOverReqwest::new().is_ok());
    }
}
