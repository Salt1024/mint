pub mod archive;
pub mod files;

use axum::{
    Json, Router,
    body::Body,
    extract::{ConnectInfo, DefaultBodyLimit, Request, State, rejection::JsonRejection},
    http::{Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env, io,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const SESSION_COOKIE: &str = "mint_session";
const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);

pub struct Config {
    pub root: PathBuf,
    pub username: String,
    pub password_hash: String,
    pub host: String,
    pub port: u16,
    pub cookie_secure: bool,
}

impl Config {
    pub fn from_env(overrides: &HashMap<&str, String>) -> Result<Self, Box<dyn std::error::Error>> {
        let get =
            |key: &str, default: Option<&str>| -> Result<String, Box<dyn std::error::Error>> {
                if let Some(value) = overrides.get(key) {
                    return Ok(value.clone());
                }
                match env::var(key) {
                    Ok(value) => Ok(value),
                    Err(env::VarError::NotPresent) => default
                        .map(str::to_owned)
                        .ok_or_else(|| format!("必须配置 {key}").into()),
                    Err(_) => Err(format!("{key} 必须是有效的 UTF-8 文本").into()),
                }
            };
        let root = PathBuf::from(get("ROOT_DIR", None)?);
        if !root.is_absolute() {
            return Err("ROOT_DIR 必须是管理目录的绝对路径".into());
        }
        let root = root
            .canonicalize()
            .map_err(|_| "ROOT_DIR 必须指向存在且可读取的目录")?;
        if !root.is_dir() || std::fs::read_dir(&root).is_err() {
            return Err("ROOT_DIR 必须指向存在且可读取的目录".into());
        }
        let username = get("USERNAME", None)?;
        if username.trim().is_empty() {
            return Err("USERNAME 不能为空".into());
        }
        let password_hash = get("PASSWORD_HASH", None)?;
        let hash_parts = password_hash
            .parse::<bcrypt::HashParts>()
            .map_err(|_| "PASSWORD_HASH 必须是有效的 bcrypt 哈希")?;
        if !(4..=31).contains(&hash_parts.get_cost()) {
            return Err("PASSWORD_HASH 的 cost 必须为 4 到 31".into());
        }
        let host = get("HOST", Some("0.0.0.0"))?;
        if host.trim().is_empty() {
            return Err("HOST 不能为空".into());
        }
        let port = get("PORT", Some("3000"))?
            .parse::<u16>()
            .ok()
            .filter(|port| *port > 0)
            .ok_or("PORT 必须是 1 到 65535 之间的整数")?;
        let cookie_secure = get("COOKIE_SECURE", Some("false"))?
            .parse::<bool>()
            .map_err(|_| "COOKIE_SECURE 必须为 true 或 false")?;
        Ok(Self {
            root,
            username,
            password_hash,
            host,
            port,
            cookie_secure,
        })
    }
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub archive_slots: Arc<tokio::sync::Semaphore>,
    auth: Arc<AuthState>,
}

#[derive(Default)]
struct AuthState {
    sessions: Mutex<HashMap<String, Instant>>,
    attempts: Mutex<HashMap<IpAddr, (Instant, u8)>>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(config),
            archive_slots: Arc::new(tokio::sync::Semaphore::new(2)),
            auth: Arc::new(AuthState::default()),
        }
    }

    fn cookie(&self, value: String) -> Cookie<'static> {
        Cookie::build((SESSION_COOKIE, value))
            .path("/api")
            .http_only(true)
            .same_site(SameSite::Strict)
            .secure(self.config.cookie_secure)
            .build()
    }
}

impl AuthState {
    fn check_limit(&self, ip: IpAddr) -> Result<(), AppError> {
        let now = Instant::now();
        let mut attempts = self.attempts.lock().unwrap();
        attempts.retain(|_, (start, _)| now.duration_since(*start) < Duration::from_secs(60));
        let (_, count) = attempts.entry(ip).or_insert((now, 0));
        if *count >= 10 {
            return Err(AppError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "登录请求过于频繁",
            ));
        }
        *count += 1;
        Ok(())
    }

    fn assert_session(&self, token: Option<&str>) -> Result<(), AppError> {
        if let Some(token) = token {
            let mut sessions = self.sessions.lock().unwrap();
            if sessions
                .get(token)
                .is_some_and(|expiry| *expiry > Instant::now())
            {
                return Ok(());
            }
            sessions.remove(token);
        }
        Err(AppError::new(StatusCode::UNAUTHORIZED, "请先登录"))
    }
}

pub struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl From<io::Error> for AppError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => {
                Self::new(StatusCode::NOT_FOUND, "文件或目录不存在")
            }
            io::ErrorKind::PermissionDenied => {
                Self::new(StatusCode::FORBIDDEN, "没有访问该路径的权限")
            }
            _ => {
                eprintln!("文件系统错误：{error}");
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, "文件操作失败")
            }
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            self.status,
            [(header::CACHE_CONTROL, "private, no-store")],
            Json(serde_json::json!({
                "statusCode": self.status.as_u16(),
                "message": self.message,
                "error": self.status.canonical_reason().unwrap_or("Error"),
            })),
        )
            .into_response()
    }
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Serialize)]
struct SessionInfo {
    username: String,
}

async fn login(
    State(state): State<AppState>,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    jar: CookieJar,
    payload: Result<Json<LoginRequest>, JsonRejection>,
) -> Result<impl IntoResponse, AppError> {
    state.auth.check_limit(address.ip())?;
    let Json(body) = payload.map_err(|error| {
        let status = error.status();
        if status == StatusCode::UNSUPPORTED_MEDIA_TYPE {
            AppError::new(status, "请使用 JSON 提交登录信息")
        } else if status == StatusCode::PAYLOAD_TOO_LARGE {
            AppError::new(status, "登录请求体过大")
        } else {
            AppError::new(StatusCode::BAD_REQUEST, "请输入有效的账号和密码")
        }
    })?;
    if body.username.trim().is_empty() || body.password.is_empty() || body.password.len() > 72 {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "请输入账号和密码，密码最多为 72 个 UTF-8 字节",
        ));
    }
    let password_hash = state.config.password_hash.clone();
    let matches =
        tokio::task::spawn_blocking(move || bcrypt::verify(body.password, &password_hash))
            .await
            .map_err(|_| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "登录失败"))?
            .map_err(|_| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "登录失败"))?;
    if body.username != state.config.username || !matches {
        return Err(AppError::new(StatusCode::UNAUTHORIZED, "账号或密码错误"));
    }

    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|_| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "无法创建会话"))?;
    let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    {
        let now = Instant::now();
        let mut sessions = state.auth.sessions.lock().unwrap();
        sessions.retain(|_, expiry| *expiry > now);
        if let Some(previous) = jar.get(SESSION_COOKIE) {
            sessions.remove(previous.value());
        }
        sessions.insert(token.clone(), now + SESSION_TTL);
    }
    let mut cookie = state.cookie(token);
    cookie.set_max_age(time::Duration::hours(12));
    Ok((
        jar.add(cookie),
        Json(SessionInfo {
            username: state.config.username.clone(),
        }),
    ))
}

async fn logout(State(state): State<AppState>, jar: CookieJar) -> impl IntoResponse {
    if let Some(cookie) = jar.get(SESSION_COOKIE) {
        state.auth.sessions.lock().unwrap().remove(cookie.value());
    }
    (
        jar.remove(state.cookie(String::new())),
        StatusCode::NO_CONTENT,
    )
}

async fn me(State(state): State<AppState>) -> Json<SessionInfo> {
    Json(SessionInfo {
        username: state.config.username.clone(),
    })
}

async fn require_session(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let jar = CookieJar::from_headers(request.headers());
    state
        .auth
        .assert_session(jar.get(SESSION_COOKIE).map(Cookie::value))?;
    Ok(next.run(request).await)
}

async fn no_cache(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("private, no-store"),
    );
    response
}

async fn api_not_found() -> AppError {
    AppError::new(StatusCode::NOT_FOUND, "接口不存在")
}

#[derive(RustEmbed)]
#[folder = "ui/dist/"]
struct WebAssets;

async fn static_asset(request: Request) -> Result<Response, AppError> {
    let path = request.uri().path();
    if path == "/api" || path.starts_with("/api/") {
        return Err(api_not_found().await);
    }
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return Err(AppError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "请求方法不支持",
        ));
    }
    let path = percent_encoding::percent_decode_str(path.trim_start_matches('/'))
        .decode_utf8()
        .map_err(|_| AppError::new(StatusCode::BAD_REQUEST, "无效的资源路径"))?;
    let path = if path.is_empty() {
        "index.html"
    } else {
        path.as_ref()
    };
    let asset =
        WebAssets::get(path).ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, "资源不存在"))?;
    let size = asset.data.len();
    let body = if request.method() == Method::HEAD {
        Body::empty()
    } else {
        Body::from(asset.data)
    };
    Ok(Response::builder()
        .header(
            header::CONTENT_TYPE,
            mime_guess::from_path(path).first_or_octet_stream().as_ref(),
        )
        .header(header::CONTENT_LENGTH, size)
        .body(body)
        .unwrap())
}

pub fn router(state: AppState) -> Router {
    let protected =
        files::routes()
            .route("/auth/me", get(me))
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_session,
            ));
    let api = Router::new()
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .merge(protected)
        .fallback(api_not_found)
        .layer(DefaultBodyLimit::max(4096))
        .layer(middleware::from_fn(no_cache));
    Router::new()
        .nest("/api", api)
        .fallback(static_asset)
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_sessions_are_removed_and_rejected() {
        let auth = AuthState::default();
        auth.sessions
            .lock()
            .unwrap()
            .insert("expired".into(), Instant::now() - Duration::from_secs(1));
        assert!(auth.assert_session(Some("expired")).is_err());
        assert!(auth.sessions.lock().unwrap().is_empty());
    }
}
