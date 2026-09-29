# ftp-toolbox 全库代码审计报告

- **日期**：2026-09-29
- **仓库**：`C:\WorkBuddy\FTP`（ftp-toolbox v0.2.5，Rust + Tauri 2 + React 18）
- **审计方法**：open-code-review-delegate（range 模式，空树 base → HEAD 快照 `fa4e5d8`，全库 46 个可审查文件）+ 宿主逐文件人工审查 + `cargo test --workspace` 运行时验证
- **技术栈**：`ftp-core`（纯 tokio 引擎：libunftp FTP 服务端 / suppaftp 客户端 / 自研 TFTP）+ Tauri 命令层 + React UI

---

## 1. 覆盖率

| 指标 | 数值 |
|------|------|
| 代码文件总数（git 跟踪） | 60 |
| OCR 可审查 | 46（其余为二进制图标、Markdown 交付物、Cargo.lock 等） |
| 已逐文件审查 | 41 |
| 结构性抽查（styles.css、index.html、tsconfig、5 个集成测试经由 `cargo test` 行为验证） | 5 |
| **覆盖率** | **100%**（46/46 全部 accounted for） |

审查依据规则组：Rust 规则（所有权/错误处理/unsafe/并发/异步取消安全/安全敏感代码）与 TS/React 规则（Hooks/状态/安全检查）。

---

## 2. 发现汇总

| 级别 | 数量 | 编号 |
|------|------|------|
| Critical | 0 | — |
| High | 2 | H-1, H-2 |
| Medium | 3 | M-1 ~ M-3 |
| Low | 5 | L-1 ~ L-5 |
| 备注（设计使然，不要求修改） | 4 | N-1 ~ N-4 |

---

## 3. High

### H-1 Tauri 安全配置：`csp: null` + `withGlobalTauri: true`（security）

**位置**：[tauri.conf.json](file:///c:/WorkBuddy/FTP/app/src-tauri/tauri.conf.json#L13-L24)

```json
"withGlobalTauri": true,
"security": { "csp": null }
```

CSP 完全禁用，且 API 以全局对象注入。Tauri 官方安全清单将 `csp: null` 列为发布前应修复项：一旦 webview 内出现 XSS（本应用渲染来自**任意远端 FTP/TFTP 服务器**的文件名、目录列表、错误消息——恶意服务器是现实威胁模型），没有任何第二道防线，且 `withGlobalTauri` 让注入脚本无需 import 即可直接 `invoke()` 调用后端命令（含 `start_ftp_server`、`ftp_download`——可把任意文件写到磁盘任意路径）。

**缓解因素（为何实际风险降级）**：前端全部经 React 转义渲染，无 `dangerouslySetInnerHTML`/`innerHTML`/`eval`；远端数据仅以文本显示；capabilities 已最小化（仅 `core:default` + `dialog`）。

**建议修复**（发布前）：

```json
"withGlobalTauri": false,
"security": {
  "csp": "default-src 'self'; img-src 'self' asset: data:; style-src 'self' 'unsafe-inline'"
}
```

移除 `withGlobalTauri` 需确认前端无 `window.__TAURI__` 直接引用（已核查：`api.ts` 全部使用 ES import，无引用）。

### H-2 被动端口区间语义与 libunftp 实际行为不一致（off-by-one，CI 偶发失败根因）（bug）

**证据链**（动态测试 + 库源码双重确认）：

1. **测试实跑失败**：`cargo test --workspace` 首跑，`custom_passive_ports_are_used_for_pasv` 失败——配置 `40000..40010`，PASV 应答 `227 Entering Passive Mode (127,0,0,1,156,74)` = 端口 **40010**（156×256+74），恰为半开区间的排他端点。
2. **libunftp 0.20 源码定位**（`pasv.rs:46-57`）：

```rust
let rng_length = passive_ports.end - passive_ports.start + 1;   // +1！
let port = random_u32 % rng_length + passive_ports.start;
```

   libunftp 从**闭区间 `[start, end]`** 均匀随机取端口。
3. **复跑即绿**：`--no-fail-fast` 重跑全部通过（33+3+1+8+4+1）——失败是概率性的：取到 `end` 的概率 1/11 ≈ 9%，**该测试是 flaky test**，CI 将间歇性挂掉。

**影响面**（ftp-toolbox 全库按「半开区间」假设编写，[passive.rs](file:///c:/WorkBuddy/FTP/crates/ftp-core/src/ftp/passive.rs#L26-L28) 注释明示「libunftp takes it half-open」——与实际相反）：

- `label()` 与 [lib.rs](file:///c:/WorkBuddy/FTP/app/src-tauri/src/lib.rs#L127) 启动消息显示 `start..end` 为 `start-(end-1)`：**UI 少报一个端口**；用户按显示段开防火墙会漏掉 `end`。
- `conflicts()`/`managed_overlaps()` 只查到 `end-1`：当 `end` 恰落在系统保留段而 `end-1` 不在时，**真冲突被漏报**（默认段 50000..50100 的 50100 与实测保留段 50000-50059 不重叠，故默认配置暂无实害；自定义段可触发）。
- 测试断言 `ports.contains(&port)` 按半开：1/11 概率失败。

**修复方向（二选一）**：

- **A（推荐）**：承认 libunftp 闭区间语义，全库改口径——`label()` 显示 `start-end`、冲突检测 inclusive 上界取 `end`、测试断言含 `end`、文档注释更正；
- **B**：保持对外半开语义，传给 libunftp 前收紧为 `start..end-1`（单点修改 [server.rs](file:///c:/WorkBuddy/FTP/crates/ftp-core/src/ftp/server.rs#L119-L121)），其余不动。

修复后该 flaky test 应稳定通过（A）或保持原断言成立（B）。

---

## 4. Medium

### M-1 `check_passive_ports` 在主线程同步调用 `netsh`（performance）

**位置**：[lib.rs](file:///c:/WorkBuddy/FTP/app/src-tauri/src/lib.rs#L453-L520) — `#[tauri::command] fn check_passive_ports`（非 async）→ `excluded_tcp_ranges()` → `std::process::Command::new("netsh").output()`

Tauri 2 中**非 async 命令在主线程执行**。`netsh` 子进程启动 + 表格输出实测 50-300ms，而该命令由前端被动端口输入框 250ms debounce 后触发（[ServersView.tsx](file:///c:/WorkBuddy/FTP/app/ui/src/views/ServersView.tsx#L179-L188)）——每次停顿输入都会冻结 UI 与 webview 事件循环一个 netsh 周期。`ftps_cert_info`（首次会生成 ECDSA 密钥对 + 写盘）同理但只执行一次。

**建议**：改为 `async fn`（内部 `spawn_blocking` 或直接容忍，量级小），或将 netsh 结果缓存数秒。

### M-2 TFTP WRQ 无文件大小上限（security/robustness）

**位置**：[tftp/server.rs](file:///c:/WorkBuddy/FTP/crates/ftp-core/src/tftp/server.rs#L290-L331) `recv_file`

`tsize` 选项未协商（mod.rs 文档已声明），服务端无法预知写入量，`File::create` 直接覆盖同名文件，对端可持续发送 DATA 块直至写满磁盘。结合 N-2（无认证），LAN 内任意主机可向共享目录灌入无限数据。

**建议**：短期在 UI/文档注明「仅可信局域网使用」；长期可对 WRQ 会话加累计字节上限（如 4 GiB）超限即发 ERROR 并断开。

### M-3 FTP 下载中断可能残留半成品文件（robustness）

**位置**：[ftp/client.rs](file:///c:/WorkBuddy/FTP/cates/ftp-core/src/ftp/client.rs#L138-L160) `download`

直接 `File::create(local)` 开始流式写入；中途出错（网络断、超时）返回 `Err` 但已写入的半成品文件留在目标路径，且进度事件已发 `Error`——用户重试时若选同名路径，`File::create` 会截断重来（尚可），但若未注意，磁盘上留有不完整文件易误用。TFTP 客户端下载路径相同。

**建议**：先写 `local + ".part"`，成功后 `rename`；失败时删除 `.part`。

---

## 5. Low

| 编号 | 位置 | 问题 | 建议 |
|------|------|------|------|
| L-1 | [App.tsx](file:///c:/WorkBuddy/FTP/app/ui/src/App.tsx#L49-L52) + [LogView.tsx](file:///c:/WorkBuddy/FTP/app/ui/src/views/LogView.tsx#L27) | 日志用 `slice(-499)` 截头但渲染 `key={i}` 索引 key，截断后所有行内容与 key 错位平移（纯文本行无内部状态，仅性能与可访问性小损） | 改用单调递增 id 作 key |
| L-2 | [api.ts](file:///c:/WorkBuddy/FTP/app/ui/src/api.ts#L196-L201) `fmtBytes` | ≥1 TiB 显示为 `1024.00 GB` | 加 TB 档 |
| L-3 | [ServersView.tsx](file:///c:/WorkBuddy/FTP/app/ui/src/views/ServersView.tsx#L635-L642) | 默认根目录硬编码 `C:\ftp-root` / `C:\tftp-root`，非 Windows 平台无意义 | 按平台给默认值或留空提示 |
| L-4 | [.cargo/config.toml](file:///c:/WorkBuddy/FTP/.cargo/config.toml) | 本机开发配置（rsproxy 镜像、OneDrive 规避的 target-dir 指向个人目录）提交进仓库，其他机器构建行为漂移 | 拆为 `config.toml`（仓库通用）+ 本机未跟踪覆盖；或注释说明 |
| L-5 | [lib.rs](file:///c:/WorkBuddy/FTP/app/src-tauri/src/lib.rs#L554-L558) | `ftps_cert_info` 为同步命令且首次调用含密钥生成 + 文件写入，主线程短暂阻塞 | 并入 M-1 一并改 async |

---

## 6. 备注（设计使然，评估后不要求修改）

- **N-1 FTPS `required: false` 固定为可选 TLS**：勾选「启用 FTPS」后明文客户端仍可登录——注释与 UI 文案均已明确说明这是有意为之（渐进采用），凭据可能仍走明文的风险已向用户披露（[FtpClientView.tsx](file:///c:/WorkBuddy/FTP/app/ui/src/views/FtpClientView.tsx#L120-L125) 有中间人警告）。若未来面向不可信网络，建议增加「强制 TLS」开关。
- **N-2 TFTP/FTP 匿名模式本质无认证**：工具定位是局域网文件传输；TFTP 协议（RFC 1350）本身无认证。防火墙提示日志（不自行开端口）处理得当。
- **N-3 TFTP 会话无并发/速率限制**：恶意 UDP flood 可 spawn 大量会话任务（每任务一个 SessionGuard，仅计数无上限）。LAN 威胁模型下可接受。
- **N-4 Windows 下私钥无显式 ACL**：`tls.rs` 仅在 unix 上 `chmod 600`；Windows 依赖 `%APPDATA%` 的 per-user 目录默认 ACL，对单用户桌面应用足够。

---

## 7. 分模块审查记录

### 7.1 `crates/ftp-core`（引擎，~1900 行）

- **`ftp/server.rs`**：libunftp 封装结构清晰；`StaticAuth` 凭据仅存内存 HashMap（不落盘）；每连接重建 `Server::service` 的设计有注释支撑；bind 前置校验（root 目录、证书文件存在性）；accept 失败不致命（`continue`）。`broadcast(1)` 信号 + `LoopGuard`/`SessionGuard` 的 panic 安全生命周期管理是亮点（lifecycle.rs 有针对性单测）。
- **`ftp/client.rs`**：TLS 在 `USER/PASS` 之前协商（凭据不裸奔，注释明示）；`accept_invalid_certs` 双开关（cert+hostname）仅在 UI 明确勾选时生效；`ProgressReader` 用 `Pin`/`poll_read` 正确实现 futures-io 包装。
- **`tftp/packet.rs`**：手写编解码器边界检查完备（短包、缺终结符、未知 opcode 均有 typed error）；blksize 按 RFC 2348 clamp（8..65464）；`decode` 对畸形输入返回 `Result` 而非 panic。测试覆盖 round-trip + legacy + clamp。
- **`tftp/server.rs`**：**路径遍历防护已实现**（`resolve()` 拒绝 `..` 组件）；RFC 2348 OACK 握手、Sorcerer's apprentice 重复块重 ACK、超时重传 `MAX_RETRIES=5 × TIMEOUT=3s` 均正确。
- **`tftp/client.rs`**：新 TID `recv_from` + `connect` 的握手处理正确（注释解释了为何不能按请求地址过滤）；进度事件在 error 路径也发送（`emit_err`）。
- **`passive.rs`**：纯函数 + 12 个单测；netsh 表格解析仅依赖「行首两个数字」不依赖本地化表头；managed/plain 排除段区分有实测依据并写明出处。`parse` 拒绝闭区间写法的 65535（溢出保护）。
- **`error.rs`**：`BindCause` 分类 + Windows wildcard 二次探测是精心设计（有 6 个单测锁定平台错误码映射）；`error_chain` 解决 thiserror 不走 source 链的问题。
- **`tls.rs`**：自签证书 SAN 覆盖 localhost + 双栈回环；有效期前后各留 1 天偏移；损坏自愈但指纹可见（身份变化不静默）；unix 0o600。
- **`net.rs` / `lifecycle.rs` / `progress.rs`**：纯决策函数与共享状态机，测试密度高（lifecycle 的「loop 死了 UI 不许说还在运行」回归测试尤其好）。

### 7.2 `app/src-tauri`（Tauri 壳，~950 行）

- 命令层确如注释所述是「薄壳」：协议逻辑零泄漏到壳层。
- `AppState` 的 `std::Mutex`（句柄槽）与 `tokio::Mutex`（跨 await 的客户端会话）选型有明确注释论证（deadlock/Send 理由），符合并发规则。
- `start_ftp_server` 的「先查活实例再启动」防重复守护、`stop` 的「signal + wait 端口确定释放」语义正确。
- `progress_forwarder` 每命令新建 mpsc + spawn，任务随 tx drop 退出，无泄漏。
- `CREATE_NO_WINDOW`（0x08000000）防 netsh 闪窗、firewall 提示只打日志不自行改防火墙——安全意识到位。
- `FrontendLogLayer` 把 tracing 事件桥接到前端，`record_debug`/`record_str` 双实现避免 `{:?}` 引号污染。

### 7.3 `app/ui`（React，~1600 行 ts/tsx + 600 行 css）

- **架构**：运行态单一事实源在后端（状态查询 + watch 推送），组件卸载不丢状态——注释详述了此前「切页后按钮复活」的 bug 根因，方案正确。
- **Hooks**：全部顶层调用；每个 `listen` 都有 `then(unlisten)` 清理；防抖（250ms）+ `cancelled` 标志防竞态；`useRef` 做「只警告一次」的边沿触发。
- **无任何** `innerHTML`/`eval`/`var`/`==`/`any`/嵌套三元；无组件内定义组件。
- 密码不持久化（`pass` 独立 state 且注释声明）；localStorage 只存非敏感偏好且损坏自愈。
- 网卡轮询 3s + visibility 感知 + 快照逐字比较防抖重渲染——细节考虑周到。

### 7.4 配置与依赖

- `capabilities/default.json`：最小权限（core:default + dialog open/save）✅。
- 依赖选型合理（libunftp/suppaftp/native-tls/rcgen 均为主流维护库）；Cargo.lock 已提交保证可重现构建；`rcgen` 用 `aws_lc_rs` 而非 ring 的理由有注释（沙箱无 C 编译器）。
- `tauri.conf.json` bundle 仅 Windows 目标（nsis/msi），与「Windows 优先」定位一致（但见 L-3）。
- 前端依赖极简（仅 react + tauri api/plugin-dialog），无臃肿。

### 7.5 测试（`cargo test --workspace` 结果）

见下方「运行时验证」。5 个集成测试覆盖：FTPS 回环（自签证书真实握手 + 传输）、bind 错误分类、被动数据通道、服务器生命周期（含意外退出场景）、TFTP 回环。

---

## 8. 运行时验证

`cargo test --workspace` 审计当日执行两遍（第二遍 `--no-fail-fast`）：

| 套件 | 结果 |
|------|------|
| ftp-core 单元测试（33 个） | ✅ 全部通过 |
| `ftp_bind_errors`（3） | ✅ 通过 |
| `ftp_data_channel`（1） | ✅ 通过 |
| `ftp_server_lifecycle`（8） | ⚠ 首跑 **7/8**（`custom_passive_ports_are_used_for_pasv` 失败），复跑 8/8 —— **概率性失败**，根因即 H-2（libunftp 闭区间取端口，断言按半开，1/11 触发概率） |
| `ftps_loopback`（4） | ✅ 通过（含真实 TLS 握手与传输） |
| `tftp_loopback`（1） | ✅ 通过 |

**测试体系评价**：集成测试用真实 socket/TLS 验证而非 mock，回归测试明确针对历史 bug（bind 误分类、UI 状态撒谎等）编写；H-2 的发现本身正是这套测试的价值证明。

---

## 9. 总体结论

| 维度 | 评估 |
|------|------|
| 架构与分层 | 优秀（引擎/壳/UI 职责边界严格，注释解释"为什么"而非"是什么"） |
| 正确性 | 良好（协议边界处理严谨，回归测试针对历史 bug；H-2 区间语义错误是唯一行为级缺陷，已实证） |
| 并发/异步 | 良好（锁选型有论证，取消安全有 guard 保障） |
| 安全 | 中（H-1 配置层缺失纵深防御；路径遍历、凭据时序、注入等代码层防护到位） |
| 可维护性 | 良好（测试密度高，错误信息面向操作者） |

**结论：代码质量整体处于高位，无阻塞性缺陷。发布前建议完成 H-1（CSP + withGlobalTauri）与 H-2（被动端口区间语义，CI 稳定性 + 冲突检测正确性），M-1/M-2/M-3 按优先级排期。**

---

## 10. 行动清单

- [x] **H-2**：修正被动端口区间语义 — 已于 2026-09-29 完成，采用方案 B：[server.rs](file:///c:/WorkBuddy/FTP/crates/ftp-core/src/ftp/server.rs#L118-L128) 传入 libunftp 前收紧为 `start..end-1`（单点修改），全库保持对外半开口径不变；`custom_passive_ports_are_used_for_pasv` 复跑稳定通过，flaky 消除
- [x] **H-1**：配置 CSP 并关闭 `withGlobalTauri` — 已完成（[tauri.conf.json](file:///c:/WorkBuddy/FTP/app/src-tauri/tauri.conf.json#L12-L26)），前端已确认无 `window.__TAURI__` 引用；CSP 运行时行为建议在下次 `tauri dev/build` 人工回归一遍 UI
- [x] **M-1/L-5**：`check_passive_ports`、`ftps_cert_info`、`ftps_regenerate_cert` 改为 async 命令，netsh/密钥生成移入 `tauri::async_runtime::spawn_blocking` — 已完成
- [x] **M-2**：TFTP WRQ 累计字节上限 4 GiB（`MAX_UPLOAD_BYTES`），超限发 ERROR(3) 并删除半成品文件 — 已完成
- [x] **M-3**：FTP/TFTP 下载改写 `.part` 临时文件，成功后 rename；任一失败路径清理 `.part` — 已完成
- [x] **L-1~L-4**：日志改单调递增 id 作 key、`fmtBytes` 加 TB 档、共享目录默认值按平台给值（非 Windows 留空 + placeholder）、`.cargo/config.toml` 加本机配置说明注释 — 已完成

**验证**：`cargo test --workspace` 全绿（33+3+1+8+4+1，含 ftps/tftp 回环与此前 flaky 的被动端口测试）；`tsc --noEmit` 通过。
