use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::media_transcode::{
    transcode_voice_note_to_m4a_with_cancellation, transcode_voice_note_to_mp4_with_cancellation,
};

const VOICE_NOTE_MAX_INPUT_BYTES: usize = 128 * 1024 * 1024;

pub(super) fn is_voice_note_filename(filename: Option<&str>) -> bool {
    filename.is_some_and(|name| {
        let lower = name.to_ascii_lowercase();
        lower.starts_with("voice-note-") && lower.ends_with(".wav")
    })
}

/// How a recorded voice note is packaged for the relay it is going to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VoiceNoteContainer {
    /// AAC inside a stub H.264 MP4, uploaded as `video/mp4`. Every relay
    /// accepts it; clients recognise the `voice-note-*.mp4` filename.
    Mp4Envelope,
    /// Bare AAC in an M4A, uploaded as `audio/mp4`. Only for relays that
    /// advertise the `buzz-audio` NIP-11 extension.
    M4aAudio,
}

impl VoiceNoteContainer {
    pub(super) fn extension(self) -> &'static str {
        match self {
            Self::Mp4Envelope => "mp4",
            Self::M4aAudio => "m4a",
        }
    }
}

/// Rewrite the recorder's `voice-note-<id>.wav` name for the packaged upload.
pub(super) fn voice_note_upload_filename(filename: &str, container: VoiceNoteContainer) -> String {
    let ext = container.extension();
    filename
        .strip_suffix(".wav")
        .or_else(|| filename.strip_suffix(".WAV"))
        .map_or_else(
            || format!("{filename}.{ext}"),
            |stem| format!("{stem}.{ext}"),
        )
}

/// Package a recorded voice note (`data`, the recorder's WAV) for upload in
/// `container`. The source is shared rather than owned so the caller can
/// package the same recording again as the envelope if the relay refuses the
/// M4A, without copying it.
pub(super) async fn prepare_voice_note_for_upload(
    data: Arc<Vec<u8>>,
    cancellation: Option<&CancellationToken>,
    container: VoiceNoteContainer,
) -> Result<(Vec<u8>, Option<Vec<u8>>), String> {
    validate_voice_note_input_size(data.len())?;
    let cancellation = cancellation.cloned();
    tokio::task::spawn_blocking(move || {
        let detected = infer::get(data.as_slice())
            .ok_or_else(|| "Voice note has an unrecognized audio format.".to_string())?;
        if !detected.mime_type().starts_with("audio/") {
            return Err("Voice note upload did not contain audio.".to_string());
        }

        let tmp_input =
            std::env::temp_dir().join(format!("buzz-voice-input-{}", uuid::Uuid::new_v4()));
        let result = (|| {
            std::fs::write(&tmp_input, data.as_slice())
                .map_err(|error| format!("failed to prepare voice note: {error}"))?;
            let output = match container {
                VoiceNoteContainer::Mp4Envelope => transcode_voice_note_to_mp4_with_cancellation(
                    &tmp_input,
                    cancellation.as_ref(),
                )?,
                VoiceNoteContainer::M4aAudio => transcode_voice_note_to_m4a_with_cancellation(
                    &tmp_input,
                    cancellation.as_ref(),
                )?,
            };
            let bytes = std::fs::read(&output)
                .map_err(|error| format!("failed to read prepared voice note: {error}"));
            let _ = std::fs::remove_file(&output);
            bytes.map(|bytes| (bytes, None))
        })();
        let _ = std::fs::remove_file(&tmp_input);
        result
    })
    .await
    .map_err(|error| format!("voice note task failed: {error}"))?
}

fn validate_voice_note_input_size(size: usize) -> Result<(), String> {
    if size > VOICE_NOTE_MAX_INPUT_BYTES {
        return Err(format!(
            "Voice note exceeds the maximum input size of {VOICE_NOTE_MAX_INPUT_BYTES} bytes."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        is_voice_note_filename, validate_voice_note_input_size, voice_note_upload_filename,
        VoiceNoteContainer, VOICE_NOTE_MAX_INPUT_BYTES,
    };

    #[test]
    fn voice_note_filenames_are_scoped_and_rewritten_for_video_upload() {
        assert!(is_voice_note_filename(Some("voice-note-123.wav")));
        assert!(!is_voice_note_filename(Some("meeting.wav")));
        assert!(!is_voice_note_filename(Some("voice-note-123.mp4")));
        assert_eq!(
            voice_note_upload_filename("voice-note-123.wav", VoiceNoteContainer::Mp4Envelope),
            "voice-note-123.mp4"
        );
        assert_eq!(
            voice_note_upload_filename("voice-note-123.wav", VoiceNoteContainer::M4aAudio),
            "voice-note-123.m4a"
        );
        assert_eq!(
            voice_note_upload_filename("voice-note-123.WAV", VoiceNoteContainer::M4aAudio),
            "voice-note-123.m4a"
        );
    }

    #[test]
    fn voice_note_input_size_is_bounded_before_transcoding() {
        assert!(validate_voice_note_input_size(VOICE_NOTE_MAX_INPUT_BYTES).is_ok());
        assert!(validate_voice_note_input_size(VOICE_NOTE_MAX_INPUT_BYTES + 1).is_err());
    }
}
