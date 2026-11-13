//! Render the signature seal's three states to a static HTML specimen for
//! visual QA: the seal mark for a completed envelope, the awaiting state's
//! empty labelled lines, and the refused state marked in words. Serve the
//! page behind `server/public` (`python3 -m http.server`) so the theme and
//! token stylesheets resolve, then capture it in light and dark schemes.
//!
//! Deterministic by construction: every fact on the seals is a fixed
//! reference string, nothing is read from the environment.

use dioxus::prelude::*;
use webapp::components::{SealRole, SealSigner, SealState, SignatureSeal, SignatureSealData};

/// The fixed reference executed seal: both parties signed, the certificate
/// and the signed PDF archived.
fn executed() -> SignatureSealData {
    SignatureSealData {
        state: SealState::Signed,
        provider: "docusign".to_string(),
        signed_at: Some("2026-11-11T09:24:00-08:00".to_string()),
        signers: vec![
            SealSigner {
                name: "Avery Client".to_string(),
                role: SealRole::Client,
                bar_number: None,
                signed_at: Some("2026-11-11T09:22:37-08:00".to_string()),
            },
            SealSigner {
                name: "Nicholas Shook".to_string(),
                role: SealRole::Firm,
                bar_number: Some("12345".to_string()),
                signed_at: Some("2026-11-11T09:24:00-08:00".to_string()),
            },
        ],
        envelope_id: Some("3a1f9c2e-77c4-4d58-9e0a-6b2f8d1c4e77".to_string()),
        signed_sha256: Some(
            "9f2b0e4a6d1c3587ab4e0f6d9c1b2a3e4f5d6c7b8a9012fefedcba9876543210".to_string(),
        ),
        signed_href: Some("/public/engagements/agreement_signed.pdf".to_string()),
        certificate_href: Some("/public/engagements/certificate.pdf".to_string()),
    }
}

/// The awaiting seal: the envelope is out for signature, so the element
/// shows an empty labelled line per signer and no mark.
fn awaiting() -> SignatureSealData {
    SignatureSealData {
        state: SealState::Awaiting,
        signers: vec![
            SealSigner {
                name: "Avery Client".to_string(),
                role: SealRole::Client,
                bar_number: None,
                signed_at: None,
            },
            SealSigner {
                name: "Nicholas Shook".to_string(),
                role: SealRole::Firm,
                bar_number: Some("12345".to_string()),
                signed_at: None,
            },
        ],
        provider: "docusign".to_string(),
        ..SignatureSealData::default()
    }
}

/// The refused state: a declined envelope is marked in words, never a seal.
fn declined() -> SignatureSealData {
    SignatureSealData {
        state: SealState::Declined,
        provider: "docusign".to_string(),
        signers: vec![SealSigner {
            name: "Avery Client".to_string(),
            role: SealRole::Client,
            bar_number: None,
            signed_at: None,
        }],
        ..SignatureSealData::default()
    }
}

fn main() {
    let page = dioxus_ssr::render_element(rsx! {
        head {
            meta { charset: "utf-8" }
            link { rel: "stylesheet", href: "/css/theme.css" }
        }
        body {
            style { "body {{ font-family: sans-serif; margin: 0; padding: 3rem 2rem; display: grid; gap: 3rem; }}" }
            section {
                h1 { "Executed envelope — the seal mark" }
                SignatureSeal { seal: executed() }
            }
            section {
                h1 { "Awaiting signature — empty labelled lines" }
                SignatureSeal { seal: awaiting() }
            }
            section {
                h1 { "Declined — marked, never a seal" }
                SignatureSeal { seal: declined() }
            }
        }
    });
    println!("<!DOCTYPE html>\n{page}");
}
