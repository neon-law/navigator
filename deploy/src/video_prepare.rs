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

fn ffprobe(input: &Path) -> Command {
    let mut command = Command::new("ffprobe");
    command
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
        .arg(input);
    command
}

async fn probe(input: &Path) -> Result<Probe> {
    let output = tokio::time::timeout(
        Duration::from_secs(60),
        ffprobe(input)
            .kill_on_drop(true)
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

fn encode_pass(
    input: &Path,
    output: &Path,
    rate: u64,
    duration: f64,
    passlog: &Path,
    pass: &str,
) -> Command {
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
    command
}

fn decode_check(prepared: &Path) -> Command {
    let mut command = Command::new("ffmpeg");
    command
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
        .arg(prepared)
        .args(["-f", "null", "-"]);
    command
}

/// The original's duration and the video bitrate that keeps it inside the upload budget.
fn source_plan(inspected: &Probe) -> Result<(f64, u64)> {
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
    Ok((duration, bitrate(duration)?))
}

/// The prepared file's video track, once its size, duration, and dimensions are publishable.
fn publishable(bytes: u64, checked: &Probe, duration: f64) -> Result<&Video> {
    if bytes == 0 || bytes > MAX_BYTES {
        bail!("prepared video exceeds the 64 MiB upload budget; no output was published");
    }
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
    Ok(video)
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
    let (duration, rate) = source_plan(&probe(&input).await?)?;
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
    for pass in ["1", "2"] {
        eprintln!("Encoding pass {pass}/2…");
        run(&mut encode_pass(
            &input,
            prepared.path(),
            rate,
            duration,
            &passlog,
            pass,
        ))
        .await?;
    }
    let bytes = prepared.as_file().metadata()?.len();
    let checked = probe(prepared.path()).await?;
    let video = publishable(bytes, &checked, duration)?;
    run(&mut decode_check(prepared.path())).await?;
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

    /// A file whose only box is a valid `ftyp` header: no streams follow.
    fn mp4_header_only(path: &Path) {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&24_u32.to_be_bytes());
        bytes.extend_from_slice(b"ftypisom");
        bytes.extend_from_slice(&[0; 12]);
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn local_mp4_accepts_only_an_ftyp_container_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(local_mp4(&dir.path().join("missing.mp4"))
            .unwrap_err()
            .to_string()
            .contains("metadata"));
        assert!(local_mp4(dir.path())
            .unwrap_err()
            .to_string()
            .contains("local MP4 file"));

        let short = dir.path().join("short.mp4");
        std::fs::write(&short, b"tiny").unwrap();
        assert!(local_mp4(&short)
            .unwrap_err()
            .to_string()
            .contains("MP4 header"));

        let other = dir.path().join("other.mp4");
        std::fs::write(&other, b"\0\0\0\x18moovabcdefgh").unwrap();
        assert!(local_mp4(&other)
            .unwrap_err()
            .to_string()
            .contains("MP4 container"));

        let valid = dir.path().join("valid.mp4");
        mp4_header_only(&valid);
        local_mp4(&valid).unwrap();
    }

    #[tokio::test]
    async fn output_must_be_a_new_mp4_path() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("original.mp4");
        mp4_header_only(&input);
        for name in ["ready.mov", "ready"] {
            assert!(prepare(&input, &dir.path().join(name))
                .await
                .unwrap_err()
                .to_string()
                .contains(".mp4 extension"));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_dangling_symlink_output_is_never_followed() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("original.mp4");
        mp4_header_only(&input);
        let target = dir.path().join("elsewhere.mp4");
        let output = dir.path().join("ready.mp4");
        std::os::unix::fs::symlink(&target, &output).unwrap();
        assert!(prepare(&input, &output)
            .await
            .unwrap_err()
            .to_string()
            .contains("already exists"));
        assert!(!target.exists(), "the link target is never created");
    }

    #[tokio::test]
    async fn a_header_only_mp4_fails_inspection_without_an_output() {
        // Passes the local container check, then fails at ffprobe — either
        // because FFmpeg is absent or because the file carries no streams.
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("original.mp4");
        let output = dir.path().join("ready.mp4");
        mp4_header_only(&input);
        let error = prepare(&input, &output).await.unwrap_err().to_string();
        assert!(
            error.contains("ffprobe") || error.contains("no video track"),
            "{error}"
        );
        assert!(!output.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn run_prepare_reports_failure_as_an_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            run_prepare(
                &dir.path().join("missing.mp4"),
                &dir.path().join("ready.txt")
            )
            .await,
            ExitCode::FAILURE
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_maps_exit_status_and_spawn_failure_to_errors() {
        run(&mut Command::new("true")).await.unwrap();
        assert!(run(&mut Command::new("false"))
            .await
            .unwrap_err()
            .to_string()
            .contains("original is unchanged"));
        assert!(run(&mut Command::new("navigator-no-such-ffmpeg-binary"))
            .await
            .unwrap_err()
            .to_string()
            .contains("install FFmpeg"));
    }

    #[test]
    fn probe_output_decodes_dimensions_aspect_and_duration() {
        let probe: Probe = serde_json::from_str(
            r#"{"streams":[{"width":1080,"height":1920,"sample_aspect_ratio":"1:1"}],
                "format":{"duration":"12.500000"}}"#,
        )
        .unwrap();
        assert_eq!(
            (probe.streams[0].width, probe.streams[0].height),
            (1080, 1920)
        );
        assert_eq!(probe.streams[0].sample_aspect_ratio.as_deref(), Some("1:1"));
        assert_eq!(probe.format.duration, "12.500000");
        let bare: Probe = serde_json::from_str(
            r#"{"streams":[{"width":640,"height":360}],"format":{"duration":"3"}}"#,
        )
        .unwrap();
        assert!(bare.streams[0].sample_aspect_ratio.is_none());
    }

    fn inspected(streams: &str, duration: &str) -> Probe {
        serde_json::from_str(&format!(
            r#"{{"streams":[{streams}],"format":{{"duration":"{duration}"}}}}"#
        ))
        .unwrap()
    }

    fn refusal<T>(result: Result<T>) -> String {
        result.err().expect("refused").to_string()
    }

    #[test]
    fn the_source_plan_needs_a_square_pixel_video_track_of_a_postable_length() {
        let square = r#"{"width":1080,"height":1920,"sample_aspect_ratio":"1:1"}"#;
        assert_eq!(
            source_plan(&inspected(square, "3.0")).unwrap(),
            (3.0, 8_000_000)
        );
        let unknown = r#"{"width":1080,"height":1920,"sample_aspect_ratio":"N/A"}"#;
        assert!(source_plan(&inspected(unknown, "30")).is_ok());
        assert_eq!(
            refusal(source_plan(&inspected("", "30"))),
            "input has no video track"
        );
        let anamorphic = r#"{"width":720,"height":480,"sample_aspect_ratio":"8:9"}"#;
        assert!(refusal(source_plan(&inspected(anamorphic, "30"))).contains("square pixels"));
        assert_eq!(
            refusal(source_plan(&inspected(square, "N/A"))),
            "read video duration"
        );
        assert!(refusal(source_plan(&inspected(square, "601"))).contains("between 3 and 600"));
    }

    #[test]
    fn a_prepared_file_publishes_only_inside_the_size_duration_and_dimension_limits() {
        let portrait = inspected(r#"{"width":1080,"height":1920}"#, "12.1");
        let video = publishable(1, &portrait, 12.0).unwrap();
        assert_eq!((video.width, video.height), (1080, 1920));
        for bytes in [0, MAX_BYTES + 1] {
            assert!(refusal(publishable(bytes, &portrait, 12.0)).contains("64 MiB"));
        }
        assert!(refusal(publishable(1, &portrait, 12.5)).contains("duration differs"));
        for duration in ["inf", "soon"] {
            let odd = inspected(r#"{"width":1080,"height":1920}"#, duration);
            assert!(publishable(1, &odd, 12.0).is_err());
        }
        assert_eq!(
            refusal(publishable(1, &inspected("", "12"), 12.0)),
            "prepared MP4 has no video track"
        );
        for size in [
            r#"{"width":640,"height":359}"#,
            r#"{"width":1921,"height":1080}"#,
        ] {
            assert!(refusal(publishable(1, &inspected(size, "12"), 12.0)).contains("360 pixels"));
        }
    }

    fn argv(command: &Command) -> Vec<String> {
        let command = command.as_std();
        std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|part| part.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn every_ffmpeg_invocation_reads_only_local_mov_input() {
        let (input, output, passlog) =
            (Path::new("in.mp4"), Path::new("out.mp4"), Path::new("log"));
        let probe = argv(&ffprobe(input));
        assert_eq!(probe[0], "ffprobe");
        assert_eq!(probe.last().unwrap(), "in.mp4");

        let first = argv(&encode_pass(input, output, 5_000_000, 12.5, passlog, "1"));
        let second = argv(&encode_pass(input, output, 5_000_000, 12.5, passlog, "2"));
        assert_eq!(first[0], "ffmpeg");
        assert_eq!(first.last().unwrap(), "out.mp4");
        let value = |args: &[String], flag: &str| {
            args[args.iter().position(|arg| arg == flag).unwrap() + 1].clone()
        };
        assert_eq!(value(&first, "-i"), "in.mp4");
        assert_eq!(value(&first, "-b:v"), "5000000");
        assert_eq!(value(&first, "-t"), "12.5");
        assert_eq!(value(&first, "-passlogfile"), "log");
        assert_eq!(
            (value(&first, "-pass"), value(&second, "-pass")),
            ("1".to_owned(), "2".to_owned())
        );
        assert!(first.contains(&"-nostdin".to_owned()) && value(&first, "-map_metadata") == "-1");

        let decode = argv(&decode_check(output));
        assert_eq!(
            decode[decode.len() - 5..],
            ["-i", "out.mp4", "-f", "null", "-"]
        );
        for command in [&probe, &first, &decode] {
            assert_eq!(value(command, "-protocol_whitelist"), "file,pipe");
            assert_eq!(value(command, "-f"), "mov");
        }
    }
}
