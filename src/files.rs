use axum::{
    Form, Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, RawQuery, Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, SecondsFormat, Utc};
use http_range_header::{RangeUnsatisfiableError, parse_range_header};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fs::Metadata, path::PathBuf};
use tokio::fs;
use tower_http::services::ServeFile;

use crate::{AppError, AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/files", get(list).delete(delete_file))
        .route("/files/download", get(download).head(download))
        .route("/files/preview", get(preview).head(preview))
        .route(
            "/files/delete",
            post(delete_batch).layer(DefaultBodyLimit::max(1024 * 1024)),
        )
        .route(
            "/files/archive",
            get(archive_directory)
                .post(archive_selection)
                .layer(DefaultBodyLimit::max(1024 * 1024)),
        )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FileEntry {
    name: String,
    path: String,
    #[serde(rename = "type")]
    kind: &'static str,
    size: Option<u64>,
    modified_at: String,
}

#[derive(Serialize)]
struct DirectoryListing {
    path: String,
    entries: Vec<FileEntry>,
}

pub(crate) struct Target {
    pub path: PathBuf,
    pub relative: String,
    pub metadata: Metadata,
}

fn query_path(query: Option<String>, allow_root: bool) -> Result<String, AppError> {
    let parameters: Vec<(String, String)> =
        serde_urlencoded::from_str(query.as_deref().unwrap_or_default())
            .map_err(|_| AppError::new(StatusCode::BAD_REQUEST, "无效的查询参数"))?;
    if parameters.len() > 1 || parameters.iter().any(|(key, _)| key != "path") {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "path 必须是单个相对路径字符串",
        ));
    }
    let path = parameters.into_iter().next().map(|(_, path)| path);
    match path {
        Some(path) if allow_root || !path.is_empty() => Ok(path),
        None if allow_root => Ok(String::new()),
        _ => Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "必须指定非空文件路径",
        )),
    }
}

async fn resolve_path(
    state: &AppState,
    input: &str,
    expect_directory: Option<bool>,
) -> Result<Target, AppError> {
    if input.contains(['\\', '\0']) {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "路径包含不允许的字符",
        ));
    }
    let path = std::path::Path::new(input);
    let drive_prefix =
        input.as_bytes().get(1) == Some(&b':') && input.as_bytes()[0].is_ascii_alphabetic();
    if path.is_absolute()
        || drive_prefix
        || input.split('/').any(|part| part == "." || part == "..")
    {
        return Err(AppError::new(StatusCode::FORBIDDEN, "不允许访问该路径"));
    }

    let segments: Vec<_> = input.split('/').filter(|part| !part.is_empty()).collect();
    let mut path = state.config.root.clone();
    let mut metadata = fs::symlink_metadata(&path).await?;
    if metadata.is_symlink() {
        return Err(AppError::new(StatusCode::FORBIDDEN, "不允许访问符号链接"));
    }
    for segment in &segments {
        if !metadata.is_dir() {
            return Err(AppError::new(
                StatusCode::BAD_REQUEST,
                "路径中包含非目录条目",
            ));
        }
        path.push(segment);
        if !path.starts_with(&state.config.root) {
            return Err(AppError::new(StatusCode::FORBIDDEN, "不允许访问该路径"));
        }
        metadata = fs::symlink_metadata(&path).await?;
        if metadata.is_symlink() {
            return Err(AppError::new(StatusCode::FORBIDDEN, "不允许访问符号链接"));
        }
    }
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(AppError::new(StatusCode::FORBIDDEN, "不允许访问特殊文件"));
    }
    if expect_directory.is_some_and(|directory| metadata.is_dir() != directory) {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            if expect_directory == Some(true) {
                "目标必须是目录"
            } else {
                "目标必须是普通文件"
            },
        ));
    }
    Ok(Target {
        path,
        relative: segments.join("/"),
        metadata,
    })
}

async fn list(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> Result<Json<DirectoryListing>, AppError> {
    let path = query_path(query, true)?;
    let target = resolve_path(&state, &path, Some(true)).await?;
    let mut directory = fs::read_dir(&target.path).await?;
    let mut entries = Vec::new();
    while let Some(entry) = directory.next_entry().await? {
        let metadata = fs::symlink_metadata(entry.path()).await?;
        if !metadata.is_file() && !metadata.is_dir() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let modified: DateTime<Utc> = metadata.modified()?.into();
        entries.push(FileEntry {
            path: if target.relative.is_empty() {
                name.clone()
            } else {
                format!("{}/{name}", target.relative)
            },
            name,
            kind: if metadata.is_dir() {
                "directory"
            } else {
                "file"
            },
            size: metadata.is_file().then_some(metadata.len()),
            modified_at: modified.to_rfc3339_opts(SecondsFormat::Millis, true),
        });
    }
    entries.sort_by(|a, b| a.kind.cmp(b.kind).then_with(|| a.name.cmp(&b.name)));
    Ok(Json(DirectoryListing {
        path: target.relative,
        entries,
    }))
}

async fn delete_file(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> Result<StatusCode, AppError> {
    let path = query_path(query, false)?;
    let target = resolve_path(&state, &path, Some(false)).await?;
    fs::remove_file(target.path).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn download(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    request: Request,
) -> Result<Response, AppError> {
    let path = query_path(query, false)?;
    let target = resolve_path(&state, &path, Some(false)).await?;
    file_response(target, request, false).await
}

async fn preview(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    request: Request,
) -> Result<Response, AppError> {
    let path = query_path(query, false)?;
    let target = resolve_path(&state, &path, Some(false)).await?;
    if preview_type(&target.relative).is_none() {
        return Err(AppError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "该文件不支持预览",
        ));
    }
    file_response(target, request, true).await
}

fn preview_type(path: &str) -> Option<&'static str> {
    match path.rsplit('.').next()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "avif" => Some("image/avif"),
        "bmp" => Some("image/bmp"),
        "ico" => Some("image/vnd.microsoft.icon"),
        "mp4" | "m4v" => Some("video/mp4"),
        "webm" => Some("video/webm"),
        "ogv" => Some("video/ogg"),
        "mov" => Some("video/quicktime"),
        "mp3" => Some("audio/mpeg"),
        "m4a" => Some("audio/mp4"),
        "aac" => Some("audio/aac"),
        "wav" => Some("audio/wav"),
        "ogg" | "oga" | "opus" => Some("audio/ogg"),
        "flac" => Some("audio/flac"),
        _ => None,
    }
}

async fn file_response(
    target: Target,
    mut request: Request,
    inline: bool,
) -> Result<Response, AppError> {
    // ServeFile maps access-denied errors to 404; preserve the API's 403 response.
    drop(fs::File::open(&target.path).await?);

    let parsed_range = request
        .headers()
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.contains(','))
        .map(parse_range_header);
    let (supported_range, oversized_suffix) = match parsed_range {
        Some(Ok(parsed)) if parsed.ranges.len() == 1 => (
            true,
            matches!(
                parsed.validate(target.metadata.len()),
                Err(RangeUnsatisfiableError::FileSuffixOutOfBounds)
            ),
        ),
        Some(Err(RangeUnsatisfiableError::ZeroSuffix)) => (true, false),
        _ => (false, false),
    };
    let if_range_matches = if let Some(value) = request.headers().get(header::IF_RANGE) {
        let modified = httpdate::fmt_http_date(target.metadata.modified()?);
        value
            .to_str()
            .ok()
            .and_then(|value| httpdate::parse_http_date(value).ok())
            .is_some_and(|date| httpdate::fmt_http_date(date) == modified)
    } else {
        true
    };
    let use_range = request.method() != Method::HEAD && supported_range && if_range_matches;
    if !use_range {
        request.headers_mut().remove(header::RANGE);
    } else if oversized_suffix && target.metadata.len() > 0 {
        // http-range-header 0.4 rejects suffixes longer than the file.
        request
            .headers_mut()
            .insert(header::RANGE, HeaderValue::from_static("bytes=0-"));
    }

    let mut response = if use_range && target.metadata.len() == 0 {
        (
            StatusCode::RANGE_NOT_SATISFIABLE,
            [(header::CONTENT_RANGE, "bytes */0")],
        )
            .into_response()
    } else {
        ServeFile::new(&target.path)
            .try_call(request)
            .await?
            .map(Body::new)
    };
    if response.status() == StatusCode::NOT_FOUND {
        return Err(AppError::new(StatusCode::NOT_FOUND, "文件或目录不存在"));
    }
    if response.status() == StatusCode::RANGE_NOT_SATISFIABLE {
        let content_range = response.headers().get(header::CONTENT_RANGE).cloned();
        response =
            AppError::new(StatusCode::RANGE_NOT_SATISFIABLE, "请求的范围不可满足").into_response();
        if let Some(content_range) = content_range {
            response
                .headers_mut()
                .insert(header::CONTENT_RANGE, content_range);
        }
    }
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if response.status().is_success() {
        let name = target.relative.rsplit('/').next().unwrap();
        response.headers_mut().insert(
            header::CONTENT_DISPOSITION,
            content_disposition(if inline { "inline" } else { "attachment" }, name)?,
        );
        if inline {
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(preview_type(name).unwrap()),
            );
        }
    }
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

pub(crate) fn content_disposition(kind: &str, name: &str) -> Result<HeaderValue, AppError> {
    let fallback: String = name
        .chars()
        .map(|character| match character {
            ' '..='~' if !matches!(character, '"' | '\\') => character,
            _ => '_',
        })
        .collect();
    HeaderValue::try_from(format!(
        "{kind}; filename=\"{fallback}\"; filename*=UTF-8''{}",
        utf8_percent_encode(name, NON_ALPHANUMERIC)
    ))
    .map_err(|_| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "无法生成下载文件名"))
}

#[derive(Deserialize)]
struct Selection {
    paths: Vec<String>,
}

#[derive(Deserialize)]
struct ArchiveForm {
    paths: String,
}

fn validate_selection(paths: &[String]) -> Result<(), AppError> {
    let mut unique = HashSet::new();
    if paths.is_empty()
        || paths.len() > 1000
        || paths
            .iter()
            .any(|path| path.is_empty() || !unique.insert(path))
    {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "请选择 1 到 1000 个不重复的条目",
        ));
    }
    Ok(())
}

fn input_error(status: StatusCode) -> AppError {
    let status = match status {
        StatusCode::UNSUPPORTED_MEDIA_TYPE | StatusCode::PAYLOAD_TOO_LARGE => status,
        _ => StatusCode::BAD_REQUEST,
    };
    AppError::new(status, "无效的文件选择请求")
}

#[derive(Serialize)]
struct DeleteFailure {
    path: String,
    message: String,
}

#[derive(Serialize)]
struct DeleteResult {
    deleted: Vec<String>,
    failed: Vec<DeleteFailure>,
}

async fn delete_batch(
    State(state): State<AppState>,
    payload: Result<Json<Selection>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<DeleteResult>, AppError> {
    let Json(selection) = payload.map_err(|error| input_error(error.status()))?;
    validate_selection(&selection.paths)?;
    let mut result = DeleteResult {
        deleted: Vec::new(),
        failed: Vec::new(),
    };
    for path in selection.paths {
        let operation = async {
            let target = resolve_path(&state, &path, None).await?;
            if target.relative.is_empty() {
                return Err(AppError::new(StatusCode::FORBIDDEN, "不允许删除根目录"));
            }
            if target.metadata.is_dir() {
                fs::remove_dir_all(&target.path).await?;
            } else {
                fs::remove_file(&target.path).await?;
            }
            Ok::<(), AppError>(())
        }
        .await;
        match operation {
            Ok(()) => result.deleted.push(path),
            Err(error) => result.failed.push(DeleteFailure {
                path,
                message: error.message,
            }),
        }
    }
    Ok(Json(result))
}

async fn archive_directory(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> Result<Response, AppError> {
    let path = query_path(query, false)?;
    let target = resolve_path(&state, &path, Some(true)).await?;
    if target.relative.is_empty() {
        return Err(AppError::new(StatusCode::BAD_REQUEST, "请选择一个文件夹"));
    }
    drop(fs::read_dir(&target.path).await?);
    let filename = format!("{}.zip", target.relative.rsplit('/').next().unwrap());
    crate::archive::stream_archive(&state, vec![target], &filename)
}

async fn archive_selection(
    State(state): State<AppState>,
    payload: Result<Form<ArchiveForm>, axum::extract::rejection::FormRejection>,
) -> Result<Response, AppError> {
    let Form(form) = payload.map_err(|error| input_error(error.status()))?;
    let paths: Vec<String> = serde_json::from_str(&form.paths)
        .map_err(|_| AppError::new(StatusCode::BAD_REQUEST, "无效的文件选择"))?;
    validate_selection(&paths)?;
    let mut targets = Vec::with_capacity(paths.len());
    let mut unique = HashSet::new();
    let mut parent = None;
    for path in paths {
        let target = resolve_path(&state, &path, None).await?;
        if target.relative.is_empty() || !unique.insert(target.relative.clone()) {
            return Err(AppError::new(StatusCode::BAD_REQUEST, "请选择不重复的条目"));
        }
        let current_parent = target
            .relative
            .rsplit_once('/')
            .map_or("", |(parent, _)| parent)
            .to_owned();
        if parent
            .as_ref()
            .is_some_and(|parent| *parent != current_parent)
        {
            return Err(AppError::new(
                StatusCode::BAD_REQUEST,
                "请选择同一目录中的条目",
            ));
        }
        parent = Some(current_parent);
        if target.metadata.is_dir() {
            drop(fs::read_dir(&target.path).await?);
        } else {
            drop(fs::File::open(&target.path).await?);
        }
        targets.push(target);
    }
    crate::archive::stream_archive(&state, targets, "download.zip")
}
