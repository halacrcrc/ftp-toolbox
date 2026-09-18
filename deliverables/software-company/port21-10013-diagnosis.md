# 诊断报告：启动 FTP 服务器报 `127.0.0.1:21 ... os error 10013`

- 版本：ftp-toolbox **v0.2.2**
- 现象：`FTP 服务器启动失败: 无法绑定监听地址 127.0.0.1:21：以一种访问权限不允许的方式做了一个访问套接字的尝试。(os error 10013)`
- 诊断主机：`thinkbook14plus\22534`（Windows，**非管理员**，实测 `IsInRole(Administrator) = False`）
- 诊断日期：2026-09-18
- 纪律：**未修改仓库任何源码**；所有实验探针在 `C:\Users\22534\.workbuddy\build\` 下（`port_probe.rs` / `bindlab.rs` / `iface-probe`）。

---

## 0. 一句话根因（实测判定）

> **本机端口 21 已被 `filezilla-server.exe`（以 Windows 服务运行，PID 6404）以 `0.0.0.0:21` + `[::]:21` 独占方式持有着。用户界面选的是「本机回环 127.0.0.1」，于是我们的进程去 bind 具体地址 `127.0.0.1:21`，撞上「已被**独占**持有的通配地址」，Windows 按 WSAEACCES 返回 → `os error 10013`。**
>
> **端口 21 并不在本机的系统保留段里**（实测保留段为 `28385`、`28390`、`50000-50059`）。所以「21 落在保留段」**不是**本次原因；`Error::bind()` 在 `PermissionDenied` 分支给出的 hint（「多为端口落在系统保留段」）在本例中是**误导**。

---

## 1. 实测证据区（本机原样输出）

### 1.1 端口 21 到底被谁占着

`netstat -ano | findstr :21`：
```
  TCP    0.0.0.0:21             0.0.0.0:0              LISTENING       6404
  TCP    [::]:21                [::]:0                 LISTENING       6404
  UDP    10.57.25.225:2177      *:*                                    25944
  UDP    192.168.9.1:2177       *:*                                    25944
  UDP    192.168.136.1:2177     *:*                                    25944
  ...（其余为 2177/其他端口的噪音，与 :21 无关）
```

`netstat -anoq | findstr :21`（`-q` 会列出「已 BOUND 但未 LISTENING」的端口）：
```
  TCP    0.0.0.0:21             0.0.0.0:0              LISTENING       6404
  TCP    [::]:21                [::]:0                 LISTENING       6404
```
> 结论：**没有任何「BOUND 但未 LISTENING」的幽灵占用**；21 只有一条正常的 LISTENING。

`netstat -q`（前几行，确认无其它绑定形态）：
```
  协议  本地地址          外部地址        状态
  TCP    0.0.0.0:21             ThinkBook14Plus:0      LISTENING
  TCP    0.0.0.0:135            ThinkBook14Plus:0      LISTENING
  ...
```

`tasklist /FI "PID eq 6404" /FO LIST`：
```
映像名称:     filezilla-server.exe
PID:          6404
会话名:       Services
会话#   :     0
内存使用 :    9,484 K
```

PowerShell `Get-NetTCPConnection -LocalPort 21`：
```
LocalAddress LocalPort  State OwningProcess
------------ ---------  ----- -------------
::                  21 Listen          6404
0.0.0.0             21 Listen          6404
```
PowerShell `Get-CimInstance Win32_Process -Filter "ProcessId=6404"`：`Name = filezilla-server.exe`（`ExecutablePath`/`CommandLine` 为空，因当前非管理员无权限读取服务进程的路径）。

`sc query filezilla-server`：
```
SERVICE_NAME: filezilla-server
DISPLAY_NAME: filezilla-server
        TYPE               : 10  WIN32_OWN_PROCESS
        STATE              : 4  RUNNING
```

**残留本程序实例检查**（排除「自己上次没关干净」）：
- `tasklist | grep -i ftp` → `(none)`
- `Get-Process | Where-Object { $_.ProcessName -like "*ftp*" }` → 空
- 结论：**没有 ftp-toolbox 残留实例**。占端口的确实不是本程序。

### 1.2 系统保留段 / 动态端口 / portproxy

`netsh int ipv4 show excludedportrange protocol=tcp`：
```
协议 tcp 端口排除范围

开始端口    结束端口
----------    --------
     28385       28385
     28390       28390
     50000       50059     *

* - 管理的端口排除。
```
`netsh int ipv4 show excludedportrange protocol=udp`：
```
     50000       50059     *
* - 管理的端口排除。
```
`netsh int ipv4 show dynamicport tcp`：
```
协议 tcp 动态端口范围
---------------------------------
启动端口        : 1024
端口数          : 64511
```
`netsh interface portproxy show all`：**空**（未配置 portproxy，排除该成因）。

> **端口 21 不在任何排除段内**。`28385/28390` 是普通排除（无 `*`），`50000-50059` 是「管理的端口排除」（带 `*`，Hyper-V/WSL/winnat 类）。

### 1.3 最小复现：直接 bind，看真实错误码

探针 `port_probe.rs`（`std::net::TcpListener::bind`，**不设 `SO_REUSEADDR`**，与 `ftp-core` 的 `tokio::net::TcpListener::bind`（`crates/ftp-core/src/ftp/server.rs:211`）语义一致）：
```
--- target: bottom of the reported problem (port 21) ---
BIND FAIL  127.0.0.1:21           kind=PermissionDenied raw_os_error=Some(10013) [WSAEACCES (访问被禁止/无权限)] msg=以一种访问权限不允许的方式做了一个访问套接字的尝试。 (os error 10013)
BIND FAIL  0.0.0.0:21             kind=AddrInUse raw_os_error=Some(10048) [WSAEADDRINUSE (已被占用)] msg=通常每个套接字地址(协议/网络地址/端口)只允许使用一次。 (os error 10048)

--- controls: high, free ports ---
BIND OK    127.0.0.1:2121         -> local=127.0.0.1:2121
BIND OK    0.0.0.0:2121           -> local=0.0.0.0:2121
BIND OK    127.0.0.1:30000        -> local=127.0.0.1:30000

--- ports inside this machine's reserved bands ---
BIND OK    127.0.0.1:50010        -> local=127.0.0.1:50010
BIND FAIL  127.0.0.1:28385        kind=PermissionDenied raw_os_error=Some(10013) [WSAEACCES]
BIND OK    0.0.0.0:50010          -> local=0.0.0.0:50010
```

> 关键：
> - **`127.0.0.1:21` 的报错文案与用户贴的完全一致**（含 `os error 10013`）。
> - 同一个端口 21，bind **通配** `0.0.0.0:21` 得到的是 **10048**，bind **具体** `127.0.0.1:21` 得到的是 **10013**。→ 报错码取决于「目标地址形态」，这是本案的核心。

### 1.4 机制实验：为什么是 10013 而不是 10048

用 `bindlab.exe` 起一个「占用者」进程，再从**另一个进程**用相同的 `std` bind（不设 SO_REUSEADDR）去撞：

```
########## A2: PLAIN(普通) wildcard 占用者 0.0.0.0:30007 ##########
-- probe specific 127.0.0.1:30007 --   BIND OK   127.0.0.1:30007
-- probe wildcard 0.0.0.0:30007   --   BIND  0.0.0.0:30007  FAIL code=10048 [WSAEADDRINUSE]

########## B2: EXCLUSIVE(SO_EXCLUSIVEADDRUSE) wildcard 占用者 0.0.0.0:30008 ##########
HOLD-EXCL 0.0.0.0:30008 WSAStartup_rc=0 setsockopt_rc=0(err=0) bind_rc=0(err=0) listen_rc=0
-- probe specific 127.0.0.1:30008 --   BIND  127.0.0.1:30008  FAIL kind=PermissionDenied code=Some(10013) [WSAEACCES]
-- probe wildcard 0.0.0.0:30008   --   BIND  0.0.0.0:30008    FAIL code=10048 [WSAEADDRINUSE]
```

| 占用者持有方式 | 我们 bind **具体** `127.0.0.1:P` | 我们 bind **通配** `0.0.0.0:P` |
|---|---|---|
| 普通（未设 SO_EXCLUSIVEADDRUSE）持有 `0.0.0.0:P` | **成功** | 10048 |
| **独占**（SO_EXCLUSIVEADDRUSE）持有 `0.0.0.0:P` | **10013 WSAEACCES** | 10048 |
| **filezilla-server 持有 `0.0.0.0:21`（实测）** | **10013 WSAEACCES** | 10048 |

> **判定闭环**：`10013`（对具体地址）这件事**只在占用者用了 `SO_EXCLUSIVEADDRUSE` 时才会出现**。filezilla-server 的实测行为与「独占持有」分支**逐位吻合**。⇒ 占用者 = 一个**带独占绑定加固的 FTP 服务**（FileZilla Server 常规做法，防端口劫持）。
>
> 换言之：**`10013` 不是「端口被普通占用」的信号，而是「端口被独占/受保护占用，或被系统禁止」的信号。** 普通占用会给 `10048`。

补充扫描（同一进程顺序 bind，互不残留）：
```
127.0.0.1:21   → 10013      127.0.0.1:28385 → 10013      127.0.0.1:50010 → OK
127.0.0.1:80   → OK         127.0.0.1:28390 → 10013      127.0.0.1:50059 → OK
127.0.0.1:443  → OK         127.0.0.1:28391 → OK         0.0.0.0:28385   → 10013
127.0.0.1:1023 → OK         127.0.0.1:30000 → OK         0.0.0.0:1023    → OK
127.0.0.1:1024 → OK
127.0.0.1:1025 → OK
```

### 1.5 关于「用户文案里没有 hint」的不一致

用户贴的文本止于 `... (os error 10013)`，**没有**括号 hint；但 `PermissionDenied` 分支（`crates/ftp-core/src/error.rs:70-72`）本应追加 `（没有权限绑定该端口：Windows 上多为端口落在系统保留段，见 netsh ...）`。

可能原因（**无法证实，按可能性排序**）：
1. **用户粘贴时截断/只复制了首行**（该 hint 很长，一行显示容易被截）。← 最可能；
2. 用户所跑的构建**早于**该 hint 的引入时间点；
3. 前端日志视图对该行做了视觉截断。

> 结论：**该不一致不影响根因判定**——`os error 10013` + 文案与 `Error::Bind` 的 `Display` 模板逐字吻合，足以确认用户跑的确实是含 `Error::Bind` 的 v0.2.2。需要说明的是，**即便 hint 完整显示，它在本次也是错的**（见 §2 第 1 行）。

---

## 2. 根因优先级表

| # | 成因 | 为什么会导致 **10013**（而非 10048） | 判定命令 | 判定标准（看到什么=就是它） | 处置办法 |
|---|---|---|---|---|---|
| **1** | **另一个进程以 `SO_EXCLUSIVEADDRUSE` 独占持有 `0.0.0.0:21`（本案：filezilla-server）** | Windows 把通配绑定视为覆盖全部具体地址；占用者设了独占位时，后来者对**具体地址**的 bind 被拒 → WSAEACCES。对通配再 bind 才会是 10048 | `netstat -ano \| findstr :21` → 记 PID；`tasklist /FI "PID eq <pid>"`；`Get-NetTCPConnection -LocalPort 21` | 21 有 LISTENING 且 PID ≠ 自己；**且**你的目标是 `127.0.0.1:21` 这类具体地址；把目标换成 `0.0.0.0:21` 会变成 **10048** | 停掉该服务（`net stop filezilla-server`）或改用它；或改用别的端口（≥1024，如 2121）；不要指望「换地址」能绕过 |
| **2** | **端口落在系统保留段** | 保留段内的端口 bind 直接被 ACL 层拒绝 → WSAEACCES | `netsh int ipv4 show excludedportrange protocol=tcp` | 目标端口**落在**列出的 `[开始,结束]` 区间内 | 移出保留段：改用段外端口；或 `net stop winnat`/`net start winnat` 释放 Hyper-V 类保留；或用 `netsh int ipv4 set dynamicport tcp start=... num=...` 挪动态范围（有风险，见 §4） |
| **3** | **`netsh interface portproxy` 占端口** | portproxy 会用 `SO_EXCLUSIVEADDRUSE` 绑定被转发的端口 → WSAEACCES | `netsh interface portproxy show all` | 列表里出现 21 | `netsh interface portproxy delete v4tov4 listenport=21 listenaddress=0.0.0.0` |
| **4** | **安全软件 / EDR 的 WFP 过滤驱动** | 内核 WFP 层直接拒绝该进程的 bind / 或对低端口做策略拦截 → WSAEACCES | 关闭杀软/EDR 复测；或查其网络防护日志；`netsh wfp show state` | 无占用却仍 10013，且关掉防护后恢复 | 在该安全软件里放行本程序；或改用高端口 |
| **5** | **手工 `netsh int ipv4 add excludedportrange` 保留** | 与 #2 同机制，但来源是永久/会话性的人工保留 | `netsh int ipv4 show excludedportrange protocol=tcp` | 21 在列表里，且行尾**无** `*`（普通排除） | `netsh int ipv4 delete excludedportrange protocol=tcp startport=21 numberofports=1`（需管理员） |
| **6** | **本程序残留实例仍绑着 `0.0.0.0:21`** | **注意**：tokio/std 默认**不设**独占位。左列实验 A2 已证：**普通**占用者持 `0.0.0.0:P` 时，我们对 `127.0.0.1:P` 的 bind **会成功**；只有绑 `0.0.0.0:P` 才得 10048 | `tasklist \| grep -i ftp`；`netstat -ano \| findstr :21` | 有 ftp-toolbox 进程且持 21 | 关掉残留实例。**注意**：进程内守卫（`app/src-tauri/src/lib.rs:63-71`）只查本进程 `AppState`，**看不到另一个进程**，别指望它拦 |

> **据此推出的重要判据**：用户目标是 `127.0.0.1:21` 却拿到 **10013**，几乎**排除** #6（残留实例走普通绑定应给 10048 甚至放行），**强烈指向 #1（被独占加固的服务占用）或 #2/#5（保留段）**。本机实测：21 **不在**保留段（排除 #2/#5），21 的持有者是带独占语义的服务（命中 #1）。

---

## 3. 平台成因对比

**同一串 `os error 10013`，在三个平台上「不许可」的来源完全不同**：

| 维度 | Windows | Linux | macOS |
|---|---|---|---|
| **是否存在「<1024 需管理员」规则** | **不存在**（实测非管理员 bind `127.0.0.1:80 / :443 / :1023` **均成功**） | **存在**：内核 `net.ipv4.ip_unprivileged_port_start`（默认 1024） | **存在**：<1024 需特权 |
| 错误号与含义 | `WSAEACCES = 10013`：**端口被独占持有 / 落在保留段 / 被 WFP 或 ACL 拒绝** | **`EACCES`，errno 13**：内核拒绝非特权进程绑低端口 | `EACCES`（同为 errno 13）：同 Linux 语义 |
| 同一个「13」是否同义 | 否 | 否 | 否 |
| 触发低端口拒绝的机制 | ——（无此机制） | `ip_unprivileged_port_start`（capability `CAP_NET_BIND_SERVICE` 可越过） | 特权端口策略 |
| SELinux/AppArmor | 无（对应 WFP/EDR） | bind 也可被 **SELinux**（如 `http_port_t` 外的端口）/ **AppArmor** 拒（多为 `EACCES`），与 uid 无关 | 沙盒/`sandbox` 配置可拒 |
| 被 systemd socket 占着 | 对应「服务/portproxy 占端口」 | 若存在 `foo.socket`，端口由 systemd 持有，你的进程 bind 得 `EADDRINUSE`（10048 类比），**不是** EACCES | 对应 launchd 已占 |

**低端口四类处置（Linux 为例，Windows 用不上）**：
1. 以 **root** 运行（最直接，最不推荐长期用）。
2. **`setcap`**：`sudo setcap 'cap_net_bind_service=+ep' /path/to/ftp-toolbox`。**注意**：对 Tauri 打包产物/AppImage 未必适用——AppImage 每次运行挂在临时目录、二进制路径变化，且**每次升级后需重设** capability；发行版若用 squashfs 挂载通常无法持久设置 文件 capability。
3. **`sysctl`**：`sudo sysctl -w net.ipv4.ip_unprivileged_port_start=21`（降低门槛，全系统生效）；持久化写 `/etc/sysctl.d/99-ftp-toolbox.conf`：`net.ipv4.ip_unprivileged_port_start=21`（**注意**：这会让**任何**用户都能绑 ≥21 的端口，安全面变大）。
4. **authbind / 端口转发**：`authbind --deep ./ftp-toolbox`；或用 `iptables -t nat -A PREROUTING -p tcp --dport 21 -j REDIRECT --to-port 2121`（把 21 转发到高端口，程序本身只需绑 2121）；或用 **systemd socket activation**（`ftp-toolbox.socket` 持 21，`Accept=no`，把 fd 传给服务）。
5. 另外两类「不是权限问题却像权限问题」：**SELinux/AppArmor** 拒 bind（查 `ausearch -m avc` / `dmesg`）；**systemd socket 已占**（查 `systemctl status *.socket`，报的通常是 `EADDRINUSE`）。

**macOS**：同为 <1024 特权，但**没有 `setcap`**。可用：`sudo` 运行；**`launchd` 特权 helper**（把绑定放到 root 的 LaunchDaemon，再把 fd 交给 GUI 进程，做成 SMAppService/`SMJobBless` 风格）；**`pf` 端口转发**（`rdr pass on lo0 proto tcp from any to any port 21 -> 127.0.0.1 port 2121`）。另需注意系统服务抢端口：**AirPlay Receiver** 会占用 5000/7000 等（**本机未验证，属已知系统行为，需在目标机实测确认**），若撞上需在「系统设置 → 通用 → 隔空投送与接力」关闭接收。

> **结论**：**同一行错误文案在 Windows / Linux / macOS 上的「正确修法」互不相同**——Windows 要去查「谁独占/是否保留段」，Linux/macOS 要去解决「低端口特权」。因此**产品不能只给一条通用提示**。

---

## 4. 修复方案

### 4.1 用户立刻能做的（按成本排序）

1. **改用 ≥1024 的端口**（最省事、最正确）：把监听端口从 `21` 改成 `2121`。实测 `127.0.0.1:2121` / `0.0.0.0:2121` 均 bind 成功。
2. **先查清并让出 21**：本案 `net stop filezilla-server`（需管理员）即可释放；或将本工具指向别的端口。
3. **释放 Hyper-V/winnat 类保留**（仅当端口确实落在保留段时）：`net stop winnat && net start winnat`。
4. **杀掉残留实例**：`taskkill /PID <pid> /F`。
5. **以管理员身份运行**：仅在「端口被别的特权进程独占」这类场景有意义；**对本案无效**（FileZilla 已 LISTENING，管理员也抢不过一个已独占的端口）。
6. **保留段的正确增删（管理员，谨慎）**：
   - 查看：`netsh int ipv4 show excludedportrange protocol=tcp`
   - 加（持久）：`netsh int ipv4 add excludedportrange protocol=tcp startport=2121 numberofports=100 store=persistent`
   - 删：`netsh int ipv4 delete excludedportrange protocol=tcp startport=2121 numberofports=100`
   - 挪动态范围：`netsh int ipv4 set dynamicport tcp start=49152 num=16384`（**风险**：会改变整机临时端口分配区间，可能影响其它服务；不建议普通用户动）。

### 4.2 产品该改的（具体到文件与函数）

> ⚠️ 以下为**建议代码**，本次**未落地**（诊断任务）。

**(a) `app/src-tauri/src/lib.rs` — `start_ftp_server` 绑前预检 + 分码提示**
在 `ftp_core::ftp::start_server_with(...)` **之前**插入预检，把 `10013/10048/10049` 分开提示：
```rust
// 新增：端口可用性预检（示意，未落地）
#[tauri::command]
fn check_listen_addr(addr: String) -> PortCheck { /* 用 std::net::TcpListener::bind 试一次再 drop；
     Ok => ok；Err(e) => 按 e.raw_os_error() 归因（10013/10048/10049）并附平台处置命令 */ }

// start_ftp_server 里，在 start_server_with 之前：
if let Some(report) = preflight_conflict(&addr) {
    return Err(report); // 直接给出「谁占了 / 该执行什么命令」
}
```
预检的纯函数思路与 `crates/ftp-core/src/ftp/passive.rs` 保持一致（纯逻辑可单测，OS 调用放 GUI 壳）。

**(b) `crates/ftp-core/src/error.rs:64-83` — hint 分平台细化**
现在的 `PermissionDenied` 分支把「保留段」当**唯一**解释（`error.rs:70-72`），本例已证其误导。建议：
```text
Windows(WSAEACCES/10013)：
  「(10013 许可被拒) 常见原因有三：①端口被另一进程【独占】占用（netstat -ano | findstr :<端口> 查 PID，
   注意此时绑 0.0.0.0 会报 10048 而绑具体 IP 报 10013）；②端口落在系统保留段
   (netsh int ipv4 show excludedportrange protocol=tcp)；③被安全软件/WFP 拦截。」
Linux(EACCES/13)：  「1024 以下端口需 root / CAP_NET_BIND_SERVICE（setcap 或 authbind）」
macOS(EACCES/13)：  「1024 以下端口需 root / launchd 特权 helper」
```
建议按 `e.raw_os_error()` 而非仅 `ErrorKind` 分派，因为**同一个 `PermissionDenied` kind 在 Windows 上混装了「保留段」「独占占用」「WFP」三种语义**。

**(c) 低端口 UI 提醒（`app/ui/src/views/ServersView.tsx`）**
当 `prefs.port < 1024` 时直接红字提醒（Windows 上说明「需 ≥1024 通常无权限问题，但更可能被保留段/独占占用」，Linux/macOS 说明「需 root/setcap」）。

**(d) 是否把 FTP 默认端口从 21 换成 2121？——判断与理由**
- **建议：不要仅因「可能被占用」就偷偷改默认值。** 理由：①FTP 21 是协议惯例，客户端/文档/用户预期都绑定 21，改了会让「复制教程却连不上」；②真正的问题是**错误归因不清**（本例 hint 误导），不是默认值选择；③与 **TFTP 默认 69** 一起看：两者都是 <1024 惯例端口，若只改 FTP 会造成内部不一致。
- **推荐**：**保留 21 为默认**，但把「端口无法绑定时」的提示做成**可执行的一键改用 2121**（后端预检命令返回 `suggested: "2121"`，UI 给按钮，逻辑与 `PassivePortCheck.suggested` 完全同构）。这样既不破坏惯例，又把「换个端口就好了」的路径缩短到一次点击。
- 若产品要更保守：可在**首次启动引导**里提示「21 可能被系统/其它 FTP 服务占用，若失败可改用 2121」。

---

## 5. 未验证 / 不确定项

- **无法在用户机器上复核**：以上全部为本诊断机（thinkbook14plus\22534）实测。用户机保留段/占用进程可能不同——但**判据表 §2 可直接套用**。
- **用户文案缺 hint** 的成因未证实（§1.5，按可能性排序，判为「粘贴截断」最可能）。
- **filezilla-server 是否显式设置 `SO_EXCLUSIVEADDRUSE`**：按其**行为逐位等价于独占绑定**推断（§1.4），未读取其配置/二进制确认。
- **「管理的端口排除（带 `*`）」是否阻止 bind**：本机实测**不阻止**（`127.0.0.1:50010`、`0.0.0.0:50000/50059` 均 OK），而**普通排除**（`28385/28390`，无 `*`）**阻止**（10013）。此「带 `*` 不阻拦显式 bind」的行为**可能随 Windows 版本/winnat 状态变化**，仅对本机当日结果有效；上表已按此区分并标注。
- Linux/macOS 平台成因**未在本机执行验证**（无可用 Linux 环境；WSL 的 `docker-desktop` 发行版处于 Stopped，未启用），依据为既有机制知识，已标注「需在目标机实测」。
