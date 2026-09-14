# ftp-toolbox

基于 **Tauri v2 + React + Rust** 的 FTP / TFTP 服务器与客户端一体桌面工具。

![Rust](https://img.shields.io/badge/Rust-1.7x+-orange?logo=rust)
![Tauri](https://img.shields.io/badge/Tauri-v2-24C8D8?logo=tauri)
![React](https://img.shields.io/badge/React-18-61DAFB?logo=react)
![Platform](https://img.shields.io/badge/platform-Windows-blue?logo=windows)
![License](https://img.shields.io/badge/license-MIT-green)

一个窗口同时搞定「起服务」和「传文件」：左边开 FTP/TFTP 服务器，右边用内置客户端连上去互传，底部实时进度，后端日志逐条可见——调嵌入式设备、路由器、旧仪器这类只支持 TFTP/FTP 的对端时，不用再东拼西凑一堆小工具。

## 功能

- **FTP 服务器**：匿名 / 账号密码认证，被动端口 50000-50099，会话级日志（基于 libunftp）
- **FTP 客户端**：连接 / 列目录 / 上传 / 下载，全程进度反馈
- **TFTP 服务器 / 客户端**（自实现 RFC 1350）：
  - RFC 2348 `blksize` 协商（请求 8192，单文件上限 ~536 MB，兼容端自动回退 512 字节经典模式）
  - 超时重传、防目录穿越、每连接独立线程（新 TID）
- **服务器配置体验**：
  - 共享目录走系统原生文件夹选择框，配置自动记忆（密码不保存）
  - 监听地址自动枚举本机网卡，FTP 默认 21 / TFTP 默认 69
- **详细运行日志**：后端引擎 + libunftp 会话日志实时推送到前端，TFTP 每次传输记录对端、文件名、字节数、块数、耗时——排错直接看日志面板
- **现代 UI**：侧边栏导航、卡片式布局、全局传输进度条（React + TypeScript + Vite）

## 架构

```
crates/ftp-core    核心引擎（纯 tokio，不依赖任何 GUI）
  src/ftp/         FTP server（libunftp）+ client（suppaftp）
  src/tftp/        TFTP（RFC 1350 + RFC 2348）server + client，自实现
app/src-tauri      Tauri v2 壳：命令层 + 进度/日志事件转发
app/ui             React 前端（Vite + TypeScript）
```

核心库与 GUI 完全解耦：想换成 CLI、gpui 或别的壳，只需要替换 `app/`，核心代码一行不动。

## 快速开始

环境要求：Rust（stable）、Node.js ≥ 18、Windows 需 WebView2（Win10/11 通常自带）。

```bash
cd app/ui
npm install
npm run build        # 产出 app/ui/dist

cd ../..             # 回到仓库根目录
cargo run -p ftp-toolbox-app
```

## 打包安装包

```bash
cd app               # 必须在这个目录（tauri CLI 只在 cwd 往下找配置）
../ui/node_modules/.bin/tauri build
```

产物在 target 的 `release/bundle/{nsis,msi}/`（NSIS 出 `setup.exe`，WiX 出 `.msi`）。

## 测试

```bash
cargo test -p ftp-core   # 含 TFTP 回环集成测试（起真实 UDP 服务端互传文件）
```

## 已知限制

- TFTP 未实现 `tsize` / `timeout` 选项协商；对端不支持 `blksize` 时回退经典模式（~32 MiB 上限）
- TFTP 69 端口在 Linux/macOS 需要特权；Windows 上若与其他 TFTP 服务冲突会提示 bind 失败
- FTP 服务器为单用户静态账号；多用户 / 权限 / TLS（FTPS）是 libunftp 的现成能力，未接入

## Roadmap

- [ ] FTP over TLS（FTPS）
- [ ] TFTP `tsize` 协商（下载前获知文件大小，进度条可显示百分比）
- [ ] FTP 远端文件树浏览（拖拽上传/下载）
- [ ] macOS / Linux 构建

## License

MIT
