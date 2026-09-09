//! Audio upload validation.
//!
//! Buzz stores uploads byte-for-byte (the Blossom `x` tag binds the client's
//! SHA-256 to the stored blob), so the relay cannot rewrite an audio file to
//! strip metadata. Instead, like the MP4 video validator, it accepts only
//! *canonical* streams that carry no ID3, APE or `udta` metadata and rejects
//! everything else with [`MediaError::MetadataForbidden`]. Client encoders and
//! the agent plugin produce such streams (`ffmpeg -map_metadata -1
//! -id3v2_version 0 -write_id3v1 0`, or the platform AAC encoders with
//! metadata disabled). What survives is the encoder's own fingerprint: the
//! LAME/Xing Info frame names the encoder, and the M4A box layout pins the
//! muxer. Neither carries user or location data.
//!
//! Two containers are accepted:
//!
//! - **MP3** (`audio/mpeg`): the file must be a contiguous sequence of MPEG
//!   audio frames. No ID3v2 header, no ID3v1 trailer, no APE tag, no bytes
//!   outside frame boundaries. A leading Xing/Info frame (LAME's VBR header)
//!   is a valid frame and is tolerated.
//! - **M4A** (`audio/mp4`): an ISO-BMFF file with exactly one AAC audio track
//!   and no video, walked with the same box allow-list the video path uses.
//!
//! The whole feature sits behind [`MediaConfig::audio_uploads_enabled`] and is
//! advertised to clients through the NIP-11 `buzz-audio` extension so clients
//! can fall back to the video envelope on relays without it.

use std::path::Path;

use crate::config::MediaConfig;
use crate::error::MediaError;
use crate::validation::{
    inspect_video_tracks, parse_mp4_file, track_duration_secs, ParsedMp4, VideoMeta,
};

/// Longest audio clip the relay accepts, in seconds (30 minutes).
///
/// Voice notes are capped at five minutes by the clients; agent replies can
/// run longer. Anything beyond this is a podcast, not a message.
pub const MAX_AUDIO_DURATION_SECS: f64 = 1800.0;

/// Metadata extracted from a validated audio file.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioMeta {
    /// Duration in seconds, summed from the frames or the track header.
    pub duration_secs: f64,
}

/// Outcome of validating an ISO-BMFF upload that reached the streaming path.
#[derive(Debug, Clone)]
pub enum IsoBmffMedia {
    /// A constrained H.264/AAC video.
    Video(VideoMeta),
    /// An audio-only AAC file (`audio/mp4`, served as `.m4a`).
    Audio(AudioMeta),
}

/// Sniff a buffered upload for MPEG audio.
///
/// `infer` only recognises MPEG-1 Layer III headers (`FF FB`) and ID3-tagged
/// files, so a 22.05 kHz MPEG-2 stream from a speech encoder would fall
/// through to the generic path and be rejected as unrecognised audio. This
/// sniff parses frame headers instead.
///
/// One header is not enough evidence: the eleven sync bits plus a plausible
/// bitrate and sample-rate index also match the start of ordinary files. A
/// UTF-16LE text file with a BOM begins `FF FE xx 00`, which parses as an
/// MPEG-1 Layer I header for most first characters, and routing such a file
/// to the audio validator would refuse a generic upload that used to be
/// stored. So the body must carry two consecutive frames, the second starting
/// exactly where the first ends and sharing its version and sample rate, or
/// an `ID3` prefix. The prefix counts so a tagged file reaches the audio
/// validator and fails with the precise [`MediaError::MetadataForbidden`]
/// rather than a generic rejection. ISO-BMFF audio is routed by the container
/// sniff, not here.
pub fn sniff_audio_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"ID3") {
        return Some("audio/mpeg");
    }
    let first = parse_frame_header(bytes)?;
    let second = bytes.get(first.len..).and_then(parse_frame_header)?;
    if second.version != first.version || second.sample_rate != first.sample_rate {
        return None;
    }
    Some("audio/mpeg")
}

/// Validate a buffered audio upload (the MP3 path).
///
/// Returns `(mime, ext, meta)`. ISO-BMFF audio never reaches this function:
/// the relay routes every `ftyp` container through the streaming path, which
/// calls [`validate_iso_bmff_file`] instead.
pub fn validate_audio_content(
    bytes: &[u8],
    config: &MediaConfig,
) -> Result<(String, String, AudioMeta), MediaError> {
    if !config.audio_uploads_enabled {
        return Err(MediaError::DisallowedContentType(
            infer::get(bytes)
                .map(|k| k.mime_type().to_string())
                .unwrap_or_else(|| "audio/*".to_string()),
        ));
    }
    if bytes.len() as u64 > config.max_audio_bytes {
        return Err(MediaError::FileTooLarge {
            size: bytes.len() as u64,
            max: config.max_audio_bytes,
        });
    }
    match sniff_audio_mime(bytes) {
        Some("audio/mpeg") => {
            let meta = validate_mp3_stream(bytes)?;
            Ok(("audio/mpeg".to_string(), "mp3".to_string(), meta))
        }
        _ => Err(MediaError::DisallowedContentType(
            infer::get(bytes)
                .map(|k| k.mime_type().to_string())
                .unwrap_or_else(|| "application/octet-stream".to_string()),
        )),
    }
}

/// Validate an ISO-BMFF file that may be either a video or an audio-only
/// M4A.
///
/// The file is parsed once. Video is tried first; when the track scan finds
/// no video track and audio uploads are enabled, the audio size cap is
/// applied to the size already known and the same parse is inspected as an
/// M4A. Nothing is re-opened or re-walked.
pub fn validate_iso_bmff_file(
    path: &Path,
    config: &MediaConfig,
) -> Result<IsoBmffMedia, MediaError> {
    let parsed = parse_mp4_file(path, config.max_video_bytes, MediaError::InvalidVideo)?;
    match inspect_video_tracks(&parsed) {
        Ok(video) => Ok(IsoBmffMedia::Video(video)),
        Err(MediaError::DisallowedContentType(mime))
            if mime == "audio/mp4" && config.audio_uploads_enabled =>
        {
            // No video track: this is an M4A, and the audio cap applies from
            // here on, before any further work on the file.
            if parsed.size > config.max_audio_bytes {
                return Err(MediaError::FileTooLarge {
                    size: parsed.size,
                    max: config.max_audio_bytes,
                });
            }
            inspect_m4a_tracks(&parsed).map(IsoBmffMedia::Audio)
        }
        Err(e) => Err(e),
    }
}

/// Validate an audio-only MP4 (M4A) on disk.
///
/// Reuses the video path's structural checks (moov before mdat, box
/// allow-list with no metadata boxes), then requires exactly one track, AAC,
/// with a positive duration under [`MAX_AUDIO_DURATION_SECS`].
pub fn validate_m4a_file(path: &Path, config: &MediaConfig) -> Result<AudioMeta, MediaError> {
    let parsed = parse_mp4_file(path, config.max_audio_bytes, MediaError::InvalidAudio)?;
    inspect_m4a_tracks(&parsed)
}

/// Walk the tracks of a parsed MP4 and apply the M4A constraints: exactly one
/// track, AAC, positive duration under [`MAX_AUDIO_DURATION_SECS`].
fn inspect_m4a_tracks(parsed: &ParsedMp4) -> Result<AudioMeta, MediaError> {
    let mut duration: Option<f64> = None;
    for track in parsed.mp4.tracks().values() {
        match track.track_type().map_err(|_| MediaError::InvalidAudio)? {
            mp4::TrackType::Audio => {
                if duration.is_some() {
                    return Err(MediaError::MetadataForbidden);
                }
                let media_type = track.media_type().map_err(|_| MediaError::WrongCodec)?;
                if media_type != mp4::MediaType::AAC {
                    return Err(MediaError::WrongCodec);
                }
                let secs = track_duration_secs(track).ok_or(MediaError::InvalidAudio)?;
                if secs <= 0.0 {
                    return Err(MediaError::InvalidAudio);
                }
                if secs > MAX_AUDIO_DURATION_SECS {
                    return Err(MediaError::DurationTooLong);
                }
                duration = Some(secs);
            }
            // A video track means this is not an M4A; anything else is a
            // side channel we did not ask for.
            _ => return Err(MediaError::MetadataForbidden),
        }
    }

    duration
        .map(|duration_secs| AudioMeta { duration_secs })
        .ok_or(MediaError::InvalidAudio)
}

// ---------------------------------------------------------------------------
// MPEG audio frame walker
// ---------------------------------------------------------------------------

/// Bitrates in kbit/s, indexed by `[version_group][layer][bitrate_index]`.
/// `version_group` 0 is MPEG-1, 1 is MPEG-2/2.5. `layer` 0..=2 is I, II, III.
const BITRATES: [[[u16; 16]; 3]; 2] = [
    [
        [
            0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448, 0,
        ],
        [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 0,
        ],
        [
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
        ],
    ],
    [
        [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256, 0,
        ],
        [
            0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
        ],
        [
            0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
        ],
    ],
];

/// Sample rates in Hz, indexed by `[version][sample_rate_index]` where
/// `version` is the raw two-bit field (0 = MPEG-2.5, 2 = MPEG-2, 3 = MPEG-1).
const SAMPLE_RATES: [[u32; 3]; 4] = [
    [11_025, 12_000, 8_000],
    [0, 0, 0],
    [22_050, 24_000, 16_000],
    [44_100, 48_000, 32_000],
];

/// One parsed MPEG audio frame header.
#[derive(Debug, Clone, Copy)]
struct FrameHeader {
    /// Total frame length in bytes, header included.
    len: usize,
    /// PCM samples per channel in this frame.
    samples: u32,
    /// Sample rate in Hz.
    sample_rate: u32,
    /// Raw two-bit version field (3 = MPEG-1).
    version: u8,
    /// Raw two-bit channel mode field (3 = mono).
    channel_mode: u8,
}

/// Parse a four-byte MPEG audio frame header. Returns `None` for anything
/// that is not a valid, non-free-format header.
fn parse_frame_header(h: &[u8]) -> Option<FrameHeader> {
    if h.len() < 4 || h[0] != 0xFF || (h[1] & 0xE0) != 0xE0 {
        return None;
    }
    let version = (h[1] >> 3) & 0x03; // 0=2.5, 1=reserved, 2=2, 3=1
    let layer_bits = (h[1] >> 1) & 0x03; // 1=III, 2=II, 3=I
    let bitrate_index = (h[2] >> 4) & 0x0F;
    let sample_rate_index = (h[2] >> 2) & 0x03;
    let padding = ((h[2] >> 1) & 0x01) as usize;
    let channel_mode = (h[3] >> 6) & 0x03;

    if version == 1 || layer_bits == 0 || bitrate_index == 0 || bitrate_index == 15 {
        return None; // reserved version, reserved layer, free format, bad index
    }
    if sample_rate_index == 3 {
        return None;
    }
    let layer = 3 - layer_bits as usize; // 0=I, 1=II, 2=III
    let version_group = if version == 3 { 0 } else { 1 };
    let bitrate = BITRATES[version_group][layer][bitrate_index as usize] as usize * 1000;
    let sample_rate = SAMPLE_RATES[version as usize][sample_rate_index as usize];
    if bitrate == 0 || sample_rate == 0 {
        return None;
    }
    let sample_rate_usize = sample_rate as usize;
    let (len, samples) = match layer {
        0 => ((12 * bitrate / sample_rate_usize + padding) * 4, 384),
        1 => (144 * bitrate / sample_rate_usize + padding, 1152),
        _ => {
            if version == 3 {
                (144 * bitrate / sample_rate_usize + padding, 1152)
            } else {
                (72 * bitrate / sample_rate_usize + padding, 576)
            }
        }
    };
    if len < 4 {
        return None;
    }
    Some(FrameHeader {
        len,
        samples,
        sample_rate,
        version,
        channel_mode,
    })
}

/// Byte offset of the Xing/Info tag inside a Layer III frame: the four-byte
/// header plus the side information, whose size depends on the version and
/// channel mode (MPEG-1: 17 mono / 32 otherwise; MPEG-2 and 2.5: 9 / 17).
fn xing_tag_offset(header: &FrameHeader) -> usize {
    let side_info = match (header.version == 3, header.channel_mode == 3) {
        (true, true) => 17,
        (true, false) => 32,
        (false, true) => 9,
        (false, false) => 17,
    };
    4 + side_info
}

/// Whether the frame at `frame` is a LAME/Xing "Xing" or "Info" VBR header
/// frame. Those frames carry no audio and must not count toward duration.
fn is_xing_info_frame(frame: &[u8], header: &FrameHeader) -> bool {
    let at = xing_tag_offset(header);
    frame.len() >= at + 4 && (&frame[at..at + 4] == b"Xing" || &frame[at..at + 4] == b"Info")
}

/// Validate that `bytes` is a canonical MPEG audio stream and measure it.
///
/// Every byte must belong to a frame: the walk starts at offset 0 and ends
/// exactly at the last byte. Any tag (ID3v2 at the front, ID3v1 or APE at the
/// back) or trailing junk fails with [`MediaError::MetadataForbidden`]; a
/// malformed header fails with [`MediaError::InvalidAudio`]. A final header
/// whose frame runs past the end of the file is treated as trailing junk too:
/// ffmpeg and LAME end on a frame boundary, and a short "frame" is where an
/// appended tag with sync-looking bytes (an ID3v2.4 footer, say) would hide.
pub fn validate_mp3_stream(bytes: &[u8]) -> Result<AudioMeta, MediaError> {
    if bytes.starts_with(b"ID3") {
        return Err(MediaError::MetadataForbidden);
    }
    if bytes.len() >= 128 && &bytes[bytes.len() - 128..bytes.len() - 125] == b"TAG" {
        return Err(MediaError::MetadataForbidden);
    }
    if bytes.len() >= 32 && &bytes[bytes.len() - 32..bytes.len() - 24] == b"APETAGEX" {
        return Err(MediaError::MetadataForbidden);
    }

    let mut off = 0usize;
    let mut samples: u64 = 0;
    let mut sample_rate: Option<u32> = None;
    let mut frames: u32 = 0;

    while off < bytes.len() {
        if bytes.len() - off < 4 {
            // A partial header where a frame should start is not audio.
            return Err(MediaError::MetadataForbidden);
        }
        let header = match parse_frame_header(&bytes[off..]) {
            Some(h) => h,
            None if frames == 0 => return Err(MediaError::InvalidAudio),
            // Bytes after the frame sequence that do not parse as a frame are
            // a tag or padding the encoder did not ask for.
            None => return Err(MediaError::MetadataForbidden),
        };
        if off + header.len > bytes.len() {
            // The frame runs past the end of the file. As the first frame the
            // stream is simply broken; after real frames it is a trailer that
            // happens to start with sync bits.
            if frames == 0 {
                return Err(MediaError::InvalidAudio);
            }
            return Err(MediaError::MetadataForbidden);
        }
        match sample_rate {
            None => sample_rate = Some(header.sample_rate),
            Some(rate) if rate != header.sample_rate => return Err(MediaError::InvalidAudio),
            Some(_) => {}
        }
        if !(frames == 0 && is_xing_info_frame(&bytes[off..off + header.len], &header)) {
            samples += header.samples as u64;
        }
        frames += 1;
        off += header.len;
    }

    let rate = sample_rate.ok_or(MediaError::InvalidAudio)?;
    let duration_secs = samples as f64 / rate as f64;
    if duration_secs <= 0.0 {
        return Err(MediaError::InvalidAudio);
    }
    if duration_secs > MAX_AUDIO_DURATION_SECS {
        return Err(MediaError::DurationTooLong);
    }
    Ok(AudioMeta { duration_secs })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validation::mp4_test_support::{
        mdhd_timescale, mdhd_v0, mdhd_v1, rewrite_first_mdhd,
    };
    use crate::validation::validate_file_content;

    fn test_config(enabled: bool) -> MediaConfig {
        MediaConfig {
            s3_endpoint: String::new(),
            s3_access_key: String::new(),
            s3_secret_key: String::new(),
            s3_bucket: String::new(),
            s3_region: "us-east-1".to_string(),
            s3_addressing_style: crate::config::S3AddressingStyle::Path,
            max_image_bytes: 50 * 1024 * 1024,
            max_gif_bytes: 10 * 1024 * 1024,
            max_video_bytes: 524_288_000,
            max_file_bytes: 104_857_600,
            max_audio_bytes: 26_214_400,
            audio_uploads_enabled: enabled,
            public_base_url: String::new(),
            upload_records_enabled: false,
            upload_ip_header: None,
            upload_port_header: None,
        }
    }

    /// MPEG-1 Layer III, 128 kbit/s, 44.1 kHz, stereo, no padding: 417 bytes.
    const FRAME_HEADER_128K: [u8; 4] = [0xFF, 0xFB, 0x90, 0x00];

    fn frame(n: usize) -> Vec<u8> {
        frames_with_header(&FRAME_HEADER_128K, n)
    }

    /// `n` zero-filled frames carrying `header`, sized from the header itself.
    fn frames_with_header(header: &[u8; 4], n: usize) -> Vec<u8> {
        let len = parse_frame_header(header).expect("valid header").len;
        let mut out = Vec::with_capacity(len * n);
        for _ in 0..n {
            out.extend_from_slice(header);
            out.extend(std::iter::repeat_n(0u8, len - 4));
        }
        out
    }

    /// The reviewer's probe: a UTF-16LE BOM followed by ordinary text. The
    /// first four bytes (`FF FE 'H' 00`) parse as an MPEG-1 Layer I header.
    fn utf16le_text() -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "Hello world, quarterly notes".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn frame_header_math_matches_the_spec() {
        let h = parse_frame_header(&FRAME_HEADER_128K).expect("valid header");
        assert_eq!(h.len, 417);
        assert_eq!(h.samples, 1152);
        assert_eq!(h.sample_rate, 44_100);
        // Padding bit adds one byte.
        let padded = parse_frame_header(&[0xFF, 0xFB, 0x92, 0x00]).expect("valid header");
        assert_eq!(padded.len, 418);
        // MPEG-2 Layer III, 64 kbit/s, 22.05 kHz: 72*64000/22050 = 208 bytes, 576 samples.
        let mpeg2 = parse_frame_header(&[0xFF, 0xF3, 0x80, 0x00]).expect("valid header");
        assert_eq!(mpeg2.len, 208);
        assert_eq!(mpeg2.samples, 576);
        // Free format, reserved version, bad bitrate index are all rejected.
        assert!(parse_frame_header(&[0xFF, 0xFB, 0x00, 0x00]).is_none());
        assert!(parse_frame_header(&[0xFF, 0xEB, 0x90, 0x00]).is_none());
        assert!(parse_frame_header(&[0xFF, 0xFB, 0xF0, 0x00]).is_none());
        assert!(parse_frame_header(&[0xFF, 0xFB, 0x9C, 0x00]).is_none());
    }

    #[test]
    fn sniff_requires_two_consecutive_frames_or_an_id3_prefix() {
        assert_eq!(sniff_audio_mime(&frame(2)), Some("audio/mpeg"));
        assert_eq!(
            sniff_audio_mime(b"ID3\x04\x00\x00\x00\x00\x00\x00"),
            Some("audio/mpeg")
        );
        // One header on its own is not evidence of a stream.
        assert_eq!(sniff_audio_mime(&frame(1)), None);
        let mut one_then_junk = frame(1);
        one_then_junk.extend_from_slice(b"not a frame header");
        assert_eq!(sniff_audio_mime(&one_then_junk), None);
        // Two headers that disagree on the sample rate are not one stream.
        let mut mixed = frame(1);
        mixed.extend(frames_with_header(&[0xFF, 0xFB, 0x94, 0x00], 1));
        assert_eq!(sniff_audio_mime(&mixed), None);
        assert_eq!(sniff_audio_mime(&[]), None);
    }

    /// Regression for the UTF-16LE misrouting: with audio enabled, a text
    /// file whose BOM parses as a frame header must still take the generic
    /// path and be stored as it was before the feature existed.
    #[test]
    fn utf16le_text_is_not_sniffed_as_audio() {
        let bytes = utf16le_text();
        // The fixture reproduces the hazard: one header does parse.
        assert!(parse_frame_header(&bytes).is_some());
        assert_eq!(sniff_audio_mime(&bytes), None);
        assert_eq!(
            validate_file_content(&bytes, &test_config(true)).expect("generic file"),
            ("application/octet-stream".to_string(), "bin".to_string())
        );
        assert_eq!(
            validate_file_content(&bytes, &test_config(false)).expect("generic file"),
            ("application/octet-stream".to_string(), "bin".to_string())
        );
        // Sent straight to the audio validator it is refused, so the sniff
        // is the only thing standing between the file and a 422.
        assert!(matches!(
            validate_audio_content(&bytes, &test_config(true)),
            Err(MediaError::DisallowedContentType(_))
        ));
    }

    #[test]
    fn canonical_stream_is_measured() {
        // 100 frames * 1152 samples / 44100 Hz = 2.612 s
        let meta = validate_mp3_stream(&frame(100)).expect("canonical stream");
        assert!((meta.duration_secs - 2.612).abs() < 0.001, "{meta:?}");
    }

    #[test]
    fn xing_info_frame_does_not_count() {
        let mut bytes = frame(1);
        // Stereo MPEG-1: side info is 32 bytes, so the tag sits at offset 36.
        bytes[36..40].copy_from_slice(b"Info");
        bytes.extend(frame(10));
        let meta = validate_mp3_stream(&bytes).expect("info frame is a frame");
        let expected = 10.0 * 1152.0 / 44_100.0;
        assert!((meta.duration_secs - expected).abs() < 0.0001, "{meta:?}");
    }

    /// Every side-info layout, so a wrong table entry fails here rather than
    /// hiding inside a 0.1 s duration tolerance on a real fixture. For each
    /// layout the tag is recognised at its own offset and at no other
    /// layout's offset, and the walker drops exactly one frame's samples.
    #[test]
    fn xing_info_side_info_sizes_cover_every_layout() {
        // (header, expected tag offset, label)
        let layouts: [([u8; 4], usize, &str); 5] = [
            ([0xFF, 0xFB, 0x90, 0x00], 36, "MPEG-1 stereo"),
            ([0xFF, 0xFB, 0x90, 0xC0], 21, "MPEG-1 mono"),
            ([0xFF, 0xF3, 0x80, 0x00], 21, "MPEG-2 stereo"),
            ([0xFF, 0xF3, 0x80, 0xC0], 13, "MPEG-2 mono"),
            ([0xFF, 0xE3, 0x80, 0x00], 21, "MPEG-2.5 stereo"),
        ];
        let offsets = [13usize, 21, 36];
        for (header_bytes, expected, label) in layouts {
            let header = parse_frame_header(&header_bytes).expect(label);
            assert_eq!(xing_tag_offset(&header), expected, "{label}");
            for candidate in offsets {
                let mut single = frames_with_header(&header_bytes, 1);
                single[candidate..candidate + 4].copy_from_slice(b"Info");
                assert_eq!(
                    is_xing_info_frame(&single, &header),
                    candidate == expected,
                    "{label}: Info at {candidate}"
                );
            }
            let mut stream = frames_with_header(&header_bytes, 1);
            stream[expected..expected + 4].copy_from_slice(b"Xing");
            stream.extend(frames_with_header(&header_bytes, 10));
            let meta = validate_mp3_stream(&stream).expect(label);
            let want = 10.0 * header.samples as f64 / header.sample_rate as f64;
            assert!(
                (meta.duration_secs - want).abs() < 1e-9,
                "{label}: got {} want {want}",
                meta.duration_secs
            );
        }
    }

    #[test]
    fn tags_are_rejected_as_metadata() {
        let mut id3v2 = b"ID3\x04\x00\x00\x00\x00\x00\x00".to_vec();
        id3v2.extend(frame(2));
        assert!(matches!(
            validate_mp3_stream(&id3v2),
            Err(MediaError::MetadataForbidden)
        ));

        let mut id3v1 = frame(2);
        let mut tag = b"TAG".to_vec();
        tag.extend(std::iter::repeat_n(b' ', 125));
        id3v1.extend(tag);
        assert!(matches!(
            validate_mp3_stream(&id3v1),
            Err(MediaError::MetadataForbidden)
        ));

        let mut ape = frame(2);
        ape.extend(std::iter::repeat_n(0u8, 8));
        ape.extend_from_slice(b"APETAGEX");
        ape.extend(std::iter::repeat_n(0u8, 24));
        assert!(matches!(
            validate_mp3_stream(&ape),
            Err(MediaError::MetadataForbidden)
        ));

        // Trailing junk that is not a tag is still outside a frame.
        let mut junk = frame(2);
        junk.extend_from_slice(b"hello");
        assert!(matches!(
            validate_mp3_stream(&junk),
            Err(MediaError::MetadataForbidden)
        ));
    }

    /// An ID3v2.4 tag appended after the stream with its footer flag set,
    /// preceded by four sync-looking bytes. The old truncated-final-frame
    /// allowance accepted this as a short last frame; it is a trailer.
    #[test]
    fn id3v24_footer_tag_after_the_stream_is_rejected() {
        let mut bytes = frame(2);
        bytes.extend_from_slice(&FRAME_HEADER_128K);
        // Header: ID3, version 2.4, flags 0x10 (footer present), size 25.
        bytes.extend_from_slice(b"ID3\x04\x00\x10\x00\x00\x00\x19");
        // One TIT2 frame: id, syncsafe size 15, flags, 15 bytes of text.
        bytes.extend_from_slice(b"TIT2\x00\x00\x00\x0F\x00\x00");
        bytes.extend_from_slice(b"\x03quarterly note");
        // Footer mirrors the header with the reversed identifier.
        bytes.extend_from_slice(b"3DI\x04\x00\x10\x00\x00\x00\x19");
        assert_eq!(bytes.len(), 417 * 2 + 4 + 45);
        // The sniff still routes it to the audio validator, which must refuse it.
        assert_eq!(sniff_audio_mime(&bytes), Some("audio/mpeg"));
        assert!(matches!(
            validate_mp3_stream(&bytes),
            Err(MediaError::MetadataForbidden)
        ));
    }

    #[test]
    fn truncated_last_frame_is_rejected_as_a_trailer() {
        let mut bytes = frame(3);
        bytes.truncate(417 * 2 + 100);
        assert!(matches!(
            validate_mp3_stream(&bytes),
            Err(MediaError::MetadataForbidden)
        ));
        // A lone short frame is a broken stream, not a trailer.
        let mut short = frame(1);
        short.truncate(100);
        assert!(matches!(
            validate_mp3_stream(&short),
            Err(MediaError::InvalidAudio)
        ));
    }

    #[test]
    fn sample_rate_change_mid_stream_is_invalid() {
        let mut bytes = frame(2);
        // MPEG-1 Layer III, 128 kbit/s at 48 kHz: 384-byte frame.
        bytes.extend(frames_with_header(&[0xFF, 0xFB, 0x94, 0x00], 1));
        assert!(matches!(
            validate_mp3_stream(&bytes),
            Err(MediaError::InvalidAudio)
        ));
    }

    #[test]
    fn garbage_and_empty_input_are_invalid() {
        assert!(matches!(
            validate_mp3_stream(b"not audio at all"),
            Err(MediaError::InvalidAudio)
        ));
        assert!(matches!(
            validate_mp3_stream(&[]),
            Err(MediaError::InvalidAudio)
        ));
    }

    #[test]
    fn over_long_streams_are_rejected() {
        // 1801 s at 1152 samples per frame and 44.1 kHz is 68,935 frames;
        // build it from a single frame's bytes without allocating per frame.
        let one = frame(1);
        let frames = (MAX_AUDIO_DURATION_SECS * 44_100.0 / 1152.0) as usize + 40;
        let mut bytes = Vec::with_capacity(one.len() * frames);
        for _ in 0..frames {
            bytes.extend_from_slice(&one);
        }
        assert!(matches!(
            validate_mp3_stream(&bytes),
            Err(MediaError::DurationTooLong)
        ));
    }

    #[test]
    fn audio_content_is_gated_by_config_and_size() {
        let bytes = frame(5);
        assert!(matches!(
            validate_audio_content(&bytes, &test_config(false)),
            Err(MediaError::DisallowedContentType(m)) if m == "audio/mpeg"
        ));
        let (mime, ext, meta) =
            validate_audio_content(&bytes, &test_config(true)).expect("enabled");
        assert_eq!(mime, "audio/mpeg");
        assert_eq!(ext, "mp3");
        assert!(meta.duration_secs > 0.0);

        let mut small = test_config(true);
        small.max_audio_bytes = 100;
        assert!(matches!(
            validate_audio_content(&bytes, &small),
            Err(MediaError::FileTooLarge { .. })
        ));
    }

    // Real encoder output, generated with ffmpeg from a 1.2 s sine wave:
    //   clean:  -map_metadata -1 -id3v2_version 0 -write_id3v1 0 (mp3)
    //           -map_metadata -1 -movflags +faststart -f mp4 (m4a)
    //   tagged: -metadata title=... -id3v2_version 3 -write_id3v1 1 / -f ipod
    const CLEAN_MP3: &[u8] = include_bytes!("../tests/fixtures/audio/sine-clean.mp3");
    const TAGGED_MP3: &[u8] = include_bytes!("../tests/fixtures/audio/sine-tagged.mp3");
    const CLEAN_M4A: &[u8] = include_bytes!("../tests/fixtures/audio/sine-clean.m4a");
    const TAGGED_M4A: &[u8] = include_bytes!("../tests/fixtures/audio/sine-tagged.m4a");

    fn temp_file(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().expect("temp file");
        std::io::Write::write_all(&mut f, bytes).expect("write fixture");
        f
    }

    #[test]
    fn ffmpeg_clean_mp3_is_accepted_and_measured() {
        // `infer` does not know this MPEG-2 header; our sniff must.
        assert_eq!(infer::get(CLEAN_MP3), None);
        assert_eq!(sniff_audio_mime(CLEAN_MP3), Some("audio/mpeg"));
        assert_eq!(sniff_audio_mime(TAGGED_MP3), Some("audio/mpeg"));
        assert_eq!(sniff_audio_mime(b"RIFF\x24\x00\x00\x00WAVEfmt "), None);
        assert_eq!(sniff_audio_mime(CLEAN_M4A), None);
        let (mime, ext, meta) =
            validate_audio_content(CLEAN_MP3, &test_config(true)).expect("clean mp3");
        assert_eq!((mime.as_str(), ext.as_str()), ("audio/mpeg", "mp3"));
        assert!(
            (meta.duration_secs - 1.2).abs() < 0.1,
            "duration {}",
            meta.duration_secs
        );
    }

    #[test]
    fn ffmpeg_tagged_mp3_is_rejected() {
        assert!(matches!(
            validate_audio_content(TAGGED_MP3, &test_config(true)),
            Err(MediaError::MetadataForbidden)
        ));
    }

    #[test]
    fn ffmpeg_clean_m4a_is_accepted_through_the_iso_bmff_path() {
        let file = temp_file(CLEAN_M4A);
        let meta = validate_m4a_file(file.path(), &test_config(true)).expect("clean m4a");
        assert!(
            (meta.duration_secs - 1.2).abs() < 0.1,
            "duration {}",
            meta.duration_secs
        );
        match validate_iso_bmff_file(file.path(), &test_config(true)) {
            Ok(IsoBmffMedia::Audio(_)) => {}
            other => panic!("expected audio, got {other:?}"),
        }
        // With the feature off, the video validator's verdict stands.
        assert!(matches!(
            validate_iso_bmff_file(file.path(), &test_config(false)),
            Err(MediaError::DisallowedContentType(m)) if m == "audio/mp4"
        ));
    }

    /// The audio cap, not the video cap, bounds an M4A on the streaming
    /// path, and it is applied as soon as the track scan shows no video.
    #[test]
    fn m4a_on_the_iso_bmff_path_is_bounded_by_the_audio_cap() {
        let file = temp_file(CLEAN_M4A);
        let mut config = test_config(true);
        config.max_audio_bytes = 100;
        let result = validate_iso_bmff_file(file.path(), &config);
        assert!(
            matches!(
                result,
                Err(MediaError::FileTooLarge { size, max })
                    if size == CLEAN_M4A.len() as u64 && max == 100
            ),
            "expected FileTooLarge at the audio cap, got {result:?}"
        );
        // A video-sized cap on the same file is not what applies.
        config.max_audio_bytes = CLEAN_M4A.len() as u64;
        assert!(validate_iso_bmff_file(file.path(), &config).is_ok());
    }

    #[test]
    fn ffmpeg_tagged_m4a_is_rejected() {
        let file = temp_file(TAGGED_M4A);
        assert!(matches!(
            validate_m4a_file(file.path(), &test_config(true)),
            Err(MediaError::MetadataForbidden)
        ));
    }

    /// Both M4A entry points, so the fixture surgery exercises the shared
    /// track inspection through the streaming path as well.
    fn m4a_verdicts(
        bytes: &[u8],
    ) -> (
        Result<AudioMeta, MediaError>,
        Result<IsoBmffMedia, MediaError>,
    ) {
        let file = temp_file(bytes);
        (
            validate_m4a_file(file.path(), &test_config(true)),
            validate_iso_bmff_file(file.path(), &test_config(true)),
        )
    }

    #[test]
    fn m4a_over_the_duration_cap_is_rejected() {
        let long = rewrite_first_mdhd(CLEAN_M4A, |old| {
            let timescale = mdhd_timescale(old);
            mdhd_v0(timescale, 2000 * timescale)
        });
        let (direct, streamed) = m4a_verdicts(&long);
        assert!(
            matches!(direct, Err(MediaError::DurationTooLong)),
            "{direct:?}"
        );
        assert!(
            matches!(streamed, Err(MediaError::DurationTooLong)),
            "{streamed:?}"
        );
    }

    #[test]
    fn m4a_zero_timescale_is_invalid() {
        let zero = rewrite_first_mdhd(CLEAN_M4A, |old| {
            let duration = u32::from_be_bytes(old[16..20].try_into().expect("v0 duration bytes"));
            mdhd_v0(0, duration)
        });
        let (direct, streamed) = m4a_verdicts(&zero);
        assert!(
            matches!(direct, Err(MediaError::InvalidAudio)),
            "{direct:?}"
        );
        assert!(
            matches!(streamed, Err(MediaError::InvalidAudio)),
            "{streamed:?}"
        );
    }

    /// The reviewer's version-1 `mdhd` probe: `duration = 2^58 + timescale`
    /// wraps to one second in `mp4::Mp4Track::duration` (release) or panics
    /// (debug). The checked helper must reject it, and a sane version-1
    /// header must still be measured, so the rejection is about the value.
    #[test]
    fn m4a_version1_mdhd_uses_checked_duration() {
        let sane = rewrite_first_mdhd(CLEAN_M4A, |old| {
            let timescale = mdhd_timescale(old);
            let duration = u32::from_be_bytes(old[16..20].try_into().expect("v0 duration bytes"));
            mdhd_v1(timescale, u64::from(duration))
        });
        let (direct, streamed) = m4a_verdicts(&sane);
        let meta = direct.expect("sane version-1 mdhd");
        assert!((meta.duration_secs - 1.2).abs() < 0.1, "{meta:?}");
        assert!(
            matches!(streamed, Ok(IsoBmffMedia::Audio(_))),
            "{streamed:?}"
        );

        let huge = rewrite_first_mdhd(CLEAN_M4A, |old| {
            let timescale = mdhd_timescale(old);
            mdhd_v1(timescale, (1u64 << 58) + u64::from(timescale))
        });
        let (direct, streamed) = m4a_verdicts(&huge);
        assert!(
            matches!(direct, Err(MediaError::DurationTooLong)),
            "{direct:?}"
        );
        assert!(
            matches!(streamed, Err(MediaError::DurationTooLong)),
            "{streamed:?}"
        );

        // Microseconds beyond u64 are an overflow, not a duration.
        let overflow = rewrite_first_mdhd(CLEAN_M4A, |_| mdhd_v1(1, u64::MAX));
        let (direct, streamed) = m4a_verdicts(&overflow);
        assert!(
            matches!(direct, Err(MediaError::InvalidAudio)),
            "{direct:?}"
        );
        assert!(
            matches!(streamed, Err(MediaError::InvalidAudio)),
            "{streamed:?}"
        );
    }
}
