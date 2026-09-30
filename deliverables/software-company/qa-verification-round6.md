# QA 验证报告 — Round 6（d650ad9 新功能验证 + 待办清单复核）

- 验证人：QA Engineer（严过关）
- 对象：FTP Toolbox（Tauri v2 + React + Rust），版本 0.3.0
- 被测提交：`d650ad9`（feat(ui): show transfer speed and stop dropping structured log fields）
- 方式：**单元测试 + 类型检查 + 真实 GUI 端到端**（走真实 DOM 交互，不绕过 UI 直调后端）
- 结论：**待办清单 5/5 已闭环；三条新功能全部在真实界面成立；过程中发现并修复 1 个真实缺陷**（速度系统性低估 3.1 倍）

---

## 一、审查发现 · 待办清单复核

| # | 条目 | 状态 | 证据 |
|---|------|------|------|
| 1 | `MAX_READ_CHUNK` 限制客户端请求的读长度 | 已闭环 | `crates/ftp-core/src/sftp/server.rs:53`，配 `read_chunk_len()` 及 2 条单测 |
| 2 | `CONNECT_TIMEOUT` 连接超时 | 已闭环 | `crates/ftp-core/src/sftp/client.rs:116`，配 `refused_connection_fails_fast_instead_of_timing_out` |
| 3 | `safe_path` 符号链接限制注释 | 已闭环 | `server.rs:441` 词法检查的说明 |
| 4 | 空用户名拒绝 | 已闭环 | `server.rs:175`，配 `empty_username_is_rejected_at_start` |
| 5 | `transfer_kind_for` / `write_known_hosts_file` | 已闭环 | `server.rs` / `keys.rs`，配 `rw_handle_reports_one_direction_throughout`、`known_hosts_upsert_leaves_no_temp_file` |

**唯一有意保留项**：`opendir` 仍全量预加载目录（非懒加载）。已在代码注释中说明取舍，行为未变——属已知限制而非缺陷。

> 结论：本轮**不需要**新的整改工作，待办清单无遗留。

---

## 二、d650ad9 三条新功能的逐项验证

### ① 页脚显示传输速度 —— 成立（但先发现一个缺陷，见第三节）

**证据（修复后，192 MB TFTP 上传，真实界面）：**

```
4 213 ms  上传中 · up192.bin 67% · 129.1 MB / 192.0 MB · 30.9 MB/s
4 936 ms  上传中 · up192.bin 79% · 151.2 MB / 192.0 MB · 30.8 MB/s
6 260 ms  上传中 · up192.bin 100% · 191.2 MB / 192.0 MB · 30.5 MB/s
```

- 全程 30.1–31.2 MB/s，与字节增量实测值（3.6 MB / 120 ms ≈ 30 MB/s）一致；
- 日志侧权威平均值为 **30.6 MB/s**，两者相差 < 2%；
- 首个采样点（120 ms）**不显示速度**，符合"没有真实样本前不显示 0 B/s"的设计；
- 已知大小时同排显示百分比与 `已传 / 总量`，大小未知时（TFTP 下载）只显示已传字节数与速度。

视觉证据：`transfer-speed-log-2026-09-30/02-footer-progress-speed.png`

### ② 日志带大小 / 时长 / 平均速度 —— 成立

**证据（同一轮 GUI 会话，运行日志视图原文）：**

```
20:36:54 上传开始: up192.bin（192.0 MB）
20:37:01 完成: up192.bin（192.0 MB · 6.3s · 平均 30.6 MB/s）
20:32:36 下载开始: big.bin（大小未知）
20:32:36 完成: big.bin（6.0 MB · 0.3s · 平均 18.2 MB/s）
```

- 已知大小 → `（192.0 MB）`；TFTP 下载无 `tsize` 协商 → `（大小未知）`，两条分支都走到；
- `完成` 行带 `大小 · 时长 · 平均速度`，三者齐全。

视觉证据：`transfer-speed-log-2026-09-30/01-log-view-size-elapsed-fields.png`

### ③ 后端结构化日志字段不再被丢弃 —— 成立（这是 d650ad9 修的真实 bug）

**证据（界面日志视图，TFTP 服务端事件）：**

```
[tftp::server] tftp WRQ: client wants to upload peer=127.0.0.1:50061 file=up192.bin blksize=8192
[tftp::server] tftp receive complete file=up192.bin bytes=201326592 blocks=24577 elapsed_ms=6260
[tftp::server] tftp server listening local=127.0.0.1:6969
```

修复前这些行只有 `tftp receive complete`，`bytes` / `blocks` / `elapsed_ms` 只出现在终端。
**这是 README 承诺过的能力，此前在 UI 上根本不成立**——本轮确认已在界面上成立。

---

## 三、本轮发现的缺陷（已修复）

### D1（严重）：页脚速度系统性低估，64 MB 上传显示 7.8 MB/s，真实为 24.3 MB/s

**发现路径**：GUI 端到端采样页脚时，注意到速度**恒定**为 7.8 MB/s，而字节数每 120 ms 涨 3.2 MB（≈27 MB/s）——两者自相矛盾。

**证据（修复前，d650ad9）：**

| 层 | 数值 |
|----|------|
| 页脚显示 | `7.8 MB/s`（21 个采样点全程不变） |
| 日志权威平均 | `24.3 MB/s`（`tftp receive complete … elapsed_ms=2626`，64 MB / 2.626 s） |
| 偏差 | **3.1 倍** |

**根因**（数字精确对上，不是猜的）：

1. 回环上 8192 字节的块间隔只有约 **0.3 ms**（8193 块 / 2.626 s）；
2. `Date.now()` 分辨率是 **1 ms**，绝大多数事件量出的 `now - lastAt` 就是 `0`；
3. 旧实现用 `Math.max(1, dt)` 把 0 夹成 1 ms，于是
   `8192 B ÷ 1 ms = 8.192 MB/s = 7.81 MiB/s` —— **正是显示值**；
4. EMA（0.7/0.3）把这个假值锁死，所以全程不动。

**修复**（`app/ui/src/lib/transfer.ts`）：引入 `MIN_SPEED_WINDOW_MS = 200`，窗口没攒够就
**只推进字节数、不动窗口起点**，于是下一次的差值天然覆盖整个窗口，时钟分辨率误差被稀释到 1/200 以下。
`lastAt`/`lastBytes` 相应改名为 `speedAt`/`speedBytes`，语义明确为"测速窗口起点"，并留注释警告不要改成每条事件刷新。

**修复后复测：**

| 场景 | 页脚速度 | 日志权威平均 | 偏差 |
|------|---------|-------------|------|
| 64 MB TFTP 上传 | 27.4–28.8 MB/s | 27.7 MB/s | < 4% |
| 192 MB TFTP 上传 | 30.1–31.2 MB/s | 30.6 MB/s | < 2% |
| 6 MB TFTP 下载 | — | 18.2 MB/s | 传输仅 313 ms，不足一个窗口，符合预期 |

**回归测试**：`advanceSample: 回环上块间隔远小于时钟分辨率时不再系统性低估`
（模拟 0.3 ms 一块、`Date.now()` 只能分辨 1 ms 的场景，断言速率与真实值误差 < 1%）。
修复前该用例会得到 8.19 MB/s。

**副作用（可接受）**：速度显示延后到首个窗口（200 ms）攒够之后。短于 200 ms 的传输不显示速度——
这类传输的速度本来也没有参考价值，而日志里的平均速度仍然照常给出。

---

## 四、测试执行结果

### 前端（新增测试基建，零新运行时依赖）

```
$ npm test          # = node --test src/lib/format.test.ts src/lib/transfer.test.ts
# tests 22
# pass 22
# fail 0

$ npm run typecheck # tsc --noEmit
（无输出，通过）

$ npm run build     # vite build
✓ 48 modules transformed.  ✓ built in 4.31s
```

用 **Node 22 内置测试运行器**（`node --test` 直接加载 `.ts`），不需要 vitest/jest，
不引入任何运行时依赖。唯一新增的 devDependency 是 `@types/node`（让 `tsc` 认识 `node:test` 的类型）。

### Rust

```
$ cargo test -p ftp-core
# 单元测试：66 passed（原 61 + 新增 5 条 log_fields）
# 集成测试：22 passed（ftp_bind_errors 3 / ftp_data_channel 1 / ftp_server_lifecycle 8
#                      ftps_loopback 4 / sftp_loopback 5 / tftp_loopback 1）
# fail 0

$ cargo check -p ftp-toolbox-app
Finished `dev` profile
```

### GUI 端到端（真实界面，非直调后端）

链路：TFTP 服务器卡片填表 → 点「启动服务」→ 切 TFTP 客户端 → 填表 → 点「上传/下载」→ 读页脚与日志。

| 检查项 | 结果 |
|--------|------|
| TFTP 服务器经界面启动 | 已监听 127.0.0.1:6969 运行中 |
| 6 MB 下载 | 落盘 `cmp` **内容一致** |
| 64 MB 上传 | 落盘 `cmp` **内容一致** |
| 192 MB 上传 | 落盘 `cmp` **内容一致** |
| 页面错误事件 | 无 EXCEPTION、无 console error |

---

## 五、本轮改动文件

**新增**

- `app/ui/src/lib/format.ts`、`app/ui/src/lib/transfer.ts`（纯逻辑，可单测）
- `app/ui/src/lib/format.test.ts`、`app/ui/src/lib/transfer.test.ts`
- `crates/ftp-core/src/log_fields.rs`（含 5 条单测）

**修改**

- `app/ui/src/App.tsx`、`api.ts`、`components/ProgressBar.tsx`、`views/SftpClientView.tsx`
- `app/ui/tsconfig.json`（`allowImportingTsExtensions`）、`package.json`（`test` / `typecheck` 脚本）
- `crates/ftp-core/src/lib.rs`、`crates/ftp-core/Cargo.toml`（dev-dep `tracing-subscriber`）
- `app/src-tauri/src/lib.rs`（改为复用 `ftp_core::event_parts`）
- `README.md`（补测试章节）

**为什么把日志字段采集下沉到 ftp-core**：`FrontendLogLayer` 原本把 visitor 写在 Tauri 壳里，
要测它就得链接整个 Tauri 应用（Windows 上还要拉 WebView2）。下沉到 `ftp-core::log_fields` 后
与 `progress.rs` 一样属"给前端用的管道"，用 `cargo test` 就能覆盖，且 Tauri 壳只是调用方。

---

## 六、未覆盖 / 遗留

1. **FTP / FTPS 的页脚速度未单独采样**：本轮 GUI 只跑了 TFTP。FTP 的进度事件粒度不同
   （按读取块而非 8192 字节块），修复对其同样有效（窗口机制与事件粒度无关），但未做实测。
2. **只测了回环**：真实网卡（WLAN）下速率更低、事件间隔更大，是修复更有利的方向。
3. **进度条动画、多传输并发**（页脚单槽位）未纳入本轮范围——单槽位是已知设计，未改。
4. 前端测试只覆盖 `src/lib/` 下的纯逻辑，**React 组件的订阅/重渲染路径靠 GUI 端到端覆盖**，
   没有组件级测试基建（未引入 jsdom / testing-library）。
