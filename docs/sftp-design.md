# SFTP 功能设计（目标版本 0.3.0）

> 2026-09-29 grilling 会话定案。本文档是前后端实现的唯一契约源。
> 底线原则：**镜像现有 FTP/FTPS 模式**，结构与命名与现有代码保持一致。

## 一、决策记录

| # | 决策点 | 结论 |
|---|--------|------|
| Q1 | 底层库 | `russh 0.63`（`default-features = false, features = ["aws-lc-rs"]`）+ `russh-sftp 3.x`。实现时发现：russh 0.63 源码 compile_error 强制 ring 或 aws-lc-rs 二选一，无法纯 Rust；选 aws-lc-rs 与 rustls 现有后端一致（构建链已验证可行） |
| Q2 | 服务端认证 | 单用户 + 密码 + 可选公钥 |
| Q9 | 公钥录入 | 文件选择器（可多选 `.pub` / `authorized_keys`），载入成列表，可逐条移除 |
| Q10 | 公钥格式 | 仅 OpenSSH 格式公钥行（`ssh-ed25519` / `ssh-rsa` / `ecdsa-*`） |
| Q11 | 认证语义 | 密码 或 公钥，任一通过即放行（未配置公钥时即纯密码） |
| Q3 | 指纹 UI | 折叠区默认收起（详见前端契约 §4.3），FTPS 证书与 SFTP 主机密钥共用该组件 |
| Q12 | FTPS 同步改造 | 现有 FTPS 常显指纹行改造成折叠组件；启动成功消息不再携带指纹长串（tracing 日志保留） |
| Q4 | 客户端功能 | 对齐 FTP 客户端四件套：连接 / 列表 / 上传 / 下载 |
| Q5 | 客户端认证 | 仅用户名 + 密码 |
| Q6 | 主机密钥校验 | TOFU（详见 §3.4） |
| Q7 | 默认端口 | 2222（避免与系统 sshd 冲突，UI 可改） |
| Q8 | UI 结构 | 镜像现有结构：ServersView 加 SFTP 卡片 + 新增 SftpClientView + 侧栏加一项 |

其他按项目惯例直接定：主机密钥存 `app-data/keys/`（ed25519）；状态推送复用 `watch` 通道（`sftp-server-state` 事件）；进度事件复用 `ProgressTx`，`TransferKind` 增加 `SftpUpload` / `SftpDownload`；回环测试 `tests/sftp_loopback.rs`。

## 二、后端架构（crates/ftp-core/src/sftp/）

镜像 `ftp/` 模块结构：

```
sftp/
├── mod.rs      公共导出；SftpClient、SftpEntry
├── server.rs   SftpServerConfig / SftpServerHandle / start_sftp_server
│               （russh Server + russh-sftp SftpSession 服务端）
├── client.rs   SftpClient（russh client + russh-sftp client session）；TOFU 逻辑
└── keys.rs     主机密钥 load-or-generate（ed25519）；指纹；authorized_keys 解析；
                known_hosts 读写
```

`lib.rs`：`pub mod sftp;` 并 re-export 公共类型。

### 2.1 keys.rs（完整实现，本阶段核心）

- `HostKeyInfo { algorithm: String, fingerprint: String }`，指纹用 OpenSSH 风格 `SHA256:<base64>`（与 `ssh-keygen -lf` 一致，便于跨工具核对）
- `host_key_paths(app_data)`：目录 `keys/`，私钥 `sftp_host_ed25519`、公钥 `sftp_host_ed25519.pub`
- `load_or_generate_host_key(app_data) -> Result<(PrivateKey, HostKeyInfo)>`：镜像 `tls.rs` 的 load-or-generate 模式；生成后私钥文件权限收紧
- `known_hosts` 文件：`app-data/keys/known_hosts`，自用简单格式，每行 `host:port SHA256:<base64>`
- `check_known_host(host, port) -> HostKeyStatus`、`record_known_host`、`update_known_host`、`clear_known_hosts`

### 2.2 server.rs

```rust
pub struct SftpServerConfig {
    pub bind_addr: IpAddr,
    pub port: u16,                      // UI 默认 2222
    pub username: String,
    pub password: String,
    pub authorized_keys: Vec<String>,   // OpenSSH 公钥行内容
    pub root_dir: PathBuf,              // 初始 = 运行目录，UI 可改
    pub read_only: bool,
}
pub struct SftpServerHandle { /* stop_tx, watch_rx, join handle —— 镜像 FtpServerHandle */ }
pub async fn start_sftp_server(cfg, app_data, shutdown) -> Result<SftpServerHandle>
```

- 认证回调：`validate_password`（单用户比对）+ `validate_public_key`（authorized_keys 解析比对）；任一通过即放行
- 状态事件 `SftpServerStatus { running, port, addr, sessions, error, host_key: Option<HostKeyInfo> }` 经 watch 通道转发（镜像 `ftp-server-state`）
- SFTP v3 的 POSIX 权限语义在 Windows 上返回合理默认值（permissions/uid/gid），不做复杂映射
- russh-sftp 官方服务端示例不完整，参考 russh 仓库自带 `sftp_server` 示例；把握不足处留 `// TODO` 注释，但结构必须完整、必须编译通过

### 2.3 client.rs

```rust
pub struct SftpClientConfig { pub host: String, pub port: u16, pub username: String, pub password: String }
impl SftpClient {
    pub async fn connect(cfg, trust_new_host: bool) -> Result<Self, ConnectError>;
    pub async fn list(&self, path) -> Result<Vec<SftpEntry>>;
    pub async fn upload_file(&self, local, remote, progress) -> Result<()>;
    pub async fn download_file(&self, remote, local, progress) -> Result<()>;
    pub async fn disconnect(self);
}
```

- `SftpEntry { name, file_type, size, mtime }` 与 FTP 的 `FtpEntry` 字段对齐，便于前端复用行渲染
- 进度回调复用 `ProgressTx`（字节数方式与 FTP 客户端一致）

### 2.4 TOFU 语义（Q6 定案）

- 连接前检查：`sftp_client_check_host_key(host, port) -> { status: "known"|"unknown"|"changed", fingerprint: Option<String> }`
  - `known`：直接连接，自动比对，一致即静默通过
  - `unknown`（首连）：UI 弹确认框显示指纹 → 用户确认后带 `trust_new_host = true` 重连 → 记入 known_hosts
  - `changed`（硬失败）：UI 明确警告"不是上次那台服务器" → 用户可在确认后调 `sftp_client_update_known_host(host, port)` 覆盖
- `connect` 内部仍要校验：unknown 且未 trust → 拒绝；changed → 一律拒绝（更新只能走显式命令）
- "清除已信任主机"入口调 `sftp_client_clear_known_hosts()`

## 三、Tauri 命令层契约（app/src-tauri/src/lib.rs）

命令名（驼峰映射由 Tauri 处理，参数命名与现有命令风格一致）：

| 命令 | 签名要点 |
|------|----------|
| `start_sftp_server` | `(state, opts: SftpServerOptions) -> Result<SftpServerInfo>` |
| `stop_sftp_server` | `() -> Result<()>` |
| `sftp_server_regenerate_host_key` | `() -> Result<HostKeyInfo>`（镜像现有 FTPS 重新生成证书命令） |
| `sftp_client_check_host_key` | `(host, port) -> Result<HostKeyStatus>` |
| `sftp_client_connect` | `(host, port, username, password, trust_new_host) -> Result<()>` |
| `sftp_client_disconnect` | `() -> Result<()>` |
| `sftp_client_list` | `(path) -> Result<Vec<SftpEntry>>` |
| `sftp_client_upload` | `(local_path, remote_path) -> Result<()>` |
| `sftp_client_download` | `(remote_path, local_path) -> Result<()>` |
| `sftp_client_update_known_host` | `(host, port) -> Result<()>` |
| `sftp_client_clear_known_hosts` | `() -> Result<()>` |

DTO（`SftpServerOptions` 等）字段命名与前端 `types.ts` 一一对应（蛇形/驼峰按现有惯例）：
- `SftpServerOptions { bind_addr, port, username, password, authorized_keys, root_dir, read_only }`
- `SftpServerInfo { port, addr, host_key }`
- `HostKeyStatus { status, fingerprint }`

AppState 扩展（镜像现有字段）：
- `sftp_server: AsyncMutex<Option<SftpServerState>>`（handle + watch 转发任务）
- `sftp_client: AsyncMutex<Option<SftpClient>>`

事件：`sftp-server-state`（镜像 `ftp-server-state` 的转发任务模式）；`progress` 事件复用，`TransferKind` 增加 `SftpUpload` / `SftpDownload`（前端 label 映射同步补）。

启动成功消息：**不含指纹**（如"SFTP 服务器已启动 ws://…:2222"式简讯），指纹只在折叠区与 tracing 日志出现（Q12）。

## 四、前端契约（app/ui/src/）

### 4.1 导航

- `Sidebar`：在"TFTP 客户端"之后、"运行日志"之前加 `{ id: 'sftp-client', label: 'SFTP 客户端' }`
- `App.tsx`：对应路由 case

### 4.2 ServersView + ServerCard

- 第三张 SFTP 卡片：端口默认 2222、单用户名+密码、授权公钥（文件多选载入 → 列表展示可移除）、只读开关（与 FTP 卡片一致）、根目录选择
- 保持现有 `grid-2` 布局，第三张自然换行
- `ServerCard` 以现有 props 模式扩展（如 `withSftp`），不破坏 FTP/TFTP 卡片

### 4.3 折叠指纹组件（Q3/Q12 核心）

- 新组件（如 `FingerprintBlock`）：默认收起，展开显示指纹（等宽字体、`word-break: break-all`）+ "重新生成"按钮
- 折叠行文案："证书详情"（FTPS）/ "主机密钥详情"（SFTP），右侧 chevron 旋转指示
- 展开动画用 `grid-template-rows: 0fr → 1fr` 过渡（尊重 `prefers-reduced-motion`）
- **不变量**：折叠区永远随卡片渲染，不随开关消失（对端核对身份随时可查）
- **FTPS 同步改造**：现有常显指纹 hint-line 换成该组件；`ServersView` 里相关注释随之更新

### 4.4 SftpClientView（镜像 FtpClientView）

- 连接表单：host / port（默认 22 客户端连远端用 22，输入框占位提示）/ username / password
- 首连流程：connect 前 `check_host_key` → `unknown`/`changed` 弹确认框（显示指纹，changed 用警告文案）→ 确认后 `trust_new_host=true` 重连或先 `update_known_host` → 再连
- 远端列表 / 上传 / 下载：交互与 FtpClientView 一致
- 客户端设置区加"清除已信任主机"按钮（`sftp_client_clear_known_hosts`），带确认提示

### 4.5 api.ts / types.ts

- 按第三节命令表补 `invoke` 封装与 TS 类型
- `TransferKind` 前端 label 映射补 `SftpUpload` / `SftpDownload`

### 4.6 项目惯例（重要，历史教训）

- **CSS 选择器对 `input` 必须加 `:not([type="radio"], [type="checkbox"])`** 排除，避免复选框/单选框被错误套用文本框样式（v0.2.6 修过此 bug）

## 五、测试与版本

- `crates/ftp-core/tests/sftp_loopback.rs`：镜像 `ftps_loopback.rs`（服务端起 → 客户端 TOFU 首连信任 → 列表 → 上传下载断言 → 指纹变更拒绝）。框架阶段先建 `#[ignore]` 骨架占位
- 版本 0.2.6 → 0.3.0：`app/src-tauri/tauri.conf.json`、`app/src-tauri/Cargo.toml`、`app/ui/package.json`（若有 version 字段）三处同步

## 六、实现顺序建议

1. `keys.rs`（主机密钥 + 指纹 + known_hosts）→ 可独立验证
2. `server.rs` + 命令 + 回环测试
3. `client.rs` + TOFU + 命令 + 回环测试
4. 前端（折叠组件改造 → SFTP 卡片 → SftpClientView）
5. 版本号 + `cargo check` / ui 构建 + 本地手工验证（FTPS 折叠改造要回归验证开关布局）

## 七、风险与注意

- russh 0.63 MSRV 1.89+（本地 1.98.1 ✓）
- russh 0.63 加密后端强制要求 ring 或 aws-lc-rs 二选一（compile_error），已选 aws-lc-rs（rustls 现用后端，本项目构建链已验证可编译）
- **杀软误报风险（已发生）**：cargo target 目录（`C:\Users\22534\.workbuddy\build\ftp-toolbox-target`）里新生成的 build-script-build.exe 可能被杀软误杀导致 `os error 5`；该目录需加入杀毒软件白名单
- russh-sftp 完整服务端示例缺失 → 参考 russh 仓库 `sftp_server` / `sftp_client` 示例
- SFTP v3 POSIX 权限语义在 Windows 服务端只需返回合理默认值
- 用户惯例：**构建完成后先本地测试确认，再推送**；未经用户确认不要 push

### 已知限制（2026-09-30 代码审查确认 —— 均为知情取舍，不是缺陷）

1. **root 约束不解析符号链接**（`server.rs::safe_path`）。路径约束是**词法层**
   的：逐段拒绝 `..`、反斜杠与冒号，但不解析符号链接 —— root **内部**一个指向
   外部的链接会被 `open`/`read`/`stat`/`opendir` 正常跟随。
   与 OpenSSH `sftp-server` 行为一致；且服务端没有建链入口（`symlink` 回
   `OpUnsupported`），所以实际威胁模型是"共享目录的本地属主自己放了链接"。
   **`root_dir` 是共享边界，不是沙箱** —— 不要把不信任的目录挂成 root。
2. **回环场景进度条有两个事件源**。GUI 客户端连本应用自己的 SFTP 服务端时，
   客户端与服务端各上报一路进度，kind 相同、file id 都是远程路径，汇入
   `App.tsx` 的全局单槽位后数字会两边跳。FTP 模块存在同样行为，属既有设计。
3. `opendir` 一次性把整个目录读入内存（`readdir` 的 256 条分批只影响单次回包
   大小），超大目录会有一次内存尖峰。流式化会把 IO 错误从 `opendir` 推迟到
   `readdir`，改变客户端看到的错误时序，故暂不改动。
4. `rename` 是"先删目标再改名"：若紧接着的 `rename(2)` 自身失败（磁盘满、
   权限变化等），目标已被删除。posix-rename 语义的固有窗口。
5. 客户端只支持密码认证是 Q5 的设计契约，不是缺口；要加客户端公钥登录属规格
   变更。

### 资源上限与超时（2026-09-30 审查加固项落地）

- `SSH_FXP_READ` 的 `len` 由对端控制，服务端按 `MAX_READ_CHUNK = 1 MiB` 封顶，
  而不是按请求量裸分配；超出部分由对端在下一个 offset 重新请求（SFTP v3 允许
  短读，EOF 另有 `SSH_FX_EOF`）。
- `SftpClient::connect` 受 `CONNECT_TIMEOUT = 15s` 约束（握手 → 认证 → 子系统）。
  刻意**不用** russh 的 `inactivity_timeout`：那是整条连接的会话级定时器，而本
  应用把一个长生命周期的 `SftpClient` 存在 `AppState` 里跨命令复用，空闲的健康
  会话不该被它悄悄断开。指纹探针是短连接，`inactivity_timeout` 正合适。
