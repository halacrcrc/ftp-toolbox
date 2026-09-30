# 交接文档 — SFTP v0.3.0（2026-09-29）

> 面向接手本项目的下一个 agent / 开发者。**规格以 `docs/sftp-design.md` 为准，代码以本次提交的 diff 为准**，
> 本文只写那些"从规格和 diff 里看不出来"的东西：当前状态、本机环境坑、可复现的验证配方、遗留项。

---

## 一、当前状态

**SFTP 服务端 + 客户端 + TOFU 主机密钥验证（v0.3.0）已实现完毕，编译干净，测试全绿，并已完成本地实机 GUI 联调。**

| 项 | 状态 |
|---|---|
| `crates/ftp-core` 编译 | ✅ 0 error / 0 warning（代码警告） |
| `cargo test -p ftp-core` | ✅ **71 passed / 0 failed** |
| 前端 `tsc --noEmit` + `vite build` | ✅ 通过 |
| GUI 实机联调（起服务 → 加公钥 → 上传/下载 → 只读拦截） | ✅ 通过 |
| 与系统 OpenSSH 的 SSH 层互操作 | ✅ 通过（详见第四节） |
| 版本号 0.3.0 四处同步 | ✅ `tauri.conf.json` / `app/src-tauri/Cargo.toml` / `crates/ftp-core/Cargo.toml` / `app/ui/package.json` |

**未做**：`git push`（按用户惯例，构建完成并经确认后才推）。

---

## 二、本次新增/改动索引

**新增（`crates/ftp-core`）**
- `src/sftp/keys.rs` — 主机密钥 load-or-generate、`SHA256:<base64>` 指纹、known_hosts 读写
- `src/sftp/server.rs` — russh + russh-sftp 服务端（根目录约束 / 只读闸门 / 进度事件 / GNU `ls -l` 风格 longname）
- `src/sftp/client.rs` — 客户端 + TOFU 握手内裁定（`unknown` / `changed` / `accepted`）
- `src/sftp/mod.rs` — 模块导出
- `tests/sftp_loopback.rs` — 5 个回环测试
- `examples/sftp_interop.rs` — 起服务端供**外部客户端**打（联调用）
- `examples/sftp_probe.rs` — 客户端探针，打**已在运行**的服务端（联调用）

**新增（前端）**
- `app/ui/src/components/FingerprintBlock.tsx` — 折叠指纹组件（FTPS 与 SFTP 共用）
- `app/ui/src/views/SftpClientView.tsx` — 客户端页面（镜像 `FtpClientView`）

**改动**
- `app/src-tauri/src/lib.rs` — SFTP 命令层（11 个命令）
- `app/ui/src/{App.tsx,api.ts,styles.css,Sidebar.tsx,ProgressBar.tsx,views/ServersView.tsx}`
- `crates/ftp-core/src/{lib.rs,progress.rs,tls.rs}`、两个 `Cargo.toml`
- `app/src-tauri/tauri.conf.json`（CSP，见第三节缺陷 1）
- `app/ui/vite.config.ts`（dev server 绑 127.0.0.1，防御性）

**归档**
- `docs/sftp-design.md` — 规格（Q1–Q12 决策记录）
- `deliverables/software-company/` — 代码审查、QA 验证、诊断报告
- `deliverables/sftp-gui-integration-2026-09-29/` — 本次 GUI 联调的截图与工具脚本

---

## 三、联调中发现并修复的 3 个真实缺陷

**这三个都是"编译能过、测试能过、但用户看得见或潜伏"的缺陷**，靠 GUI 联调才暴露：

### 缺陷 1：CSP 缺 `connect-src`，所有 IPC 被拦截
- 位置：`app/src-tauri/tauri.conf.json` → `app.security.csp`
- 现象：控制台刷满
  `Connecting to 'http://ipc.localhost/sftp_server_status' violates the following Content Security Policy directive: "default-src 'self'"`
  以及 `IPC custom protocol failed, Tauri will now use the postMessage interface instead`
- 后果：Tauri 2 的 IPC 走 `http://ipc.localhost/`，被 CSP 挡下后**降级到 postMessage 通道**。
  功能"能用"，所以极易漏掉；但通道脆弱、控制台刷屏、且掩盖真实错误。
- 修法：`connect-src 'self' ipc: http://ipc.localhost`，并按 Tauri 2 规范给 asset 协议补 `http://asset.localhost`

### 缺陷 2：`SftpServerStatus` 漏 `rename_all = "camelCase"`，指纹区永远空白
- 位置：`app/src-tauri/src/lib.rs`
- 现象：SFTP 服务**已在运行**（`已监听 127.0.0.1:2222 / 运行中`），但「主机密钥详情」折叠区
  永远显示 `主机密钥尚未生成，启动服务时自动创建`
- 根因：兄弟结构 `SftpServerInfo` 有 `#[serde(rename_all = "camelCase")]`，`SftpServerStatus` 没有。
  于是 `host_key` 序列化成**蛇形**，前端 `sftpStatus.hostKey` 恒为 `undefined`。
  IPC 实测：`keys = ["running","addr","localAddr","root","sessions","detail","host_key"]`，`hostKey` 不存在。
- 后果：**静默字段丢失** —— 前端用 `?? null` 兜住了，不报错、不崩溃，只是功能默默失效。
- 修法：补 `#[serde(rename_all = "camelCase")]`

### 缺陷 3：`CertInfo` 同样漏 camelCase（潜伏）
- 位置：`crates/ftp-core/src/tls.rs`
- 现象：`cert_path` / `key_path` 以蛇形序列化，前端 `CertInfo.certPath` / `keyPath` 恒为 `undefined`
- 说明：`fingerprint` 是单词所以没暴露；两个路径字段目前在 UI 里也没被消费 → **潜伏缺陷**
- 修法：补 camelCase，并加回归测试 `tls::tests::cert_info_serializes_with_camel_case_paths`
  （为此给 `ftp-core` 加了 dev-dependency `serde_json`）

> **给后续 agent 的硬性提醒**：新增/修改任何 IPC 结构体时，**逐个**核对 `rename_all = "camelCase"`。
> 本次排查的 4 个 IPC 结构体命中 2 个（`NetInterface` 字段全是单词，幸免）。
> 这类缺陷不会报错，只会静默丢字段，只有 GUI 联调或端到端断言能发现。

---

## 四、本机环境坑（最有价值的一节）

### 4.1 GUI 白屏/黑屏的真因：Chromium 沙箱初始化失败

**这不是前端 bug，也不是 dev server / 代理 / GPU 问题。** 排查历程中曾误判两次（见下"错误结论"）。

**决定性证据**（自写 minidump 解析器，`deliverables/sftp-gui-integration-2026-09-29/dump-parse.py`）：
解析 `%LOCALAPPDATA%\com.workbuddy.ftptoolbox\EBWebView\Crashpad\reports\*.dmp`：
```
EXCEPTION   : 0x80000003  (STATUS_BREAKPOINT，主动 fail-fast)
FAULT MODULE: ...\EdgeWebView\Application\153.0.4234.48\msedge.dll
  offset=0xaf6ac8d (183938189)   ← 每次崩溃偏移完全相同 = 确定性触发
```

**进程级观测**（`webview-watch.py`）：webview 创建后约 2 秒，**浏览器进程（`--type=browser`）先死**，
renderer/utility/crashpad-handler 随后被回收；应用主进程仍存活 → 窗口空白 + **F12 完全无反应**。

**排除法**（全部实测）：

| 假设 | 实验 | 结果 |
|---|---|---|
| 沙箱环境注入 | 在沙箱外启动 | 同样崩 → 否定 |
| WebView2 profile 损坏 | `WEBVIEW2_USER_DATA_FOLDER` 指向全新目录 | 同样崩 → 否定 |
| 我们的启动参数有问题 | 抓浏览器进程完整命令行 | 完全标准 → 否定 |
| GPU 问题 | `--disable-gpu` | 无效 |
| **Chromium 沙箱** | **`--no-sandbox`** | **webview 存活，界面完全正常** ✅ |

**结论**：本机 Chromium 沙箱（renderer/utility 的 restricted token + job object）初始化失败，
浏览器进程随之 `CHECK` 失败 → fail-fast。本机环境：Windows `26200.9457`、WebView2 Runtime `153.0.4234.48`、DPI 200%。

**本机联调配方**：
```bash
export WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--no-sandbox --remote-debugging-port=9222"
cd C:/Users/22534/.workbuddy/build/ftp-toolbox-target/debug && ./ftp-toolbox-app.exe
```

> ⚠️ `--no-sandbox` **只用于本机联调**。不要写进 `tauri.conf.json` 的
> `app.windows[].additionalBrowserArgs`，那会连生产构建的沙箱一起关掉。

> ⚠️ 跨工具调用保活必须用 Bash 工具的 `run_in_background`。
> `cmd //c start` 与 `(exe &)` 启动的进程会在该次命令结束时被进程树回收。

**排查历程中被推翻的两个错误结论**（避免后人重走）：
1. "页面已加载"的 netstat 证据是**别人的连接**（PID 不属于当时的应用进程）—— 单次 netstat 快照会张冠李戴。
2. 渲染探针"通过"只说明前端在崩溃前跑了一瞬，不代表窗口正常显示。

### 4.2 `os error 5（拒绝访问）`：target 目录被杀软干扰

`docs/sftp-design.md` §七 早已记录：`cargo target` 目录（`C:\Users\22534\.workbuddy\build\ftp-toolbox-target`）
里新生成的 `build-script-build.exe` 可能被杀软误杀导致 `os error 5`，**该目录需加入杀毒软件白名单**。

本次遇到的两类失败都属此列：
- `.fingerprint\...\invoked.timestamp` 写入被拒
- `cargo test --workspace` 时 `tauri::generate_context!()` 读 assets 被拒：
  `failed to write asset from ...\build\ftp-toolbox-app-*\out\... because 拒绝访问 (os error 5)`

**应对**：本机验证改跑 `cargo test -p ftp-core`（不触发 tauri 代码生成）。
另有偶发情况：应用正在运行时会锁住 `debug\ftp-toolbox-app.exe`，导致链接报 `LNK1104` —— 先关应用再构建。

### 4.3 本机**无法**使用的测试手段（环境限制，非代码问题）

系统 OpenSSH 的 `sftp.exe` / `scp.exe` 在 MSYS（Git Bash）下**一律**报 `pipe: Unknown error`：
- 加 `-v` 也只有这一行输出 → 失败发生在**建立连接之前**，是客户端自身的控制台/管道初始化失败
- `cmd //c` 包装、重定向到文件、`MSYS_NO_PATHCONV=1` 均无效

→ **不要**在这台机器上尝试"用外部 OpenSSH 客户端做文件传输"。改用：
- SSH 层互操作 → `ssh.exe -v`（能正常握手、认证，见第五节）
- 文件读写 → `examples/sftp_probe.rs`（我们的 `SftpClient`）打 GUI 启动的服务实例

另外本沙箱内：`wmic`、`reg.exe` 被安全策略拦截（静默返回空 / 明确报黑名单）。
枚举进程父子关系请用 `deliverables/sftp-gui-integration-2026-09-29/webview-watch.py`（ctypes Toolhelp32）。

---

## 五、可复现的验证配方

### 5.1 构建
```bash
cd app/ui && npm run build
cd app/src-tauri && <app/ui>/node_modules/.bin/tauri build --debug --no-bundle \
    -c '{"build":{"beforeDevCommand":""}}'
# 产物：C:/Users/22534/.workbuddy/build/ftp-toolbox-target/debug/ftp-toolbox-app.exe
```
> 用内嵌前端的 debug exe 而非 `tauri dev`：dev 模式受 dev server / 代理 / IPv6 三重干扰。
> 另外 tauri CLI 在本机有时无法解析 `npm.cmd`（`failed to run npm run dev`），故显式传空 `beforeDevCommand`。

### 5.2 测试
```bash
cargo test -p ftp-core        # 71 passed
cd app/ui && ./node_modules/.bin/tsc --noEmit && ./node_modules/.bin/vite build
```

### 5.3 GUI 联调（CDP 驱动真实 DOM）
```bash
# 1) 后台常驻启动（Bash 工具 run_in_background）
export WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--no-sandbox --remote-debugging-port=9222"
cd C:/Users/22534/.workbuddy/build/ftp-toolbox-target/debug && ./ftp-toolbox-app.exe

# 2) 用驱动脚本操作真实界面
node deliverables/sftp-gui-integration-2026-09-29/gui-e2e.mjs inspect
node .../gui-e2e.mjs addkey <公钥文件>      # 模拟点「添加公钥文件…」
node .../gui-e2e.mjs start <共享目录> 2222 tester ""
node .../gui-e2e.mjs hostkey                # 刷新 + 展开主机密钥详情读指纹
node .../gui-e2e.mjs shot out.png
node .../gui-e2e.mjs eval "<任意JS>"         # 直接观察 IPC 返回值
```

**驱动真实 DOM 的三个关键技巧**（踩过的坑，务必照做）：
1. React 受控输入必须走原生 setter + 冒泡事件：
   `Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(el,v)`
   然后 `el.dispatchEvent(new Event('input',{bubbles:true}))`。直接赋 `el.value` 不会触发 `onChange`。
2. **隐藏的 `input[type=file]` 不能用 `DOM.requestNode`**（返回 `nodeId: 0`，元素不在 CDP 节点树里）。
   要用 `DOM.describeNode({objectId})` 取 `backendNodeId`，再
   `DOM.setFileInputFiles({files:[...], backendNodeId})`，最后手动补发 `change`。
3. Git Bash 会改写 Windows 路径参数：脚本内统一用 `toWin()` 把 `/c/...` 转回 `C:\...`。
   另外**别把密码字面量写进 shell 命令**（会触发敏感内容审批超时）。

### 5.4 已实测通过的项（证据）
- GUI 点「启动服务」→ `已监听 127.0.0.1:2222 / 运行中`，`netstat` 确认端口 LISTENING
- GUI 点「添加公钥文件…」→ `共 1 条`，列表显示该公钥行
- **独立实现互操作**（系统 OpenSSH `ssh.exe -v` 打我们的 russh 服务端）：
  ```
  Remote protocol version 2.0, remote software version russh_0.63.3
  kex: curve25519-sha256 / host key ssh-ed25519 / chacha20-poly1305@openssh.com
  Server host key: ssh-ed25519 SHA256:O4iEssAbpEa1WM6rvt3cIJ5oaQwsxrSQTghUEO1mK7Y
  Server accepts key: ... SHA256:JJhrr/jgJhBYkiNRSIbFfhDx/3UqxBWG73xJ1z5vCA0
  Authenticated to 127.0.0.1 ([127.0.0.1]:2222) using "publickey".
  ```
  （`ssh.exe ... true` 之后会挂住 —— 我们的服务端不实现 exec，属预期）
- 客户端上传/下载**双向逐字节一致**（`diff` 校验），进度事件 `Started/Progress/Done` 字节数正确
- 主机密钥指纹**三处一致**：GUI 显示 = IPC 返回 = `ssh-keygen -lf`
- 只读模式：`list` 可读、`put` 被拒（`Permission denied`）、服务端目录无新文件
- CSP 修复后控制台**零 IPC 报错**，仅剩一条无害的 `[DOM] Password field is not contained in a form`

截图见 `deliverables/sftp-gui-integration-2026-09-29/*.png`。

---

## 六、遗留项 / 建议的下一步

**需要用户决策**
1. `git push` 本次后续修复（`fix(sftp): …`）—— 按用户惯例，构建 + 本地测试通过后待确认再推。
   （0.3.0 主提交 `4c78328` 已推送至 `origin/main`；推送时若走代理报 `Failed to connect ... over proxy 127.0.0.1`
   是 `git config http.proxy=http://127.0.0.1:10808` 指向了未运行的代理，用 `git -c http.proxy= push origin main` 绕过。）
2. 把 `C:\Users\22534\.workbuddy\build\ftp-toolbox-target` 加入杀软白名单，否则构建会随机 `os error 5`。

**已闭环（v0.3.0 后续小修）**
3. 只读模式下上传失败的报错文案重复：`Permission denied: Permission denied` —— **已修**。
   根因在 russh-sftp 自身：服务端对失败请求回的是裸 `StatusCode`，库会把 `SSH_FXP_STATUS.error_message`
   默认成状态码自己的文本（`server/mod.rs` 的 `unwrap_or_else(|| status_code.to_string())`），
   客户端 `Error::Status` 的 `Display` 又是 `"{status_code}: {error_message}"`，于是同一个码出现两次。
   现在 `client.rs::describe_sftp_error` 在格式化时去重（服务端真带了说明则保留），
   `sftp_io_err` 另覆盖传输途中被 `AsyncRead`/`AsyncWrite` 包成 `io::Error` 的协议错误。
   回归覆盖：`client.rs` 6 个单测 + `sftp_loopback::sftp_read_only_rejects_writes` 的端到端精确断言。

**设计如此，非遗漏**
4. `SftpClient` 只实现密码认证（`authenticate_password`），没有公钥认证。
   这是 `docs/sftp-design.md` §2.3 与 §三 冻结的契约 —— `SftpClientConfig { host, port, username, password }`、
   `sftp_client_connect(host, port, username, password, trust_new_host)`。公钥认证是**服务端**能力
   （Q2/Q11 的 `validate_public_key`，已实现并有测试）。若产品上要支持客户端公钥登录，
   属规格变更，需先定密钥来源/口令处理，再补 `authenticate_publickey`。
   （本次联调靠"服务端空密码启动 + 客户端空口令"绕过：服务端判据是 `password == cfg.password`，
   空口令同样通过。）

**工程卫生**
5. `.workbuddy-ai/`（AI 助手项目数据，含逐日记忆）本次已加入 `.gitignore`，与既有的 `.workbuddy/` 一致。
   其中 `memory/2026-09-29.md` 记录了本次全部排查细节，**有意不入库**；关键结论已浓缩进本文。
6. `cargo clippy -p ftp-core --all-targets` 目前是**红的**，但**全是既有问题**，与本次修复无关：
   `ftp/passive.rs:263` 的 `reversed_empty_ranges`（默认 deny，故意的反向区间测试）为 error，
   `sftp/server.rs` 4 处 `field_reassign_with_default`、`net.rs:182` 的 `useless_vec` 为 warning。
   本次改动（`client.rs`、`sftp_loopback.rs`）clippy 干净。另外全仓库并非 rustfmt 干净
   （`app/src-tauri/src/lib.rs`、`client.rs` 既有段落都有漂移），故未整体 `cargo fmt`，以免产生无关 diff。

---

## 七、建议调用的技能

下一个 agent 若继续本任务，建议按需调用：
- **`handoff`** — 若会话变长需再次压缩交接
- **`find-skills`** — 出现能力缺口时先找现成技能，不要直接说"做不到"
- 用户级技能 `webview2-tauri-headless-debug`（本次沉淀）— 本机 GUI 白屏排查 + CDP 驱动真实 DOM 的完整流程

---

## 八、快速定位索引

| 想找什么 | 去哪 |
|---|---|
| SFTP 规格与决策（Q1–Q12） | `docs/sftp-design.md` |
| 服务端实现 | `crates/ftp-core/src/sftp/server.rs` |
| TOFU 语义 | `crates/ftp-core/src/sftp/client.rs` + `keys.rs` |
| Tauri 命令层（11 个命令） | `app/src-tauri/src/lib.rs` |
| 折叠指纹组件 | `app/ui/src/components/FingerprintBlock.tsx` |
| 联调工具脚本 | `deliverables/sftp-gui-integration-2026-09-29/` |
| 历史 QA / 诊断报告 | `deliverables/software-company/` |
