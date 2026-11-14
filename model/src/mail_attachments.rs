//! How an inbound email attachment is named and screened before it is filed
//! as a matter document.
//!
//! Every filed row needs a slug — `project gate --check` fails a Project on
//! any live row without one — and a slug's file name must follow the
//! `YYYY.MM.DD_name.ext` convention (`C004`), because `project sync` writes
//! the bytes under it. Signature logos and other inline images ride on every
//! message of a thread; they are not documents, and filing them buries the
//! real attachments under copies of one logo.

use chrono::NaiveDate;

/// The largest image still read as a signature or inline graphic whatever
/// its name. A scan or photo of a document is far larger.
pub const INLINE_IMAGE_MAX_BYTES: usize = 32 * 1024;

const IMAGE_EXTENSIONS: [&str; 7] = ["png", "jpg", "jpeg", "gif", "bmp", "webp", "tif"];

/// The attachment's base name, never blank, carrying an extension
/// (`.bin` when it has none), so it can back a slug.
#[must_use]
pub fn filename_with_extension(filename: &str) -> String {
    let base = filename
        .rsplit(['/', '\\'])
        .next()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("attachment");
    match base.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => base.to_string(),
        _ => format!("{base}.bin"),
    }
}

/// The deterministic slug for part `index` of the message `message_id`,
/// received on `received_on`: `mail/<message_id>/<YYYY.MM.DD>_<name>-<index>.<ext>`.
/// `filename` should already carry its extension
/// ([`filename_with_extension`]); the extension keeps its case, because a
/// committed pointer's name follows the stored filename's.
#[must_use]
pub fn slug(message_id: &str, index: usize, filename: &str, received_on: NaiveDate) -> String {
    let filename = filename_with_extension(filename);
    let (stem, extension) = filename
        .rsplit_once('.')
        .unwrap_or((filename.as_str(), "bin"));
    let name = rules::c004::name_slug(stem).unwrap_or_else(|| "attachment".to_string());
    format!(
        "mail/{message_id}/{}_{name}-{index}.{extension}",
        received_on.format("%Y.%m.%d")
    )
}

/// True when the attachment reads as a signature logo or inline image
/// rather than a document: an image named the way mail clients name inline
/// parts (`image.png`, `image001.png`, `Outlook-…`, `…signature…`,
/// `…logo…`), or any image no larger than [`INLINE_IMAGE_MAX_BYTES`].
#[must_use]
pub fn is_inline_image(filename: &str, content_type: &str, size: usize) -> bool {
    let lower = filename.trim().to_ascii_lowercase();
    let (stem, extension) = lower.rsplit_once('.').unwrap_or((lower.as_str(), ""));
    let is_image = content_type
        .trim()
        .to_ascii_lowercase()
        .starts_with("image/")
        || IMAGE_EXTENSIONS.contains(&extension);
    if !is_image {
        return false;
    }
    let mail_client_name = stem
        .strip_prefix("image")
        .is_some_and(|rest| rest.bytes().all(|byte| byte.is_ascii_digit()))
        || stem.starts_with("outlook-")
        || stem.contains("signature")
        || stem.contains("logo");
    mail_client_name || size <= INLINE_IMAGE_MAX_BYTES
}

#[cfg(test)]
mod tests {
    use super::{filename_with_extension, is_inline_image, slug, INLINE_IMAGE_MAX_BYTES};
    use chrono::NaiveDate;

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 13).unwrap()
    }

    #[test]
    fn slug_names_the_message_and_part_and_follows_the_filename_convention() {
        let slug = slug("0193a", 2, "Signed Intake Form.PDF", day());
        assert_eq!(slug, "mail/0193a/2026.11.13_signed-intake-form-2.PDF");
        let name = slug.rsplit('/').next().unwrap();
        assert!(rules::c004::is_conventional_filename(name), "{name}");
    }

    #[test]
    fn slug_survives_names_with_nothing_to_keep() {
        assert_eq!(
            slug("m", 0, "", day()),
            "mail/m/2026.11.13_attachment-0.bin"
        );
        assert_eq!(
            slug("m", 1, "???.pdf", day()),
            "mail/m/2026.11.13_attachment-1.pdf"
        );
    }

    #[test]
    fn filename_gains_an_extension_only_when_it_has_none() {
        assert_eq!(filename_with_extension("letter.pdf"), "letter.pdf");
        assert_eq!(filename_with_extension("notes"), "notes.bin");
        assert_eq!(filename_with_extension(" "), "attachment.bin");
        assert_eq!(filename_with_extension("dir/.hidden"), ".hidden.bin");
        assert_eq!(filename_with_extension("C:\\scan.tif"), "scan.tif");
    }

    #[test]
    fn mail_client_inline_names_are_inline_at_any_size() {
        let big = INLINE_IMAGE_MAX_BYTES * 10;
        for name in [
            "image.png",
            "image001.png",
            "Outlook-abc123.gif",
            "Email Signature.png",
            "firm-logo.jpg",
        ] {
            assert!(is_inline_image(name, "image/png", big), "{name}");
        }
    }

    #[test]
    fn small_images_are_inline_and_large_photos_are_documents() {
        assert!(is_inline_image("facebook.png", "image/png", 4_000));
        assert!(!is_inline_image(
            "damage-photo.jpg",
            "image/jpeg",
            2_000_000
        ));
        assert!(is_inline_image("icon", "IMAGE/GIF", 100));
        assert!(is_inline_image(
            "icon.webp",
            "application/octet-stream",
            100
        ));
    }

    #[test]
    fn documents_are_never_inline_however_small_or_named() {
        assert!(!is_inline_image(
            "signature-page.pdf",
            "application/pdf",
            10
        ));
        assert!(!is_inline_image("image001.docx", "application/msword", 10));
    }
}
