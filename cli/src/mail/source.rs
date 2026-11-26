//! The original message and its Project repository pointer.
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use model::document_pointers::{DocumentPointer, MatterFolder};
use model::documents::Evidence;

use crate::remote::{DocumentClient, DocumentExecution};

use super::{mail_slug_ref, ParsedEmlFile};

/// Gmail returns its thread ID outside of the RFC 5322 bytes. Key the local
/// receipt by content so renaming a saved copy cannot misattribute a thread.
pub(super) fn thread_receipt(path: &Path, digest: &str) -> PathBuf {
    path.with_file_name(format!(".{digest}.gmail-thread"))
}

pub(super) struct SourceFile<'a> {
    pub path: &'a Path,
    pub checkout: Option<&'a Path>,
    pub visibility: &'a str,
    pub thread_id: Option<&'a str>,
}

impl SourceFile<'_> {
    fn evidence(&self, parsed: &ParsedEmlFile, digest: &str) -> Result<Evidence> {
        let receipt = match std::fs::read_to_string(thread_receipt(self.path, digest)) {
            Ok(id) => Some(id),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error).context("read the saved Gmail thread ID"),
        };
        let thread = self
            .thread_id
            .or(receipt.as_deref())
            .or(parsed.gmail_thread.as_deref());
        let source = match thread {
            Some(id) => {
                ensure!(
                    !id.is_empty()
                        && id.len() <= 128
                        && id
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
                    "invalid Gmail thread ID"
                );
                format!("Gmail thread {id}")
            }
            None => parsed.source_ref.clone(),
        };
        Ok(Evidence {
            dated_at: Some(parsed.dated_at),
            source: Some(source),
            ..Evidence::default()
        })
    }

    pub(super) async fn upload(
        &self,
        client: &DocumentClient,
        parsed: &ParsedEmlFile,
        bytes: &[u8],
        kind: &str,
        provenance: serde_json::Value,
    ) -> Result<(String, DocumentPointer, Evidence)> {
        let digest = model::documents::sha256_hex(bytes);
        let evidence = self.evidence(parsed, &digest)?;
        let filename = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .context("missing .eml filename")?;
        let dated = dated_filename(filename, parsed.dated_at);
        let slug_ref = mail_slug_ref(&parsed.message_ref);
        let stored_name = format!("{slug_ref}.eml");
        let target = self
            .source_slug(client, &slug_ref, &dated, &stored_name, &digest)
            .await?;
        let mut pointer = client
            .upload_bytes_with_details(
                &stored_name,
                bytes,
                kind,
                Some(self.visibility),
                None,
                Some("message/rfc822"),
                Some(&target),
                None,
                None,
                None,
                &DocumentExecution {
                    evidence: Some(evidence.clone()),
                    ..DocumentExecution::default()
                },
                Some(provenance),
            )
            .await?;
        ensure!(pointer.current_version.sha256 == digest && pointer.visibility == self.visibility,
            "the returned mail revision differs in content or visibility; review the existing document");
        // The buffered door can deduplicate across slugs. Validate the
        // published name before recording it, even on a concurrent retry.
        let revisions = client.list_revisions(&target).await?;
        ensure!(
            revisions.revisions.first().is_some_and(
                |head| head.operative && head.asset_id == pointer.current_version.asset_id
            ),
            "the source slug does not resolve to the filed message; retry filing"
        );
        let folder = MatterFolder::MailCorrespondence;
        if pointer.folder != Some(folder) {
            client
                .set_folder(pointer.current_version.asset_id, Some(folder.as_str()))
                .await?;
            pointer.folder = Some(folder);
        }
        Ok((target, pointer, evidence))
    }

    async fn source_slug(
        &self,
        client: &DocumentClient,
        slug_ref: &str,
        dated: &str,
        stored_name: &str,
        digest: &str,
    ) -> Result<String> {
        let prefix = format!("mail/{slug_ref}/");
        let slug = format!("{prefix}{dated}");
        // Reuse the filed asset so a Communication created on an earlier
        // filing continues to resolve to the same original bytes.
        let documents = client.list_documents().await?;
        ensure!(!documents.iter().any(|row| {
            row.filename.as_deref() == Some(stored_name)
                && row.slug.as_deref().is_some_and(|held| held.starts_with(&prefix))
                && row.sha256_hex != digest
        }), "this message reference already has different original bytes; review the existing filing");
        let existing = documents
            .iter()
            .find(|row| row.sha256_hex == digest && row.filename.as_deref() == Some(stored_name));

        if let Some(row) = existing {
            let held = row
                .slug
                .as_deref()
                .context("filed message has no document slug")?;
            ensure!(held.starts_with(&prefix), "the same bytes and filename are already filed under a different document identity; review the existing filing");
            ensure!(row.visibility == self.visibility, "the filed message has different visibility; review the existing document before filing");
            if held == format!("{prefix}source.eml") || held == format!("{prefix}source") {
                if let Some(root) = self.checkout {
                    crate::remote::reslug_with_local(client, row.id, &slug, root, false).await?;
                } else {
                    client.reslug(row.id, &slug, false).await?;
                }
            } else {
                return Ok(held.to_owned());
            }
        }
        Ok(slug)
    }
}

fn dated_filename(filename: &str, message_date: chrono::NaiveDate) -> String {
    if rules::c004::is_conventional_filename(filename) {
        return filename.to_owned();
    }
    let (date, name) = filename
        .split_once('_')
        .and_then(|(date, name)| {
            chrono::NaiveDate::parse_from_str(date, "%Y.%m.%d")
                .ok()
                .map(|date| (date, name))
        })
        .unwrap_or((message_date, filename));
    let stem = Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(name);
    let slug = rules::c004::name_slug(stem).unwrap_or_else(|| "message".into());
    format!("{}_{slug}.eml", date.format("%Y.%m.%d"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn a_refiled_message_keeps_its_document_identity() {
        let server = MockServer::start().await;
        let project = uuid::Uuid::now_v7();
        let asset = uuid::Uuid::now_v7();
        let old = "mail/ref/source.eml";
        let dated = "mail/ref/2025.10.09_saved.eml";
        let row = |slug: &str| {
            json!({
                "id": asset, "byte_size": 5, "sha256_hex": "digest", "filename": "ref.eml",
                "visibility": "internal", "slug": slug, "inserted_at": "2025-10-09T00:00:00Z"
            })
        };
        let list = format!("/app/api/projects/{project}/documents");
        Mock::given(method("GET"))
            .and(path(&list))
            .respond_with(ResponseTemplate::new(200).set_body_json([row(old)]))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(&list))
            .respond_with(ResponseTemplate::new(200).set_body_json([row(dated)]))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(format!("{list}/{asset}")))
            .and(wiremock::matchers::body_partial_json(
                json!({"slug":dated,"reslug":true}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"slug":dated})))
            .expect(1)
            .mount(&server)
            .await;
        let client = DocumentClient::for_check_tests(server.uri(), project);
        let source = SourceFile {
            path: Path::new("saved.eml"),
            checkout: None,
            visibility: "internal",
            thread_id: None,
        };
        for filename in ["2025.10.09_saved.eml", "2025.10.09_renamed.eml"] {
            assert_eq!(
                source
                    .source_slug(&client, "ref", filename, "ref.eml", "digest")
                    .await
                    .unwrap(),
                dated
            );
        }
        let client_visible = SourceFile {
            visibility: "client",
            ..source
        };
        assert!(client_visible
            .source_slug(&client, "ref", "saved.eml", "ref.eml", "digest")
            .await
            .is_err());
    }
}
