//! The `evidence_index` section: the matter's exhibits grouped by the claim
//! each bears on, with the date each exhibit itself bears and where it came
//! from (LAW-185).
//!
//! Every value here is display-ready and wasm-safe; the server half
//! ([`derive`]) builds the groups from the matter's `asset` rows, already
//! scoped to the lens. The index is firm working state — a client page never
//! carries it — so the loader fills it on the lawyer lens only.
//!
//! An exhibit with no evidence metadata still indexes, showing its gaps: the
//! point of the section is what is *not* yet tied down, not a trophy case of
//! what is. An exhibit whose metadata names no claim reference reads under
//! the unclaimed rail, and a count of exhibits with no metadata at all heads
//! the section's note.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use super::PageLens;
use crate::components::{Badge, BadgeTone, Empty, Panel};

/// One exhibit in the index: a filed exhibit and the evidence metadata its
/// upload recorded, with the gaps where none was.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceExhibit {
    /// The document id, for stable keys.
    pub document_id: String,
    /// The document's download href.
    pub href: String,
    /// The filename, or the slug when the upload named none.
    pub label: String,
    /// The date the exhibit itself bears, `YYYY-MM-DD` — a letter's own
    /// date, a photograph's capture date — never the filing date. `None`
    /// renders as the gap it is.
    pub dated_at: Option<String>,
    /// Where the exhibit came from, as the filing lawyer recorded it.
    pub source: Option<String>,
    /// `exact`, `inferred`, or `unknown`.
    pub confidence: Option<String>,
}

/// One claim's exhibits, soonest exhibit date first, undated last.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceClaim {
    /// The claim reference as the exhibits name it, e.g. `¶ 12` or
    /// `Count II — Conversion`.
    pub claim: String,
    pub exhibits: Vec<EvidenceExhibit>,
}

/// The `evidence_index` section's data: exhibits grouped by the claim each
/// bears on, the exhibits that name no claim, and how many name none of it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceIndexView {
    pub lens: Option<PageLens>,
    /// One group per claim reference, references sorted; an exhibit bearing
    /// on two claims appears under each.
    pub claims: Vec<EvidenceClaim>,
    /// Exhibits whose evidence names no claim reference.
    pub unclaimed: Vec<EvidenceExhibit>,
    /// Exhibits filed with no evidence metadata at all.
    pub unrecorded: usize,
}

fn confidence_tone(confidence: &str) -> BadgeTone {
    match confidence {
        "exact" => BadgeTone::Ready,
        "inferred" => BadgeTone::Review,
        _ => BadgeTone::Default,
    }
}

/// The `evidence_index` section.
#[component]
pub fn EvidenceIndexSection(view: EvidenceIndexView) -> Element {
    use std::fmt::Write as _;
    let lawyer = view.lens == Some(PageLens::Lawyer);
    let mut note = "Exhibits grouped by the claim each bears on.".to_string();
    if lawyer && view.unrecorded > 0 {
        let _ = write!(
            note,
            " {} exhibit{} on this matter record no evidence metadata yet.",
            view.unrecorded,
            if view.unrecorded == 1 { "" } else { "s" }
        );
    }
    let grouped = !view.claims.is_empty() || !view.unclaimed.is_empty();
    rsx! {
        section {
            class: "matter-section",
            "data-section": "evidence_index",
            aria_labelledby: "section-evidence_index-title",
            Panel {
                id: "section-evidence_index".to_string(),
                title: "Evidence index".to_string(),
                note,
                if !grouped {
                    div { class: "matter-section__empty",
                        Empty { title: "No exhibits are filed on this matter yet.".to_string() }
                    }
                } else {
                    for group in view.claims.iter() {
                        div { class: "matter-evidence__claim",
                            h4 { class: "matter-evidence__claim-title", "{group.claim}" }
                            ul { class: "matter-evidence",
                                for exhibit in group.exhibits.iter() {
                                    EvidenceExhibitRow { key: "{exhibit.document_id}", exhibit: exhibit.clone() }
                                }
                            }
                        }
                    }
                    if !view.unclaimed.is_empty() {
                        div { class: "matter-evidence__claim",
                            h4 { class: "matter-evidence__claim-title", "No claim recorded" }
                            ul { class: "matter-evidence",
                                for exhibit in view.unclaimed.iter() {
                                    EvidenceExhibitRow { key: "{exhibit.document_id}", exhibit: exhibit.clone() }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn EvidenceExhibitRow(exhibit: EvidenceExhibit) -> Element {
    rsx! {
        li { class: "matter-evidence__item",
            a { class: "nav-link", href: "{exhibit.href}", "{exhibit.label}" }
            if let Some(dated_at) = exhibit.dated_at.clone() {
                span { class: "matter-evidence__dated", " · {dated_at}" }
            } else {
                span { class: "matter-evidence__dated matter-evidence__gap", " · no date recorded" }
            }
            if let Some(source) = exhibit.source.clone() {
                span { class: "matter-evidence__source", " · {source}" }
            } else {
                span { class: "matter-evidence__source matter-evidence__gap", " · no source recorded" }
            }
            if let Some(confidence) = exhibit.confidence.clone() {
                span { class: "matter-evidence__confidence",
                    Badge { tone: confidence_tone(&confidence), "{confidence}" }
                }
            }
        }
    }
}

/// The server half: derive the index from the matter's asset rows.
#[cfg(feature = "server")]
pub mod derive {
    use super::{EvidenceClaim, EvidenceExhibit, EvidenceIndexView};
    use store::assets::Asset;

    /// The asset kind an exhibit carries (the Client Production default).
    const EXHIBIT_KIND: &str = "exhibit";

    /// Group the exhibit assets by the claim references their evidence
    /// names, each exhibit under every claim it bears on. Exhibits with
    /// metadata but no reference read under `unclaimed`; exhibits with no
    /// metadata at all count in `unrecorded`. Claims sort by reference,
    /// exhibits by the date they bear (undated last), then label.
    #[must_use]
    pub fn index(assets: &[Asset], project_code: &str) -> EvidenceIndexView {
        let mut view = EvidenceIndexView::default();
        let mut claims: Vec<EvidenceClaim> = Vec::new();
        for asset in assets
            .iter()
            .filter(|asset| asset.kind.as_deref() == Some(EXHIBIT_KIND))
        {
            if asset.evidence.is_none() {
                view.unrecorded += 1;
            }
            let exhibit = EvidenceExhibit {
                document_id: asset.id.to_string(),
                href: format!("/app/projects/{project_code}/documents/{}", asset.id),
                label: asset
                    .filename
                    .clone()
                    .or_else(|| asset.slug.clone())
                    .unwrap_or_else(|| asset.id.to_string()),
                dated_at: asset
                    .evidence
                    .as_ref()
                    .and_then(|evidence| evidence.dated_at.map(|date| date.to_string())),
                source: asset.evidence.as_ref().and_then(|evidence| {
                    evidence.source.clone().filter(|source| !source.is_empty())
                }),
                confidence: asset
                    .evidence
                    .as_ref()
                    .and_then(|evidence| evidence.confidence)
                    .map(|confidence| confidence.as_str().to_string()),
            };
            let supports = asset
                .evidence
                .as_ref()
                .map(|evidence| evidence.supports.clone())
                .unwrap_or_default();
            if supports.is_empty() {
                view.unclaimed.push(exhibit);
            } else {
                for claim in supports {
                    match claims.iter_mut().find(|group| group.claim == claim) {
                        Some(group) => group.exhibits.push(exhibit.clone()),
                        None => claims.push(EvidenceClaim {
                            claim,
                            exhibits: vec![exhibit.clone()],
                        }),
                    }
                }
            }
        }
        for group in &mut claims {
            group
                .exhibits
                // Undated exhibits close the group: dated first, gaps last.
                .sort_by(|a, b| {
                    (&a.dated_at.is_none(), &a.dated_at, &a.label).cmp(&(
                        &b.dated_at.is_none(),
                        &b.dated_at,
                        &b.label,
                    ))
                });
        }
        claims.sort_by(|a, b| a.claim.cmp(&b.claim));
        view.unclaimed
            // Same order for unclaimed exhibits: undated last.
            .sort_by(|a, b| {
                (&a.dated_at.is_none(), &a.dated_at, &a.label).cmp(&(
                    &b.dated_at.is_none(),
                    &b.dated_at,
                    &b.label,
                ))
            });
        view.claims = claims;
        view
    }
}

#[cfg(all(test, feature = "server"))]
mod tests {
    use super::derive::index;
    use super::EvidenceIndexView;
    use chrono::Utc;
    use store::assets::Asset;
    use store::documents::{Evidence, EvidenceConfidence};

    /// An exhibit asset carrying every field a live row does, with the
    /// fields under test passed in.
    fn exhibit_asset(filename: &str, evidence: Option<Evidence>) -> Asset {
        let now = Utc::now();
        Asset {
            id: uuid::Uuid::now_v7(),
            storage_key: "projects/c/documents/asset".to_string(),
            secondary_storage_key: None,
            content_type: "application/pdf".to_string(),
            byte_size: 10,
            sha256_hex: "0".repeat(64),
            project_id: Some(uuid::Uuid::now_v7()),
            filename: Some(filename.to_string()),
            kind: Some("exhibit".to_string()),
            source: Some(store::documents::source::UPLOAD.to_string()),
            received_at: None,
            description: None,
            visibility: store::documents::visibility::INTERNAL.to_string(),
            slug: None,
            published_at: None,
            source_message_id: None,
            source_sender: None,
            source_received_at: None,
            source_subject: None,
            metadata: None,
            derived_from: None,
            transcript_quality: None,
            signing: None,
            notarization: Vec::new(),
            evidence,
            deleted_at: None,
            purge_after: None,
            deleted_by_person_id: None,
            deleted_commit_sha: None,
            legal_hold_at: None,
            purged_at: None,
            inserted_at: now,
            updated_at: now,
        }
    }

    fn evidence(claims: &[&str], dated: Option<&str>) -> Evidence {
        Evidence {
            dated_at: dated.map(|date| {
                chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").expect("test date")
            }),
            source: Some("demand letter".to_string()),
            confidence: Some(EvidenceConfidence::Exact),
            supports: claims.iter().map(|claim| (*claim).to_string()).collect(),
        }
    }

    #[test]
    fn exhibits_group_under_each_claim_they_name() {
        let letter = exhibit_asset(
            "letter.pdf",
            Some(evidence(&["Count I", "Count II"], "2026-01-05")),
        );
        let photo = exhibit_asset("photo.png", Some(evidence(&["Count II"], "2026-01-04")));
        let view = index(&[letter, photo], "c");
        let names: Vec<&str> = view.claims.iter().map(|g| g.claim.as_str()).collect();
        assert_eq!(names, vec!["Count I", "Count II"]);
        let count_two = &view.claims[1];
        // Count II sorts by exhibit date: the photo (Jan 4) before the letter (Jan 5).
        assert_eq!(count_two.exhibits.len(), 2);
        assert_eq!(count_two.exhibits[0].label, "photo.png");
        assert_eq!(count_two.exhibits[1].label, "letter.pdf");
        // The letter bears on two claims, so it appears under both.
        assert_eq!(view.claims[0].exhibits[0].label, "letter.pdf");
        assert_eq!(view.unclaimed.len(), 0);
        assert_eq!(view.unrecorded, 0);
    }

    #[test]
    fn undated_exhibits_sort_last_within_a_claim() {
        let dated = exhibit_asset("dated.pdf", Some(evidence(&["Count I"], "2026-01-05")));
        let undated = exhibit_asset("undated.pdf", Some(evidence(&["Count I"], None)));
        let view = index(&[undated, dated], "c");
        let labels: Vec<&str> = view.claims[0]
            .exhibits
            .iter()
            .map(|exhibit| exhibit.label.as_str())
            .collect();
        assert_eq!(labels, vec!["dated.pdf", "undated.pdf"]);
    }

    #[test]
    fn exhibits_without_metadata_index_as_gaps() {
        let bare = exhibit_asset("scan.pdf", None);
        let view = index(std::slice::from_ref(&bare), "c");
        assert_eq!(view.unrecorded, 1);
        assert_eq!(view.unclaimed.len(), 1);
        assert_eq!(view.unclaimed[0].label, "scan.pdf");
        assert_eq!(view.unclaimed[0].dated_at, None);
        assert_eq!(view.claims.len(), 0);
    }

    #[test]
    fn non_exhibit_kinds_never_index() {
        let letter = exhibit_asset("letter.pdf", None);
        let mut pleading = letter.clone();
        pleading.id = uuid::Uuid::now_v7();
        pleading.kind = Some("pleading".to_string());
        pleading.evidence = Some(evidence(&["Count I"], None));
        let view = index(&[letter, pleading], "c");
        assert_eq!(view.unrecorded, 1);
        assert_eq!(view.claims.len(), 0);
    }

    #[test]
    fn an_empty_matter_indexes_empty() {
        let view = index(&[], "c");
        assert_eq!(view, EvidenceIndexView::default());
    }

    #[test]
    fn exhibit_hrefs_point_at_the_project_document() {
        let bare = exhibit_asset("scan.pdf", None);
        let view = index(std::slice::from_ref(&bare), "acme");
        assert_eq!(
            view.unclaimed[0].href,
            format!("/app/projects/acme/documents/{}", bare.id)
        );
    }

    #[test]
    fn an_exhibit_without_a_filename_falls_back_to_its_slug_then_id() {
        let mut slugged = exhibit_asset("named.pdf", None);
        slugged.filename = None;
        slugged.slug = Some("documents/scan".to_string());
        let view = index(std::slice::from_ref(&slugged), "c");
        assert_eq!(view.unclaimed[0].label, "documents/scan");

        let mut nameless = exhibit_asset("named.pdf", None);
        nameless.filename = None;
        nameless.slug = None;
        let id = nameless.id.to_string();
        let view = index(std::slice::from_ref(&nameless), "c");
        assert_eq!(view.unclaimed[0].label, id);
    }
}

#[cfg(test)]
mod render_tests {
    use super::{EvidenceClaim, EvidenceExhibit, EvidenceIndexView};
    use crate::sections::tests::render;
    use crate::sections::{PageLens, SectionView};

    fn exhibit(id: &str, label: &str, dated: Option<&str>) -> EvidenceExhibit {
        EvidenceExhibit {
            document_id: id.to_string(),
            href: format!("/app/projects/c/documents/{id}"),
            label: label.to_string(),
            dated_at: dated.map(str::to_string),
            source: dated.map(|_| "demand letter".to_string()),
            confidence: dated.map(|_| "exact".to_string()),
        }
    }

    fn lawyer_view() -> EvidenceIndexView {
        EvidenceIndexView {
            lens: Some(PageLens::Lawyer),
            claims: vec![EvidenceClaim {
                claim: "Count II — Conversion".to_string(),
                exhibits: vec![exhibit("01j-exhibit", "letter.pdf", Some("2026-01-05"))],
            }],
            unclaimed: vec![exhibit("01j-scan", "scan.pdf", None)],
            unrecorded: 1,
        }
    }

    #[test]
    fn the_lawyer_view_renders_claims_dates_provenance_and_gaps() {
        let html = render(SectionView::EvidenceIndex(lawyer_view()));
        // Visual-evidence fixture: running with EVIDENCE_RENDER_DUMP=<path>
        // writes the section's SSR output beside the assertions, so PR review
        // sees exactly what the lawyer's matter page shows.
        if let Ok(path) = std::env::var("EVIDENCE_RENDER_DUMP") {
            std::fs::write(&path, &html).expect("writes the render dump");
        }
        assert!(html.contains("Evidence index"), "{html}");
        assert!(html.contains("Count II — Conversion"), "{html}");
        assert!(html.contains("letter.pdf"), "{html}");
        assert!(html.contains("2026-01-05"), "{html}");
        assert!(html.contains("demand letter"), "{html}");
        assert!(html.contains("exact"), "{html}");
        assert!(
            html.contains("/app/projects/c/documents/01j-exhibit"),
            "{html}"
        );
        // The gap rows name what is missing, and the note counts them.
        assert!(html.contains("no date recorded"), "{html}");
        assert!(html.contains("no source recorded"), "{html}");
        assert!(html.contains("1 exhibit on this matter record"), "{html}");
    }

    #[test]
    fn an_empty_index_renders_the_empty_state() {
        let html = render(SectionView::EvidenceIndex(EvidenceIndexView::default()));
        assert!(
            html.contains("No exhibits are filed on this matter yet."),
            "{html}"
        );
        assert!(!html.contains("matter-evidence__claim"), "{html}");
    }
}
