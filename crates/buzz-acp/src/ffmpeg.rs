//! Bounded ffmpeg invocations for audio envelopes.
//!
//! The relay's `buzz-audio` validator accepts only metadata-free MPEG audio,
//! and relays without the extension need Buzz's own voice-note envelope (an
//! H.264 stub video plus AAC in a fast-start MP4). Both are produced with
//! ffmpeg using the same flags as the Hermes gateway plugin, so a note made
//! here renders identically to one made there. Every run is capped by
//! [`FFMPEG_TIMEOUT`], has its stdin closed, and is killed if the future is
//! dropped, so a stuck encoder can never outlive the turn.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Wall-clock cap for one ffmpeg run.
pub(crate) const FFMPEG_TIMEOUT: Duration = Duration::from_secs(120);
/// Tail of ffmpeg's stderr kept for diagnostics.
const MAX_STDERR_TAIL: usize = 300;

/// Locate an ffmpeg binary: `$PATH` first, then the usual Homebrew and
/// `/usr/local` locations that a launchd-started daemon does not see on its
/// `PATH`.
pub fn find_ffmpeg() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("ffmpeg");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

/// Run one bounded ffmpeg invocation writing to `out`.
///
/// The final `out` check is **advisory only**. It is a second resolution of a
/// name in a directory the engine can write, by path and following symlinks,
/// so after a swap it can report the attacker's file rather than the one the
/// caller reserved. It exists to turn a silent no-output run into an error,
/// not to establish what was produced. Every caller is authoritative for that
/// and re-reads the result through the descriptor it holds on the directory
/// (`read_back_size` inbound, `PublishScratch::open_file` outbound); nothing
/// downstream may be built on this call's answer.
async fn run(ffmpeg: &Path, args: &[&str], out: &Path) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-nostdin"])
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let output = match tokio::time::timeout(FFMPEG_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(format!("failed to run ffmpeg: {e}")),
        Err(_) => return Err(format!("ffmpeg exceeded {FFMPEG_TIMEOUT:?}")),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr
            .chars()
            .rev()
            .take(MAX_STDERR_TAIL)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        return Err(format!(
            "ffmpeg exited with {}: {}",
            output.status,
            tail.trim()
        ));
    }
    // Advisory, per this function's doc: by path, following symlinks, and
    // never the authority on what the caller ended up with.
    match std::fs::metadata(out) {
        Ok(meta) if meta.len() > 0 => Ok(()),
        Ok(_) => Err("ffmpeg produced an empty file".into()),
        Err(e) => Err(format!("ffmpeg produced no output: {e}")),
    }
}

fn path_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Produce a metadata-free MPEG audio stream from `src` at `out`.
///
/// Frames are copied when `src` is already MP3 and `reencode` is false (tags
/// stripped, no quality loss); anything else goes through libmp3lame.
pub async fn convert_to_clean_mp3(
    ffmpeg: &Path,
    src: &Path,
    out: &Path,
    reencode: bool,
) -> Result<(), String> {
    let is_mp3 = src
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("mp3"));
    let codec: &[&str] = if is_mp3 && !reencode {
        &["-c:a", "copy"]
    } else {
        &["-c:a", "libmp3lame", "-b:a", "96k"]
    };
    let src = path_arg(src);
    let out_arg = path_arg(out);
    let mut args = vec![
        "-i",
        &src,
        "-vn",
        "-sn",
        "-dn",
        "-map_metadata",
        "-1",
        "-map_chapters",
        "-1",
        "-fflags",
        "+bitexact",
        "-flags:a",
        "+bitexact",
    ];
    args.extend_from_slice(codec);
    args.extend_from_slice(&[
        "-id3v2_version",
        "0",
        "-write_id3v1",
        "0",
        "-f",
        "mp3",
        &out_arg,
    ]);
    run(ffmpeg, &args, out).await
}

/// Wrap `src` in Buzz's voice-note envelope at `out`: a 16x16 black H.264
/// stub video plus 96k AAC, fast-start, no metadata.
pub async fn wrap_as_voice_note_mp4(ffmpeg: &Path, src: &Path, out: &Path) -> Result<(), String> {
    let src = path_arg(src);
    let out_arg = path_arg(out);
    let args = [
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=16x16:r=1",
        "-i",
        &src,
        "-map",
        "0:v:0",
        "-map",
        "1:a:0",
        "-shortest",
        "-map_metadata",
        "-1",
        "-map_chapters",
        "-1",
        "-sn",
        "-dn",
        "-fflags",
        "+bitexact",
        "-flags:v",
        "+bitexact",
        "-flags:a",
        "+bitexact",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-tune",
        "stillimage",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-b:a",
        "96k",
        "-movflags",
        "+faststart",
        "-metadata",
        "encoder=",
        "-f",
        "mp4",
        &out_arg,
    ];
    run(ffmpeg, &args, out).await
}

/// Pull the audio track out of a voice-note MP4 envelope as MP3.
pub async fn extract_voice_note_audio(ffmpeg: &Path, src: &Path, out: &Path) -> Result<(), String> {
    let src = path_arg(src);
    let out_arg = path_arg(out);
    let args = [
        "-i",
        &src,
        "-vn",
        "-sn",
        "-dn",
        "-map_metadata",
        "-1",
        "-c:a",
        "libmp3lame",
        "-b:a",
        "96k",
        "-id3v2_version",
        "0",
        "-write_id3v1",
        "0",
        "-f",
        "mp3",
        &out_arg,
    ];
    run(ffmpeg, &args, out).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_binary_is_an_error_not_a_panic() {
        let dir = std::env::temp_dir().join(format!("buzz-acp-ffmpeg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("out.mp3");
        let err = convert_to_clean_mp3(
            Path::new("/nonexistent/ffmpeg"),
            &dir.join("in.wav"),
            &out,
            false,
        )
        .await
        .unwrap_err();
        assert!(err.contains("failed to run ffmpeg"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn failing_process_reports_stderr_tail_and_missing_output() {
        // `false` ignores its arguments and exits 1 without writing anything.
        let dir = std::env::temp_dir().join(format!("buzz-acp-ffmpeg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("out.mp3");
        let err = run(Path::new("/usr/bin/false"), &["-i", "x"], &out)
            .await
            .unwrap_err();
        assert!(err.contains("exited with"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
