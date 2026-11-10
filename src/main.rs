use mint::{AppState, Config, router};
use std::{
    collections::HashMap,
    env,
    error::Error,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    process::ExitCode,
};

const USAGE: &str = r#"Mint 文件管理器

用法：
  mint [参数]
  mint --passwd <密码>
  mint -h | --help

参数：
  -h, --help                    显示帮助并退出，无需服务配置
  -c, --config <env文件>         加载指定配置文件，不自动加载 .env
  --root-dir <目录>             管理根目录，必须为已存在且可读取的绝对路径（ROOT_DIR，必填）
  --username <账号>             登录账号（USERNAME，必填）
  --password-hash <哈希>        bcrypt 密码哈希，通过 --passwd 生成（PASSWORD_HASH，必填）
  --host <地址>                 监听 IP 或主机名（HOST，默认 0.0.0.0）
  --port <端口>                 监听端口，范围 1–65535（PORT，默认 3000）
  --cookie-secure <true|false>   设置 Cookie 的 Secure 属性，不开启 HTTPS（COOKIE_SECURE，默认 false）
  --passwd <密码>               生成 bcrypt 密码哈希并退出，须独立使用，无需服务配置

必填配置可通过参数、环境变量或配置文件提供。
配置优先级：命令行参数 > 环境变量 > 配置文件 > 默认值。

示例：
  mint --passwd your_password
  mint --config ./mint.env
  mint -c ./mint.env --host 127.0.0.1 --port 8080"#;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() == 1 && matches!(args[0].as_str(), "-h" | "--help") {
        println!("{USAGE}");
        return Ok(());
    }
    if args.len() == 2 && args[0] == "--passwd" {
        if args[1].is_empty() || args[1].len() > 72 {
            return Err("密码不能为空，且最多为 72 个 UTF-8 字节".into());
        }
        println!("{}", bcrypt::hash(&args[1], 12)?);
        return Ok(());
    }
    let (pairs, remainder) = args.as_chunks::<2>();
    if !remainder.is_empty() {
        return Err(USAGE.into());
    }
    let mut overrides = HashMap::new();
    let mut config_path = None;
    for [flag, value] in pairs {
        let flag = flag.as_str();
        if matches!(flag, "-c" | "--config") {
            if config_path.replace(value).is_some() {
                return Err("配置文件参数不能重复".into());
            }
            continue;
        }
        let key = match flag {
            "--root-dir" => "ROOT_DIR",
            "--username" => "USERNAME",
            "--password-hash" => "PASSWORD_HASH",
            "--host" => "HOST",
            "--port" => "PORT",
            "--cookie-secure" => "COOKIE_SECURE",
            _ => return Err(USAGE.into()),
        };
        if overrides.insert(key, value.clone()).is_some() {
            return Err(format!("参数不能重复：{flag}").into());
        }
    }
    if let Some(path) = config_path {
        dotenvy::from_path(path).map_err(|error| {
            let message = match error {
                dotenvy::Error::Io(error) => error.to_string(),
                _ => "env 文件格式错误".to_owned(),
            };
            format!("无法加载配置文件 {path}：{message}")
        })?;
    }
    let config = Config::from_env(&overrides)?;
    let host = config.host.clone();
    let port = config.port;
    let state = AppState::new(config);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(async move {
            let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
            let mut address = listener.local_addr()?;
            if address.ip().is_unspecified() {
                address.set_ip(if address.is_ipv4() {
                    Ipv4Addr::LOCALHOST.into()
                } else {
                    Ipv6Addr::LOCALHOST.into()
                });
            }
            println!("Mint 已启动");
            println!("用户名：{}", state.config.username);
            println!("Web 访问地址：http://{address}/");
            println!("根目录：{}", state.config.root.display());
            println!("Cookie Secure：{}", state.config.cookie_secure);
            axum::serve(
                listener,
                router(state).into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown())
            .await?;
            Ok::<(), Box<dyn Error>>(())
        })
}

async fn shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).expect("无法监听退出信号");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
