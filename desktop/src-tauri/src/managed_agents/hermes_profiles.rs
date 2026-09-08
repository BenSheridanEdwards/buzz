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

/// Largest avatar inlined as a data URL. Mirrors the snapshot avatar cap so a
/// profile avatar never exceeds what the persona avatar path already accepts.
const MAX_AVATAR_INLINE_BYTES: u64 = 2 * 1024 * 1024;

/// Hard cap on listed profiles so a pathological directory cannot balloon the
/// IPC payload (each entry may carry an inlined avatar).
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

/// Default profiles root: `$HOME/.hermes/profiles`.
pub fn default_hermes_profiles_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".hermes").join("profiles"))
}

/// Scan `root` for Hermes profiles. A missing or unreadable root yields an
/// empty list rather than an error: the picker simply has nothing to offer.
///
/// A directory counts as a profile when it holds a `SOUL.md` or a
/// `config.yaml`; anything else (caches, stray files, dot-directories) is
/// skipped. Results are sorted by display name, then slug.
pub fn scan_hermes_profiles(root: &Path) -> Vec<HermesProfile> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };

    let mut profiles: Vec<HermesProfile> = entries
        .flatten()
        .filter_map(|entry| read_profile_dir(&entry.path()))
        .collect();

    profiles.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.slug.cmp(&b.slug))
    });
    profiles.truncate(MAX_PROFILES);
    profiles
}

fn read_profile_dir(dir: &Path) -> Option<HermesProfile> {
    if !dir.is_dir() {
        return None;
    }
    let slug = dir.file_name()?.to_str()?.to_string();
    if slug.starts_with('.') {
        return None;
    }
    let soul_path = dir.join("SOUL.md");
    let has_soul = soul_path.is_file();
    if !has_soul && !dir.join("config.yaml").is_file() {
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
        .unwrap_or_else(|| slug_display_name(&slug));
    let description = soul.as_deref().and_then(soul_description);
    let avatar_data_url = read_avatar_data_url(dir);

    Some(HermesProfile {
        slug,
        name,
        description,
        path,
        avatar_data_url,
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
        Some(format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ))
    })
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
    let heading = soul
        .lines()
        .map(|line| line.trim_start_matches('\u{feff}').trim())
        .find(|line| line.starts_with('#'))
        .map(|line| line.trim_start_matches('#').trim());
    if let Some(name) = heading.and_then(name_from_heading) {
        return Some(name);
    }
    soul.lines().map(str::trim).find_map(name_from_you_are_line)
}

fn name_from_heading(heading: &str) -> Option<String> {
    let mut rest = heading.trim();
    for label in ["SOUL.md", "SOUL.MD", "soul.md", "SOUL", "Soul", "soul"] {
        if let Some(stripped) = rest.strip_prefix(label) {
            // Only treat it as a label when it is a whole word.
            if stripped
                .chars()
                .next()
                .is_none_or(|next| !next.is_alphanumeric())
            {
                rest = stripped;
                break;
            }
        }
    }
    let rest = rest.trim_matches(|character: char| {
        character.is_whitespace() || matches!(character, '\u{2014}' | '\u{2013}' | '-' | ':' | '*')
    });
    clean_name(rest)
}

fn name_from_you_are_line(line: &str) -> Option<String> {
    let rest = line.strip_prefix("You are ")?;
    let end = rest
        .find([',', '.', ';', ':', '!', '\n'])
        .unwrap_or(rest.len());
    let candidate =
        rest[..end].trim_matches(|character: char| character.is_whitespace() || character == '*');
    // "You are a helpful assistant" is a role, not a name.
    let first = candidate.split_whitespace().next()?;
    if matches!(first, "a" | "an" | "the" | "not") {
        return None;
    }
    clean_name(candidate)
}

fn clean_name(candidate: &str) -> Option<String> {
    let cleaned: String = candidate
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.is_empty() || cleaned.chars().count() > MAX_NAME_CHARS {
        return None;
    }
    Some(cleaned)
}

/// Short description parsed from a `**Title:** ...` or `Title: ...` line.
pub fn soul_description(soul: &str) -> Option<String> {
    soul.lines().map(str::trim).find_map(|line| {
        let stripped = line.trim_start_matches('*').trim_start();
        let rest = stripped.strip_prefix("Title:")?;
        let rest = rest.trim_start_matches('*').trim();
        let cleaned: String = rest
            .chars()
            .filter(|character| !character.is_control())
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

    #[test]
    fn missing_root_yields_empty_list() {
        let temp = tempfile::tempdir().expect("tempdir");
        let missing = temp.path().join("does-not-exist");
        assert!(scan_hermes_profiles(&missing).is_empty());
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

        let profiles = scan_hermes_profiles(root);
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

        let profiles = scan_hermes_profiles(root);
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

        let slugs: Vec<_> = scan_hermes_profiles(root)
            .into_iter()
            .map(|profile| profile.slug)
            .collect();
        assert_eq!(slugs, vec!["real".to_string()]);
    }

    #[test]
    fn sorts_by_display_name_case_insensitively() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(&root.join("zed"), "SOUL.md", b"# alpha\n");
        write(&root.join("amy"), "SOUL.md", b"# Bravo\n");
        write(&root.join("carl"), "config.yaml", b"");

        let names: Vec<_> = scan_hermes_profiles(root)
            .into_iter()
            .map(|profile| profile.name)
            .collect();
        assert_eq!(names, vec!["alpha", "Bravo", "Carl"]);
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

        let profiles = scan_hermes_profiles(root);
        assert_eq!(profiles[0].avatar_data_url, None);
    }

    #[test]
    fn avatar_candidates_prefer_first_match_and_mime() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let dir = root.join("png-only");
        write(&dir, "SOUL.md", b"# Png\n");
        write(&dir, "avatar.png", &[0x89, 0x50, 0x4E, 0x47]);

        let profiles = scan_hermes_profiles(root);
        assert_eq!(
            profiles[0].avatar_data_url.as_deref(),
            Some("data:image/png;base64,iVBORw==")
        );
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
    fn soul_description_table() {
        let cases: &[(&str, Option<&str>)] = &[
            ("# X\n**Title:** Fleet executor\n", Some("Fleet executor")),
            ("# X\nTitle: Plain   spaced\n", Some("Plain spaced")),
            ("# X\n**Title:**\n", None),
            ("# X\nNo title here\n", None),
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
