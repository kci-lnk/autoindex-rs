use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use askama::Template;
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::header::{
    ALLOW, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_SECURITY_POLICY, CONTENT_TYPE, LOCATION,
    REFERRER_POLICY, RETRY_AFTER, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::http::{HeaderValue, Method, Request, Response, StatusCode};
use axum::routing::any;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, File, Metadata};
use sha2::{Digest, Sha256};
use tower_http::trace::TraceLayer;

use crate::Config;
use crate::file_response::serve_file;
use crate::listing::{
    ListingError, ListingItem, SortField, SortOrder, SortSpec, escape_path_segment, scan_directory,
    sort_query,
};
use crate::markdown::render_readme;
use crate::path_policy::{
    RequestPath, open_validated_directory, open_validated_regular, parse_request_path,
    same_directory_handle, validate_configured_root,
};

const GENERATED_CACHE_CONTROL: &str = "private, no-store";
const THEME_SCRIPT: &str = r#"(function () {
  "use strict";
  var root = document.documentElement;
  var storageKey = "autoindex-rs-theme";
  var media = window.matchMedia("(prefers-color-scheme: dark)");
  var stored = "";
  try {
    stored = window.localStorage.getItem(storageKey) || "";
  } catch (_) {}
  if (stored !== "light" && stored !== "dark") {
    stored = "";
  }
  var manual = stored !== "";
  function systemTheme() {
    return media.matches ? "dark" : "light";
  }
  function applyTheme(theme, persist) {
    root.dataset.theme = theme;
    var toggle = document.getElementById("theme-toggle");
    if (toggle) {
      toggle.setAttribute("aria-pressed", theme === "dark" ? "true" : "false");
    }
    if (persist) {
      manual = true;
      try {
        window.localStorage.setItem(storageKey, theme);
      } catch (_) {}
    }
  }
  applyTheme(stored || systemTheme(), false);
  function initializeThemeToggle() {
    var toggle = document.getElementById("theme-toggle");
    if (toggle) {
      applyTheme(root.dataset.theme || systemTheme(), false);
      toggle.addEventListener("click", function () {
        applyTheme(root.dataset.theme === "dark" ? "light" : "dark", true);
      });
      toggle.hidden = false;
    }
    function followSystem(event) {
      if (!manual) {
        applyTheme(event.matches ? "dark" : "light", false);
      }
    }
    if (typeof media.addEventListener === "function") {
      media.addEventListener("change", followSystem);
    } else if (typeof media.addListener === "function") {
      media.addListener(followSystem);
    }
    window.requestAnimationFrame(function () {
      root.classList.add("theme-ready");
    });
  }
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", initializeThemeToggle, { once: true });
  } else {
    initializeThemeToggle();
  }
}());"#;

pub struct AppState {
    pub config: Config,
    root: Arc<Dir>,
}

enum Target {
    File {
        file: File,
        metadata: Box<Metadata>,
        name: String,
    },
    Directory(Arc<Dir>),
    Missing,
    Unavailable,
}

struct CancelOnDrop {
    cancelled: Arc<AtomicBool>,
    armed: bool,
}

impl CancelOnDrop {
    fn new(cancelled: Arc<AtomicBool>) -> Self {
        Self {
            cancelled,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.cancelled.store(true, Ordering::Relaxed);
        }
    }
}

#[derive(Debug, Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    title: String,
    theme_script: &'static str,
    breadcrumbs: Vec<Breadcrumb>,
    sort_links: Vec<SortLink>,
    items: Vec<ListingItem>,
    has_parent: bool,
    parent_href: String,
    has_previous: bool,
    previous_href: String,
    has_next: bool,
    next_href: String,
    has_readme: bool,
    readme_html: String,
    timezone_name: String,
}

#[derive(Debug)]
struct Breadcrumb {
    name: String,
    href: String,
    current: bool,
}

#[derive(Debug)]
struct SortLink {
    field_class: &'static str,
    label: &'static str,
    href: String,
    aria_label: String,
    aria_sort: &'static str,
    indicator: &'static str,
    active: bool,
}

pub fn open_state(mut config: Config) -> io::Result<AppState> {
    config
        .validate_for_server()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let root = Dir::open_ambient_dir(&config.directory, ambient_authority())?;
    let resolved = std::fs::canonicalize(&config.directory)?;
    validate_configured_root(&resolved, config.allow_sensitive_paths)
        .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error))?;
    let verifier = Dir::open_ambient_dir(&resolved, ambient_authority())?;
    if !same_directory_handle(&root, &verifier) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "served directory changed while it was being opened",
        ));
    }
    Ok(AppState {
        config,
        root: Arc::new(root),
    })
}

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .fallback(any(handle_request))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn handle_request(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
) -> Response<Body> {
    let method = request.method().clone();
    if method != Method::GET && method != Method::HEAD {
        let mut response = error_response(
            &method,
            StatusCode::METHOD_NOT_ALLOWED,
            "Method not allowed",
        );
        response
            .headers_mut()
            .insert(ALLOW, HeaderValue::from_static("GET, HEAD"));
        return response;
    }

    let raw_path = request.uri().path().to_string();
    let Some(path) = parse_request_path(&raw_path) else {
        return error_response(&method, StatusCode::NOT_FOUND, "Not found");
    };

    let target = inspect_target(state.root.clone(), path.relative.clone()).await;
    match target {
        Target::File {
            file,
            metadata,
            name,
        } => {
            if path.trailing_slash {
                return redirect_response(
                    request
                        .uri()
                        .path_and_query()
                        .map_or(&raw_path, |value| value.as_str()),
                    false,
                );
            }
            let mut response = serve_file(&request, file, *metadata, &name);
            apply_common_headers(&mut response);
            response
        }
        Target::Directory(directory) => {
            if !path.trailing_slash {
                return redirect_response(
                    request
                        .uri()
                        .path_and_query()
                        .map_or(&raw_path, |value| value.as_str()),
                    true,
                );
            }
            serve_directory(state, request, path, directory).await
        }
        Target::Missing => error_response(&method, StatusCode::NOT_FOUND, "Not found"),
        Target::Unavailable if path.components.is_empty() => {
            unavailable_response(&method, "Static source unavailable")
        }
        Target::Unavailable => error_response(&method, StatusCode::NOT_FOUND, "Not found"),
    }
}

async fn inspect_target(root: Arc<Dir>, requested: PathBuf) -> Target {
    tokio::task::spawn_blocking(move || {
        let metadata = match root.metadata(&requested) {
            Ok(metadata) => metadata,
            Err(_) => return Target::Unavailable,
        };
        if metadata.is_dir() {
            return open_validated_directory(&root, &requested)
                .map(|directory| Target::Directory(Arc::new(directory)))
                .unwrap_or(Target::Unavailable);
        }
        if !metadata.is_file() {
            return Target::Missing;
        }
        match open_validated_regular(&root, &requested) {
            Some((file, metadata)) => Target::File {
                file,
                metadata: Box::new(metadata),
                name: requested
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("download")
                    .to_string(),
            },
            None => Target::Unavailable,
        }
    })
    .await
    .unwrap_or(Target::Unavailable)
}

async fn serve_directory(
    state: Arc<AppState>,
    request: Request<Body>,
    request_path: RequestPath,
    directory: Arc<Dir>,
) -> Response<Body> {
    for index_name in &state.config.index_files {
        let root = directory.clone();
        let requested = PathBuf::from(index_name);
        let display_name = index_name.clone();
        let opened =
            tokio::task::spawn_blocking(move || open_regular_file(&root, &requested)).await;
        if let Ok(Some((file, metadata))) = opened {
            let mut response = serve_file(&request, file, metadata, &display_name);
            apply_common_headers(&mut response);
            return response;
        }
    }

    let query = match parse_listing_query(request.uri().query()) {
        Ok(query) => query,
        Err(message) => return error_response(request.method(), StatusCode::BAD_REQUEST, message),
    };
    let sort = match SortSpec::parse(
        query.get("sort").map(String::as_str),
        query.get("order").map(String::as_str),
    ) {
        Ok(sort) => sort,
        Err(_) => {
            return error_response(
                request.method(),
                StatusCode::BAD_REQUEST,
                "Invalid directory sort",
            );
        }
    };
    let cursor = query.get("cursor").cloned();
    let root_listing = request_path.components.is_empty();
    let page_size = state.config.page_size;
    let timezone = state.config.timezone;
    let render_readme_enabled = state.config.render_readme;
    let directory_for_scan = directory;
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_for_scan = cancelled.clone();
    let mut cancel_on_drop = CancelOnDrop::new(cancelled);
    let listing = tokio::task::spawn_blocking(move || {
        let page = scan_directory(
            &directory_for_scan,
            Path::new("."),
            root_listing,
            sort,
            cursor.as_deref(),
            page_size,
            timezone,
            &cancelled_for_scan,
        )?;
        let readme = (!cancelled_for_scan.load(Ordering::Relaxed) && render_readme_enabled)
            .then(|| render_readme(&directory_for_scan, Path::new(".")))
            .flatten();
        Ok::<_, ListingError>((page, readme))
    })
    .await;
    cancel_on_drop.disarm();

    let (page, readme) = match listing {
        Ok(Ok(value)) => value,
        Ok(Err(ListingError::InvalidCursor)) => {
            return error_response(
                request.method(),
                StatusCode::BAD_REQUEST,
                "Invalid directory cursor",
            );
        }
        Ok(Err(ListingError::InvalidSort)) => {
            return error_response(
                request.method(),
                StatusCode::BAD_REQUEST,
                "Invalid directory sort",
            );
        }
        Ok(Err(ListingError::TooLarge | ListingError::Unavailable | ListingError::Cancelled))
        | Err(_)
            if request_path.components.is_empty() =>
        {
            return unavailable_response(request.method(), "Static source unavailable");
        }
        Ok(Err(ListingError::TooLarge | ListingError::Unavailable | ListingError::Cancelled))
        | Err(_) => {
            return error_response(request.method(), StatusCode::NOT_FOUND, "Not found");
        }
    };

    let display_path = if request_path.components.is_empty() {
        "/".to_string()
    } else {
        format!("/{}/", request_path.components.join("/"))
    };
    let breadcrumbs = breadcrumbs(&request_path.components, sort);
    let sort_links = sort_links(sort);
    let parent_href = parent_href(&request_path.components, sort);
    let previous_href = page
        .previous_cursor
        .as_deref()
        .map_or_else(String::new, |cursor| sort_query(sort, Some(cursor)));
    let next_href = page
        .next_cursor
        .as_deref()
        .map_or_else(String::new, |cursor| sort_query(sort, Some(cursor)));
    let template = IndexTemplate {
        title: format!("Index of {display_path}"),
        theme_script: THEME_SCRIPT,
        breadcrumbs,
        sort_links,
        items: page.items,
        has_parent: !parent_href.is_empty(),
        parent_href,
        has_previous: !previous_href.is_empty(),
        previous_href,
        has_next: !next_href.is_empty(),
        next_href,
        has_readme: readme.is_some(),
        readme_html: readme.unwrap_or_default(),
        timezone_name: state.config.timezone_name.clone(),
    };
    let body = match template.render() {
        Ok(body) => body,
        Err(_) => return unavailable_response(request.method(), "Static source unavailable"),
    };
    listing_response(request.method(), body)
}

fn open_regular_file(root: &Dir, requested: &Path) -> Option<(File, Metadata)> {
    open_validated_regular(root, requested)
}

fn parse_listing_query(raw: Option<&str>) -> Result<HashMap<String, String>, &'static str> {
    let Some(raw) = raw else {
        return Ok(HashMap::new());
    };
    if malformed_percent_encoding(raw) {
        return Err("Invalid directory cursor");
    }
    let mut result = HashMap::new();
    for (key, value) in url::form_urlencoded::parse(raw.as_bytes()) {
        if matches!(key.as_ref(), "sort" | "order" | "cursor") {
            let is_cursor = key == "cursor";
            if value.is_empty() {
                return Err(if is_cursor {
                    "Invalid directory cursor"
                } else {
                    "Invalid directory sort"
                });
            }
            if result
                .insert(key.into_owned(), value.into_owned())
                .is_some()
            {
                return Err(if is_cursor {
                    "Invalid directory cursor"
                } else {
                    "Invalid directory sort"
                });
            }
        }
    }
    Ok(result)
}

fn malformed_percent_encoding(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len()
            || !bytes[index + 1].is_ascii_hexdigit()
            || !bytes[index + 2].is_ascii_hexdigit()
        {
            return true;
        }
        index += 3;
    }
    false
}

fn breadcrumbs(components: &[String], sort: SortSpec) -> Vec<Breadcrumb> {
    let mut result = Vec::with_capacity(components.len() + 1);
    result.push(Breadcrumb {
        name: "root".to_string(),
        href: format!("/{}", sort_query(sort, None)),
        current: components.is_empty(),
    });
    let mut path = String::from("/");
    for (index, component) in components.iter().enumerate() {
        path.push_str(&escape_path_segment(component));
        path.push('/');
        result.push(Breadcrumb {
            name: component.clone(),
            href: format!("{path}{}", sort_query(sort, None)),
            current: index + 1 == components.len(),
        });
    }
    result
}

fn parent_href(components: &[String], sort: SortSpec) -> String {
    if components.is_empty() {
        return String::new();
    }
    let mut path = String::from("/");
    for component in &components[..components.len() - 1] {
        path.push_str(&escape_path_segment(component));
        path.push('/');
    }
    format!("{path}{}", sort_query(sort, None))
}

fn sort_links(active: SortSpec) -> Vec<SortLink> {
    [
        (SortField::Name, "name", "Name"),
        (SortField::Size, "size", "Size"),
        (SortField::Modified, "modified", "Modified"),
    ]
    .into_iter()
    .map(|(field, field_class, label)| {
        let is_active = active.field == field;
        let default_order = if field == SortField::Name {
            SortOrder::Ascending
        } else {
            SortOrder::Descending
        };
        let target_order = if is_active {
            if active.order == SortOrder::Ascending {
                SortOrder::Descending
            } else {
                SortOrder::Ascending
            }
        } else {
            default_order
        };
        let target = SortSpec {
            field,
            order: target_order,
        };
        let direction = if target_order == SortOrder::Ascending {
            "ascending"
        } else {
            "descending"
        };
        SortLink {
            field_class,
            label,
            href: sort_query(target, None),
            aria_label: format!("Sort by {label}, {direction}"),
            aria_sort: if is_active {
                if active.order == SortOrder::Ascending {
                    "ascending"
                } else {
                    "descending"
                }
            } else {
                ""
            },
            indicator: if is_active {
                if active.order == SortOrder::Ascending {
                    "↑"
                } else {
                    "↓"
                }
            } else {
                "↕"
            },
            active: is_active,
        }
    })
    .collect()
}

fn redirect_response(path_and_query: &str, add_slash: bool) -> Response<Body> {
    let (path, query) = path_and_query
        .split_once('?')
        .map_or((path_and_query, ""), |(path, query)| (path, query));
    let path = if add_slash {
        format!("{}/", path.trim_end_matches('/'))
    } else {
        path.trim_end_matches('/').to_string()
    };
    let location = if query.is_empty() {
        path
    } else {
        format!("{path}?{query}")
    };
    let mut response = generated_response(StatusCode::MOVED_PERMANENTLY, Body::empty());
    if let Ok(value) = HeaderValue::from_str(&location) {
        response.headers_mut().insert(LOCATION, value);
    }
    response
}

fn listing_response(method: &Method, body: String) -> Response<Body> {
    let length = body.len();
    let response_body = if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(body)
    };
    let mut response = generated_response(StatusCode::OK, response_body);
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response.headers_mut().insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string()).expect("length is valid"),
    );
    response.headers_mut().insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(listing_csp()).expect("listing CSP is valid"),
    );
    response
        .headers_mut()
        .insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    response
        .headers_mut()
        .insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response
}

fn error_response(method: &Method, status: StatusCode, message: &str) -> Response<Body> {
    let text = format!("{message}\n");
    let length = text.len();
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(text)
    };
    let mut response = generated_response(status, body);
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response.headers_mut().insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string()).expect("length is valid"),
    );
    response
}

fn unavailable_response(method: &Method, message: &str) -> Response<Body> {
    let mut response = error_response(method, StatusCode::SERVICE_UNAVAILABLE, message);
    response
        .headers_mut()
        .insert(RETRY_AFTER, HeaderValue::from_static("5"));
    response
}

fn generated_response(status: StatusCode, body: Body) -> Response<Body> {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static(GENERATED_CACHE_CONTROL),
    );
    apply_common_headers(&mut response);
    response
}

fn apply_common_headers(response: &mut Response<Body>) {
    response
        .headers_mut()
        .insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
}

fn listing_csp() -> &'static str {
    static CSP: OnceLock<String> = OnceLock::new();
    CSP.get_or_init(|| {
        let digest = Sha256::digest(THEME_SCRIPT.as_bytes());
        format!(
            "default-src 'none'; style-src 'unsafe-inline'; script-src 'sha256-{}'; script-src-attr 'none'; img-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
            STANDARD.encode(digest)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_rejects_duplicate_known_keys_and_bad_percent_encoding() {
        assert!(parse_listing_query(Some("sort=name&sort=size")).is_err());
        assert!(parse_listing_query(Some("s%6frt=name&sort=size")).is_err());
        assert!(parse_listing_query(Some("cursor=%ZZ")).is_err());
        assert!(parse_listing_query(Some("sort=name&ignored=x")).is_ok());
    }

    #[test]
    fn listing_csp_contains_only_the_theme_script_hash() {
        let digest = STANDARD.encode(Sha256::digest(THEME_SCRIPT.as_bytes()));
        assert!(listing_csp().contains(&format!("script-src 'sha256-{digest}'")));
        assert!(!listing_csp().contains("data:"));
    }

    #[test]
    fn request_drop_guard_cancels_only_while_armed() {
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let _guard = CancelOnDrop::new(cancelled.clone());
        }
        assert!(cancelled.load(Ordering::Relaxed));

        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let mut guard = CancelOnDrop::new(cancelled.clone());
            guard.disarm();
        }
        assert!(!cancelled.load(Ordering::Relaxed));
    }
}
