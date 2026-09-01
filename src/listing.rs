use std::{
    cmp::Ordering,
    collections::BinaryHeap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering as AtomicOrdering},
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use cap_std::fs::Dir;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use thiserror::Error;

use crate::path_policy::{is_safe_visible_name, validate_resolved_relative};

const CURSOR_VERSION: u8 = 2;
const CURSOR_HEADER_SIZE: usize = 25;
const MAX_CURSOR_BYTES: usize = 512;
const MAX_SCANNED_ENTRIES: usize = 1_000_000;
const PATH_SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'/')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SortField {
    Name,
    Size,
    Modified,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SortOrder {
    Ascending,
    Descending,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SortSpec {
    pub field: SortField,
    pub order: SortOrder,
}

impl Default for SortSpec {
    fn default() -> Self {
        Self {
            field: SortField::Name,
            order: SortOrder::Ascending,
        }
    }
}

impl SortSpec {
    pub(crate) fn parse(sort: Option<&str>, order: Option<&str>) -> Result<Self, ListingError> {
        let field = match sort.unwrap_or("name") {
            "name" => SortField::Name,
            "size" => SortField::Size,
            "modified" => SortField::Modified,
            _ => return Err(ListingError::InvalidSort),
        };
        let default_order = if field == SortField::Name {
            "asc"
        } else {
            "desc"
        };
        let order = match order.unwrap_or(default_order) {
            "asc" => SortOrder::Ascending,
            "desc" => SortOrder::Descending,
            _ => return Err(ListingError::InvalidSort),
        };
        Ok(Self { field, order })
    }

    pub(crate) const fn field_name(self) -> &'static str {
        match self.field {
            SortField::Name => "name",
            SortField::Size => "size",
            SortField::Modified => "modified",
        }
    }

    pub(crate) const fn order_name(self) -> &'static str {
        match self.order {
            SortOrder::Ascending => "asc",
            SortOrder::Descending => "desc",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Candidate {
    directory: bool,
    name: String,
    folded: String,
    size: u64,
    modified_secs: i64,
    modified_nanos: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CursorDirection {
    After,
    Before,
}

#[derive(Clone, Debug)]
struct ListingCursor {
    direction: CursorDirection,
    sort: SortSpec,
    candidate: Candidate,
}

#[derive(Clone, Debug)]
struct HeapEntry {
    candidate: Candidate,
    sort: SortSpec,
    reverse: bool,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        compare_candidates(&self.candidate, &other.candidate, self.sort) == Ordering::Equal
    }
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        let order = compare_candidates(&self.candidate, &other.candidate, self.sort);
        if self.reverse { order.reverse() } else { order }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ListingItem {
    pub name: String,
    pub href: String,
    pub directory: bool,
    pub size: String,
    pub modified: String,
    pub modified_rfc3339: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ListingPage {
    pub items: Vec<ListingItem>,
    pub previous_cursor: Option<String>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Error)]
pub(crate) enum ListingError {
    #[error("invalid directory sort")]
    InvalidSort,
    #[error("invalid directory cursor")]
    InvalidCursor,
    #[error("directory exceeds scan limit")]
    TooLarge,
    #[error("directory unavailable")]
    Unavailable,
    #[error("directory scan cancelled")]
    Cancelled,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_directory(
    root: &Dir,
    directory: &Path,
    root_listing: bool,
    sort: SortSpec,
    cursor_value: Option<&str>,
    page_size: usize,
    timezone: Tz,
    cancelled: &AtomicBool,
) -> Result<ListingPage, ListingError> {
    if cancelled.load(AtomicOrdering::Relaxed) {
        return Err(ListingError::Cancelled);
    }
    let cursor = cursor_value.map(decode_cursor).transpose()?;
    if cursor.as_ref().is_some_and(|value| value.sort != sort) {
        return Err(ListingError::InvalidCursor);
    }
    let forward = cursor
        .as_ref()
        .is_none_or(|value| value.direction == CursorDirection::After);
    let mut heap = BinaryHeap::with_capacity(page_size + 1);
    let entries = root
        .read_dir(directory)
        .map_err(|_| ListingError::Unavailable)?;
    let mut scanned = 0usize;
    for entry in entries {
        if cancelled.load(AtomicOrdering::Relaxed) {
            return Err(ListingError::Cancelled);
        }
        scanned += 1;
        if scanned > MAX_SCANNED_ENTRIES {
            return Err(ListingError::TooLarge);
        }
        let Ok(entry) = entry else { continue };
        let Some(name) = entry.file_name().to_str().map(ToString::to_string) else {
            continue;
        };
        if !is_safe_visible_name(&name) || root_listing && name.starts_with("__") {
            continue;
        }
        let requested = if directory == Path::new(".") {
            PathBuf::from(&name)
        } else {
            directory.join(&name)
        };
        let Ok(initial_metadata) = root.metadata(&requested) else {
            continue;
        };
        if !initial_metadata.is_dir() && !initial_metadata.is_file() {
            continue;
        }
        let Ok(resolved) = root.canonicalize(&requested) else {
            continue;
        };
        if !validate_resolved_relative(&resolved) {
            continue;
        }
        let Ok(metadata) = root.metadata(&resolved) else {
            continue;
        };
        if !metadata.is_dir() && !metadata.is_file() {
            continue;
        }
        let modified = metadata
            .modified()
            .map(cap_std::time::SystemTime::into_std)
            .unwrap_or(UNIX_EPOCH);
        let (modified_secs, modified_nanos) = system_time_parts(modified);
        let candidate = Candidate {
            directory: metadata.is_dir(),
            folded: name.to_lowercase(),
            name,
            size: if metadata.is_file() {
                metadata.len()
            } else {
                0
            },
            modified_secs,
            modified_nanos,
        };
        if let Some(cursor) = &cursor {
            let comparison = compare_candidates(&candidate, &cursor.candidate, sort);
            if (forward && comparison != Ordering::Greater)
                || (!forward && comparison != Ordering::Less)
            {
                continue;
            }
        }
        let entry = HeapEntry {
            candidate,
            sort,
            reverse: !forward,
        };
        if heap.len() < page_size + 1 {
            heap.push(entry);
        } else {
            let should_replace = heap.peek().is_some_and(|current| {
                let comparison = compare_candidates(&entry.candidate, &current.candidate, sort);
                if forward {
                    comparison == Ordering::Less
                } else {
                    comparison == Ordering::Greater
                }
            });
            if should_replace {
                heap.pop();
                heap.push(entry);
            }
        }
    }

    let mut candidates: Vec<_> = heap.into_iter().map(|entry| entry.candidate).collect();
    candidates.sort_by(|left, right| compare_candidates(left, right, sort));
    let mut has_previous = forward && cursor.is_some();
    let mut has_next = !forward;
    if forward && candidates.len() > page_size {
        candidates.truncate(page_size);
        has_next = true;
    } else if !forward && candidates.len() > page_size {
        candidates = candidates.split_off(candidates.len() - page_size);
        has_previous = true;
    }
    if candidates.is_empty() {
        has_previous = false;
        has_next = false;
    }

    let previous_cursor = has_previous.then(|| {
        encode_cursor(
            CursorDirection::Before,
            sort,
            candidates.first().expect("non-empty page"),
        )
    });
    let next_cursor = has_next.then(|| {
        encode_cursor(
            CursorDirection::After,
            sort,
            candidates.last().expect("non-empty page"),
        )
    });
    let items = candidates
        .into_iter()
        .map(|candidate| candidate_view(candidate, sort, timezone))
        .collect();
    Ok(ListingPage {
        items,
        previous_cursor,
        next_cursor,
    })
}

fn candidate_view(candidate: Candidate, sort: SortSpec, timezone: Tz) -> ListingItem {
    let escaped = escape_path_segment(&candidate.name);
    let href = if candidate.directory {
        format!("./{escaped}/{}", sort_query(sort, None))
    } else {
        format!("./{escaped}")
    };
    let (modified, modified_rfc3339) =
        format_modified(candidate.modified_secs, candidate.modified_nanos, timezone);
    ListingItem {
        name: candidate.name,
        href,
        directory: candidate.directory,
        size: if candidate.directory {
            "—".to_string()
        } else {
            format_file_size(candidate.size)
        },
        modified,
        modified_rfc3339,
    }
}

pub(crate) fn sort_query(sort: SortSpec, cursor: Option<&str>) -> String {
    let mut result = format!("?sort={}&order={}", sort.field_name(), sort.order_name());
    if let Some(cursor) = cursor {
        result.push_str("&cursor=");
        result.push_str(cursor);
    }
    result
}

pub(crate) fn escape_path_segment(value: &str) -> String {
    let escaped = utf8_percent_encode(value, PATH_SEGMENT).to_string();
    let mut result = String::with_capacity(escaped.len());
    for character in escaped.chars() {
        if character.is_ascii() {
            result.push(character);
        } else {
            let mut bytes = [0_u8; 4];
            for byte in character.encode_utf8(&mut bytes).bytes() {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                result.push('%');
                result.push(HEX[(byte >> 4) as usize] as char);
                result.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
    result
}

fn compare_candidates(left: &Candidate, right: &Candidate, sort: SortSpec) -> Ordering {
    if left.directory != right.directory {
        return if left.directory {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }
    let name_order = || {
        left.folded
            .cmp(&right.folded)
            .then(left.name.cmp(&right.name))
    };
    match sort.field {
        SortField::Name => {
            let ordering = name_order();
            if sort.order == SortOrder::Descending {
                ordering.reverse()
            } else {
                ordering
            }
        }
        SortField::Size if left.directory => name_order(),
        SortField::Size => {
            let primary = left.size.cmp(&right.size);
            let primary = if sort.order == SortOrder::Descending {
                primary.reverse()
            } else {
                primary
            };
            primary.then_with(name_order)
        }
        SortField::Modified => {
            let primary = (left.modified_secs, left.modified_nanos)
                .cmp(&(right.modified_secs, right.modified_nanos));
            let primary = if sort.order == SortOrder::Descending {
                primary.reverse()
            } else {
                primary
            };
            primary.then_with(name_order)
        }
    }
}

fn encode_cursor(direction: CursorDirection, sort: SortSpec, candidate: &Candidate) -> String {
    let mut payload = Vec::with_capacity(CURSOR_HEADER_SIZE + candidate.name.len());
    payload.push(CURSOR_VERSION);
    payload.push(u8::from(direction == CursorDirection::Before));
    payload.push(match sort.field {
        SortField::Name => 0,
        SortField::Size => 1,
        SortField::Modified => 2,
    });
    payload.push(u8::from(sort.order == SortOrder::Descending));
    payload.push(u8::from(!candidate.directory));
    payload.extend_from_slice(&candidate.size.to_be_bytes());
    payload.extend_from_slice(&(candidate.modified_secs as u64).to_be_bytes());
    payload.extend_from_slice(&candidate.modified_nanos.to_be_bytes());
    payload.extend_from_slice(candidate.name.as_bytes());
    URL_SAFE_NO_PAD.encode(payload)
}

fn decode_cursor(value: &str) -> Result<ListingCursor, ListingError> {
    if value.is_empty() || value.len() > MAX_CURSOR_BYTES {
        return Err(ListingError::InvalidCursor);
    }
    let payload = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ListingError::InvalidCursor)?;
    if payload
        .first()
        .is_some_and(|version| *version != CURSOR_VERSION)
    {
        return decode_legacy_cursor(&payload);
    }
    if payload.len() <= CURSOR_HEADER_SIZE
        || payload[0] != CURSOR_VERSION
        || payload[1] > 1
        || payload[2] > 2
        || payload[3] > 1
        || payload[4] > 1
    {
        return Err(ListingError::InvalidCursor);
    }
    let name = std::str::from_utf8(&payload[CURSOR_HEADER_SIZE..])
        .map_err(|_| ListingError::InvalidCursor)?;
    if name.len() > 255 || !is_safe_visible_name(name) {
        return Err(ListingError::InvalidCursor);
    }
    let size = u64::from_be_bytes(payload[5..13].try_into().expect("fixed slice"));
    let modified_secs = u64::from_be_bytes(payload[13..21].try_into().expect("fixed slice")) as i64;
    let modified_nanos = u32::from_be_bytes(payload[21..25].try_into().expect("fixed slice"));
    if modified_nanos >= 1_000_000_000 {
        return Err(ListingError::InvalidCursor);
    }
    let sort = SortSpec {
        field: match payload[2] {
            0 => SortField::Name,
            1 => SortField::Size,
            _ => SortField::Modified,
        },
        order: if payload[3] == 0 {
            SortOrder::Ascending
        } else {
            SortOrder::Descending
        },
    };
    Ok(ListingCursor {
        direction: if payload[1] == 0 {
            CursorDirection::After
        } else {
            CursorDirection::Before
        },
        sort,
        candidate: Candidate {
            directory: payload[4] == 0,
            folded: name.to_lowercase(),
            name: name.to_string(),
            size,
            modified_secs,
            modified_nanos,
        },
    })
}

fn decode_legacy_cursor(payload: &[u8]) -> Result<ListingCursor, ListingError> {
    if payload.len() < 3 || payload[0] > 1 || payload[1] > 1 {
        return Err(ListingError::InvalidCursor);
    }
    let name = std::str::from_utf8(&payload[2..]).map_err(|_| ListingError::InvalidCursor)?;
    if name.len() > 255 || !is_safe_visible_name(name) {
        return Err(ListingError::InvalidCursor);
    }
    let sort = SortSpec::default();
    Ok(ListingCursor {
        direction: if payload[0] == 0 {
            CursorDirection::After
        } else {
            CursorDirection::Before
        },
        sort,
        candidate: Candidate {
            directory: payload[1] == 0,
            folded: name.to_lowercase(),
            name: name.to_string(),
            size: 0,
            modified_secs: 0,
            modified_nanos: 0,
        },
    })
}

fn system_time_parts(value: SystemTime) -> (i64, u32) {
    match value.duration_since(UNIX_EPOCH) {
        Ok(duration) => (
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
            duration.subsec_nanos(),
        ),
        Err(error) => {
            let duration = error.duration();
            let Ok(seconds) = i64::try_from(duration.as_secs()) else {
                return (i64::MIN, 0);
            };
            if duration.subsec_nanos() == 0 {
                (-seconds, 0)
            } else {
                (
                    seconds.saturating_neg().saturating_sub(1),
                    1_000_000_000 - duration.subsec_nanos(),
                )
            }
        }
    }
}

fn format_modified(seconds: i64, nanos: u32, timezone: Tz) -> (String, String) {
    let Some(value) = DateTime::<Utc>::from_timestamp(seconds, nanos) else {
        return (String::new(), String::new());
    };
    let value = value.with_timezone(&timezone);
    (
        value.format("%Y-%m-%d %H:%M:%S").to_string(),
        value.to_rfc3339(),
    )
}

pub(crate) fn format_file_size(size: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];
    if size < 1024 {
        return format!("{size} B");
    }
    let mut unit = 0usize;
    let mut scale = 1u64;
    while unit < UNITS.len() - 1 && size >= scale.saturating_mul(1024) {
        scale = scale.saturating_mul(1024);
        unit += 1;
    }
    let whole = size / scale;
    let remainder = size % scale;
    let mut tenths = whole
        .saturating_mul(10)
        .saturating_add((remainder.saturating_mul(10) + scale / 2) / scale);
    if tenths >= 10_240 && unit < UNITS.len() - 1 {
        unit += 1;
        tenths = 10;
    }
    if tenths.checked_rem(10) == Some(0) {
        format!("{} {}", tenths / 10, UNITS[unit])
    } else {
        format!("{}.{} {}", tenths / 10, tenths % 10, UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(directory: bool, name: &str, size: u64, modified: i64) -> Candidate {
        Candidate {
            directory,
            name: name.to_string(),
            folded: name.to_lowercase(),
            size,
            modified_secs: modified,
            modified_nanos: 0,
        }
    }

    #[test]
    fn file_size_boundaries_match_the_go_service() {
        for (size, expected) in [
            (0, "0 B"),
            (1023, "1023 B"),
            (1024, "1 KB"),
            (1536, "1.5 KB"),
            (1_048_524, "1023.9 KB"),
            (1_048_525, "1 MB"),
            (u64::MAX >> 1, "8 EB"),
        ] {
            assert_eq!(format_file_size(size), expected);
        }
    }

    #[test]
    fn pre_epoch_timestamps_keep_their_signed_value() {
        assert_eq!(
            system_time_parts(UNIX_EPOCH - std::time::Duration::from_millis(1500)),
            (-2, 500_000_000)
        );
    }

    #[test]
    fn path_segments_escape_unicode_and_url_delimiters() {
        assert_eq!(
            escape_path_segment("目录 #1?.txt"),
            "%E7%9B%AE%E5%BD%95%20%231%3F.txt"
        );
    }

    #[test]
    fn cursor_round_trips_every_comparison_key() {
        let sort = SortSpec {
            field: SortField::Modified,
            order: SortOrder::Descending,
        };
        let candidate = Candidate {
            modified_nanos: 987_654_321,
            ..candidate(true, "archive-#%.data", (1 << 40) + 37, 1_700_000_000)
        };
        let value = encode_cursor(CursorDirection::Before, sort, &candidate);
        let decoded = decode_cursor(&value).unwrap();
        assert_eq!(decoded.direction, CursorDirection::Before);
        assert_eq!(decoded.sort, sort);
        assert_eq!(decoded.candidate, candidate);
    }

    #[test]
    fn legacy_name_cursors_remain_compatible_and_empty_cursors_fail() {
        let mut payload = vec![0, 1];
        payload.extend_from_slice(b"file-a.txt");
        let cursor = URL_SAFE_NO_PAD.encode(payload);
        let decoded = decode_cursor(&cursor).unwrap();
        assert_eq!(decoded.direction, CursorDirection::After);
        assert_eq!(decoded.sort, SortSpec::default());
        assert_eq!(decoded.candidate.name, "file-a.txt");
        assert!(!decoded.candidate.directory);
        assert!(decode_cursor("").is_err());
    }

    #[test]
    fn cancelled_scan_stops_before_reading_the_directory() {
        let temporary = tempfile::TempDir::new().unwrap();
        let directory =
            Dir::open_ambient_dir(temporary.path(), cap_std::ambient_authority()).unwrap();
        let cancelled = AtomicBool::new(true);
        let result = scan_directory(
            &directory,
            Path::new("."),
            true,
            SortSpec::default(),
            None,
            100,
            chrono_tz::UTC,
            &cancelled,
        );
        assert!(matches!(result, Err(ListingError::Cancelled)));
    }

    #[test]
    fn all_sorts_keep_directories_first() {
        let mut values = [
            candidate(false, "large", 30, 30),
            candidate(true, "z-dir", 0, 10),
            candidate(false, "small", 5, 10),
            candidate(true, "a-dir", 0, 30),
        ];
        for field in [SortField::Name, SortField::Size, SortField::Modified] {
            for order in [SortOrder::Ascending, SortOrder::Descending] {
                let sort = SortSpec { field, order };
                values.sort_by(|left, right| compare_candidates(left, right, sort));
                assert!(values[0].directory && values[1].directory);
            }
        }
    }
}
