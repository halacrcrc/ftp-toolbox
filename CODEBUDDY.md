# CODEBUDDY.md
This file provides guidance to CodeBuddy when working with code in this repository.

## 项目概览

**ftp-toolbox**：基于 Tauri v2 + React + Rust 的 FTP / FTPS / SFTP / TFTP 服务器与客户端一体 Windows 桌面工具。当前版本 0.3.4，MIT OR Apache-2.0 双许可。设计规格与决策记录在 `docs/`（如 `docs/sftp-design.md` 的 SFTP Q1–Q12 取舍）；代码审查的分级标准、审查节点与红线清单在 `docs/code-review.md`（改代码前先过一遍）。

## 常用命令

```bash
# 引擎测试（单测 + 真实回环集成测试：起真实服务端再用内置客户端互传）
cargo test -p ftp-core

# 跑单个测试（按测试函数名过滤；集成测试用文件名限范围）
cargo test -p ftp-core <test_name>
cargo test -p ftp-core --test ftps_loopback

# 全工作区编译检查
cargo check --workspace

# 前端纯逻辑单测（Node 22+ 内置 node --test，直接加载 .ts，无 vitest/jest）
cd app/ui && npm test

# 前端类型检查
cd app/ui && npm run typecheck

# 前端构建（产出 app/ui/dist，release 运行依赖它）
cd app/ui && npm install && npm run build

# 运行应用（release 形态，需先构建前端 dist）
cargo run -p ftp-toolbox-app

# 开发调试（tauri dev 走 devUrl，必须先起前端 dev server）
cd app/ui && npm run dev        # http://localhost:1420

# 打包安装包（必须在 app/ 目录；用项目内 CLI，全局 cargo-tauri 是 v1 会报 schema 错）
cd app
node "C:/WorkBuddy/FTP/app/ui/node_modules/@tauri-apps/cli/tauri.js" build
```

测试前置要求：Rust stable ≥ 1.89、Node ≥ 18（前端单测需 ≥ 22）。

## 本机环境关键约定（Windows）

- cargo 镜像配置由仓库根 `.cargo/config.toml` 提供（rsproxy 镜像 + git-fetch-with-cli，**不含 target-dir**——个人路径已随 510c3eb 移出仓库）。target-dir 由本机 `~/.cargo/config.toml` 提供（指向仓库外 `C:/Users/22534/.workbuddy/build/ftp-toolbox-target`，2026-10-08 已核实；**不要**把 target 目录挪回仓库，OneDrive/网盘会干扰构建脚本）。
- 长时间编译/测试用后台任务跑，以输出中的 `Finished` 判定成功（stderr 会让 PowerShell 误报失败）。
- 取证/脚本优先用 node（`spawnSync` + `fs.writeFileSync` 再读文件）；PowerShell 工具会吞 stdout，且 PS 5.1 的 `>` 默认写 UTF-16LE。
- 路径传 `C:/...` 形式，别传 `/c/...`。

## 架构

三层严格解耦：**核心引擎（crates/ftp-core）→ Tauri 壳（app/src-tauri）→ React 前端（app/ui）**。核心库不依赖任何 GUI，换壳只需替换 `app/`。

```
crates/ftp-core        纯 tokio 引擎，无 GUI 依赖
  src/ftp/             FTP server（libunftp 0.20 + unftp-sbe-fs）+ client（suppaftp 6），FTPS 自签证书
  src/sftp/            SFTP server + client（russh + russh-sftp），主机密钥 TOFU
  src/tftp/            TFTP 自实现（RFC 1350 + 2347/2348/2349 blksize+tsize），不走 libunftp
  src/tls.rs           FTPS 自签证书（rcgen），load_or_generate 自愈
  src/lifecycle.rs     运行态真相源：ServerShared + watch
  src/cancel.rs        客户端传输取消（CancellationToken）+ 30s 空闲超时
  src/net.rs           网卡枚举与链路判定纯函数
  src/progress.rs / log_fields.rs / error.rs
  tests/               回环集成测试（ftps_loopback、sftp、tftp、ftp_active_mode 等）
app/src-tauri          Tauri v2 壳：#[tauri::command] 命令层 + 事件转发（lib.rs 集中）
app/ui                 React 18 + Vite + TS：api.ts 封装 invoke，views/ + components/
```

### 服务器生命周期（改服务端代码前必读）

- **bind-first**：`start_*` 先 bind，失败即返 `Error::Bind`；Windows 10013 = 端口被独占/受保护（≠普通占用），10048 = AddrInUse；`Error::bind` 用通配探针区分 `BindCause::Taken/Forbidden`。
- 停止统一走 `broadcast::Sender` + `handle.stop().await`，不要用 AbortHandle。
- 状态推送：Tauri 层用 watch 的 `send_replace`（`send` 在无订阅者时不更新存量）+ `spawn_state_forwarder()` 把变化 emit 成 `ftp/tftp-server-state` 事件；`*_server_status` 是前端状态的唯一来源。
- 错误日志统一用 `error_chain()` 展开 source 链。

### 引擎纯函数边界（别在壳层重写逻辑）

- 被动端口解析/校验/建议：`crates/ftp-core/src/ftp/passive.rs`；壳层只留 netsh 读取与提示。
- 网卡链路判定：`net.rs` 纯函数（双信号都读不到 → 宁显示不隐藏）。
- 前端要用的日志/进度纯逻辑放 `crates/ftp-core`（如 `progress.rs`、`log_fields.rs`），保证可被单测覆盖。

### 传输取消（改传输相关代码前必读）

- 六个客户端传输函数（`FtpClient::upload/download`、`SftpClient::upload_file/download_file`、`tftp::put/get`）末参都是 `cancel: Option<CancellationToken>`；取消在分块边界落地，分块读写统一走 `cancel::chunk`（带 30s 空闲超时）。
- 壳层传输命令带 `transfer_id: Option<String>` 尾参 + `cancel_transfer` 命令；前端 `newTransferId()` 生成、`finally` 里清理，App.tsx 据此显示进度条取消按钮。
- 出包核对发布者认准主程序 `ftp-toolbox-app.exe` 的 CompanyName 与 MSI Manufacturer；NSIS setup.exe 包装器的 CompanyName 本来就是空，不是回归。

### 前端约定（app/ui）

- 所有后端调用经 `src/api.ts` 封装 invoke；纯逻辑集中在 `src/lib/`，被 node --test 覆盖。
- 网卡列表：挂载拉一次 + 3s 轮询 + focus 补拉 + JSON 指纹去重；hidden 不轮询。
- `prefs.iface` 绝不能被运行时改写；掉线只告警（边沿触发 + ref 去重）。`ifaceUnlisted`（select 补 option）与 `ifaceMissing`（掉线标签/禁启动）语义不同，勿合并。
- 客户端「本地文件」统一用 `components/LocalFileField.tsx`。

### 踩坑清单（均有历史教训）

- **IPC 结构体必须标 `#[serde(rename_all = "camelCase")]`** —— 漏标不报错，前端静默读到 undefined，只有端到端联调能发现。
- **`src/lib/*` 里相对 import 必须带 `.ts` 后缀**（node --test 的 ESM 不做扩展名补全；Vite 不受影响）。被测模块不要 import `api.ts`（它顶层引 `@tauri-apps/api`，Node 下直接报错）。
- **JSX 空白陷阱**：`{expr}` 后紧跟的换行会剥空格 → `{join("、")}` 与后续汉字必须同行。
- `cargo fmt --check` 与 `cargo clippy -p ftp-core --all-targets` 在本仓库**不是全绿的**：只对齐自己新增/修改的代码，**不要顺手整体 `cargo fmt`**（会产生大量无关 diff）。
- libunftp 0.20 的 PORT 处理无 bounce 防护，主动模式必须保持开关默认关闭（`FtpServerOptions.allow_active_mode`，默认 false）。
- rcgen 依赖必须是 `default-features=false, features=["pem","aws_lc_rs"]`（默认 ring 需要 C 编译器）。
- 图标唯一来源 `app/src-tauri/icons/icon.ico`，三份派生物（ui/src/assets/icon.png、public/favicon.png）需一起重导。

## 版本与发版

- **升版本 5 处同步**：`crates/ftp-core/Cargo.toml`、`app/src-tauri/Cargo.toml`、`app/src-tauri/tauri.conf.json`、`app/ui/package.json`、`README.md` 下载段文件名。
- **先提交再发版**：安装包一旦上传 Releases，对应源码必须已在 main 上（发版门禁详见 `docs/code-review.md`）；同版本号重复出 MSI 会报 1638。
- 开发者/发布者固定 **halacrcrc**（tauri.conf.json bundle.publisher；exe CompanyName 与 MSI Manufacturer 均需核对）。
- 前端改动必须重新打包（dist 内嵌二进制），出包约 1–6 分钟。
- **每次发版要出两个 WebView2 变体**：默认 `tauri.conf.json` 是 downloadBootstrapper（在线装 WebView2）；再出一份 embedBootstrapper（引导器内嵌，离线机可用）——`app/src-tauri/tauri.conf.embed.json` 只覆写 `webviewInstallMode`，出包命令 `node <项目内 tauri.js> build --config src-tauri/tauri.conf.embed.json`（仍在 `app/` 目录）。两个变体产物同名，第二个出包后改名（加 `-offline` 后缀）再放 `dist/`，**两个都要上传 Release**。
- 安装包不进版本库（仓库根 `dist/` 被 .gitignore 忽略），只上传 GitHub Releases。
- 提交信息用 Conventional Commits 风格（英文）：`feat(sftp): ...`、`fix(tftp): ...`。

## 贡献许可

项目以 MIT OR Apache-2.0 双许可发布；提交即视为按双许可授权，不接受者不要提 PR。
