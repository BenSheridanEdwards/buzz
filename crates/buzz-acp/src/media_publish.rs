//! Outbound media: from the engine's reply to a kind-9 with `imeta`.
//!
//! The harness never publishes the engine's text (engines post through the
//! `buzz` CLI), but a file the engine produced has nowhere to go: the CLI's
//! upload path does not know the engine's reply, and the engine cannot sign a
//! Blossom upload. So the read loop keeps a bounded capture of the turn's
//! `agent_message_chunk` stream ([`TurnMediaCapture`]), and after the prompt
//! completes this module resolves the files it names, uploads each with the
//! agent key, and publishes exactly one kind-9 per reply carrying every
//! successful upload (rule 5).
//!
//! Two inputs are accepted and treated the same: ACP content blocks
//! (`resource_link`/`resource` with a file path, or `image`/`audio` with
//! inline data) and Hermes's `MEDIA:<path>` text convention. Audio is
//! delivered as native audio on `buzz-audio` relays, as the MP4 envelope when
//! the relay refuses audio and ffmpeg is available, and as a generic file
//! otherwise; the log says which.

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;

use crate::blossom::{self, AudioSupportCache, BlobDescriptor, BlossomError, RelayOrigin};
use crate::relay::RestClient;

/// Most files published from one reply.
pub(crate) const MAX_OUTBOUND_FILES: usize = 4;
/// Largest file published from one reply (the relay's default audio cap).
pub(crate) const MAX_OUTBOUND_FILE_BYTES: u64 = 25 * 1024 * 1024;
/// Tail of the reply text kept for `MEDIA:` scanning.
const MAX_CAPTURED_TEXT_BYTES: usize = 64 * 1024;
/// Most non-text content blocks kept from one reply.
const MAX_CAPTURED_BLOCKS: usize = 8;
/// Longest inline base64 payload kept from one block (about 25 MiB decoded).
const MAX_INLINE_BASE64_BYTES: usize = 34 * 1024 * 1024;
/// Wall-clock cap for submitting the kind-9.
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(20);
/// Extensions the `MEDIA:` matcher accepts; mirrors the Hermes gateway list.
const MEDIA_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "mp4", "mov", "avi", "mkv", "webm", "ogg", "opus", "mp3",
    "wav", "m4a", "flac", "epub", "pdf", "zip", "rar", "7z", "doc", "docx", "xls", "xlsx", "ppt",
    "pptx", "txt", "csv", "apk", "ipa",
];

/// Bounded capture of one turn's `agent_message_chunk` stream.
///
/// Reset by the ACP client at the start of every `session/prompt` and taken
/// by the pool once the prompt returns, so a capture can only ever describe
/// the turn that just completed.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TurnMediaCapture {
    text: String,
    text_dropped_bytes: usize,
    blocks: Vec<serde_json::Value>,
    dropped_blocks: usize,
}

impl TurnMediaCapture {
    /// Record one content block from an `agent_message_chunk` update.
    pub fn record_chunk(&mut self, content: &serde_json::Value) {
        match content.get("type").and_then(|t| t.as_str()) {
            Some("text") | None => {
                if let Some(text) = content.get("text").and_then(|t| t.as_str()) {
                    self.push_text(text);
                }
            }
            Some(_) => {
                let inline_len = ["data", "blob"]
                    .iter()
                    .filter_map(|k| content.get(*k).and_then(|v| v.as_str()))
                    .chain(
                        content
                            .get("resource")
                            .and_then(|r| r.get("blob"))
                            .and_then(|b| b.as_str()),
                    )
                    .map(str::len)
                    .max()
                    .unwrap_or(0);
                if self.blocks.len() >= MAX_CAPTURED_BLOCKS || inline_len > MAX_INLINE_BASE64_BYTES
                {
                    self.dropped_blocks += 1;
                } else {
                    self.blocks.push(content.clone());
                }
            }
        }
    }

    fn push_text(&mut self, text: &str) {
        self.text.push_str(text);
        if self.text.len() > MAX_CAPTURED_TEXT_BYTES {
            let mut cut = self.text.len() - MAX_CAPTURED_TEXT_BYTES;
            while !self.text.is_char_boundary(cut) {
                cut += 1;
            }
            self.text_dropped_bytes += cut;
            self.text.drain(..cut);
        }
    }

    /// True when nothing was captured.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.blocks.is_empty() && self.dropped_blocks == 0
    }

    /// Captured reply text (the bounded tail).
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Captured non-text content blocks.
    pub fn blocks(&self) -> &[serde_json::Value] {
        &self.blocks
    }

    /// Non-text blocks that did not fit the capture.
    pub fn dropped_blocks(&self) -> usize {
        self.dropped_blocks
    }
}

/// Find `MEDIA:<path>` references in reply text.
///
/// Extension-anchored like the Hermes matcher: the token after `MEDIA:` must
/// be an absolute (or `~/`) path ending in a known media extension, so a
/// bare `MEDIA:` in prose never triggers an upload. Trailing punctuation
/// after the extension is dropped.
pub fn extract_media_paths(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(idx) = rest.find("MEDIA:") {
        let preceded_ok = idx == 0
            || rest[..idx]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_whitespace() || matches!(c, '(' | '[' | '`' | '"' | '\''));
        let after = &rest[idx + "MEDIA:".len()..];
        let after = after.strip_prefix(' ').unwrap_or(after);
        let token: &str = after.split(char::is_whitespace).next().unwrap_or("");
        if preceded_ok {
            if let Some(path) = trim_to_media_extension(token) {
                if is_absolute_or_home(path) && !found.iter().any(|p| p == path) {
                    found.push(path.to_string());
                }
            }
        }
        rest = &rest[idx + "MEDIA:".len()..];
    }
    found
}

fn is_absolute_or_home(path: &str) -> bool {
    path.starts_with('/')
        || path.starts_with("~/")
        || (path.len() > 2
            && path.as_bytes()[0].is_ascii_alphabetic()
            && path.as_bytes()[1] == b':'
            && matches!(path.as_bytes()[2], b'/' | b'\\'))
}

fn trim_to_media_extension(token: &str) -> Option<&str> {
    let mut candidate = token;
    while !candidate.is_empty() {
        if has_media_extension(candidate) {
            return Some(candidate);
        }
        let mut end = candidate.len() - 1;
        while !candidate.is_char_boundary(end) {
            end -= 1;
        }
        candidate = &candidate[..end];
    }
    None
}

fn has_media_extension(path: &str) -> bool {
    let Some((stem, ext)) = path.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && !stem.ends_with('/')
        && MEDIA_EXTENSIONS
            .iter()
            .any(|allowed| ext.eq_ignore_ascii_case(allowed))
}

/// Broad media class used for the body line and the audio path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    /// `audio/*`.
    Audio,
    /// `image/*`.
    Image,
    /// `video/*`.
    Video,
    /// Anything else.
    File,
}

/// Classify a MIME type.
pub fn media_kind(mime: &str) -> MediaKind {
    let mime = mime.to_ascii_lowercase();
    if mime.starts_with("audio/") {
        MediaKind::Audio
    } else if mime.starts_with("image/") {
        MediaKind::Image
    } else if mime.starts_with("video/") {
        MediaKind::Video
    } else {
        MediaKind::File
    }
}

/// MIME type from a file extension (small fixed table; unknown is
/// `application/octet-stream`).
pub fn mime_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("wav") => "audio/wav",
        Some("ogg" | "opus") => "audio/ogg",
        Some("flac") => "audio/flac",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("mp4") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        Some("pdf") => "application/pdf",
        Some("txt" | "md") => "text/plain",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

/// File extension for an inline block's MIME type.
fn extension_for_mime(mime: &str) -> &'static str {
    match mime.to_ascii_lowercase().as_str() {
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/mp4" | "audio/x-m4a" => "m4a",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/ogg" | "audio/opus" => "ogg",
        "audio/flac" => "flac",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        _ => "bin",
    }
}

/// A file the reply asked the harness to publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundFile {
    /// Absolute path on disk.
    pub path: PathBuf,
    /// Filename to publish (basename of `path` unless the block named one).
    pub filename: String,
    /// MIME type: from the block when given, else from the extension.
    pub mime: String,
    /// Where the reference came from, for the log.
    pub origin: &'static str,
}

/// Files resolved from a capture plus the reasons anything was skipped.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OutboundResolution {
    /// Files to upload, at most [`MAX_OUTBOUND_FILES`].
    pub files: Vec<OutboundFile>,
    /// Human-readable reasons for references that were not turned into files.
    pub notes: Vec<String>,
}

/// Turn a capture into concrete files. Inline block data is written under
/// `scratch`. Missing, unreadable, oversized, or over-cap references become
/// notes instead of files.
pub fn resolve_outbound_files(capture: &TurnMediaCapture, scratch: &Path) -> OutboundResolution {
    let mut out = OutboundResolution::default();
    let mut inline_index = 0usize;

    let add = |out: &mut OutboundResolution,
               path: PathBuf,
               filename: Option<String>,
               mime: Option<String>,
               origin: &'static str| {
        if out.files.iter().any(|f| f.path == path) {
            return;
        }
        if out.files.len() >= MAX_OUTBOUND_FILES {
            out.notes.push(format!(
                "{} skipped: over the {MAX_OUTBOUND_FILES}-file limit per reply",
                path.display()
            ));
            return;
        }
        match std::fs::metadata(&path) {
            Ok(meta)
                if meta.is_file() && meta.len() > 0 && meta.len() <= MAX_OUTBOUND_FILE_BYTES =>
            {
                let filename = filename
                    .filter(|n| !n.trim().is_empty())
                    .map(|n| crate::attachments::safe_attachment_filename(&n))
                    .unwrap_or_else(|| {
                        path.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "attachment.bin".into())
                    });
                let mime = mime
                    .filter(|m| !m.trim().is_empty())
                    .unwrap_or_else(|| mime_for_path(&path).to_string());
                out.files.push(OutboundFile {
                    path,
                    filename,
                    mime,
                    origin,
                });
            }
            Ok(meta) if !meta.is_file() => out
                .notes
                .push(format!("{} skipped: not a regular file", path.display())),
            Ok(meta) if meta.len() == 0 => out
                .notes
                .push(format!("{} skipped: empty file", path.display())),
            Ok(meta) => out.notes.push(format!(
                "{} skipped: {} bytes exceeds the {MAX_OUTBOUND_FILE_BYTES} byte limit",
                path.display(),
                meta.len()
            )),
            Err(e) => out.notes.push(format!("{} skipped: {e}", path.display())),
        }
    };

    for raw in extract_media_paths(capture.text()) {
        let expanded = if let Some(rest) = raw.strip_prefix("~/") {
            match std::env::var_os("HOME") {
                Some(home) => PathBuf::from(home).join(rest),
                None => {
                    out.notes.push(format!("{raw} skipped: HOME is not set"));
                    continue;
                }
            }
        } else {
            PathBuf::from(&raw)
        };
        add(&mut out, expanded, None, None, "MEDIA: line");
    }

    for block in capture.blocks() {
        let kind = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match kind {
            "resource_link" => {
                let uri = block.get("uri").and_then(|u| u.as_str()).unwrap_or("");
                match path_from_uri(uri) {
                    Some(path) => add(
                        &mut out,
                        path,
                        block
                            .get("name")
                            .and_then(|n| n.as_str())
                            .map(str::to_string),
                        block
                            .get("mimeType")
                            .and_then(|m| m.as_str())
                            .map(str::to_string),
                        "resource_link block",
                    ),
                    None => out.notes.push(format!(
                        "resource_link {uri:?} skipped: not a local file URI"
                    )),
                }
            }
            "resource" => {
                let resource = block.get("resource").cloned().unwrap_or_default();
                let uri = resource.get("uri").and_then(|u| u.as_str()).unwrap_or("");
                let mime = resource
                    .get("mimeType")
                    .and_then(|m| m.as_str())
                    .map(str::to_string);
                if let Some(blob) = resource.get("blob").and_then(|b| b.as_str()) {
                    inline_index += 1;
                    match write_inline(scratch, inline_index, blob, mime.as_deref()) {
                        Ok(path) => add(&mut out, path, name_from_uri(uri), mime, "resource block"),
                        Err(e) => out.notes.push(format!("resource blob skipped: {e}")),
                    }
                } else if let Some(path) = path_from_uri(uri) {
                    add(&mut out, path, None, mime, "resource block");
                } else {
                    out.notes.push(format!(
                        "resource {uri:?} skipped: no blob and not a local file URI"
                    ));
                }
            }
            "image" | "audio" => {
                let mime = block
                    .get("mimeType")
                    .and_then(|m| m.as_str())
                    .map(str::to_string);
                match block.get("data").and_then(|d| d.as_str()) {
                    Some(data) => {
                        inline_index += 1;
                        match write_inline(scratch, inline_index, data, mime.as_deref()) {
                            Ok(path) => add(&mut out, path, None, mime, "inline content block"),
                            Err(e) => out.notes.push(format!("{kind} block skipped: {e}")),
                        }
                    }
                    None => out
                        .notes
                        .push(format!("{kind} block skipped: no inline data")),
                }
            }
            _ => {}
        }
    }
    if capture.dropped_blocks() > 0 {
        out.notes.push(format!(
            "{} content block(s) were not captured (over the {MAX_CAPTURED_BLOCKS}-block or inline-size limit)",
            capture.dropped_blocks()
        ));
    }
    out
}

fn path_from_uri(uri: &str) -> Option<PathBuf> {
    if let Some(stripped) = uri.strip_prefix("file://") {
        return url::Url::parse(uri)
            .ok()
            .and_then(|u| u.to_file_path().ok())
            .or_else(|| stripped.starts_with('/').then(|| PathBuf::from(stripped)));
    }
    uri.starts_with('/').then(|| PathBuf::from(uri))
}

fn name_from_uri(uri: &str) -> Option<String> {
    let name = uri.rsplit('/').next()?;
    (!name.is_empty() && !name.contains(':')).then(|| name.to_string())
}

fn write_inline(
    scratch: &Path,
    index: usize,
    base64_data: &str,
    mime: Option<&str>,
) -> Result<PathBuf, String> {
    if base64_data.len() > MAX_INLINE_BASE64_BYTES {
        return Err("inline data exceeds the size limit".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_data.trim())
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(base64_data.trim()))
        .map_err(|e| format!("inline data is not valid base64: {e}"))?;
    std::fs::create_dir_all(scratch)
        .map_err(|e| format!("cannot create {}: {e}", scratch.display()))?;
    let ext = extension_for_mime(mime.unwrap_or(""));
    let path = scratch.join(format!("inline-{index}.{ext}"));
    std::fs::write(&path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

/// How a piece of audio ended up on the relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDelivery {
    /// Uploaded as `audio/*` on a `buzz-audio` relay.
    NativeAudio,
    /// Wrapped in the H.264/AAC MP4 envelope and uploaded as `video/mp4`.
    Mp4Envelope,
    /// Uploaded as an opaque file; clients show a download card.
    GenericFile,
}

impl AudioDelivery {
    fn label(self) -> &'static str {
        match self {
            Self::NativeAudio => "native audio",
            Self::Mp4Envelope => "mp4 envelope",
            Self::GenericFile => "generic file",
        }
    }
}

/// Ordered attempts for publishing one audio file.
///
/// Native audio is tried only when the relay advertises `buzz-audio`; the
/// envelope needs ffmpeg; a generic upload always closes the list so audio is
/// never dropped for lack of a better path.
pub fn audio_delivery_plan(
    relay_supports_audio: bool,
    ffmpeg_available: bool,
) -> Vec<AudioDelivery> {
    let mut plan = Vec::with_capacity(3);
    if relay_supports_audio {
        plan.push(AudioDelivery::NativeAudio);
    }
    if ffmpeg_available {
        plan.push(AudioDelivery::Mp4Envelope);
    }
    plan.push(AudioDelivery::GenericFile);
    plan
}

/// Whether an upload error means "try the next delivery" (415/422) rather
/// than "the relay is unreachable or refusing us" (anything else).
pub fn falls_through(error: &BlossomError) -> bool {
    error.is_media_rejection()
}

/// Voice-note filename carrying the marker clients key on.
pub fn voice_note_filename(ext: &str, now_ms: u128) -> String {
    format!("voice-note-{now_ms}.{ext}")
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// One successful upload.
#[derive(Debug, Clone, PartialEq)]
pub struct PublishedMedia {
    /// Relay's descriptor for the stored blob.
    pub descriptor: BlobDescriptor,
    /// Filename to put in `imeta` and the body.
    pub filename: String,
    /// Class of the file as published (an envelope is still `Audio`).
    pub kind: MediaKind,
    /// Which audio path succeeded, for audio only.
    pub delivery: Option<AudioDelivery>,
}

/// Build the `imeta` tag for one published file.
pub fn imeta_tag(item: &PublishedMedia) -> Vec<String> {
    let d = &item.descriptor;
    let mut tag = vec![
        "imeta".to_string(),
        format!("url {}", d.url),
        format!("m {}", d.mime_type),
        format!("x {}", d.sha256),
        format!("size {}", d.size),
    ];
    if let Some(dim) = d.dim.as_deref().filter(|s| !s.is_empty()) {
        tag.push(format!("dim {dim}"));
    }
    if let Some(duration) = d.duration {
        // The relay cross-checks this against the stored duration; echo its
        // own value verbatim.
        tag.push(format!("duration {duration}"));
    }
    tag.push(format!("filename {}", item.filename));
    tag
}

/// Body line for one published file.
pub fn body_line(item: &PublishedMedia) -> String {
    match item.kind {
        MediaKind::Image => format!("![image]({})", item.descriptor.url),
        MediaKind::Video => format!("![video]({})", item.descriptor.url),
        MediaKind::Audio | MediaKind::File => {
            format!("[{}]({})", item.filename, item.descriptor.url)
        }
    }
}

/// Compose the single kind-9 body and tag set for a reply's uploads.
pub fn compose_message(items: &[PublishedMedia]) -> (String, Vec<Vec<String>>) {
    let body = items.iter().map(body_line).collect::<Vec<_>>().join("\n");
    let tags = items.iter().map(imeta_tag).collect();
    (body, tags)
}

/// What the harness needs to upload and publish on the agent's behalf.
pub struct MediaPublisher<'a> {
    /// REST client carrying the agent key and auth tag.
    pub rest: &'a RestClient,
    /// Cached NIP-11 audio-support answer.
    pub audio_support: &'a AudioSupportCache,
    /// ffmpeg binary, when one was found at startup.
    pub ffmpeg: Option<&'a Path>,
}

/// Outcome of publishing one reply's media.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PublishReport {
    /// Files that reached the relay, in body order.
    pub published: Vec<PublishedMedia>,
    /// Reasons for every file or reference that did not.
    pub failed: Vec<String>,
    /// Id of the kind-9 that carries the uploads, when one was accepted.
    pub event_id: Option<String>,
}

impl PublishReport {
    /// Whether every file and reference from the reply was handled without loss.
    pub fn is_clean(&self) -> bool {
        self.failed.is_empty()
    }
}

/// Reply anchoring for the media message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyTarget {
    /// Channel the trigger arrived in.
    pub channel_id: uuid::Uuid,
    /// Thread root to anchor to: the trigger's root when threaded, else the
    /// trigger itself.
    pub root_event_id: nostr::EventId,
}

impl ReplyTarget {
    /// Derive the target from the triggering event, matching the human-facing
    /// anchoring rule in `format_prompt`.
    pub fn for_trigger(channel_id: uuid::Uuid, trigger: &nostr::Event) -> Self {
        let thread = crate::queue::parse_thread_tags(trigger);
        let root_event_id = thread
            .root_event_id
            .as_deref()
            .and_then(|hex| nostr::EventId::from_hex(hex).ok())
            .unwrap_or(trigger.id);
        Self {
            channel_id,
            root_event_id,
        }
    }
}

impl MediaPublisher<'_> {
    /// Upload every file a capture names and publish one kind-9 with the
    /// results. Returns `None` when the capture referenced no media at all.
    pub async fn publish_turn_media(
        &self,
        target: &ReplyTarget,
        capture: &TurnMediaCapture,
        scratch: &Path,
    ) -> Option<PublishReport> {
        if capture.is_empty() {
            return None;
        }
        let resolution = resolve_outbound_files(capture, scratch);
        if resolution.files.is_empty() && resolution.notes.is_empty() {
            return None;
        }
        let mut report = PublishReport {
            failed: resolution.notes,
            ..Default::default()
        };
        let Some(origin) = RelayOrigin::from_base_url(&self.rest.base_url) else {
            report
                .failed
                .push("relay base URL has no usable origin for uploads".into());
            return Some(report);
        };
        let wants_audio = resolution
            .files
            .iter()
            .any(|f| media_kind(&f.mime) == MediaKind::Audio);
        let relay_supports_audio = if wants_audio {
            self.audio_support.relay_supports_audio(self.rest).await
        } else {
            false
        };
        for file in &resolution.files {
            match self
                .upload_one(&origin, file, relay_supports_audio, scratch)
                .await
            {
                Ok(item) => {
                    tracing::info!(
                        target: "acp::media",
                        file = %file.path.display(),
                        via = %file.origin,
                        delivery = item.delivery.map(AudioDelivery::label).unwrap_or("upload"),
                        "uploaded {} as {}",
                        item.filename,
                        item.descriptor.mime_type
                    );
                    report.published.push(item);
                }
                Err(reason) => {
                    tracing::warn!(
                        target: "acp::media",
                        file = %file.path.display(),
                        via = %file.origin,
                        "upload failed: {reason}"
                    );
                    report
                        .failed
                        .push(format!("{}: {reason}", file.path.display()));
                }
            }
        }
        if report.published.is_empty() {
            return Some(report);
        }
        match self.publish_message(target, &report.published).await {
            Ok(event_id) => {
                tracing::info!(
                    target: "acp::media",
                    event = %event_id,
                    channel = %target.channel_id,
                    "published {} attachment(s) in one kind-9",
                    report.published.len()
                );
                report.event_id = Some(event_id);
            }
            Err(reason) => {
                tracing::warn!(target: "acp::media", "kind-9 publish failed: {reason}");
                report
                    .failed
                    .push(format!("kind-9 publish failed: {reason}"));
            }
        }
        Some(report)
    }

    async fn upload_one(
        &self,
        origin: &RelayOrigin,
        file: &OutboundFile,
        relay_supports_audio: bool,
        scratch: &Path,
    ) -> Result<PublishedMedia, String> {
        let kind = media_kind(&file.mime);
        if kind != MediaKind::Audio {
            let bytes = read_bounded(&file.path).await?;
            let descriptor = blossom::upload_blob(self.rest, origin, bytes, &file.mime)
                .await
                .map_err(|e| e.to_string())?;
            return Ok(PublishedMedia {
                kind: media_kind(&descriptor.mime_type),
                descriptor,
                filename: file.filename.clone(),
                delivery: None,
            });
        }

        let ffmpeg_available = self.ffmpeg.is_some();
        let plan = audio_delivery_plan(relay_supports_audio, ffmpeg_available);
        let mut last_error = String::from("no delivery path attempted");
        for delivery in plan {
            let attempt = match delivery {
                AudioDelivery::NativeAudio => self.try_native_audio(origin, file, scratch).await,
                AudioDelivery::Mp4Envelope => self.try_envelope(origin, file, scratch).await,
                AudioDelivery::GenericFile => self.try_generic(origin, file).await,
            };
            match attempt {
                Ok(item) => {
                    tracing::info!(
                        target: "acp::media",
                        file = %file.path.display(),
                        "audio delivered as {} ({})",
                        delivery.label(),
                        item.filename
                    );
                    return Ok(item);
                }
                Err(AudioAttemptError::FallThrough(reason)) => {
                    tracing::warn!(
                        target: "acp::media",
                        file = %file.path.display(),
                        "{} not possible ({reason}); trying the next delivery",
                        delivery.label()
                    );
                    last_error = reason;
                }
                Err(AudioAttemptError::Terminal(reason)) => return Err(reason),
            }
        }
        Err(last_error)
    }

    async fn try_native_audio(
        &self,
        origin: &RelayOrigin,
        file: &OutboundFile,
        scratch: &Path,
    ) -> Result<PublishedMedia, AudioAttemptError> {
        let stamp = now_ms();
        let (candidates, mime): (Vec<PathBuf>, &str) = match self.ffmpeg {
            Some(ffmpeg) => {
                let copied = scratch.join(format!("native-{stamp}-copy.mp3"));
                let reencoded = scratch.join(format!("native-{stamp}-enc.mp3"));
                let mut list = Vec::new();
                match crate::ffmpeg::convert_to_clean_mp3(ffmpeg, &file.path, &copied, false).await
                {
                    Ok(()) => list.push(copied),
                    Err(e) => tracing::debug!(target: "acp::media", "mp3 copy failed: {e}"),
                }
                match crate::ffmpeg::convert_to_clean_mp3(ffmpeg, &file.path, &reencoded, true)
                    .await
                {
                    Ok(()) => list.push(reencoded),
                    Err(e) => tracing::debug!(target: "acp::media", "mp3 re-encode failed: {e}"),
                }
                if list.is_empty() {
                    return Err(AudioAttemptError::FallThrough(
                        "ffmpeg could not produce a clean MP3".into(),
                    ));
                }
                (list, "audio/mpeg")
            }
            None => {
                let lower = file.mime.to_ascii_lowercase();
                if lower == "audio/mpeg" || lower == "audio/mp3" {
                    (vec![file.path.clone()], "audio/mpeg")
                } else if lower == "audio/mp4" || lower == "audio/x-m4a" {
                    (vec![file.path.clone()], "audio/mp4")
                } else {
                    return Err(AudioAttemptError::FallThrough(format!(
                        "{} cannot be uploaded as native audio without ffmpeg",
                        file.mime
                    )));
                }
            }
        };
        let ext = if mime == "audio/mp4" { "m4a" } else { "mp3" };
        let mut last = String::new();
        for candidate in candidates {
            let bytes = read_bounded(&candidate)
                .await
                .map_err(AudioAttemptError::FallThrough)?;
            match blossom::upload_blob(self.rest, origin, bytes, mime).await {
                Ok(descriptor) => {
                    return Ok(PublishedMedia {
                        descriptor,
                        filename: voice_note_filename(ext, stamp),
                        kind: MediaKind::Audio,
                        delivery: Some(AudioDelivery::NativeAudio),
                    })
                }
                Err(e) if falls_through(&e) => {
                    self.audio_support_hint(&e);
                    last = e.to_string();
                }
                Err(e) => return Err(AudioAttemptError::Terminal(e.to_string())),
            }
        }
        Err(AudioAttemptError::FallThrough(last))
    }

    fn audio_support_hint(&self, error: &BlossomError) {
        // A 415 on audio/* means the relay has audio uploads disabled even if
        // the NIP-11 document said otherwise; remember that for later turns.
        if matches!(error, BlossomError::Refused { status: 415, .. }) {
            self.audio_support.store(false);
        }
    }

    async fn try_envelope(
        &self,
        origin: &RelayOrigin,
        file: &OutboundFile,
        scratch: &Path,
    ) -> Result<PublishedMedia, AudioAttemptError> {
        let Some(ffmpeg) = self.ffmpeg else {
            return Err(AudioAttemptError::FallThrough(
                "ffmpeg not available".into(),
            ));
        };
        let stamp = now_ms();
        let out = scratch.join(voice_note_filename("mp4", stamp));
        crate::ffmpeg::wrap_as_voice_note_mp4(ffmpeg, &file.path, &out)
            .await
            .map_err(AudioAttemptError::FallThrough)?;
        let bytes = read_bounded(&out)
            .await
            .map_err(AudioAttemptError::FallThrough)?;
        match blossom::upload_blob(self.rest, origin, bytes, "video/mp4").await {
            Ok(descriptor) => Ok(PublishedMedia {
                descriptor,
                filename: voice_note_filename("mp4", stamp),
                kind: MediaKind::Audio,
                delivery: Some(AudioDelivery::Mp4Envelope),
            }),
            Err(e) if falls_through(&e) => Err(AudioAttemptError::FallThrough(e.to_string())),
            Err(e) => Err(AudioAttemptError::Terminal(e.to_string())),
        }
    }

    async fn try_generic(
        &self,
        origin: &RelayOrigin,
        file: &OutboundFile,
    ) -> Result<PublishedMedia, AudioAttemptError> {
        let bytes = read_bounded(&file.path)
            .await
            .map_err(AudioAttemptError::Terminal)?;
        match blossom::upload_blob(self.rest, origin, bytes, "application/octet-stream").await {
            Ok(descriptor) => Ok(PublishedMedia {
                descriptor,
                filename: file.filename.clone(),
                kind: MediaKind::Audio,
                delivery: Some(AudioDelivery::GenericFile),
            }),
            Err(e) => Err(AudioAttemptError::Terminal(e.to_string())),
        }
    }

    async fn publish_message(
        &self,
        target: &ReplyTarget,
        items: &[PublishedMedia],
    ) -> Result<String, String> {
        let (content, media_tags) = compose_message(items);
        let thread_ref = buzz_sdk::ThreadRef {
            root_event_id: target.root_event_id,
            parent_event_id: target.root_event_id,
        };
        let builder = buzz_sdk::build_message(
            target.channel_id,
            &content,
            Some(&thread_ref),
            &[],
            false,
            &media_tags,
            &[],
        )
        .map_err(|e| format!("build failed: {e}"))?;
        let event = builder
            .sign_with_keys(&self.rest.keys)
            .map_err(|e| format!("sign failed: {e}"))?;
        let event_id = event.id.to_hex();
        match tokio::time::timeout(PUBLISH_TIMEOUT, self.rest.submit_event(&event)).await {
            Ok(Ok(_)) => Ok(event_id),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err(format!("timed out after {PUBLISH_TIMEOUT:?}")),
        }
    }
}

enum AudioAttemptError {
    /// This delivery is not possible here; try the next one.
    FallThrough(String),
    /// The relay or the disk is failing; stop trying.
    Terminal(String),
}

async fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|e| format!("cannot stat {}: {e}", path.display()))?;
    if meta.len() > MAX_OUTBOUND_FILE_BYTES {
        return Err(format!(
            "{} is {} bytes, over the {MAX_OUTBOUND_FILE_BYTES} byte limit",
            path.display(),
            meta.len()
        ));
    }
    tokio::fs::read(path)
        .await
        .map_err(|e| format!("cannot read {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::{JsonUtil as _, Keys};

    fn temp_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!("buzz-acp-out-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn capture_keeps_text_tail_and_bounded_blocks() {
        let mut capture = TurnMediaCapture::default();
        capture.record_chunk(&serde_json::json!({"type": "text", "text": "hello "}));
        capture.record_chunk(&serde_json::json!({"text": "world"}));
        assert_eq!(capture.text(), "hello world");

        let big = "x".repeat(MAX_CAPTURED_TEXT_BYTES);
        capture.record_chunk(&serde_json::json!({"type": "text", "text": big}));
        capture.record_chunk(&serde_json::json!({"type": "text", "text": "\nMEDIA:/tmp/a.mp3"}));
        assert!(capture.text().len() <= MAX_CAPTURED_TEXT_BYTES);
        assert!(capture.text().ends_with("MEDIA:/tmp/a.mp3"));
        assert!(!capture.text().starts_with("hello"));

        for i in 0..(MAX_CAPTURED_BLOCKS + 2) {
            capture.record_chunk(&serde_json::json!({
                "type": "resource_link", "uri": format!("file:///tmp/{i}.png")
            }));
        }
        assert_eq!(capture.blocks().len(), MAX_CAPTURED_BLOCKS);
        assert_eq!(capture.dropped_blocks(), 2);

        let mut oversized = TurnMediaCapture::default();
        oversized.record_chunk(&serde_json::json!({
            "type": "audio", "mimeType": "audio/mpeg", "data": "A".repeat(MAX_INLINE_BASE64_BYTES + 1)
        }));
        assert!(oversized.blocks().is_empty());
        assert_eq!(oversized.dropped_blocks(), 1);
        assert!(!oversized.is_empty(), "a dropped block is still a signal");
        assert!(TurnMediaCapture::default().is_empty());
    }

    #[test]
    fn media_paths_are_extension_anchored_like_hermes() {
        let text = "Here you go.\nMEDIA:/tmp/out/voice.mp3\nAlso (MEDIA:/tmp/pic.PNG).\nnotMEDIA:/tmp/x.mp3 MEDIA:relative.mp3 MEDIA:/tmp/noext MEDIA: /tmp/spaced.wav MEDIA:~/home.ogg MEDIA:/tmp/out/voice.mp3";
        assert_eq!(
            extract_media_paths(text),
            vec![
                "/tmp/out/voice.mp3".to_string(),
                "/tmp/pic.PNG".to_string(),
                "/tmp/spaced.wav".to_string(),
                "~/home.ogg".to_string(),
            ]
        );
        assert!(extract_media_paths("MEDIA: is a tag used by tools").is_empty());
        assert!(extract_media_paths("MEDIA:/tmp/archive.tar.gz").is_empty());
        assert_eq!(
            extract_media_paths("MEDIA:/tmp/a.mp3."),
            vec!["/tmp/a.mp3".to_string()]
        );
    }

    #[test]
    fn mime_and_kind_tables() {
        assert_eq!(mime_for_path(Path::new("/x/a.MP3")), "audio/mpeg");
        assert_eq!(mime_for_path(Path::new("/x/a.png")), "image/png");
        assert_eq!(
            mime_for_path(Path::new("/x/a.weird")),
            "application/octet-stream"
        );
        assert_eq!(media_kind("audio/mpeg"), MediaKind::Audio);
        assert_eq!(media_kind("IMAGE/png"), MediaKind::Image);
        assert_eq!(media_kind("video/mp4"), MediaKind::Video);
        assert_eq!(media_kind("application/pdf"), MediaKind::File);
    }

    #[test]
    fn resolve_files_from_text_and_blocks() {
        let root = temp_root();
        let audio = root.join("clip.wav");
        std::fs::write(&audio, b"RIFF....").unwrap();
        let png = root.join("shot.png");
        std::fs::write(&png, b"\x89PNG").unwrap();
        let empty = root.join("empty.mp3");
        std::fs::write(&empty, b"").unwrap();

        let mut capture = TurnMediaCapture::default();
        capture.record_chunk(&serde_json::json!({
            "type": "text",
            "text": format!("done\nMEDIA:{}\nMEDIA:{}\nMEDIA:{}\nMEDIA:/nonexistent/x.mp3", audio.display(), empty.display(), audio.display())
        }));
        capture.record_chunk(&serde_json::json!({
            "type": "resource_link", "uri": format!("file://{}", png.display()), "name": "screen.png", "mimeType": "image/png"
        }));
        capture.record_chunk(&serde_json::json!({
            "type": "audio", "mimeType": "audio/mpeg",
            "data": base64::engine::general_purpose::STANDARD.encode(b"mp3bytes")
        }));
        capture.record_chunk(&serde_json::json!({
            "type": "resource_link", "uri": "https://example.com/not-local.png"
        }));
        let scratch = root.join("out");
        let resolved = resolve_outbound_files(&capture, &scratch);

        let paths: Vec<&Path> = resolved.files.iter().map(|f| f.path.as_path()).collect();
        assert_eq!(paths[0], audio.as_path());
        assert_eq!(resolved.files[0].mime, "audio/wav");
        assert_eq!(resolved.files[0].origin, "MEDIA: line");
        assert_eq!(paths[1], png.as_path());
        assert_eq!(resolved.files[1].filename, "screen.png");
        assert_eq!(resolved.files[1].mime, "image/png");
        assert_eq!(resolved.files[2].path, scratch.join("inline-1.mp3"));
        assert_eq!(std::fs::read(&resolved.files[2].path).unwrap(), b"mp3bytes");
        assert_eq!(resolved.files.len(), 3, "duplicate MEDIA path collapsed");
        assert!(
            resolved.notes.iter().any(|n| n.contains("empty file")),
            "{:?}",
            resolved.notes
        );
        assert!(
            resolved
                .notes
                .iter()
                .any(|n| n.contains("/nonexistent/x.mp3")),
            "{:?}",
            resolved.notes
        );
        assert!(
            resolved
                .notes
                .iter()
                .any(|n| n.contains("not a local file URI")),
            "{:?}",
            resolved.notes
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolve_caps_file_count() {
        let root = temp_root();
        let mut text = String::new();
        for i in 0..(MAX_OUTBOUND_FILES + 1) {
            let p = root.join(format!("f{i}.txt"));
            std::fs::write(&p, b"x").unwrap();
            text.push_str(&format!("MEDIA:{}\n", p.display()));
        }
        let mut capture = TurnMediaCapture::default();
        capture.record_chunk(&serde_json::json!({"type": "text", "text": text}));
        let resolved = resolve_outbound_files(&capture, &root.join("out"));
        assert_eq!(resolved.files.len(), MAX_OUTBOUND_FILES);
        assert!(
            resolved.notes.iter().any(|n| n.contains("file limit")),
            "{:?}",
            resolved.notes
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn audio_delivery_plan_matrix() {
        use AudioDelivery::*;
        assert_eq!(
            audio_delivery_plan(true, true),
            vec![NativeAudio, Mp4Envelope, GenericFile]
        );
        assert_eq!(
            audio_delivery_plan(true, false),
            vec![NativeAudio, GenericFile]
        );
        assert_eq!(
            audio_delivery_plan(false, true),
            vec![Mp4Envelope, GenericFile]
        );
        assert_eq!(audio_delivery_plan(false, false), vec![GenericFile]);
        assert!(falls_through(&BlossomError::Refused {
            status: 415,
            body: String::new()
        }));
        assert!(falls_through(&BlossomError::Refused {
            status: 422,
            body: String::new()
        }));
        assert!(!falls_through(&BlossomError::Refused {
            status: 500,
            body: String::new()
        }));
        assert!(!falls_through(&BlossomError::Http("down".into())));
    }

    fn published(kind: MediaKind, filename: &str, duration: Option<f64>) -> PublishedMedia {
        PublishedMedia {
            descriptor: BlobDescriptor {
                url: format!("https://relay.example/media/{}.bin", "a".repeat(64)),
                sha256: "a".repeat(64),
                size: 42,
                mime_type: match kind {
                    MediaKind::Audio => "audio/mpeg",
                    MediaKind::Image => "image/png",
                    MediaKind::Video => "video/mp4",
                    MediaKind::File => "application/pdf",
                }
                .into(),
                dim: (kind == MediaKind::Image).then(|| "16x16".to_string()),
                duration,
            },
            filename: filename.into(),
            kind,
            delivery: None,
        }
    }

    #[test]
    fn imeta_and_body_follow_the_client_contract() {
        let url = format!("https://relay.example/media/{}.bin", "a".repeat(64));
        let voice = published(MediaKind::Audio, "voice-note-1700.mp3", Some(3.25));
        assert_eq!(
            imeta_tag(&voice),
            vec![
                "imeta".to_string(),
                format!("url {url}"),
                "m audio/mpeg".to_string(),
                format!("x {}", "a".repeat(64)),
                "size 42".to_string(),
                "duration 3.25".to_string(),
                "filename voice-note-1700.mp3".to_string(),
            ]
        );
        assert_eq!(body_line(&voice), format!("[voice-note-1700.mp3]({url})"));
        assert!(crate::attachments::is_voice_note(
            "audio/mpeg",
            &voice.filename
        ));

        let image = published(MediaKind::Image, "shot.png", None);
        assert!(imeta_tag(&image).contains(&"dim 16x16".to_string()));
        assert_eq!(body_line(&image), format!("![image]({url})"));
        let video = published(MediaKind::Video, "clip.mp4", Some(1.0));
        assert_eq!(body_line(&video), format!("![video]({url})"));
        let file = published(MediaKind::File, "doc.pdf", None);
        assert_eq!(body_line(&file), format!("[doc.pdf]({url})"));

        let (body, tags) = compose_message(&[voice, image]);
        assert_eq!(body.lines().count(), 2);
        assert_eq!(tags.len(), 2);
        assert_eq!(voice_note_filename("mp4", 12), "voice-note-12.mp4");
    }

    #[test]
    fn reply_target_anchors_to_thread_root_or_trigger() {
        let keys = Keys::generate();
        let channel = uuid::Uuid::new_v4();
        let root = nostr::EventBuilder::new(nostr::Kind::Custom(9), "root")
            .sign_with_keys(&keys)
            .unwrap();
        // A direct reply to the root carries both markers pointing at it; a
        // lone `root` marker is top-level per ingest and must not anchor.
        let reply = nostr::EventBuilder::new(nostr::Kind::Custom(9), "reply")
            .tags([
                nostr::Tag::parse(["e", &root.id.to_hex(), "", "root"]).unwrap(),
                nostr::Tag::parse(["e", &root.id.to_hex(), "", "reply"]).unwrap(),
            ])
            .sign_with_keys(&keys)
            .unwrap();
        let root_only = nostr::EventBuilder::new(nostr::Kind::Custom(9), "top-level")
            .tags([nostr::Tag::parse(["e", &root.id.to_hex(), "", "root"]).unwrap()])
            .sign_with_keys(&keys)
            .unwrap();
        assert_eq!(
            ReplyTarget::for_trigger(channel, &reply).root_event_id,
            root.id
        );
        assert_eq!(
            ReplyTarget::for_trigger(channel, &root).root_event_id,
            root.id
        );
        assert_eq!(
            ReplyTarget::for_trigger(channel, &root_only).root_event_id,
            root_only.id
        );
    }

    /// Scripted HTTP/1.1 server: one canned response per accepted
    /// connection, in order; captures each request (head plus body). The
    /// script is built from the bound base URL so descriptors can point at
    /// the server itself.
    async fn scripted_server(
        script: impl FnOnce(&str) -> Vec<(&'static str, String)>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let responses = script(&base);
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        tokio::spawn(async move {
            for (status, body) in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut chunk = vec![0u8; 16384];
                let mut header_end = None;
                loop {
                    let n = socket.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if header_end.is_none() {
                        header_end = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4);
                    }
                    if let Some(end) = header_end {
                        let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if buf.len() >= end + len {
                            break;
                        }
                    }
                }
                seen.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf).into_owned());
                let resp = format!(
                    "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(resp.as_bytes()).await.unwrap();
                socket.shutdown().await.ok();
            }
        });
        (base, requests)
    }

    fn descriptor_json(base: &str, sha: &str, mime: &str, size: usize) -> String {
        format!(
            r#"{{"url":"{base}/media/{sha}.bin","sha256":"{sha}","size":{size},"type":"{mime}","uploaded":1,"duration":2.5}}"#
        )
    }

    fn rest_for(base: &str) -> RestClient {
        RestClient {
            http: reqwest::Client::new(),
            base_url: base.to_string(),
            keys: Keys::generate(),
            auth_tag_json: None,
        }
    }

    fn decode_auth(head: &str) -> nostr::Event {
        let line = head
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("authorization: nostr "))
            .expect("Authorization header");
        let token = line.split_whitespace().nth(2).unwrap();
        let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(token)
            .unwrap();
        nostr::Event::from_json(&json).unwrap()
    }

    #[tokio::test]
    async fn native_audio_upload_then_single_kind9_publish() {
        let root = temp_root();
        let mp3 = root.join("reply.mp3");
        let bytes = b"\xff\xfbfake-mp3".to_vec();
        std::fs::write(&mp3, &bytes).unwrap();
        let sha = blossom::sha256_hex(&bytes);

        // Connection 1: PUT /upload -> descriptor. Connection 2: POST /events -> ok.
        let (base, requests) = scripted_server(|base| {
            vec![
                (
                    "200 OK",
                    descriptor_json(base, &sha, "audio/mpeg", bytes.len()),
                ),
                ("200 OK", r#"{"accepted":true}"#.to_string()),
            ]
        })
        .await;
        let rest = rest_for(&base);
        let cache = AudioSupportCache::default();
        cache.store(true);
        let publisher = MediaPublisher {
            rest: &rest,
            audio_support: &cache,
            ffmpeg: None,
        };
        let keys = Keys::generate();
        let trigger = nostr::EventBuilder::new(nostr::Kind::Custom(9), "say hi")
            .sign_with_keys(&keys)
            .unwrap();
        let target = ReplyTarget::for_trigger(uuid::Uuid::new_v4(), &trigger);
        let mut capture = TurnMediaCapture::default();
        capture.record_chunk(&serde_json::json!({
            "type": "text", "text": format!("Here is your note.\nMEDIA:{}", mp3.display())
        }));

        let report = publisher
            .publish_turn_media(&target, &capture, &root.join("out"))
            .await
            .expect("media referenced");

        assert!(report.is_clean(), "{:?}", report.failed);
        assert_eq!(report.published.len(), 1);
        assert_eq!(
            report.published[0].delivery,
            Some(AudioDelivery::NativeAudio)
        );
        assert!(report.published[0].filename.starts_with("voice-note-"));
        assert!(report.published[0].filename.ends_with(".mp3"));
        assert!(report.event_id.is_some());

        let seen = requests.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "one upload and exactly one kind-9 publish");
        let upload = &seen[0];
        assert!(upload.starts_with("PUT /upload HTTP/1.1"), "{upload}");
        assert!(upload
            .to_ascii_lowercase()
            .contains("content-type: audio/mpeg"));
        assert!(upload
            .to_ascii_lowercase()
            .contains(&format!("x-sha-256: {sha}")));
        let auth = decode_auth(upload);
        assert_eq!(auth.kind.as_u16(), 24242);
        assert_eq!(auth.pubkey, rest.keys.public_key());
        assert!(auth.verify().is_ok());
        let tags: Vec<Vec<String>> = auth.tags.iter().map(|t| t.as_slice().to_vec()).collect();
        assert!(tags.contains(&vec!["t".to_string(), "upload".to_string()]));
        assert!(tags.contains(&vec!["x".to_string(), sha.clone()]));
        assert!(
            upload.ends_with(std::str::from_utf8(&bytes).unwrap_or(""))
                || upload.contains("fake-mp3")
        );

        let publish = &seen[1];
        assert!(publish.starts_with("POST /events HTTP/1.1"), "{publish}");
        let body_start = publish.find("\r\n\r\n").unwrap() + 4;
        let event: nostr::Event = serde_json::from_str(&publish[body_start..]).unwrap();
        assert_eq!(event.kind.as_u16(), 9);
        assert_eq!(event.pubkey, rest.keys.public_key());
        let tags: Vec<Vec<String>> = event.tags.iter().map(|t| t.as_slice().to_vec()).collect();
        assert!(tags
            .iter()
            .any(|t| t[0] == "h" && t[1] == target.channel_id.to_string()));
        assert!(tags
            .iter()
            .any(|t| t[0] == "e" && t[1] == trigger.id.to_hex()));
        let imeta = tags.iter().find(|t| t[0] == "imeta").expect("imeta tag");
        assert!(imeta.contains(&"m audio/mpeg".to_string()));
        assert!(imeta.contains(&format!("x {sha}")));
        assert!(imeta.contains(&"duration 2.5".to_string()));
        assert!(imeta.iter().any(|f| f.starts_with("filename voice-note-")));
        assert!(
            event.content.starts_with("[voice-note-"),
            "{}",
            event.content
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn audio_rejection_falls_back_to_generic_file_without_ffmpeg() {
        let root = temp_root();
        let mp3 = root.join("reply.mp3");
        let bytes = b"ID3tagged".to_vec();
        std::fs::write(&mp3, &bytes).unwrap();
        let sha = blossom::sha256_hex(&bytes);
        let (base, requests) = scripted_server(|base| {
            vec![
                (
                    "422 Unprocessable Entity",
                    r#"{"error":"metadata forbidden"}"#.to_string(),
                ),
                (
                    "200 OK",
                    descriptor_json(base, &sha, "application/octet-stream", bytes.len()),
                ),
                ("200 OK", r#"{"accepted":true}"#.to_string()),
            ]
        })
        .await;
        let rest = rest_for(&base);
        let cache = AudioSupportCache::default();
        cache.store(true);
        let publisher = MediaPublisher {
            rest: &rest,
            audio_support: &cache,
            ffmpeg: None,
        };
        let keys = Keys::generate();
        let trigger = nostr::EventBuilder::new(nostr::Kind::Custom(9), "x")
            .sign_with_keys(&keys)
            .unwrap();
        let target = ReplyTarget::for_trigger(uuid::Uuid::new_v4(), &trigger);
        let mut capture = TurnMediaCapture::default();
        capture.record_chunk(&serde_json::json!({
            "type": "resource_link", "uri": format!("file://{}", mp3.display()), "mimeType": "audio/mpeg"
        }));

        let report = publisher
            .publish_turn_media(&target, &capture, &root.join("out"))
            .await
            .unwrap();

        assert!(report.is_clean(), "{:?}", report.failed);
        assert_eq!(
            report.published[0].delivery,
            Some(AudioDelivery::GenericFile)
        );
        assert_eq!(
            report.published[0].filename, "reply.mp3",
            "generic keeps the original name"
        );
        let seen = requests.lock().unwrap().clone();
        assert_eq!(seen.len(), 3);
        assert!(seen[0]
            .to_ascii_lowercase()
            .contains("content-type: audio/mpeg"));
        assert!(seen[1]
            .to_ascii_lowercase()
            .contains("content-type: application/octet-stream"));
        assert!(seen[2].starts_with("POST /events"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn transport_failure_is_terminal_and_reported() {
        let root = temp_root();
        let mp3 = root.join("reply.mp3");
        std::fs::write(&mp3, b"bytes").unwrap();
        let (base, requests) =
            scripted_server(|_| vec![("500 Internal Server Error", "boom".to_string())]).await;
        let rest = rest_for(&base);
        let cache = AudioSupportCache::default();
        cache.store(true);
        let publisher = MediaPublisher {
            rest: &rest,
            audio_support: &cache,
            ffmpeg: None,
        };
        let keys = Keys::generate();
        let trigger = nostr::EventBuilder::new(nostr::Kind::Custom(9), "x")
            .sign_with_keys(&keys)
            .unwrap();
        let target = ReplyTarget::for_trigger(uuid::Uuid::new_v4(), &trigger);
        let mut capture = TurnMediaCapture::default();
        capture.record_chunk(
            &serde_json::json!({"type": "text", "text": format!("MEDIA:{}", mp3.display())}),
        );

        let report = publisher
            .publish_turn_media(&target, &capture, &root.join("out"))
            .await
            .unwrap();

        assert!(report.published.is_empty());
        assert!(
            report.event_id.is_none(),
            "nothing published, nothing announced"
        );
        assert_eq!(report.failed.len(), 1);
        assert!(
            report.failed[0].contains("HTTP 500"),
            "{}",
            report.failed[0]
        );
        assert_eq!(requests.lock().unwrap().len(), 1, "no fallback after a 500");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn text_without_media_publishes_nothing() {
        let rest = rest_for("http://127.0.0.1:1");
        let cache = AudioSupportCache::default();
        let publisher = MediaPublisher {
            rest: &rest,
            audio_support: &cache,
            ffmpeg: None,
        };
        let keys = Keys::generate();
        let trigger = nostr::EventBuilder::new(nostr::Kind::Custom(9), "x")
            .sign_with_keys(&keys)
            .unwrap();
        let target = ReplyTarget::for_trigger(uuid::Uuid::new_v4(), &trigger);
        let mut capture = TurnMediaCapture::default();
        capture.record_chunk(&serde_json::json!({"type": "text", "text": "plain reply, no files"}));
        assert!(publisher
            .publish_turn_media(&target, &capture, Path::new("/nonexistent"))
            .await
            .is_none());
    }
}
