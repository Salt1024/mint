use axum::{
    Router,
    body::{Body, Bytes},
    extract::ConnectInfo,
    http::{Request, Response, StatusCode},
};
use http_body_util::BodyExt;
use mint::{AppState, Config, router};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{Cursor, Read},
    net::SocketAddr,
    path::PathBuf,
    time::{Duration, Instant},
};
use tempfile::TempDir;
use tower::ServiceExt;

const PASSWORD: &str = "test-password";
const SPECIAL_NAME: &str = "中文 + #%2e%2e.txt";

struct Fixture {
    _directory: TempDir,
    root: PathBuf,
    app: Router,
    content: Vec<u8>,
}

fn config(root: PathBuf, cookie_secure: bool) -> Config {
    Config {
        root,
        username: "admin".into(),
        password_hash: bcrypt::hash(PASSWORD, 4).unwrap(),
        host: "127.0.0.1".into(),
        port: 3000,
        cookie_secure,
    }
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("files");
        fs::create_dir_all(root.join("reports")).unwrap();
        fs::create_dir(root.join("read-only")).unwrap();
        fs::create_dir(directory.path().join("files-neighbor")).unwrap();
        let content: Vec<u8> = (0..1024).map(|index| (index % 251) as u8).collect();
        for (name, bytes) in [
            ("download.bin", content.as_slice()),
            ("short.txt", b"0123456789"),
            ("empty.txt", b""),
            (".hidden", b"hidden content"),
            (SPECIAL_NAME, b"special content"),
            ("reports/nested.txt", b"nested content"),
            ("read-only/protected.txt", b"protected"),
        ] {
            fs::write(root.join(name), bytes).unwrap();
        }
        fs::write(
            directory.path().join("files-neighbor/secret.txt"),
            b"outside root",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(
                directory.path().join("files-neighbor"),
                root.join("linked-directory"),
            )
            .unwrap();
            symlink(
                directory.path().join("files-neighbor/secret.txt"),
                root.join("linked-file"),
            )
            .unwrap();
            symlink(root.join("reports"), root.join("linked-inside")).unwrap();
        }
        let app = router(AppState::new(config(root.clone(), false)));
        Self {
            _directory: directory,
            root,
            app,
            content,
        }
    }

    async fn login(&self, previous: Option<&str>) -> (String, String) {
        let mut headers = vec![("content-type", "application/json")];
        if let Some(cookie) = previous {
            headers.push(("cookie", cookie));
        }
        let response = send(
            &self.app,
            "POST",
            "/api/auth/login",
            &headers,
            r#"{"username":"admin","password":"test-password"}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let set_cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .to_owned();
        let cookie = set_cookie.split(';').next().unwrap().to_owned();
        assert_eq!(json_body(response).await, json!({ "username": "admin" }));
        (cookie, set_cookie)
    }

    async fn download(&self, name: &str, cookie: &str, range: Option<&str>) -> Response<Body> {
        let mut headers = vec![("cookie", cookie)];
        if let Some(range) = range {
            headers.push(("range", range));
        }
        send(
            &self.app,
            "GET",
            &path_uri("/api/files/download", name),
            &headers,
            "",
        )
        .await
    }

    async fn archive(&self, paths: &[&str], cookie: &str) -> Response<Body> {
        let form = serde_urlencoded::to_string([("paths", json!(paths).to_string())]).unwrap();
        send(
            &self.app,
            "POST",
            "/api/files/archive",
            &[
                ("cookie", cookie),
                ("content-type", "application/x-www-form-urlencoded"),
            ],
            &form,
        )
        .await
    }

    async fn delete_many(&self, paths: &[&str], cookie: &str) -> Response<Body> {
        send(
            &self.app,
            "POST",
            "/api/files/delete",
            &[("cookie", cookie), ("content-type", "application/json")],
            &json!({ "paths": paths }).to_string(),
        )
        .await
    }
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Response<Body> {
    let mut request = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let mut request = request.body(Body::from(body.to_owned())).unwrap();
    request.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:43210".parse::<SocketAddr>().unwrap(),
    ));
    app.clone().oneshot(request).await.unwrap()
}

fn path_uri(base: &str, name: &str) -> String {
    format!(
        "{base}?path={}",
        utf8_percent_encode(name, NON_ALPHANUMERIC)
    )
}

async fn bytes(response: Response<Body>) -> Bytes {
    response.into_body().collect().await.unwrap().to_bytes()
}

async fn json_body(response: Response<Body>) -> Value {
    serde_json::from_slice(&bytes(response).await).unwrap()
}

async fn zip_entries(response: Response<Body>) -> BTreeMap<String, Vec<u8>> {
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/zip")
    );
    assert!(
        response.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment;")
    );
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    assert!(!response.headers().contains_key("content-encoding"));
    let data = bytes(response).await;
    let mut archive = zip::ZipArchive::new(Cursor::new(data)).unwrap();
    let mut entries = BTreeMap::new();
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).unwrap();
        assert_eq!(file.compression(), zip::CompressionMethod::Stored);
        assert!(
            file.enclosed_name().is_some(),
            "unsafe ZIP path: {}",
            file.name()
        );
        let name = file.name().to_owned();
        let mut content = Vec::new();
        file.read_to_end(&mut content).unwrap();
        assert!(
            entries.insert(name, content).is_none(),
            "duplicate ZIP entry"
        );
    }
    entries
}

#[tokio::test]
async fn every_file_operation_and_me_require_a_session() {
    let fixture = Fixture::new();
    for (method, uri) in [
        ("GET", "/api/files"),
        ("GET", "/api/files/download?path=short.txt"),
        ("HEAD", "/api/files/download?path=short.txt"),
        ("GET", "/api/files/preview?path=photo.png"),
        ("HEAD", "/api/files/preview?path=photo.png"),
        ("GET", "/api/files/archive?path=reports"),
        ("POST", "/api/files/archive"),
        ("POST", "/api/files/delete"),
        ("DELETE", "/api/files?path=short.txt"),
        ("GET", "/api/auth/me"),
    ] {
        let response = send(&fixture.app, method, uri, &[], "").await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {uri}"
        );
        assert_eq!(response.headers()["cache-control"], "private, no-store");
    }
}

#[tokio::test]
async fn login_rejects_wrong_credentials_and_non_json_requests() {
    let fixture = Fixture::new();
    let response = send(
        &fixture.app,
        "POST",
        "/api/auth/login",
        &[("content-type", "application/json")],
        r#"{"username":"admin","password":"wrong"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = json_body(response).await;
    assert_eq!(body["message"], "账号或密码错误");
    assert_eq!(body["statusCode"], 401);
    assert!(body["error"].is_string());

    let response = send(
        &fixture.app,
        "POST",
        "/api/auth/login",
        &[("content-type", "application/x-www-form-urlencoded")],
        "username=admin&password=test-password",
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn login_validates_required_fields_and_bcrypt_byte_limit() {
    let fixture = Fixture::new();
    for body in [
        json!({}),
        json!({ "username": "admin", "password": "" }),
        json!({ "username": [], "password": PASSWORD }),
        json!({ "username": "admin", "password": "密".repeat(25) }),
    ] {
        let response = send(
            &fixture.app,
            "POST",
            "/api/auth/login",
            &[("content-type", "application/json")],
            &body.to_string(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn login_replaces_the_current_private_cookie_and_logout_invalidates_it() {
    let fixture = Fixture::new();
    let (first, header) = fixture.login(None).await;
    assert!(header.contains("HttpOnly"));
    assert!(header.contains("SameSite=Strict"));
    assert!(header.contains("Path=/api"));
    assert!(header.contains("Max-Age=43200"));
    assert!(!header.contains("Secure"));
    let (second, _) = fixture.login(Some(&first)).await;
    assert_ne!(first, second);
    let old = send(
        &fixture.app,
        "GET",
        "/api/auth/me",
        &[("cookie", &first)],
        "",
    )
    .await;
    assert_eq!(old.status(), StatusCode::UNAUTHORIZED);
    let current = send(
        &fixture.app,
        "GET",
        "/api/auth/me",
        &[("cookie", &second)],
        "",
    )
    .await;
    assert_eq!(current.status(), StatusCode::OK);
    assert_eq!(json_body(current).await, json!({ "username": "admin" }));
    let logout = send(
        &fixture.app,
        "POST",
        "/api/auth/logout",
        &[("cookie", &second)],
        "",
    )
    .await;
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    let clear = logout.headers()["set-cookie"].to_str().unwrap();
    assert!(clear.contains("Path=/api"));
    assert!(clear.contains("Max-Age=0") || clear.contains("Expires=Thu, 01 Jan 1970"));
    let denied = send(
        &fixture.app,
        "GET",
        "/api/files",
        &[("cookie", &second)],
        "",
    )
    .await;
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    let again = send(&fixture.app, "POST", "/api/auth/logout", &[], "").await;
    assert_eq!(again.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn secure_cookies_require_https() {
    let fixture = Fixture::new();
    let secure = router(AppState::new(config(fixture.root.clone(), true)));
    let response = send(
        &secure,
        "POST",
        "/api/auth/login",
        &[("content-type", "application/json")],
        r#"{"username":"admin","password":"test-password"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Secure")
    );
}

#[tokio::test]
async fn sessions_are_not_shared_across_application_restarts() {
    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    let restarted = router(AppState::new(config(fixture.root.clone(), false)));
    let response = send(&restarted, "GET", "/api/files", &[("cookie", &cookie)], "").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn lists_only_immediate_regular_entries_with_directories_first() {
    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    let response = send(
        &fixture.app,
        "GET",
        "/api/files",
        &[("cookie", &cookie)],
        "",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    let body = json_body(response).await;
    assert_eq!(body["path"], "");
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries[0]["name"], "read-only");
    assert_eq!(entries[1]["name"], "reports");
    for entry in &entries[..2] {
        assert_eq!(entry["type"], "directory");
        assert!(entry["size"].is_null());
    }
    for name in [".hidden", SPECIAL_NAME, "download.bin"] {
        assert!(entries.iter().any(|entry| entry["name"] == name));
    }
    for name in [
        "nested.txt",
        "linked-file",
        "linked-directory",
        "linked-inside",
    ] {
        assert!(!entries.iter().any(|entry| entry["name"] == name));
    }
    assert_eq!(
        entries
            .iter()
            .find(|entry| entry["name"] == "download.bin")
            .unwrap()["size"],
        fixture.content.len()
    );
    for entry in entries {
        chrono::DateTime::parse_from_rfc3339(entry["modifiedAt"].as_str().unwrap()).unwrap();
    }
    assert!(!body.to_string().contains(fixture.root.to_str().unwrap()));
}

#[tokio::test]
async fn browses_subdirectories_using_relative_paths() {
    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    let response = send(
        &fixture.app,
        "GET",
        "/api/files?path=reports",
        &[("cookie", &cookie)],
        "",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["path"], "reports");
    assert_eq!(body["entries"].as_array().unwrap().len(), 1);
    assert_eq!(body["entries"][0]["path"], "reports/nested.txt");
    assert_eq!(body["entries"][0]["type"], "file");
}

#[tokio::test]
async fn rejects_traversal_absolute_paths_and_duplicate_parameters() {
    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    for path in [
        "../files-neighbor/secret.txt",
        fixture.root.to_str().unwrap(),
        "reports/../../files-neighbor",
        "reports\\nested.txt",
        "reports/../short.txt",
        ".",
        "\0",
    ] {
        let response = fixture.download(path, &cookie, None).await;
        assert!(
            [StatusCode::BAD_REQUEST, StatusCode::FORBIDDEN].contains(&response.status()),
            "{path:?} returned {}",
            response.status()
        );
        let error = json_body(response).await;
        assert!(!error.to_string().contains(fixture.root.to_str().unwrap()));
    }
    let duplicate = send(
        &fixture.app,
        "GET",
        "/api/files?path=reports&path=short.txt",
        &[("cookie", &cookie)],
        "",
    )
    .await;
    assert_eq!(duplicate.status(), StatusCode::BAD_REQUEST);
    let encoded = send(
        &fixture.app,
        "GET",
        "/api/files/download?path=%2e%2e%2ffiles-neighbor%2fsecret.txt",
        &[("cookie", &cookie)],
        "",
    )
    .await;
    assert_eq!(encoded.status(), StatusCode::FORBIDDEN);
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_symlinks_to_both_inside_and_outside_the_root() {
    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    for path in [
        "linked-file",
        "linked-directory/secret.txt",
        "linked-inside/nested.txt",
    ] {
        for (method, endpoint) in [("GET", "/api/files/download"), ("DELETE", "/api/files")] {
            let response = send(
                &fixture.app,
                method,
                &path_uri(endpoint, path),
                &[("cookie", &cookie)],
                "",
            )
            .await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method} {path}");
        }
    }
    let response = send(
        &fixture.app,
        "GET",
        "/api/files?path=linked-directory",
        &[("cookie", &cookie)],
        "",
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        fs::read(
            fixture
                .root
                .parent()
                .unwrap()
                .join("files-neighbor/secret.txt")
        )
        .unwrap(),
        b"outside root"
    );
}

#[tokio::test]
async fn downloads_full_files_hidden_files_and_encoded_filenames() {
    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    let response = fixture.download("download.bin", &cookie, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-length"],
        fixture.content.len().to_string()
    );
    assert_eq!(response.headers()["accept-ranges"], "bytes");
    assert!(
        response.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment;")
    );
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    assert!(!response.headers().contains_key("content-encoding"));
    assert_eq!(bytes(response).await.as_ref(), fixture.content);

    let hidden = fixture.download(".hidden", &cookie, None).await;
    assert_eq!(hidden.status(), StatusCode::OK);
    assert_eq!(bytes(hidden).await.as_ref(), b"hidden content");
    let special = fixture.download(SPECIAL_NAME, &cookie, None).await;
    assert_eq!(special.status(), StatusCode::OK);
    assert!(
        special.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .contains("filename*=UTF-8''")
    );
    assert_eq!(bytes(special).await.as_ref(), b"special content");
}

#[tokio::test]
async fn serves_closed_open_suffix_and_truncated_ranges() {
    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    for (range, start, end) in [
        ("bytes=0-99", 0, 99),
        ("bytes=900-", 900, 1023),
        ("bytes=-100", 924, 1023),
        ("bytes=1000-9999", 1000, 1023),
    ] {
        let response = fixture.download("download.bin", &cookie, Some(range)).await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT, "{range}");
        assert_eq!(
            response.headers()["content-range"],
            format!("bytes {start}-{end}/1024")
        );
        assert_eq!(
            response.headers()["content-length"],
            (end - start + 1).to_string()
        );
        assert_eq!(
            bytes(response).await.as_ref(),
            &fixture.content[start..=end]
        );
    }
}

#[tokio::test]
async fn clamps_an_oversized_suffix_to_the_whole_file() {
    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    let response = fixture
        .download("short.txt", &cookie, Some("bytes=-100"))
        .await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()["content-range"], "bytes 0-9/10");
    assert_eq!(bytes(response).await.as_ref(), b"0123456789");
}

#[tokio::test]
async fn head_ignores_range_and_returns_full_metadata_without_a_body() {
    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    let response = send(
        &fixture.app,
        "HEAD",
        "/api/files/download?path=download.bin",
        &[("cookie", &cookie), ("range", "bytes=0-9")],
        "",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-length"], "1024");
    assert_eq!(response.headers()["accept-ranges"], "bytes");
    assert!(!response.headers().contains_key("content-range"));
    assert!(bytes(response).await.is_empty());
}

#[tokio::test]
async fn unsatisfiable_ranges_return_416_and_empty_files_are_supported() {
    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    let response = fixture
        .download("download.bin", &cookie, Some("bytes=1024-"))
        .await;
    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(response.headers()["content-range"], "bytes */1024");
    let empty = fixture.download("empty.txt", &cookie, None).await;
    assert_eq!(empty.status(), StatusCode::OK);
    assert_eq!(empty.headers()["content-length"], "0");
    assert!(bytes(empty).await.is_empty());
    let empty_range = fixture
        .download("empty.txt", &cookie, Some("bytes=0-"))
        .await;
    assert_eq!(empty_range.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(empty_range.headers()["content-range"], "bytes */0");
}

#[tokio::test]
async fn if_range_mismatch_returns_the_complete_file_and_matching_date_keeps_the_range() {
    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    let response = send(
        &fixture.app,
        "GET",
        "/api/files/download?path=download.bin",
        &[
            ("cookie", &cookie),
            ("range", "bytes=0-9"),
            ("if-range", "\"another-version\""),
        ],
        "",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key("content-range"));
    let modified = response.headers()["last-modified"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(bytes(response).await.as_ref(), fixture.content);

    let matching = send(
        &fixture.app,
        "GET",
        "/api/files/download?path=download.bin",
        &[
            ("cookie", &cookie),
            ("range", "bytes=0-9"),
            ("if-range", &modified),
        ],
        "",
    )
    .await;
    assert_eq!(matching.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(bytes(matching).await.as_ref(), &fixture.content[..10]);
}

#[tokio::test]
async fn concurrent_ranges_reconstruct_the_source_file() {
    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    let mut downloads = tokio::task::JoinSet::new();
    for index in 0..4 {
        let app = fixture.app.clone();
        let cookie = cookie.clone();
        downloads.spawn(async move {
            let range = format!("bytes={}-{}", index * 256, index * 256 + 255);
            let response = send(
                &app,
                "GET",
                "/api/files/download?path=download.bin",
                &[("cookie", &cookie), ("range", &range)],
                "",
            )
            .await;
            assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
            (index, bytes(response).await)
        });
    }
    let mut parts = vec![Bytes::new(); 4];
    while let Some(result) = downloads.join_next().await {
        let (index, content) = result.unwrap();
        parts[index] = content;
    }
    let merged: Vec<u8> = parts.iter().flat_map(|part| part.iter().copied()).collect();
    assert_eq!(merged, fixture.content);
}

#[tokio::test]
async fn directories_cannot_be_downloaded_or_deleted_and_missing_files_return_404() {
    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    let directory = fixture.download("reports", &cookie, None).await;
    assert_eq!(directory.status(), StatusCode::BAD_REQUEST);
    let missing = fixture.download("missing.txt", &cookie, None).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    for path in ["reports", ""] {
        let response = send(
            &fixture.app,
            "DELETE",
            &path_uri("/api/files", path),
            &[("cookie", &cookie)],
            "",
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    let file_listing = send(
        &fixture.app,
        "GET",
        "/api/files?path=short.txt",
        &[("cookie", &cookie)],
        "",
    )
    .await;
    assert_eq!(file_listing.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn deletes_only_the_requested_regular_file_and_reports_it_missing_afterward() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("delete-me.txt"), b"delete me").unwrap();
    let (cookie, _) = fixture.login(None).await;
    let response = send(
        &fixture.app,
        "DELETE",
        "/api/files?path=delete-me.txt",
        &[("cookie", &cookie)],
        "",
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(!fixture.root.join("delete-me.txt").exists());
    assert!(fixture.root.join("short.txt").exists());
    let second = send(
        &fixture.app,
        "DELETE",
        "/api/files?path=delete-me.txt",
        &[("cookie", &cookie)],
        "",
    )
    .await;
    assert_eq!(second.status(), StatusCode::NOT_FOUND);
    let listing = send(
        &fixture.app,
        "GET",
        "/api/files",
        &[("cookie", &cookie)],
        "",
    )
    .await;
    assert!(
        !json_body(listing).await["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "delete-me.txt")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn filesystem_permission_denials_are_reported_without_removing_files() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    let directory = fixture.root.join("read-only");
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(directory.join("probe"), b"probe").is_ok() {
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        return; // Root and equivalent capabilities bypass directory write restrictions.
    }
    let response = send(
        &fixture.app,
        "DELETE",
        "/api/files?path=read-only%2Fprotected.txt",
        &[("cookie", &cookie)],
        "",
    )
    .await;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(directory.join("protected.txt").exists());
}

#[tokio::test]
async fn limits_login_attempts_per_source_ip() {
    let fixture = Fixture::new();
    for _ in 0..10 {
        let response = send(
            &fixture.app,
            "POST",
            "/api/auth/login",
            &[("content-type", "application/json")],
            r#"{"username":"admin","password":"wrong"}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let denied = send(
        &fixture.app,
        "POST",
        "/api/auth/login",
        &[("content-type", "application/json")],
        r#"{"username":"admin","password":"wrong"}"#,
    )
    .await;
    assert_eq!(denied.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn embedded_frontend_is_served_without_exposing_unknown_api_routes() {
    let fixture = Fixture::new();
    let page = send(&fixture.app, "GET", "/", &[], "").await;
    assert_eq!(page.status(), StatusCode::OK);
    assert!(
        page.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    let html = String::from_utf8(bytes(page).await.to_vec()).unwrap();
    assert!(html.contains("<div id=\"app\"></div>"));
    let unknown = send(&fixture.app, "GET", "/api/not-found", &[], "").await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert!(
        !String::from_utf8(bytes(unknown).await.to_vec())
            .unwrap()
            .contains("<div id=\"app\"></div>")
    );
}

#[tokio::test]
async fn common_media_previews_are_inline_and_downloads_remain_attachments() {
    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    for (name, mime) in [
        ("图片 #%.png", "image/png"),
        ("movie.mp4", "video/mp4"),
        ("recording.mp3", "audio/mpeg"),
        ("recording.oga", "audio/ogg"),
    ] {
        fs::write(fixture.root.join(name), &fixture.content).unwrap();
        let preview = send(
            &fixture.app,
            "GET",
            &path_uri("/api/files/preview", name),
            &[("cookie", &cookie)],
            "",
        )
        .await;
        assert_eq!(preview.status(), StatusCode::OK);
        assert_eq!(preview.headers()["content-type"], mime);
        assert!(
            preview.headers()["content-disposition"]
                .to_str()
                .unwrap()
                .starts_with("inline;")
        );
        assert_eq!(preview.headers()["cache-control"], "private, no-store");
        assert_eq!(bytes(preview).await.as_ref(), fixture.content);

        let download = fixture.download(name, &cookie, None).await;
        assert_eq!(download.status(), StatusCode::OK);
        assert!(
            download.headers()["content-disposition"]
                .to_str()
                .unwrap()
                .starts_with("attachment;")
        );
        assert_eq!(bytes(download).await.as_ref(), fixture.content);
    }
}

#[tokio::test]
async fn media_previews_support_range_and_head_but_reject_active_documents() {
    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    fs::write(fixture.root.join("movie.webm"), &fixture.content).unwrap();
    let preview = send(
        &fixture.app,
        "GET",
        "/api/files/preview?path=movie.webm",
        &[("cookie", &cookie), ("range", "bytes=100-199")],
        "",
    )
    .await;
    assert_eq!(preview.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(preview.headers()["content-range"], "bytes 100-199/1024");
    assert_eq!(bytes(preview).await.as_ref(), &fixture.content[100..200]);
    let head = send(
        &fixture.app,
        "HEAD",
        "/api/files/preview?path=movie.webm",
        &[("cookie", &cookie), ("range", "bytes=0-9")],
        "",
    )
    .await;
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(head.headers()["content-length"], "1024");
    assert!(bytes(head).await.is_empty());

    for name in ["page.html", "drawing.svg", "notes.txt"] {
        fs::write(fixture.root.join(name), b"document content").unwrap();
        let response = send(
            &fixture.app,
            "GET",
            &path_uri("/api/files/preview", name),
            &[("cookie", &cookie)],
            "",
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "{name}"
        );
    }
}

#[tokio::test]
async fn selected_files_stream_as_an_uncompressed_zip_with_original_names_and_bytes() {
    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    let response = fixture
        .archive(
            &[SPECIAL_NAME, "short.txt", "empty.txt", ".hidden"],
            &cookie,
        )
        .await;
    let entries = zip_entries(response).await;
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[SPECIAL_NAME], b"special content");
    assert_eq!(entries["short.txt"], b"0123456789");
    assert!(entries["empty.txt"].is_empty());
    assert_eq!(entries[".hidden"], b"hidden content");
    assert!(!fixture.root.join("download.zip").exists());
}

#[tokio::test]
async fn a_directory_zip_preserves_nested_files_empty_directories_and_skips_links() {
    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    fs::create_dir(fixture.root.join("reports/空目录")).unwrap();
    fs::write(fixture.root.join("reports/.hidden"), b"hidden report").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        fs::write(
            fixture.root.join("reports/..\\escape.txt"),
            b"unsafe archive path",
        )
        .unwrap();
        symlink(
            fixture.root.parent().unwrap().join("files-neighbor"),
            fixture.root.join("reports/external-link"),
        )
        .unwrap();
        symlink(
            fixture.root.join("short.txt"),
            fixture.root.join("reports/internal-link"),
        )
        .unwrap();
    }
    let response = send(
        &fixture.app,
        "GET",
        "/api/files/archive?path=reports",
        &[("cookie", &cookie)],
        "",
    )
    .await;
    let entries = zip_entries(response).await;
    assert_eq!(entries.len(), 4);
    assert!(entries["reports/"].is_empty());
    assert!(entries["reports/空目录/"].is_empty());
    assert_eq!(entries["reports/nested.txt"], b"nested content");
    assert_eq!(entries["reports/.hidden"], b"hidden report");
    assert_eq!(
        fs::read(
            fixture
                .root
                .parent()
                .unwrap()
                .join("files-neighbor/secret.txt")
        )
        .unwrap(),
        b"outside root"
    );
}

#[tokio::test]
async fn same_directory_files_and_folders_can_be_archived_together() {
    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    fs::create_dir(fixture.root.join("另一目录")).unwrap();
    let entries = zip_entries(
        fixture
            .archive(&["short.txt", "reports", "另一目录"], &cookie)
            .await,
    )
    .await;
    assert_eq!(entries.len(), 4);
    assert_eq!(entries["short.txt"], b"0123456789");
    assert!(entries["reports/"].is_empty());
    assert_eq!(entries["reports/nested.txt"], b"nested content");
    assert!(entries["另一目录/"].is_empty());
}

#[tokio::test]
async fn archives_reject_invalid_selections_without_crossing_the_root() {
    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    for paths in [
        vec![],
        vec!["short.txt", "short.txt"],
        vec!["short.txt", "reports/nested.txt"],
        vec!["short.txt"; 1001],
    ] {
        let response = fixture.archive(&paths, &cookie).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    for path in [
        "../files-neighbor",
        #[cfg(unix)]
        "linked-directory",
    ] {
        let response = send(
            &fixture.app,
            "GET",
            &path_uri("/api/files/archive", path),
            &[("cookie", &cookie)],
            "",
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
    }
    for form in ["paths=not-json", "paths=%7B%7D", "paths=%5B1%5D"] {
        let response = send(
            &fixture.app,
            "POST",
            "/api/files/archive",
            &[
                ("cookie", &cookie),
                ("content-type", "application/x-www-form-urlencoded"),
            ],
            form,
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn batch_delete_removes_files_and_directories_and_reports_individual_failures() {
    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    fs::create_dir_all(fixture.root.join("delete-folder/nested/empty")).unwrap();
    fs::write(
        fixture.root.join("delete-folder/nested/file.txt"),
        b"delete me",
    )
    .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        fixture.root.parent().unwrap().join("files-neighbor"),
        fixture.root.join("delete-folder/external"),
    )
    .unwrap();
    let response = fixture
        .delete_many(
            &[
                "short.txt",
                "delete-folder",
                "missing.txt",
                "../files-neighbor/secret.txt",
            ],
            &cookie,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let result = json_body(response).await;
    assert_eq!(result["deleted"], json!(["short.txt", "delete-folder"]));
    let failures = result["failed"].as_array().unwrap();
    assert_eq!(failures.len(), 2);
    assert!(
        failures
            .iter()
            .all(|failure| failure["path"].is_string() && failure["message"].is_string())
    );
    assert!(!fixture.root.join("short.txt").exists());
    assert!(!fixture.root.join("delete-folder").exists());
    assert!(fixture.root.join("reports/nested.txt").exists());
    assert_eq!(
        fs::read(
            fixture
                .root
                .parent()
                .unwrap()
                .join("files-neighbor/secret.txt")
        )
        .unwrap(),
        b"outside root"
    );

    let root = fixture.delete_many(&[""], &cookie).await;
    if root.status() != StatusCode::BAD_REQUEST {
        assert_eq!(root.status(), StatusCode::OK);
        let result = json_body(root).await;
        assert!(result["deleted"].as_array().unwrap().is_empty());
        assert_eq!(result["failed"].as_array().unwrap().len(), 1);
    }
    assert!(fixture.root.exists());
    #[cfg(unix)]
    {
        let linked = fixture.delete_many(&["linked-file"], &cookie).await;
        assert_eq!(linked.status(), StatusCode::OK);
        let result = json_body(linked).await;
        assert!(result["deleted"].as_array().unwrap().is_empty());
        assert_eq!(result["failed"][0]["path"], "linked-file");
        assert!(
            fixture
                .root
                .join("linked-file")
                .symlink_metadata()
                .unwrap()
                .is_symlink()
        );
        assert_eq!(
            fs::read(
                fixture
                    .root
                    .parent()
                    .unwrap()
                    .join("files-neighbor/secret.txt")
            )
            .unwrap(),
            b"outside root"
        );
    }
}

#[tokio::test]
async fn batch_requests_validate_shape_count_and_body_size() {
    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    for body in [
        json!({}),
        json!({ "paths": [] }),
        json!({ "paths": "short.txt" }),
        json!({ "paths": [1] }),
        json!({ "paths": ["short.txt", "short.txt"] }),
        json!({ "paths": vec!["short.txt"; 1001] }),
    ] {
        let response = send(
            &fixture.app,
            "POST",
            "/api/files/delete",
            &[("cookie", &cookie), ("content-type", "application/json")],
            &body.to_string(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    let oversized = "x".repeat(1024 * 1024 + 1);
    for (endpoint, content_type, body) in [
        (
            "/api/files/delete",
            "application/json",
            json!({ "paths": [&oversized] }).to_string(),
        ),
        (
            "/api/files/archive",
            "application/x-www-form-urlencoded",
            serde_urlencoded::to_string([("paths", json!([&oversized]).to_string())]).unwrap(),
        ),
    ] {
        let response = send(
            &fixture.app,
            "POST",
            endpoint,
            &[("cookie", &cookie), ("content-type", content_type)],
            &body,
        )
        .await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}

#[tokio::test]
async fn unfinished_archives_apply_backpressure_and_cancelled_downloads_release_slots() {
    let fixture = Fixture::new();
    let (cookie, _) = fixture.login(None).await;
    fs::File::create(fixture.root.join("large.bin"))
        .unwrap()
        .set_len(8 * 1024 * 1024)
        .unwrap();
    let first = fixture.archive(&["large.bin"], &cookie).await;
    let second = fixture.archive(&["large.bin"], &cookie).await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(second.status(), StatusCode::OK);
    let busy = fixture.archive(&["large.bin"], &cookie).await;
    assert_eq!(busy.status(), StatusCode::TOO_MANY_REQUESTS);
    drop(first);
    drop(second);

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let response = fixture.archive(&["large.bin"], &cookie).await;
        if response.status() == StatusCode::OK {
            drop(response);
            break;
        }
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(Instant::now() < deadline, "取消下载后仍未释放归档并发槽位");
        tokio::task::yield_now().await;
    }
}
