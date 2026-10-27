//! Local, explicit preparation of an upload-ready social MP4. Originals are never written.

use std::io::Read;
use std::path::Path;
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use tokio::process::Command;

const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const AUDIO_RATE: u64 = 128_000;

#[derive(Deserialize)]
struct Probe {
    streams: Vec<Video>,
    format: Format,
}

#[derive(Deserialize)]
struct Video {
    width: u32,
    height: u32,
    sample_aspect_ratio: Option<String>,
}

#[derive(Deserialize)]
struct Format {
    duration: String,
}

fn bitrate(duration: f64) -> Result<u64> {
    if !duration.is_finite() || !(3.0..=600.0).contains(&duration) {
        bail!("social video duration must be between 3 and 600 seconds");
    }
    // Reserve 5% for container overhead and encoding variation, plus the audio budget.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let total_rate = (MAX_BYTES as f64 * 8.0 * 0.95 / duration) as u64;
    Ok(total_rate.saturating_sub(AUDIO_RATE).min(8_000_000))
}

fn local_mp4(input: &Path) -> Result<()> {
    let metadata = input.metadata().context("read original video metadata")?;
    if !metadata.is_file() || metadata.len() > MAX_SOURCE_BYTES {
        bail!("choose a local MP4 file no larger than 2 GiB");
    }
    let mut header = [0; 12];
    std::fs::File::open(input)?
        .read_exact(&mut header)
        .context("read MP4 header")?;
    if &header[4..8] != b"ftyp" {
        bail!("input must be an MP4 container");
    }
    Ok(())
}

async fn probe(input: &Path) -> Result<Probe> {
    let output = tokio::time::timeout(
        Duration::from_secs(60),
        Command::new("ffprobe")
            .kill_on_drop(true)
            .args([
                "-v",
                "error",
                "-protocol_whitelist",
                "file,pipe",
                "-f",
                "mov",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height,sample_aspect_ratio:format=duration",
                "-of",
                "json",
            ])
            .arg(input)
            .stderr(Stdio::null())
            .output(),
    )
    .await
    .context("video inspection exceeded 60 seconds")?
    .context("run ffprobe; install FFmpeg with ffmpeg and ffprobe on PATH")?;
    if !output.status.success() {
        bail!("ffprobe could not inspect the MP4");
    }
    serde_json::from_slice(&output.stdout).context("decode video dimensions and duration")
}

async fn run(command: &mut Command) -> Result<()> {
    command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let status = tokio::time::timeout(Duration::from_secs(600), command.status())
        .await
        .context("video preparation exceeded 10 minutes; no output was published")?
        .context("run ffmpeg; install FFmpeg with ffmpeg and ffprobe on PATH")?;
    if !status.success() {
        bail!("ffmpeg could not prepare or decode the video; the original is unchanged");
    }
    Ok(())
}

async fn encode(
    input: &Path,
    output: &Path,
    rate: u64,
    duration: f64,
    passlog: &Path,
) -> Result<()> {
    for pass in ["1", "2"] {
        eprintln!("Encoding pass {pass}/2…");
        let mut command = Command::new("ffmpeg");
        command.args(["-nostdin", "-v", "error", "-y", "-protocol_whitelist", "file,pipe", "-f", "mov", "-i"])
            .arg(input)
            .args(["-map", "0:v:0", "-map", "0:a:0?", "-map_metadata", "-1", "-map_chapters", "-1",
                "-vf", "scale=w='min(1920,iw)':h='min(1920,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2,setsar=1,fps=30",
                "-c:v", "libx264", "-preset", "medium", "-pix_fmt", "yuv420p", "-threads", "2", "-filter_threads", "1", "-b:v"])
            .arg(rate.to_string())
            .args(["-t", &duration.to_string()])
            .args(["-pass", pass, "-passlogfile"]).arg(passlog)
            .args(["-c:a", "aac", "-b:a", "128k", "-movflags", "+faststart", "-f", "mp4"])
            .arg(output);
        run(&mut command).await?;
    }
    Ok(())
}

async fn prepare(input: &Path, output: &Path) -> Result<()> {
    if output.extension().and_then(|ext| ext.to_str()) != Some("mp4") {
        bail!("output must have an .mp4 extension");
    }
    // symlink_metadata also protects an existing dangling symlink.
    if output.symlink_metadata().is_ok() {
        bail!("output already exists; choose a new path");
    }
    local_mp4(input)?;
    let input = input.canonicalize().context("resolve original MP4 path")?;
    let inspected = probe(&input).await?;
    let Some(source_video) = inspected.streams.first() else {
        bail!("input has no video track");
    };
    if source_video
        .sample_aspect_ratio
        .as_deref()
        .is_some_and(|ratio| !matches!(ratio, "1:1" | "N/A"))
    {
        bail!("input must use square pixels; convert its display aspect ratio before preparation");
    }
    let duration: f64 = inspected
        .format
        .duration
        .parse()
        .context("read video duration")?;
    let rate = bitrate(duration)?;
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let prepared = tempfile::Builder::new()
        .prefix(".navigator-video-")
        .suffix(".mp4")
        .tempfile_in(parent)
        .context("create temporary output beside destination")?;
    let logs = tempfile::tempdir().context("create temporary encoding pass data")?;
    let passlog = logs.path().join("pass");
    eprintln!(
        "Preparing H.264 MP4, up to 1920 pixels on its longest side, 30 fps; original unchanged."
    );
    encode(&input, prepared.path(), rate, duration, &passlog).await?;
    let bytes = prepared.as_file().metadata()?.len();
    if bytes == 0 || bytes > MAX_BYTES {
        bail!("prepared video exceeds the 64 MiB upload budget; no output was published");
    }
    let checked = probe(prepared.path()).await?;
    let checked_duration: f64 = checked
        .format
        .duration
        .parse()
        .context("read prepared duration")?;
    if !checked_duration.is_finite() || (checked_duration - duration).abs() > 0.2 {
        bail!("prepared video duration differs from the original; no output was saved");
    }
    let Some(video) = checked.streams.first() else {
        bail!("prepared MP4 has no video track")
    };
    if video.width.min(video.height) < 360 || video.width.max(video.height) > 1920 {
        bail!("prepared video must have at least 360 pixels on each side and at most 1920 on its longest side");
    }
    let mut decode = Command::new("ffmpeg");
    decode
        .args([
            "-nostdin",
            "-v",
            "error",
            "-xerror",
            "-protocol_whitelist",
            "file,pipe",
            "-f",
            "mov",
            "-i",
        ])
        .arg(prepared.path())
        .args(["-f", "null", "-"]);
    run(&mut decode).await?;
    prepared
        .persist_noclobber(output)
        .context("save prepared MP4 without replacing an existing file")?;
    println!(
        "Prepared MP4: {bytes} bytes, {}×{}, 30 fps, video budget {rate} bits/s. Inspect video and audio before uploading.",
        video.width,
        video.height
    );
    println!("Preparation is local only; no asset was uploaded and no social post was sent.");
    Ok(())
}

pub async fn run_prepare(input: &Path, output: &Path) -> ExitCode {
    match prepare(input, output).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator: video preparation: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};

    #[test]
    fn duration_budget_rejects_invalid_values_and_reserves_audio_and_overhead() {
        for value in [f64::NAN, f64::INFINITY, -1.0, 0.0, 2.9, 600.1] {
            assert!(bitrate(value).is_err(), "accepted invalid duration {value}");
        }
        assert_eq!(bitrate(3.0).unwrap(), 8_000_000);
        assert!((bitrate(600.0).unwrap() + AUDIO_RATE) * 600 / 8 < MAX_BYTES);
    }

    #[tokio::test]
    async fn existing_output_is_preserved_even_with_invalid_input() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("ready.mp4");
        std::fs::write(&output, b"keep existing bytes").unwrap();
        assert!(prepare(&dir.path().join("missing.mp4"), &output)
            .await
            .unwrap_err()
            .to_string()
            .contains("already exists"));
        assert_eq!(std::fs::read(output).unwrap(), b"keep existing bytes");
    }

    #[tokio::test]
    async fn corrupt_source_does_not_create_an_output() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("original.mp4");
        let output = dir.path().join("ready.mp4");
        std::fs::write(&input, b"not a video container").unwrap();
        assert!(prepare(&input, &output).await.is_err());
        assert!(!output.exists());
        assert_eq!(std::fs::read(input).unwrap(), b"not a video container");
    }

    #[test]
    fn oversized_source_is_refused_without_reading_it_into_memory() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("oversized.mp4");
        std::fs::File::create(&input)
            .unwrap()
            .set_len(MAX_SOURCE_BYTES + 1)
            .unwrap();
        assert!(local_mp4(&input).unwrap_err().to_string().contains("2 GiB"));
    }

    #[tokio::test]
    #[ignore = "requires ffmpeg and ffprobe; run explicitly for media changes"]
    async fn prepares_real_portrait_video_without_changing_original() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("original.mp4");
        let output = dir.path().join("ready.mp4");
        let mut fixture = Command::new("ffmpeg");
        fixture
            .args([
                "-nostdin",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=1080x1920:rate=24",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440",
                "-t",
                "3",
                "-c:v",
                "libx264",
                "-threads",
                "2",
                "-c:a",
                "aac",
            ])
            .arg(&input);
        run(&mut fixture).await.unwrap();
        // A valid sparse MP4 free box makes the source larger than the upload ceiling.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&input)
            .unwrap();
        file.write_all(&(70_u32 * 1024 * 1024).to_be_bytes())
            .unwrap();
        file.write_all(b"free").unwrap();
        let end = file.seek(SeekFrom::End(0)).unwrap() + 70 * 1024 * 1024 - 8;
        file.set_len(end).unwrap();
        drop(file);
        assert!(input.metadata().unwrap().len() > MAX_BYTES);
        let original = std::fs::read(&input).unwrap();
        prepare(&input, &output).await.unwrap();
        assert_eq!(std::fs::read(&input).unwrap(), original);
        assert!(output.metadata().unwrap().len() <= MAX_BYTES);
        let checked = probe(&output).await.unwrap();
        assert_eq!(
            (checked.streams[0].width, checked.streams[0].height),
            (1080, 1920)
        );
    }
}
