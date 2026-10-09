# 代码审查 0.3.4 整改轮事后审计（fee031d..8be8cf9，2026-10-09）

## 审计范围与方法

- **提交区间**：`git diff fee031d..HEAD`，共 7 个提交（新→旧）
  `8be8cf9` / `f8fee98` / `c14eef2` / `a9a4a5b` / `2045a4a` / `9f30444` / `9489ac9`。
  HEAD = `8be8cf9`，工作区干净（`git status --short` 为空）。区间内非文档代码改动
  集中在 `9489ac9`（10 文件）与 `9f30444`（2 文件）；其余为发版/构建/文档。
- **方法**：
  1. 逐提交 `git show` + 逐文件读**当前态**源码核对（不以抽样代替）；
  2. 深读前端事件处理 `app/ui/src/App.tsx`（L106-L147）+ 三个客户端视图的
     `transfer()`（`FtpClientView.tsx#L68-L83`、`TftpClientView.tsx#L22-L36`、
     `SftpClientView.tsx#L137`）确认「进度条只被 `done`/`error` 事件复位」这一前提；
  3. 读 `docs/code-review.md` 红线/分级、`CODEBUDDY.md`、`.workbuddy/memory/MEMORY.md`；
  4. 逐条核对 `code-review-033-remediation-2026-10-09.md`（#11–#19）与
     `change-report-033-remediation-2026-10-09.md` 的闭环声明；
  5. 跑四条验证命令（实际输出见下）；对 `9489ac9` 涉及文件逐文件
     `rustfmt --edition 2021 --check`，并与 `fee031d` 基线版本对照计数；
  6. 用 `git rev-parse`/`git rev-list`/`git show-ref` 核实版本号、tag、`origin/main`
     等可验证断言（未轻信提交信息与既有报告）。
- **评审人**：独立第三方（审计代理），非提交作者、非整改方。

## 整体评估

⚠️ —— `9489ac9` 对 #11–#17 的整改**方向正确、实现可逐行核对**（#11 排空、
#13 排空读套 `IDLE_TIMEOUT`、#15 日期、#16 定向 fmt + 基线恢复、#17 门禁升级均成立），
四条验证命令全绿且测试基线 **111（80 单测 + 31 集成）** 精确吻合；但整改只覆盖了
评审**已枚举**的失败路径，同类的**兄弟路径仍漏发 `TransferEvent::Error`**（SFTP 下载
rename、TFTP 下载本地落盘、FTP 上传源读），会复现同类「进度条卡死」；另 `a9a4a5b`
把 README 中 TFTP `tsize` 的落地版本从 v0.3.3 误改为 v0.3.4（与 tag 事实相悖），属
文档漂移。**无 Critical/High，不阻塞已完成的 0.3.4 发版**，建议下轮清账。

## 发现清单（按严重度排序）

### 2026-10-09 #20 SFTP 下载 rename 失败不发 Error 事件，进度条卡死（🟡）

- **位置**：`crates/ftp-core/src/sftp/client.rs#L496-L511`（`Ok(bytes)` 分支内
  `tokio::fs::rename` 失败处 `return Err(Error::Io(e))`）。
- **分析**：`Started` 已在 L456 发出；`rename` 失败时直接 `return`，**不发
  `TransferEvent::Error`**。前端 `App.tsx#L142-L144` 的进度条只在 `error`/`done`
  事件里 `apply(null)` 复位，视图的 `catch` 只写日志、不碰进度态
  （`SftpClientView.tsx#L137`）。结果：改名失败（目标被占用/只读/权限）后进度条
  停在原位。这与本轮已修的 **FTP 下载 rename 路径（`ftp/client.rs#L281-L292`）是
  同一类缺陷**，SFTP 侧被漏掉。
- **修复建议**（对齐 `ftp/client.rs#L281-L292`）：
  ```rust
  // 现在
  if let Err(e) = tokio::fs::rename(&part, local).await {
      let _ = tokio::fs::remove_file(&part).await;
      return Err(Error::Io(e));
  }
  // 改为
  if let Err(e) = tokio::fs::rename(&part, local).await {
      let _ = tokio::fs::remove_file(&part).await;
      let err = Error::Io(e);
      TransferEvent::emit(
          &progress,
          TransferEvent::Error {
              kind: TransferKind::SftpDownload,
              file: remote.to_string(),
              message: error_chain(&err),
          },
      );
      return Err(err);
  }
  ```

### 2026-10-09 #21 TFTP 下载本地落盘失败不发 Error 事件，进度条卡死（🟡）

- **位置**：`crates/ftp-core/src/tftp/client.rs#L152`（`File::create(&part).await?`）、
  `#L159`/`#L195`（`out.write_all(...).await?`）、`#L174`/`#L209`（`out.flush().await?`），
  落盘块 `Err(e)` 分支 `#L233-L236`（仅 `remove_file` + `Err(e)`）。
- **分析**：TFTP 下载的 `Started` 在握手成功后、写盘块之前发出（`#L136-L143`）；
  写盘块内任何 `?` 直接把错误抛出到 `written` 的 `Err` 分支，**该分支只删 `.part`
  并返回，不发 `Error` 事件**。因此本地磁盘满/权限不足时，进度条永久停在
  `Started`。与本轮已修的 **FTP 下载本地写失败（#11，`ftp/client.rs#L235-L276`）
  同类**，TFTP 侧未覆盖。
- **修复建议**（在 `Err(e)` 分支补事件即可，块内 `?` 不必改动）：
  ```rust
  Err(e) => {
      let _ = tokio::fs::remove_file(&part).await;
      TransferEvent::emit(
          &progress,
          TransferEvent::Error {
              kind: TransferKind::Download,
              file: name,
              message: e.to_string(),
          },
      );
      Err(e)
  }
  ```

### 2026-10-09 #22 README 把 TFTP `tsize` 归属到 v0.3.4，与 tag 事实相悖（🟡 文档漂移）

- **位置**：`README.md#L148`，`a9a4a5b` 改动行：
  `- [x] TFTP \`tsize\` 协商（…）— v0.3.3 落地` → `… — v0.3.4 落地`。
- **分析**：`tsize` 协商的落地提交是 `e701518`
  `feat(tftp,client): negotiate tsize and make client transfers cancellable`，
  而 tag `v0.3.3` 指向其子提交 `ce828d8`（`git rev-list -n1 v0.3.3` →
  `ce828d8…`，`git tag --contains e701518` 列出 `v0.3.3`），tag 说明亦为
  “v0.3.3 — TFTP tsize negotiation, cancellable client transfers”。
  故该特性属于 **v0.3.3**，改标 v0.3.4 属事实错误。且 `a9a4a5b` 的提交信息只声明
  「5 处版本号 + 移除 deliverables 指针」两项，**未提及**这处 roadmap 修改（夹带）。
  `.workbuddy/memory/MEMORY.md` 亦记 “0.3.3 = TFTP tsize + 取消/超时”，与 tag 一致。
- **修复建议**：把 `README.md#L148` 改回 `— v0.3.3 落地`；发版提交如夹带此类
  文档改动应在提交信息中列明。

### 2026-10-09 #23 FTP 上传本地源文件读失败绕过 Error 事件（🟢）

- **位置**：`crates/ftp-core/src/ftp/client.rs#L130-L133`。
- **分析**：`let n = match cancel::chunk(… file.read(&mut buf)).await { Ok(r) => r?, … }`
  中 `Ok(r) => r?` 收到 `Ok(Err(io))` 时会把 io 错误 `?` **直接抛出 `upload()`**，
  跳过下方统一发 `Error` 的 `Err(e)` 分支（L179-L200）。`Started` 已发
  （L98-L105），故源盘读失败（磁盘/IO 异常）时进度条同样卡死。对照同循环的写路径
  （L137-L139 走 `break Err(e)`）可见不一致。触发概率极低（源文件已打开且顺序读）。
- **修复建议**：与写侧保持一致，`Ok(r) => match r { Ok(n) => n, Err(e) => break Err(Error::Io(e)) }`
  （或统一 `break Err(e.into())`），交由现有 `Err` 分支发事件。

### 2026-10-09 #24 handoff 的可验证断言已漂移（🟢）

- **位置**：`deliverables/software-company/handoff-2026-10-09.md#L8-L10`。
- **分析**：
  - 其称 `origin/main` = `c14eef2`；实测 `git rev-parse origin/main` = `8be8cf9`
    （= HEAD），**已漂移**（`8be8cf9` 于 `f8fee98` 之后提交并推送）。
  - 其称发版序列 `a9a4a5b → c14eef2 → tag v0.3.4`；实测 **tag `v0.3.4` 指向
    `a9a4a5b`**（`git show-ref`/`git rev-list -n1 v0.3.4` → `a9a4a5b…`），即
    `c14eef2`（Cargo.lock 重生成）**落在 tag 之后**；因此 tag 处的 `Cargo.lock`
    里 `ftp-core` / `ftp-toolbox-app` 版本仍是 `0.3.3`（`git show v0.3.4:Cargo.lock`）。
- **影响**：均属快照类文档，非功能性；`Cargo.lock` 会由 cargo 构建时自动更新。
- **修复建议**：handoff 属历史快照，可保留；后续 handoff 对 `origin/main` 一类
  易变断言加“截至 <时间>”限定，或改为指向 `git rev-parse` 命令而非硬编码哈希。

### 2026-10-09 #25 abort 路径 `data.close().await` 仍无超时上界（🟢）

- **位置**：`crates/ftp-core/src/ftp/client.rs#L335`（`abort_retr`）与 `#L186`（上传取消路径）。
- **分析**：`#13` 把排空读收口到 `drain_closing_response` 并套了 `IDLE_TIMEOUT`
  （L339-L353），但其前置的 `let _ = data.close().await;` **未设界**。对端静默时，
  TLS 关闭（close_notify 写）理论上仍可能滞留——即 `#13` 想消除的“取消路径永久
  挂起/持有会话锁”存在一条未封口的残留。普通 TCP（明文）下 `close` 立即返回，
  风险主要限于 FTPS。
- **修复建议**：与排空读同口径设界：
  ```rust
  let _ = tokio::time::timeout(cancel::IDLE_TIMEOUT, data.close()).await;
  ```

## #11–#19 闭环核对表

| # | 级别 | 标题要点 | 结论 | 代码位置 / 证据 |
| --- | --- | --- | --- | --- |
| #11 | 🟡 | FTP 下载本地写失败不排空 | ✅ 已闭环 | 内层只落盘，外层 `match r` 对 `Err` 统一 `abort_retr`（`ftp/client.rs#L240-L276`，`abort_retr` L329-L337）。`File::create`/`write_all`/`flush` 任一失败均走排空。 |
| #12 | 🟡 | Error 事件五路径缺口 | ✅ 已闭环（有同类残留，见 #20/#21/#23） | STOR 拒绝 `ftp/client.rs#L107-L123`；finalize 失败 L156-L168；下载 rename 失败 L281-L292；SFTP create 失败 `sftp/client.rs#L368-L384`；TFTP 握手失败 `tftp/client.rs#L74-L100/L116-L126`（上传方向 Started 在握手前 L253-L260，补发有效；下载方向 Started 在握手后 L136-L143，补发无害）。**未枚举的兄弟路径仍漏**→ 新发现 #20/#21/#23。 |
| #13 | 🟡 | 排空读无超时上界 | ✅ 已闭环（`close()` 残留见 #25） | `drain_closing_response` 包 `tokio::time::timeout(cancel::IDLE_TIMEOUT, read_response_in(..))`（`ftp/client.rs#L339-L353`），上传/下载取消路径统一调用（L188、L336）。`cancel::IDLE_TIMEOUT` = 30s（`cancel.rs#L86`）。 |
| #14 | 🟡 | 取消上传测试竞态断言 | ✅ 已闭环 | 「服务器不得留文件」断言已删，新增调度竞态说明注释，与 SFTP 取消测试姿势对齐（`tests/tftp_loopback.rs`，`cancelled_upload_reports_cancelled`）。 |
| #15 | 🟡 | 注释评审日期错位 | ✅ 已闭环 | `grep 2026-10-08` 在 `crates/` 下**零命中**；全部改为 2026-10-09（含 `tftp/server.rs#L297` 的 `#9`，由 `9f30444` 修正）。 |
| #16 | 🟡 | 新增代码未过 rustfmt | ✅ 已闭环 | 逐文件 `rustfmt --check`：本轮新增代码所在文件 `cancel.rs`/`ftp/client.rs`/`tftp/client.rs`/三个测试文件**零偏差**；`sftp/client.rs` 10 处 = `fee031d` 基线 10 处（无新增）。基线恢复：`git diff fee031d..HEAD -- tftp/packet.rs` **为空**，`server.rs` **仅 1 行**（日期注释）。 |
| #17 | 🟢 | 编译告警 + 测试死重 | ✅ 已闭环 | 未用绑定 `n`×2/`an`/`addr` 与经典测试的死重（真实服务器/共享目录/种子文件）已清；`cargo check --workspace --all-targets` → Finished（无 error）。 |
| #18 | 🟢 | TFTP 伪造包循环无上界 | ✅ 记录取舍（未修，符合声明） | `tftp/server.rs` 本区间**逻辑未改**（仅 1 行注释），未偷改协议行为；change-report/handoff 均记录为已知取舍。 |
| #19 | 🟢 | 排队期取消按钮可见 | ✅ 记录取舍（未修） | 区间内 `app/ui/src/**` 无源码改动（仅 `package.json` 版本），UI 行为未变，与声明一致。 |

**结论**：#11–#19 **无未闭环项**；#18/#19 为恰当记录的取舍（非偷偷改坏）。#12 就
“已枚举五路径”而言闭环，但其**同类的兄弟路径**（#20/#21/#23）为本轮新发现。

## 验证结果（2026-10-09 实跑）

| 命令 | 结果 | 说明 |
| --- | --- | --- |
| `cargo test -p ftp-core`（`CARGO_INCREMENTAL=0`） | **80 单测 + 31 集成 = 111 passed / 0 failed**；doc-tests 0 | 精确吻合基线 111。集成明细：ftp_active_mode 2 + ftp_bind_errors 3 + ftp_data_channel 2 + ftp_server_lifecycle 8 + ftps_loopback 4 + sftp_loopback 6 + tftp_loopback 6 = 31。 |
| `cargo check --workspace --all-targets`（`CARGO_INCREMENTAL=0`） | **Finished**（`dev` profile，约 43.96s），无 error/warning | 仅 MSVC linker 中文 stdout 基线告警（`linker_messages`）。 |
| `cd app/ui && npm test` | **22/22 pass** | `node --test src/lib/format.test.ts src/lib/transfer.test.ts`。 |
| `cd app/ui && npm run typecheck` | **通过**（`tsc --noEmit` 无输出） | — |
| 定向 `rustfmt --edition 2021 --check <touched files>` | 本轮新增代码零偏差；基线未动 | 详见 #16。 |

> **环境备注**：两条 `cargo` 命令首跑失败于 `failed to create directory
> …\.fingerprint\… : 拒绝访问 (os error 5)`（沙箱对仓库外 `target-dir` 无写权限），
> 改用非沙箱执行后通过。另：`cargo fmt --check` / `cargo clippy -p ftp-core
> --all-targets` 基线本即红，本轮未作为回归项，也**未**执行整体 `cargo fmt`。

## 逐提交结论

| 提交 | 类型 | 结论 |
| --- | --- | --- |
| `9489ac9` | 修复（#11-#17） | ✅ 整改方向与实现正确、可逐行核对；#11/#13/#15/#17 完全闭环，#12 已枚举路径闭环。⚠️ 同类兄弟路径（SFTP rename / TFTP 本地写 / FTP 源读）仍漏发事件 → #20/#21/#23。 |
| `9f30444` | 修复（日期/基线） | ✅ 忠实：`packet.rs` 净零改动（基线恢复），`server.rs` 仅保留 1 行日期修正。 |
| `2045a4a` | 文档（归档） | ✅ 仅新增 2 个评审/整改报告文件，无副作用。 |
| `a9a4a5b` | 发版 | ⚠️ 5 处版本号同步正确（见下），但 README roadmap 把 tsize 误改为 v0.3.4（#22），且未在提交信息说明。 |
| `c14eef2` | 构建 | ✅ `Cargo.lock` 中 `ftp-core`/`ftp-toolbox-app` 均 `0.3.3 → 0.3.4`，与版本号一致。 |
| `f8fee98` | 文档 | ⚠️ handoff/CODEBUDDY 大体准确；`origin/main=c14eef2` 等断言已漂移（#24）。 |
| `8be8cf9` | 构建 | ✅ `tauri.conf.embed.json` **只覆写** `bundle.windows.webviewInstallMode`（type=embedBootstrapper）；默认 `tauri.conf.json` 为 downloadBootstrapper；README/CODEBUDDY 双变体说明与之一致。 |

### 发版五处同步核对（`docs/code-review.md` 要求）

| 位置 | 值 | 结论 |
| --- | --- | --- |
| `crates/ftp-core/Cargo.toml#L3` | `0.3.4` | ✅ |
| `app/src-tauri/Cargo.toml#L3` | `0.3.4` | ✅ |
| `app/src-tauri/tauri.conf.json#L4` | `0.3.4` | ✅ |
| `app/ui/package.json#L4` | `0.3.4` | ✅ |
| `README.md#L17-L18` 下载段 | `ftp-toolbox_0.3.4_x64-setup.exe` / `ftp-toolbox_0.3.4_x64_en-US.msi` | ✅ |

（`Cargo.lock` 作为第 6 个派生物，见 #24：tag `v0.3.4` 处仍为 0.3.3，regen 在 tag 之后。）

## 发现统计

- 🔴 Critical：**0**
- 🟠 High：**0**
- 🟡 Medium：**3** —— #20（SFTP 下载 rename 缺 Error 事件）、#21（TFTP 下载本地写缺 Error 事件）、#22（README tsize 版本漂移）
- 🟢 Low：**3** —— #23（FTP 上传源读绕过 Error）、#24（handoff 断言漂移）、#25（abort `close()` 无界）

**闭环核对**：#11–#19 **全部闭环或恰当记录取舍，无未闭环项**。
**测试**：达标（111 = 80 + 31，四条命令全绿）。