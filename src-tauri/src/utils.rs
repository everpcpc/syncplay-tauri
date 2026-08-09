use regex::Regex;
use sha2::{Digest, Sha256};
use url::Url;

use crate::config::PrivacyMode;
use crate::network::messages::FileSizeInfo;

pub const PRIVACY_HIDDEN_FILENAME: &str = "**Hidden filename**";
pub const MUSIC_FORMATS: [&str; 8] = [
    ".mp3", ".m4a", ".m4p", ".wav", ".aiff", ".r", ".ogg", ".flac",
];

pub fn truncate_text(value: &str, max_len: usize) -> String {
    value.chars().take(max_len).collect()
}

pub fn is_music_file(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    MUSIC_FORMATS.iter().any(|ext| lower.ends_with(ext))
}

pub fn is_url(value: &str) -> bool {
    if !value.contains("://") {
        return false;
    }
    Url::parse(value).is_ok()
}

pub fn is_trustable_and_trusted(
    value: &str,
    trusted_domains: &[String],
    only_switch_to_trusted: bool,
) -> (bool, bool) {
    let url = match Url::parse(value) {
        Ok(url) => url,
        Err(_) => return (false, false),
    };

    let scheme = url.scheme();
    let trustable = scheme == "http" || scheme == "https";
    if !trustable {
        return (false, false);
    }

    if !only_switch_to_trusted {
        return (true, true);
    }

    let host = match url.host_str() {
        Some(host) => host,
        None => return (true, false),
    };

    for entry in trusted_domains {
        let mut parts = entry.splitn(2, '/');
        let domain = parts.next().unwrap_or("").trim();
        if domain.is_empty() {
            continue;
        }
        let path = parts.next().unwrap_or("").trim();

        let mut domain_match = false;
        if domain.contains('*') {
            let regex_pattern = format!("^{}$", regex::escape(domain).replace("\\*", "([^.]+)"));
            if let Ok(regex) = Regex::new(&regex_pattern) {
                domain_match = regex.is_match(host);
            }
        } else if host.eq_ignore_ascii_case(domain)
            || host.eq_ignore_ascii_case(&format!("www.{}", domain))
        {
            domain_match = true;
        }

        if !domain_match {
            continue;
        }

        if path.is_empty() {
            return (true, true);
        }

        let path_prefix = format!("/{}", path);
        if url.path().starts_with(&path_prefix) {
            return (true, true);
        }
    }

    (true, false)
}

pub fn strip_filename(filename: &str, strip_url: bool) -> String {
    // The reference client percent-decodes before selecting the final URL
    // segment, then decodes that segment once more. Preserve that order so
    // privacy hashes remain interoperable for encoded and double-encoded names.
    let decoded = urlencoding::decode(filename)
        .map(|value| value.into_owned())
        .unwrap_or_else(|_| filename.to_string());
    let selected = if strip_url || is_url(filename) {
        decoded.rsplit('/').next().unwrap_or(&decoded)
    } else {
        &decoded
    };
    let base = urlencoding::decode(selected)
        .map(|value| value.into_owned())
        .unwrap_or_else(|_| selected.to_string());
    let regex = Regex::new(r"[-~_\.\[\](): ]").expect("invalid filename regex");
    regex.replace_all(&base, "").to_string()
}

pub fn hash_filename(filename: &str, strip_url: bool) -> String {
    let stripped = strip_filename(filename, strip_url);
    let mut hasher = Sha256::new();
    hasher.update(stripped.as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{:x}", digest);
    hex.chars().take(12).collect()
}

pub fn hash_filesize(size: u64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(size.to_string().as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{:x}", digest);
    hex.chars().take(12).collect()
}

pub fn apply_privacy(
    filename: Option<String>,
    filesize: Option<u64>,
    filename_mode: &PrivacyMode,
    filesize_mode: &PrivacyMode,
) -> (Option<String>, Option<FileSizeInfo>) {
    let name = match (filename, filename_mode) {
        (Some(name), PrivacyMode::SendRaw) => Some(name),
        (Some(name), PrivacyMode::SendHashed) => Some(hash_filename(&name, true)),
        (Some(_), PrivacyMode::DoNotSend) => Some(PRIVACY_HIDDEN_FILENAME.to_string()),
        (None, _) => None,
    };

    let size = match (filesize, filesize_mode) {
        (Some(size), PrivacyMode::SendRaw) => Some(FileSizeInfo::Number(size)),
        (Some(size), PrivacyMode::SendHashed) => Some(FileSizeInfo::Text(hash_filesize(size))),
        (Some(_), PrivacyMode::DoNotSend) => Some(FileSizeInfo::Number(0)),
        (None, _) => None,
    };

    (name, size)
}

pub fn same_filename(a: Option<&str>, b: Option<&str>) -> bool {
    let a = match a {
        Some(value) => value,
        None => return false,
    };
    let b = match b {
        Some(value) => value,
        None => return false,
    };

    if a == PRIVACY_HIDDEN_FILENAME || b == PRIVACY_HIDDEN_FILENAME {
        return true;
    }

    if a.eq_ignore_ascii_case(b) {
        return true;
    }

    let a_stripped = strip_filename(a, is_url(a) ^ is_url(b));
    let b_stripped = strip_filename(b, is_url(a) ^ is_url(b));
    if a_stripped == b_stripped {
        return true;
    }

    let a_hash = hash_filename(a, is_url(a) ^ is_url(b));
    let b_hash = hash_filename(b, is_url(a) ^ is_url(b));
    a_stripped == b_hash || a_hash == b_stripped || a_hash == b_hash
}

pub fn same_filesize(a: Option<&FileSizeInfo>, b: Option<&FileSizeInfo>) -> bool {
    let (Some(a), Some(b)) = (a, b) else {
        return false;
    };

    let (a_number, a_text) = match a {
        FileSizeInfo::Number(value) => (Some(*value), None),
        FileSizeInfo::Text(value) => (None, Some(value.as_str())),
    };
    let (b_number, b_text) = match b {
        FileSizeInfo::Number(value) => (Some(*value), None),
        FileSizeInfo::Text(value) => (None, Some(value.as_str())),
    };

    if let (Some(a_raw), Some(b_raw)) = (a_number, b_number) {
        if a_raw == 0 || b_raw == 0 {
            return true;
        }
        if a_raw == b_raw {
            return true;
        }
    }

    let a_hash = match (a_number, a_text) {
        (Some(value), _) => hash_filesize(value),
        (None, Some(text)) => text.to_string(),
        _ => String::new(),
    };
    let b_hash = match (b_number, b_text) {
        (Some(value), _) => hash_filesize(value),
        (None, Some(text)) => text.to_string(),
        _ => String::new(),
    };

    if a_hash.is_empty() || b_hash.is_empty() {
        return false;
    }

    a_hash == b_hash
}

#[allow(dead_code)]
pub fn parse_player_arguments(value: &str) -> Vec<String> {
    if value.trim().is_empty() {
        return Vec::new();
    }
    shell_words::split(value)
        .unwrap_or_else(|_| value.split_whitespace().map(|s| s.to_string()).collect())
}

pub fn strip_control_password(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect::<String>()
        .to_uppercase()
}

pub fn parse_controlled_room_input(room: &str) -> (String, Option<String>) {
    if !room.starts_with('+') {
        return (room.to_string(), None);
    }
    let parts: Vec<&str> = room.split(':').collect();
    if parts.len() < 3 {
        return (room.to_string(), None);
    }
    let normalized_room = format!("{}:{}", parts[0], parts[1]);
    let password = strip_control_password(parts[2]);
    let password = if password.is_empty() {
        None
    } else {
        Some(password)
    };
    (normalized_room, password)
}

pub fn is_controlled_room(room: &str) -> bool {
    if !room.starts_with('+') {
        return false;
    }
    let parts: Vec<&str> = room.split(':').collect();
    if parts.len() != 2 {
        return false;
    }
    let hash = parts[1];
    if hash.len() != 12 {
        return false;
    }
    hash.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn version_meets_min(version: &str, min_version: &str) -> bool {
    fn parse_parts(value: &str) -> Vec<u32> {
        let sanitized: String = value
            .chars()
            .map(|c| if c.is_ascii_digit() { c } else { '.' })
            .collect();
        sanitized
            .split('.')
            .filter(|part| !part.is_empty())
            .map(|part| part.parse::<u32>().unwrap_or(0))
            .collect()
    }

    let current = parse_parts(version);
    let minimum = parse_parts(min_version);
    let max_len = current.len().max(minimum.len());
    for idx in 0..max_len {
        let a = *current.get(idx).unwrap_or(&0);
        let b = *minimum.get(idx).unwrap_or(&0);
        if a > b {
            return true;
        }
        if a < b {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_filename() {
        let hashed = hash_filename("Movie File.mp4", true);
        assert_eq!(hashed.len(), 12);
    }

    #[test]
    fn hash_filename_matches_reference_for_percent_encoded_urls() {
        assert_eq!(
            hash_filename("https://example.com/My%20Movie.mkv", true),
            "a9fc3f97cce9"
        );
        assert_eq!(
            hash_filename("https://example.com/%E6%B5%8B%E8%AF%95.mkv", true),
            "eb7701c65986"
        );
        assert_eq!(
            hash_filename("https://example.com/My%2520Movie.mkv", true),
            "a9fc3f97cce9"
        );
    }

    #[test]
    fn test_same_filename_hidden() {
        assert!(same_filename(Some(PRIVACY_HIDDEN_FILENAME), Some("foo")));
    }

    #[test]
    fn test_same_filename_hash_match() {
        let name = "Movie File.mp4";
        let hashed = hash_filename(name, true);
        assert!(same_filename(Some(name), Some(&hashed)));
    }

    #[test]
    fn test_parse_player_arguments() {
        let args = parse_player_arguments("--foo bar --baz=1");
        assert_eq!(args, vec!["--foo", "bar", "--baz=1"]);
    }

    #[test]
    fn truncate_text_counts_unicode_code_points() {
        assert_eq!(truncate_text("hello", 3), "hel");
        assert_eq!(truncate_text("测试用户名", 4), "测试用户");
        assert_eq!(truncate_text("😀🚀✨", 2), "😀🚀");
        assert_eq!(truncate_text("anything", 0), "");
    }

    #[test]
    fn controlled_room_password_is_parsed_from_complete_input() {
        let (room, password) = parse_controlled_room_input(
            "+abcdefghijklmnopqrstuvwxyz0123456789:123456789012:AB-123-456",
        );

        assert_eq!(room, "+abcdefghijklmnopqrstuvwxyz0123456789:123456789012");
        assert_eq!(password.as_deref(), Some("AB-123-456"));
    }
}
