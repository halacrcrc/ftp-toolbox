# QA 独立验证报告 + 验收测试计划

> 验证者：严过关（QA Engineer）｜任务 #3
> 验证对象：寇豆码（Engineer）的两项诊断
> 验证方式：**独立读码 + 实机探针实测**（未修改仓库任何源码）
> 验证时间：2026-09-18｜环境：Windows + Git Bash，rustc 1.98.1 (48a229cea 2026-09-01)

---

## 0. 一句话结论（最重要的事先说）

**本机可以复现用户报告的 `无法绑定监听地址 127.0.0.1:21：…(os error 10013)`。**

根因不是「21 落在系统保留段」（本机 `netsh` 明确显示 21 **不在**任何保留段），而是：

> 另一个进程以 **`SO_EXCLUSIVEADDRUSE`** 独占绑定了 `0.0.0.0:21`（本机是 `filezilla-server.exe`，Windows 服务 `filezilla-server`）。当用户在下拉里选了**具体地址 `127.0.0.1`** 去绑一个**已被通配地址独占**的端口时，Windows 返回 **WSAEACCES(10013)**（而不是 10048）；而 Rust 把 10013 映射成 `ErrorKind::PermissionDenied`，于是 `error.rs::bind` 选中了**「端口落在系统保留段」这条错误的提示分支** → 用户被引导去查 `excludedportrange`，而真正该做的是 `netstat` 找占用进程。

因此：**team-lead 预期的「端口落在保留段」方向对本机这个实际案例是「误诊」**。这是一个**源码层文案缺陷**（会导致用户按错误方向排障），按 QA 路由规则应回给 Engineer 修复。详见 §2 断言 #1 与 §4。

---

## 1. 验证方法与环境

| 项 | 值 |
|---|---|
| 仓库 | `C:\WorkBuddy\FTP`（v0.2.2） |
| 工具链 | cargo/rustc 1.98.1（`C:\Users\22534\.cargo\bin`） |
| 探针位置 | `C:\Users\22534\.workbuddy\build\qa-probe\`（自带 rsproxy `.cargo/config.toml`，**未污染仓库**） |
| 探针日志 | `qa-probe-run.log` / `-run2.log` / `-run3.log` / `-run4.log` |
| 第三方依赖 | `ipconfig 0.3`、`get_if_addrs 0.5`、`socket2 0.6`、`windows-sys 0.61`（均为验证用途，仅放在探针里） |
| 权限 | 当前会话 **非管理员**（`net session` → Access denied） |
| 未做 | 未改仓库源码；未停止用户的 FileZilla 服务；未插拔真实网线；未在 Linux 机器上实测 |

---

## 2. 论断核对表

图例：✅证实 ｜ ⚠️存疑/不完整 ｜ ❌证伪

### 断言 1 — `error.rs` 的 Display 与 hint 分支；10013 是否映射为 `PermissionDenied`

| 子断言 | 判定 | 证据 |
|---|---|---|
| `Error::Bind` 的 Display 是 `无法绑定监听地址 {addr}：{source}{hint}` | ✅ | `crates/ftp-core/src/error.rs:36` |
| `hint` 按 `source.kind()` 分支：AddrInUse / PermissionDenied / AddrNotAvailable / 其它为空 | ✅ | `error.rs:64-83` |
| **`io::Error::from_raw_os_error(10013).kind() == PermissionDenied`** | ✅ **实测证实** | 探针日志 run2 第 18 行：`from_raw_os_error(10013) -> kind=PermissionDenied` |
| Windows 上 os error 10013 会走到「端口落在系统保留段」hint | ✅ | 由上一行直接推出（同一 `kind()`） |
| **该 hint 对「端口被别的进程独占」场景是误导（缺陷）** | ❌ **证伪 team-lead 预期方向** | 见断言 5 + §4：本机 21 不在保留段，仍产出 10013，hint 却只说保留段 |

同批实测映射（run2 第 17-24 行）：

```
from_raw_os_error(     5) -> PermissionDenied   (ERROR_ACCESS_DENIED)
from_raw_os_error( 10013) -> PermissionDenied   (WSAEACCES)   ← 关键
from_raw_os_error( 10048) -> AddrInUse          (WSAEADDRINUSE)
from_raw_os_error( 10049) -> AddrNotAvailable   (WSAEADDRNOTAVAIL)
from_raw_os_error( 10061) -> ConnectionRefused
from_raw_os_error( 10014) -> Uncategorized
```

> 结论：Rust **确实**把 10013 映射为 `PermissionDenied`（工程/team-lead 的说法成立）。但这恰恰是问题所在——`PermissionDenied` 在 Windows 上是**多因一果**的。

### 断言 2 — `start_ftp_server` 是否「bind 失败即在返回前报错」+ 仅进程内查重

| 子断言 | 判定 | 证据 |
|---|---|---|
| 不是 fire-and-forget；bind 失败在命令返回前冒泡 | ✅ | `app/src-tauri/src/lib.rs:87-89` `.await.map_err(err)?`；`crates/ftp-core/src/ftp/server.rs:211` `TcpListener::bind(addr).await.map_err(|e| Error::bind(addr,e))?` 在任何 spawn 之前 |
| 只做**进程内**查重，挡不住别的进程占端口 | ✅ | `lib.rs:63-71` 只检查 `state.ftp_server`（本进程自己的句柄 `h.is_running()`），无跨进程检测 |
| 前端确实会展示该错误 | ✅ | `views/ServersView.tsx:190-192` `catch(e){ log(`${title}启动失败: ${e}`,"error") }` |

补充（我加的观察）：命令层**先**查重、**再** bind，顺序正确；但 `Error::bind` 的 hint 是唯一会被用户看到、也唯一被读作「诊断结论」的文本，它的准确性至关重要。

### 断言 3 — `enumerate_interfaces` 过滤规则

| 子断言 | 判定 | 证据 |
|---|---|---|
| Windows 路径按 `OperStatus::IfOperStatusUp` 过滤 | ✅ | `lib.rs:640-642` |
| Windows 路径**另外**过滤 SoftwareLoopback 与 169.254 link-local | ✅（team-lead 未提，但存在） | `lib.rs:644-646`、`lib.rs:657-659` |
| 非 Windows 路径**完全不过滤链路状态** | ✅ | `lib.rs:678-697`；`enumerate_interfaces_fallback` 直接 `get_if_addrs()`，无任何 oper/link 判断 |
| Windows 在 ipconfig 失败时也回退到该无过滤路径 | ✅ | `lib.rs:632-635` |
| 实测：fallback 路径在 Windows 上确实返回「Down 的网卡」 | ✅ 实测 | run4 第 28-33 行：`get_if_addrs` 返回了 Down 的 `以太网 2 (192.168.102.161)`，且名字是 GUID `{299671A0-…}` |
| 实测：Windows 路径过滤后 = WLAN + VMnet1 + VMnet8 + 本机回环 | ✅ 实测 | run4 第 25 行 |

**实测补充（重要）**：run4 第 16 行显示 `以太网 2` 的 `oper_status=IfOperStatusDown`，但它**仍然持有真实 IP `192.168.102.161`**。这说明 **`OperStatus Down` ≠ 地址已消失**。因此「按 Up 过滤」只是启发式，会**隐藏一个仍可能可绑定的 Down 网卡**；反过来也说明「拔网线→地址立刻收回」并不总成立。

### 断言 4 — `ServersView.tsx` 是否只在挂载拉一次 / 失效改写 effect

| 子断言 | 判定 | 证据 |
|---|---|---|
| 网卡列表只在挂载时拉一次 `useEffect(..., [])` | ✅ | `ServersView.tsx:402-405` |
| 无任何轮询 / window focus / 系统网络事件订阅 | ✅ | 全文件仅 3 个 `useEffect`（`:115` 持久化、`:128` 被动端口检查、`:156` iface 失效改写、`:402` 挂载拉取），无 `setInterval`/`addEventListener`/`online` |
| 唯一能触发重新枚举的入口是「刷新」按钮 | ✅ | `:244-246` 按钮 → `refresh` → `onRefresh` → `refreshAll`（`:408-410`）同时刷状态与网卡 |
| 「保存的 iface 失效就改写 `0.0.0.0` 并写回 localStorage」的 effect **存在** | ✅ | `ServersView.tsx:156-166`；写回由 `:115-117` 的持久化 effect 完成 |
| 该 effect **不检查 `running`**，会在服务运行中也触发 | ✅ **隐患确认** | `:156-166` 的守卫只有 `prefs.iface==="0.0.0.0" \|\| interfaces.length===0`，**没有 `!running`**；依赖数组 `[interfaces, prefs.iface]` |

**隐患成立性判定**：成立，且比 team-lead 描述得更具体——
- 当前（无轮询）该 effect 只在「`interfaces` 数组引用变化」时触发，即用户**手动点刷新**、或启动/停止后 `await onRefresh()`（`:194`、`:206`）。
- 也就是说**即便今天**：服务运行中若网卡掉了、用户手点一次「刷新」，`interfaces` 变化 → effect 触发 → `set("iface","0.0.0.0")` → 持久化 effect 把 `ftp-toolbox:server:ftp` 的 `iface` **静默改写成 `0.0.0.0`**，而服务器其实**仍绑在旧 IP** 上（后端 `status.localAddr` 不变）。用户下次启动就会用 `0.0.0.0`，丢失原选择。
- **一旦按方案加轮询，这个 effect 会在运行时被自动触发**，隐患从「要点一下刷新」升级为「拔网线即发生」。→ 必须与轮询修复同批处理，见 §6 回归风险 R1。

### 断言 5 — 实机平台数据（我自己跑的原始输出）

**5.1 `netsh int ipv4 show excludedportrange protocol=tcp`（bash 直调 `netsh.exe`）**

```
协议 tcp 端口排除范围

开始端口    结束端口
----------    --------
     28385       28385
     28390       28390
     50000       50059     *
     50131       50131

* - 管理的端口排除。
```

**→ 端口 21 不在任何保留段。** 保留段为 28385、28390、50000-50059(*)、50131。

**5.2 `netsh int ipv4 show dynamicport tcp`**

```
启动端口        : 1024
端口数          : 64511
```
→ 动态端口 = 1024-65535，**21 低于该区间**，既非动态段也非保留段。

**5.3 `netsh interface portproxy show all`** → **空**（无 portproxy 规则）。

**5.4 `netstat -ano | findstr :21`**（`netstat -anoq` 结果相同）

```
  TCP    0.0.0.0:21             0.0.0.0:0              LISTENING       6404
  TCP    [::]:21                [::]:0                 LISTENING       6404
```
→ 端口 21 **已被占用**。

**5.5 占用者身份（`tasklist /svc /fi "PID eq 6404"`）**

```
映像名称                       PID 服务
filezilla-server.exe          6404 filezilla-server
```
→ **`filezilla-server.exe`（Windows 服务 `filezilla-server`）在 `0.0.0.0:21` 与 `[::]:21` 监听。**

**5.6 实测绑定（探针 run2 / run3，原样摘录）**

```
# run2  Part A：保留段 × 地址矩阵
[port 28385] bind 0.0.0.0:28385 => ERR raw=10013 kind=PermissionDenied
             bind 127.0.0.1:28385 => ERR raw=10013 kind=PermissionDenied
[port 28390] bind 0.0.0.0:28390 => ERR raw=10013 ; bind 127.0.0.1:28390 => ERR raw=10013
[port 50000] bind 0.0.0.0:50000 => OK  ; bind 127.0.0.1:50000 => OK   ← 见 §7 存疑项
[port 50010] bind 0.0.0.0:50010 => OK  ; bind 127.0.0.1:50010 => OK
[port 50055] bind 0.0.0.0:50055 => OK  ; bind 127.0.0.1:50055 => OK
[port 50131] bind 0.0.0.0:50131 => ERR raw=10013 ; bind 127.0.0.1:50131 => ERR raw=10013

# run3 Part D：自建通配 holder 并显式 setsockopt(SO_EXCLUSIVEADDRUSE,1)（rc=0 成功）
  holder: 0.0.0.0:45991 LISTENING (exclusive)
  bind 127.0.0.1:45991 => ERR raw=10013 kind=PermissionDenied
  bind 0.0.0.0:45991   => ERR raw=10048 kind=AddrInUse
  bind 127.0.0.2:45991 => ERR raw=10013 kind=PermissionDenied
# run3 Part E：同样的 holder，但**不设** exclusive（普通 std socket）
  bind 127.0.0.1:45992 => OK      ← 关键对照：普通通配 holder 并不阻止具体地址绑定
  bind 0.0.0.0:45992   => ERR raw=10048

# run3 Part F：真实案例（filezilla 占 21）
  bind 127.0.0.1:21 => ERR raw=10013 kind=PermissionDenied   ← 与用户报错逐字一致
  bind 0.0.0.0:21   => ERR raw=10048 kind=AddrInUse
```

**5.7 枚举耗时（探针 run4 Part I）**

```
ipconfig::get_adapters() 50 次： min=3.16ms p50=3.65ms p95=4.54ms max=5.20ms mean=3.71ms
get_if_addrs()          50 次： min=2.30ms p50=2.52ms max=3.51ms
```
→ **单次枚举约 3-4ms**。2s 轮询约 `0.2%` 的 CPU，1s 轮询约 `0.4%`，**轮询方案完全可接受**（见 §3）。

---

## 3. 问题 2（下拉不实时刷新）——诊断核对

| team-lead/工程的说法 | 判定 | 依据 |
|---|---|---|
| 根因：网卡列表只在挂载拉一次 | ✅ | `ServersView.tsx:402-405`，无轮询/事件，仅手动刷新可更新 |
| 「轮询就能实时刷新」是否可行 | ✅ 可行 | 实测枚举 p50≈3.7ms；2s 周期成本可忽略 |
| 「网卡重新枚举就会立刻消失」 | ⚠️ **存疑（过度简化）** | 见下 |

**对「重新枚举就立刻消失」的反例与边界**（我能实测的 + 不能实测的分开说）：
- **能实测**：run4 第 16 行 `以太网 2` 处于 `IfOperStatusDown` 却**仍持有 `192.168.102.161`** → 「链路 down」与「地址是否存在」是两件事，**不能假设拔网线后地址一定消失或一定保留**。
- **能实测**：Windows 路径过滤的是 `OperStatus`，**不是地址存在性**；因此「拔网线→OperStatus 转 Down→从列表消失」是**依赖 NDIS 媒体状态及时翻转**的假设，本机无法在不物理拔线的情况下验证其**时延**。
- **不能实测（Linux）**：Linux 走的是 `enumerate_interfaces_fallback`，**根本不读 operstate**。Linux 上拔网线后地址是否仍在，取决于 NetworkManager/`dhclient` 的租约与 `carrier` 状态；`get_if_addrs()` 只列地址，**没有任何链路状态信号**。→ 结论：**非 Windows 平台即使加了轮询，也未必会移除已 down 的网口**，除非同时在 fallback 路径补上链路状态读取（`/sys/class/net/<if>/operstate` 或 netlink）。这一点 team-lead 的说法在 Windows 上可能成立、在 Linux 上**不成立**。

---

## 4. 问题 1（10013）——根因判定（本机）

**本机当前状态下，绑定 `127.0.0.1:21` 失败的最可能原因 = 端口被 `filezilla-server.exe` 以 `SO_EXCLUSIVEADDRUSE` 独占（监听在 `0.0.0.0:21`），而不是保留段。**

证据链（全部可复现）：
1. `netsh` 显示 21 **不在**保留段（5.1），动态段也是 1024+（5.2）→ 排除保留段/动态段。
2. `netstat` 显示 21 被 PID 6404 占用（5.4）→ 占用成立。
3. PID 6404 = `filezilla-server.exe`（5.5）→ 占用者确认。
4. 绑定 `127.0.0.1:21` → **10013**，绑定 `0.0.0.0:21` → **10048**（5.6 run3-F）→ 与用户报错**逐字一致**。
5. **对照实验**（run3-D/E）：普通通配 holder 下 `127.0.0.1:P` 能绑成功；给 holder 显式设 `SO_EXCLUSIVEADDRUSE` 后 `127.0.0.1:P` 变 10013、`0.0.0.0:P` 变 10048 → **机制被隔离证明**。

**结论**：Windows 上 `WSAEACCES(10013)` 是「多因一果」，至少两类：
- (a) 端口落在**真正的**系统排除段（28385/28390/50131 实测 10013）；
- (b) 目标地址与一个**带 `SO_EXCLUSIVEADDRUSE` 的通配监听**发生重叠（实测 10013）；
- 另有 (c) WFP/安全软件、组策略、portproxy 等（本机未观测到，无法证实）。

而 `error.rs` 收到 `PermissionDenied` 时**只提 (a)**，对用户实际的 (b) 给出**错误排查方向** → **文案缺陷，会误诊**。

> 另：team-lead 提醒的「Windows 低端口需要管理员」是错的——本报告确认**仓库中不存在该错误表述**（`error.rs:71`、`server.rs:186`、`tftp/server.rs:77` 均把特权**正确限定在 Linux/macOS**）。唯一措辞不严谨处：`passive.rs:21` 注释 "privileged range on every platform"——Windows 实际无 <1024 特权规则（实测非管理员可绑 7/23/80/1023，run2 第 45-49、76-77 行）。

---

## 5. 被证伪 / 需修正的假设清单（含我自己先提出的）

| # | 假设 | 结果 | 说明 |
|---|---|---|---|
| H1 | team-lead：10013 = 端口落在保留段 | ❌ 过窄/误诊 | 本机 21 不在保留段却给出 10013；真正原因是独占占用 |
| H2 | 我初判：任何「通配 holder」都会让具体地址绑定报 10013 | ❌ 我自己证伪 | run3-E：普通 holder 下具体地址绑定**成功**；必须是 holder 设了 `SO_EXCLUSIVEADDRUSE` 才报 10013 |
| H3 | 保留段一定不可绑定 | ⚠️ 不成立（本机） | 50000-50059 标 `*`（managed）却**可绑定**；非 `*` 的 28385/28390/50131 不可绑定 → 见 §7 |
| H4 | 「网卡重枚举就立刻消失」 | ⚠️ 过度简化 | Down 网卡仍可能持有地址；Linux fallback 根本无链路状态 |
| H5 | Windows 低端口需管理员 | ❌ 本身就是错的 | 仓库无此表述；实测非管理员可绑低端口 |

---

## 6. 验收测试计划（QA 主产出）

> 目标：把上面每条「应该怎样」落成**可照做、可判失败**的用例。

### 6.1 手工验收用例 A —— Windows：端口被独占时的错误文案（优先级 P0）

**前置**
- 同一台机器上有**另一个进程**以通配地址独占某端口。任一即可：
  - 现成的：本机 `filezilla-server` 占 `0.0.0.0:21`；
  - 或自建：用探针 `qa-probe`（run3 Part D）在 `0.0.0.0:45991` 起一个带 `SO_EXCLUSIVEADDRUSE` 的监听。
- 用**非管理员**身份运行 ftp-toolbox。

**步骤**
1. 打开「服务器」页，`监听接口` 选 **`本机回环（127.0.0.1）`**，`端口` 填 **21**（或自建监听的那个端口）。
2. 点「启动服务」，读日志里那条 `FTP 服务器启动失败: …`。

**期望结果**
- 文案必须让用户能定位到「**端口已被其它进程占用**」，并给出可执行动作：`netstat -ano | findstr :21`（或等价命令）+ 结束该进程/换端口。
- 文案**可以**同时提保留段，但**不得只提保留段**、不得把用户引向 `excludedportrange` 作为唯一方向。
- 文案中的地址要保留 `127.0.0.1:21`（现状 OK）。

**失败判据**
- 文案只出现「端口落在系统保留段 / excludedportrange」而**未提占用与 netstat** → **FAIL**。
- 文案把原因说成「权限不足/需要管理员」→ FAIL（Windows 无此规则）。
- 服务界面显示「运行中」→ FAIL（现状不会）。

**当前实测**：给出的是「端口落在系统保留段…excludedportrange」→ **本用例当前 FAIL**（这正是要修的缺陷）。

### 6.2 手工验收用例 B —— Windows/Linux：非管理员绑低端口（P1）

**步骤（Windows，非管理员）**
1. 确认某低端口空闲（如 23 或 2121）。
2. `端口` 填该值、`接口` 选 `0.0.0.0`，启动。

**期望**：**启动成功**（Windows 允许非管理员绑任意空闲端口）。
**失败判据**：以「需要管理员/权限不足」为由拒绝或提示升级 → FAIL。

**步骤（Linux，非 root）**
1. `端口` 填 **21** 启动 → 期望**失败**，且文案是 **Linux 专属**指向「<1024 端口需要 root 或 `CAP_NET_BIND_SERVICE`」。
2. `端口` 填 **2121** 启动 → 期望**成功**。
**失败判据**：Linux 下 2121 失败；或 21 失败文案里只出现 Windows 的 `excludedportrange` 建议而**没有** Linux 的 root 提示 → 记为文案缺陷（建议：hint 按平台 `cfg` 分叉，避免给用户看另一平台的指令）。

### 6.3 手工验收用例 C —— 拔网线：下拉实时移除（P1，问题 2 核心）

**步骤**
1. 插着网线启动工具，进入「服务器」页，记下 `监听接口` 下拉里那块有线网卡（本机为 `以太网 2（192.168.102.161）`）。
2. **不点刷新**，拔掉网线（或「禁用」该网卡）。
3. 等待 ≤ `轮询周期 + 1 个周期余量`（若实现为 2s，则等 ≤ 10s）。

**期望**：该网口在等待窗口内从下拉中消失（Windows）；此时若它恰是当前选中项且服务**未运行**，选中项回落到 `0.0.0.0` 并产生一条明确的日志。
**失败判据**：等待 30s 后下拉仍显示该网口；或列表变空（只剩 0.0.0.0）；或界面卡顿（枚举阻塞主线程）。

### 6.4 手工验收用例 D —— 服务运行中拔网线（P0，team-lead 点名的隐患）

**步骤**
1. `监听接口` 选**具体网卡 IP**（如 `192.168.102.161`），`端口` 用高位空闲口（如 2121），启动服务 → 记下界面「已监听 192.168.102.161:2121」。
2. **保持服务运行**，拔网线 / 禁用该网卡。
3. 观察下拉选中项、日志、以及界面显示的监听地址；再打开 DevTools 看 `localStorage["ftp-toolbox:server:ftp"].iface`。

**期望（安全行为）**
- 下拉选中项**不得**被静默改成 `0.0.0.0`；`localStorage` 里的 `iface` **不得**被改写（可提示「原接口已失效」但保留用户原选择/给出显式确认）。
- 界面「已监听 …」必须与后端真实绑定地址一致（后端 `status.localAddr` 为准），**不得谎报**为 `0.0.0.0`。

**失败判据（任一即 FAIL）**
- `localStorage.iface` 变为 `"0.0.0.0"`（持久化被污染）；
- 下拉值静默变为 `0.0.0.0` 且无任何提示；
- 界面显示「已监听 0.0.0.0:2121」而实际仍绑 `192.168.102.161`。

**当前实测**：无轮询时，手点一次「刷新」即触发 `ServersView.tsx:156-166` 的改写 → **本用例当前 FAIL**；加轮询后若不修，将**自动 FAIL**。

### 6.5 自动化测试设计

#### (A) 能进 `crates/ftp-core/` 的（引擎层纯逻辑，秒级）

**A1. 建议把「hint 选择」抽成纯函数**（与 `passive.rs` 同款约定：纯逻辑在引擎、OS 调用在壳层）：

```rust
// 建议签名（示意）
pub fn bind_hint(kind: ErrorKind, raw_os_error: Option<i32>, in_reserved: bool) -> &'static str
```

需要把 `error.rs:64-83` 里对 `kind()` 的分支改为可注入 `raw_os_error`/`in_reserved`，`Error::bind` 只负责调用它。理由：当前的误诊**只有把「10013 且不在保留段」这一组合喂进纯函数才能被自动测到**。

建议用例名与断言点（放进 `error.rs` 的 `#[cfg(test)]` 或新 `src/net.rs`）：
| 用例名 | 输入 | 断言 |
|---|---|---|
| `win_10013_not_reserved_points_to_netstat` | kind=PermissionDenied, raw=10013, in_reserved=false | hint 含 `netstat`/「占用」，**不含**「保留段」作为唯一解释 |
| `win_10013_in_reserved_points_to_excludedportrange` | raw=10013, in_reserved=true | hint 含 `excludedportrange` |
| `win_10048_is_addrinuse` | kind=AddrInUse, raw=10048 | hint 含「端口已被占用」 |
| `win_10049_addr_not_available` | kind=AddrNotAvailable, raw=10049 | hint 含「不属于本机/网卡已断开」 |
| `unix_eacces_privileged_port_mentions_root` | kind=PermissionDenied, port<1024, unix | hint 含 `root`/`CAP_NET_BIND_SERVICE`，**不含** `excludedportrange` |
| `display_includes_addr_and_source` | 任意 | `to_string()` 含 「无法绑定监听地址」+ addr + os error 文本 |

**A2. 集成用例（引擎层，起真实 socket）** —— 新增 `crates/ftp-core/tests/ftp_bind_errors.rs`：
| 用例名 | 断言 |
|---|---|
| `wildcard_exclusive_holder_makes_specific_bind_10013`（`#[cfg(windows)]`） | 先用 `socket2`+`windows-sys` 在 `0.0.0.0:P` 起带 `SO_EXCLUSIVEADDRUSE` 的监听；`start_server("127.0.0.1:P")` 返回 `Error::Bind`，且 `Display` **提到占用/netstat**、不把原因说成保留段 |
| `same_addr_conflict_is_addrinuse` | 同址 blocker + `start_server` 同址 → hint 为「端口已被占用」 |

> **现状盲区（重要）**：现有 `tests/ftp_server_lifecycle.rs:99` 的 `port_conflict_returns_readable_error` 用 `TcpListener::bind("127.0.0.1:0")` 做 blocker，只覆盖了**同址**冲突（10048），**因此它一直绿灯，却掩盖了用户实际命中的 10013/独占场景**。→ 该用例给人**虚假信心**，必须补 A2。

**A3. 「网卡链路状态过滤」建议做成引擎层纯函数**：

```rust
pub struct InterfaceInfo { pub name: String, pub desc: String,
    pub ip: std::net::IpAddr, pub link_up: Option<bool> /* None=平台不提供 */ }
pub fn selectable(list: &[InterfaceInfo]) -> Vec<InterfaceInfo>
```
`link_up: Option<bool>`：Windows 填 `Some(oper_up)`，Linux 若拿不到填 `None`（**不过滤**）。建议用例：
| 用例名 | 断言 |
|---|---|
| `down_adapter_hidden_when_link_known` | `link_up=Some(false)` 被移除 |
| `link_local_v4_hidden` | 169.254.x 被移除 |
| `loopback_not_duplicated` | 回环不由自动列表重复产出 |
| `unknown_link_is_kept` | `link_up=None` **保留**（保护 Linux fallback 不被误过滤） |
| `all_filtered_falls_back_to_unfiltered` | 若过滤后为空，返回未过滤列表（避免下拉变空） |

**我的架构意见（team-lead 专门问的）**：**应该**把「hint 选择」和「网卡可选性判定」都下沉到 `ftp-core` 做成纯函数，**与 `passive.rs` 的既有约定一致**（纯解析/比对在引擎、`netsh` 调用在壳层）。收益：(1) 秒级可测、无需链 GUI；(2) 本次「10013 误诊」这类缺陷能被纯函数单测直接钉死；(3) Linux 过滤逻辑（`link_up=None` 的保护）可在任何平台单测。**OS 枚举本身**（`ipconfig`/`get_if_addrs`/`netsh`/`/sys`）必须留在壳层。

#### (B) 只能留壳层或手动的
- 真实 `ipconfig::get_adapters()` 枚举、`netsh` 调用（依赖 OS 与全局状态）→ 手工或壳层 smoke。
- UI 轮询定时器、拔网线后 React 的渲染/持久化行为 → 组件测试（当前仓库无 JS 测试框架）或**手工用例 C/D**。
- 运行中拔线的真实时延（Windows NDIS 翻转时间、Linux carrier 时延）→ **只能手工**，且需物理拔线。

### 6.6 回归风险清单（这两处改动最可能碰坏什么）

| # | 风险 | 触发路径 | 建议 |
|---|---|---|---|
| **R1** | 加轮询后，`ServersView.tsx:156-166` 在**服务运行中**自动触发，把 `prefs.iface` 静默改成 `0.0.0.0` 并写回 localStorage，同时服务器仍绑旧 IP | 轮询 → `interfaces` 变化 → effect | effect 增加 `!running` 守卫；运行时只提示、不改写、不持久化 |
| **R2** | 下拉**闪动/清空**：每轮 `setInterfaces(新数组)` 触发重渲染；偶发枚举失败返回空数组时 `<select>` 值无匹配项显示空白 | 轮询 + 枚举偶发失败/空 | 空结果**不覆盖**上一次非空结果；更新前做内容 diff（相同则保持引用） |
| **R3** | `prefs.iface` 持久化被污染（与 R1 同源，独立记录以便回归时单独核对） | 同上 | 同 R1；并加一条「运行中拔线后 localStorage 不变」的回归断言 |
| **R4** | 给 `enumerate_interfaces_fallback` 加过滤后，**在 Linux 上把用户所有网卡过滤掉**（虚拟网卡/网桥/VPN 的 operstate 常为 `unknown`） | Linux/非 Windows 过滤 | 用 `link_up=None` 不过滤；过滤后为空则回退未过滤；仅当明确 `false` 才隐藏 |
| **R5** | `ipconfig::get_adapters()`/`get_if_addrs()` 是**阻塞**调用（实测 3-4ms），在 async 命令/定时器里直接调用会占住 runtime 工作线程 | 轮询实现 | 用 `tokio::task::spawn_blocking`，或像现有 `refreshAll` 一样在命令层调用并 `await` |
| **R6** | 轮询与手动刷新/启停后的 `onRefresh` **并发**，响应乱序导致列表回退到旧值 | 多路 `loadInterfaces` | 加请求代次计数（generation）或取消旧请求，只接受最新一次结果 |
| **R7** | 停止服务期间恰好刷新 → iface 被改写，导致下次启动用错接口 | 停止 + 轮询 | 同 R1 的 `!running` 守卫；或「仅在用户改选时写 localStorage」 |

---

## 7. 遗留问题与不确定性

### 7.1 本机无法证实 / 只能原理推断的
1. **Linux 侧全部行为**（<1024 特权、拔网线后地址是否保留、NetworkManager 时延、fallback 过滤后的表现）——本机为 Windows，**未在 Linux 实测**，结论均为原理推断。
2. **拔网线→`OperStatus` 翻转的时延**：未物理插拔，无法给出「可接受时间」的实测数字。可在用例 C 中测量。
3. **WFP / 安全软件 / 组策略导致的 10013**：本机未观测到，无法证实；只能列为候选。
4. **50000-50059 的保留段可绑定**（run2 Part A）：`netsh` 标 `*`（managed）的段却**绑得成功**，非 `*` 段（28385/28390/50131）才被 10013 拒绝。这与 `lib.rs:389-392` / `passive.rs` 注释里「落在保留段的那部分会直接失败」的**前提不完全一致**。可能是排除段随时间变化（Hyper-V 动态预留）或 `*` 语义差异。**→ 这是对既有「被动端口预检」功能前提的一个存疑点，建议单独复核**（不在本次两 bug 范围内，但会影响该功能的可信度）。

### 7.2 需要用户补充的信息（3-6 条，按优先级）

> 本机**已能复现**该 10013（不是「复现不出」的情形），但为确认用户那一台是否同因，请补充：

1. **出错时你选的监听接口**：是「所有接口 (0.0.0.0)」还是下拉里的**具体 IP**？错误里写的是 `127.0.0.1:21`，说明选的是「本机回环」那条——请确认。（若选 0.0.0.0，则错误码应为 10048 而非 10013，这本身就是个鉴别点。）
2. **那台机器上还有别的 FTP/占用 21 的进程吗？** 请在**出错的那台机器**上运行并回传原始输出：
   - `netstat -ano | findstr :21`
   - `tasklist | findstr /i ftp`
   （把 `LISTENING` 的 PID 与进程名发来——本机是 `filezilla-server.exe` / PID 6404。）
3. **21 是否在保留段？** 在出错机器上（普通用户即可）运行 `netsh int ipv4 show excludedportrange protocol=tcp`，把**整段**发来。
4. **是否配置过 portproxy / 装过 Hyper-V·WSL2·Docker？** 运行 `netsh interface portproxy show all`；Hyper-V/WSL2 会动态预留端口段。
5. **必现还是偶发？** 每次点启动都失败，还是时好时坏（偶发更偏向保留段/动态预留或安全软件干预）。
6. **那台机器的 Windows 版本、是否域内机**（组策略/EDR 可能注入 WFP 过滤，产生 10013）。

---

## 8. 附录：复现方式

```bash
# 探针（未污染仓库，独立 target 目录）
cd /c/Users/22534/.workbuddy/build/qa-probe
/c/Users/22534/.cargo/bin/cargo.exe run --release     # 输出见 qa-probe-run*.log

# 平台数据
netsh.exe int ipv4 show excludedportrange protocol=tcp
netsh.exe int ipv4 show dynamicport tcp
netsh.exe interface portproxy show all
netstat.exe -ano | findstr :21
tasklist.exe /svc /fi "PID eq 6404"
```

关键证据索引：
- `crates/ftp-core/src/error.rs:36`（Display）、`:64-83`（hint 分支）
- `crates/ftp-core/src/ftp/server.rs:211`（bind 在返回前）
- `app/src-tauri/src/lib.rs:63-71`（进程内查重）、`:87-89`（await）、`:627-675`（Windows 过滤）、`:683-697`（fallback 无过滤）
- `app/ui/src/views/ServersView.tsx:156-166`（iface 改写隐患）、`:402-405`（挂载只拉一次）
- 探针日志：`C:\Users\22534\.workbuddy\build\qa-probe-run{,2,3,4}.log`
