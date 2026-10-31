//! Read a claims agent's notice — a [Claims Agent](#) mailing from Kroll,
//! Epiq, Stretto, or Omni — beside the court's
//! [`nef`](super::nef) module.
//!
//! A bankruptcy's claims-and-noticing agent mails its own notices, and the
//! documents behind its links sit on the agent's site, not on CM/ECF. Those
//! sites answer a scripted client with a block page (a `CloudFront` 403 that
//! is 919 bytes of HTML wearing a `.pdf` name), so every download is
//! verified against the `%PDF` magic bytes before anything is filed, and a
//! blocked link falls back to the same entry's CM/ECF free look before a
//! person is asked to download it by hand.
//!
//! Like [`super::nef`], this module is pure: bytes in, a
//! [`ClaimsNotice`] out. It opens no connection and touches no file.

use anyhow::{Context, Result};

/// The claims-and-noticing vendors Navigator reads. A notice from any other
/// sender is not a claims notice. Add a vendor's domain as its mail is seen.
const VENDOR_DOMAINS: &[&str] = &[
    "kroll.com",
    "epiq11.com",
    "epiq.com",
    "stretto.com",
    "omniagentsolutions.com",
];

/// The token every vendor's document link has carried so far. Kroll's is
/// `…/Home-DownloadPDF?id1=…`; extend the check, not the caller, as other
/// vendors are seen.
const DOWNLOAD_TOKEN: &str = "downloadpdf";

/// A claims agent's notice: the bankruptcy case it names, and every document
/// link it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimsNotice {
    /// The case number as the notice prints it, judge initials included.
    pub case_no: Option<String>,
    pub links: Vec<ClaimLink>,
}

/// One document link in a claims notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimLink {
    pub url: String,
    /// The anchor text, which names the document and usually its Docket
    /// Number. Empty when a plain-text notice carries the bare URL.
    pub label: String,
}

/// Parse one `.eml` message. `None` when the sender is not a claims agent or
/// the message carries no document link — a [`super::nef`] notice parses
/// there instead.
#[must_use]
pub fn parse(bytes: &[u8]) -> Option<ClaimsNotice> {
    let message = mail_parser::MessageParser::default().parse(bytes)?;
    let from = message
        .from()
        .and_then(|addresses| addresses.first())
        .and_then(mail_parser::Addr::address)
        .map(str::to_ascii_lowercase)?;
    if !is_claims_agent_sender(&from) {
        return None;
    }
    if let Some(html) = message
        .html_part(0)
        .filter(|part| part.is_text_html())
        .and_then(mail_parser::MessagePart::text_contents)
    {
        let (text, _) = super::nef::html_to_text(html);
        let links: Vec<ClaimLink> = labeled_links(html)
            .into_iter()
            .filter(|link| is_agent_download(&link.url))
            .collect();
        return notices(&text, links);
    }
    let text = message.body_text(0)?.into_owned();
    let links = inline_links(&text);
    notices(&text, links)
}

fn notices(text: &str, links: Vec<ClaimLink>) -> Option<ClaimsNotice> {
    if links.is_empty() {
        return None;
    }
    Some(ClaimsNotice {
        case_no: case_number(text),
        links,
    })
}

/// The sender's domain belongs to a claims-and-noticing vendor.
#[must_use]
pub fn is_claims_agent_sender(address: &str) -> bool {
    let domain = address.rsplit('@').next().unwrap_or(address);
    VENDOR_DOMAINS.iter().any(|vendor| {
        domain == *vendor
            || domain.ends_with(&format!(".{vendor}"))
            || domain
                .strip_prefix(vendor)
                .is_some_and(|rest| rest.starts_with('.'))
    })
}

/// Every agent link whose URL is a document download.
fn is_agent_download(url: &str) -> bool {
    url.to_ascii_lowercase().contains(DOWNLOAD_TOKEN)
}

/// The docket entry number a link label names: the first digits after
/// `Docket`/`Dkt`, as printed (`0029` stays padded; comparisons unpadded
/// elsewhere).
#[must_use]
pub fn docket_entry_number(label: &str) -> Option<String> {
    let after = after_docket_word(label)?;
    let digits = after.trim_start_matches(|c: char| !c.is_ascii_digit());
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    (!digits.is_empty()).then(|| digits[..end].to_string())
}

/// The label without its leading `Docket No. 29 - ` prefix, as the
/// document's title.
#[must_use]
pub fn link_title(label: &str) -> String {
    let Some(after) = after_docket_word(label) else {
        return label.trim().to_string();
    };
    let digits = after.trim_start_matches(|c: char| !c.is_ascii_digit());
    if digits.is_empty() {
        return label.trim().to_string();
    }
    let rest = digits.trim_start_matches(|c: char| c.is_ascii_digit());
    let rest = rest.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
    if rest.is_empty() || !label.is_char_boundary(label.len() - rest.len()) {
        return label.trim().to_string();
    }
    label[label.len() - rest.len()..].trim().to_string()
}

/// The lowercased text after the `Docket`/`Dkt` word, when the label names one.
fn after_docket_word(label: &str) -> Option<String> {
    let lower = label.to_ascii_lowercase();
    let at = lower.find("docket").or_else(|| lower.find("dkt"))?;
    Some(
        lower[at..]
            .trim_start_matches(|c: char| c.is_ascii_alphabetic())
            .to_string(),
    )
}

/// The case number a notice names, from a `Case No.`/`Case Number:` marker
/// or, failing that, the first bankruptcy (`26-10042`) or district
/// (`9:26-cv-00042`) shape in the text. Dates do not match: a bankruptcy
/// year is two digits and a district office number one.
#[must_use]
pub fn case_number(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    for marker in ["case no", "case:"] {
        if let Some(at) = lower.find(marker) {
            let after = &lower[at + marker.len()..];
            let token: String = after
                .trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            if as_case_number(&token).is_some() {
                return Some(token);
            }
        }
    }
    lower
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '(' | ')'))
        .map(|token| token.trim_end_matches(char::is_whitespace))
        .find_map(|token| {
            if as_case_number(token).is_some() || is_district_case_number(token) {
                Some(token.to_string())
            } else {
                None
            }
        })
}

/// A bankruptcy case number: two-digit year, four- to seven-digit sequence,
/// optional judge initials. `2026-03-05` is a date, not a case.
fn as_case_number(token: &str) -> Option<String> {
    let mut segments = token.split('-');
    let year = segments.next()?;
    if year.len() != 2 || !year.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let number = segments.next()?;
    if !(4..=7).contains(&number.len())
        || !number.chars().all(|c| c.is_ascii_digit())
        || segments.next().is_some_and(|initials| {
            initials.is_empty() || !initials.chars().all(|c| c.is_ascii_alphabetic())
        })
    {
        return None;
    }
    Some(token.to_string())
}

/// A district case number: `9:26-cv-00042`, judge initials included.
fn is_district_case_number(token: &str) -> bool {
    let Some((office, rest)) = token.split_once(':') else {
        return false;
    };
    if !(1..=2).contains(&office.len()) || !office.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let Some((year, rest)) = rest.split_once('-') else {
        return false;
    };
    if year.len() != 2 || !year.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let Some((kind, number)) = rest.split_once('-') else {
        return false;
    };
    if !matches!(kind, "cv" | "bk" | "adv") {
        return false;
    }
    number.chars().take_while(char::is_ascii_digit).count() >= 4
}

/// Every `<a href>` and its anchor text, in document order. The twin of
/// [`super::nef::html_to_text`]'s link list, which keeps no anchor text.
fn labeled_links(html: &str) -> Vec<ClaimLink> {
    let lower = html.to_ascii_lowercase();
    let mut links = Vec::new();
    let mut cursor = 0;
    while let Some(rel) = lower[cursor..].find("<a") {
        let open = cursor + rel;
        let after = &lower[open + 2..];
        if !after.starts_with('>') && !after.starts_with(|c: char| c.is_ascii_whitespace()) {
            cursor = open + 2;
            continue;
        }
        let Some(close) = lower[open..].find('>') else {
            break;
        };
        let body_start = open + close + 1;
        let end = lower[body_start..]
            .find("</a>")
            .map_or(html.len(), |rel| body_start + rel);
        let label = html[body_start..end]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let tag = &html[open + 1..open + close];
        if let Some(href) = attribute(tag, "href") {
            links.push(ClaimLink { url: href, label });
        }
        cursor = end;
    }
    links
}

/// The `href` of an anchor tag, decoded.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let at = lower.find(name)?;
    let rest = tag[at + name.len()..].trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let value = match rest.chars().next()? {
        '"' | '\'' => rest[1..].split(rest.chars().next()?).next()?,
        _ => rest.split_whitespace().next()?,
    };
    Some(value.replace("&amp;", "&"))
}

/// A plain-text notice carries its URLs inline; the line they sit on is the
/// label, and it usually names the docket entry.
fn inline_links(text: &str) -> Vec<ClaimLink> {
    text.lines()
        .flat_map(|line| {
            line.split_whitespace()
                .filter(|token| is_agent_download(token))
                .map(move |token| ClaimLink {
                    url: token
                        .trim_matches(|c: char| matches!(c, '<' | '>' | ')'))
                        .to_string(),
                    label: line.trim().to_string(),
                })
        })
        .collect()
}

/// Why a claims-agent link did not yield a document. Every arm means the
/// same next step — try the CM/ECF free look, else a person — but the
/// message tells the operator which wall they hit.
#[derive(Debug, thiserror::Error)]
pub enum AgentFetchError {
    /// The site refused the request outright (the `CloudFront` 403 of record).
    #[error("the claims-agent site answered {status} instead of the document")]
    Refused { status: reqwest::StatusCode },
    /// A `CloudFront` block page: HTML that reached the client with a success
    /// status. A manual download is needed, not a missing document.
    #[error("the claims-agent link answered with a block page, not a PDF")]
    BlockPage,
    /// Something else that is not a PDF.
    #[error("the claims-agent link answered with something other than a PDF")]
    NotPdf,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// One claims-agent download. The real one is [`HttpAgentFetch`]; tests use
/// a fake, so no test reaches an agent's site.
#[async_trait::async_trait]
pub trait AgentFetch: Send + Sync {
    async fn fetch(&self, url: &str) -> Result<Vec<u8>, AgentFetchError>;
}

/// Follows a claims-agent document link over HTTPS, as a browser would. A
/// link that answers with anything but PDF bytes is refused here, so a
/// block page can never be saved under a `.pdf` name.
pub struct HttpAgentFetch {
    client: reqwest::Client,
}

impl HttpAgentFetch {
    /// # Errors
    ///
    /// When the HTTP client cannot be built.
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .user_agent(concat!(
                    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) ",
                    "AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36"
                ))
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .context("build claims-agent client")?,
        })
    }
}

#[async_trait::async_trait]
impl AgentFetch for HttpAgentFetch {
    async fn fetch(&self, url: &str) -> Result<Vec<u8>, AgentFetchError> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .context("follow the claims-agent link")?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .context("read claims-agent bytes")?
            .to_vec();
        if !status.is_success() {
            return Err(AgentFetchError::Refused { status });
        }
        if super::ingest::is_pdf(&bytes) {
            return Ok(bytes);
        }
        if is_block_page(&bytes) {
            return Err(AgentFetchError::BlockPage);
        }
        Err(AgentFetchError::NotPdf)
    }
}

/// The `CloudFront` block page of record: "403 ERROR … Request blocked …
/// Generated by cloudfront". A scripted client hits it before any PDF
/// exists; only a person's browser gets through.
#[must_use]
pub fn is_block_page(bytes: &[u8]) -> bool {
    let page = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    page.contains("generated by cloudfront")
        || (page.contains("cloudfront") && page.contains("request blocked"))
}

#[cfg(test)]
mod tests {
    use super::is_claims_agent_sender;

    #[test]
    fn vendor_senders_match_exactly_and_on_subdomains() {
        for address in [
            "notices@stretto.com",
            "no-reply@restructuring.ra.kroll.com",
            "claims@epiq11.com",
            "no-reply@claims.epiq.com",
        ] {
            assert!(
                is_claims_agent_sender(&address.to_ascii_lowercase()),
                "{address} is a claims-agent sender"
            );
        }
        for address in ["notices@notkroll.com", "clerk@ecf.nvb.uscourts.gov"] {
            assert!(
                !is_claims_agent_sender(&address.to_ascii_lowercase()),
                "{address} is not a claims-agent sender"
            );
        }
    }
}
