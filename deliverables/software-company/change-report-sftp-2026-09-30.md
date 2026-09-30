# 整改报告 — SFTP 模块审查发现闭环（2026-09-30）

- 基线：`main` @ `aee7ae4`（v0.3.0 + 错误文案去重修复）
- 依据：`deliverables/software-company/code-review-sftp-2026-09-30.md`（结论 Comment）
        + `deliverables/software-company/handoff-sftp-2026-09-30.md` §三 待办表
- 原则：**不改变原有功能与行为**；凡属知情取舍的一律只补注释/文档，不改逻辑
- 验证：`cargo test -p ftp-core` 全绿 —— 61 个单元测试（新增 6）+ 22 个集成测试
  （5 个 SFTP loopback + 3 + 1 + 8 + 4 + 1），0 失败

---

## 一、改动清单（逐项对应报告条目）

| # | 报告条目 | 严重度 | 位置 | 改动 | 类型 |
|---|---|---|---|---|---|
| 1 | #1 `read()` 按客户端 `len` 裸分配 | 🟡 | `server.rs` | 新增 `MAX_READ_CHUNK = 1 MiB` 与 `read_chunk_len()`，`read()` 按封顶值分配 | 修复 |
| 2 | #2 `connect` 无握手超时 | 🟡 | `client.rs` | 新增 `CONNECT_TIMEOUT = 15s` 包裹整段 connect 序列 | 修复 |
| 3 | #3 root 约束不解析符号链接 | 🟡 | `server.rs` / `docs/sftp-design.md` | `safe_path` 补已知限制注释；设计文档 §七 新增「已知限制」 | 文档 |
| 4 | #6 空用户名必然认证失败且无提示 | 🟢 | `server.rs` | `start_sftp_server` 在开端口前拒绝空用户名 | 修复 |
| 5 | #5 RW 句柄进度方向混报 | 🟢 | `server.rs` | 抽出 `transfer_kind_for(write)`，`open`/`read`/`close` 共用同一方向判定 | 修复 |
| 6 | #9 known_hosts 整文件覆写非原子 | 🟢 | `keys.rs` | 新增 `write_known_hosts_file()`：临时文件 + rename，失败回退原地写 | 优化 |
| 7 | #10 `formatEntry` 注释仍写「骨架阶段」 | 🟢 | `SftpClientView.tsx` | 注释改写为已落地的 `fileType` 契约 | 文档 |
| 8 | #4 `opendir` 全量预载 | 🟢 | `server.rs` | 补注释说明取舍与「为何不改成流式」 | 文档 |
| 9 | #7 `rename` 先删目标 | 🟢 | `server.rs` | 补注「rename(2) 自身失败时目标已删除」的固有窗口 | 文档 |
| 10 | #8 回环双事件源进度交错 | 🟢 | `docs/sftp-design.md` | 写入「已知限制」第 2 条（既有设计，不改） | 文档 |

---

## 二、逐项说明与修改理由

### 1. `read()` 分配封顶（报告 #1）

```rust
const MAX_READ_CHUNK: usize = 1024 * 1024;
fn read_chunk_len(requested: u32) -> usize {
    usize::try_from(requested).unwrap_or(usize::MAX).min(MAX_READ_CHUNK)
}
// read(): let mut buf = vec![0u8; read_chunk_len(len)];
```

**理由**：`len` 是 `SSH_FXP_READ` 里对端填的 u32，裸分配等于把内存配额交给客户端 ——
每个请求要 4 GiB 即可制造内存压力甚至 OOM。OpenSSH / FileZilla 只请求 32–64 KiB，
封顶对正常客户端零成本；SFTP v3 允许短读（EOF 另有 `SSH_FX_EOF`），超出的部分由对端
在下一个 offset 重新请求，语义完全不变。

**新增测试**：`read_chunk_len_caps_client_requested_sizes`（0 / 32K / 64K / 1M / u32::MAX
五档）、`oversized_read_request_still_returns_the_file`（请求 4 GiB 仍返回文件真实 4096
字节，验证「多读截断」而非报错或脏数据）。

### 2. `connect` 增加整段超时（报告 #2）

```rust
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
pub async fn connect(...) -> Result<Self, ConnectError> {
    let connecting = Self::connect_inner(cfg, app_data, trust_new_host);
    match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await { ... }
}
```

**理由 / 与报告建议的差异（重要）**：报告建议给 `connect` 加 `inactivity_timeout`。
实测确认 russh 0.63 的 `inactivity_timeout` 是**整条连接生命周期**的会话级定时器
（`client/mod.rs` 里 `inactivity_timer` 随 session task 常驻），而本应用把一个长生命周期
的 `SftpClient` 存在 `AppState.sftp_client` 里跨 UI 命令复用，且客户端没有配
`keepalive_interval`。给会话挂 10s 不活动超时，会让"用户连上后去挑本地文件"这种
**健康的空闲会话被悄悄断开**。

因此改成**只给 connect 序列加 deadline**（握手 → 密码认证 → channel → sftp 子系统），
精确覆盖报告描述的故障（黑洞/半死服务器永不响应握手），且对已建立的连接零影响。
15s 比探针的 10s 略宽，因为这一段比探针多了认证与子系统协商两步。

**新增测试**：`refused_connection_fails_fast_instead_of_timing_out` —— 先 bind 再 drop
拿到一个确定无人监听的端口，验证失败是**立刻**返回而不是被拖成超时（防止新加的
超时吞掉本来就该快速失败的错误）。

### 3. 符号链接限制（报告 #3）

`safe_path` 的文档注释里写明：约束是**词法层**的，`open`/`read`/`stat`/`opendir` 会跟随
root 内部指向外部的链接；与 OpenSSH `sftp-server` 一致，且服务端无建链入口
（`symlink` 回 `OpUnsupported`）。`docs/sftp-design.md` §七 新增「已知限制」第 1 条，
明确 **root_dir 是共享边界，不是沙箱**。

**不改代码的原因**：改成分辨 symlink 需要 canonicalize 后重新校验前缀，会改变
`realpath` 等现有语义并引入 TOCTOU；与 OpenSSH 行为对齐是设计选择。

### 4. 空用户名在启动阶段拒绝（报告 #6）

```rust
if cfg.username.is_empty() {
    return Err(Error::Config("SFTP 用户名不能为空：…".to_string()));
}
```

**理由**：两个认证回调的门都是 `!username.is_empty()`，留空等于开出一个谁也进不来的服务，
而用户只会看到"被拒绝"，无法区分是密码错还是压根没配用户名。判定刻意**只判空不 trim**，
与认证门的写法严格一致，避免出现"启动放行但登录必拒"的错位。放在 root_dir 校验之后、
生成主机密钥与 bind 之前，失败不留副作用。

UI 层 `ServersView` 的 `username: user ?? ""` 保持不变 —— 现在的表现是从"必然失败且无
提示"变成"启动时一条明确的中文错误"，正是报告要求的收敛方式。

**新增测试**：`empty_username_is_rejected_at_start`。

### 5. RW 句柄进度方向一致（报告 #5）

抽出 `transfer_kind_for(write)`，`open` / `read` / `close` 三处共用。原先 `read()` 恒定
发 `SftpDownload`，而 `open()`/`close()` 按 WRITE 位判定 —— 一个 `READ|WRITE` 句柄会在
Started 报 Upload、Progress 报 Download、Done 又报 Upload。现在全程同向。

**新增测试**：`rw_handle_reports_one_direction_throughout`（断言 Started/Progress/Done
三个事件的 kind 全为 `SftpUpload`）。纯读句柄的既有行为未动
（`read_only_gate_does_not_break_upload_or_download` 仍绿）。

### 6. known_hosts 原子写（报告 #9）

`upsert_known_host` 原本 `std::fs::write(&path, …)` 原地覆写整文件，进程在写中途崩溃会
丢掉**全部**已信任主机（恢复成本 = 逐台重新核对指纹）。改为写 `known_hosts.tmp` 再
`rename` 覆盖（unix `rename(2)` 与 Windows `MoveFileEx` 都是原子替换）；若 rename 失败
（如安全软件锁住临时文件）则清理临时文件并回退原地写 —— 非原子写仍好过丢记录。

**新增测试**：`known_hosts_upsert_leaves_no_temp_file`（内容正确 + `keys/` 下无 `.tmp` 残留）。

### 7–10. 注释与文档

- `SftpClientView.formatEntry`：注释从"fileType 取值以后端最终实现为准（骨架阶段）"
  改写为已落地的 `"file" | "dir" | "symlink" | "other"` 契约，并说明为何保留宽容匹配。
- `opendir`：写明整目录预载的内存尖峰，以及**为何不改成惰性读取**（会把 IO 错误从
  `opendir` 推迟到 `readdir`，改变客户端看到的错误时序）。
- `rename`：补充"rename(2) 自身失败时目标已删除"这一 posix-rename 固有窗口。
- 回环双事件源进度交错（#8）：写入设计文档「已知限制」第 2 条，标注为既有设计（FTP 模块
  同样如此），不改代码。

---

## 三、未改动项（明确为设计契约，非缺陷）

- `SftpClient` 只支持密码认证 —— `docs/sftp-design.md` Q5 的设计契约；要加客户端公钥
  登录属规格变更，需先确认。
- 加密后端 `aws-lc-rs` 未动。
- 未执行全局 `cargo fmt`（本仓库 `clippy --all-targets` / `fmt --check` 本来就是红的），
  只保证新增代码风格与既有代码一致。

## 四、验证结果

```
cargo test -p ftp-core
  ftp_core (lib)    61 passed; 0 failed      ← 新增 6 个
  ftp_bind_errors    3 passed
  ftp_data_channel   1 passed
  ftp_server_lifecycle 8 passed
  ftps_loopback      4 passed
  sftp_loopback      5 passed
  tftp_loopback      1 passed
```

新增的 6 个测试：`read_chunk_len_caps_client_requested_sizes`、
`oversized_read_request_still_returns_the_file`、`rw_handle_reports_one_direction_throughout`、
`empty_username_is_rejected_at_start`、`known_hosts_upsert_leaves_no_temp_file`、
`refused_connection_fails_fast_instead_of_timing_out`。

## 五、遗留事项

1. `4c78328` / `aee7ae4` 两个提交及本次改动**均未 push**（按用户惯例，需确认后推）。
2. 报告 🟢 中未落地的仅剩 #4（`opendir` 全量预载）—— 已补注释，改动需权衡错误时序，
   建议等真有超大目录需求时再做。
