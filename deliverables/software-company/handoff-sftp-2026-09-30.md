# 交接文档 — ftp-toolbox SFTP 模块（2026-09-30）

> 面向接手本项目的下一个 agent / 开发者。本文是 `handoff-sftp-0.3.0-2026-09-29.md` 的续篇：
> 那一篇覆盖 v0.3.0 的实现与环境坑，**本文不重复其内容，只记录其后的增量**（aee7ae4 修复、
> 本次审查发现、当前待办）并给出导航。**规格以 `docs/sftp-design.md` 为准。**

---

## 一、当前状态

- `main` @ `aee7ae4`，工作区干净，**两个提交（4c78328 + aee7ae4）均只在本地，未 push**
  （按用户惯例：构建验证后经用户确认才推）。
- `cargo test -p ftp-core` 全绿（2026-09-30 实测）。
- 版本号 0.3.0，四处同步（`tauri.conf.json` / `app/src-tauri/Cargo.toml` /
  `crates/ftp-core/Cargo.toml` / `app/ui/package.json`）。

## 二、上一篇交接之后的增量

1. **提交 `aee7ae4`** — 修复 russh-sftp 错误文案双重渲染（"Permission denied: Permission
   denied" → "SFTP 会话错误: Permission denied"）。修复点全在 `client.rs`
   （`describe_sftp_error` / `sftp_core_err` / `sftp_io_err`），按错误变体匹配、
   io 路径用 `get_ref()` 而非 `into_inner()`。6 个新单元测试 + loopback 端到端断言钉死。
   背景机制详见 `code-review-sftp-2026-09-30.md` §二.3。
2. **本次代码审查**（报告：`deliverables/software-company/code-review-sftp-2026-09-30.md`），
   结论 = Comment（通过）。三个 🟡 加固项已列入下方待办。

## 三、审查发现 · 待办清单（按优先级）

| # | 严重度 | 事项 | 位置 | 工作量 |
|---|---|---|---|---|
| 1 | 🟡 | `read()` 按客户端控制的 `len`（u32）裸分配 buf，恶意客户端可 OOM；封顶 ~1 MiB | `server.rs:727` | ~3 行 + 测试 |
| 2 | 🟡 | `SftpClient::connect` 无 `inactivity_timeout`，黑洞服务器可挂死握手；对齐探针的 10s | `client.rs:224` | 1 行配置 |
| 3 | 🟡 | root 约束不解析符号链接（词法层防护；与 OpenSSH 行为一致）→ 写进规格"已知限制" | `docs/sftp-design.md` | 文档 |
| 4 | 🟢 | 空用户名配置 → 所有认证必然失败且无提示；考虑在 `start_sftp_server` 拒绝 | `lib.rs` 命令层 | 小 |
| 5 | 🟢 | RW 句柄进度方向混报 / 回环场景双事件源进度交错 / `opendir` 全量预载 / known_hosts 非原子写 / 过时注释 | 各处 | 可选 |

**重要约定（改代码前必读，均来自 `MEMORY.md` / 上一篇交接）**：

- **未经用户明确确认不要 `git push`**；提交信息英文 + conventional commits。
- 新增 IPC 结构体**逐个核对** `#[serde(rename_all = "camelCase")]`——漏标注不会报错，
  只会静默丢字段（v0.3.0 命中过 3 处）。改完跑 `tls.rs` 里的序列化回归测试模式。
- `cargo check --workspace` 绿 ≠ 测试目标能编译；验证用 `cargo test --no-run`。
- 加密后端锁死 `aws-lc-rs`（ring 在本机 C 工具链下编译失败），别"优化"回 ring。
- 本仓库 `cargo clippy --all-targets` / `cargo fmt --check` **本来就是红的**（既有告警 +
  格式漂移）；只对齐自己新增的代码，别整体 `cargo fmt`，别把既有告警当成自己引入的。
- `SftpClient` 只有密码认证是设计契约（`docs/sftp-design.md` §2.3）；要加客户端公钥
  登录属于规格变更，先问用户。
- russh-sftp 的错误双重渲染是**库行为**；去重只许加在我们格式化的地方
  （`client.rs`），别去改服务端回包。

## 四、验证配方（本机）

- 引擎测试：`cargo test -p ftp-core`（全部）。
- GUI 联调：需要 `export WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--no-sandbox
  --remote-debugging-port=9222"`（Chromium 沙箱在本机初始化失败导致白屏；**只用于联调，
  不写进 tauri.conf.json**）。WebView2 白屏排查用技能 `webview2-tauri-headless-debug`（CDP
  驱动真实 DOM）。
- 文件传输测试**不要**用系统 OpenSSH 的 `sftp.exe`/`scp.exe`（MSYS 下恒报
  `pipe: Unknown error`）；用 `examples/sftp_probe.rs` 打已在运行的服务端。
- `cargo target` 目录（`C:\Users\22534\.workbuddy\build\ftp-toolbox-target`）在杀软
  白名单里；构建前关掉正在运行的应用（exe 被锁会 LNK1104）。

## 五、关键文件导航

| 想知道什么 | 看哪里 |
|---|---|
| SFTP 规格与 Q1–Q12 决策（含为什么不支持 X） | `docs/sftp-design.md` |
| v0.3.0 实现细节、GUI 联调发现（CSP/camelCase 等 3 个真实缺陷） | `deliverables/software-company/handoff-sftp-0.3.0-2026-09-29.md` |
| 本次审查的完整发现与 🎉 亮点 | `deliverables/software-company/code-review-sftp-2026-09-30.md` |
| 引擎：服务端 / 客户端 / 密钥与 TOFU | `crates/ftp-core/src/sftp/{server,client,keys}.rs` |
| Tauri 命令层（11 个命令 + DTO） | `app/src-tauri/src/lib.rs`（`// ---------- SFTP` 段起） |
| 前端线格式契约 | `app/ui/src/api.ts`（`---------- SFTP` 段） |
| TOFU 确认模态框流程 | `app/ui/src/views/SftpClientView.tsx` |
| 集成测试 | `crates/ftp-core/tests/sftp_loopback.rs`（5 个） |

## 六、建议下一个 agent 加载的技能（Skill 工具）

- `code-review-skill` — 对待办 #1/#2 的小改动做语言级审查（Rust 指南）。
- `handoff` — 完成待办后再产出增量交接。
- `webview2-tauri-headless-debug` — 若涉及 GUI 联调/白屏排查。
- 若要把审查发现落成 issue 或 PR：先确认远端是否已 push，再问用户。

## 七、遗留事项汇总

1. 两个提交未 push → 问用户。
2. 待办表 #1–#5（见第三节）。
3. `deliverables/` 下归档文件已 12 篇，命名按 `<类型>-<主题>-<日期>.md`；QA 验证已到
   round5，下次 GUI 联调从 round6 编号续起。

---

## 八、后记：第三节待办已闭环（2026-09-30 晚）

整改记录见 **`deliverables/software-company/change-report-sftp-2026-09-30.md`**
（逐项对应表 + 修改理由 + 验证结果）。摘要：

- **#1 / #2 / #6 / #5 / #9 已改代码**，各带回归测试；**#3 / #4 / #7 / #8 / #10 只补
  注释或文档**（知情取舍，不改行为）。
- **⚠️ #2 没有采用报告字面建议的 `inactivity_timeout`**：查 russh 0.63 源码确认它是
  **整条连接生命周期**的会话级定时器，而本应用把长生命周期 `SftpClient` 存在
  `AppState.sftp_client` 里跨命令复用（且未配 `keepalive_interval`），挂 10s 会悄悄
  断掉健康的空闲会话。改为只给 connect 序列加 15s deadline（`CONNECT_TIMEOUT`）。
  **下一个改 `client.rs` 的人不要"顺手改回" `inactivity_timeout`。**
- 空用户名校验放在 `start_sftp_server` 里、bind 与生成主机密钥**之前**，且刻意只判空
  不 trim —— 与认证门 `!username.is_empty()` 严格对齐，避免出现"启动放行但登录必拒"。
- 仍未提交、未 push（等用户确认）。
