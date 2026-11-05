//! Compose one PDF's pages onto another's.
//!
//! The `AcroForm` packet path fills and flattens a government form whose
//! template body — the engagement terms and the signature blocks the
//! client signs — renders separately through Typst. The packet sent for
//! signature is the composition: the state's own form followed by the
//! rendered authorization. [`append_document`] deep-copies the
//! addendum's pages into the base document, remapping every reference,
//! so no object is shared between the two sources.

use std::collections::BTreeMap;

use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::passage::{copy_object, inherited};
use crate::PdfError;

/// Append `addendum`'s pages after `base`'s, returning the combined PDF.
///
/// The base document's structure is untouched; addendum pages are
/// deep-copied (references remapped, inherited attributes materialised)
/// so the result renders identically to both sources rendered alone. A
/// pageless addendum returns the base unchanged.
///
/// # Errors
/// [`PdfError::Lopdf`] when either document fails to parse, a page
/// tree is malformed, or the result fails to serialise.
pub fn append_document(base: &[u8], addendum: &[u8]) -> Result<Vec<u8>, PdfError> {
    let lopdf_err = |e: String| PdfError::Lopdf(e);
    let mut doc = Document::load_mem(base).map_err(|e| lopdf_err(e.to_string()))?;
    let src = Document::load_mem(addendum).map_err(|e| lopdf_err(e.to_string()))?;

    let parent_pages = pages_node(&doc)?;
    let mut mapping: BTreeMap<ObjectId, ObjectId> = BTreeMap::new();
    let mut appended: Vec<ObjectId> = Vec::new();
    for page_id in src.get_pages().values().copied() {
        appended.push(copy_page(
            &src,
            page_id,
            &mut doc,
            parent_pages,
            &mut mapping,
        )?);
    }
    if appended.is_empty() {
        return Ok(base.to_vec());
    }

    let added =
        i64::try_from(appended.len()).map_err(|e| PdfError::Lopdf(format!("page count: {e}")))?;
    let pages_dict = doc
        .get_dictionary_mut(parent_pages)
        .map_err(|e| lopdf_err(e.to_string()))?;
    let count = pages_dict
        .get(b"Count")
        .and_then(Object::as_i64)
        .unwrap_or(0);
    let kids = pages_dict
        .get_mut(b"Kids")
        .and_then(Object::as_array_mut)
        .map_err(|e| lopdf_err(format!("page tree Kids: {e}")))?;
    kids.extend(appended.iter().map(|id| Object::Reference(*id)));
    pages_dict.set("Count", count + added);

    let mut out = Vec::new();
    doc.save_to(&mut out)
        .map_err(|e| lopdf_err(e.to_string()))?;
    Ok(out)
}

/// The base document's root `/Pages` node, through trailer → catalog.
fn pages_node(doc: &Document) -> Result<ObjectId, PdfError> {
    let root = doc
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .map_err(|e| PdfError::Lopdf(format!("catalog: {e}")))?;
    let catalog = doc
        .get_dictionary(root)
        .map_err(|e| PdfError::Lopdf(e.to_string()))?;
    catalog
        .get(b"Pages")
        .and_then(Object::as_reference)
        .map_err(|e| PdfError::Lopdf(format!("page tree: {e}")))
}

/// Deep-copy one page of `src` into `out`, registering it under the
/// base page tree's `pages_id`. Copies every key the page carries
/// (references remapped through `mapping`), then materialises the
/// attributes it inherited from the source tree so the appended page
/// keeps the geometry and resources it rendered with.
fn copy_page(
    src: &Document,
    page_id: ObjectId,
    out: &mut Document,
    parent_pages: ObjectId,
    mapping: &mut BTreeMap<ObjectId, ObjectId>,
) -> Result<ObjectId, PdfError> {
    let page_dict = src
        .get_dictionary(page_id)
        .map_err(|e| PdfError::Lopdf(e.to_string()))?;
    let media = inherited(src, page_dict, b"MediaBox");
    let resources = inherited(src, page_dict, b"Resources");
    let rotate = inherited(src, page_dict, b"Rotate");

    let mut new_page = Dictionary::new();
    for (key, value) in page_dict {
        if key.as_slice() == b"Parent" {
            continue;
        }
        new_page.set(key.clone(), copy_object(src, out, value, mapping));
    }
    if let Some(res) = resources {
        if !new_page.has(b"Resources") {
            new_page.set("Resources", copy_object(src, out, &res, mapping));
        }
    }
    if let Some(rot) = rotate {
        if !new_page.has(b"Rotate") {
            new_page.set("Rotate", rot);
        }
    }
    if let Some(media) = media {
        new_page.set("MediaBox", copy_object(src, out, &media, mapping));
    }
    new_page.set("Type", Object::Name(b"Page".to_vec()));
    new_page.set("Parent", Object::Reference(parent_pages));
    Ok(out.add_object(Object::Dictionary(new_page)))
}
