//! `navigator mail attachments`: write a Gmail message's attachments to a
//! local directory, one file per attachment, without clobbering.
//!
//! The message is read with `format=raw` — the same read `mail save` makes
//! — and walked with the MIME parser, so the bytes written are exactly the
//! transfer-decoded parts. Nothing is uploaded; filing stays with
//! `navigator site document upload`.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};

/// One attachment pulled out of a message, named and ready to write.
#[derive(Debug)]
pub(super) struct ExtractedAttachment {
    pub(super) name: String,
    pub(super) bytes: Vec<u8>,
}

/// What the walk is told to keep.
pub(super) struct Selection<'a> {
    /// A filename glob (`*` and `?`, ASCII case-insensitive).
    pub(super) only: Option<&'a str>,
    /// Keep inline images, which are skipped by default.
    pub(super) include_inline: bool,
}

/// Refuses a name that cannot be written as one plain file in the target
/// directory: empty, `.` or `..`, a path separator, or a control character.
/// Surrounding whitespace is trimmed; nothing else is rewritten.
pub(super) fn checked_filename(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." {
        return Err(anyhow!("an attachment has an empty or dot-only filename"));
    }
    if name.chars().any(char::is_control) {
        return Err(anyhow!(
            "an attachment filename carries a control character; pass --only to pick the others"
        ));
    }
    if name.contains(['/', '\\']) {
        return Err(anyhow!(
            "the attachment filename {name:?} carries a path separator; pass --only to pick the others"
        ));
    }
    Ok(name.to_owned())
}

/// A shell-style glob on a filename: `*` matches any run, `?` one
/// character, ASCII case-insensitive.
pub(super) fn glob_matches(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().map(|c| c.to_ascii_lowercase()).collect();
    let name: Vec<char> = name.chars().map(|c| c.to_ascii_lowercase()).collect();
    let (mut p, mut n) = (0, 0);
    let mut backtrack: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            backtrack = Some((p, n));
            p += 1;
        } else if let Some((star, matched)) = backtrack {
            p = star + 1;
            n = matched + 1;
            backtrack = Some((star, matched + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

/// The extension for an unnamed part, from its MIME type.
fn extension_for(ctype: &str, subtype: &str) -> &'static str {
    match (ctype, subtype) {
        ("application", "pdf") => "pdf",
        ("message", "rfc822") => "eml",
        ("image", "png") => "png",
        ("image", "jpeg" | "jpg") => "jpg",
        ("image", "gif") => "gif",
        ("text", "plain") => "txt",
        ("text", "html") => "html",
        ("text", "calendar") => "ics",
        _ => "bin",
    }
}

/// Walks a raw RFC 822 message and returns its attachments, named and
/// filtered. Every name is checked before anything is returned, so a bad
/// name refuses the whole run before a byte is written.
///
/// Inline images (a `cid:` logo, a pasted screenshot) are skipped unless
/// `include_inline` is set. Other inline parts — Apple Mail marks a PDF
/// inline — are kept. A forwarded message is one `.eml` attachment.
pub(super) fn extract(raw: &[u8], selection: &Selection<'_>) -> Result<Vec<ExtractedAttachment>> {
    let message = mail_parser::MessageParser::default()
        .parse(raw)
        .ok_or_else(|| anyhow!("the message is not a MIME email"))?;
    let mut found = Vec::new();
    for (index, part) in message.attachments().enumerate() {
        let (ctype, subtype) = part.content_type().map_or_else(
            || ("application".to_owned(), "octet-stream".to_owned()),
            |content_type| {
                (
                    content_type.ctype().to_ascii_lowercase(),
                    content_type
                        .subtype()
                        .unwrap_or_default()
                        .to_ascii_lowercase(),
                )
            },
        );
        if part.is_inline() && ctype == "image" && !selection.include_inline {
            continue;
        }
        let name = match part.attachment_name() {
            Some(name) => checked_filename(name)?,
            None => format!(
                "attachment-{}.{}",
                index + 1,
                extension_for(&ctype, &subtype)
            ),
        };
        if selection
            .only
            .is_some_and(|pattern| !glob_matches(pattern, &name))
        {
            continue;
        }
        found.push(ExtractedAttachment {
            name,
            bytes: part.decoded().into_owned(),
        });
    }
    Ok(disambiguate(found))
}

/// Two attachments may share a filename. Identical bytes collapse to one
/// file; differing bytes get `-2`, `-3`, ... before the extension, in
/// message order, so a re-run names them the same way.
fn disambiguate(attachments: Vec<ExtractedAttachment>) -> Vec<ExtractedAttachment> {
    let mut kept: Vec<ExtractedAttachment> = Vec::new();
    for mut attachment in attachments {
        if kept
            .iter()
            .any(|seen| seen.name == attachment.name && seen.bytes == attachment.bytes)
        {
            continue;
        }
        if kept.iter().any(|seen| seen.name == attachment.name) {
            let (stem, extension) = match attachment.name.rsplit_once('.') {
                Some((stem, extension)) if !stem.is_empty() => {
                    (stem.to_owned(), format!(".{extension}"))
                }
                _ => (attachment.name.clone(), String::new()),
            };
            let mut counter = 2;
            while kept
                .iter()
                .any(|seen| seen.name == format!("{stem}-{counter}{extension}"))
            {
                counter += 1;
            }
            attachment.name = format!("{stem}-{counter}{extension}");
        }
        kept.push(attachment);
    }
    kept
}

/// What happened to one file.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum Written {
    Written,
    Unchanged,
}

/// Writes every attachment into `dir`. Checks every target first: an
/// existing file with different bytes refuses the run before anything is
/// written. An identical existing file is left alone.
pub(super) fn write_all(
    dir: &Path,
    attachments: &[ExtractedAttachment],
) -> Result<Vec<(PathBuf, Written)>> {
    let mut plan = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        let path = dir.join(&attachment.name);
        let state = match std::fs::read(&path) {
            Ok(existing) if existing == attachment.bytes => Written::Unchanged,
            Ok(_) => {
                return Err(anyhow!(
                    "{} already exists with different contents; nothing was written",
                    path.display()
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Written::Written,
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        plan.push((path, state));
    }
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    for ((path, state), attachment) in plan.iter().zip(attachments) {
        if *state == Written::Written {
            // create_new: a file that appeared since the check is never
            // overwritten.
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .with_context(|| format!("create {}", path.display()))?;
            file.write_all(&attachment.bytes)
                .with_context(|| format!("write {}", path.display()))?;
        }
    }
    Ok(plan)
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// One output line: name, byte count, digest, and whether it was written.
pub(super) fn report_line(attachment: &ExtractedAttachment, state: &Written) -> String {
    format!(
        "{}\t{} bytes\tsha256:{}\t{}",
        attachment.name,
        attachment.bytes.len(),
        sha256_hex(&attachment.bytes),
        match state {
            Written::Written => "written",
            Written::Unchanged => "already present",
        }
    )
}

/// `navigator mail attachments`, from its parsed flags.
pub(super) async fn run(
    reference: &str,
    dir: &Path,
    only: Option<&str>,
    include_inline: bool,
) -> Result<()> {
    let lines = attachments_command(
        reference,
        dir,
        &Selection {
            only,
            include_inline,
        },
        super::client,
    )
    .await?;
    if lines.is_empty() {
        println!("No attachments matched; nothing was written");
    }
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

/// Fetches the raw message through `connect`, writes its attachments, and
/// returns one report line per file. A reference that names no message is
/// refused before `connect` runs.
async fn attachments_command<F, Fut>(
    reference: &str,
    dir: &Path,
    selection: &Selection<'_>,
    connect: F,
) -> Result<Vec<String>>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<super::GmailClient>>,
{
    let message_id = super::gmail_message_id(reference).ok_or_else(|| {
        anyhow!("the reference is neither a Gmail web URL nor a Gmail message id: {reference}")
    })?;
    let gmail = connect().await?;
    let raw_field = gmail.raw_message(&message_id).await?;
    let raw = super::decode_gmail_raw(
        raw_field
            .raw
            .as_deref()
            .ok_or_else(|| anyhow!("message {message_id} came back with no raw copy"))?,
    )?;
    let attachments = extract(&raw, selection)?;
    if attachments.is_empty() {
        return Ok(Vec::new());
    }
    Ok(write_all(dir, &attachments)?
        .iter()
        .zip(&attachments)
        .map(|((_, state), attachment)| report_line(attachment, state))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/mail/attachments.eml");

    fn all() -> Selection<'static> {
        Selection {
            only: None,
            include_inline: false,
        }
    }

    fn names(attachments: &[ExtractedAttachment]) -> Vec<&str> {
        attachments.iter().map(|a| a.name.as_str()).collect()
    }

    #[test]
    fn walks_the_fixture_skipping_the_inline_logo_and_decoding_each_part() {
        let attachments = extract(FIXTURE, &all()).unwrap();
        assert_eq!(
            names(&attachments),
            [
                "Engagement Letter.pdf",
                "notes.txt",
                "Résumé draft.txt",
                "forwarded.eml",
                "notes-2.txt",
            ]
        );
        assert_eq!(attachments[0].bytes, b"%PDF-1.4 synthetic\n");
        assert_eq!(attachments[1].bytes, b"First notes.\n");
        assert_eq!(attachments[4].bytes, b"Second notes, different bytes.\n");
        assert!(String::from_utf8_lossy(&attachments[3].bytes).contains("Subject: Forwarded"));
    }

    #[test]
    fn include_inline_keeps_the_inline_image() {
        let attachments = extract(
            FIXTURE,
            &Selection {
                only: None,
                include_inline: true,
            },
        )
        .unwrap();
        assert!(names(&attachments).contains(&"logo.png"));
        let logo = attachments.iter().find(|a| a.name == "logo.png").unwrap();
        assert_eq!(logo.bytes, b"\x89PNG synthetic");
    }

    #[test]
    fn only_picks_by_case_insensitive_glob() {
        let attachments = extract(
            FIXTURE,
            &Selection {
                only: Some("*.PDF"),
                include_inline: false,
            },
        )
        .unwrap();
        assert_eq!(names(&attachments), ["Engagement Letter.pdf"]);
    }

    #[test]
    fn glob_handles_stars_and_question_marks() {
        assert!(glob_matches("*.pdf", "a.pdf"));
        assert!(glob_matches("*", ""));
        assert!(glob_matches("exhibit-?.pdf", "Exhibit-A.PDF"));
        assert!(glob_matches("*letter*", "Engagement Letter.pdf"));
        assert!(glob_matches("a*b*c", "aXXbYYc"));
        assert!(!glob_matches("a*b*c", "aXXbYY"));
        assert!(!glob_matches("*.pdf", "a.pdf.exe"));
        assert!(!glob_matches("?", ""));
    }

    #[test]
    fn filenames_refuse_separators_controls_and_dots() {
        assert_eq!(checked_filename("  Letter.pdf ").unwrap(), "Letter.pdf");
        assert_eq!(checked_filename("Résumé.pdf").unwrap(), "Résumé.pdf");
        for bad in [
            "../etc/passwd",
            "dir/file.pdf",
            "dir\\file.pdf",
            "line\nbreak.pdf",
            "bell\u{7}.pdf",
            "nul\0.pdf",
            "",
            "   ",
            ".",
            "..",
        ] {
            assert!(checked_filename(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn a_traversal_name_in_a_message_refuses_the_whole_run() {
        let raw = b"From: a@example.test\r\nSubject: x\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nhi\r\n\
--b\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=\"../../x.pdf\"\r\n\r\n\
bytes\r\n--b--\r\n";
        assert!(extract(raw, &all()).is_err());
    }

    #[test]
    fn an_unnamed_part_gets_a_numbered_name_from_its_type() {
        let raw = b"From: a@example.test\r\nSubject: x\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nhi\r\n\
--b\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment\r\n\r\nbytes\r\n--b--\r\n";
        let attachments = extract(raw, &all()).unwrap();
        assert_eq!(names(&attachments), ["attachment-1.pdf"]);
    }

    #[test]
    fn writes_never_clobber_and_reruns_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out");
        let attachments = vec![ExtractedAttachment {
            name: "a.pdf".into(),
            bytes: b"one".to_vec(),
        }];
        let first = write_all(&target, &attachments).unwrap();
        assert_eq!(first[0].1, Written::Written);
        let again = write_all(&target, &attachments).unwrap();
        assert_eq!(again[0].1, Written::Unchanged);

        let changed = vec![
            ExtractedAttachment {
                name: "b.pdf".into(),
                bytes: b"new".to_vec(),
            },
            ExtractedAttachment {
                name: "a.pdf".into(),
                bytes: b"two".to_vec(),
            },
        ];
        let error = write_all(&target, &changed).unwrap_err();
        assert!(format!("{error}").contains("different contents"));
        assert_eq!(std::fs::read(target.join("a.pdf")).unwrap(), b"one");
        assert!(
            !target.join("b.pdf").exists(),
            "a refused run writes nothing"
        );
    }

    const MESSAGE_ID: &str = "18fa3bc2d4e5f6a7";

    async fn gmail_serving(raw: Option<&[u8]>) -> (wiremock::MockServer, url::Url) {
        use wiremock::matchers::{method, path, query_param};
        let server = wiremock::MockServer::start().await;
        let body = match raw {
            Some(raw) => serde_json::json!({
                "id": MESSAGE_ID,
                "raw": base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, raw),
            }),
            None => serde_json::json!({ "id": MESSAGE_ID }),
        };
        wiremock::Mock::given(method("GET"))
            .and(path(format!("/users/me/messages/{MESSAGE_ID}")))
            .and(query_param("format", "raw"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let base = url::Url::parse(&server.uri()).unwrap();
        (server, base)
    }

    #[tokio::test]
    async fn the_command_writes_each_attachment_from_a_gmail_web_url() {
        let (_server, base) = gmail_serving(Some(FIXTURE)).await;
        let dir = tempfile::tempdir().unwrap();
        let reference = format!("https://mail.google.com/mail/u/0/#inbox/{MESSAGE_ID}");
        let connect = || async {
            Ok(super::super::GmailClient::with_base_url(
                "t".into(),
                base.clone(),
            ))
        };
        let lines = attachments_command(&reference, dir.path(), &all(), connect)
            .await
            .unwrap();
        assert_eq!(lines.len(), 5);
        assert!(lines[0].starts_with("Engagement Letter.pdf\t19 bytes\tsha256:"));
        assert!(lines.iter().all(|line| line.ends_with("\twritten")));
        assert_eq!(
            std::fs::read(dir.path().join("Engagement Letter.pdf")).unwrap(),
            b"%PDF-1.4 synthetic\n"
        );

        let connect = || async {
            Ok(super::super::GmailClient::with_base_url(
                "t".into(),
                base.clone(),
            ))
        };
        let again = attachments_command(MESSAGE_ID, dir.path(), &all(), connect)
            .await
            .unwrap();
        assert!(again.iter().all(|line| line.ends_with("\talready present")));
    }

    #[tokio::test]
    async fn the_command_reports_nothing_when_no_attachment_matches() {
        let (_server, base) = gmail_serving(Some(FIXTURE)).await;
        let dir = tempfile::tempdir().unwrap();
        let connect = || async { Ok(super::super::GmailClient::with_base_url("t".into(), base)) };
        let lines = attachments_command(
            MESSAGE_ID,
            &dir.path().join("never-created"),
            &Selection {
                only: Some("*.docx"),
                include_inline: false,
            },
            connect,
        )
        .await
        .unwrap();
        assert!(lines.is_empty());
        assert!(!dir.path().join("never-created").exists());
    }

    #[tokio::test]
    async fn the_command_refuses_a_message_without_a_raw_copy() {
        let (_server, base) = gmail_serving(None).await;
        let dir = tempfile::tempdir().unwrap();
        let connect = || async { Ok(super::super::GmailClient::with_base_url("t".into(), base)) };
        let error = attachments_command(MESSAGE_ID, dir.path(), &all(), connect)
            .await
            .unwrap_err();
        assert!(format!("{error}").contains("no raw copy"));
    }

    #[tokio::test]
    async fn the_command_refuses_a_reference_that_names_no_message_before_connecting() {
        let dir = tempfile::tempdir().unwrap();
        let connect = || async {
            panic!("connect must not run for a bad reference");
            #[allow(unreachable_code)]
            Ok(super::super::GmailClient::with_base_url(
                "t".into(),
                url::Url::parse("http://127.0.0.1:1").unwrap(),
            ))
        };
        let error = attachments_command("not a message", dir.path(), &all(), connect)
            .await
            .unwrap_err();
        assert!(format!("{error}").contains("neither a Gmail web URL"));
    }

    #[test]
    fn only_and_include_inline_combine() {
        let attachments = extract(
            FIXTURE,
            &Selection {
                only: Some("*.png"),
                include_inline: true,
            },
        )
        .unwrap();
        assert_eq!(names(&attachments), ["logo.png"]);
        let none = extract(
            FIXTURE,
            &Selection {
                only: Some("*.png"),
                include_inline: false,
            },
        )
        .unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn identical_duplicates_collapse_and_numbering_skips_taken_names() {
        let attachment = |name: &str, bytes: &[u8]| ExtractedAttachment {
            name: name.into(),
            bytes: bytes.to_vec(),
        };
        let kept = disambiguate(vec![
            attachment("a.pdf", b"1"),
            attachment("a.pdf", b"1"),
            attachment("a.pdf", b"2"),
            attachment("a.pdf", b"3"),
            attachment("README", b"x"),
            attachment("README", b"y"),
            attachment(".hidden", b"p"),
            attachment(".hidden", b"q"),
        ]);
        assert_eq!(
            names(&kept),
            [
                "a.pdf",
                "a-2.pdf",
                "a-3.pdf",
                "README",
                "README-2",
                ".hidden",
                ".hidden-2"
            ]
        );
    }

    #[test]
    fn unnamed_parts_take_their_extension_from_the_mime_type() {
        for (ctype, subtype, extension) in [
            ("application", "pdf", "pdf"),
            ("message", "rfc822", "eml"),
            ("image", "png", "png"),
            ("image", "jpeg", "jpg"),
            ("image", "gif", "gif"),
            ("text", "plain", "txt"),
            ("text", "html", "html"),
            ("text", "calendar", "ics"),
            ("application", "zip", "bin"),
        ] {
            assert_eq!(extension_for(ctype, subtype), extension);
        }
    }

    #[test]
    fn a_message_that_is_not_mime_is_refused() {
        assert!(extract(b"", &all()).is_err());
    }

    #[test]
    fn report_lines_carry_name_size_and_digest_never_bytes() {
        let attachment = ExtractedAttachment {
            name: "a.txt".into(),
            bytes: b"abc".to_vec(),
        };
        assert_eq!(
            report_line(&attachment, &Written::Written),
            "a.txt\t3 bytes\tsha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\twritten"
        );
    }
}
