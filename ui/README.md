# Mint Web

Vue 3 + Vite 的 PC 文件管理页面，使用 shadcn-vue。

在仓库根目录运行：

```sh
pnpm --dir ui install
pnpm --dir ui dev
pnpm --dir ui build
```

开发服务器将 `/api` 代理到 `http://localhost:3000`，需要同时通过 `cargo run` 启动根目录的 Rust 后端。构建产物位于 `ui/dist`，在 Rust release 构建时内嵌进单个可执行文件。
