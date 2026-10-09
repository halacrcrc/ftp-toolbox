# 整改报告：0.3.4 事后审计发现（2026-10-09）

对应评审：`code-review-034-audit-2026-10-09.md`（审计区间 `fee031d..8be8cf9`，发现 #20–#25）。

本轮处置用户裁定的 3 条 🟡（#20/#21/#22）；3 条 🟢（#23/#24/#25）记录为延后。
改动已本地复验（见文末），**提交/发版待确认**。

## 逐条闭环

| # | 级别 | 标题 | 处置 | 复验 |
| --- | --- | --- | --- | --- |
| #20 | 🟡 | SFTP 下载 rename 失败不发 Error 事件 → 进度条卡死 | 修复：`sftp/client.rs` 下载 `Ok(bytes)` 分支的 `rename(.part → 目标)` 失败路径补发 `TransferEvent::Error{SftpDownload}`，与 FTP 下载 rename 路径（`ftp/client.rs`）对齐 | ✅ 新增失败用例 `sftp_download_rename_failure_emits_error_event` 通过（目标为已存在目录 → rename 必失败） |
| #21 | 🟡 | TFTP 下载本地落盘失败不发 Error 事件 → 进度条卡死 | 修复：`tftp/client.rs` 下载在 `written` 的 `Err` 分支与 `rename` 失败分支各补 `emit_err(...Download...)`；覆盖 create/write_all/flush 全部失败路径（与 FTP 下载 #11 同类） | ✅ 新增失败用例 `download_local_write_failure_emits_error_event` 通过（父目录不存在 → `File::create` 必失败） |
| #22 | 🟡 | README 把 TFTP `tsize` 落地版本误标为 v0.3.4 | 修复：`README.md` roadmap 改回 `— v0.3.3 落地`（`e701518` 由 tag `v0.3.3` 包含，MEMORY 亦记为 0.3.3） | ✅ 文档断言与 `git tag --contains e701518` 事实一致 |
| #23 | 🟢 | FTP 上传本地源读失败绕过统一 Error 分支 | 延后（触发概率极低：源文件已打开且顺序读）；建议下轮与写侧统一 `break Err` | — |
| #24 | 🟢 | handoff 断言漂移（`origin/main`、tag 指向） | 不修：handoff 属历史快照；建议后续写明「截至 <时间>」或指向 `git rev-parse` 命令 | — |
| #25 | 🟢 | abort 路径 `data.close().await` 无超时上界 | 延后（明文 TCP 下 `close` 立即返回，风险限于 FTPS）；建议下轮与排空读同口径套 `IDLE_TIMEOUT` | — |

## 改动清单（工作区，未提交）

- `crates/ftp-core/src/sftp/client.rs` —— #20 补发 Error（+13 −1）
- `crates/ftp-core/src/tftp/client.rs` —— #21 补发 Error（rename + Err 两处）
- `crates/ftp-core/tests/sftp_loopback.rs` —— #20 回归用例（+47）
- `crates/ftp-core/tests/tftp_loopback.rs` —— #21 回归用例（+40）
- `README.md` —— #22 版本纠正；下载段 `0.3.4 → 0.3.5`
- 版本 5 处同步至 **0.3.5**：`crates/ftp-core/Cargo.toml`、`app/src-tauri/Cargo.toml`、
  `app/src-tauri/tauri.conf.json`、`app/ui/package.json`、`README.md`；另顺带
  `CODEBUDDY.md`（当前版本）、`Cargo.lock`（由 cargo 自动重生成）

## 复验（2026-10-09 实跑）

| 命令 | 结果 |
| --- | --- |
| `cargo test -p ftp-core` | **113 passed / 0 failed**（80 单测 + 33 集成；基线 111 → +2 新用例） |
| `cargo check --workspace --all-targets` | Finished（无 error/warning） |
| `cd app/ui && npm test` | 22/22 pass |
| `cd app/ui && npm run typecheck` | 通过 |
| 定向 `rustfmt --edition 2021 --check`（本轮触碰文件） | `tftp/client.rs`／两个测试文件 **零偏差**；`sftp/client.rs` 仍为 **10 处 = 基线 10 处**（本轮新增块不在其中），未整体 fmt |

> 环境备注：`cargo` 命令因仓库外 `target-dir` 沙箱无写权限（`os error 5`）改用非沙箱执行。

## 遗留（记录在案）

- #23/#24/#25 三条 🟢 延后，建议并入下轮审计清账。
- 本轮整改提交自身按 `docs/code-review.md` 节点 2 需并入下轮审计。