//! `TikTok` Accounts API video requests. App approval and account authorization
//! are checked by the publisher before this request reaches the transport.

use serde::{Deserialize, Serialize};

/// The Business API's own-business disclosure is always enabled for a Brand.
/// Paid partnerships and drafts require a different editorial flow.
#[derive(Serialize)]
pub(crate) struct VideoRequest<'a> {
    pub(crate) business_id: &'a str,
    pub(crate) video_url: &'a str,
    pub(crate) post_info: PostInfo<'a>,
}

// These booleans are the provider's exact JSON disclosure/interaction contract.
#[allow(clippy::struct_excessive_bools)]
#[derive(Serialize)]
pub(crate) struct PostInfo<'a> {
    caption: &'a str,
    is_brand_organic: bool,
    is_branded_content: bool,
    is_ai_generated: bool,
    disable_comment: bool,
    disable_duet: bool,
    disable_stitch: bool,
    upload_to_draft: bool,
    is_ads_only: bool,
}

impl<'a> VideoRequest<'a> {
    pub(crate) fn new(
        business_id: &'a str,
        video_url: &'a str,
        caption: &'a str,
        ai_generated: bool,
    ) -> Result<Self, &'static str> {
        if business_id.trim().is_empty() {
            return Err("The authorized TikTok account identity is missing");
        }
        if caption.encode_utf16().count() > 2200 {
            return Err("TikTok captions permit at most 2200 UTF-16 units");
        }
        if caption
            .chars()
            .filter(|character| *character == '@')
            .count()
            > 30
        {
            return Err("TikTok captions permit at most 30 mentions");
        }
        let url = url::Url::parse(video_url).map_err(|_| "The video URL is invalid")?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("TikTok requires a public HTTPS video URL without credentials");
        }
        Ok(Self {
            business_id,
            video_url,
            post_info: PostInfo {
                caption,
                is_brand_organic: true,
                is_branded_content: false,
                is_ai_generated: ai_generated,
                disable_comment: true,
                disable_duet: true,
                disable_stitch: true,
                upload_to_draft: false,
                is_ads_only: false,
            },
        })
    }
}

/// A task receipt is not a published post. Only the provider's public post
/// identifiers allow the publisher to advance to confirmation and link lookup.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PublishStatus {
    Processing,
    Complete(Vec<String>),
    Failed,
    Draft,
}

#[derive(Deserialize)]
struct StatusEnvelope {
    code: i64,
    data: Option<StatusData>,
}

#[derive(Deserialize)]
struct StatusData {
    status: String,
    #[serde(default)]
    post_ids: Vec<String>,
}

pub(crate) fn publish_status(bytes: &[u8]) -> Result<PublishStatus, &'static str> {
    if bytes.len() > 64 * 1024 {
        return Err("TikTok status response is too large");
    }
    let envelope: StatusEnvelope =
        serde_json::from_slice(bytes).map_err(|_| "TikTok status response is invalid")?;
    if envelope.code != 0 {
        return Err("TikTok could not verify the publishing status");
    }
    let data = envelope
        .data
        .ok_or("TikTok status response is incomplete")?;
    match data.status.as_str() {
        "PROCESSING_DOWNLOAD" => Ok(PublishStatus::Processing),
        "PUBLISH_COMPLETE" if data.post_ids.is_empty() => Ok(PublishStatus::Processing),
        "PUBLISH_COMPLETE"
            if data.post_ids.len() <= 20
                && data
                    .post_ids
                    .iter()
                    .all(|id| !id.trim().is_empty() && id.len() <= 256) =>
        {
            Ok(PublishStatus::Complete(data.post_ids))
        }
        "FAILED" => Ok(PublishStatus::Failed),
        "SEND_TO_USER_INBOX" => Ok(PublishStatus::Draft),
        _ => Err("TikTok publishing status is not recognized"),
    }
}

/// MP4 media timing measured from the visual track, never from editorial flags.
/// Fragmented MP4 and missing timing tables fail closed.
pub(crate) fn video_duration(bytes: &[u8], max_seconds: u32) -> Result<f64, &'static str> {
    if bytes.len() > 1_000_000_000 {
        return Err("TikTok videos must be at most 1 GB");
    }
    let moov = child(bytes, *b"moov").ok_or("The MP4 timing metadata is unavailable")?;
    if child(moov, *b"mvex").is_some() {
        return Err("Fragmented MP4 timing is not supported");
    }
    for (kind, track) in boxes(moov)? {
        if &kind != b"trak" {
            continue;
        }
        let Some(media) = child(track, *b"mdia") else {
            continue;
        };
        let Some(handler) = child(media, *b"hdlr") else {
            continue;
        };
        if handler.get(8..12) != Some(b"vide".as_slice()) {
            continue;
        }
        let timing = child(media, *b"mdhd").ok_or("The video duration cannot be measured")?;
        let (scale_at, duration_at, wide) = match timing.first() {
            Some(0) => (12, 16, false),
            Some(1) => (20, 24, true),
            _ => return Err("The video timing version is unsupported"),
        };
        let scale = u32_at(timing, scale_at).ok_or("The video timescale is truncated")?;
        let ticks = if wide {
            u64::from_be_bytes(
                timing
                    .get(duration_at..duration_at + 8)
                    .and_then(|v| v.try_into().ok())
                    .ok_or("The video duration is truncated")?,
            )
        } else {
            u64::from(u32_at(timing, duration_at).ok_or("The video duration is truncated")?)
        };
        if scale == 0 || ticks == 0 {
            return Err("The video duration is invalid");
        }
        let seconds = f64::from(
            u32::try_from(ticks).map_err(|_| "The video timing exceeds supported precision")?,
        ) / f64::from(scale);
        if seconds < 3.0 || seconds > f64::from(max_seconds.min(600)) {
            return Err("The video duration exceeds TikTok's account limits");
        }
        let stts = child(media, *b"minf")
            .and_then(|v| child(v, *b"stbl"))
            .and_then(|v| child(v, *b"stts"))
            .ok_or("The video frame rate cannot be measured")?;
        let entries = u32_at(stts, 4).ok_or("The video timing table is truncated")?;
        if entries > 36_000 {
            return Err("The video timing table exceeds supported limits");
        }
        let mut elapsed = 0_u64;
        for index in 0..entries {
            let offset = usize::try_from(index)
                .ok()
                .and_then(|i| i.checked_mul(8))
                .and_then(|i| i.checked_add(8))
                .ok_or("The video timing table is invalid")?;
            let samples = u32_at(stts, offset).ok_or("The video timing table is truncated")?;
            let delta = u32_at(stts, offset + 4).ok_or("The video timing table is truncated")?;
            if samples == 0
                || delta == 0
                || u64::from(scale) < 23 * u64::from(delta)
                || u64::from(scale) > 60 * u64::from(delta)
            {
                return Err("TikTok videos require 23 to 60 frames per second");
            }
            elapsed = elapsed
                .checked_add(u64::from(samples) * u64::from(delta))
                .ok_or("The video timing table is invalid")?;
        }
        if elapsed != ticks {
            return Err("The video timing tables disagree");
        }
        return Ok(seconds);
    }
    Err("The MP4 has no measurable visual track")
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn child(bytes: &[u8], name: [u8; 4]) -> Option<&[u8]> {
    boxes(bytes)
        .ok()?
        .into_iter()
        .find(|(kind, _)| *kind == name)
        .map(|(_, data)| data)
}

type MediaBox<'a> = ([u8; 4], &'a [u8]);

fn boxes(mut bytes: &[u8]) -> Result<Vec<MediaBox<'_>>, &'static str> {
    let mut found = Vec::new();
    while !bytes.is_empty() {
        if found.len() >= 4096 {
            return Err("The MP4 metadata exceeds supported limits");
        }
        let size = u32_at(bytes, 0).ok_or("The MP4 box is truncated")?;
        let kind = bytes
            .get(4..8)
            .and_then(|v| v.try_into().ok())
            .ok_or("The MP4 box is truncated")?;
        let (size, header) = if size == 1 {
            let long = u64::from_be_bytes(
                bytes
                    .get(8..16)
                    .and_then(|v| v.try_into().ok())
                    .ok_or("The MP4 box is truncated")?,
            );
            (
                usize::try_from(long).map_err(|_| "The MP4 box is too large")?,
                16,
            )
        } else if size == 0 {
            (bytes.len(), 8)
        } else {
            (
                usize::try_from(size).map_err(|_| "The MP4 box is too large")?,
                8,
            )
        };
        if size < header || size > bytes.len() {
            return Err("The MP4 box length is invalid");
        }
        found.push((kind, &bytes[header..size]));
        bytes = &bytes[size..];
    }
    Ok(found)
}

/// A verified app URL property must cover the permanent public media URL.
pub(crate) fn verified_property(properties: &serde_json::Value, media: &str) -> bool {
    let Ok(media) = url::Url::parse(media) else {
        return false;
    };
    let Some(host) = media.domain() else {
        return false;
    };
    if media.scheme() != "https"
        || !media.username().is_empty()
        || media.password().is_some()
        || media.port().is_some()
        || media.query().is_some()
        || media.fragment().is_some()
    {
        return false;
    }
    properties
        .get("url_property_info_list")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|properties| {
            properties.iter().any(|property| {
                if property
                    .get("property_status")
                    .and_then(serde_json::Value::as_u64)
                    != Some(1)
                {
                    return false;
                }
                let Some(value) = property.get("url").and_then(serde_json::Value::as_str) else {
                    return false;
                };
                let property_url = if value.contains("://") {
                    value.to_string()
                } else {
                    format!("https://{value}")
                };
                let Ok(property_url) = url::Url::parse(&property_url) else {
                    return false;
                };
                let Some(domain) = property_url.domain() else {
                    return false;
                };
                if property_url.scheme() != "https"
                    || !property_url.username().is_empty()
                    || property_url.password().is_some()
                    || property_url.port().is_some()
                    || property_url.query().is_some()
                    || property_url.fragment().is_some()
                {
                    return false;
                }
                match property
                    .get("property_type")
                    .and_then(serde_json::Value::as_u64)
                {
                    Some(1) => {
                        property_url.path() == "/"
                            && (host == domain || host.ends_with(&format!(".{domain}")))
                    }
                    Some(2) => host == domain && media.path().starts_with(property_url.path()),
                    _ => false,
                }
            })
        })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{publish_status, verified_property, video_duration, PublishStatus, VideoRequest};

    fn boxed(kind: [u8; 4], body: &[u8]) -> Vec<u8> {
        let mut value = u32::try_from(body.len() + 8)
            .expect("small fixture")
            .to_be_bytes()
            .to_vec();
        value.extend(kind);
        value.extend(body);
        value
    }

    pub(crate) fn video(seconds: u32, fps: u32) -> Vec<u8> {
        let mut handler = vec![0; 8];
        handler.extend(b"vide");
        let mut timing = vec![0; 12];
        timing.extend((fps * 100).to_be_bytes());
        timing.extend((fps * 100 * seconds).to_be_bytes());
        let mut stts = vec![0; 4];
        stts.extend(1_u32.to_be_bytes());
        stts.extend((fps * seconds).to_be_bytes());
        stts.extend(100_u32.to_be_bytes());
        let mut media = boxed(*b"hdlr", &handler);
        media.extend(boxed(*b"mdhd", &timing));
        media.extend(boxed(*b"minf", &boxed(*b"stbl", &boxed(*b"stts", &stts))));
        boxed(*b"moov", &boxed(*b"trak", &boxed(*b"mdia", &media)))
    }

    #[test]
    fn media_requires_verified_property_and_domain_boundary() {
        let properties = serde_json::json!({"url_property_info_list":[{"property_type":1,"property_status":1,"url":"example.test"}]});
        assert!(verified_property(
            &properties,
            "https://assets.example.test/video.mp4"
        ));
        assert!(!verified_property(
            &properties,
            "https://evilexample.test/video.mp4"
        ));
        assert!(!verified_property(
            &properties,
            "https://assets.example.test/video.mp4?expires=1"
        ));
        let pending = serde_json::json!({"url_property_info_list":[{"property_type":1,"property_status":0,"url":"example.test"}]});
        assert!(!verified_property(
            &pending,
            "https://assets.example.test/video.mp4"
        ));
        let prefix = serde_json::json!({"url_property_info_list":[{"property_type":2,"property_status":1,"url":"https://assets.example.test/approved/"}]});
        assert!(verified_property(
            &prefix,
            "https://assets.example.test/approved/video.mp4"
        ));
        assert!(!verified_property(
            &prefix,
            "https://assets.example.test/unapproved/video.mp4"
        ));
    }

    #[test]
    fn measured_duration_and_frame_rate_obey_current_business_limits() {
        assert_eq!(video_duration(&video(30, 30), 600), Ok(30.0));
        for (duration, fps, limit) in [
            (2, 30, 600),
            (601, 30, 600),
            (30, 22, 600),
            (30, 61, 600),
            (30, 30, 20),
        ] {
            assert!(video_duration(&video(duration, fps), limit).is_err());
        }
        assert!(video_duration(b"truncated", 600).is_err());
        let fragmented = boxed(*b"moov", &boxed(*b"mvex", &[]));
        assert!(video_duration(&fragmented, 600).is_err());
    }

    #[test]
    fn task_completion_without_public_ids_keeps_polling() {
        assert_eq!(
            publish_status(br#"{"code":0,"data":{"status":"PUBLISH_COMPLETE","post_ids":[]}}"#),
            Ok(PublishStatus::Processing)
        );
        assert_eq!(
            publish_status(
                br#"{"code":0,"data":{"status":"PUBLISH_COMPLETE","post_ids":["123"]}}"#
            ),
            Ok(PublishStatus::Complete(vec!["123".into()]))
        );
        assert_eq!(
            publish_status(br#"{"code":0,"data":{"status":"SEND_TO_USER_INBOX"}}"#),
            Ok(PublishStatus::Draft)
        );
        assert_eq!(
            publish_status(br#"{"code":0,"data":{"status":"FAILED"}}"#),
            Ok(PublishStatus::Failed)
        );
    }

    #[test]
    fn unreadable_or_future_status_is_never_confirmed() {
        for bytes in [
            b"not json".as_slice(),
            br#"{"code":1,"data":{"status":"PUBLISH_COMPLETE","post_ids":["123"]}}"#,
            br#"{"code":0,"data":{"status":"UNKNOWN"}}"#,
            br#"{"code":0}"#,
        ] {
            assert!(publish_status(bytes).is_err());
        }
    }

    #[test]
    fn own_brand_disclosures_and_disabled_interactions_are_explicit() {
        let request = VideoRequest::new(
            "synthetic-account",
            "https://media.example/video.mp4",
            "Approved",
            true,
        )
        .expect("synthetic request");
        let body = serde_json::to_value(request).expect("serialize request");
        assert_eq!(body["business_id"], "synthetic-account");
        let info = &body["post_info"];
        for field in [
            "is_brand_organic",
            "is_ai_generated",
            "disable_comment",
            "disable_duet",
            "disable_stitch",
        ] {
            assert_eq!(info[field], true);
        }
        for field in ["is_branded_content", "upload_to_draft", "is_ads_only"] {
            assert_eq!(info[field], false);
        }
    }

    #[test]
    fn caption_limit_counts_utf16_units() {
        let caption = "🦀".repeat(1101);
        assert!(VideoRequest::new(
            "account",
            "https://media.example/video.mp4",
            &caption,
            false
        )
        .is_err());
        let caption = "🦀".repeat(1100);
        assert!(VideoRequest::new(
            "account",
            "https://media.example/video.mp4",
            &caption,
            false
        )
        .is_ok());
    }

    #[test]
    fn credentials_and_non_https_urls_are_refused() {
        for video in [
            "http://media.example/video.mp4",
            "https://user:secret@media.example/video.mp4",
            "file:///video.mp4",
        ] {
            assert!(VideoRequest::new("account", video, "Approved", false).is_err());
        }
        assert!(
            VideoRequest::new(" ", "https://media.example/video.mp4", "Approved", false).is_err()
        );
    }
}
