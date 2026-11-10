# Mint

简易的单账号 Web 文件管理器，使用 Rust、Axum、Vue 和 shadcn-vue，无需数据库。

## 特性

- 支持目录浏览、媒体预览、文件查询下载和删除。
- 单文件下载支持 HTTP Range，可供下载器并发分段下载。
- 支持使用流式 ZIP 批量下载。

## 使用

- 根据你的系统下载预构建包。

- 创建 `mint.env`，参考 [env 示例](.env.example)，填写`ROOT_DIR`、`USERNAME`。

- 生成密码哈希：

  ```sh
  ./mint --passwd your_password
  ```

- 创建 `mint.env`，在`PASSWORD_HASH`填入生成的密码哈希；哈希用单引号包裹。

- 启动服务：

  ```sh
  ./mint --config ./mint.env
  ```

按启动日志中的 Web 地址访问，默认本机地址为 `http://127.0.0.1:3000/`。日志同时输出用户名、根目录绝对路径和 Cookie Secure 状态。

## 配置

所有配置均支持环境变量和命令行参数。

| 环境变量 | 命令行参数 | 说明 |
| --- | --- | --- |
| `ROOT_DIR` | `--root-dir` | 必填，已存在且可读取的根目录绝对路径 |
| `USERNAME` | `--username` | 必填，登录账号 |
| `PASSWORD_HASH` | `--password-hash` | 必填，`--passwd` 生成的 bcrypt 哈希 |
| `HOST` | `--host` | 监听地址，默认 `0.0.0.0` |
| `PORT` | `--port` | 端口，默认 `3000`，范围 `1–65535` |
| `COOKIE_SECURE` | `--cookie-secure` | `true` 或 `false`，默认 `false` |

优先级：命令行参数 > 环境变量 > 配置文件 > 默认值。程序不自动加载 `.env`。

覆盖部分配置：

```sh
./mint -c ./mint.env --host 127.0.0.1 --port 8080 --cookie-secure false
```

也可仅通过环境变量启动：

```sh
ROOT_DIR=/absolute/path/to/files USERNAME=admin \
  PASSWORD_HASH="$(./mint --passwd your_password)" ./mint
```

当使用 HTTPS 反向代理访问时，建议设置 `COOKIE_SECURE=true`，本地 HTTP 使用 `false`。

## 构建

需要 Rust、Node.js 和 pnpm，在仓库根目录执行：

```sh
pnpm --dir ui build
cargo build --release --locked
```

默认构建当前宿主平台，产物为 `target/release/mint`。

## 开发

安装依赖后，先构建前端，再分别启动后端和前端开发服务器：

```sh
pnpm --dir ui build
cargo run -- --config ./.env
# 另一个终端
pnpm --dir ui dev
```
