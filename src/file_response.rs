use std::io::SeekFrom;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_stream::stream;
use axum::body::Body;
use axum::http::header::{
    ACCEPT_RANGES, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, IF_MATCH,
    IF_MODIFIED_SINCE, IF_NONE_MATCH, IF_RANGE, IF_UNMODIFIED_SINCE, LAST_MODIFIED, RANGE,
};
use axum::http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode};
use bytes::Bytes;
use cap_std::fs::File;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

const READ_BUFFER_SIZE: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ByteRange {
    start: u64,
    end: u64,
}

impl ByteRange {
    fn len(self) -> u64 {
        self.end - self.start + 1
    }
}

pub fn serve_file(
    request: &Request<Body>,
    file: File,
    metadata: cap_std::fs::Metadata,
    name: &str,
) -> Response<Body> {
    let length = metadata.len();
    let metadata_modified = metadata
        .modified()
        .ok()
        .map(cap_std::time::SystemTime::into_std);
    let modified = metadata_modified.filter(|time| time.duration_since(UNIX_EPOCH).is_ok());
    let etag = make_etag(length, metadata_modified);
    let mut content_type = mime_guess::from_path(name)
        .first_or_octet_stream()
        .to_string();
    if content_type.starts_with("text/") && !content_type.contains("charset=") {
        content_type.push_str("; charset=utf-8");
    }

    let mut base_headers = HeaderMap::new();
    base_headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str(&content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    base_headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    base_headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=0, must-revalidate"),
    );
    base_headers.insert(
        ETAG,
        HeaderValue::from_str(&etag).expect("generated ETag is valid"),
    );
    if let Some(modified) = modified
        && let Ok(value) = HeaderValue::from_str(&httpdate::fmt_http_date(modified))
    {
        base_headers.insert(LAST_MODIFIED, value);
    }

    if let Some(status) = evaluate_preconditions(request.headers(), modified, &etag) {
        return response_with_headers(status, base_headers, Body::empty());
    }

    let mut ranges = None;
    if if_range_allows(request.headers(), modified, &etag)
        && let Some(value) = request.headers().get(RANGE)
    {
        let parsed = value
            .to_str()
            .ok()
            .and_then(|value| parse_ranges(value, length).ok());
        match parsed {
            Some(parsed) if !parsed.is_empty() => {
                if total_range_length(&parsed).is_some_and(|total| total <= length) {
                    ranges = Some(parsed);
                }
            }
            _ => {
                base_headers.insert(
                    CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes */{length}"))
                        .expect("content range is valid"),
                );
                return response_with_headers(
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    base_headers,
                    Body::empty(),
                );
            }
        }
    }

    match ranges {
        None => {
            base_headers.insert(
                CONTENT_LENGTH,
                HeaderValue::from_str(&length.to_string()).expect("length is valid"),
            );
            let body = if request.method() == Method::HEAD {
                Body::empty()
            } else {
                stream_file(file, 0, length)
            };
            response_with_headers(StatusCode::OK, base_headers, body)
        }
        Some(ranges) if ranges.len() == 1 => {
            let range = ranges[0];
            base_headers.insert(
                CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes {}-{}/{length}", range.start, range.end))
                    .expect("content range is valid"),
            );
            base_headers.insert(
                CONTENT_LENGTH,
                HeaderValue::from_str(&range.len().to_string()).expect("length is valid"),
            );
            let body = if request.method() == Method::HEAD {
                Body::empty()
            } else {
                stream_file(file, range.start, range.len())
            };
            response_with_headers(StatusCode::PARTIAL_CONTENT, base_headers, body)
        }
        Some(ranges) => serve_multipart(
            request.method(),
            file,
            length,
            &content_type,
            &etag,
            ranges,
            base_headers,
        ),
    }
}

fn evaluate_preconditions(
    headers: &HeaderMap,
    modified: Option<SystemTime>,
    etag: &str,
) -> Option<StatusCode> {
    if let Some(value) = headers.get(IF_MATCH).and_then(|value| value.to_str().ok()) {
        if value.trim() != "*" && !etag_list_matches(value, etag, false) {
            return Some(StatusCode::PRECONDITION_FAILED);
        }
    } else if let (Some(value), Some(modified)) = (
        headers
            .get(IF_UNMODIFIED_SINCE)
            .and_then(|value| value.to_str().ok()),
        modified,
    ) && httpdate::parse_http_date(value)
        .is_ok_and(|date| truncate_seconds(modified) > date)
    {
        return Some(StatusCode::PRECONDITION_FAILED);
    }

    if let Some(value) = headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
    {
        if value.trim() == "*" || etag_list_matches(value, etag, true) {
            return Some(StatusCode::NOT_MODIFIED);
        }
    } else if let (Some(value), Some(modified)) = (
        headers
            .get(IF_MODIFIED_SINCE)
            .and_then(|value| value.to_str().ok()),
        modified,
    ) && httpdate::parse_http_date(value)
        .is_ok_and(|date| truncate_seconds(modified) <= date)
    {
        return Some(StatusCode::NOT_MODIFIED);
    }
    None
}

fn if_range_allows(headers: &HeaderMap, modified: Option<SystemTime>, etag: &str) -> bool {
    let Some(value) = headers.get(IF_RANGE).and_then(|value| value.to_str().ok()) else {
        return true;
    };
    if value.starts_with('"') || value.starts_with("W/") {
        return !value.starts_with("W/") && value == etag;
    }
    match (httpdate::parse_http_date(value), modified) {
        (Ok(date), Some(modified)) => truncate_seconds(modified) <= date,
        _ => false,
    }
}

fn etag_list_matches(list: &str, current: &str, weak: bool) -> bool {
    list.split(',').any(|candidate| {
        let candidate = candidate.trim();
        if weak {
            candidate.trim_start_matches("W/") == current.trim_start_matches("W/")
        } else {
            !candidate.starts_with("W/") && candidate == current
        }
    })
}

fn truncate_seconds(time: SystemTime) -> SystemTime {
    let duration = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    UNIX_EPOCH + Duration::from_secs(duration.as_secs())
}

fn make_etag(length: u64, modified: Option<SystemTime>) -> String {
    let nanos = modified
        .map(|time| match time.duration_since(UNIX_EPOCH) {
            Ok(duration) => duration.as_nanos() as i128,
            Err(error) => -(error.duration().as_nanos() as i128),
        })
        .unwrap_or_default();
    format!("W/\"{nanos:x}-{length:x}\"")
}

fn parse_ranges(header: &str, length: u64) -> Result<Vec<ByteRange>, ()> {
    let Some(specification) = header.strip_prefix("bytes=") else {
        return Err(());
    };
    if specification.trim().is_empty() || length == 0 {
        return Err(());
    }

    let mut ranges = Vec::new();
    let mut members = 0usize;
    for item in specification.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        members += 1;
        if members > 32 {
            return Err(());
        }
        let (start, end) = item.split_once('-').ok_or(())?;
        let range = if start.is_empty() {
            let suffix = end.parse::<u64>().map_err(|_| ())?;
            if suffix == 0 {
                return Err(());
            }
            let count = suffix.min(length);
            ByteRange {
                start: length - count,
                end: length - 1,
            }
        } else {
            let start = start.parse::<u64>().map_err(|_| ())?;
            if start >= length {
                continue;
            }
            let end = if end.is_empty() {
                length - 1
            } else {
                end.parse::<u64>().map_err(|_| ())?.min(length - 1)
            };
            if start > end {
                return Err(());
            }
            ByteRange { start, end }
        };
        ranges.push(range);
    }
    if ranges.is_empty() {
        Err(())
    } else {
        Ok(ranges)
    }
}

fn total_range_length(ranges: &[ByteRange]) -> Option<u64> {
    ranges
        .iter()
        .try_fold(0_u64, |total, range| total.checked_add(range.len()))
}

fn serve_multipart(
    method: &Method,
    file: File,
    full_length: u64,
    content_type: &str,
    etag: &str,
    ranges: Vec<ByteRange>,
    mut headers: HeaderMap,
) -> Response<Body> {
    let boundary = multipart_boundary(etag, &ranges);
    let part_headers: Vec<Vec<u8>> = ranges
        .iter()
        .map(|range| {
            format!(
                "--{boundary}\r\nContent-Type: {content_type}\r\nContent-Range: bytes {}-{}/{full_length}\r\n\r\n",
                range.start, range.end
            )
            .into_bytes()
        })
        .collect();
    let closing = format!("--{boundary}--\r\n").into_bytes();
    let content_length = part_headers.iter().zip(&ranges).try_fold(
        closing.len() as u64,
        |total, (header, range)| {
            total
                .checked_add(header.len() as u64)?
                .checked_add(range.len())?
                .checked_add(2)
        },
    );
    let Some(content_length) = content_length else {
        return response_with_headers(StatusCode::RANGE_NOT_SATISFIABLE, headers, Body::empty());
    };

    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str(&format!("multipart/byteranges; boundary={boundary}"))
            .expect("boundary is valid"),
    );
    headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&content_length.to_string()).expect("length is valid"),
    );

    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        let mut file = tokio::fs::File::from_std(file.into_std());
        Body::from_stream(stream! {
            for (header, range) in part_headers.into_iter().zip(ranges) {
                yield Ok::<Bytes, std::io::Error>(Bytes::from(header));
                if let Err(error) = file.seek(SeekFrom::Start(range.start)).await {
                    yield Err(error);
                    return;
                }
                let mut remaining = range.len();
                let mut buffer = vec![0_u8; READ_BUFFER_SIZE];
                while remaining > 0 {
                    let size = usize::try_from(remaining.min(READ_BUFFER_SIZE as u64)).unwrap_or(READ_BUFFER_SIZE);
                    let read = match file.read(&mut buffer[..size]).await {
                        Ok(read) => read,
                        Err(error) => {
                            yield Err(error);
                            return;
                        }
                    };
                    if read == 0 {
                        yield Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "file changed during response"));
                        return;
                    }
                    remaining -= read as u64;
                    yield Ok(Bytes::copy_from_slice(&buffer[..read]));
                }
                yield Ok(Bytes::from_static(b"\r\n"));
            }
            yield Ok(Bytes::from(closing));
        })
    };
    response_with_headers(StatusCode::PARTIAL_CONTENT, headers, body)
}

fn multipart_boundary(etag: &str, ranges: &[ByteRange]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(etag.as_bytes());
    for range in ranges {
        hasher.update(range.start.to_be_bytes());
        hasher.update(range.end.to_be_bytes());
    }
    format!("autoindex-{:x}", hasher.finalize())[..42].to_owned()
}

fn stream_file(file: File, offset: u64, length: u64) -> Body {
    let mut file = tokio::fs::File::from_std(file.into_std());
    Body::from_stream(stream! {
        if let Err(error) = file.seek(SeekFrom::Start(offset)).await {
            yield Err::<Bytes, std::io::Error>(error);
            return;
        }
        let mut remaining = length;
        let mut buffer = vec![0_u8; READ_BUFFER_SIZE];
        while remaining > 0 {
            let size = usize::try_from(remaining.min(READ_BUFFER_SIZE as u64)).unwrap_or(READ_BUFFER_SIZE);
            let read = match file.read(&mut buffer[..size]).await {
                Ok(read) => read,
                Err(error) => {
                    yield Err(error);
                    return;
                }
            };
            if read == 0 {
                yield Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "file changed during response"));
                return;
            }
            remaining -= read as u64;
            yield Ok(Bytes::copy_from_slice(&buffer[..read]));
        }
    })
}

fn response_with_headers(status: StatusCode, headers: HeaderMap, body: Body) -> Response<Body> {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_suffix_and_open_ranges() {
        assert_eq!(
            parse_ranges("bytes=2-5", 10),
            Ok(vec![ByteRange { start: 2, end: 5 }])
        );
        assert_eq!(
            parse_ranges("bytes=-3", 10),
            Ok(vec![ByteRange { start: 7, end: 9 }])
        );
        assert_eq!(
            parse_ranges("bytes=8-", 10),
            Ok(vec![ByteRange { start: 8, end: 9 }])
        );
    }

    #[test]
    fn ignores_unsatisfiable_members_but_rejects_empty_result() {
        assert_eq!(
            parse_ranges("bytes=99-100,0-0", 10),
            Ok(vec![ByteRange { start: 0, end: 0 }])
        );
        assert_eq!(parse_ranges("bytes=99-100", 10), Err(()));
    }

    #[test]
    fn range_amplification_is_detected_and_empty_members_are_ignored() {
        let parsed = parse_ranges("bytes=0-9,0-9", 10).unwrap();
        assert_eq!(total_range_length(&parsed), Some(20));
        assert_eq!(
            parse_ranges("bytes=,0-0,", 10),
            Ok(vec![ByteRange { start: 0, end: 0 }])
        );
    }

    #[test]
    fn etags_distinguish_pre_epoch_timestamps_from_missing_timestamps() {
        let before_epoch = UNIX_EPOCH - Duration::from_secs(1);
        assert_ne!(make_etag(10, Some(before_epoch)), make_etag(10, None));
    }
}
