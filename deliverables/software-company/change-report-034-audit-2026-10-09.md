# 整改报告：0.3.4 事后审计发现（2026-10-09）

对应评审：`code-review-034-audit-2026-10-09.md`（审计区间 `fee031d..8be8cf9`，发现 #20–#25）。

两轮处置：第一轮修 3 条 🟡（#20/#21/#22），第二轮清 3 条 🟢（#23/#24/#25），
**#20–#25 全部闭环**。改动已本地复验（见文末）；**未打包、未发版**。

## 逐条闭环

| # | 级别 | 标题 | 处置 | 复验 |
| --- | --- | --- | --- | --- |
| #20 | 🟡 | SFTP 下载 rename 失败不发 Error 事件 → 进度条卡死 | 修复：`sftp/client.rs` 下载 `Ok(bytes)` 分支的 `rename(.part → 目标)` 失败路径补发 `TransferEvent::Error{SftpDownload}`，与 FTP 下载 rename 路径（`ftp/client.rs`）对齐 | ✅ 新增失败用例 `sftp_download_rename_failure_emits_error_event` 通过（目标为已存在目录 → rename 必失败） |
| #21 | 🟡 | TFTP 下载本地落盘失败不发 Error 事件 → 进度条卡死 | 修复：`tftp/client.rs` 下载在 `written` 的 `Err` 分支与 `rename` 失败分支各补 `emit_err(...Download...)`；覆盖 create/write_all/flush 全部失败路径（与 FTP 下载 #11 同类） | ✅ 新增失败用例 `download_local_write_failure_emits_error_event` 通过（父目录不存在 → `File::create` 必失败） |
| #22 | 🟡 | README 把 TFTP `tsize` 落地版本误标为 v0.3.4 | 修复：`README.md` roadmap 改回 `— v0.3.3 落地`（`e701518` 由 tag `v0.3.3` 包含，MEMORY 亦记为 0.3.3） | ✅ 文档断言与 `git tag --contains e701518` 事实一致 |
| #23 | 🟢 | FTP 上传本地源读失败绕过统一 Error 分支 | **已修复**（第二轮）：`ftp/client.rs` 上传循环的 `file.read` 失败改为 `break Err`，与写侧走同一统一分支补发 Error（`2026-10-09 #23` 跟进） | ✅ `cargo test` 113 通过（路径在既有取消/错误用例覆盖范围内，未新增独立用例：文件打开后顺序读失败的确定性复现需故障注入） |
| #24 | 🟢 | handoff 断言漂移（`origin/main`、tag 指向） | **已处理**（第二轮）：`handoff-2026-10-09.md` 文末加「勘误（#24）」表；MEMORY.md 约定固化「handoff 不得硬编码易变 git 状态」；`origin/main`/tag 指向均已逐条核实 | ✅ 文档断言与 `git rev-parse`/`git rev-list` 一致 |
| #25 | 🟢 | abort 路径 `data.close().await` 无超时上界 | **已修复**（第二轮）：`ftp/client.rs` 两处收尾 `close`（上传取消路径 + `abort_retr`）包 `tokio::time::timeout(cancel::IDLE_TIMEOUT, …)`，与排空读同口径 | ✅ `cargo test` 113 通过；`check --all-targets` Finished |

> 第二轮（2026-10-09 下午）用户裁定：3 条 🟢 一并处理完。此时整改报告进入最终状态：
> **#20–#25 全部闭环**，无遗留发现。

## 改动清单

- `crates/ftp-core/src/sftp/client.rs` —— #20 补发 Error（+13 −1）
- `crates/ftp-core/src/tftp/client.rs` —— #21 补发 Error（rename + Err 两处）
- `crates/ftp-core/tests/sftp_loopback.rs` —— #20 回归用例（+47）
- `crates/ftp-core/tests/tftp_loopback.rs` —— #21 回归用例（+40）
- `README.md` —— #22 版本纠正；下载段 `0.3.4 → 0.3.5`
- 版本 5 处同步至 **0.3.5**：`crates/ftp-core/Cargo.toml`、`app/src-tauri/Cargo.toml`、
  `app/src-tauri/tauri.conf.json`、`app/ui/package.json`、`README.md`；另顺带
  `CODEBUDDY.md`（当前版本）、`Cargo.lock`（由 cargo 自动重生成）

第二轮（🟢 清账）：

- `crates/ftp-core/src/ftp/client.rs` —— #23 源读失败改 `break Err`；#25 两处 `data.close()` 套 `IDLE_TIMEOUT`
- `deliverables/software-company/handoff-2026-10-09.md` —— #24 文末加勘误表
- `.workbuddy-ai/memory/MEMORY.md` —— #24 固化「handoff 不硬编码易变 git 状态」约定

## 复验（2026-10-09 实跑）

| 命令 | 结果 |
| --- | --- |
| `cargo test -p ftp-core` | **113 passed / 0 failed**（80 单测 + 33 集成；基线 111 → +2 新用例） |
| `cargo check --workspace --all-targets` | Finished（无 error/warning） |
| `cd app/ui && npm test` | 22/22 pass |
| `cd app/ui && npm run typecheck` | 通过 |
| 定向 `rustfmt --edition 2021 --check`（本轮触碰文件） | `tftp/client.rs`／`ftp/client.rs`／两个测试文件 **零偏差**；`sftp/client.rs` 仍为 **10 处 = 基线 10 处**（本轮新增块不在其中），未整体 fmt |

> 环境备注：`cargo` 命令因仓库外 `target-dir` 沙箱无写权限（`os error 5`）改用非沙箱执行。

## 遗留（记录在案）

- 无未闭环发现（#20–#25 全部闭环）。
- 本轮整改提交（`fix(client)` ×2 + `chore(release): 0.3.5` + `docs` ×N）自身按
  `docs/code-review.md` 节点 2 需并入下轮审计。