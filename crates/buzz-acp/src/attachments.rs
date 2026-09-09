//! Inbound attachments: from `imeta` tags on a dispatched event to local
//! files the ACP engine can open.
//!
//! The relay serves media only to a relay member presenting a kind-24242 get
//! authorization, and the engine never holds the agent key, so a bare URL in
//! the prompt is useless to it. This module validates each `imeta` tag the
//! way the Hermes gateway plugin does (first value wins per key, relay origin
//! only, 64-hex `x`, positive bounded `size`, sanitised `filename`), fetches
//! the blob with the agent key, checks size and hash, stores it under the
//! per-agent temp directory, and renders both a `<buzz-attachments>` prompt
//! section and one `resource_link` content block per stored file.
//!
//! Nothing is dropped silently: every rejected tag, failed download, and
//! over-cap attachment is named in the section with its reason (rule 1).
//!
//! Storage is bounded two ways: the whole inbound phase runs under
//! [`INBOUND_DEADLINE`], and the attachment root keeps at most
//! [`KEEP_TURN_DIRS`] turn directories, never pruning one whose turn is
//! still running or still publishing ([`LiveTurnDirs`]).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use nostr::Event;

use crate::acp::PromptBlock;
use crate::blossom::{self, BlossomError, RelayOrigin};
use crate::prompt_framing::{escape_semantic_text, semantic_section};
use crate::relay::RestClient;

/// Most attachments fetched for one turn, across every event in the batch.
pub(crate) const MAX_INBOUND_ATTACHMENTS: usize = 4;
/// Most bytes fetched for one turn, summed over accepted `imeta` sizes.
pub(crate) const MAX_INBOUND_BYTES: u64 = 25 * 1024 * 1024;
/// Longest `m` value kept.
const MAX_MIME_LEN: usize = 255;
/// Longest sanitised filename, in bytes.
const MAX_FILENAME_BYTES: usize = 120;
/// Turn directories kept under the attachment root before the oldest go.
const KEEP_TURN_DIRS: usize = 16;
/// Directory under the attachment root that holds the per-publish scratch
/// directories ([`PublishScratch`]). It is not a turn directory and is never
/// pruned or handed to the engine.
const OUTBOUND_DIR: &str = ".outbound";
/// Wall-clock cap for fetching one turn's attachments, all events included.
pub(crate) const INBOUND_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// One structurally valid `imeta` attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImetaAttachment {
    /// Blob URL, already checked against the relay origin.
    pub url: String,
    /// Lowercase 64-hex SHA-256 from the `x` field.
    pub sha256: String,
    /// Declared size from the `size` field.
    pub size: u64,
    /// Safe basename derived from `filename` (or `attachment.bin`).
    pub filename: String,
    /// Bounded `m` value, possibly empty.
    pub mime_type: String,
}

impl ImetaAttachment {
    /// True for a Buzz voice note: an `audio/*` blob or a `video/mp4`
    /// envelope, both named `voice-note-*`. Mirrors `isVoiceNoteAttachment`
    /// in the desktop client and `classifyMediaUrl` on mobile.
    pub fn is_voice_note(&self) -> bool {
        is_voice_note(&self.mime_type, &self.filename)
    }

    /// True when the engine should treat the blob as audio.
    pub fn is_audio(&self) -> bool {
        self.mime_type.to_ascii_lowercase().starts_with("audio/") || self.is_voice_note()
    }
}

/// Voice-note test shared with the client rendering rules.
pub fn is_voice_note(mime_type: &str, filename: &str) -> bool {
    let mime = mime_type.to_ascii_lowercase();
    let name = filename.to_ascii_lowercase();
    if !name.starts_with("voice-note-") {
        return false;
    }
    if mime.starts_with("audio/") {
        return true;
    }
    mime == "video/mp4" && name.ends_with(".mp4")
}

/// Per-turn admission budget shared by every event in a batch.
#[derive(Debug, Clone)]
pub struct AttachmentBudget {
    remaining_count: usize,
    remaining_bytes: u64,
}

impl Default for AttachmentBudget {
    fn default() -> Self {
        Self {
            remaining_count: MAX_INBOUND_ATTACHMENTS,
            remaining_bytes: MAX_INBOUND_BYTES,
        }
    }
}

impl AttachmentBudget {
    fn admit(&mut self, size: u64) -> Result<(), String> {
        if self.remaining_count == 0 {
            return Err(format!(
                "over the {MAX_INBOUND_ATTACHMENTS}-attachment limit for this turn"
            ));
        }
        if size > self.remaining_bytes {
            return Err(format!(
                "size {size} exceeds the remaining {} byte budget for this turn",
                self.remaining_bytes
            ));
        }
        self.remaining_count -= 1;
        self.remaining_bytes -= size;
        Ok(())
    }
}

/// Result of parsing every `imeta` tag on one event.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ParsedAttachments {
    /// Tags that passed every structural check, in tag order.
    pub accepted: Vec<ImetaAttachment>,
    /// One reason per rejected tag.
    pub rejected: Vec<String>,
}

/// Split an `imeta` tag into `key value` fields; the first value per key wins.
pub fn imeta_fields(tag: &[String]) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    for entry in tag.iter().skip(1) {
        if let Some((key, value)) = entry.split_once(' ') {
            fields
                .entry(key.to_string())
                .or_insert_with(|| value.trim().to_string());
        }
    }
    fields
}

/// Unicode format characters (general category Cf): zero-width joiners,
/// bidirectional overrides such as U+202E, soft hyphens, and the like. They
/// are invisible in a prompt or a chat body and can reorder what a reader
/// sees, so they are stripped alongside control characters.
fn is_format_char(c: char) -> bool {
    matches!(
        c as u32,
        0x00AD
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    )
}

/// Reduce an untrusted `filename` field to a safe basename.
///
/// Path separators, control and format characters, and dot-only names are
/// removed; the result is capped at [`MAX_FILENAME_BYTES`] while keeping a
/// short extension.
pub fn safe_attachment_filename(value: &str) -> String {
    let base = value.replace('\\', "/");
    let base = base.rsplit('/').next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !is_format_char(*c))
        .collect::<String>()
        .trim()
        .to_string();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return "attachment.bin".to_string();
    }
    let (stem, suffix) = match cleaned.rfind('.') {
        Some(idx) if idx > 0 && cleaned.len() - idx <= 20 => cleaned.split_at(idx),
        _ => (cleaned.as_str(), ""),
    };
    let budget = MAX_FILENAME_BYTES.saturating_sub(suffix.len());
    let mut safe_stem = String::new();
    for ch in stem.chars() {
        if safe_stem.len() + ch.len_utf8() > budget {
            break;
        }
        safe_stem.push(ch);
    }
    let safe_stem = safe_stem.trim_end_matches([' ', '.']);
    let safe_stem = if safe_stem.is_empty() {
        "attachment"
    } else {
        safe_stem
    };
    format!("{safe_stem}{suffix}")
}

/// Parse and validate the `imeta` tags on `event` against `origin` and `budget`.
pub fn parse_imeta_attachments(
    event: &Event,
    origin: &RelayOrigin,
    budget: &mut AttachmentBudget,
) -> ParsedAttachments {
    let mut parsed = ParsedAttachments::default();
    for tag in event.tags.iter() {
        let parts = tag.as_slice();
        if parts.first().map(String::as_str) != Some("imeta") {
            continue;
        }
        match validate_imeta(parts, origin, budget) {
            Ok(attachment) => parsed.accepted.push(attachment),
            Err(reason) => parsed.rejected.push(reason),
        }
    }
    parsed
}

fn validate_imeta(
    parts: &[String],
    origin: &RelayOrigin,
    budget: &mut AttachmentBudget,
) -> Result<ImetaAttachment, String> {
    let fields = imeta_fields(parts);
    let url = fields.get("url").cloned().unwrap_or_default();
    if url.is_empty() {
        return Err("imeta tag has no url".into());
    }
    let parsed_url = origin.permits(&url).map_err(|e| e.to_string())?;
    let sha256 = fields
        .get("x")
        .map(|x| x.to_ascii_lowercase())
        .unwrap_or_default();
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("imeta x is not a 64-hex SHA-256".into());
    }
    let size = fields
        .get("size")
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .ok_or_else(|| "imeta size is missing, zero, or not an integer".to_string())?;
    if size > MAX_INBOUND_BYTES {
        return Err(format!(
            "imeta size {size} exceeds the {MAX_INBOUND_BYTES} byte cap"
        ));
    }
    budget.admit(size)?;
    let mime_type: String = fields
        .get("m")
        .map(|m| m.chars().take(MAX_MIME_LEN).collect())
        .unwrap_or_default();
    let filename =
        safe_attachment_filename(fields.get("filename").map(String::as_str).unwrap_or(""));
    Ok(ImetaAttachment {
        url: parsed_url.to_string(),
        sha256,
        size,
        filename,
        mime_type,
    })
}

/// A blob stored on disk for the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalAttachment {
    /// Absolute path of the file the engine should open.
    pub path: PathBuf,
    /// Basename of `path`.
    pub filename: String,
    /// MIME type of the file at `path` (may differ from the imeta `m` after
    /// voice-note extraction).
    pub mime_type: String,
    /// Size of the file at `path`.
    pub size: u64,
    /// SHA-256 of the original blob (the imeta `x`).
    pub sha256: String,
    /// True when the blob is a voice note or other audio.
    pub is_audio: bool,
    /// True when the blob was a voice note (native audio or MP4 envelope).
    pub is_voice_note: bool,
    /// Extra note for the prompt, e.g. that audio was extracted from the
    /// envelope or that extraction failed.
    pub note: Option<String>,
}

/// What happened to one attachment on one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentOutcome {
    /// Downloaded, verified, and stored.
    Stored {
        /// Event that carried the tag.
        event_id: String,
        /// The stored file.
        local: LocalAttachment,
    },
    /// Structurally valid but the fetch or integrity check failed.
    Failed {
        /// Event that carried the tag.
        event_id: String,
        /// Declared filename.
        filename: String,
        /// Why it failed.
        reason: String,
    },
    /// The tag itself was rejected before any request was made.
    Rejected {
        /// Event that carried the tag.
        event_id: String,
        /// Why it was rejected.
        reason: String,
    },
    /// The same blob (by `x`) was already fetched for this turn; it is not
    /// fetched twice, and the engine is told where the first copy went.
    Duplicate {
        /// Event that carried the tag.
        event_id: String,
        /// Declared filename on this tag.
        filename: String,
        /// Event whose copy of the blob was stored.
        first_event_id: String,
    },
}

/// Everything the harness learned about a batch's attachments.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InboundAttachments {
    /// One entry per `imeta` tag seen, in event order.
    pub outcomes: Vec<AttachmentOutcome>,
}

impl InboundAttachments {
    /// True when no event in the batch carried an `imeta` tag.
    pub fn is_empty(&self) -> bool {
        self.outcomes.is_empty()
    }

    /// Name every `imeta` tag on `events` as rejected for one shared reason,
    /// for when the harness cannot store blobs at all (no directory). Keeps
    /// rule 1: the engine is told what it did not get.
    pub fn unavailable(events: &[&Event], reason: &str) -> Self {
        let mut inbound = Self::default();
        for event in events {
            let count = event
                .tags
                .iter()
                .filter(|t| t.as_slice().first().map(String::as_str) == Some("imeta"))
                .count();
            for _ in 0..count {
                inbound.outcomes.push(AttachmentOutcome::Rejected {
                    event_id: event.id.to_hex(),
                    reason: reason.to_string(),
                });
            }
        }
        inbound
    }

    /// Stored files only.
    pub fn stored(&self) -> impl Iterator<Item = &LocalAttachment> {
        self.outcomes.iter().filter_map(|o| match o {
            AttachmentOutcome::Stored { local, .. } => Some(local),
            _ => None,
        })
    }

    /// Render the `<buzz-attachments>` prompt section, or `None` when there
    /// is nothing to say.
    pub fn section(&self) -> Option<String> {
        if self.outcomes.is_empty() {
            return None;
        }
        let mut lines: Vec<String> = Vec::with_capacity(self.outcomes.len() + 1);
        for outcome in &self.outcomes {
            lines.push(match outcome {
                AttachmentOutcome::Stored { event_id, local } => {
                    let mut line = format!(
                        "Event {event_id}: {} ({}, {} bytes) saved to {}",
                        escape_semantic_text(&local.filename),
                        escape_semantic_text(&local.mime_type),
                        local.size,
                        escape_semantic_text(&local.path.to_string_lossy()),
                    );
                    if local.is_voice_note {
                        line.push_str(" [voice note]");
                    } else if local.is_audio {
                        line.push_str(" [audio]");
                    }
                    if let Some(note) = &local.note {
                        line.push_str(&format!(" ({})", escape_semantic_text(note)));
                    }
                    line
                }
                AttachmentOutcome::Failed {
                    event_id,
                    filename,
                    reason,
                } => format!(
                    "Event {event_id}: attachment {} could not be fetched: {}",
                    escape_semantic_text(filename),
                    escape_semantic_text(reason)
                ),
                AttachmentOutcome::Rejected { event_id, reason } => format!(
                    "Event {event_id}: imeta tag rejected: {}",
                    escape_semantic_text(reason)
                ),
                AttachmentOutcome::Duplicate {
                    event_id,
                    filename,
                    first_event_id,
                } => format!(
                    "Event {event_id}: attachment {} is the same blob as the one on event {first_event_id}; see that entry",
                    escape_semantic_text(filename)
                ),
            });
        }
        if self.stored().next().is_some() {
            lines.push(
                "Open the saved files with your tools; the paths are local to this machine and are pruned after later turns. Voice notes are speech addressed to you."
                    .to_string(),
            );
        }
        Some(semantic_section("buzz-attachments", &lines.join("\n")))
    }

    /// One `resource_link` block per stored file, for the same `session/prompt`.
    pub fn prompt_blocks(&self) -> Vec<PromptBlock> {
        self.stored()
            .map(|local| PromptBlock::ResourceLink {
                uri: file_uri(&local.path),
                name: local.filename.clone(),
                mime_type: (!local.mime_type.is_empty()).then(|| local.mime_type.clone()),
                size: Some(local.size),
                description: Some(if local.is_voice_note {
                    "Buzz voice note attached to the triggering message".to_string()
                } else {
                    "Buzz attachment on the triggering message".to_string()
                }),
            })
            .collect()
    }
}

/// `file://` URI for an absolute path.
pub fn file_uri(path: &Path) -> String {
    url::Url::from_file_path(path)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| format!("file://{}", path.to_string_lossy()))
}

/// Turn directories that must survive the prune: their turn is still being
/// prompted, or its reply media is still being published.
///
/// Shared by every prompt task of one harness. Hold a guard from
/// [`LiveTurnDirs::hold`] for as long as the directory may be read.
#[derive(Debug, Clone, Default)]
pub struct LiveTurnDirs(Arc<Mutex<HashSet<String>>>);

impl LiveTurnDirs {
    /// Mark `turn_id`'s directory live until the returned guard drops.
    pub fn hold(&self, turn_id: &str) -> LiveTurnGuard {
        let name = safe_attachment_filename(turn_id);
        if let Ok(mut set) = self.0.lock() {
            set.insert(name.clone());
        }
        LiveTurnGuard {
            dirs: self.clone(),
            name,
        }
    }

    /// Whether the directory named `dir_name` under the root is live.
    pub fn is_live(&self, dir_name: &str) -> bool {
        self.0.lock().is_ok_and(|set| set.contains(dir_name))
    }

    /// Number of live turn directories.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.0.lock().map(|set| set.len()).unwrap_or(0)
    }

    /// True when no turn directory is live.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Keeps one turn directory out of the prune; see [`LiveTurnDirs::hold`].
#[derive(Debug)]
pub struct LiveTurnGuard {
    dirs: LiveTurnDirs,
    name: String,
}

impl Drop for LiveTurnGuard {
    fn drop(&mut self) {
        if let Ok(mut set) = self.dirs.0.lock() {
            set.remove(&self.name);
        }
    }
}

/// `<root>/<turn_id>`, the directory every inbound blob and outbound scratch
/// file of one turn lives under.
pub fn turn_dir_path(root: &Path, turn_id: &str) -> PathBuf {
    root.join(safe_attachment_filename(turn_id))
}

/// Create `<root>/<turn_id>` and prune the oldest sibling turn directories
/// beyond [`KEEP_TURN_DIRS`], so the attachment root stays bounded no matter
/// how many turns the daemon runs. Directories in `live` are never pruned.
pub fn prepare_turn_dir(
    root: &Path,
    turn_id: &str,
    live: &LiveTurnDirs,
) -> std::io::Result<PathBuf> {
    let turn_dir = turn_dir_path(root, turn_id);
    std::fs::create_dir_all(&turn_dir)?;
    // The root was verified at startup; re-check it per turn so a root
    // replaced underneath a running harness is refused rather than written
    // through.
    check_private_dir(root)?;
    prune_turn_dirs(root, live, Some(&turn_dir))?;
    Ok(turn_dir)
}

/// Remove the oldest turn directories under `root` beyond [`KEEP_TURN_DIRS`],
/// skipping the live ones and `keep`, the directory the caller just created
/// (its mtime says nothing about whether it is in use). Live directories
/// count toward the total, so a busy pool keeps more than `KEEP_TURN_DIRS`
/// on disk only while those turns are running.
pub fn prune_turn_dirs(
    root: &Path,
    live: &LiveTurnDirs,
    keep: Option<&Path>,
) -> std::io::Result<()> {
    let mut siblings: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(root)?
        .filter_map(Result::ok)
        .filter(|entry| keep != Some(entry.path().as_path()))
        .filter(|entry| entry.file_name() != std::ffi::OsStr::new(OUTBOUND_DIR))
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            meta.is_dir().then(|| {
                (
                    meta.modified().ok().unwrap_or(std::time::UNIX_EPOCH),
                    entry.path(),
                )
            })
        })
        .collect();
    siblings.sort();
    let total = siblings.len() + usize::from(keep.is_some());
    let mut excess = total.saturating_sub(KEEP_TURN_DIRS);
    for (_, stale) in siblings {
        if excess == 0 {
            break;
        }
        let name = stale
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if live.is_live(&name) {
            continue;
        }
        excess -= 1;
        if let Err(e) = std::fs::remove_dir_all(&stale) {
            tracing::debug!(target: "acp::media", "prune {} failed: {e}", stale.display());
        }
    }
    Ok(())
}

/// Where the per-agent attachment root goes: `XDG_RUNTIME_DIR` when set (a
/// per-user, mode-0700 directory on Linux hosts), else the system temp dir.
pub fn default_attachment_base() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| !dir.as_os_str().is_empty() && dir.is_absolute() && dir.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

/// Create the per-agent attachment root privately and refuse one that could
/// have been planted by another local user.
///
/// Every directory on the path below `base` is created with mode `0700`
/// (rule 4: a shared `/tmp` on a Linux host is not ours alone) and every one
/// of them is then checked: `<base>/buzz-acp` first, because on the `/tmp`
/// fallback that is the one component another local user could have created
/// before us, then the per-agent directory and the root itself. A directory
/// is accepted only when it is real, not a symlink, and owned by this
/// process; a wider mode is tightened back to `0700`. Any other state is an
/// error, and the caller must not store blobs there.
pub fn prepare_attachment_root(base: &Path, agent_key: &str) -> std::io::Result<PathBuf> {
    let shared = base.join("buzz-acp");
    let per_agent = shared.join(agent_key);
    let root = per_agent.join("attachments");
    private_dir_builder().create(&root)?;
    for dir in [shared.as_path(), per_agent.as_path(), root.as_path()] {
        check_private_dir(dir)?;
    }
    Ok(root)
}

/// A `DirBuilder` that creates every directory it makes with mode `0700`.
fn private_dir_builder() -> std::fs::DirBuilder {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder
}

fn check_private_dir(dir: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() {
        return Err(std::io::Error::other(format!(
            "{} is a symlink; refusing to store attachments through it",
            dir.display()
        )));
    }
    if !meta.is_dir() {
        return Err(std::io::Error::other(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let uid = nix::unistd::Uid::current().as_raw();
        if meta.uid() != uid {
            return Err(std::io::Error::other(format!(
                "{} is owned by uid {} rather than this process (uid {uid}); refusing to use it",
                dir.display(),
                meta.uid()
            )));
        }
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            tracing::warn!(
                target: "acp::media",
                dir = %dir.display(),
                "attachment directory was mode {mode:o}; tightened to 700"
            );
        }
    }
    Ok(())
}

/// A directory the harness resolved once and keeps open, so every later file
/// operation in it is `openat`-relative to a descriptor rather than a fresh
/// walk from `/`.
///
/// This is the module's one file-writing primitive. `O_EXCL | O_NOFOLLOW`
/// guard the last component of a path only, so a plain `fs::write` at a name
/// something else can predict, inside a directory something else can write,
/// is an arbitrary-write primitive: the write follows a symlink planted
/// there. Both sides of the media path have that shape (the inbound download
/// writes a name the *sender* chose; the outbound publish writes staged
/// copies and ffmpeg output), so both go through this.
///
/// Renaming or replacing the directory after the descriptor is taken moves
/// the name, not the descriptor, so a swap redirects nothing. The one thing
/// this cannot cover is a subprocess, which is handed a path and does its own
/// open; [`DirHandle::still_at_its_path`] is checked immediately before that
/// and the result is read back through the descriptor, so a swap costs the
/// operation rather than moving it.
#[derive(Debug)]
pub struct DirHandle {
    dir: PathBuf,
    /// The directory, opened once. Every file operation goes through this,
    /// never through `dir`, which is kept for logs, for subprocesses, and
    /// for naming.
    #[cfg(unix)]
    fd: std::os::fd::OwnedFd,
}

impl DirHandle {
    /// Open an existing directory with `O_DIRECTORY | O_NOFOLLOW` and keep
    /// the descriptor. A symlink at `dir`'s last component is refused rather
    /// than followed.
    pub fn open(dir: &Path) -> std::io::Result<Self> {
        #[cfg(unix)]
        let fd = open_dir_no_follow(dir)?;
        #[cfg(not(unix))]
        if !std::fs::symlink_metadata(dir)?.is_dir() {
            return Err(std::io::Error::other("it is not a directory"));
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            #[cfg(unix)]
            fd,
        })
    }

    /// The directory itself, for logs, for naming, and for the one
    /// subprocess that has to be given a path.
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Create a fresh, empty file at `name` in this directory, relative to
    /// the retained descriptor, with `O_CREAT | O_EXCL | O_NOFOLLOW` and mode
    /// `0600`. `O_EXCL` refuses anything already at the name, a planted
    /// symlink included, so this never writes through one and never silently
    /// overwrites.
    #[cfg(unix)]
    pub fn create_at(&self, name: &std::ffi::OsStr) -> std::io::Result<std::fs::File> {
        use std::os::fd::AsFd as _;
        let flags = nix::fcntl::OFlag::O_WRONLY
            | nix::fcntl::OFlag::O_CREAT
            | nix::fcntl::OFlag::O_EXCL
            | nix::fcntl::OFlag::O_NOFOLLOW
            | nix::fcntl::OFlag::O_CLOEXEC;
        let mode = nix::sys::stat::Mode::from_bits_truncate(0o600);
        let fd =
            nix::fcntl::openat(self.fd.as_fd(), name, flags, mode).map_err(std::io::Error::from)?;
        Ok(std::fs::File::from(fd))
    }

    #[cfg(not(unix))]
    pub fn create_at(&self, name: &std::ffi::OsStr) -> std::io::Result<std::fs::File> {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.dir.join(name))
    }

    /// Reopen a file in this directory for reading, relative to the retained
    /// descriptor and without following a symlink at the last component.
    #[cfg(unix)]
    pub fn open_at(&self, name: &std::ffi::OsStr) -> std::io::Result<std::fs::File> {
        use std::os::fd::AsFd as _;
        let flags = nix::fcntl::OFlag::O_RDONLY
            | nix::fcntl::OFlag::O_NOFOLLOW
            | nix::fcntl::OFlag::O_CLOEXEC;
        let fd = nix::fcntl::openat(self.fd.as_fd(), name, flags, nix::sys::stat::Mode::empty())
            .map_err(std::io::Error::from)?;
        Ok(std::fs::File::from(fd))
    }

    #[cfg(not(unix))]
    pub fn open_at(&self, name: &std::ffi::OsStr) -> std::io::Result<std::fs::File> {
        std::fs::File::open(self.dir.join(name))
    }

    /// Remove a file in this directory, relative to the retained descriptor.
    #[cfg(unix)]
    pub fn unlink_at(&self, name: &std::ffi::OsStr) -> std::io::Result<()> {
        use std::os::fd::AsFd as _;
        nix::unistd::unlinkat(
            self.fd.as_fd(),
            name,
            nix::unistd::UnlinkatFlags::NoRemoveDir,
        )
        .map_err(std::io::Error::from)
    }

    #[cfg(not(unix))]
    pub fn unlink_at(&self, name: &std::ffi::OsStr) -> std::io::Result<()> {
        std::fs::remove_file(self.dir.join(name))
    }

    /// Confirm the path still names the directory the descriptor holds, for
    /// the moment before a subprocess is given a path to write.
    ///
    /// A swap that has already happened is caught here; one that happens
    /// after this returns costs the operation (the output is read back
    /// through the descriptor and is not there) rather than moving it.
    pub fn still_at_its_path(&self) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::fd::AsFd as _;
            let held = nix::sys::stat::fstat(self.fd.as_fd()).map_err(std::io::Error::from)?;
            let named = nix::sys::stat::lstat(&self.dir).map_err(std::io::Error::from)?;
            if (held.st_dev, held.st_ino) != (named.st_dev, named.st_ino) {
                return Err(std::io::Error::other(
                    "the directory path no longer names the directory it was opened as",
                ));
            }
        }
        Ok(())
    }

    /// The single name component of `path` within this directory. A path
    /// from anywhere else is a programming error and is refused rather than
    /// resolved.
    pub fn name_in<'a>(&self, path: &'a Path) -> std::io::Result<&'a std::ffi::OsStr> {
        match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) if parent == self.dir => Ok(name),
            _ => Err(std::io::Error::other("it is not a file in this directory")),
        }
    }
}

/// A harness-private directory for one publish, created fresh with an
/// unguessable name and mode `0700`, and removed when the publish task drops
/// it.
///
/// Both outbound roots are engine-writable, the turn directory because it is
/// handed to the engine as a `file://` URI and the workspace because it is
/// the engine's own working directory, so anything the harness writes at a
/// name the engine can predict is a symlink it can plant: `File::create`
/// overwrite whatever the link points at, with harness privileges. Staged
/// copies, decoded inline blocks, and ffmpeg output therefore never go under
/// either root. They go here, under `<root>/.outbound/pub-<uuid>`.
///
/// The location is not what makes this safe, and it never was: the engine
/// runs under the same uid, `.outbound` is a compile-time constant one level
/// above the turn directory it was handed, and mode `0700` says nothing to a
/// process that is us. A same-uid engine can list the base, learn the name,
/// and swap the directory for a symlink; `O_EXCL | O_NOFOLLOW` guard the
/// last component of a path only, so the swap redirects the write.
///
/// What makes it safe is that the path is resolved exactly once. The
/// directory is opened when it is created, with `O_DIRECTORY | O_NOFOLLOW`,
/// and every later operation on it (create, reopen, unlink) is an
/// `openat`/`unlinkat` against that descriptor rather than a fresh walk from
/// `/`. Renaming or replacing the directory after that point moves the name,
/// not the descriptor, so a swap redirects nothing.
///
/// The one path this cannot cover is a subprocess: ffmpeg is handed a path
/// and does its own open. [`PublishScratch::still_at_its_path`] is checked
/// immediately before that, and the output is read back through the
/// descriptor, so a swap costs the publish rather than moving it. See
/// `media_publish::MediaPublisher`.
#[derive(Debug)]
pub struct PublishScratch {
    /// The directory the publish writes into, opened once when it was
    /// created. See [`DirHandle`].
    dir: DirHandle,
}

impl PublishScratch {
    /// Create the per-publish directory under the attachment `root` and open
    /// it.
    ///
    /// `std::fs::create_dir` is atomic and fails with `EEXIST` on anything
    /// already at the path, symlinks included, which is what `mkdtemp(3)`
    /// buys; the UUID makes the name unguessable. The open that follows is
    /// `O_DIRECTORY | O_NOFOLLOW`, so a directory swapped for a symlink
    /// between the two fails the publish instead of redirecting it.
    pub fn create(root: &Path) -> std::io::Result<Self> {
        let base = root.join(OUTBOUND_DIR);
        private_dir_builder().create(&base)?;
        check_private_dir(&base)?;
        let dir = base.join(format!("pub-{}", uuid::Uuid::new_v4().simple()));
        let mut once = private_dir_builder();
        once.recursive(false);
        once.create(&dir)?;
        let dir = DirHandle::open(&dir)?;
        prune_abandoned_scratch(&base, dir.path());
        Ok(Self { dir })
    }

    /// The directory itself, for logs and for the one subprocess that has to
    /// be given a path.
    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// A fresh, empty file in the directory with a random name, created
    /// relative to the retained descriptor with
    /// `O_CREAT | O_EXCL | O_NOFOLLOW` and mode `0600`.
    pub fn create_file(&self, ext: &str) -> std::io::Result<(std::fs::File, PathBuf)> {
        let name = self.random_name(ext);
        let file = self.dir.create_at(std::ffi::OsStr::new(&name))?;
        Ok((file, self.dir.path().join(&name)))
    }

    /// Reopen a file this publish created, relative to the retained
    /// descriptor, and verify it through the handle the way
    /// [`open_verified`] does for a file the reply named.
    ///
    /// This is the only way the upload phase reads: no path produced during
    /// resolution is ever resolved again, so neither the file nor the
    /// directory holding it can be swapped between staging and upload.
    pub fn open_file(
        &self,
        path: &Path,
        expected_len: Option<u64>,
    ) -> std::io::Result<(std::fs::File, u64)> {
        let name = self.dir.name_in(path)?;
        let file = self.dir.open_at(name)?;
        verified_handle(file, expected_len, false)
    }

    /// Remove a file this publish created, relative to the retained
    /// descriptor.
    pub fn remove_file(&self, path: &Path) -> std::io::Result<()> {
        let name = self.dir.name_in(path)?;
        self.dir.unlink_at(name)
    }

    /// A random, unused path in the directory for a subprocess (ffmpeg) to
    /// write to. The caller must check [`Self::still_at_its_path`] before
    /// handing the path over and read the result back through
    /// [`Self::open_file`].
    pub fn reserve(&self, ext: &str) -> PathBuf {
        self.dir.path().join(self.random_name(ext))
    }

    /// Confirm the scratch path still names the directory the descriptor
    /// holds, for the moment before a subprocess is given a path to write.
    ///
    /// A swap that has already happened is caught here; one that happens
    /// after this returns costs the publish (the output is read back through
    /// the descriptor and is not there) rather than moving it.
    pub fn still_at_its_path(&self) -> std::io::Result<()> {
        self.dir.still_at_its_path()
    }

    /// A random file name with a sanitised extension.
    fn random_name(&self, ext: &str) -> String {
        let ext = safe_attachment_filename(ext);
        let ext = if ext.is_empty() || ext == "attachment.bin" {
            "bin".to_string()
        } else {
            ext
        };
        format!("{}.{ext}", uuid::Uuid::new_v4().simple())
    }
}

/// Open an existing directory without following a symlink at its last
/// component, for `openat`-relative work against it.
#[cfg(unix)]
fn open_dir_no_follow(dir: &Path) -> std::io::Result<std::os::fd::OwnedFd> {
    let flags = nix::fcntl::OFlag::O_RDONLY
        | nix::fcntl::OFlag::O_DIRECTORY
        | nix::fcntl::OFlag::O_NOFOLLOW
        | nix::fcntl::OFlag::O_CLOEXEC;
    nix::fcntl::open(dir, flags, nix::sys::stat::Mode::empty()).map_err(std::io::Error::from)
}

/// How long an abandoned publish scratch directory can sit under the
/// outbound base before the next publish removes it. Comfortably past the
/// pool's whole reply-media budget, so a directory this old belongs to a
/// process that is gone (rule 4: the disk is a bounded resource, and a
/// harness that is killed mid-publish must not leave copies behind forever).
const ABANDONED_SCRATCH_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Remove publish scratch directories left behind by a process that died
/// before its `Drop` ran. Failures are only logged: this is housekeeping on
/// the way to doing the real work, never a reason to refuse a publish.
fn prune_abandoned_scratch(base: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path == keep {
            continue;
        }
        let stale = entry
            .metadata()
            .ok()
            .filter(|meta| meta.is_dir())
            .and_then(|meta| meta.modified().ok())
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > ABANDONED_SCRATCH_AGE);
        if stale {
            if let Err(e) = std::fs::remove_dir_all(&path) {
                tracing::debug!(
                    target: "acp::media",
                    "removing abandoned publish scratch {} failed: {e}",
                    path.display()
                );
            }
        }
    }
}

impl Drop for PublishScratch {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(self.dir.path()) {
            tracing::debug!(
                target: "acp::media",
                "removing publish scratch {} failed: {e}",
                self.dir.path().display()
            );
        }
    }
}

/// Open a file the reply named, for reading, without following a symlink at
/// its last component, and decide on the open handle whether it is eligible
/// to be published. Returns the handle and its length.
///
/// This is the last of the three containment primitives and the only one
/// that looks at the file itself rather than at a path. `confine` decides
/// where a name is allowed to resolve to; this decides whether the thing it
/// resolved to may leave the host, and it decides it on the handle the bytes
/// will actually be read from, so a path that resolved to a regular file
/// cannot be swapped for a link (or for different content) in between.
///
/// A file with more than one link is refused. Containment is decided with
/// `canonicalize`, which resolves symlinks and has nothing to resolve for a
/// hard link: a second name for an inode is its own canonical path, so
/// `ln ~/Documents/passport.pdf <turn dir>/note.pdf` puts a file that is
/// nowhere near a root under one, and every path-shaped guard agrees that it
/// belongs there. Both roots are engine-writable, so the engine can always
/// make that second name. `nlink == 1` is the property that says a file has
/// no other name, and it is only readable from the inode. The cost is that a
/// legitimately hard-linked file (a package store, a de-duplicated backup)
/// cannot be published from a root; it earns a note, and copying it into the
/// root makes it publishable.
pub fn open_verified(
    path: &Path,
    expected_len: Option<u64>,
) -> std::io::Result<(std::fs::File, u64)> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits());
    }
    let file = options.open(path)?;
    verified_handle(file, expected_len, true)
}

/// Check an open handle: a regular file, of exactly `expected_len` bytes
/// when one is given, and, when `single_link`, with no second name.
///
/// `single_link` is on for a file the reply named and off for a file this
/// publish created in its own scratch, whose provenance is not in question
/// and whose length is the only thing worth re-checking.
///
/// The messages name the condition and never the path. The two callers are
/// the outbound staging and upload paths, which prefix the reply's own name
/// for the file and post the result into the channel; the absolute path is
/// on the `tracing` line beside each call instead.
fn verified_handle(
    file: std::fs::File,
    expected_len: Option<u64>,
    single_link: bool,
) -> std::io::Result<(std::fs::File, u64)> {
    #[cfg(not(unix))]
    let _ = single_link;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::other("it is not a regular file"));
    }
    #[cfg(unix)]
    if single_link {
        use std::os::unix::fs::MetadataExt as _;
        if meta.nlink() > 1 {
            return Err(std::io::Error::other(format!(
                "it has {} names on this filesystem; a file with more than one name is not \
                 contained by the directory it was found in",
                meta.nlink()
            )));
        }
    }
    if let Some(expected) = expected_len.filter(|expected| *expected != meta.len()) {
        return Err(std::io::Error::other(format!(
            "it changed underneath us ({expected} bytes became {})",
            meta.len()
        )));
    }
    Ok((file, meta.len()))
}

/// Fetch every attachment on `events` into `turn_dir`.
///
/// `ffmpeg`, when present, is used to extract MP3 audio from voice-note MP4
/// envelopes so speech reaches the engine as audio rather than video.
/// `deadline` bounds the whole phase: an attachment whose turn comes after it
/// is reported as failed without a request, and a fetch in progress is cut
/// at it, so the pool slot is held for at most [`INBOUND_DEADLINE`].
///
/// `turn_dir` is resolved exactly once, here, and every blob is written
/// `openat`-relative to the descriptor that resolution produced. The bytes
/// are a remote sender's, the name is a remote sender's (`imeta filename`,
/// reduced to a basename and prefixed with the tag's index, so it is fully
/// predictable), and the directory is writable by the engine, which is the
/// exact shape [`DirHandle`] exists to refuse. A plain `fs::write` here is
/// an arbitrary host-file overwrite with sender-chosen bytes.
pub async fn collect_inbound_attachments(
    rest: &RestClient,
    events: &[&Event],
    turn_dir: &Path,
    ffmpeg: Option<&Path>,
    deadline: tokio::time::Instant,
) -> InboundAttachments {
    let mut inbound = InboundAttachments::default();
    let turn_dir = match DirHandle::open(turn_dir).map(Arc::new) {
        Ok(handle) => handle,
        Err(e) => {
            tracing::error!(
                target: "acp::media",
                dir = %turn_dir.display(),
                "this turn's attachment directory could not be opened: {e}"
            );
            return InboundAttachments::unavailable(
                events,
                &format!("this turn's attachment directory could not be opened: {e}"),
            );
        }
    };
    let Some(origin) = RelayOrigin::from_base_url(&rest.base_url) else {
        for event in events {
            if event
                .tags
                .iter()
                .any(|t| t.as_slice().first().map(String::as_str) == Some("imeta"))
            {
                inbound.outcomes.push(AttachmentOutcome::Rejected {
                    event_id: event.id.to_hex(),
                    reason: "relay base URL has no usable origin".into(),
                });
            }
        }
        return inbound;
    };
    let mut budget = AttachmentBudget::default();
    let mut seen: HashMap<String, String> = HashMap::new();
    let mut index = 0usize;
    for event in events {
        let event_id = event.id.to_hex();
        let parsed = parse_imeta_attachments(event, &origin, &mut budget);
        for reason in parsed.rejected {
            inbound.outcomes.push(AttachmentOutcome::Rejected {
                event_id: event_id.clone(),
                reason,
            });
        }
        for attachment in parsed.accepted {
            if let Some(first_event_id) = seen.get(&attachment.sha256) {
                inbound.outcomes.push(AttachmentOutcome::Duplicate {
                    event_id: event_id.clone(),
                    filename: attachment.filename,
                    first_event_id: first_event_id.clone(),
                });
                continue;
            }
            seen.insert(attachment.sha256.clone(), event_id.clone());
            index += 1;
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let fetched = if remaining.is_zero() {
                Err(format!(
                    "skipped: the {INBOUND_DEADLINE:?} attachment deadline for this turn passed"
                ))
            } else {
                tokio::time::timeout(
                    remaining,
                    fetch_attachment(rest, &origin, &attachment, &turn_dir, index, ffmpeg),
                )
                .await
                .unwrap_or_else(|_| {
                    Err(format!(
                        "cut off at the {INBOUND_DEADLINE:?} attachment deadline for this turn"
                    ))
                })
            };
            match fetched {
                Ok(local) => inbound.outcomes.push(AttachmentOutcome::Stored {
                    event_id: event_id.clone(),
                    local,
                }),
                Err(reason) => {
                    tracing::warn!(
                        target: "acp::media",
                        event = %event_id,
                        filename = %attachment.filename,
                        "attachment fetch failed: {reason}"
                    );
                    inbound.outcomes.push(AttachmentOutcome::Failed {
                        event_id: event_id.clone(),
                        filename: attachment.filename,
                        reason,
                    })
                }
            }
        }
    }
    inbound
}

/// Store one downloaded blob in the turn directory and return its path.
///
/// The create is `openat`-relative to the descriptor
/// [`collect_inbound_attachments`] took, with `O_CREAT | O_EXCL | O_NOFOLLOW`
/// and mode `0600`, so the write cannot follow a symlink planted at the name
/// and cannot silently replace a file that is already there. The reason it
/// returns names the condition and not the path.
async fn write_blob(dir: &Arc<DirHandle>, name: &str, data: Vec<u8>) -> Result<PathBuf, String> {
    let dir = Arc::clone(dir);
    let name = name.to_string();
    // Up to MAX_ATTACHMENT_BYTES of it; not work for a runtime thread.
    tokio::task::spawn_blocking(move || {
        use std::io::Write as _;
        let mut file = dir.create_at(std::ffi::OsStr::new(&name))?;
        file.write_all(&data)?;
        Ok::<PathBuf, std::io::Error>(dir.path().join(&name))
    })
    .await
    .map_err(|e| format!("could not be stored: {e}"))?
    .map_err(|e| format!("could not be stored: {e}"))
}

/// Reopen a file this turn wrote, through the descriptor, and report its
/// length. Used for the one output a subprocess produced, so its size is
/// read from the directory we hold rather than from the path ffmpeg was
/// given.
async fn read_back_size(dir: &Arc<DirHandle>, name: &str) -> Result<u64, String> {
    let dir = Arc::clone(dir);
    let name = name.to_string();
    tokio::task::spawn_blocking(move || {
        let file = dir.open_at(std::ffi::OsStr::new(&name))?;
        let (_, len) = verified_handle(file, None, false)?;
        Ok::<u64, std::io::Error>(len)
    })
    .await
    .map_err(|e| format!("extracted file unreadable: {e}"))?
    .map_err(|e| format!("extracted file unreadable: {e}"))
    .and_then(|size| {
        (size > 0)
            .then_some(size)
            .ok_or_else(|| "extracted file is empty".to_string())
    })
}

async fn fetch_attachment(
    rest: &RestClient,
    origin: &RelayOrigin,
    attachment: &ImetaAttachment,
    turn_dir: &Arc<DirHandle>,
    index: usize,
    ffmpeg: Option<&Path>,
) -> Result<LocalAttachment, String> {
    let url = origin.permits(&attachment.url).map_err(|e| e.to_string())?;
    let data = blossom::download_blob(rest, origin, &url, &attachment.sha256, attachment.size)
        .await
        .map_err(|e| match e {
            BlossomError::Refused { status, .. } => format!("relay returned HTTP {status}"),
            other => other.to_string(),
        })?;
    // Sanitised again here rather than only where the tag was parsed: this is
    // the line that turns a remote string into a name on the host's
    // filesystem, and it should not depend on a caller having cleaned it.
    let filename = safe_attachment_filename(&format!("{index}-{}", attachment.filename));
    let path = write_blob(turn_dir, &filename, data).await?;
    let mut local = LocalAttachment {
        path: path.clone(),
        filename,
        mime_type: attachment.mime_type.clone(),
        size: attachment.size,
        sha256: attachment.sha256.clone(),
        is_audio: attachment.is_audio(),
        is_voice_note: attachment.is_voice_note(),
        note: None,
    };
    let is_envelope = local.is_voice_note && attachment.mime_type.eq_ignore_ascii_case("video/mp4");
    if is_envelope {
        match ffmpeg {
            Some(ffmpeg) => {
                let mp3_name = safe_attachment_filename(&format!(
                    "{}.mp3",
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("voice-note")
                ));
                let mp3_path = turn_dir.path().join(&mp3_name);
                // ffmpeg is a subprocess: it is given a path and does its own
                // open, so the descriptor cannot cover it. Claim the name
                // through the descriptor first (`O_CREAT | O_EXCL |
                // O_NOFOLLOW`), so there is no symlink and no existing file
                // at the path ffmpeg is about to be handed, check the
                // directory is still the one we opened, and read the result
                // back through the descriptor. A swap after that costs the
                // extraction rather than moving it.
                let extracted = match turn_dir
                    .create_at(std::ffi::OsStr::new(&mp3_name))
                    .and_then(|_| turn_dir.still_at_its_path())
                {
                    Ok(()) => {
                        match crate::ffmpeg::extract_voice_note_audio(ffmpeg, &path, &mp3_path)
                            .await
                        {
                            Ok(()) => read_back_size(turn_dir, &mp3_name).await,
                            Err(e) => Err(e),
                        }
                    }
                    Err(e) => Err(format!("could not reserve a file for the extraction: {e}")),
                };
                match extracted {
                    Ok(size) => {
                        local.path = mp3_path;
                        local.filename = mp3_name;
                        local.mime_type = "audio/mpeg".into();
                        local.size = size;
                        local.note =
                            Some("audio extracted from the voice-note MP4 envelope".into());
                    }
                    Err(e) => {
                        local.note = Some(format!(
                            "voice note left in its MP4 envelope; audio extraction failed: {e}"
                        ));
                    }
                }
            }
            None => {
                local.note = Some(
                    "voice note in an MP4 envelope (audio track only; ffmpeg not available to extract it)"
                        .into(),
                );
            }
        }
    }
    Ok(local)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::{EventBuilder, JsonUtil as _, Keys, Kind, Tag};

    fn origin() -> RelayOrigin {
        RelayOrigin::from_base_url("https://relay.example").unwrap()
    }

    fn far_deadline() -> tokio::time::Instant {
        tokio::time::Instant::now() + std::time::Duration::from_secs(3600)
    }

    fn event_with_imeta(tags: &[Vec<&str>]) -> Event {
        let keys = Keys::generate();
        let tags: Vec<Tag> = tags
            .iter()
            .map(|t| Tag::parse(t.iter().copied()).unwrap())
            .collect();
        EventBuilder::new(Kind::Custom(9), "hello")
            .tags(tags)
            .sign_with_keys(&keys)
            .unwrap()
    }

    fn good_tag(sha: &str) -> Vec<String> {
        vec![
            "imeta".into(),
            format!("url https://relay.example/media/{sha}.mp3"),
            "m audio/mpeg".into(),
            format!("x {sha}"),
            "size 1234".into(),
            "duration 3.2".into(),
            "filename voice-note-1.mp3".into(),
        ]
    }

    fn refs(tag: &[String]) -> Vec<&str> {
        tag.iter().map(String::as_str).collect()
    }

    #[test]
    fn imeta_fields_first_value_wins_and_ignores_bare_entries() {
        let fields = imeta_fields(&[
            "imeta".into(),
            "url https://a".into(),
            "url https://b".into(),
            "bare".into(),
            "m  audio/mpeg ".into(),
        ]);
        assert_eq!(fields["url"], "https://a");
        assert_eq!(fields["m"], "audio/mpeg");
        assert!(!fields.contains_key("bare"));
    }

    #[test]
    fn accepts_a_well_formed_voice_note_tag() {
        let sha = "a".repeat(64);
        let event = event_with_imeta(&[refs(&good_tag(&sha))]);
        let parsed = parse_imeta_attachments(&event, &origin(), &mut AttachmentBudget::default());
        assert!(parsed.rejected.is_empty(), "{:?}", parsed.rejected);
        assert_eq!(parsed.accepted.len(), 1);
        let att = &parsed.accepted[0];
        assert_eq!(att.sha256, sha);
        assert_eq!(att.size, 1234);
        assert_eq!(att.filename, "voice-note-1.mp3");
        assert_eq!(att.mime_type, "audio/mpeg");
        assert!(att.is_voice_note());
        assert!(att.is_audio());
    }

    #[test]
    fn rejects_malformed_tags_with_reasons() {
        let sha = "a".repeat(64);
        let mut wrong_origin = good_tag(&sha);
        wrong_origin[1] = format!("url https://evil.example/media/{sha}.mp3");
        let mut http = good_tag(&sha);
        http[1] = format!("url http://relay.example/media/{sha}.mp3");
        let mut bad_hash = good_tag(&sha);
        bad_hash[3] = "x deadbeef".into();
        let mut zero_size = good_tag(&sha);
        zero_size[4] = "size 0".into();
        let mut huge = good_tag(&sha);
        huge[4] = format!("size {}", MAX_INBOUND_BYTES + 1);
        let no_url = vec!["imeta".to_string(), "m audio/mpeg".to_string()];
        let event = event_with_imeta(&[
            refs(&wrong_origin),
            refs(&http),
            refs(&bad_hash),
            refs(&zero_size),
            refs(&huge),
            refs(&no_url),
        ]);
        let parsed = parse_imeta_attachments(&event, &origin(), &mut AttachmentBudget::default());
        assert!(parsed.accepted.is_empty());
        assert_eq!(parsed.rejected.len(), 6);
        assert!(
            parsed.rejected[0].contains("not the relay origin"),
            "{}",
            parsed.rejected[0]
        );
        assert!(
            parsed.rejected[1].contains("not https"),
            "{}",
            parsed.rejected[1]
        );
        assert!(
            parsed.rejected[2].contains("64-hex"),
            "{}",
            parsed.rejected[2]
        );
        assert!(
            parsed.rejected[3].contains("size"),
            "{}",
            parsed.rejected[3]
        );
        assert!(parsed.rejected[4].contains("cap"), "{}", parsed.rejected[4]);
        assert!(
            parsed.rejected[5].contains("no url"),
            "{}",
            parsed.rejected[5]
        );
    }

    #[test]
    fn budget_caps_count_and_bytes_across_events() {
        let mut budget = AttachmentBudget::default();
        let tags: Vec<Vec<String>> = (0..MAX_INBOUND_ATTACHMENTS + 1)
            .map(|i| good_tag(&format!("{i:0>64}")))
            .collect();
        let tag_refs: Vec<Vec<&str>> = tags.iter().map(|t| refs(t)).collect();
        let event = event_with_imeta(&tag_refs);
        let parsed = parse_imeta_attachments(&event, &origin(), &mut budget);
        assert_eq!(parsed.accepted.len(), MAX_INBOUND_ATTACHMENTS);
        assert_eq!(parsed.rejected.len(), 1);
        assert!(parsed.rejected[0].contains("attachment limit"));

        let mut budget = AttachmentBudget::default();
        let big = MAX_INBOUND_BYTES / 2 + 1;
        let mut first = good_tag(&"1".repeat(64));
        first[4] = format!("size {big}");
        let mut second = good_tag(&"2".repeat(64));
        second[4] = format!("size {big}");
        let event = event_with_imeta(&[refs(&first), refs(&second)]);
        let parsed = parse_imeta_attachments(&event, &origin(), &mut budget);
        assert_eq!(parsed.accepted.len(), 1);
        assert!(
            parsed.rejected[0].contains("byte budget"),
            "{}",
            parsed.rejected[0]
        );
    }

    #[test]
    fn voice_note_classification_matches_clients() {
        assert!(is_voice_note("audio/mpeg", "voice-note-12.mp3"));
        assert!(is_voice_note("AUDIO/WAV", "Voice-Note-12.wav"));
        assert!(is_voice_note("video/mp4", "voice-note-12.mp4"));
        assert!(!is_voice_note("video/mp4", "voice-note-12.mov"));
        assert!(!is_voice_note("video/mp4", "clip.mp4"));
        assert!(!is_voice_note("audio/mpeg", "song.mp3"));
        let plain_audio = ImetaAttachment {
            url: String::new(),
            sha256: String::new(),
            size: 1,
            filename: "song.mp3".into(),
            mime_type: "audio/mpeg".into(),
        };
        assert!(plain_audio.is_audio());
        assert!(!plain_audio.is_voice_note());
    }

    #[test]
    fn filenames_are_sanitised() {
        assert_eq!(safe_attachment_filename("../../etc/passwd"), "passwd");
        assert_eq!(safe_attachment_filename("C:\\x\\note.mp3"), "note.mp3");
        assert_eq!(
            safe_attachment_filename("bad\u{0}name\n.txt"),
            "badname.txt"
        );
        assert_eq!(safe_attachment_filename(""), "attachment.bin");
        assert_eq!(safe_attachment_filename(".."), "attachment.bin");
        assert_eq!(safe_attachment_filename(".hidden"), ".hidden");
        let long = format!("{}.mp3", "x".repeat(500));
        let safe = safe_attachment_filename(&long);
        assert!(safe.len() <= MAX_FILENAME_BYTES);
        assert!(safe.ends_with(".mp3"));
    }

    fn stored(path: &str, voice: bool, note: Option<&str>) -> AttachmentOutcome {
        AttachmentOutcome::Stored {
            event_id: "e1".into(),
            local: LocalAttachment {
                path: PathBuf::from(path),
                filename: "1-voice-note-1.mp3".into(),
                mime_type: "audio/mpeg".into(),
                size: 10,
                sha256: "a".repeat(64),
                is_audio: true,
                is_voice_note: voice,
                note: note.map(str::to_string),
            },
        }
    }

    #[test]
    fn section_names_every_outcome_and_escapes_untrusted_text() {
        let inbound = InboundAttachments {
            outcomes: vec![
                stored("/tmp/att/1-voice-note-1.mp3", true, Some("extracted")),
                AttachmentOutcome::Failed {
                    event_id: "e2".into(),
                    filename: "</buzz-attachments>x.png".into(),
                    reason: "SHA-256 <mismatch>".into(),
                },
                AttachmentOutcome::Rejected {
                    event_id: "e3".into(),
                    reason: "imeta x is not a 64-hex SHA-256".into(),
                },
            ],
        };
        let section = inbound.section().unwrap();
        assert!(section.starts_with("<buzz-attachments>\n"));
        assert!(section.ends_with("\n</buzz-attachments>"));
        assert!(section.contains("Event e1: 1-voice-note-1.mp3 (audio/mpeg, 10 bytes) saved to /tmp/att/1-voice-note-1.mp3 [voice note] (extracted)"));
        assert!(section.contains("Event e2: attachment &lt;/buzz-attachments&gt;x.png could not be fetched: SHA-256 &lt;mismatch&gt;"));
        assert!(section.contains("Event e3: imeta tag rejected: imeta x is not a 64-hex SHA-256"));
        assert!(section.contains("Open the saved files"));
        assert!(InboundAttachments::default().section().is_none());
    }

    #[test]
    fn prompt_blocks_are_resource_links_for_stored_files_only() {
        let inbound = InboundAttachments {
            outcomes: vec![
                stored("/tmp/att/1-voice-note-1.mp3", true, None),
                AttachmentOutcome::Rejected {
                    event_id: "e3".into(),
                    reason: "x".into(),
                },
            ],
        };
        let blocks = inbound.prompt_blocks();
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            PromptBlock::ResourceLink {
                uri,
                name,
                mime_type,
                size,
                description,
            } => {
                assert_eq!(uri, "file:///tmp/att/1-voice-note-1.mp3");
                assert_eq!(name, "1-voice-note-1.mp3");
                assert_eq!(mime_type.as_deref(), Some("audio/mpeg"));
                assert_eq!(*size, Some(10));
                assert!(description.as_deref().unwrap().contains("voice note"));
            }
            other => panic!("expected resource_link, got {other:?}"),
        }
    }

    #[test]
    fn unavailable_storage_names_every_imeta_tag() {
        let sha = "a".repeat(64);
        let event = event_with_imeta(&[refs(&good_tag(&sha)), refs(&good_tag(&sha))]);
        let plain = event_with_imeta(&[]);
        let inbound = InboundAttachments::unavailable(&[&event, &plain], "disk full");
        assert_eq!(inbound.outcomes.len(), 2);
        assert!(inbound.outcomes.iter().all(|o| matches!(
            o,
            AttachmentOutcome::Rejected { reason, .. } if reason == "disk full"
        )));
        assert!(inbound.section().unwrap().contains("disk full"));
        assert!(inbound.prompt_blocks().is_empty());
    }

    #[test]
    fn prepare_turn_dir_prunes_oldest_siblings() {
        let root = std::env::temp_dir().join(format!("buzz-acp-att-{}", uuid::Uuid::new_v4()));
        for i in 0..(KEEP_TURN_DIRS + 3) {
            let dir =
                prepare_turn_dir(&root, &format!("turn-{i:03}"), &LiveTurnDirs::default()).unwrap();
            assert!(dir.is_dir());
            // Distinct mtimes so the prune order is deterministic.
            let t = filetime_now_plus(i as u64);
            let _ = std::fs::File::open(&dir).and_then(|f| f.set_modified(t));
        }
        let remaining: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(remaining.len(), KEEP_TURN_DIRS);
        assert!(!remaining.contains(&"turn-000".to_string()));
        assert!(remaining.contains(&format!("turn-{:03}", KEEP_TURN_DIRS + 2)));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Review item 3: a turn whose engine is still reading its attachments
    /// must not have its directory removed by later turns completing.
    #[test]
    fn prune_skips_live_turn_dirs_until_their_guard_drops() {
        let root = std::env::temp_dir().join(format!("buzz-acp-att-{}", uuid::Uuid::new_v4()));
        let live = LiveTurnDirs::default();
        let guard = live.hold("turn-000");
        assert_eq!(live.len(), 1);
        for i in 0..(KEEP_TURN_DIRS + 3) {
            let dir = prepare_turn_dir(&root, &format!("turn-{i:03}"), &live).unwrap();
            let t = filetime_now_plus(i as u64);
            let _ = std::fs::File::open(&dir).and_then(|f| f.set_modified(t));
        }
        let names = |root: &Path| -> Vec<String> {
            std::fs::read_dir(root)
                .unwrap()
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        };
        let remaining = names(&root);
        assert_eq!(remaining.len(), KEEP_TURN_DIRS, "{remaining:?}");
        assert!(
            remaining.contains(&"turn-000".to_string()),
            "the oldest directory is live and survives: {remaining:?}"
        );
        assert!(
            !remaining.contains(&"turn-001".to_string())
                && !remaining.contains(&"turn-002".to_string()),
            "the prune takes the next-oldest instead: {remaining:?}"
        );
        drop(guard);
        assert!(live.is_empty());
        prune_turn_dirs(&root, &live, None).unwrap();
        assert!(
            names(&root).contains(&"turn-000".to_string()),
            "no excess, nothing more pruned"
        );
        prepare_turn_dir(&root, "turn-999", &live).unwrap();
        assert!(
            !names(&root).contains(&"turn-000".to_string()),
            "once released, the old directory goes at the next prune"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn attachment_root_is_private_and_refuses_symlinks() {
        use std::os::unix::fs::PermissionsExt as _;
        let base = std::env::temp_dir().join(format!("buzz-acp-root-{}", uuid::Uuid::new_v4()));
        let root = prepare_attachment_root(&base, "abcdef0123456789").unwrap();
        assert_eq!(root, base.join("buzz-acp/abcdef0123456789/attachments"));
        for dir in [root.as_path(), root.parent().unwrap()] {
            let mode = std::fs::metadata(dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{} is {mode:o}", dir.display());
        }
        // A pre-existing root that is too open is tightened, not refused.
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        prepare_attachment_root(&base, "abcdef0123456789").unwrap();
        let mode = std::fs::metadata(&root).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);

        // A symlink planted where the root should be is an error.
        let planted =
            std::env::temp_dir().join(format!("buzz-acp-planted-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&planted).unwrap();
        std::fs::create_dir_all(base.join("buzz-acp/victim")).unwrap();
        std::os::unix::fs::symlink(&planted, base.join("buzz-acp/victim/attachments")).unwrap();
        let err = prepare_attachment_root(&base, "victim").unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");
        // So is a regular file.
        std::fs::create_dir_all(base.join("buzz-acp/plain")).unwrap();
        std::fs::write(base.join("buzz-acp/plain/attachments"), b"").unwrap();
        assert!(prepare_attachment_root(&base, "plain").is_err());
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&planted);
    }

    /// S6 from the confirmation pass: on the shared `/tmp` fallback the
    /// `<base>/buzz-acp` component is the one another local user can create
    /// first, so it is checked like the two below it.
    #[test]
    fn the_shared_component_of_the_root_is_checked_too() {
        let base = std::env::temp_dir().join(format!("buzz-acp-shared-{}", uuid::Uuid::new_v4()));
        let planted =
            std::env::temp_dir().join(format!("buzz-acp-shared-planted-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&planted).unwrap();
        std::fs::create_dir_all(&base).unwrap();
        std::os::unix::fs::symlink(&planted, base.join("buzz-acp")).unwrap();
        let err = prepare_attachment_root(&base, "abcdef0123456789")
            .expect_err("a planted `buzz-acp` component is refused");
        assert!(err.to_string().contains("symlink"), "{err}");
        assert!(
            err.to_string().contains("buzz-acp"),
            "the refusal names the component: {err}"
        );
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&planted);
    }

    /// The publish scratch is a fresh directory per publish, private, at a
    /// name nothing else knows, outside every directory the engine was
    /// handed, and it takes its files with it when it drops.
    fn set_dir_mtime(dir: &Path, when: std::time::SystemTime) {
        let spec = |t: std::time::SystemTime| {
            let d = t.duration_since(std::time::UNIX_EPOCH).unwrap();
            nix::sys::time::TimeSpec::new(d.as_secs() as i64, d.subsec_nanos() as i64)
        };
        nix::sys::stat::utimensat(
            nix::fcntl::AT_FDCWD,
            dir,
            &spec(when),
            &spec(when),
            nix::sys::stat::UtimensatFlags::NoFollowSymlink,
        )
        .unwrap();
    }

    #[test]
    fn publish_scratch_is_private_fresh_and_removed_with_its_publish() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = std::env::temp_dir().join(format!("buzz-acp-scratch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let dir = {
            let scratch = PublishScratch::create(&root).unwrap();
            let mode = std::fs::metadata(scratch.dir())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700, "{mode:o}");
            let second = PublishScratch::create(&root).unwrap();
            assert_ne!(
                scratch.dir(),
                second.dir(),
                "each publish gets its own directory"
            );

            let (_, first) = scratch.create_file("txt").unwrap();
            let (_, again) = scratch.create_file("txt").unwrap();
            assert_ne!(first, again, "names are not predictable");
            assert!(first.starts_with(scratch.dir()));
            let mode = std::fs::metadata(&first).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{mode:o}");

            // A link planted at a scratch name is never written through:
            // `create_at` is the open every scratch file is created with,
            // and the name is the only thing the test chooses.
            let victim = root.join("victim.txt");
            std::fs::write(&victim, b"victim").unwrap();
            std::os::unix::fs::symlink(&victim, scratch.dir().join("planted.txt")).unwrap();
            let err = scratch
                .dir
                .create_at(std::ffi::OsStr::new("planted.txt"))
                .expect_err("a planted symlink is not written through");
            assert!(
                err.kind() == std::io::ErrorKind::AlreadyExists
                    || err.raw_os_error() == Some(nix::libc::ELOOP),
                "{err:?}"
            );
            assert_eq!(std::fs::read(&victim).unwrap(), b"victim");
            // And so is a plain existing file.
            std::fs::write(scratch.dir().join("taken.txt"), b"first").unwrap();
            assert!(scratch
                .dir
                .create_at(std::ffi::OsStr::new("taken.txt"))
                .is_err());
            scratch.dir().to_path_buf()
        };
        assert!(!dir.exists(), "the scratch goes when the publish does");

        // A directory left behind by a process that died mid-publish is
        // reclaimed by the next one; a fresh one is not.
        let abandoned = root.join(OUTBOUND_DIR).join("pub-abandoned");
        std::fs::create_dir_all(&abandoned).unwrap();
        let long_ago = std::time::SystemTime::now()
            - (ABANDONED_SCRATCH_AGE + std::time::Duration::from_secs(60));
        set_dir_mtime(&abandoned, long_ago);
        let recent = root.join(OUTBOUND_DIR).join("pub-recent");
        std::fs::create_dir_all(&recent).unwrap();
        let live = PublishScratch::create(&root).unwrap();
        assert!(!abandoned.exists(), "the abandoned scratch was reclaimed");
        assert!(recent.is_dir(), "a fresh one is left alone");
        assert!(live.dir().is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// B1b from the third confirmation pass: the scratch is not hidden from
    /// the engine (same uid, a constant `.outbound` one level above the
    /// turn directory it was handed), so a same-uid process can list the
    /// base, learn the name, and swap the directory for a symlink. Holding
    /// the directory open and working `openat`-relative to it is what makes
    /// that swap inert: the descriptor still names the real directory, the
    /// creates and the reads follow it, and the attacker's directory stays
    /// empty. Replacing `create_at`/`open_at` with a path-based open puts
    /// the file in `victim` instead and fails this test.
    #[test]
    fn a_swapped_scratch_directory_redirects_nothing() {
        let root = std::env::temp_dir().join(format!("buzz-acp-swap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let scratch = PublishScratch::create(&root).unwrap();
        let victim = root.join("victim");
        std::fs::create_dir_all(&victim).unwrap();

        // The swap, exactly as the probe did it: the real directory is moved
        // aside and a symlink to the attacker's directory takes its name.
        let named = scratch.dir().to_path_buf();
        let real = root.join(OUTBOUND_DIR).join("moved");
        std::fs::rename(&named, &real).unwrap();
        std::os::unix::fs::symlink(&victim, &named).unwrap();

        let (mut file, path) = scratch
            .create_file("txt")
            .expect("the descriptor still names a usable directory");
        std::io::Write::write_all(&mut file, b"staged bytes").unwrap();
        drop(file);
        let name = path.file_name().unwrap();
        assert!(
            std::fs::read_dir(&victim).unwrap().next().is_none(),
            "the write did not follow the swapped directory"
        );
        assert_eq!(
            std::fs::read(real.join(name)).unwrap(),
            b"staged bytes",
            "it landed in the directory the descriptor holds"
        );
        // And the read side comes back through the same descriptor.
        let (mut reopened, len) = scratch.open_file(&path, Some(12)).unwrap();
        let mut back = Vec::new();
        std::io::Read::read_to_end(&mut reopened, &mut back).unwrap();
        assert_eq!((len, back.as_slice()), (12, b"staged bytes".as_slice()));
        // The one operation that cannot go through the descriptor is a
        // subprocess opening a path, and that is refused once swapped.
        let err = scratch
            .still_at_its_path()
            .expect_err("the path no longer names the publish directory");
        assert!(err.to_string().contains("no longer names"), "{err}");

        std::fs::remove_file(&named).unwrap();
        drop(scratch);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn filenames_lose_format_characters_too() {
        assert_eq!(
            safe_attachment_filename("in\u{202E}fdp.exe"),
            "infdp.exe",
            "a right-to-left override cannot disguise the extension"
        );
        assert_eq!(safe_attachment_filename("a\u{200B}b\u{FEFF}.txt"), "ab.txt");
        assert_eq!(
            safe_attachment_filename("caf\u{E9}.txt"),
            "caf\u{E9}.txt",
            "letters stay"
        );
    }

    #[tokio::test]
    async fn inbound_deadline_fails_attachments_it_cannot_reach_in_time() {
        let rest = rest_for("http://127.0.0.1:1");
        let event = imeta_event_for(
            "http://127.0.0.1:1",
            &"a".repeat(64),
            4,
            "audio/mpeg",
            "one.mp3",
        );
        let root = std::env::temp_dir().join(format!("buzz-acp-dl-{}", uuid::Uuid::new_v4()));
        let turn_dir = prepare_turn_dir(&root, "turn-1", &LiveTurnDirs::default()).unwrap();
        let passed = tokio::time::Instant::now() - std::time::Duration::from_secs(1);
        let inbound = collect_inbound_attachments(&rest, &[&event], &turn_dir, None, passed).await;
        assert_eq!(inbound.outcomes.len(), 1);
        match &inbound.outcomes[0] {
            AttachmentOutcome::Failed { reason, .. } => {
                assert!(reason.contains("deadline"), "{reason}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(inbound.section().unwrap().contains("deadline"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn duplicate_blob_on_a_second_event_is_named_not_refetched() {
        let body = b"same blob".to_vec();
        let sha = blossom::sha256_hex(&body);
        let (base, _head_rx) = one_shot_server("200 OK", body.clone(), Some(body.len())).await;
        let rest = rest_for(&base);
        let first = imeta_event_for(&base, &sha, body.len(), "audio/mpeg", "voice-note-1.mp3");
        let second = imeta_event_for(&base, &sha, body.len(), "audio/mpeg", "voice-note-2.mp3");
        let root = std::env::temp_dir().join(format!("buzz-acp-dl-{}", uuid::Uuid::new_v4()));
        let turn_dir = prepare_turn_dir(&root, "turn-1", &LiveTurnDirs::default()).unwrap();
        let inbound =
            collect_inbound_attachments(&rest, &[&first, &second], &turn_dir, None, far_deadline())
                .await;
        assert_eq!(inbound.outcomes.len(), 2, "{:?}", inbound.outcomes);
        assert!(matches!(
            inbound.outcomes[0],
            AttachmentOutcome::Stored { .. }
        ));
        match &inbound.outcomes[1] {
            AttachmentOutcome::Duplicate {
                first_event_id,
                filename,
                ..
            } => {
                assert_eq!(first_event_id, &first.id.to_hex());
                assert_eq!(filename, "voice-note-2.mp3");
            }
            other => panic!("expected Duplicate, got {other:?}"),
        }
        assert!(inbound
            .section()
            .unwrap()
            .contains("same blob as the one on event"));
        assert_eq!(inbound.prompt_blocks().len(), 1, "one file, one link");
        let _ = std::fs::remove_dir_all(&root);
    }

    fn filetime_now_plus(secs: u64) -> std::time::SystemTime {
        std::time::SystemTime::now() + std::time::Duration::from_secs(secs)
    }

    /// Minimal HTTP/1.1 server that serves one canned response, capturing the
    /// request head so the test can assert on the Blossom headers.
    async fn one_shot_server(
        status: &'static str,
        body: Vec<u8>,
        content_length: Option<usize>,
    ) -> (String, tokio::sync::oneshot::Receiver<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let mut head = Vec::new();
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                head.extend_from_slice(&buf[..n]);
                if head.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&head).into_owned());
            let mut resp = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
            if let Some(len) = content_length {
                resp.push_str(&format!("Content-Length: {len}\r\n"));
            }
            resp.push_str("\r\n");
            socket.write_all(resp.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
            socket.shutdown().await.ok();
        });
        (format!("http://{addr}"), rx)
    }

    fn rest_for(base: &str) -> RestClient {
        RestClient {
            http: reqwest::Client::new(),
            base_url: base.to_string(),
            keys: Keys::generate(),
            auth_tag_json: Some("[\"auth\",\"tag\"]".into()),
        }
    }

    fn imeta_event_for(base: &str, sha: &str, size: usize, mime: &str, name: &str) -> Event {
        let tag = vec![
            "imeta".to_string(),
            format!("url {base}/media/{sha}.bin"),
            format!("m {mime}"),
            format!("x {sha}"),
            format!("size {size}"),
            format!("filename {name}"),
        ];
        event_with_imeta(&[refs(&tag)])
    }

    #[tokio::test]
    async fn download_sends_get_auth_and_stores_verified_blob() {
        let body = b"hello voice".to_vec();
        let sha = blossom::sha256_hex(&body);
        let (base, head_rx) = one_shot_server("200 OK", body.clone(), Some(body.len())).await;
        let rest = rest_for(&base);
        let event = imeta_event_for(&base, &sha, body.len(), "audio/mpeg", "voice-note-9.mp3");
        let root = std::env::temp_dir().join(format!("buzz-acp-dl-{}", uuid::Uuid::new_v4()));
        let turn_dir = prepare_turn_dir(&root, "turn-1", &LiveTurnDirs::default()).unwrap();

        let inbound =
            collect_inbound_attachments(&rest, &[&event], &turn_dir, None, far_deadline()).await;

        let head = head_rx.await.unwrap();
        assert!(
            head.starts_with(&format!("GET /media/{sha}.bin HTTP/1.1")),
            "{head}"
        );
        let auth_line = head
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("authorization: nostr "))
            .expect("Blossom Authorization header");
        let token = auth_line.split_whitespace().nth(2).unwrap();
        let json = base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, token)
            .unwrap();
        let auth_event = Event::from_json(&json).unwrap();
        assert_eq!(auth_event.kind.as_u16(), 24242);
        assert_eq!(auth_event.pubkey, rest.keys.public_key());
        assert!(auth_event.verify().is_ok());
        let tags: Vec<Vec<String>> = auth_event
            .tags
            .iter()
            .map(|t| t.as_slice().to_vec())
            .collect();
        assert!(tags.contains(&vec!["t".to_string(), "get".to_string()]));
        assert!(tags.contains(&vec!["x".to_string(), sha.clone()]));
        assert!(
            head.to_ascii_lowercase()
                .contains("x-auth-tag: [\"auth\",\"tag\"]"),
            "{head}"
        );

        assert_eq!(inbound.outcomes.len(), 1, "{:?}", inbound.outcomes);
        let AttachmentOutcome::Stored { local, .. } = &inbound.outcomes[0] else {
            panic!("expected Stored, got {:?}", inbound.outcomes[0]);
        };
        assert_eq!(std::fs::read(&local.path).unwrap(), body);
        assert_eq!(local.filename, "1-voice-note-9.mp3");
        assert!(local.is_voice_note);
        assert_eq!(local.size, body.len() as u64);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// B2 from the third confirmation pass, and the one finding in that pass
    /// that is a boundary crossing rather than defence in depth: the bytes
    /// and the name both come from a remote sender, so the harness must
    /// never write them through a link.
    ///
    /// The name is fully predictable (`<index>-<the sender's own imeta
    /// filename>`) and the turn directory is writable by the engine, so a
    /// link planted at that name turns the download into an arbitrary
    /// host-file overwrite with sender-chosen, hash-verified bytes. The write
    /// goes through [`DirHandle::create_at`] (`O_CREAT | O_EXCL |
    /// O_NOFOLLOW`, relative to the descriptor taken when the turn directory
    /// was opened), so the link is refused rather than followed.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_link_planted_at_an_inbound_name_is_never_written_through() {
        let body = b"attacker-controlled blob".to_vec();
        let sha = blossom::sha256_hex(&body);
        let (base, _head) = one_shot_server("200 OK", body.clone(), Some(body.len())).await;
        let rest = rest_for(&base);
        // The sender chooses this name, so it knows exactly what the harness
        // will call the file.
        let event = imeta_event_for(&base, &sha, body.len(), "audio/mpeg", "voice-note-9.mp3");
        let root = std::env::temp_dir().join(format!("buzz-acp-dl-{}", uuid::Uuid::new_v4()));
        let turn_dir = prepare_turn_dir(&root, "turn-1", &LiveTurnDirs::default()).unwrap();
        let victim = std::env::temp_dir().join(format!("buzz-acp-victim-{}", uuid::Uuid::new_v4()));
        std::fs::write(&victim, b"victim").unwrap();
        std::os::unix::fs::symlink(&victim, turn_dir.join("1-voice-note-9.mp3")).unwrap();

        let inbound =
            collect_inbound_attachments(&rest, &[&event], &turn_dir, None, far_deadline()).await;

        match &inbound.outcomes[0] {
            AttachmentOutcome::Failed { reason, .. } => {
                // The reason names the condition, never the host path.
                assert!(
                    !reason.contains(&victim.display().to_string())
                        && !reason.contains(&turn_dir.display().to_string()),
                    "{reason}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"victim",
            "the file the link pointed at was not overwritten"
        );
        assert!(
            !inbound
                .stored()
                .any(|local| local.filename == "1-voice-note-9.mp3"),
            "nothing was reported as stored"
        );
        let _ = std::fs::remove_file(&victim);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// ffmpeg is the one inbound writer the descriptor cannot cover: it is
    /// handed a path and does its own open. The name it is given is derived
    /// from the sender's filename, so it is as predictable as the download's,
    /// and the harness therefore claims it through the descriptor first
    /// (`O_CREAT | O_EXCL | O_NOFOLLOW`). A link already at that name costs
    /// the extraction; the note says so and the file the link pointed at is
    /// untouched.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_link_planted_at_the_extraction_output_is_never_written_through() {
        let body = b"fake mp4 envelope".to_vec();
        let sha = blossom::sha256_hex(&body);
        let (base, _head) = one_shot_server("200 OK", body.clone(), Some(body.len())).await;
        let rest = rest_for(&base);
        let event = imeta_event_for(&base, &sha, body.len(), "video/mp4", "voice-note-1.mp4");
        let root = std::env::temp_dir().join(format!("buzz-acp-dl-{}", uuid::Uuid::new_v4()));
        let turn_dir = prepare_turn_dir(&root, "turn-1", &LiveTurnDirs::default()).unwrap();
        let victim = std::env::temp_dir().join(format!("buzz-acp-victim-{}", uuid::Uuid::new_v4()));
        std::fs::write(&victim, b"victim").unwrap();
        // The name ffmpeg would be given: `<index>-<sender's stem>.mp3`.
        std::os::unix::fs::symlink(&victim, turn_dir.join("1-voice-note-1.mp3")).unwrap();
        // A stand-in for ffmpeg so the test does not need a real one: it
        // writes to whatever path it is given last, which is exactly the
        // behaviour the guard has to survive.
        let fake = root.join("fake-ffmpeg");
        std::fs::write(
            &fake,
            "#!/bin/sh\nfor a in \"$@\"; do last=\"$a\"; done\nprintf 'ID3extracted' > \"$last\"\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let inbound =
            collect_inbound_attachments(&rest, &[&event], &turn_dir, Some(&fake), far_deadline())
                .await;

        let AttachmentOutcome::Stored { local, .. } = &inbound.outcomes[0] else {
            panic!("expected the envelope itself to be stored: {inbound:?}");
        };
        assert_eq!(local.filename, "1-voice-note-1.mp4");
        assert_eq!(local.mime_type, "video/mp4");
        let note = local.note.as_deref().unwrap_or_default();
        assert!(note.contains("extraction failed"), "{note}");
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"victim",
            "the file the link pointed at was not overwritten"
        );
        let _ = std::fs::remove_file(&victim);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The inbound half resolves the turn directory once, with
    /// `O_DIRECTORY | O_NOFOLLOW`, so a turn directory that is a link stores
    /// nothing rather than storing a remote sender's bytes wherever the link
    /// points. `prepare_turn_dir` cannot do this for us: `create_dir_all`
    /// walks happily through a link and only the root is checked afterwards.
    ///
    /// This is deliberately asymmetric with the outbound half, which still
    /// treats a link-replaced turn directory as a root (B1 of the third
    /// confirmation pass, accepted): out there the only party who can plant
    /// the link is the engine, which has a shell at our uid and does not need
    /// it. In here the bytes and the name belong to a remote member, so the
    /// write has to land where we meant it to.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_inbound_turn_directory_that_is_a_link_stores_nothing() {
        let body = b"attacker-controlled blob".to_vec();
        let sha = blossom::sha256_hex(&body);
        let (base, _head) = one_shot_server("200 OK", body.clone(), Some(body.len())).await;
        let rest = rest_for(&base);
        let event = imeta_event_for(&base, &sha, body.len(), "audio/mpeg", "voice-note-9.mp3");
        let root = std::env::temp_dir().join(format!("buzz-acp-dl-{}", uuid::Uuid::new_v4()));
        let elsewhere =
            std::env::temp_dir().join(format!("buzz-acp-elsewhere-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        let turn_dir = root.join("turn-1");
        std::os::unix::fs::symlink(&elsewhere, &turn_dir).unwrap();
        // The name resolves to a real directory, so every path-shaped check
        // agrees with it; `prepare_turn_dir` would too.
        assert!(turn_dir.is_dir());

        let inbound =
            collect_inbound_attachments(&rest, &[&event], &turn_dir, None, far_deadline()).await;

        match &inbound.outcomes[0] {
            AttachmentOutcome::Rejected { reason, .. } => {
                assert!(reason.contains("could not be opened"), "{reason}")
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
        assert!(
            std::fs::read_dir(&elsewhere).unwrap().next().is_none(),
            "nothing was written through the link"
        );
        let _ = std::fs::remove_dir_all(&elsewhere);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn download_rejects_hash_mismatch_and_reports_it() {
        let body = b"tampered".to_vec();
        let claimed = "f".repeat(64);
        let (base, _head) = one_shot_server("200 OK", body.clone(), Some(body.len())).await;
        let rest = rest_for(&base);
        let event = imeta_event_for(&base, &claimed, body.len(), "image/png", "pic.png");
        let root = std::env::temp_dir().join(format!("buzz-acp-dl-{}", uuid::Uuid::new_v4()));
        let turn_dir = prepare_turn_dir(&root, "turn-1", &LiveTurnDirs::default()).unwrap();

        let inbound =
            collect_inbound_attachments(&rest, &[&event], &turn_dir, None, far_deadline()).await;

        match &inbound.outcomes[0] {
            AttachmentOutcome::Failed {
                reason, filename, ..
            } => {
                assert!(reason.contains("SHA-256"), "{reason}");
                assert_eq!(filename, "pic.png");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(
            std::fs::read_dir(&turn_dir).unwrap().next().is_none(),
            "nothing stored"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn download_rejects_content_length_mismatch_before_reading_body() {
        let body = b"0123456789".to_vec();
        let sha = blossom::sha256_hex(&body);
        // Server declares 10 bytes; imeta claims 5.
        let (base, _head) = one_shot_server("200 OK", body.clone(), Some(body.len())).await;
        let rest = rest_for(&base);
        let event = imeta_event_for(&base, &sha, 5, "text/plain", "a.txt");
        let root = std::env::temp_dir().join(format!("buzz-acp-dl-{}", uuid::Uuid::new_v4()));
        let turn_dir = prepare_turn_dir(&root, "turn-1", &LiveTurnDirs::default()).unwrap();

        let inbound =
            collect_inbound_attachments(&rest, &[&event], &turn_dir, None, far_deadline()).await;

        match &inbound.outcomes[0] {
            AttachmentOutcome::Failed { reason, .. } => {
                assert!(reason.contains("Content-Length"), "{reason}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn download_reports_relay_refusal_status() {
        let body = b"denied".to_vec();
        let sha = "e".repeat(64);
        let (base, _head) =
            one_shot_server("401 Unauthorized", body.clone(), Some(body.len())).await;
        let rest = rest_for(&base);
        let event = imeta_event_for(&base, &sha, 6, "audio/mpeg", "voice-note-1.mp3");
        let root = std::env::temp_dir().join(format!("buzz-acp-dl-{}", uuid::Uuid::new_v4()));
        let turn_dir = prepare_turn_dir(&root, "turn-1", &LiveTurnDirs::default()).unwrap();

        let inbound =
            collect_inbound_attachments(&rest, &[&event], &turn_dir, None, far_deadline()).await;

        match &inbound.outcomes[0] {
            AttachmentOutcome::Failed { reason, .. } => {
                assert!(reason.contains("HTTP 401"), "{reason}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
