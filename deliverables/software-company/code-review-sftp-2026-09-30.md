# 代码审查报告 — SFTP 服务端/客户端（v0.3.0 + aee7ae4）

- 日期：2026-09-30
- 范围：提交 `4c78328`（SFTP v0.3.0）与 `aee7ae4`（错误文案去重修复）触及的全部代码
- 基线：`main` @ `aee7ae4`，工作区干净
- 验证：`cargo test -p ftp-core` 全绿（2026-09-30 实测，含 5 个 SFTP loopback + 19 个 SFTP 单元测试）
- 规格参照：`docs/sftp-design.md`（Q1–Q12）；上一篇审查：`code-review-2026-09-29.md`

---

## 一、整体结构与核心功能

```
crates/ftp-core/src/sftp/
├── keys.rs    (362 行)  ed25519 主机密钥 load-or-generate；SHA256 指纹；known_hosts 读写（TOFU 状态）
├── server.rs  (1315 行) russh 0.63 SSH 传输 + russh-sftp 3 SFTP v3 协议；root 约束 / 只读闸门 / 进度事件
├── client.rs  (600 行)  russh 客户端 + 握手内 TOFU 裁定；list / upload / download
└── mod.rs     (16 行)   模块导出
app/src-tauri/src/lib.rs      11 个 Tauri 命令（服务端 4 + 客户端 7）
app/ui/src/api.ts             IPC 线格式契约（camelCase DTO）
app/ui/src/views/SftpClientView.tsx   客户端页面（TOFU 确认模态框）
app/ui/src/components/FingerprintBlock.tsx  指纹折叠区（FTPS/SFTP 共用）
crates/ftp-core/tests/sftp_loopback.rs      5 个回环集成测试
```

分层清晰：引擎层（GUI 无关）→ Tauri 命令层（DTO + 状态管理）→ React 视图层，
与既有 FTP/TFTP 模块的形状完全一致，接手者可以照 `ftp::server` 的既有模式对照阅读。

**核心功能**：SFTP 服务端（密码 + authorized_keys 公钥认证、只读模式、传输进度上报、
GNU `ls -l` 风格 longname）；SFTP 客户端（TOFU 主机密钥验证、目录列举、分块上传/下载）。

## 二、关键逻辑（值得接手者首先理解的部分）

1. **只读闸门覆盖 CREATE/TRUNCATE 走私**（`server.rs::open`）：
   `mutating = WRITE || APPEND || CREATE || TRUNCATE`。这是 v0.3.0 里最重要的安全判断——
   unix 上 `open(O_RDONLY|O_CREAT)` 真的能建出空文件，只查 WRITE 位会留下绕过路径。
   有专门的回归测试 `read_only_cannot_be_smuggled_past_via_create_or_truncate`。
2. **TOFU 握手内裁定**（`client.rs::TofuHandler`）：`check_server_key` 里做
   known/unknown/changed 裁定，通过 `Arc<Mutex<Option<HostKeyDecision>>>` 传回
   `connect` 的错误路径，还原成精确的 `ConnectError` 变体。changed 永远硬失败，
   只允许 `sftp_client_update_known_host` 显式覆盖——且该命令强制先探在线指纹，
   不接受凭空指定的指纹。
3. **错误文案去重**（`client.rs::describe_sftp_error / sftp_io_err`，`aee7ae4`）：
   russh-sftp 把裸 `StatusCode` 的 `error_message` 默认成状态码自身文本，客户端 Display
   再拼 `"{code}: {message}"` → 双重渲染。修复按错误**变体**匹配（不做字符串猜测），
   io 路径用 `get_ref()` 取回原始 `SftpError`（不能用 `into_inner()`，会移走 `io::Error`
   导致回退分支编译不过）。6 个单元测试钉死行为。
4. **root 词法约束**（`server.rs::safe_path`）：逐段拒绝 `..`、反斜杠、冒号；
   复合越界（`a/../..`）也被逐段拦下。有独立测试。
5. **`rename` 覆盖语义**：先删目标再改名（posix-rename 行为），目录目标走 `remove_dir`，
   非空目录目标按 `rename(2)` 原样报错——失败路径不丢数据，有测试验证。

## 三、发现的问题（按严重度）

### 🟡 重要（建议处理，不阻塞）

1. **服务端 `read()` 按客户端请求长度裸分配**（`server.rs:727`）
   `let mut buf = vec![0u8; len as usize];` — `len` 是客户端控制的 u32，
   恶意客户端对每个请求要 4 GiB 即可造成内存压力甚至 OOM。正规客户端（OpenSSH/FileZilla）
   只请求 32–64 KiB，但服务端不应信任对端。建议封顶（如 1 MiB，多读截断）或按
   `min(len, 某上限)` 分配。
2. **`SftpClient::connect` 无握手超时**（`client.rs:224`）
   `fetch_host_fingerprint` 设了 `inactivity_timeout: 10s`，`connect` 用的是
   `Config::default()`——黑洞/半死服务器可以让握手无限挂起，UI 的 busy 状态卡死。
   建议对齐探针，给 `connect` 也加 `inactivity_timeout`（注意别太短，慢网络下
   大文件传输阶段不走这条配置，风险低）。
3. **root 约束不解析符号链接**（`server.rs::safe_path` + `open/read/stat`）
   `safe_path` 只做词法约束；`open`/`read`/`stat`/`opendir` 会跟随 root 内部的
   符号链接，链接指向 root 外即可逃逸。当前服务端**没有**建链入口（`symlink` 未实现，
   回 `OpUnsupported`），所以实际威胁模型 = 共享目录的本地属主自己放的链接——
   与 OpenSSH sftp-server 行为一致。建议在 `docs/sftp-design.md` 里明确写成
   已知限制，避免接手者误以为有路径沙箱。

### 🟢 次要（知情即可，可选处理）

4. `opendir` 一次性把整目录预载入内存（`SftpHandle::Dir`）；`readdir` 的 256 条分批
   只影响发包不影响内存。超大目录会有内存尖峰。
5. RW 句柄的进度方向混报：`open()` 的方向由 WRITE 位定（可能是 Upload），
   但 `read()` 永远发 `SftpDownload` 事件。纯展示问题，SFTP 客户端很少开 RW 句柄。
6. 空用户名的服务端配置会让**所有**认证必然失败（`!username.is_empty()` 门），
   且没有任何配置错误提示，用户只看到"拒绝"。UI 层若未强制非空用户名，建议在
   `start_sftp_server` 拒绝空用户名。
7. `rename` 先删目标再改名：若 `rename(2)` 本身失败（磁盘满等），目标已删除。
   posix-rename 语义的已知取舍，注释已写明。
8. 回环场景进度条双事件源交错：GUI 客户端连本应用自己的 SFTP 服务端时，客户端与
   服务端的进度事件同 kind、同 file id（都是远程路径）汇入 App.tsx 的全局单槽位，
   进度数字会两边跳。FTP 模块存在同样行为，属既有设计，不算回归。
9. `known_hosts` 整文件覆写非原子，进程崩溃可能丢失记录。恢复成本 = 重新确认指纹，
   可接受。
10. `SftpClientView.formatEntry` 的注释还写着"骨架阶段"，但 `fileType` 契约
    （`"file"|"dir"|"symlink"|"other"`）已落地，注释过时。

### 🎉 做得好的

- read_only 门的 CREATE/TRUNCATE 走私分析与封堵，以及"加固不能误伤正常读写"的
  双向回归测试（`read_only_gate_does_not_break_upload_or_download`）。
- 错误去重按类型而非字符串，io 路径 `get_ref()` 的选型注释把"为什么不能
  `into_inner()`"写清楚了——下一个改这里的人不会踩回去。
- `safe_path` 测试覆盖了 `a/../..`、`//a//b`、空段、反斜杠/冒号等复合形态。
- `longname_timestamp` 纯日期算术（无 chrono 依赖）带闰日/世纪闰/跨年边界测试。
- TOFU 的 changed 硬失败 + 显式覆盖命令，且覆盖命令强制"先探在线指纹"，堵掉了
  凭空写记录的口子。
- `Cargo.toml` 里 russh 加密后端选 aws-lc-rs 而非 ring 的决策原因写成了注释，
  避免后人"优化"回 ring 重新踩坑。

## 四、结论

💬 **Comment（通过，附建议）**：功能正确、测试扎实、文档与注释质量高。
三个 🟡 均为加固项而非正确性缺陷——建议在下一个版本顺手处理 #1（分配封顶，
改动约 3 行）和 #2（一行配置），#3 写进规格文档即可。
