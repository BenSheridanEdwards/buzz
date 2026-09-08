//! Discovery of locally installed Hermes Agent profiles.
//!
//! Hermes keeps one directory per profile under `~/.hermes/profiles/<slug>/`.
//! A profile carries a `SOUL.md` (identity prompt), a `config.yaml`, and an
//! optional `avatar.*` image. Selecting a profile for a spawned `hermes-acp`
//! process is done purely through the environment: `HERMES_HOME` points at the
//! profile directory. This module only reads enough of a profile to populate
//! the Desktop's "Hermes profile" picker; it never writes into `~/.hermes`.

use std::io::Read;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde::Serialize;

/// Upper bound on the bytes read from a `SOUL.md`. Only the heading and the
/// first few lines matter; a runaway prompt file must not be slurped whole.
const MAX_SOUL_READ_BYTES: u64 = 64 * 1024;

/// Largest avatar data URL handed to the picker.
///
/// A picked avatar becomes the persona's `avatar_url`, which fans out into the
/// linked agents' kind:0 `picture` and the persona catalog head. The catalog
/// accepts inline raster avatars up to 256 KiB
/// (`MAX_INLINE_RASTER_AVATAR_LENGTH` in `persona_catalog.rs`) and the relay
/// rejects any event content over 256 KiB, so anything larger cannot be stored
/// inline anywhere downstream. The Desktop uploads a picked avatar to relay
/// media before saving, but the data URL still crosses IPC and lives in the
/// form draft, so it is bounded here too.
const MAX_AVATAR_DATA_URL_BYTES: usize = 256 * 1024;

/// Raw avatar bytes that still fit under [`MAX_AVATAR_DATA_URL_BYTES`] once
/// base64 expands them by 4/3 and the `data:image/jpeg;base64,` header is
/// added.
const MAX_AVATAR_INLINE_BYTES: u64 =
    ((MAX_AVATAR_DATA_URL_BYTES - MAX_AVATAR_HEADER_BYTES) / 4 * 3) as u64;

/// Slack for the longest `data:<mime>;base64,` prefix in [`AVATAR_CANDIDATES`].
const MAX_AVATAR_HEADER_BYTES: usize = 64;

/// Hard cap on listed profiles so a pathological directory cannot balloon the
/// IPC payload (each entry may carry an inlined avatar). Applied *before* the
/// avatars are read, so the cap bounds the disk work as well as the payload.
const MAX_PROFILES: usize = 200;

/// Longest display name accepted from a SOUL heading before falling back to
/// the directory slug.
const MAX_NAME_CHARS: usize = 64;

/// Description cap; matches `MAX_AGENT_DESCRIPTION_CHARS` on the persona form.
const MAX_DESCRIPTION_CHARS: usize = 280;

/// Avatar file candidates, first match wins.
const AVATAR_CANDIDATES: &[(&str, &str)] = &[
    ("avatar.jpg", "image/jpeg"),
    ("avatar.jpeg", "image/jpeg"),
    ("avatar.png", "image/png"),
    ("avatar.webp", "image/webp"),
    ("avatar.gif", "image/gif"),
];

/// Display name for a Hermes home that is itself a profile but names nobody.
const DEFAULT_HOME_NAME: &str = "Default (~/.hermes)";

/// Titles that end in a period without ending the sentence, so `You are
/// Dr. Who` keeps its surname.
const NAME_ABBREVIATIONS: &[&str] = &[
    "Dr", "Mr", "Mrs", "Ms", "Mx", "Prof", "Sr", "Jr", "St", "Fr", "Rev", "Capt", "Sgt", "Lt",
    "Col", "Gen",
];

/// One Hermes profile as presented to the Desktop picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HermesProfile {
    /// Directory name under the profiles root.
    pub slug: String,
    /// Display name from the SOUL heading, or a title-cased slug.
    pub name: String,
    /// Short description from a `Title:` line in the SOUL, when present.
    pub description: Option<String>,
    /// Absolute profile directory; becomes `HERMES_HOME` on the spawned agent.
    pub path: String,
    /// Inline `data:image/...;base64,` avatar when the profile ships one.
    pub avatar_data_url: Option<String>,
}

/// Default Hermes home: `$HOME/.hermes`.
pub fn default_hermes_home_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".hermes"))
}

/// Every profile offered for a Hermes home: the profiles under
/// `<home>/profiles`, plus the home itself when it carries a SOUL or config of
/// its own (Hermes runs happily with no `profiles/` directory at all, and those
/// users must not face an empty picker).
///
/// A missing profiles root is not an error — there is simply nothing to offer.
/// Any other failure to read it propagates so the picker can say the scan
/// failed instead of claiming there are no profiles.
pub fn list_hermes_profiles_for_home(home: &Path) -> std::io::Result<Vec<HermesProfile>> {
    let mut profiles = scan_hermes_profiles(&home.join("profiles"))?;
    if let Some(home_profile) = hermes_home_profile(home) {
        // The home is the fallback selection, so it leads the list rather than
        // sorting in among the named profiles.
        profiles.insert(0, home_profile);
        profiles.truncate(MAX_PROFILES);
    }
    Ok(profiles)
}

/// The Hermes home itself as a picker entry, when it holds a `SOUL.md` or a
/// `config.yaml`. Returns `None` for a home that only contains `profiles/`.
pub fn hermes_home_profile(home: &Path) -> Option<HermesProfile> {
    let meta = read_profile_meta(home, ProfileKind::Home)?;
    Some(meta.into_profile())
}

/// Scan `root` for Hermes profiles.
///
/// A directory counts as a profile when it holds a `SOUL.md` or a
/// `config.yaml`; anything else (caches, stray files, dot-directories) is
/// skipped. Results are sorted by display name, then slug, and capped at
/// [`MAX_PROFILES`] before any avatar is read.
///
/// A missing root yields an empty list. Every other `read_dir` failure — an
/// unreadable root, a root that is a plain file — is returned as an error:
/// reporting "no profiles found" for a directory we could not read would be a
/// terminal failure dressed up as an authoritative empty answer.
pub fn scan_hermes_profiles(root: &Path) -> std::io::Result<Vec<HermesProfile>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };

    let mut metas: Vec<ProfileMeta> = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                if let Some(meta) = read_profile_meta(&entry.path(), ProfileKind::Child) {
                    metas.push(meta);
                }
            }
            Err(error) => {
                tracing::warn!("hermes_profiles: skipping unreadable entry in {root:?}: {error}");
            }
        }
    }

    metas.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.slug.cmp(&b.slug))
    });
    // Truncate before touching avatars: the cap must bound the disk reads and
    // the encoded payload, not just the length of the returned list.
    metas.truncate(MAX_PROFILES);
    Ok(metas.into_iter().map(ProfileMeta::into_profile).collect())
}

/// Whether a candidate directory is the Hermes home or one of its children.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProfileKind {
    /// `~/.hermes` itself; its leading dot is part of the name, not a marker
    /// for a hidden directory to skip.
    Home,
    /// A directory under `<home>/profiles`.
    Child,
}

/// Everything about a profile except its avatar, which is read only for the
/// entries that survive the [`MAX_PROFILES`] cap.
struct ProfileMeta {
    dir: PathBuf,
    slug: String,
    name: String,
    description: Option<String>,
    path: String,
}

impl ProfileMeta {
    fn into_profile(self) -> HermesProfile {
        let avatar_data_url = read_avatar_data_url(&self.dir);
        HermesProfile {
            slug: self.slug,
            name: self.name,
            description: self.description,
            path: self.path,
            avatar_data_url,
        }
    }
}

fn read_profile_meta(dir: &Path, kind: ProfileKind) -> Option<ProfileMeta> {
    if !dir.is_dir() {
        return None;
    }
    let slug = dir.file_name()?.to_str()?.to_string();
    if kind == ProfileKind::Child && slug.starts_with('.') {
        return None;
    }
    let soul_path = dir.join("SOUL.md");
    let has_soul = soul_path.is_file();
    if !has_soul && !dir.join("config.yaml").is_file() {
        // A directory we cannot even open looks identical to a directory that
        // is simply not a profile. Say so once, so support has something to
        // look at when a profile "disappears" from the picker.
        if let Err(error) = std::fs::read_dir(dir) {
            tracing::warn!("hermes_profiles: skipping unreadable profile dir {dir:?}: {error}");
        }
        return None;
    }
    // A non-UTF-8 path cannot travel through an env var string; skip it.
    let path = dir.to_str()?.to_string();

    let soul = if has_soul {
        read_bounded(&soul_path, MAX_SOUL_READ_BYTES)
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    } else {
        None
    };
    let name = soul
        .as_deref()
        .and_then(soul_display_name)
        .unwrap_or_else(|| match kind {
            ProfileKind::Home => DEFAULT_HOME_NAME.to_string(),
            ProfileKind::Child => slug_display_name(&slug),
        });
    let description = soul.as_deref().and_then(soul_description);

    Some(ProfileMeta {
        dir: dir.to_path_buf(),
        slug,
        name,
        description,
        path,
    })
}

fn read_bounded(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn read_avatar_data_url(dir: &Path) -> Option<String> {
    AVATAR_CANDIDATES.iter().find_map(|(file_name, mime)| {
        let candidate = dir.join(file_name);
        let metadata = std::fs::metadata(&candidate).ok()?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_AVATAR_INLINE_BYTES {
            return None;
        }
        // Bound the read as well: the size check above is advisory when the
        // file changes between stat and read.
        let bytes = read_bounded(&candidate, MAX_AVATAR_INLINE_BYTES).ok()?;
        if bytes.is_empty() {
            return None;
        }
        let data_url = format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        );
        // Belt and braces: whatever the arithmetic above says, nothing over the
        // downstream limit leaves this module.
        (data_url.len() <= MAX_AVATAR_DATA_URL_BYTES).then_some(data_url)
    })
}

/// Iterate the SOUL's lines outside fenced code blocks.
///
/// A fenced block is prose about code, not identity: a `# Title` or `Title:`
/// line inside one belongs to the sample, not to the profile.
fn soul_lines(soul: &str) -> impl Iterator<Item = &str> {
    let mut in_fence = false;
    soul.lines().filter_map(move |line| {
        let trimmed = line.trim_start_matches('\u{feff}').trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            return None;
        }
        (!in_fence).then_some(trimmed)
    })
}

/// The text of an ATX heading (`#` through `######` followed by a space), or
/// `None` for any other line. `#hashtag` is a hashtag, not a heading.
fn atx_heading(line: &str) -> Option<&str> {
    let hashes = line.len() - line.trim_start_matches('#').len();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &line[hashes..];
    if !rest.is_empty() && !rest.starts_with(|character: char| character.is_whitespace()) {
        return None;
    }
    Some(rest.trim())
}

/// Display name parsed from a SOUL document.
///
/// The first Markdown heading wins after a `SOUL.md`/`SOUL` label and any
/// dash or colon separator are stripped (a `SOUL.md`, dash, `Bond` heading
/// yields `Bond`).
/// A bare `# SOUL` heading carries no name, so the first `You are <Name>`
/// sentence is tried next (`You are **Formula**.` yields `Formula`).
/// Returns `None` when neither form produces a usable name.
pub fn soul_display_name(soul: &str) -> Option<String> {
    let heading = soul_lines(soul).find_map(atx_heading);
    if let Some(name) = heading.and_then(name_from_heading) {
        return Some(name);
    }
    soul_lines(soul).find_map(name_from_you_are_line)
}

fn name_from_heading(heading: &str) -> Option<String> {
    let mut rest = heading.trim();
    for label in ["SOUL.md", "SOUL.MD", "soul.md", "SOUL", "Soul", "soul"] {
        let Some(stripped) = rest.strip_prefix(label) else {
            continue;
        };
        let trimmed = stripped.trim_start();
        // `Soulless` is one word, not the label plus a name.
        if stripped.starts_with(|next: char| next.is_alphanumeric()) {
            continue;
        }
        // `SOUL.md — Bond` labels a name; `Soul of Bond` *is* the name. Only a
        // separator (or nothing at all) turns the leading word into a label.
        let labels_a_name = trimmed.is_empty()
            || trimmed.starts_with(|next: char| {
                matches!(next, '\u{2014}' | '\u{2013}' | '-' | ':' | '*')
            });
        if labels_a_name {
            rest = stripped;
            break;
        }
    }
    let rest = rest.trim_matches(|character: char| {
        character.is_whitespace() || matches!(character, '\u{2014}' | '\u{2013}' | '-' | ':' | '*')
    });
    clean_name(rest)
}

fn name_from_you_are_line(line: &str) -> Option<String> {
    let rest = line.strip_prefix("You are ")?;
    let end = sentence_end(rest);
    let candidate =
        rest[..end].trim_matches(|character: char| character.is_whitespace() || character == '*');
    // "You are a helpful assistant" is a role, not a name.
    let first = candidate.split_whitespace().next()?;
    if matches!(first, "a" | "an" | "the" | "not") {
        return None;
    }
    clean_name(candidate)
}

/// Byte offset of the first character that ends the name clause.
///
/// A period that closes a known title (`Dr.`, `Prof.`) does not end it, so
/// `You are Dr. Who` keeps both words.
fn sentence_end(rest: &str) -> usize {
    let mut search = 0;
    while let Some(offset) = rest[search..].find([',', '.', ';', ':', '!', '\n']) {
        let index = search + offset;
        if rest.as_bytes()[index] == b'.' && ends_abbreviation(&rest[..index]) {
            search = index + 1;
            continue;
        }
        return index;
    }
    rest.len()
}

fn ends_abbreviation(prefix: &str) -> bool {
    let word = prefix
        .rsplit(|character: char| character.is_whitespace())
        .next()
        .unwrap_or("")
        .trim_matches('*');
    NAME_ABBREVIATIONS.contains(&word)
}

/// Characters that carry no visible identity: controls plus the format
/// (`Cf`) characters that make `Bond<ZWSP>Zed` render as `BondZed`.
fn is_ignorable_name_char(character: char) -> bool {
    character.is_control()
        || matches!(character, '\u{00ad}' | '\u{180e}' | '\u{feff}')
        || ('\u{200b}'..='\u{200f}').contains(&character)
        || ('\u{202a}'..='\u{202e}').contains(&character)
        || ('\u{2060}'..='\u{2064}').contains(&character)
        || ('\u{2066}'..='\u{2069}').contains(&character)
}

fn clean_name(candidate: &str) -> Option<String> {
    let cleaned: String = candidate
        .chars()
        .filter(|character| !is_ignorable_name_char(*character))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.is_empty() || cleaned.chars().count() > MAX_NAME_CHARS {
        return None;
    }
    // A binary or otherwise non-UTF-8 SOUL decodes to replacement characters;
    // a name made of those (or of punctuation alone) is noise, not identity.
    if cleaned.contains('\u{fffd}') || !cleaned.chars().any(char::is_alphanumeric) {
        return None;
    }
    Some(cleaned)
}

/// Short description parsed from a `**Title:** ...`, `**Title**: ...` or
/// `Title: ...` line outside any code fence.
pub fn soul_description(soul: &str) -> Option<String> {
    soul_lines(soul).find_map(|line| {
        let stripped = line.trim_start_matches('*').trim_start();
        let rest = stripped
            .strip_prefix("Title:")
            .or_else(|| stripped.strip_prefix("Title**:"))?;
        let rest = rest.trim_start_matches('*').trim();
        let cleaned: String = rest
            .chars()
            .filter(|character| !is_ignorable_name_char(*character))
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if cleaned.is_empty() {
            return None;
        }
        Some(cleaned.chars().take(MAX_DESCRIPTION_CHARS).collect())
    })
}

/// Title-case a slug: `devil-iris` becomes `Devil Iris`.
pub fn slug_display_name(slug: &str) -> String {
    slug.split(|character: char| character == '-' || character == '_' || character.is_whitespace())
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, contents: &[u8]) {
        std::fs::create_dir_all(dir).expect("mkdir");
        std::fs::write(dir.join(name), contents).expect("write");
    }

    fn scan(root: &Path) -> Vec<HermesProfile> {
        scan_hermes_profiles(root).expect("scan")
    }

    #[test]
    fn missing_root_yields_empty_list() {
        let temp = tempfile::tempdir().expect("tempdir");
        let missing = temp.path().join("does-not-exist");
        assert!(scan_hermes_profiles(&missing).expect("scan").is_empty());
    }

    #[test]
    fn unreadable_root_is_an_error_not_an_empty_list() {
        let temp = tempfile::tempdir().expect("tempdir");
        // A plain file is an unreadable "directory" on every platform, so this
        // case does not depend on POSIX permission bits.
        let file_root = temp.path().join("root-is-a-file");
        std::fs::write(&file_root, b"not a directory").expect("write");
        assert!(scan_hermes_profiles(&file_root).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn permission_denied_root_is_an_error() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("locked");
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        let readable = std::fs::read_dir(&root).is_ok();
        let result = scan_hermes_profiles(&root);
        // Restore before asserting so the tempdir can always be cleaned up.
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        // Running as root defeats the permission bits; the assertion only means
        // something when the OS actually refused the read.
        if !readable {
            assert!(result.is_err());
        }
    }

    #[test]
    fn scans_profiles_with_soul_and_avatar() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let bond = root.join("bond");
        write(
            &bond,
            "SOUL.md",
            b"# SOUL.md \xe2\x80\x94 Bond\n\n**Title:** Executor of the Fleet\n\n## Identity\nYou are Bond.\n",
        );
        write(&bond, "config.yaml", b"model: x\n");
        write(&bond, "avatar.jpg", &[0xFF, 0xD8, 0xFF, 0xE0]);

        let profiles = scan(root);
        assert_eq!(profiles.len(), 1);
        let profile = &profiles[0];
        assert_eq!(profile.slug, "bond");
        assert_eq!(profile.name, "Bond");
        assert_eq!(
            profile.description.as_deref(),
            Some("Executor of the Fleet")
        );
        assert_eq!(profile.path, bond.to_str().expect("utf8 path"));
        assert_eq!(
            profile.avatar_data_url.as_deref(),
            Some("data:image/jpeg;base64,/9j/4A==")
        );
    }

    #[test]
    fn profile_without_soul_falls_back_to_slug_name() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(&root.join("devil-iris"), "config.yaml", b"model: x\n");

        let profiles = scan(root);
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "Devil Iris");
        assert_eq!(profiles[0].description, None);
        assert_eq!(profiles[0].avatar_data_url, None);
    }

    #[test]
    fn skips_non_profile_entries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(&root.join("real"), "SOUL.md", b"# Real\n");
        std::fs::create_dir_all(root.join("empty-dir")).expect("mkdir");
        std::fs::create_dir_all(root.join(".hidden")).expect("mkdir");
        write(&root.join(".hidden"), "SOUL.md", b"# Hidden\n");
        std::fs::write(root.join("stray.txt"), b"nope").expect("write");

        let slugs: Vec<_> = scan(root).into_iter().map(|profile| profile.slug).collect();
        assert_eq!(slugs, vec!["real".to_string()]);
    }

    #[test]
    fn sorts_by_display_name_case_insensitively() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(&root.join("zed"), "SOUL.md", b"# alpha\n");
        write(&root.join("amy"), "SOUL.md", b"# Bravo\n");
        write(&root.join("carl"), "config.yaml", b"");

        let names: Vec<_> = scan(root).into_iter().map(|profile| profile.name).collect();
        assert_eq!(names, vec!["alpha", "Bravo", "Carl"]);
    }

    #[test]
    fn caps_the_listed_profiles_before_reading_avatars() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        // Names sort as p000..p209, so the last ten are the ones dropped.
        for index in 0..(MAX_PROFILES + 10) {
            let dir = root.join(format!("p{index:03}"));
            write(&dir, "SOUL.md", format!("# P{index:03}\n").as_bytes());
            write(&dir, "avatar.png", &[0x89, 0x50, 0x4E, 0x47]);
        }
        let profiles = scan(root);
        assert_eq!(profiles.len(), MAX_PROFILES);
        assert_eq!(profiles[0].slug, "p000");
        assert_eq!(
            profiles[MAX_PROFILES - 1].slug,
            format!("p{:03}", MAX_PROFILES - 1)
        );
        // Every entry that survived the cap still carries its avatar; the ten
        // that did not are never read, because the avatar is only fetched once
        // the entry is known to be in the returned window.
        assert!(profiles
            .iter()
            .all(|profile| profile.avatar_data_url.is_some()));
    }

    #[test]
    fn oversized_avatar_is_not_inlined() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let dir = root.join("big");
        write(&dir, "SOUL.md", b"# Big\n");
        let file = std::fs::File::create(dir.join("avatar.png")).expect("create");
        file.set_len(MAX_AVATAR_INLINE_BYTES + 1).expect("set_len");
        drop(file);

        assert_eq!(scan(root)[0].avatar_data_url, None);
    }

    #[test]
    fn inlined_avatar_fits_the_downstream_event_limit() {
        // 256 KiB of raw JPEG-ish bytes encodes to ~350 KB, which the relay
        // rejects; the picker must not offer it at all.
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let dir = root.join("chunky");
        write(&dir, "SOUL.md", b"# Chunky\n");
        write(&dir, "avatar.jpg", &vec![0xAB; MAX_AVATAR_DATA_URL_BYTES]);
        assert_eq!(scan(root)[0].avatar_data_url, None);

        // The largest avatar that IS inlined still fits the 256 KiB budget.
        let ok = root.join("slim");
        write(&ok, "SOUL.md", b"# Slim\n");
        write(
            &ok,
            "avatar.jpg",
            &vec![0xAB; MAX_AVATAR_INLINE_BYTES as usize],
        );
        let data_url = scan(root)
            .into_iter()
            .find(|profile| profile.slug == "slim")
            .expect("slim")
            .avatar_data_url
            .expect("inlined");
        assert!(
            data_url.len() <= MAX_AVATAR_DATA_URL_BYTES,
            "data URL of {} bytes exceeds the {MAX_AVATAR_DATA_URL_BYTES} byte budget",
            data_url.len()
        );
    }

    #[test]
    fn avatar_candidates_prefer_first_match_and_mime() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let dir = root.join("png-only");
        write(&dir, "SOUL.md", b"# Png\n");
        write(&dir, "avatar.png", &[0x89, 0x50, 0x4E, 0x47]);

        assert_eq!(
            scan(root)[0].avatar_data_url.as_deref(),
            Some("data:image/png;base64,iVBORw==")
        );
    }

    #[test]
    fn hermes_home_itself_is_offered_when_it_is_a_profile() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join(".hermes");
        write(&home, "config.yaml", b"model: x\n");
        write(&home.join("profiles").join("bond"), "SOUL.md", b"# Bond\n");

        let profiles = list_hermes_profiles_for_home(&home).expect("list");
        assert_eq!(
            profiles
                .iter()
                .map(|profile| profile.name.as_str())
                .collect::<Vec<_>>(),
            vec![DEFAULT_HOME_NAME, "Bond"]
        );
        assert_eq!(profiles[0].path, home.to_str().expect("utf8 path"));
        assert_eq!(profiles[0].slug, ".hermes");
    }

    #[test]
    fn hermes_home_uses_its_own_soul_name() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join(".hermes");
        write(&home, "SOUL.md", b"# Sky\n");

        let profiles = list_hermes_profiles_for_home(&home).expect("list");
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "Sky");
    }

    #[test]
    fn hermes_home_without_a_profile_is_not_offered() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join(".hermes");
        write(&home.join("profiles").join("bond"), "SOUL.md", b"# Bond\n");

        let profiles = list_hermes_profiles_for_home(&home).expect("list");
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "Bond");
    }

    #[test]
    fn soul_name_parsing_table() {
        let cases: &[(&str, Option<&str>)] = &[
            ("# SOUL.md \u{2014} Bond\n", Some("Bond")),
            ("# SOUL.md - Echo\n", Some("Echo")),
            ("# SOUL.md: Irene\n", Some("Irene")),
            ("# Devil Iris\n\nYou are Devil Iris.\n", Some("Devil Iris")),
            ("# SOUL\n\nYou are **Formula**.\n", Some("Formula")),
            ("# SOUL\n\nYou are Elon Musk.\n", Some("Elon Musk")),
            ("# SOUL.md\n\nYou are Bond, the executor.\n", Some("Bond")),
            ("# SOUL\n\nYou are a helpful assistant.\n", None),
            ("\u{feff}# SOUL.md \u{2013} Sky\n", Some("Sky")),
            ("## Soulless\n", Some("Soulless")),
            ("no heading at all\n", None),
            ("", None),
            // A hashtag is not an ATX heading: `#` needs a space after it.
            ("#hashtag first\n\n# Soul of Bond\n", Some("Soul of Bond")),
            // The label is only a label when a separator follows it.
            ("# SOUL.md \u{2014} Soul of Bond\n", Some("Soul of Bond")),
            ("# SOUL.md \u{2014} Soul of Bond\n", Some("Soul of Bond")),
            ("#hashtag only\n", None),
            // Seven hashes is not a heading either.
            ("####### Deep\n", None),
            // Zero-width and other format characters carry no identity.
            ("# Bond\u{200b}\u{200b}Zed\n", Some("BondZed")),
            ("# \u{200b}\u{200b}\u{200b}\n", None),
            // A binary SOUL decodes to replacement characters.
            ("# \u{fffd}\u{fffd}\u{fffd}\n", None),
            ("# Bond \u{fffd}\n", None),
            // Punctuation alone is noise, not a name.
            ("# ***\n", None),
            ("# ...\n", None),
            // A title that ends in a period keeps its surname.
            ("# SOUL\n\nYou are Dr. Who.\n", Some("Dr. Who")),
            ("# SOUL\n\nYou are Prof. X, the mentor.\n", Some("Prof. X")),
            // A heading inside a code fence belongs to the sample.
            (
                "```md\n# Fenced Name\n```\n\n# Real Name\n",
                Some("Real Name"),
            ),
            ("```\n# Fenced Only\n```\n", None),
        ];
        for (input, expected) in cases {
            assert_eq!(
                soul_display_name(input).as_deref(),
                *expected,
                "input: {input:?}"
            );
        }
    }

    #[test]
    fn soul_name_rejects_overlong_headings() {
        let long = format!("# {}\n", "x".repeat(MAX_NAME_CHARS + 1));
        assert_eq!(soul_display_name(&long), None);
    }

    #[test]
    fn non_utf8_soul_does_not_become_a_name() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let dir = root.join("binary");
        // A heading made of bytes that are not valid UTF-8 decodes to U+FFFD.
        let mut soul = b"# ".to_vec();
        soul.extend_from_slice(&[0xFF; 40]);
        soul.push(b'\n');
        write(&dir, "SOUL.md", &soul);

        assert_eq!(scan(root)[0].name, "Binary");
    }

    #[test]
    fn soul_description_table() {
        let cases: &[(&str, Option<&str>)] = &[
            ("# X\n**Title:** Fleet executor\n", Some("Fleet executor")),
            ("# X\nTitle: Plain   spaced\n", Some("Plain spaced")),
            ("# X\n**Title**: Bold outside\n", Some("Bold outside")),
            ("# X\n**Title:**\n", None),
            ("# X\nNo title here\n", None),
            // A Title line inside a code fence is part of the sample.
            (
                "# X\n```yaml\nTitle: Fenced\n```\nTitle: Real\n",
                Some("Real"),
            ),
            ("# X\n```yaml\nTitle: Fenced\n```\n", None),
        ];
        for (input, expected) in cases {
            assert_eq!(
                soul_description(input).as_deref(),
                *expected,
                "input: {input:?}"
            );
        }
        let long = format!("Title: {}\n", "y".repeat(MAX_DESCRIPTION_CHARS + 20));
        assert_eq!(
            soul_description(&long).map(|value| value.chars().count()),
            Some(MAX_DESCRIPTION_CHARS)
        );
    }

    #[test]
    fn slug_display_name_title_cases() {
        assert_eq!(slug_display_name("devil-iris"), "Devil Iris");
        assert_eq!(slug_display_name("sky"), "Sky");
        assert_eq!(slug_display_name("a_b__c"), "A B C");
    }

    #[test]
    fn serializes_snake_case_fields() {
        let profile = HermesProfile {
            slug: "s".into(),
            name: "N".into(),
            description: None,
            path: "/p".into(),
            avatar_data_url: None,
        };
        let json = serde_json::to_value(&profile).expect("json");
        assert_eq!(json["avatar_data_url"], serde_json::Value::Null);
        assert_eq!(json["path"], "/p");
    }
}
