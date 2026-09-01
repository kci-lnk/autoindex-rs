use std::{
    env,
    path::{Component, Path, PathBuf},
};

use percent_encoding::percent_decode_str;
use unicode_general_category::{GeneralCategory, get_general_category};

use cap_std::fs::{Dir, File, Metadata, OpenOptions};
use same_file::Handle;

use crate::config::{ConfigError, path_is_filesystem_root};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RequestPath {
    pub relative: PathBuf,
    pub components: Vec<String>,
    pub trailing_slash: bool,
}

pub(crate) fn validate_configured_path_lexically(path: &Path) -> Result<(), ConfigError> {
    let Some(value) = path.to_str() else {
        return Err(ConfigError::Invalid(
            "served directory path must be valid UTF-8".into(),
        ));
    };

    #[cfg(not(windows))]
    if value.contains('\\') {
        return Err(ConfigError::Invalid(
            "backslash is not allowed in a POSIX directory path".into(),
        ));
    }

    #[cfg(windows)]
    if unsafe_windows_path(value) {
        return Err(ConfigError::Invalid(
            "Windows UNC paths, device namespaces, reserved names, and trailing spaces or dots are not allowed"
                .into(),
        ));
    }

    Ok(())
}

pub(crate) fn validate_configured_root(
    root: &Path,
    allow_sensitive: bool,
) -> Result<(), ConfigError> {
    if !root.is_absolute() {
        return Err(ConfigError::Invalid(
            "served directory must resolve to an absolute path".into(),
        ));
    }
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if (path_is_filesystem_root(root) || !is_safe_visible_name(name)) && !allow_sensitive {
        return Err(ConfigError::Invalid(
            "served directory must be a visible directory below a filesystem root".into(),
        ));
    }
    if allow_sensitive {
        return Ok(());
    }
    for protected in protected_paths() {
        if paths_overlap(root, &protected) {
            return Err(ConfigError::Invalid(format!(
                "served directory overlaps protected path {}; use --allow-sensitive-paths only when this is intentional",
                protected.display()
            )));
        }
    }
    Ok(())
}

fn protected_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for name in [
        "AUTOINDEX_CONFIG_DIR",
        "FN_KNOCK_DATA_DIR",
        "FN_KNOCK_GATEWAY_CONFIG_DIR",
        "FN_KNOCK_GATEWAY_LOGS_DIR",
        "FN_KNOCK_GATEWAY_WAF_DIR",
        "FN_KNOCK_DIAGNOSTIC_LOG_DIR",
        "GO_REPROXY_DEBUG_LOG_DIR",
    ] {
        if let Some(value) = env::var_os(name).filter(|value| !value.is_empty()) {
            paths.push(PathBuf::from(value));
        }
    }
    if let Some(home) = home_dir() {
        for relative in [
            ".ssh",
            ".gnupg",
            ".aws",
            ".kube",
            ".config/fn-knock",
            ".local/share/fn-knock",
        ] {
            paths.push(home.join(relative));
        }
    }
    if cfg!(windows) {
        for name in [
            "SystemRoot",
            "WINDIR",
            "ProgramData",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramW6432",
            "APPDATA",
            "LOCALAPPDATA",
        ] {
            if let Some(value) = env::var_os(name).filter(|value| !value.is_empty()) {
                paths.push(PathBuf::from(value));
            }
        }
    } else {
        paths.extend(
            [
                "/proc",
                "/sys",
                "/dev",
                "/run",
                "/etc",
                "/boot",
                "/root",
                "/bin",
                "/sbin",
                "/usr",
                "/lib",
                "/lib64",
                "/var/lib",
                "/var/log",
                "/var/run",
                "/var/spool",
            ]
            .into_iter()
            .map(PathBuf::from),
        );
        if cfg!(target_os = "macos") {
            paths.extend(
                [
                    "/System",
                    "/Library/Keychains",
                    "/private/etc",
                    "/private/var/db",
                    "/private/var/root",
                    "/private/var/run",
                ]
                .into_iter()
                .map(PathBuf::from),
            );
        }
    }
    paths.into_iter().map(normalize_policy_path).collect()
}

fn normalize_policy_path(path: PathBuf) -> PathBuf {
    let absolute = if path.is_absolute() {
        path
    } else {
        env::current_dir().map_or(path.clone(), |directory| directory.join(path))
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
        }
    }
    std::fs::canonicalize(&normalized).unwrap_or(normalized)
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        env::var_os("HOME").map(PathBuf::from)
    }
}

fn paths_overlap(first: &Path, second: &Path) -> bool {
    path_contains(first, second) || path_contains(second, first)
}

fn path_contains(parent: &Path, child: &Path) -> bool {
    let parent = normalized_components(parent);
    let child = normalized_components(child);
    parent.len() <= child.len()
        && parent
            .iter()
            .zip(child.iter())
            .all(|(left, right)| component_equal(left, right))
}

fn normalized_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Prefix(value) => Some(value.as_os_str().to_string_lossy().to_string()),
            Component::RootDir => Some(std::path::MAIN_SEPARATOR.to_string()),
            Component::Normal(value) => Some(value.to_string_lossy().to_string()),
            Component::CurDir | Component::ParentDir => None,
        })
        .collect()
}

fn component_equal(first: &str, second: &str) -> bool {
    if cfg!(windows) || cfg!(target_os = "macos") {
        case_insensitive_component_equal(first, second)
    } else {
        first == second
    }
}

fn case_insensitive_component_equal(first: &str, second: &str) -> bool {
    caseless::canonical_caseless_match_str(first, second)
}

pub(crate) fn parse_request_path(raw_path: &str) -> Option<RequestPath> {
    if raw_path.is_empty()
        || !raw_path.starts_with('/')
        || raw_path.starts_with("//")
        || raw_path.contains('\\')
        || has_encoded_separator_or_control(raw_path)
    {
        return None;
    }
    let decoded = percent_decode_str(raw_path).decode_utf8().ok()?;
    if decoded.contains('\\')
        || decoded.chars().any(is_unsafe_character)
        || has_double_decode_hazard(&decoded)
    {
        return None;
    }
    let trailing_slash = decoded.ends_with('/');
    let content = decoded
        .strip_prefix('/')?
        .strip_suffix('/')
        .unwrap_or_else(|| decoded.strip_prefix('/').expect("prefix checked"));
    let mut components = Vec::new();
    for component in content.split('/') {
        if component.is_empty() {
            if decoded == "/" {
                continue;
            }
            return None;
        }
        if !is_safe_visible_name(component) {
            return None;
        }
        components.push(component.to_string());
    }
    if components
        .first()
        .is_some_and(|value| value.starts_with("__"))
    {
        return None;
    }
    let relative = if components.is_empty() {
        PathBuf::from(".")
    } else {
        components.iter().collect()
    };
    Some(RequestPath {
        relative,
        components,
        trailing_slash,
    })
}

pub(crate) fn is_safe_visible_name(name: &str) -> bool {
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.starts_with('.')
        || name.contains(['/', '\\', '\0'])
        || name.chars().any(is_unsafe_character)
    {
        return false;
    }
    !cfg!(windows) || is_safe_windows_name(name)
}

pub(crate) fn validate_resolved_relative(path: &Path) -> bool {
    !path.is_absolute()
        && path.components().all(|component| match component {
            Component::CurDir => true,
            Component::Normal(name) => name.to_str().is_some_and(is_safe_visible_name),
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => false,
        })
}

pub(crate) fn open_read_only(root: &Dir, path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    root.open_with(path, &options)
}

pub(crate) fn open_validated_regular(root: &Dir, requested: &Path) -> Option<(File, Metadata)> {
    let initial_metadata = root.metadata(requested).ok()?;
    if !initial_metadata.is_file() {
        return None;
    }
    let resolved = resolve_visible_target(root, requested)?;
    let file = open_read_only(root, &resolved).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || !resolved_target_is_stable(root, &resolved, &file) {
        return None;
    }
    Some((file, metadata))
}

pub(crate) fn open_validated_directory(root: &Dir, requested: &Path) -> Option<Dir> {
    let initial_metadata = root.metadata(requested).ok()?;
    if !initial_metadata.is_dir() {
        return None;
    }
    let resolved = resolve_visible_target(root, requested)?;
    let directory = root.open_dir(&resolved).ok()?;
    if !directory.dir_metadata().ok()?.is_dir()
        || !resolved_directory_is_stable(root, &resolved, &directory)
    {
        return None;
    }
    Some(directory)
}

fn resolve_visible_target(root: &Dir, requested: &Path) -> Option<PathBuf> {
    let resolved = normalize_cap_path(root.canonicalize(requested).ok()?);
    validate_resolved_relative(&resolved).then_some(resolved)
}

fn resolved_target_is_stable(root: &Dir, resolved: &Path, opened: &File) -> bool {
    if !root
        .metadata(resolved)
        .is_ok_and(|metadata| metadata.is_file())
    {
        return false;
    }
    let Some(current_resolved) = root.canonicalize(resolved).ok().map(normalize_cap_path) else {
        return false;
    };
    if current_resolved != resolved || !validate_resolved_relative(&current_resolved) {
        return false;
    }
    let Ok(current) = open_read_only(root, &current_resolved) else {
        return false;
    };
    current.metadata().is_ok_and(|metadata| metadata.is_file())
        && same_file_handle(opened, &current)
}

fn resolved_directory_is_stable(root: &Dir, resolved: &Path, opened: &Dir) -> bool {
    if !root
        .metadata(resolved)
        .is_ok_and(|metadata| metadata.is_dir())
    {
        return false;
    }
    let Some(current_resolved) = root.canonicalize(resolved).ok().map(normalize_cap_path) else {
        return false;
    };
    if current_resolved != resolved || !validate_resolved_relative(&current_resolved) {
        return false;
    }
    let Ok(current) = root.open_dir(&current_resolved) else {
        return false;
    };
    current
        .dir_metadata()
        .is_ok_and(|metadata| metadata.is_dir())
        && same_directory_handle(opened, &current)
}

fn same_file_handle(first: &File, second: &File) -> bool {
    let first = first
        .try_clone()
        .ok()
        .and_then(|file| Handle::from_file(file.into_std()).ok());
    let second = second
        .try_clone()
        .ok()
        .and_then(|file| Handle::from_file(file.into_std()).ok());
    first.is_some() && first == second
}

pub(crate) fn same_directory_handle(first: &Dir, second: &Dir) -> bool {
    let first = first
        .try_clone()
        .ok()
        .and_then(|directory| Handle::from_file(directory.into_std_file()).ok());
    let second = second
        .try_clone()
        .ok()
        .and_then(|directory| Handle::from_file(directory.into_std_file()).ok());
    first.is_some() && first == second
}

fn normalize_cap_path(path: PathBuf) -> PathBuf {
    if path.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        path
    }
}

fn is_unsafe_character(value: char) -> bool {
    value.is_control() || get_general_category(value) == GeneralCategory::Format
}

fn is_safe_windows_name(name: &str) -> bool {
    if name.contains(['<', '>', ':', '"', '|', '?', '*']) || name.ends_with([' ', '.']) {
        return false;
    }
    let stem = name.split('.').next().unwrap_or_default().trim_end();
    let upper = stem.to_uppercase();
    if matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
    ) {
        return false;
    }
    if let Some(suffix) = upper
        .strip_prefix("COM")
        .or_else(|| upper.strip_prefix("LPT"))
    {
        return !matches!(
            suffix,
            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
        );
    }
    true
}

#[cfg(any(windows, test))]
fn unsafe_windows_path(value: &str) -> bool {
    if value != value.trim() {
        return true;
    }
    let normalized = value.replace('/', "\\");
    let uppercase = normalized.to_uppercase();
    if normalized.starts_with("\\\\")
        || uppercase.starts_with("\\\\?\\")
        || uppercase.starts_with("\\\\.\\")
        || uppercase.starts_with("\\??\\")
    {
        return true;
    }

    normalized
        .split('\\')
        .enumerate()
        .filter(|(_, component)| !component.is_empty())
        .any(|(index, component)| {
            let drive = index == 0
                && component.len() == 2
                && component.as_bytes()[0].is_ascii_alphabetic()
                && component.as_bytes()[1] == b':';
            !drive && !is_safe_windows_name(component)
        })
}

fn has_encoded_separator_or_control(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return true;
        }
        let Some(high) = hex(bytes[index + 1]) else {
            return true;
        };
        let Some(low) = hex(bytes[index + 2]) else {
            return true;
        };
        let decoded = high << 4 | low;
        if matches!(decoded, b'/' | b'\\' | 0..=0x1f | 0x7f) {
            return true;
        }
        index += 3;
    }
    false
}

fn has_double_decode_hazard(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return false;
        }
        let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) else {
            return false;
        };
        if matches!(
            high << 4 | low,
            b'/' | b'\\' | b'%' | b'.' | 0..=0x1f | 0x7f
        ) {
            return true;
        }
        index += 3;
    }
    false
}

fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn request_paths_decode_once_and_reject_unsafe_components() {
        let parsed = parse_request_path("/docs/a%20b.txt").unwrap();
        assert_eq!(parsed.components, ["docs", "a b.txt"]);
        for path in [
            "//host/path",
            "/.env",
            "/../secret",
            "/%2e%2e/secret",
            "/a%2fb",
            "/a%5cb",
            "/a%252fb",
            "/%252e%252e/secret",
            "/__private/file",
            "/a//b",
            "/a//",
            "/a%",
        ] {
            assert!(parse_request_path(path).is_none(), "{path}");
        }
    }

    #[test]
    fn windows_devices_are_recognized_on_every_platform() {
        assert!(!is_safe_windows_name("CON.txt"));
        assert!(!is_safe_windows_name("LPT1"));
        assert!(!is_safe_windows_name("COM¹.log"));
        assert!(is_safe_windows_name("computer.txt"));
    }

    #[test]
    fn windows_unsafe_roots_are_rejected_before_filesystem_access() {
        for path in [
            r"\\server\share",
            r"//server/share",
            r"\\?\C:\public",
            r"\\.\pipe\name",
            r"\??\C:\public",
            r"C:\public\CON.txt",
            r"C:\public\trailing ",
            r"C:\public\trailing.",
            r"C:\public\file.txt::$DATA",
        ] {
            assert!(unsafe_windows_path(path), "{path}");
        }
        assert!(!unsafe_windows_path(r"C:\public\files"));
    }

    #[test]
    fn protected_path_normalization_resolves_relative_parent_components() {
        let current = env::current_dir().unwrap();
        let normalized = normalize_policy_path(PathBuf::from("alpha/../protected"));
        assert_eq!(normalized, current.join("protected"));
    }

    #[cfg(not(windows))]
    #[test]
    fn posix_configured_paths_reject_backslashes_before_filesystem_access() {
        assert!(validate_configured_path_lexically(Path::new("public\\alias")).is_err());
    }

    #[test]
    fn case_insensitive_components_use_unicode_normalization_and_folding() {
        assert!(case_insensitive_component_equal("CAFÉ", "cafe\u{301}"));
        assert!(case_insensitive_component_equal("Straße", "STRASSE"));
    }

    #[test]
    fn unsafe_switch_can_explicitly_unlock_a_hidden_root() {
        let temporary = TempDir::new().unwrap();
        let hidden = temporary.path().join(".credentials");
        std::fs::create_dir(&hidden).unwrap();
        let hidden = hidden.canonicalize().unwrap();
        assert!(validate_configured_root(&hidden, false).is_err());
        assert!(validate_configured_root(&hidden, true).is_ok());
    }
}
