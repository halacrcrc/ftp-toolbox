# ftp-toolbox

基于 **Tauri v2 + React + Rust** 的 FTP / FTPS / SFTP / TFTP 服务器与客户端一体桌面工具。

![Rust](https://img.shields.io/badge/Rust-1.89+-orange?logo=rust)
![Tauri](https://img.shields.io/badge/Tauri-v2-24C8D8?logo=tauri)
![React](https://img.shields.io/badge/React-18-61DAFB?logo=react)
![Platform](https://img.shields.io/badge/platform-Windows-blue?logo=windows)
![License](https://img.shields.io/badge/license-MIT_OR_Apache--2.0-blue)

一个窗口同时搞定「起服务」和「传文件」：左边开 FTP / SFTP / TFTP 服务器，右边用内置客户端连上去互传，底部实时进度，后端日志逐条可见——调嵌入式设备、路由器、旧仪器这类只支持 TFTP/FTP 的对端，或者需要一条加密的 SFTP 通道时，不用再东拼西凑一堆小工具。

## 下载

Windows 安装包见 [Releases](https://github.com/halacrcrc/ftp-toolbox/releases/latest)：

- `ftp-toolbox_0.3.3_x64-setup.exe` — NSIS 安装程序，向导式安装（推荐）
- `ftp-toolbox_0.3.3_x64_en-US.msi` — MSI 安装包，适合批量部署

依赖系统自带 WebView2（Win10/11 通常已预装）。安装包未做代码签名，首次运行 SmartScreen 会提示「未知发布者」，点「仍要运行」即可。

0.2.x 及更早版本的安装包在同一 Releases 页面的历史记录里。

从源码构建见下方[快速开始](#快速开始)。

## 功能

- **FTP 服务器**：匿名 / 账号密码认证，被动端口 50000-50099，会话级日志（基于 libunftp）
- **FTPS**：显式 TLS（`AUTH TLS`），自动生成并复用自签证书，可强制加密登录与数据通道，证书指纹可查看 / 重新生成
- **FTP 客户端**：连接 / 列目录 / 上传 / 下载，全程进度反馈
- **SFTP 服务器**（v0.3.0 起）：基于 russh 的 SSH 传输 + SFTP v3 协议
  - 密码 或 OpenSSH 公钥（authorized_keys）认证，任一通过即放行
  - 只读模式（连 `CREATE` / `TRUNCATE` 走私一并拦下）、共享目录约束
  - ed25519 主机密钥首次启动自动生成，指纹可核对 / 重新生成
- **SFTP 客户端**：连接 / 列目录 / 上传 / 下载
  - **TOFU 主机密钥校验**：首连需确认指纹并记入 known_hosts；指纹变更一律硬拒绝，只能显式更新
- **TFTP 服务器 / 客户端**（自实现 RFC 1350）：
  - RFC 2348 `blksize` 协商（请求 8192，单文件上限 ~536 MB，兼容端自动回退 512 字节经典模式）
  - RFC 2349 `tsize` 协商（下载前获知文件大小，进度条显示真实百分比；上传时声明大小，超限服务端直接拒绝）
  - 超时重传、防目录穿越、每连接独立线程（新 TID）
- **服务器配置体验**：
  - 共享目录走系统原生文件夹选择框，配置自动记忆（密码不保存）
  - 监听地址自动枚举本机网卡，FTP 默认 21 / TFTP 默认 69 / SFTP 默认 2222（客户端连远端默认 22）
- **详细运行日志**：后端引擎 + libunftp 会话日志实时推送到前端，TFTP 每次传输记录对端、文件名、字节数、块数、耗时——排错直接看日志面板
- **传输可取消**：底部进度条一键中止当前传输（FTP / FTPS / SFTP / TFTP 客户端均支持，引擎在分块边界落地）；分块读写带空闲超时，对端静默失联不会再卡死客户端
- **现代 UI**：侧边栏导航、卡片式布局（React + TypeScript + Vite）
  - 全局传输进度条：百分比 + 已传 / 总量 + **实时速度**（大小未知时只报已传字节与速度）
  - 传输日志带**大小 · 耗时 · 平均速度**；日志面板逐条显示后端结构化字段（`bytes=` `blocks=` `elapsed_ms=`）

## 架构

```
crates/ftp-core    核心引擎（纯 tokio，不依赖任何 GUI）
  src/ftp/         FTP server（libunftp）+ client（suppaftp），FTPS 证书（自签）
  src/sftp/        SFTP server + client（russh + russh-sftp），主机密钥与 TOFU
  src/tftp/        TFTP（RFC 1350 + RFC 2348）server + client，自实现
app/src-tauri      Tauri v2 壳：命令层 + 进度/日志事件转发
app/ui             React 前端（Vite + TypeScript）
```

核心库与 GUI 完全解耦：想换成 CLI、gpui 或别的壳，只需要替换 `app/`，核心代码一行不动。

各模块的规格与决策记录见 `docs/`（如 `docs/sftp-design.md` 记了 SFTP 的 Q1–Q12 取舍）；
评审、交接与验证报告归档在 `deliverables/software-company/`。

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
ui/node_modules/.bin/tauri build
```

> ⚠ 请用上面这个项目内的 CLI。`cargo tauri build` 走的是 `cargo install` 装的全局 `cargo-tauri`，它可能是 **1.x**，会拿 v1 的 schema 去校验这份 `tauri.conf.json`（v2 格式）并报一堆 `Additional property 'app' is not allowed`。

> 产物默认落在仓库内的 `target/release/bundle/{nsis,msi}/`（NSIS 出 `setup.exe`，WiX 出 `.msi`）。
> 仓库根的 `dist/` 是发布用的归档目录，被 `.gitignore` 忽略 ——
> **安装包不进版本库**，只用于手动上传到 GitHub Releases。
>
> 如果你的工作副本放在 OneDrive / 网盘同步目录里（构建脚本会被文件锁和
> 按需占位文件干扰，报 `os error 5` 或 `LNK1104`），把 target 目录挪出仓库即可，
> **不要改仓库里的 `.cargo/config.toml`** —— 用环境变量或你自己的 `~/.cargo/config.toml`：
>
> ```bash
> # 环境变量（一次性 / CI 用）
> export CARGO_TARGET_DIR=~/cargo-target/ftp-toolbox      # macOS/Linux
> setx CARGO_TARGET_DIR D:\cargo-target\ftp-toolbox       # Windows
> ```
>
> ```toml
> # ~/.cargo/config.toml（只影响本机，不进版本库）
> [build]
> target-dir = "D:/cargo-target/ftp-toolbox"
> ```

## 测试

```bash
cargo test -p ftp-core   # 引擎全部单测 + 回环集成测试

cd app/ui
npm test                 # 前端纯逻辑单测（Node 内置测试运行器，无需额外依赖）
npm run typecheck        # tsc --noEmit
```

回环测试会起真实服务端再用内置客户端互传：FTP / FTPS / TFTP 各一组，SFTP 一组
（首连信任 → 列目录 → 上传下载 → 只读拒绝 → 主机密钥变更拒绝）。

前端单测只覆盖 `src/lib/` 下的纯逻辑（字节数格式化、速度 EMA、日志文案）。
它用的是 Node 22+ 自带的 `node --test`（直接加载 `.ts`，无需 vitest/jest），
所以 `src/lib/*` 里的 import 必须带 `.ts` 后缀 —— Node 的 ESM 解析器不做扩展名
补全，Vite 打包则不受影响。

设计规格与决策记录放在 `docs/`，例如 SFTP 的 Q1–Q12 决策见 `docs/sftp-design.md`。

## 已知限制

- TFTP 未实现 `timeout` 选项协商；对端不支持 `blksize` / `tsize` 时回退经典模式
- TFTP 69 端口在 Linux/macOS 需要特权；Windows 上若与其他 TFTP 服务冲突会提示 bind 失败
- FTP 服务器为单用户静态账号；多用户 / 细粒度权限未接入（libunftp 的现成能力）
- **SFTP 客户端只支持密码认证**（设计如此，见 `docs/sftp-design.md` Q5）；公钥认证是服务端能力
- **SFTP 共享目录的约束是词法层的，不解析符号链接** —— root 内指向外部的链接会被正常跟随。
  这与 OpenSSH `sftp-server` 一致，且服务端无建链入口；把 `root_dir` 当作共享边界而非沙箱
- 安装包未做代码签名（详见上文「下载」）

## Roadmap

- [x] FTP over TLS（FTPS）— v0.2.5 落地
- [x] TFTP `tsize` 协商（下载前获知文件大小，进度条可显示百分比）— v0.3.3 落地
- [ ] FTP 远端文件树浏览（拖拽上传/下载）
- [ ] SFTP 客户端公钥认证（属规格变更，需先定私钥来源）

## License

本项目采用 **MIT 或 Apache-2.0 双许可**，使用者可任选其一（SPDX：`MIT OR Apache-2.0`）。
与 Rust 生态主流项目（rust-lang、serde、tokio 等）保持一致 —— 相比单一 MIT，
Apache-2.0 额外提供**明确的专利授权**，对法务敏感的公司用户更友好。

- [LICENSE-MIT](LICENSE-MIT)
- [LICENSE-APACHE](LICENSE-APACHE)

安装程序会在向导第二步展示许可协议（内容见 `app/src-tauri/license.rtf`），
并把两份全文安装到程序目录，便于分发时满足 Apache-2.0 §4(a) 的随附要求。
