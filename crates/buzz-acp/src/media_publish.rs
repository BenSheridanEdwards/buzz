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
//!
//! # The containment model
//!
//! A reply names a path; it does not get to read one. Two directories are
//! outbound roots: this turn's own directory under the attachment root,
//! always, and the harness working directory, only when
//! [`outbound_workspace`] finds it to be a boundary (`HOME` known and neither
//! `HOME` nor an ancestor of it, outside the attachment root and not
//! containing it, not the filesystem root, and not on or under a system
//! directory). Otherwise the turn directory is the only root and the reply
//! notes say so. A file is eligible when the reply names it absolutely,
//! without `..`, it canonicalises to a path under a root, and the handle
//! opened `O_NOFOLLOW` at that path reports a regular file, of the length
//! recorded a moment earlier, with exactly one link. Three primitives
//! enforce that and nothing else is trusted to:
//! [`OutboundRoots::confine`] resolves the name and tests it against the
//! roots, [`crate::attachments::open_verified`] decides eligibility on the
//! open handle (`nlink == 1` is what a hard link into a root cannot fake),
//! and [`crate::attachments::PublishScratch`] holds an open descriptor on a
//! private directory into which every accepted file is copied through that
//! handle, so from staging onwards the upload reads only harness-owned
//! snapshots, `openat`-relative, and resolves no path a second time.
//!
//! Both roots are writable by the engine, and an accepted workspace is a
//! read primitive over its whole tree for anyone who can talk to the agent
//! (see `docs/remote-agents.md`); that is the trade the workspace rule
//! exists to bound.
//!
//! Every refusal reason that can reach [`failure_notice`] is a *reason*, not
//! a path: the notice is posted into the channel, and the canonical path a
//! name resolved to is the host's `HOME`, attachment root or scratch. The
//! caller prefixes the reply's own basename for the file; the absolute path
//! goes to `tracing` instead.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
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
/// Most inline base64 kept from one reply, summed over every block (about
/// 25 MiB decoded). A block that would push the turn over this is dropped.
pub(crate) const MAX_INLINE_BASE64_BUDGET: usize = 34 * 1024 * 1024;
/// Wall-clock cap for submitting the kind-9.
pub(crate) const PUBLISH_TIMEOUT: Duration = Duration::from_secs(20);
/// Wall-clock cap for one reply's uploads, end to end; the kind-9 for
/// whatever finished in time is still posted.
pub(crate) const OUTBOUND_DEADLINE: Duration = Duration::from_secs(180);
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
    /// Base64 bytes held by `blocks`, charged against
    /// [`MAX_INLINE_BASE64_BUDGET`].
    inline_bytes: usize,
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
                let over_budget = inline_len > MAX_INLINE_BASE64_BUDGET - self.inline_bytes;
                if self.blocks.len() >= MAX_CAPTURED_BLOCKS || over_budget {
                    self.dropped_blocks += 1;
                } else {
                    self.inline_bytes += inline_len;
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
    #[cfg(test)]
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

    /// Base64 bytes currently held by the captured blocks.
    #[cfg(test)]
    pub fn inline_bytes(&self) -> usize {
        self.inline_bytes
    }

    /// True when the reply named or carried anything that could be a file,
    /// including a `MEDIA:` token that looked like a path but was unusable
    /// (that earns a note rather than silence).
    ///
    /// A block of a type [`resolve_outbound_files`] does not handle is not
    /// one: without this the harness would take a publish slot, create and
    /// drop a scratch, and post nothing for a reply that never named a file.
    /// A block that did not fit the capture still counts, because that one
    /// might have been a file.
    pub fn references_media(&self) -> bool {
        if self.dropped_blocks > 0 || self.blocks.iter().any(is_publishable_block) {
            return true;
        }
        let refs = extract_media_refs(&self.text);
        !refs.paths.is_empty() || !refs.notes.is_empty()
    }
}

/// The content-block types [`resolve_outbound_files`] turns into files.
/// Anything else is noted and skipped.
fn is_publishable_block(block: &serde_json::Value) -> bool {
    matches!(
        block.get("type").and_then(|t| t.as_str()).unwrap_or(""),
        "resource_link" | "resource" | "image" | "audio"
    )
}

/// `MEDIA:` references found in reply text, plus the ones that looked like a
/// reference but could not be used.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MediaRefs {
    /// Absolute (or `~/`) paths with a supported extension, deduplicated.
    pub paths: Vec<String>,
    /// One reason per `MEDIA:` token that started like a path but was unusable.
    pub notes: Vec<String>,
}

/// Find `MEDIA:<path>` references in reply text.
///
/// Extension-anchored like the Hermes matcher: the token after `MEDIA:` must
/// be an absolute (or `~/`) path ending in a known media extension, so a
/// bare `MEDIA:` in prose never triggers an upload. Trailing punctuation
/// after the extension is dropped.
#[cfg(test)]
pub fn extract_media_paths(text: &str) -> Vec<String> {
    extract_media_refs(text).paths
}

/// Like [`extract_media_paths`], also naming the tokens that started like an
/// absolute path but carried no supported extension (a path with a space, or
/// an extension outside the list), so the miss is not silent.
pub fn extract_media_refs(text: &str) -> MediaRefs {
    let mut refs = MediaRefs::default();
    let mut rest = text;
    while let Some(idx) = rest.find("MEDIA:") {
        let preceded_ok = idx == 0
            || rest[..idx].chars().next_back().is_some_and(|c| {
                c.is_whitespace() || matches!(c, '(' | '[' | '`' | '"' | '\'' | '*' | '_')
            });
        let after = &rest[idx + "MEDIA:".len()..];
        let after = after.strip_prefix(' ').unwrap_or(after);
        let token: &str = after.split(char::is_whitespace).next().unwrap_or("");
        if preceded_ok {
            match trim_to_media_extension(token) {
                // The same token twice is one reference, deduplicated here
                // with no note: nothing was skipped, the reply named one
                // file. Two *different* names for one file (a path and a
                // symlink to it) are two references, and the second earns a
                // note in `resolve_outbound_files`.
                Some(path) if is_absolute_or_home(path) && !refs.paths.iter().any(|p| p == path) => {
                    refs.paths.push(path.to_string());
                }
                Some(_) => {}
                None if is_absolute_or_home(token) => refs.notes.push(format!(
                    "MEDIA:{} skipped: no supported media extension (paths with spaces are not supported)",
                    token.chars().take(80).collect::<String>()
                )),
                None => {}
            }
        }
        rest = &rest[idx + "MEDIA:".len()..];
    }
    refs
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
    /// Absolute path of the harness-owned snapshot in the publish scratch
    /// directory. Never the path the reply named.
    pub path: PathBuf,
    /// Size of that snapshot, re-checked through the handle at upload time.
    pub len: u64,
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
    /// Files to upload, at most [`MAX_OUTBOUND_FILES`]. Every path is a
    /// harness-owned snapshot under the publish scratch directory.
    pub files: Vec<OutboundFile>,
    /// Human-readable reasons for references that were not turned into files.
    pub notes: Vec<String>,
}

impl OutboundResolution {
    /// True when the reply neither produced a file nor a reason.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.notes.is_empty()
    }
}

/// The only directories a reply may publish files from.
///
/// `turn_dir` is this turn's directory under the attachment root (the inbound
/// blobs live there); `workspace` is the engine's working directory, when it
/// passed [`outbound_workspace`]. Anything else on the host is refused,
/// whatever the reply says.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OutboundRoots {
    /// `<attachment root>/<turn id>`.
    pub turn_dir: PathBuf,
    /// The engine's working directory, when it is usable as an outbound root.
    pub workspace: Option<PathBuf>,
    /// Why the working directory is not a root, when it was refused; named in
    /// the notes so the human sees why a file they expected did not go.
    pub workspace_refused: Option<String>,
    /// `HOME` for `~/` expansion. Passed in rather than read from the
    /// environment so tests never mutate a process-wide variable.
    pub home: Option<PathBuf>,
}

impl OutboundRoots {
    /// Reason text for a path that is under no root.
    ///
    /// The notes this feeds are posted into the channel, so the refusal
    /// reason itself never appears here: it names the canonical working
    /// directory, which is the host's absolute `HOME` in the documented
    /// deployment. The pool logs the reason; the channel gets the class.
    fn outside_reason(&self) -> String {
        match (&self.workspace, &self.workspace_refused) {
            (_, Some(_)) => {
                "outside the turn directory (the working directory is not a publishable root)"
                    .into()
            }
            (Some(_), None) => "outside the turn directory and the workspace".into(),
            (None, None) => "outside the turn directory".into(),
        }
    }

    /// Resolve `path` to the canonical regular file it names, provided that
    /// file is under one of the roots.
    ///
    /// The path must be absolute and free of `..`; symlinks anywhere in it
    /// are followed and the target is what has to sit under a root, so a link
    /// planted inside the workspace cannot reach outside it. Roots that do
    /// not exist yet cannot contain anything and are skipped.
    pub fn confine(&self, path: &Path) -> Result<PathBuf, String> {
        if !path.is_absolute() {
            return Err("not an absolute path".into());
        }
        if path.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err("path traversal (..) is not allowed".into());
        }
        let is_symlink = std::fs::symlink_metadata(path)
            .map(|m| m.file_type().is_symlink())
            .map_err(|e| e.to_string())?;
        let canonical = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
        let inside = [Some(&self.turn_dir), self.workspace.as_ref()]
            .into_iter()
            .flatten()
            .filter_map(|root| std::fs::canonicalize(root).ok())
            .any(|root| canonical.starts_with(&root));
        if !inside {
            return Err(if is_symlink {
                format!("symlink resolves {}", self.outside_reason())
            } else {
                self.outside_reason()
            });
        }
        Ok(canonical)
    }
}

/// Directories that belong to the host rather than to whoever started the
/// harness. A working directory that is one of these, sits under one, or
/// contains one is not a boundary around this agent's work: it is a boundary
/// around the machine, and everything readable inside it would become
/// publishable by anyone who can talk to the agent.
///
/// Each entry is canonicalised before it is compared, so the macOS symlink
/// farm (`/etc` to `/private/etc`, `/var` to `/private/var`) is covered by
/// the same list as Linux.
const SYSTEM_PREFIXES: &[&str] = &[
    "/bin",
    "/boot",
    "/dev",
    "/etc",
    "/lib",
    "/lib32",
    "/lib64",
    "/libexec",
    "/media",
    "/mnt",
    "/net",
    "/opt",
    "/proc",
    "/root",
    "/run",
    "/sbin",
    "/srv",
    "/sys",
    "/usr",
    "/var",
    "/Applications",
    "/Library",
    "/Network",
    "/System",
    "/Volumes",
];

/// Decide whether the harness working directory may be an outbound root.
///
/// The harness runs wherever it was started, so "the engine's working
/// directory" is not by itself a boundary, and this is a positive test that
/// it is one rather than a list of the two shapes that burned us. A
/// directory passes only when all of these hold:
///
/// - it is absolute and resolves;
/// - it is not the filesystem root;
/// - `HOME` is known, and the directory is neither `HOME` nor an ancestor of
///   it (`docs/remote-agents.md` documents cwd = `HOME` as the deployment
///   convention, and a reply naming `~/Documents/passport.pdf` under that cwd
///   would publish a host file);
/// - it neither contains the attachment root nor sits inside it (either way a
///   reply could name another channel's inbound blobs out of a sibling turn
///   directory);
/// - it is not, does not contain, and does not sit under a system directory
///   ([`SYSTEM_PREFIXES`]), so a harness started from `/etc`, `/var/lib/app`
///   or a mount point does not turn the host's configuration into channel
///   content. The per-process temp directory is exempt: on macOS it lives
///   under `/var`, and a scratch directory there is per-user, not the host's.
///
/// `home` being unknown is a refusal, not a skipped check: the check that
/// matters most in the documented deployment is the `HOME` one, and a
/// containment input that cannot be resolved is an error (rule 4). A
/// launchd job, a systemd unit without `User=`, and `docker run` without
/// `-e HOME` all reach this.
///
/// Every refusal is returned with its reason, which the pool logs. The reply
/// notes carry a constant instead: the reason names host paths, and the
/// notes are posted into a channel.
pub fn outbound_workspace(
    cwd: &Path,
    home: Option<&Path>,
    attachment_root: Option<&Path>,
) -> Result<PathBuf, String> {
    if !cwd.is_absolute() {
        return Err(format!("{} is not an absolute path", cwd.display()));
    }
    let canonical = std::fs::canonicalize(cwd)
        .map_err(|e| format!("{} cannot be resolved: {e}", cwd.display()))?;
    if canonical.parent().is_none() {
        return Err("the filesystem root is not an outbound root".into());
    }
    if let Some(system) = system_directory_at(&canonical) {
        return Err(format!(
            "{} is, contains, or sits under the system directory {}; a reply must not be able to name a file the host owns",
            canonical.display(),
            system.display()
        ));
    }
    let Some(home) = home.map(|h| std::fs::canonicalize(h).unwrap_or_else(|_| h.to_path_buf()))
    else {
        return Err(
            "HOME is not set, so the working directory cannot be judged as a boundary".into(),
        );
    };
    if home.starts_with(&canonical) {
        return Err(format!(
            "{} is HOME or contains it; a reply must not be able to name a file anywhere under HOME",
            canonical.display()
        ));
    }
    if let Some(root) = attachment_root
        .map(|r| std::fs::canonicalize(r).unwrap_or_else(|_| r.to_path_buf()))
        .filter(|root| root.starts_with(&canonical) || canonical.starts_with(root))
    {
        return Err(format!(
            "{} contains the attachment root {} or sits inside it; a reply must not be able to name another turn's blobs",
            canonical.display(),
            root.display()
        ));
    }
    Ok(canonical)
}

/// The system directory `canonical` is, contains, or sits under, if any.
fn system_directory_at(canonical: &Path) -> Option<PathBuf> {
    let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
    if canonical.starts_with(&temp) {
        return None;
    }
    SYSTEM_PREFIXES
        .iter()
        .map(Path::new)
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()))
        .find(|prefix| canonical.starts_with(prefix) || prefix.starts_with(canonical))
}

/// Copy a file the reply named into the publish scratch so the upload reads
/// a harness-owned snapshot taken at the end of the turn, not whatever the
/// path points at later.
///
/// Every file goes through this, the turn directory's own inbound blobs
/// included: the turn directory is engine-writable too, so re-opening any
/// resolved path by name during a publish that can run for minutes hands the
/// engine a swap window. The source is opened once, without following a
/// symlink at its last component, checked for eligibility through that open
/// handle (regular file, unchanged length, one link), and the bytes are read
/// from the handle rather than from the path. The destination is created
/// relative to the scratch's own directory descriptor.
///
/// A directory component of the *source* swapped between `canonicalize` and
/// the open is still not caught; that needs `openat2`-style resolution and
/// is Linux-only.
///
/// Every error returned is a reason with no path in it: the caller prefixes
/// the reply's own name for the file and the result is posted into the
/// channel, so the canonical host path stays in the log.
fn stage_file(
    scratch: &crate::attachments::PublishScratch,
    source: &Path,
    meta_len: u64,
) -> Result<(PathBuf, u64), String> {
    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin")
        .to_string();
    let (mut file, _) = crate::attachments::open_verified(source, Some(meta_len))
        .map_err(|e| format!("cannot be opened: {e}"))?;
    let (mut out, staged) = scratch
        .create_file(&ext)
        .map_err(|e| format!("cannot create a staging file: {e}"))?;
    let copied = std::io::copy(
        &mut std::io::Read::take(&mut file, MAX_OUTBOUND_FILE_BYTES + 1),
        &mut out,
    )
    .map_err(|e| format!("cannot be staged: {e}"))?;
    if copied != meta_len || copied > MAX_OUTBOUND_FILE_BYTES {
        let _ = scratch.remove_file(&staged);
        return Err(format!(
            "it changed while it was being staged ({meta_len} bytes became {copied})"
        ));
    }
    Ok((staged, copied))
}

/// Record one staged or decoded file as an upload source.
fn push_file(
    out: &mut OutboundResolution,
    path: PathBuf,
    len: u64,
    shown: &str,
    filename: Option<String>,
    mime: Option<String>,
    origin: &'static str,
) {
    let filename = crate::attachments::safe_attachment_filename(
        &filename
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| shown.to_string()),
    );
    let mime = mime
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| mime_for_path(Path::new(shown)).to_string());
    out.files.push(OutboundFile {
        path,
        len,
        filename,
        mime,
        origin,
    });
}

/// Turn a capture into concrete files under `scratch`. Every named file is
/// confined to the roots and then staged; inline block data is decoded
/// straight into the scratch. Missing, unreadable, oversized, over-cap,
/// duplicate, or out-of-bounds references become notes instead of files.
pub fn resolve_outbound_files(
    capture: &TurnMediaCapture,
    roots: &OutboundRoots,
    scratch: &crate::attachments::PublishScratch,
) -> OutboundResolution {
    let mut out = OutboundResolution::default();
    // Canonical sources already staged, so a file and a symlink to it are one
    // upload and cost one slot of the per-reply cap, not two.
    let mut staged: Vec<PathBuf> = Vec::new();

    let mut add = |out: &mut OutboundResolution,
                   path: PathBuf,
                   filename: Option<String>,
                   mime: Option<String>,
                   origin: &'static str| {
        let shown = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        if out.files.len() >= MAX_OUTBOUND_FILES {
            out.notes.push(format!(
                "{shown} skipped: over the {MAX_OUTBOUND_FILES}-file limit per reply"
            ));
            return;
        }
        let canonical = match roots.confine(&path) {
            Ok(canonical) => canonical,
            Err(reason) => {
                out.notes.push(format!("{shown} refused: {reason}"));
                return;
            }
        };
        if staged.contains(&canonical) {
            out.notes
                .push(format!("{shown} skipped: already attached in this reply"));
            return;
        }
        let meta = match std::fs::symlink_metadata(&canonical) {
            Ok(meta) => meta,
            Err(e) => {
                out.notes.push(format!("{shown} skipped: {e}"));
                return;
            }
        };
        if !meta.is_file() {
            out.notes
                .push(format!("{shown} skipped: not a regular file"));
            return;
        }
        if meta.len() == 0 {
            out.notes.push(format!("{shown} skipped: empty file"));
            return;
        }
        if meta.len() > MAX_OUTBOUND_FILE_BYTES {
            out.notes.push(format!(
                "{shown} skipped: {} bytes exceeds the {MAX_OUTBOUND_FILE_BYTES} byte limit",
                meta.len()
            ));
            return;
        }
        let (stored, len) = match stage_file(scratch, &canonical, meta.len()) {
            Ok(staged) => staged,
            Err(reason) => {
                out.notes.push(format!("{shown} skipped: {reason}"));
                return;
            }
        };
        staged.push(canonical);
        push_file(out, stored, len, &shown, filename, mime, origin);
    };

    // Inline data never passes through `confine`: the harness decodes it into
    // its own scratch, so there is no host path to confine and nothing the
    // engine can point at.
    let mut inline_index = 0usize;
    let mut add_inline = |out: &mut OutboundResolution,
                          data: &str,
                          filename: Option<String>,
                          mime: Option<String>,
                          origin: &'static str,
                          label: &str| {
        if out.files.len() >= MAX_OUTBOUND_FILES {
            out.notes.push(format!(
                "{label} skipped: over the {MAX_OUTBOUND_FILES}-file limit per reply"
            ));
            return;
        }
        inline_index += 1;
        let shown = format!(
            "inline-{inline_index}.{}",
            extension_for_mime(mime.as_deref().unwrap_or(""))
        );
        match write_inline(scratch, data, mime.as_deref()) {
            Ok((path, 0)) => {
                let _ = scratch.remove_file(&path);
                out.notes.push(format!("{label} skipped: no bytes"));
            }
            Ok((path, len)) => push_file(out, path, len, &shown, filename, mime, origin),
            Err(e) => out.notes.push(format!("{label} skipped: {e}")),
        }
    };

    let refs = extract_media_refs(capture.text());
    out.notes.extend(refs.notes);
    for raw in refs.paths {
        let expanded = if let Some(rest) = raw.strip_prefix("~/") {
            match roots.home.as_ref() {
                Some(home) => home.join(rest),
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
                    add_inline(
                        &mut out,
                        blob,
                        name_from_uri(uri),
                        mime,
                        "resource block",
                        "resource blob",
                    );
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
                    Some(data) => add_inline(
                        &mut out,
                        data,
                        None,
                        mime,
                        "inline content block",
                        &format!("{kind} block"),
                    ),
                    None => out
                        .notes
                        .push(format!("{kind} block skipped: no inline data")),
                }
            }
            other => out.notes.push(format!(
                "{} content block skipped: not a file the harness can publish",
                if other.is_empty() { "untyped" } else { other }
            )),
        }
    }
    if capture.dropped_blocks() > 0 {
        out.notes.push(format!(
            "{} content block(s) were not captured (over the {MAX_CAPTURED_BLOCKS}-block limit or the {MAX_INLINE_BASE64_BUDGET} byte inline budget per reply)",
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

/// Decode one inline block into the publish scratch.
///
/// The file is created with `O_CREAT | O_EXCL | O_NOFOLLOW` at a random name
/// in a directory only this publish knows: a plain `fs::write` at a name the
/// engine can predict inside a directory it can write is an arbitrary
/// host-file overwrite, because the write follows a symlink planted there.
fn write_inline(
    scratch: &crate::attachments::PublishScratch,
    base64_data: &str,
    mime: Option<&str>,
) -> Result<(PathBuf, u64), String> {
    if base64_data.len() > MAX_INLINE_BASE64_BUDGET {
        return Err("inline data exceeds the size limit".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_data.trim())
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(base64_data.trim()))
        .map_err(|e| format!("inline data is not valid base64: {e}"))?;
    if bytes.len() as u64 > MAX_OUTBOUND_FILE_BYTES {
        return Err(format!(
            "{} bytes exceeds the {MAX_OUTBOUND_FILE_BYTES} byte limit",
            bytes.len()
        ));
    }
    let ext = extension_for_mime(mime.unwrap_or(""));
    let (mut file, path) = scratch
        .create_file(ext)
        .map_err(|e| format!("cannot create an inline file: {e}"))?;
    std::io::Write::write_all(&mut file, &bytes)
        .map_err(|e| format!("cannot be written to the staging directory: {e}"))?;
    Ok((path, bytes.len() as u64))
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
            format!(
                "[{}]({})",
                markdown_link_text(&item.filename),
                item.descriptor.url
            )
        }
    }
}

/// Escape the characters that would end a markdown link early.
fn markdown_link_text(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '[' | ']' | '(' | ')' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
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

/// Longest failure notice posted to a channel, in bytes.
const FAILURE_NOTICE_MAX_BYTES: usize = 280;

/// One short line for the channel when a reply's media did not all arrive:
/// how many references failed, the first reason, and how many published.
/// The full list stays in the log and the observer frame.
pub fn failure_notice(report: &PublishReport) -> String {
    let count = report.failed.len();
    let first = report
        .failed
        .first()
        .map(String::as_str)
        .unwrap_or("unknown reason");
    let mut notice = if count == 1 {
        format!("Could not attach a file from my reply: {first}")
    } else {
        format!("Could not attach {count} files from my reply; first reason: {first}")
    };
    if !report.published.is_empty() {
        notice.push_str(&format!(" ({} attached)", report.published.len()));
    }
    if notice.len() > FAILURE_NOTICE_MAX_BYTES {
        let mut cut = FAILURE_NOTICE_MAX_BYTES - 3;
        while !notice.is_char_boundary(cut) {
            cut -= 1;
        }
        notice.truncate(cut);
        notice.push_str("...");
    }
    notice
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
    /// Upload every resolved file and publish one kind-9 with the results.
    /// Returns `None` when the resolution is empty.
    ///
    /// `deadline` bounds the uploads as a whole: a file whose turn comes after
    /// it is skipped with a reason, an upload in progress is cut at it, and
    /// the kind-9 for whatever did finish is still posted under its own
    /// [`PUBLISH_TIMEOUT`].
    pub async fn publish_turn_media(
        &self,
        target: &ReplyTarget,
        resolution: OutboundResolution,
        scratch: &Arc<crate::attachments::PublishScratch>,
        deadline: tokio::time::Instant,
    ) -> Option<PublishReport> {
        if resolution.is_empty() {
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
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                report.failed.push(format!(
                    "{}: skipped, the {OUTBOUND_DEADLINE:?} publish deadline for this reply passed",
                    file.filename
                ));
                continue;
            }
            let attempt = tokio::time::timeout(
                remaining,
                self.upload_one(&origin, file, relay_supports_audio, scratch),
            )
            .await;
            let attempt = match attempt {
                Ok(attempt) => attempt,
                Err(_) => {
                    // The relay may already have stored the blob; nothing
                    // will reference it, so name it by hash in the log
                    // rather than leaving an orphan nobody can find.
                    let sha = read_bounded(scratch, &file.path, Some(file.len))
                        .await
                        .map(|bytes| blossom::sha256_hex(&bytes))
                        .unwrap_or_else(|_| "unknown".into());
                    tracing::warn!(
                        target: "acp::media",
                        file = %file.path.display(),
                        sha256 = %sha,
                        "upload cut at the publish deadline; any blob the relay stored for it is orphaned"
                    );
                    Err(format!(
                        "upload cut off at the {OUTBOUND_DEADLINE:?} publish deadline for this reply"
                    ))
                }
            };
            match attempt {
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
                    report.failed.push(format!("{}: {reason}", file.filename));
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
        scratch: &Arc<crate::attachments::PublishScratch>,
    ) -> Result<PublishedMedia, String> {
        let kind = media_kind(&file.mime);
        if kind != MediaKind::Audio {
            let bytes = read_bounded(scratch, &file.path, Some(file.len)).await?;
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
                AudioDelivery::GenericFile => self.try_generic(origin, file, scratch).await,
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
        scratch: &Arc<crate::attachments::PublishScratch>,
    ) -> Result<PublishedMedia, AudioAttemptError> {
        let stamp = now_ms();
        // Candidates are produced lazily: the stream copy first, and the
        // libmp3lame re-encode only after the relay rejects the copy with a
        // 415/422, the way the Hermes plugin does. One ffmpeg run and one
        // upload is the common case.
        let candidates: Vec<(NativeCandidate, &str)> = match self.ffmpeg {
            Some(_) => {
                let copied = scratch.reserve("mp3");
                let reencoded = scratch.reserve("mp3");
                vec![
                    (
                        NativeCandidate::Encode {
                            out: copied,
                            reencode: false,
                        },
                        "audio/mpeg",
                    ),
                    (
                        NativeCandidate::Encode {
                            out: reencoded,
                            reencode: true,
                        },
                        "audio/mpeg",
                    ),
                ]
            }
            None => {
                let lower = file.mime.to_ascii_lowercase();
                if lower == "audio/mpeg" || lower == "audio/mp3" {
                    vec![(NativeCandidate::AsIs, "audio/mpeg")]
                } else if lower == "audio/mp4" || lower == "audio/x-m4a" {
                    vec![(NativeCandidate::AsIs, "audio/mp4")]
                } else {
                    return Err(AudioAttemptError::FallThrough(format!(
                        "{} cannot be uploaded as native audio without ffmpeg",
                        file.mime
                    )));
                }
            }
        };
        let mut last = String::from("ffmpeg could not produce a clean MP3");
        for (candidate, mime) in candidates {
            let ext = if mime == "audio/mp4" { "m4a" } else { "mp3" };
            // The staged file's length is known; ffmpeg's output is not.
            let expected = matches!(candidate, NativeCandidate::AsIs).then_some(file.len);
            let path = match candidate {
                NativeCandidate::AsIs => file.path.clone(),
                NativeCandidate::Encode { out, reencode } => {
                    let Some(ffmpeg) = self.ffmpeg else {
                        continue;
                    };
                    // ffmpeg opens a path of its own, so this is the one
                    // place the scratch path is resolved by something other
                    // than the descriptor. Refuse if the path no longer
                    // names the directory this publish created.
                    if let Err(e) = scratch.still_at_its_path() {
                        return Err(AudioAttemptError::Terminal(e.to_string()));
                    }
                    match crate::ffmpeg::convert_to_clean_mp3(ffmpeg, &file.path, &out, reencode)
                        .await
                    {
                        Ok(()) => out,
                        Err(e) => {
                            tracing::debug!(
                                target: "acp::media",
                                "mp3 {} failed: {e}",
                                if reencode { "re-encode" } else { "copy" }
                            );
                            last = format!("ffmpeg could not produce a clean MP3: {e}");
                            continue;
                        }
                    }
                }
            };
            let bytes = read_bounded(scratch, &path, expected)
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
        scratch: &Arc<crate::attachments::PublishScratch>,
    ) -> Result<PublishedMedia, AudioAttemptError> {
        let Some(ffmpeg) = self.ffmpeg else {
            return Err(AudioAttemptError::FallThrough(
                "ffmpeg not available".into(),
            ));
        };
        let stamp = now_ms();
        let out = scratch.reserve("mp4");
        // As in `try_native_audio`: ffmpeg resolves this path itself, so the
        // path is checked against the descriptor first and the output is
        // read back through the descriptor afterwards.
        scratch
            .still_at_its_path()
            .map_err(|e| AudioAttemptError::Terminal(e.to_string()))?;
        crate::ffmpeg::wrap_as_voice_note_mp4(ffmpeg, &file.path, &out)
            .await
            .map_err(AudioAttemptError::FallThrough)?;
        let bytes = read_bounded(scratch, &out, None)
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
        scratch: &Arc<crate::attachments::PublishScratch>,
    ) -> Result<PublishedMedia, AudioAttemptError> {
        let bytes = read_bounded(scratch, &file.path, Some(file.len))
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

/// One way to produce the bytes for a native-audio upload.
enum NativeCandidate {
    /// The file already is MP3 or M4A; upload it unchanged.
    AsIs,
    /// Run ffmpeg into `out`, copying the stream or re-encoding it.
    Encode { out: PathBuf, reencode: bool },
}

/// Read an upload source through an `O_NOFOLLOW` open whose handle is
/// checked before a byte is taken from it.
///
/// `expected_len` is the size recorded when the file was staged; a staged
/// file that no longer matches it is not uploaded. `None` is for a file this
/// process just produced with ffmpeg inside the publish scratch, whose size
/// is not known in advance.
///
/// `tokio::fs::read` would re-resolve the path, which is the whole window:
/// the publish task outlives the turn, the engine's next turn runs beside it,
/// and a path resolved minutes earlier is not a promise about what the name
/// points at now.
async fn read_bounded(
    scratch: &Arc<crate::attachments::PublishScratch>,
    path: &Path,
    expected_len: Option<u64>,
) -> Result<Vec<u8>, String> {
    let owned = path.to_path_buf();
    let scratch = Arc::clone(scratch);
    tokio::task::spawn_blocking(move || read_bounded_blocking(&scratch, &owned, expected_len))
        .await
        .map_err(|e| format!("reading it failed: {e}"))?
}

/// Every string returned from here is a *reason*, never a path: it is
/// prefixed with the reply's own name for the file by the two callers and
/// ends up in [`failure_notice`], which is posted into the channel. The
/// absolute path is on the `tracing` line beside each call instead. This is
/// the same rule [`OutboundRoots::outside_reason`] follows, applied to the
/// upload half.
fn read_bounded_blocking(
    scratch: &crate::attachments::PublishScratch,
    path: &Path,
    expected_len: Option<u64>,
) -> Result<Vec<u8>, String> {
    let (file, len) = scratch
        .open_file(path, expected_len)
        .map_err(|e| format!("cannot be opened: {e}"))?;
    if len > MAX_OUTBOUND_FILE_BYTES {
        return Err(format!(
            "it is {len} bytes, over the {MAX_OUTBOUND_FILE_BYTES} byte limit"
        ));
    }
    let mut bytes = Vec::with_capacity(len as usize);
    std::io::Read::read_to_end(
        &mut std::io::Read::take(file, MAX_OUTBOUND_FILE_BYTES + 1),
        &mut bytes,
    )
    .map_err(|e| format!("cannot be read: {e}"))?;
    if bytes.len() as u64 != len {
        return Err(format!(
            "it changed while it was being read ({len} bytes became {})",
            bytes.len()
        ));
    }
    Ok(bytes)
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

    /// The test root doubles as the turn directory, so files written under
    /// it are the turn's own; `workspace` is a sibling the engine could
    /// write to.
    fn roots(turn_dir: &Path) -> OutboundRoots {
        OutboundRoots {
            turn_dir: turn_dir.to_path_buf(),
            ..Default::default()
        }
    }

    /// The harness-private publish directory, as the pool creates it
    /// (shared, because the upload phase reads every file back through it).
    fn scratch_under(root: &Path) -> Arc<crate::attachments::PublishScratch> {
        Arc::new(crate::attachments::PublishScratch::create(root).unwrap())
    }

    fn far_deadline() -> tokio::time::Instant {
        tokio::time::Instant::now() + Duration::from_secs(3600)
    }

    impl MediaPublisher<'_> {
        /// Resolve then publish, the way the pool does, for a capture whose
        /// files live under `turn_dir`.
        async fn publish_capture(
            &self,
            target: &ReplyTarget,
            capture: &TurnMediaCapture,
            turn_dir: &Path,
        ) -> Option<PublishReport> {
            let roots = roots(turn_dir);
            let scratch = scratch_under(turn_dir);
            let resolution = resolve_outbound_files(capture, &roots, &scratch);
            self.publish_turn_media(target, resolution, &scratch, far_deadline())
                .await
        }
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
            "type": "audio", "mimeType": "audio/mpeg", "data": "A".repeat(MAX_INLINE_BASE64_BUDGET + 1)
        }));
        assert!(oversized.blocks().is_empty());
        assert_eq!(oversized.dropped_blocks(), 1);
        assert!(!oversized.is_empty(), "a dropped block is still a signal");
        assert!(TurnMediaCapture::default().is_empty());
    }

    #[test]
    fn inline_budget_is_per_turn_not_per_block() {
        // Three blocks each well under the per-block size but together over
        // the turn budget: the first two fit, the third is dropped, and a
        // small fourth still fits in what is left.
        let half = MAX_INLINE_BASE64_BUDGET / 2;
        let mut capture = TurnMediaCapture::default();
        for _ in 0..2 {
            capture.record_chunk(&serde_json::json!({
                "type": "audio", "mimeType": "audio/mpeg", "data": "A".repeat(half - 8)
            }));
        }
        capture.record_chunk(&serde_json::json!({
            "type": "audio", "mimeType": "audio/mpeg", "data": "A".repeat(32)
        }));
        capture.record_chunk(&serde_json::json!({
            "type": "resource", "resource": {"uri": "file:///x.png", "blob": "A".repeat(16)}
        }));
        assert_eq!(capture.blocks().len(), 3);
        assert_eq!(capture.dropped_blocks(), 1);
        assert_eq!(capture.inline_bytes(), 2 * (half - 8) + 16);
        assert!(capture.inline_bytes() <= MAX_INLINE_BASE64_BUDGET);
        // Text and blocks without inline data cost nothing against the budget.
        capture.record_chunk(&serde_json::json!({"type": "resource_link", "uri": "file:///y.png"}));
        assert_eq!(capture.blocks().len(), 4);
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
        assert_eq!(
            extract_media_paths("**MEDIA:/tmp/bold.mp3** _MEDIA:/tmp/em.wav_"),
            vec!["/tmp/bold.mp3".to_string(), "/tmp/em.wav".to_string()],
            "markdown emphasis before the marker is not a word character"
        );
    }

    #[test]
    fn unusable_media_refs_are_named_not_dropped() {
        let refs = extract_media_refs(
            "MEDIA:/tmp/my note.mp3 and MEDIA:/tmp/archive.tar.gz then MEDIA:/tmp/ok.ogg",
        );
        assert_eq!(refs.paths, vec!["/tmp/ok.ogg".to_string()]);
        assert_eq!(refs.notes.len(), 2, "{:?}", refs.notes);
        assert!(refs.notes[0].contains("MEDIA:/tmp/my"), "{:?}", refs.notes);
        assert!(refs.notes[1].contains("archive.tar.gz"), "{:?}", refs.notes);
        assert!(
            extract_media_refs("MEDIA: is a tag used by tools")
                .notes
                .is_empty(),
            "prose after the marker is not a path and earns no note"
        );
        assert!(
            extract_media_refs("MEDIA:relative/x.mp3").notes.is_empty(),
            "a relative token is not a candidate, as before"
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
        let roots = roots(&root);
        let scratch = scratch_under(&root);
        let resolved = resolve_outbound_files(&capture, &roots, &scratch);

        // Every upload source is a snapshot in the private scratch, never the
        // path the reply named.
        assert!(
            resolved
                .files
                .iter()
                .all(|f| f.path.parent() == Some(scratch.dir())),
            "{resolved:?}"
        );
        assert_eq!(std::fs::read(&resolved.files[0].path).unwrap(), b"RIFF....");
        assert_eq!(resolved.files[0].len, 8);
        assert_eq!(resolved.files[0].filename, "clip.wav");
        assert_eq!(resolved.files[0].mime, "audio/wav");
        assert_eq!(resolved.files[0].origin, "MEDIA: line");
        assert_eq!(std::fs::read(&resolved.files[1].path).unwrap(), b"\x89PNG");
        assert_eq!(resolved.files[1].filename, "screen.png");
        assert_eq!(resolved.files[1].mime, "image/png");
        assert_eq!(std::fs::read(&resolved.files[2].path).unwrap(), b"mp3bytes");
        assert_eq!(resolved.files[2].filename, "inline-1.mp3");
        assert_eq!(resolved.files.len(), 3, "duplicate MEDIA path collapsed");
        assert!(
            resolved.notes.iter().any(|n| n.contains("empty file")),
            "{:?}",
            resolved.notes
        );
        assert!(
            resolved.notes.iter().any(|n| n.contains("x.mp3")),
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
        let resolved = resolve_outbound_files(&capture, &roots(&root), &scratch_under(&root));
        assert_eq!(resolved.files.len(), MAX_OUTBOUND_FILES);
        assert!(
            resolved.notes.iter().any(|n| n.contains("file limit")),
            "{:?}",
            resolved.notes
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The blocker from review: a reply may only name files under the turn
    /// directory or the workspace. Everything else on the host is refused
    /// with a reason, symlinks are judged by where they resolve, and every
    /// accepted file is snapshotted into the private publish scratch.
    #[test]
    fn resolve_confines_paths_to_the_turn_dir_and_workspace() {
        let base = temp_root();
        let turn_dir = base.join("turn");
        let workspace = base.join("workspace");
        let elsewhere = base.join("elsewhere");
        for d in [&turn_dir, &workspace, &elsewhere] {
            std::fs::create_dir_all(d).unwrap();
        }
        let secret = elsewhere.join("passport.pdf");
        std::fs::write(&secret, b"%PDF secret").unwrap();
        let ws_file = workspace.join("report.pdf");
        std::fs::write(&ws_file, b"%PDF report").unwrap();
        let owned = turn_dir.join("1-voice-note.mp3");
        std::fs::write(&owned, b"mp3").unwrap();
        let link_out = turn_dir.join("out-link.pdf");
        std::os::unix::fs::symlink(&secret, &link_out).unwrap();
        let link_in = workspace.join("in-link.pdf");
        std::os::unix::fs::symlink(&ws_file, &link_in).unwrap();
        let traversal = format!("{}/../elsewhere/passport.pdf", workspace.display());
        // `~/` expands through the harness HOME; point HOME somewhere the
        // roots do not cover so the expansion itself cannot rescue it.
        let home_secret = elsewhere.join("home-secret.csv");
        std::fs::write(&home_secret, b"a,b").unwrap();

        let mut capture = TurnMediaCapture::default();
        capture.record_chunk(&serde_json::json!({
            "type": "text",
            "text": format!(
                "MEDIA:{}\nMEDIA:{}\nMEDIA:{}\nMEDIA:{}\nMEDIA:{}\nMEDIA:{traversal}\nMEDIA:../../etc/passwd\nMEDIA:/etc/passwd.txt",
                secret.display(), ws_file.display(), owned.display(), link_out.display(), link_in.display()
            )
        }));
        capture.record_chunk(&serde_json::json!({
            "type": "resource_link", "uri": format!("file://{}", secret.display()), "name": "passport.pdf"
        }));
        let roots = OutboundRoots {
            turn_dir: turn_dir.clone(),
            workspace: Some(workspace.clone()),
            ..Default::default()
        };
        let scratch = scratch_under(&base);
        let resolved = resolve_outbound_files(&capture, &roots, &scratch);

        let names: Vec<&str> = resolved.files.iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(
            names,
            vec!["report.pdf", "1-voice-note.mp3"],
            "the workspace file and the symlink to it are one upload: {resolved:?}"
        );
        // Every accepted file, the turn's own included, is staged into the
        // private scratch; nothing is uploaded from a path the engine can
        // still reach by name.
        assert!(
            resolved
                .files
                .iter()
                .all(|f| f.path.parent() == Some(scratch.dir())),
            "{resolved:?}"
        );
        assert!(
            !scratch.dir().starts_with(&turn_dir) && !scratch.dir().starts_with(&workspace),
            "the scratch is outside the engine's reach: {}",
            scratch.dir().display()
        );
        assert_eq!(
            std::fs::read(&resolved.files[0].path).unwrap(),
            b"%PDF report"
        );
        assert_eq!(std::fs::read(&resolved.files[1].path).unwrap(), b"mp3");

        let refused = |needle: &str| {
            resolved
                .notes
                .iter()
                .find(|n| n.contains(needle))
                .unwrap_or_else(|| panic!("no note for {needle}: {:?}", resolved.notes))
                .clone()
        };
        // N2: the second reference to an already-staged file is skipped with
        // a reason, like every other reference that produces no upload.
        assert!(
            refused("in-link.pdf skipped").contains("already attached in this reply"),
            "{:?}",
            resolved.notes
        );
        assert!(refused("passport.pdf refused").contains("outside the turn directory"));
        assert!(refused("out-link.pdf refused").contains("symlink resolves outside"));
        assert!(refused("passport.pdf refused: path traversal").contains(".."));
        assert!(
            refused("passwd.txt refused").contains("outside")
                || refused("passwd.txt").contains("No such file")
        );
        assert_eq!(
            resolved
                .notes
                .iter()
                .filter(|n| n.contains("passport.pdf refused"))
                .count(),
            3,
            "the MEDIA line, the traversal, and the resource_link were each refused: {:?}",
            resolved.notes
        );
        assert!(
            !resolved
                .notes
                .iter()
                .any(|n| n.contains("etc/passwd") && !n.contains("passwd.txt")),
            "a relative reference never becomes a candidate: {:?}",
            resolved.notes
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// B1 from the confirmation pass: the engine owns the turn directory, so
    /// anything the harness writes at a name the engine can predict is a
    /// symlink it can plant. Nothing is written under the turn directory at
    /// all now, and every scratch file is created with `O_EXCL | O_NOFOLLOW`
    /// at a random name, so a planted link cannot be followed.
    #[test]
    fn planted_symlinks_in_the_turn_dir_cannot_be_written_through() {
        let base = temp_root();
        let turn_dir = base.join("turn");
        let workspace = base.join("ws");
        let elsewhere = base.join("elsewhere");
        for d in [&turn_dir, &workspace, &elsewhere] {
            std::fs::create_dir_all(d).unwrap();
        }
        let victim = elsewhere.join("victim.txt");
        std::fs::write(&victim, b"victim contents").unwrap();
        let ws_file = workspace.join("report.txt");
        std::fs::write(&ws_file, b"report").unwrap();
        // The engine plants links at every name the old code used, in the
        // directory it was handed in the prompt.
        let out = turn_dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        for name in ["staged-1.txt", "inline-1.png", "staged-1.bin"] {
            std::os::unix::fs::symlink(&victim, out.join(name)).unwrap();
        }

        let mut capture = TurnMediaCapture::default();
        capture.record_chunk(&serde_json::json!({
            "type": "text",
            "text": format!("MEDIA:{}", ws_file.display())
        }));
        capture.record_chunk(&serde_json::json!({
            "type": "image", "mimeType": "image/png",
            "data": base64::engine::general_purpose::STANDARD.encode(b"engine png")
        }));
        let roots = OutboundRoots {
            turn_dir: turn_dir.clone(),
            workspace: Some(workspace.clone()),
            ..Default::default()
        };
        let scratch = scratch_under(&base);
        let resolved = resolve_outbound_files(&capture, &roots, &scratch);

        assert_eq!(resolved.files.len(), 2, "{resolved:?}");
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"victim contents",
            "the planted link was never written through"
        );
        assert!(
            resolved
                .files
                .iter()
                .all(|f| f.path.parent() == Some(scratch.dir())),
            "{resolved:?}"
        );
        assert!(
            std::fs::read_dir(&out)
                .unwrap()
                .all(|e| e.unwrap().file_type().unwrap().is_symlink()),
            "the engine-writable scratch still holds only what the engine put there"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// S2: the upload reads the file through an `O_NOFOLLOW` open of the
    /// staged snapshot, so replacing the resolved path with a link to a
    /// secret after resolution leaks nothing.
    #[test]
    fn a_post_resolve_symlink_swap_is_not_read() {
        let base = temp_root();
        let turn_dir = base.join("turn");
        std::fs::create_dir_all(&turn_dir).unwrap();
        // Exactly as long as the file it replaces, so only the O_NOFOLLOW
        // open stands between the upload and the secret.
        let secret = base.join("secret.txt");
        std::fs::write(&secret, b"spies").unwrap();
        let owned = turn_dir.join("1-voice.txt");
        std::fs::write(&owned, b"voice").unwrap();

        let mut capture = TurnMediaCapture::default();
        capture.record_chunk(
            &serde_json::json!({"type": "text", "text": format!("MEDIA:{}", owned.display())}),
        );
        let scratch = scratch_under(&base);
        let resolved = resolve_outbound_files(&capture, &roots(&turn_dir), &scratch);
        assert_eq!(resolved.files.len(), 1, "{resolved:?}");

        // The engine swaps the file it named for a link to the secret while
        // the publish task is still running.
        std::fs::remove_file(&owned).unwrap();
        std::os::unix::fs::symlink(&secret, &owned).unwrap();
        let bytes = read_bounded_blocking(
            &scratch,
            &resolved.files[0].path,
            Some(resolved.files[0].len),
        )
        .expect("the staged snapshot still reads");
        assert_eq!(
            bytes, b"voice",
            "the upload reads the snapshot, not the swap"
        );

        // And the staged path itself refuses to be read through a link.
        let staged = resolved.files[0].path.clone();
        std::fs::remove_file(&staged).unwrap();
        std::os::unix::fs::symlink(&secret, &staged).unwrap();
        let err = read_bounded_blocking(&scratch, &staged, Some(resolved.files[0].len))
            .expect_err("a symlink at the staged path is not followed");
        assert!(err.contains("cannot be opened"), "{err}");
        // N1, upload half: the reason is prefixed with the reply's own name
        // for the file and posted into the channel, so it carries no path.
        assert!(!err.contains(&base.display().to_string()), "{err}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// B1a from the third confirmation pass, and the original blocker
    /// restored through a different door: containment is decided with
    /// `canonicalize`, which resolves symlinks and has nothing to resolve
    /// for a hard link, so a second name for `~/Documents/passport.pdf`
    /// inside an outbound root *is* its own canonical path and every
    /// path-shaped guard agrees it belongs there. The turn directory is a
    /// root unconditionally and is engine-writable, so this works in every
    /// deployment, with the workspace correctly refused.
    ///
    /// `nlink == 1`, read from the handle the bytes would be read from, is
    /// the only thing that catches it. Removing that check from
    /// `open_verified` publishes the file and fails this test.
    #[test]
    fn a_hard_link_into_a_root_publishes_nothing() {
        let base = temp_root();
        let home = base.join("home");
        let turn_dir = base.join("run/attachments/turn-1");
        for d in [&home, &turn_dir] {
            std::fs::create_dir_all(d).unwrap();
        }
        let secret = home.join("passport.pdf");
        std::fs::write(&secret, b"%PDF passport").unwrap();
        // The engine, running under the harness's uid in the directory it
        // was handed, gives the host file a second name inside the root.
        let planted = turn_dir.join("note.pdf");
        std::fs::hard_link(&secret, &planted).unwrap();
        // A file that is only in the root is unaffected.
        let own = turn_dir.join("1-voice-note.mp3");
        std::fs::write(&own, b"mp3").unwrap();

        let refusal = outbound_workspace(&home, Some(&home), Some(&base.join("run/attachments")))
            .expect_err("cwd = HOME, so the turn directory is the only root");
        let roots = OutboundRoots {
            turn_dir: turn_dir.clone(),
            workspace: None,
            workspace_refused: Some(refusal),
            home: Some(home.clone()),
        };
        let mut capture = TurnMediaCapture::default();
        capture.record_chunk(&serde_json::json!({
            "type": "text",
            "text": format!("MEDIA:{}\nMEDIA:{}", planted.display(), own.display()),
        }));
        let scratch = scratch_under(&base);
        let resolved = resolve_outbound_files(&capture, &roots, &scratch);

        let names: Vec<&str> = resolved.files.iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(
            names,
            vec!["1-voice-note.mp3"],
            "the hard link is not published: {resolved:?}"
        );
        assert!(
            resolved
                .notes
                .iter()
                .any(|n| n.contains("note.pdf skipped") && n.contains("more than one name")),
            "and the refusal says why: {:?}",
            resolved.notes
        );
        // N1 again, on the guard this round adds: the note goes into the
        // channel, so it names the reply's own basename for the file and
        // never the host path the reply resolved to.
        assert!(
            !resolved
                .notes
                .iter()
                .any(|n| n.contains(&base.display().to_string())),
            "no host path reaches the channel: {:?}",
            resolved.notes
        );
        assert!(
            !resolved
                .files
                .iter()
                .any(|f| std::fs::read(&f.path).unwrap_or_default() == b"%PDF passport"),
            "no staged copy holds the host file"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// N3 from the third confirmation pass: a reply carrying only a block
    /// type this module does not turn into a file used to take a publish
    /// slot, create and drop a scratch, and post nothing at all. It is not
    /// a media reference, and when it rides along with one it earns a note
    /// rather than silence.
    #[test]
    fn an_unhandled_block_type_is_not_a_media_reference() {
        let mut alone = TurnMediaCapture::default();
        alone.record_chunk(&serde_json::json!({"type": "diff", "path": "/tmp/x.rs"}));
        assert!(
            !alone.references_media(),
            "a diff block does not start a publish"
        );

        let base = temp_root();
        let turn_dir = base.join("turn");
        std::fs::create_dir_all(&turn_dir).unwrap();
        let own = turn_dir.join("clip.mp3");
        std::fs::write(&own, b"mp3").unwrap();
        let mut along = TurnMediaCapture::default();
        along.record_chunk(
            &serde_json::json!({"type": "text", "text": format!("MEDIA:{}", own.display())}),
        );
        along.record_chunk(&serde_json::json!({"type": "diff", "path": "/tmp/x.rs"}));
        assert!(along.references_media());
        let resolved = resolve_outbound_files(&along, &roots(&turn_dir), &scratch_under(&base));
        assert_eq!(resolved.files.len(), 1, "{resolved:?}");
        assert!(
            resolved
                .notes
                .iter()
                .any(|n| n.contains("diff content block skipped")),
            "{:?}",
            resolved.notes
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// S2, staging half: a source that turns into a symlink between the
    /// confine and the open is refused by `O_NOFOLLOW` rather than copied.
    #[test]
    fn stage_file_refuses_a_symlink_source() {
        let base = temp_root();
        let secret = base.join("secret.txt");
        std::fs::write(&secret, b"top secret").unwrap();
        let link = base.join("link.txt");
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        let scratch = scratch_under(&base);
        let err = stage_file(&scratch, &link, 10).expect_err("a symlink source is not staged");
        assert!(err.contains("cannot be opened"), "{err}");
        assert!(!err.contains(&base.display().to_string()), "{err}");
        assert!(
            std::fs::read_dir(scratch.dir()).unwrap().next().is_none(),
            "nothing was staged"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// B2: the harness working directory is not automatically an outbound
    /// root. HOME (the documented remote-agent cwd), an ancestor of HOME,
    /// the filesystem root, and anything containing the attachment root are
    /// all refused.
    #[test]
    fn workspace_roots_that_are_not_boundaries_are_refused() {
        let base = temp_root();
        let home = base.join("home");
        let attachments = base.join("state").join("attachments");
        let project = base.join("home").join("project");
        let inside_attachments = attachments.join("turn-a");
        for d in [&home, &attachments, &project, &inside_attachments] {
            std::fs::create_dir_all(d).unwrap();
        }
        let refused = |cwd: &Path| {
            outbound_workspace(cwd, Some(&home), Some(&attachments))
                .expect_err("expected a refusal")
        };
        assert!(refused(&home).contains("HOME"));
        assert!(refused(&base).contains("HOME"), "an ancestor of HOME");
        assert!(
            refused(Path::new("/")).contains("HOME") || refused(Path::new("/")).contains("root")
        );
        assert!(
            refused(&base.join("state")).contains("attachment root"),
            "a workspace containing the attachment root"
        );
        assert!(
            refused(&inside_attachments).contains("attachment root"),
            "and a workspace inside the attachment root, which reaches sibling turns"
        );
        // A real project directory under HOME is still a usable root.
        assert_eq!(
            outbound_workspace(&project, Some(&home), Some(&attachments)).unwrap(),
            std::fs::canonicalize(&project).unwrap()
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// B2a from the third confirmation pass: with `HOME` absent from the
    /// environment the HOME arm used to be skipped silently, so a workspace
    /// that *was* HOME passed and the home file published. An input the
    /// boundary cannot be judged without is a refusal (rule 4), not a
    /// skipped check.
    #[test]
    fn a_workspace_is_refused_when_home_is_unknown() {
        let base = temp_root();
        let project = base.join("project");
        let attachments = base.join("attachments");
        for d in [&project, &attachments] {
            std::fs::create_dir_all(d).unwrap();
        }
        let refusal = outbound_workspace(&project, None, Some(&attachments))
            .expect_err("an unjudgeable workspace is not a root");
        assert!(refusal.contains("HOME is not set"), "{refusal}");
        // The same directory with HOME known is a usable root, so it is the
        // missing input that refuses it and nothing else.
        assert!(outbound_workspace(&project, Some(&base.join("home")), Some(&attachments)).is_ok());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// S1 from the third confirmation pass: the rule was a denylist of two
    /// shapes, so a system-directory working directory was accepted and
    /// `MEDIA:/etc/shadow.txt` published. These are real host paths, taken
    /// through the production function; on macOS they canonicalise through
    /// `/private`, which is why the prefixes are canonicalised too.
    #[test]
    fn a_system_directory_is_never_a_workspace() {
        let base = temp_root();
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        for dir in ["/etc", "/usr", "/var"] {
            let path = Path::new(dir);
            assert!(path.is_dir(), "{dir} exists on both targets");
            let refusal = match outbound_workspace(path, Some(&home), None) {
                Err(refusal) => refusal,
                Ok(accepted) => panic!("{dir} was accepted as {}", accepted.display()),
            };
            assert!(
                refusal.contains("system directory"),
                "{dir} is refused as a system directory, not incidentally: {refusal}"
            );
        }
        // The per-process temp directory sits under `/var` on macOS and is
        // not the host's; a workspace there is still usable.
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        assert!(outbound_workspace(&project, Some(&home), None).is_ok());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The original review's scenario, end to end on the fixed code: the
    /// harness is started from HOME (the remote-agent convention), a member
    /// asks the agent to quote `MEDIA:~/Documents/passport.pdf` back, and the
    /// engine obliges. HOME is refused as a root, the `~/` expansion lands
    /// outside the only remaining one, and nothing is published.
    ///
    /// S3 from the third confirmation pass: the attachment root is
    /// deliberately *outside* HOME here, the way `default_attachment_base`
    /// puts it (`XDG_RUNTIME_DIR` or the temp dir). With the root under HOME
    /// the attachment-root rule refused this case and the test passed with
    /// the HOME rule deleted, so it did not pin the scenario it is named for.
    #[test]
    fn a_quoted_home_path_publishes_nothing_when_the_harness_runs_from_home() {
        let base = temp_root();
        let home = base.join("home");
        std::fs::create_dir_all(home.join("Documents")).unwrap();
        std::fs::write(home.join("Documents/passport.pdf"), b"%PDF passport").unwrap();
        let attachment_root = base.join("run/buzz-acp/attachments");
        let turn_dir = attachment_root.join("turn-1");
        std::fs::create_dir_all(&turn_dir).unwrap();

        let refusal = outbound_workspace(&home, Some(&home), Some(&attachment_root))
            .expect_err("cwd = HOME is not an outbound root");
        assert!(
            refusal.contains("HOME"),
            "the HOME rule is what refuses it, not the attachment-root rule: {refusal}"
        );
        let roots = OutboundRoots {
            turn_dir,
            workspace: None,
            workspace_refused: Some(refusal),
            home: Some(home.clone()),
        };
        let mut capture = TurnMediaCapture::default();
        capture.record_chunk(&serde_json::json!({
            "type": "text",
            "text": "Sure, you wrote: `MEDIA:~/Documents/passport.pdf`"
        }));
        let scratch = scratch_under(&base);
        let resolved = resolve_outbound_files(&capture, &roots, &scratch);
        assert!(resolved.files.is_empty(), "{resolved:?}");
        let note = resolved
            .notes
            .iter()
            .find(|n| n.contains("passport.pdf refused"))
            .unwrap_or_else(|| panic!("{:?}", resolved.notes));
        assert!(
            note.contains("the working directory is not a publishable root"),
            "{note}"
        );
        // N1: the note is posted into the channel, so it carries the class of
        // refusal and never the host path the refusal reason names.
        assert!(
            !note.contains(&home.display().to_string()),
            "the host's HOME path does not reach the channel: {note}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// S1: a workspace that contains the attachment root would publish other
    /// channels' inbound blobs out of a sibling turn directory. The B2 rule
    /// refuses it, and the refusal is what the reply notes say.
    #[test]
    fn a_workspace_containing_the_attachment_root_publishes_no_other_turn() {
        let base = temp_root();
        let attachments = base.join("attachments");
        let other_turn = attachments.join("turn-b");
        let this_turn = attachments.join("turn-a");
        for d in [&other_turn, &this_turn] {
            std::fs::create_dir_all(d).unwrap();
        }
        let other_blob = other_turn.join("1-voice-note.mp3");
        std::fs::write(&other_blob, b"another channel").unwrap();

        // HOME is elsewhere, so the attachment-root rule is the only one
        // that can refuse this workspace.
        let home = temp_root();
        let refusal = outbound_workspace(&base, Some(&home), Some(&attachments))
            .expect_err("a workspace containing the attachment root is refused");
        assert!(refusal.contains("attachment root"), "{refusal}");
        let roots = OutboundRoots {
            turn_dir: this_turn.clone(),
            workspace: None,
            workspace_refused: Some(refusal),
            home: None,
        };
        let mut capture = TurnMediaCapture::default();
        capture.record_chunk(
            &serde_json::json!({"type": "text", "text": format!("MEDIA:{}", other_blob.display())}),
        );
        let scratch = scratch_under(&base);
        let resolved = resolve_outbound_files(&capture, &roots, &scratch);
        assert!(resolved.files.is_empty(), "{resolved:?}");
        assert!(
            resolved
                .notes
                .iter()
                .any(|n| n.contains("1-voice-note.mp3 refused")
                    && n.contains("the working directory is not a publishable root")),
            "the note names the class of refusal: {:?}",
            resolved.notes
        );
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn resolve_home_expansion_is_still_confined() {
        let base = temp_root();
        let turn_dir = base.join("turn");
        std::fs::create_dir_all(&turn_dir).unwrap();
        let home = base.join("home");
        std::fs::create_dir_all(home.join("Documents")).unwrap();
        std::fs::write(home.join("Documents/tax.xlsx"), b"cells").unwrap();
        let mut capture = TurnMediaCapture::default();
        capture.record_chunk(
            &serde_json::json!({"type": "text", "text": "MEDIA:~/Documents/tax.xlsx"}),
        );
        // HOME is passed in, never set on the process: another test thread
        // reading it must not see a temp directory.
        let roots = OutboundRoots {
            turn_dir: turn_dir.clone(),
            home: Some(home.clone()),
            ..Default::default()
        };
        let expanded = resolve_outbound_files(&capture, &roots, &scratch_under(&base));
        assert!(expanded.files.is_empty(), "{expanded:?}");
        assert!(
            expanded
                .notes
                .iter()
                .any(|n| n.contains("tax.xlsx refused") && n.contains("outside")),
            "{:?}",
            expanded.notes
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn staged_copy_is_a_snapshot_of_the_turn() {
        let base = temp_root();
        let turn_dir = base.join("turn");
        let workspace = base.join("ws");
        std::fs::create_dir_all(&turn_dir).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let file = workspace.join("clip.wav");
        std::fs::write(&file, b"first").unwrap();
        let mut capture = TurnMediaCapture::default();
        capture.record_chunk(
            &serde_json::json!({"type": "text", "text": format!("MEDIA:{}", file.display())}),
        );
        let roots = OutboundRoots {
            turn_dir,
            workspace: Some(workspace),
            ..Default::default()
        };
        let scratch = scratch_under(&base);
        let resolved = resolve_outbound_files(&capture, &roots, &scratch);
        assert_eq!(resolved.files.len(), 1, "{:?}", resolved.notes);
        // The workspace file changes after the turn ended; the upload source does not.
        std::fs::write(&file, b"second, written after the turn").unwrap();
        assert_eq!(std::fs::read(&resolved.files[0].path).unwrap(), b"first");
        let _ = std::fs::remove_dir_all(&base);
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
        scripted_server_with_delays(|base| {
            script(base)
                .into_iter()
                .map(|(status, body)| (status, body, Duration::ZERO))
                .collect()
        })
        .await
    }

    /// One connection per scripted response, each held for its delay before
    /// the reply is written; a client that gives up first is tolerated so the
    /// later responses still get served.
    async fn scripted_server_with_delays(
        script: impl FnOnce(&str) -> Vec<(&'static str, String, Duration)>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let responses = script(&base);
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        tokio::spawn(async move {
            // A delayed response is written from its own task so the accept
            // loop keeps serving the connections scripted after it.
            for (status, body, delay) in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut chunk = vec![0u8; 16384];
                let mut header_end = None;
                loop {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
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
                tokio::spawn(async move {
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                    let _ = socket.write_all(resp.as_bytes()).await;
                    socket.shutdown().await.ok();
                });
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
            .publish_capture(&target, &capture, &root)
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
            .publish_capture(&target, &capture, &root)
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
            .publish_capture(&target, &capture, &root)
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
        assert!(!capture.references_media());
        let root = temp_root();
        assert!(publisher
            .publish_capture(&target, &capture, &root)
            .await
            .is_none());
        assert!(
            std::fs::read_dir(root.join(".outbound"))
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(true),
            "the publish scratch does not survive the reply that made it"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn uploads_stop_at_the_reply_deadline_but_the_kind9_still_goes_out() {
        let root = temp_root();
        let first = root.join("one.txt");
        let second = root.join("two.txt");
        let third = root.join("three.txt");
        std::fs::write(&first, b"one").unwrap();
        std::fs::write(&second, b"two").unwrap();
        std::fs::write(&third, b"three").unwrap();
        let sha = blossom::sha256_hex(b"one");
        // Connection 1: upload of `one` answers at once. Connection 2: upload
        // of `two` is held past the deadline, so the client is cut off
        // mid-request. `three` is never attempted. Connection 3: the kind-9
        // for `one`.
        let (base, requests) = scripted_server_with_delays(|base| {
            vec![
                (
                    "200 OK",
                    descriptor_json(base, &sha, "text/plain", 3),
                    Duration::ZERO,
                ),
                ("200 OK", "{}".to_string(), Duration::from_secs(5)),
                ("200 OK", r#"{"accepted":true}"#.to_string(), Duration::ZERO),
            ]
        })
        .await;
        let rest = rest_for(&base);
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
        capture.record_chunk(&serde_json::json!({
            "type": "text",
            "text": format!(
                "MEDIA:{}\nMEDIA:{}\nMEDIA:{}",
                first.display(),
                second.display(),
                third.display()
            )
        }));
        let roots = roots(&root);
        let scratch = scratch_under(&root);
        let resolution = resolve_outbound_files(&capture, &roots, &scratch);
        assert_eq!(resolution.files.len(), 3, "{:?}", resolution.notes);
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        let started = std::time::Instant::now();
        let report = publisher
            .publish_turn_media(&target, resolution, &scratch, deadline)
            .await
            .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "the held upload is cut at the deadline, not waited out: {:?}",
            started.elapsed()
        );
        assert_eq!(report.published.len(), 1, "{report:?}");
        assert!(
            report.event_id.is_some(),
            "what finished in time is still announced: {report:?}"
        );
        assert_eq!(report.failed.len(), 2, "{:?}", report.failed);
        assert!(
            report.failed[0].starts_with("two.txt: upload cut off"),
            "{}",
            report.failed[0]
        );
        assert!(
            report.failed[1].starts_with("three.txt: skipped"),
            "{}",
            report.failed[1]
        );
        assert!(report.failed.iter().all(|f| f.contains("deadline")));
        let seen = requests.lock().unwrap().clone();
        assert_eq!(
            seen.len(),
            3,
            "two uploads reached the wire, then the kind-9"
        );
        assert!(seen[0].starts_with("PUT /upload"), "{}", seen[0]);
        assert!(seen[1].starts_with("PUT /upload"), "{}", seen[1]);
        assert!(seen[2].starts_with("POST /events"), "{}", seen[2]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn failure_notice_is_short_and_names_the_first_reason() {
        let mut report = PublishReport {
            failed: vec![
                "passport.pdf refused: outside the turn directory and the workspace".into(),
            ],
            ..PublishReport::default()
        };
        assert_eq!(
            failure_notice(&report),
            "Could not attach a file from my reply: passport.pdf refused: outside the turn directory and the workspace"
        );
        report.failed.push("x".repeat(400));
        report
            .published
            .push(published(MediaKind::File, "ok.txt", None));
        let notice = failure_notice(&report);
        assert!(notice
            .starts_with("Could not attach 2 files from my reply; first reason: passport.pdf"));
        assert!(notice.len() <= FAILURE_NOTICE_MAX_BYTES, "{}", notice.len());
        let mut long = PublishReport::default();
        long.failed
            .push(format!("{}\u{e9}", "y".repeat(FAILURE_NOTICE_MAX_BYTES)));
        let notice = failure_notice(&long);
        assert!(notice.ends_with("..."), "{notice}");
        assert!(notice.len() <= FAILURE_NOTICE_MAX_BYTES);
        assert!(failure_notice(&PublishReport::default()).contains("unknown reason"));
    }

    #[test]
    fn body_line_escapes_markdown_in_filenames() {
        let item = published(MediaKind::File, "notes](x)[.txt", None);
        assert_eq!(
            body_line(&item),
            format!("[notes\\]\\(x\\)\\[.txt]({})", item.descriptor.url)
        );
    }
}
