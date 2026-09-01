use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use autoindex_rs::{Config, build_router, open_state};
use axum::body::Body;
use axum::http::header::{
    ALLOW, CONTENT_RANGE, CONTENT_SECURITY_POLICY, CONTENT_TYPE, ETAG, LAST_MODIFIED, LOCATION,
};
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use tempfile::TempDir;
use tower::ServiceExt;

fn config(root: &TempDir) -> Config {
    Config {
        directory: root.path().to_path_buf(),
        bind: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        port: 6701,
        render_readme: true,
        index_files: vec!["index.html".to_string(), "index.htm".to_string()],
        page_size: 100,
        timezone: "Asia/Shanghai".parse().unwrap(),
        timezone_name: "Asia/Shanghai".to_string(),
        log_level: "info".to_string(),
        allow_sensitive_paths: true,
    }
}

async fn request(
    app: &axum::Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
) -> axum::response::Response {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    app.clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn page_link(body: &str, relation: &str) -> String {
    let marker = format!("rel=\"{relation}\" href=\"");
    let value = body
        .split_once(&marker)
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(value, _)| value)
        .expect("page link must exist");
    let value = value.replace("&amp;", "&").replace("&#38;", "&");
    if value.starts_with('?') {
        format!("/{value}")
    } else {
        value
    }
}

fn normalized_listing_headers(headers: &axum::http::HeaderMap) -> String {
    let mut lines = Vec::new();
    for name in [
        "cache-control",
        "content-security-policy",
        "content-type",
        "referrer-policy",
        "x-content-type-options",
        "x-frame-options",
    ] {
        let mut value = headers[name].to_str().unwrap().to_string();
        if name == "content-security-policy" {
            let prefix = "script-src 'sha256-";
            let start = value.find(prefix).unwrap() + prefix.len();
            let end = start + value[start..].find('\'').unwrap();
            value.replace_range(start..end, "<HASH>");
        }
        lines.push(format!("{name}: {value}"));
    }
    format!("{}\n", lines.join("\n"))
}

#[tokio::test]
async fn listing_renders_readme_filters_hidden_entries_and_supports_head() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("docs")).unwrap();
    std::fs::write(root.path().join("hello.txt"), b"hello").unwrap();
    std::fs::write(root.path().join(".env"), b"SECRET=x").unwrap();
    std::fs::write(
        root.path().join("README.md"),
        "# Demo\n\n> [!WARNING]\n> Keep this safe.\n",
    )
    .unwrap();

    let app = build_router(Arc::new(open_state(config(&root)).unwrap()));
    let response = request(&app, Method::GET, "/", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()[CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("script-src 'sha256-")
    );
    let body = body_text(response).await;
    assert!(body.contains("Index of /"));
    assert!(body.contains("docs/"));
    assert!(body.contains("hello.txt"));
    assert!(body.contains("markdown-alert-warning"), "{body}");
    assert!(!body.contains("SECRET=x"));
    assert!(!body.contains(">.env<"));

    let response = request(&app, Method::HEAD, "/", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_ne!(response.headers()["content-length"], "0");
    assert!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
}

#[tokio::test]
async fn invalid_and_oversized_readmes_do_not_break_directory_listings() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("invalid")).unwrap();
    std::fs::create_dir(root.path().join("oversized")).unwrap();
    std::fs::write(
        root.path().join("invalid/README.md"),
        b"# INVALID-README-SECRET\n\xff",
    )
    .unwrap();
    let mut oversized = b"# OVERSIZED-README-SECRET\n".to_vec();
    oversized.resize(1024 * 1024 + 1, b'x');
    std::fs::write(root.path().join("oversized/README.md"), oversized).unwrap();

    let app = build_router(Arc::new(open_state(config(&root)).unwrap()));
    let invalid = request(&app, Method::GET, "/invalid/", &[]).await;
    assert_eq!(invalid.status(), StatusCode::OK);
    assert!(!body_text(invalid).await.contains("INVALID-README-SECRET"));
    let oversized = request(&app, Method::GET, "/oversized/", &[]).await;
    assert_eq!(oversized.status(), StatusCode::OK);
    assert!(
        !body_text(oversized)
            .await
            .contains("OVERSIZED-README-SECRET")
    );
}

#[tokio::test]
async fn empty_listing_matches_the_committed_header_and_html_contracts() {
    let root = TempDir::new().unwrap();
    let mut settings = config(&root);
    settings.render_readme = false;
    let app = build_router(Arc::new(open_state(settings).unwrap()));
    let response = request(&app, Method::GET, "/", &[]).await;
    assert_eq!(
        normalized_listing_headers(response.headers()),
        include_str!("fixtures/listing-headers.golden")
    );
    let body = body_text(response).await;
    for fragment in include_str!("fixtures/listing-contract.golden").lines() {
        assert!(
            body.contains(fragment),
            "missing golden fragment {fragment:?}"
        );
    }
}

#[tokio::test]
async fn redirects_choose_index_files_and_no_index_forces_a_listing() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("docs")).unwrap();
    std::fs::write(root.path().join("docs/index.htm"), b"fallback").unwrap();
    std::fs::write(root.path().join("docs/index.html"), b"preferred").unwrap();

    let app = build_router(Arc::new(open_state(config(&root)).unwrap()));
    let redirect = request(&app, Method::GET, "/docs?x=1&x=2", &[]).await;
    assert_eq!(redirect.status(), StatusCode::MOVED_PERMANENTLY);
    assert_eq!(redirect.headers()[LOCATION], "/docs/?x=1&x=2");

    let index = request(&app, Method::GET, "/docs/", &[]).await;
    assert_eq!(index.status(), StatusCode::OK);
    assert_eq!(body_text(index).await, "preferred");

    let mut without_index = config(&root);
    without_index.index_files.clear();
    let app = build_router(Arc::new(open_state(without_index).unwrap()));
    let listing = body_text(request(&app, Method::GET, "/docs/", &[]).await).await;
    assert!(listing.contains("index.html"));
    assert!(listing.contains("index.htm"));
}

#[tokio::test]
async fn files_support_validators_ranges_and_method_rejection() {
    let root = TempDir::new().unwrap();
    std::fs::write(root.path().join("ten.txt"), b"0123456789").unwrap();
    let app = build_router(Arc::new(open_state(config(&root)).unwrap()));

    let response = request(&app, Method::GET, "/ten.txt", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    let etag = response.headers()[ETAG].to_str().unwrap().to_string();
    let last_modified = response.headers()[LAST_MODIFIED]
        .to_str()
        .unwrap()
        .to_string();
    assert!(etag.starts_with("W/\""));
    assert_eq!(
        response.headers()[CONTENT_TYPE],
        "text/plain; charset=utf-8"
    );
    assert_eq!(body_text(response).await, "0123456789");

    let response = request(&app, Method::GET, "/ten.txt", &[("if-none-match", &etag)]).await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);

    let response = request(
        &app,
        Method::GET,
        "/ten.txt",
        &[("if-match", "\"different\"")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);

    let response = request(
        &app,
        Method::GET,
        "/ten.txt",
        &[("if-unmodified-since", "Thu, 01 Jan 1970 00:00:00 GMT")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);

    let response = request(
        &app,
        Method::GET,
        "/ten.txt",
        &[("if-modified-since", &last_modified)],
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);

    let response = request(
        &app,
        Method::GET,
        "/ten.txt",
        &[
            ("if-none-match", "\"different\""),
            ("if-modified-since", "Thu, 01 Jan 2099 00:00:00 GMT"),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_text(response).await, "0123456789");

    let response = request(&app, Method::GET, "/ten.txt", &[("range", "bytes=2-5")]).await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()[CONTENT_RANGE], "bytes 2-5/10");
    assert_eq!(body_text(response).await, "2345");

    let response = request(
        &app,
        Method::GET,
        "/ten.txt",
        &[("range", "bytes=2-5"), ("if-range", "\"different\"")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_text(response).await, "0123456789");

    let response = request(&app, Method::HEAD, "/ten.txt", &[("range", "bytes=2-5")]).await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()[CONTENT_RANGE], "bytes 2-5/10");
    assert!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );

    let response = request(&app, Method::GET, "/ten.txt", &[("range", "bytes=99-100")]).await;
    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(response.headers()[CONTENT_RANGE], "bytes */10");

    let response = request(&app, Method::GET, "/ten.txt", &[("range", "bytes=0-1,8-9")]).await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    let content_type = response.headers()["content-type"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(content_type.starts_with("multipart/byteranges; boundary="));
    let body = body_text(response).await;
    assert!(body.contains("Content-Range: bytes 0-1/10"));
    assert!(body.contains("Content-Range: bytes 8-9/10"));
    assert!(body.contains("\r\n01\r\n"));
    assert!(body.contains("\r\n89\r\n"));

    let response = request(&app, Method::GET, "/ten.txt", &[("range", "bytes=0-9,0-9")]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_text(response).await, "0123456789");

    let response = request(&app, Method::POST, "/ten.txt", &[]).await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()[ALLOW], "GET, HEAD");
}

#[tokio::test]
async fn traversal_hidden_paths_and_malformed_queries_fail_closed() {
    let root = TempDir::new().unwrap();
    std::fs::write(root.path().join("visible.txt"), b"visible").unwrap();
    let app = build_router(Arc::new(open_state(config(&root)).unwrap()));

    for uri in [
        "/.env",
        "/%2e%2e/secret",
        "/%252e%252e/secret",
        "/a%252fb",
        "/__private/file",
        "/?sort=name&sort=size",
        "/?sort=",
        "/?order=",
        "/?cursor",
        "/?cursor=",
        "/?cursor=%ZZ",
    ] {
        let response = request(&app, Method::GET, uri, &[]).await;
        assert!(
            matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::BAD_REQUEST
            ),
            "{uri} returned {}",
            response.status()
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn malicious_and_non_utf8_names_are_escaped_or_hidden() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let root = TempDir::new().unwrap();
    let html_name = "\"><img src=x onerror=alert(1)>.txt";
    let punctuation_name = "#?%&'.txt";
    std::fs::write(root.path().join(html_name), b"visible").unwrap();
    std::fs::write(root.path().join(punctuation_name), b"visible").unwrap();
    std::fs::write(root.path().join("CONTROL-SECRET\nNAME.txt"), b"hidden").unwrap();
    let _ = std::fs::write(
        root.path()
            .join(OsString::from_vec(vec![b'n', b'o', b'n', 0xff])),
        b"hidden",
    );

    let app = build_router(Arc::new(open_state(config(&root)).unwrap()));
    let body = body_text(request(&app, Method::GET, "/", &[]).await).await;
    assert!(
        body.contains("&#34;&#62;&#60;img src=x onerror=alert(1)&#62;.txt"),
        "{body}"
    );
    assert!(body.contains("./%22%3E%3Cimg%20src=x%20onerror=alert(1)%3E.txt"));
    assert!(body.contains("./%23%3F%25&#38;&#39;.txt"), "{body}");
    assert!(!body.contains("<img src=x onerror=alert(1)>"));
    assert!(!body.contains("CONTROL-SECRET"));
}

#[tokio::test]
async fn each_directory_uses_only_its_own_readme() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("child")).unwrap();
    std::fs::write(root.path().join("README.md"), "# ROOT-README-MARKER\n").unwrap();
    std::fs::write(
        root.path().join("child/README.md"),
        "# CHILD-README-MARKER\n",
    )
    .unwrap();

    let app = build_router(Arc::new(open_state(config(&root)).unwrap()));
    let root_page = body_text(request(&app, Method::GET, "/", &[]).await).await;
    assert!(root_page.contains("ROOT-README-MARKER"));
    assert!(!root_page.contains("CHILD-README-MARKER"));
    let child_page = body_text(request(&app, Method::GET, "/child/", &[]).await).await;
    assert!(child_page.contains("CHILD-README-MARKER"));
    assert!(!child_page.contains("ROOT-README-MARKER"));
}

#[tokio::test]
async fn unicode_directory_links_are_encoded_but_titles_are_readable() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("文档 1")).unwrap();
    let app = build_router(Arc::new(open_state(config(&root)).unwrap()));

    let root_page = body_text(request(&app, Method::GET, "/", &[]).await).await;
    assert!(
        root_page.contains("./%E6%96%87%E6%A1%A3%201/"),
        "{root_page}"
    );
    let child_page =
        body_text(request(&app, Method::GET, "/%E6%96%87%E6%A1%A3%201/", &[]).await).await;
    assert!(child_page.contains("Index of /文档 1/"), "{child_page}");
    assert!(!child_page.contains("Index of /%E6%96%87"), "{child_page}");
}

#[cfg(unix)]
#[tokio::test]
async fn startup_pins_the_opened_root_instead_of_following_path_replacements() {
    let workspace = TempDir::new().unwrap();
    let served = workspace.path().join("public");
    let moved = workspace.path().join("opened-root");
    std::fs::create_dir(&served).unwrap();
    std::fs::write(served.join("original.txt"), b"original").unwrap();
    let mut settings = config(&workspace);
    settings.directory = served.clone();
    let app = build_router(Arc::new(open_state(settings).unwrap()));

    std::fs::rename(&served, &moved).unwrap();
    std::fs::create_dir(&served).unwrap();
    std::fs::write(served.join("replacement.txt"), b"replacement").unwrap();

    let original = request(&app, Method::GET, "/original.txt", &[]).await;
    assert_eq!(original.status(), StatusCode::OK);
    assert_eq!(body_text(original).await, "original");
    let replacement = request(&app, Method::GET, "/replacement.txt", &[]).await;
    assert_eq!(replacement.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cursor_pages_remain_well_formed_when_the_directory_changes() {
    let root = TempDir::new().unwrap();
    for index in 0..5 {
        std::fs::write(root.path().join(format!("item-{index:02}.txt")), b"x").unwrap();
    }
    let mut settings = config(&root);
    settings.page_size = 2;
    let app = build_router(Arc::new(open_state(settings).unwrap()));

    let first = body_text(request(&app, Method::GET, "/", &[]).await).await;
    assert!(first.contains("item-00.txt"));
    assert!(first.contains("item-01.txt"));
    let next = page_link(&first, "next");

    std::fs::remove_file(root.path().join("item-00.txt")).unwrap();
    std::fs::rename(
        root.path().join("item-04.txt"),
        root.path().join("item-005.txt"),
    )
    .unwrap();
    std::fs::write(root.path().join("item-99.txt"), b"new").unwrap();

    let second = body_text(request(&app, Method::GET, &next, &[]).await).await;
    assert!(second.contains("item-02.txt"), "{second}");
    assert!(second.contains("item-03.txt"), "{second}");
    assert!(!second.contains("item-005.txt"), "{second}");
    let previous = page_link(&second, "prev");
    let back = body_text(request(&app, Method::GET, &previous, &[]).await).await;
    assert!(back.contains("item-005.txt"), "{back}");
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_work_inside_the_root_but_cannot_escape_it() {
    use std::os::unix::fs::symlink;

    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    std::fs::write(root.path().join("target.txt"), b"inside").unwrap();
    std::fs::write(outside.path().join("secret.txt"), b"outside").unwrap();
    symlink("target.txt", root.path().join("inside-link.txt")).unwrap();
    symlink(
        outside.path().join("secret.txt"),
        root.path().join("escape.txt"),
    )
    .unwrap();

    let app = build_router(Arc::new(open_state(config(&root)).unwrap()));
    let inside = request(&app, Method::GET, "/inside-link.txt", &[]).await;
    assert_eq!(inside.status(), StatusCode::OK);
    assert_eq!(body_text(inside).await, "inside");

    let escape = request(&app, Method::GET, "/escape.txt", &[]).await;
    assert_eq!(escape.status(), StatusCode::NOT_FOUND);
}

#[cfg(unix)]
#[tokio::test]
async fn special_files_are_not_listed_or_opened() {
    let root = TempDir::new().unwrap();
    let status = std::process::Command::new("mkfifo")
        .arg(root.path().join("named-pipe"))
        .status()
        .unwrap();
    assert!(status.success());

    let app = build_router(Arc::new(open_state(config(&root)).unwrap()));
    let listing = body_text(request(&app, Method::GET, "/", &[]).await).await;
    assert!(!listing.contains("named-pipe"));

    let response = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        request(&app, Method::GET, "/named-pipe", &[]),
    )
    .await
    .expect("opening a special file must not block");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn symlink_target_swaps_never_disclose_hidden_or_outside_files() {
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicBool, Ordering};

    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    std::fs::write(root.path().join("public.txt"), b"public-bytes").unwrap();
    std::fs::write(root.path().join(".hidden.txt"), b"hidden-secret").unwrap();
    std::fs::write(outside.path().join("outside.txt"), b"outside-secret").unwrap();
    let alias = root.path().join("alias.txt");
    symlink("public.txt", &alias).unwrap();

    let running = Arc::new(AtomicBool::new(true));
    let updater_running = running.clone();
    let updater_root = root.path().to_path_buf();
    let outside_target = outside.path().join("outside.txt");
    let updater = std::thread::spawn(move || {
        let targets = [
            std::path::PathBuf::from("public.txt"),
            std::path::PathBuf::from(".hidden.txt"),
            outside_target,
        ];
        let mut index = 0usize;
        while updater_running.load(Ordering::Relaxed) {
            let temporary = updater_root.join(".alias-swap");
            let _ = std::fs::remove_file(&temporary);
            if symlink(&targets[index % targets.len()], &temporary).is_ok() {
                let _ = std::fs::rename(&temporary, updater_root.join("alias.txt"));
            }
            index += 1;
            std::thread::yield_now();
        }
    });

    let app = build_router(Arc::new(open_state(config(&root)).unwrap()));
    let mut disclosed = None;
    for _ in 0..300 {
        let response = request(&app, Method::GET, "/alias.txt", &[]).await;
        if response.status() == StatusCode::OK {
            let body = body_text(response).await;
            if body != "public-bytes" {
                disclosed = Some(body);
                break;
            }
        } else if response.status() != StatusCode::NOT_FOUND {
            disclosed = Some(format!("unexpected status {}", response.status()));
            break;
        }
    }
    running.store(false, Ordering::Relaxed);
    updater.join().unwrap();
    assert_eq!(disclosed, None);
}
