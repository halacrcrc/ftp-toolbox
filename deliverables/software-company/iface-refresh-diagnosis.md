# 诊断报告：「监听接口」下拉不随网线插拔刷新

- 版本：ftp-toolbox **v0.2.2**
- 现象：插上网线后该网口出现在「监听接口」下拉里；**拔掉网线后该网口仍留在下拉里**，下拉未及时更新。
- 诊断主机：Windows（`thinkbook14plus\22534`）—— 本机恰有一块「以太网 2」正处于 **ifOperStatusDown 但保留着 `192.168.102.161`** 的状态，正好复现该现象。
- 纪律：**未修改仓库任何源码**；探针在 `C:\Users\22534\.workbuddy\build\iface-probe`。

---

## 1. 现象 → 代码证据 → 成因

### 1.1 直接原因：后端是「拉取式」，前端只拉一次

| 事实 | 证据（文件:行） |
|---|---|
| 后端**只提供**一个拉取命令 `list_interfaces`，**没有任何推送** | `app/src-tauri/src/lib.rs:616-618`（`list_interfaces` → `enumerate_interfaces()`）；整个 `lib.rs` 里没有网卡相关 `emit` |
| 前端 `ServersView` **只在挂载时拉一次** | `app/ui/src/views/ServersView.tsx:402-405`：`useEffect(() => { void loadInterfaces(); }, [])` —— 依赖数组为 `[]` |
| 重新拉取**只发生在**用户点卡片上的「刷新」按钮 | `ServersView.tsx:408-410`（`refreshAll = Promise.all([refresh(), loadInterfaces()])`）← 由 `ServersView.tsx:244-246` 的按钮触发 |
| **没有**任何轮询 / 系统网络事件订阅 / `focus` / `visibilitychange` 监听 | 全仓检索 `setInterval` / `visibilitychange` / `NotifyIpInterfaceChange`：**均无** |

> **成因一句话**：后端每次调用都能给出「当前正确的列表」，但**前端从不主动再调用**（挂载一次后就再也不拉），所以拔线后下拉保持旧快照。

### 1.2 两个**极易混淆**的点，必须分开说

**(a) 「后端确实按链路状态过滤」≠「界面会自动重新拉取」**

后端枚举时**确实**按 `OperStatus::IfOperStatusUp` 过滤（`lib.rs:640`）。本机实测证明这条过滤是生效的：
```
=== raw adapters (ALL, no filtering) ===
  name={299671A0-...} friendly=以太网 2      status=IfOperStatusDown if_type=EthernetCsmacd desc=Intel(R) Ethernet Connection (16) I219-V
      ip 192.168.102.161                 ← 下行网卡，但仍保留了可路由地址
  ...
=== picker list via ipconfig path (enumerate_interfaces) ===
  WLAN （10.57.25.225）Intel(R) Wi-Fi 6 AX201 160MHz
  VMware Network Adapter VMnet8 （192.168.136.1）
  VMware Network Adapter VMnet1 （192.168.9.1）
  本机回环 （127.0.0.1）                ← 「以太网 2」被正确过滤掉了
```
> 所以：**只要前端再拉一次，Windows 上下拉就会立刻正确**。问题**纯粹**在前端的刷新时机，**不在**后端过滤逻辑（Windows 路径）。

**(b) 「切页离开再回来会刷新」≠「停在当前页会刷新」**

`App.tsx:139-146` 用条件渲染：
```tsx
{view === "servers" && (<ServersView log={log} ... />)}
```
- **离开**「服务器」页 → `view` 变化 → `ServersView` **被卸载**；
- **回来** → `ServersView` **重新挂载** → 那条 `useEffect(..., [])`（`ServersView.tsx:402-405`）**重新执行** → 又拉了一次网卡 → 列表就对了。
- **停在当前页** → 组件常驻、`useEffect` 不会重跑 → **永远不会**再拉 → 列表停留在挂载那一刻的快照（`interfaces` state 不更新）。

> 这解释了用户的主观感受：「有时候切来切去就好了，但一直开着就不动」。

---

## 2. 隐藏缺陷（重点）：`enumerate_interfaces_fallback()` 不过滤链路状态

`lib.rs:683-697`：
```rust
fn enumerate_interfaces_fallback() -> Vec<NetInterface> {
    get_if_addrs::get_if_addrs()        // ← 不过滤 OperStatus / IFF_UP
        .unwrap_or_default()
        .into_iter()
        .filter_map(|iface| match iface.addr { get_if_addrs::IfAddr::V4(v4) => Some(...), _ => None })
        .collect()
}
```
它被两处调用：**非 Windows 平台**（`lib.rs:678-681`），以及 **Windows 上 `ipconfig::get_adapters()` 失败时**（`lib.rs:632-635`）。

### 2.1 实测：Windows 上 fallback 会把下行网卡列进来

`get_if_addrs 0.5.3` 在本机的真实输出（探针 `iface-probe`）：
```
=== get_if_addrs (fallback / non-Windows path) ===
  name={299671A0-2B76-44F0-89FA-D07F63508136}   ip=192.168.102.161  netmask=255.255.255.255   ← 「以太网 2」，状态为 Down！
  name={12C3082E-...}   ip=10.57.25.225
  name={53FED687-...}   ip=192.168.136.1
  name={30B859A1-...}   ip=192.168.9.1
  name={9D98885A-...}   ip=127.0.0.1
  -> get_if_addrs returned 8 entries
--- fallback picker list ---
  {299671A0-2B76-44F0-89FA-D07F63508136} （192.168.102.161）   ← 下行网卡照样进下拉
  ...
```
> **即便用户点「刷新」重新拉取，走 fallback 的平台上拔掉的网卡仍会留在列表里。** 这是与 §1.1 不同层级的、独立缺陷。

同时注意 fallback 的 `name` 是**适配器 GUID**（`{299671A0-...}`），正是 `lib.rs:620-622` 注释里说「picker 以前显示 `{4B3C…}`」的老问题。

### 2.2 `get_if_addrs` 到底能/不能拿到链路状态（已核源码 + 实测）

已读 `get_if_addrs-0.5.3/src/lib.rs` 源码：

- **POSIX 路径（Linux/macOS）**（源码 209-273 行）：遍历 `getifaddrs()`，只跳过 `ifa_addr` 为空者与 `fe80::` 链路本地 IPv6。**`ifa_flags` 仅用于判断 `IFF_BROADCAST(0x2)` 以决定要不要读广播地址，从不据 `IFF_UP / IFF_RUNNING` 过滤**。⇒ **Linux/macOS 上下行的网卡同样会被列出。**
- **Windows 路径**（源码 385-551 行）：`GetAdaptersAddresses` 传 flags `0x3e`（`SKIP_ANYCAST|SKIP_MULTICAST|SKIP_DNS_SERVER|INCLUDE_PREFIX|SKIP_FRIENDLY_NAME`）。**`IpAdapterAddresses` 里虽有 `oper_status` 字段，但该 crate 根本没有读取它**；只按 IP 过滤 `169.254.x.x` 与 `fe80::`。⇒ 只要下行网卡**保留着非链路本地地址**（如本机 `192.168.102.161`），就会**被列出**。

> 所以 `get_if_addrs` **在两平台上都拿不到链路的 Up/Down 语义**。要判链路状态，必须换枚举方式。

### 2.3 可行替代

| 平台 | 方案 |
|---|---|
| **Windows** | 保持 `ipconfig::get_adapters()`（它读 `OperStatus`，本机已验证有效）。**不要**在 ipconfig 成功时依赖 fallback；fallback 只作最后兜底，并在其中**显式标注「无法判断链路状态」**，UI 上据此做提示而非静默展示 |
| **Linux** | 每个接口读 `/sys/class/net/<if>/operstate`（`up`/`down`/`unknown`）与 `/sys/class/net/<if>/carrier`（`1`/`0`）。`carrier` 直接反映物理链路（网线），`operstate` 反映整体操作状态。用 `get_if_addrs` 拿到接口名与地址，再用 sysfs 过滤 |
| **通用** | 换用能暴露 flags 的枚举：用 `libc::getifaddrs` 自己读 `ifa_flags`（判 `IFF_UP`、`IFF_RUNNING`）；或 Windows 直接用 `windows` crate 的 `GetAdaptersAddresses`（`oper_status`）/`NotifyIpInterfaceChange`。也可用 `pnet`/`netdev` 等库 |

> 建议：把「链路是否可用」抽成一个**纯函数** `fn link_is_up(name, family) -> bool`（Windows 走 OperStatus；Linux 走 `/sys`；其它给 `None` 表示未知），与 `ftp_core::ftp::passive` 一样可单测。

---

## 3. 实时刷新方案（按成本递增的阶梯）

**先给成本基线（本机实测，30 次，`iface-probe`）：**
```
TIMING ipconfig::get_adapters()           iters=30 min=3.45ms p50=4.32ms avg=9.32ms max=81.82ms
TIMING enumerate_interfaces() (ipconfig)  iters=30 min=3.38ms p50=4.15ms avg=6.95ms max=37.37ms
TIMING get_if_addrs::get_if_addrs()       iters=30 min=2.26ms p50=2.70ms avg=5.02ms max=31.14ms
```
> 一次枚举 **典型 ~4ms，偶发尖峰 ~40-80ms**（首次/缓存失效时会触到 Syscall 开销）。这意味着**低频轮询成本几乎可忽略**。

### 档位 A（最小改动）—— **推荐作为本次先落地的档位**
在 `ServersView` 挂载期间：① 定时轮询；② `focus` / `visibilitychange` 触发即时补拉；③ 卸载时清理。

```tsx
// ServersView.tsx —— 示意（未落地）
useEffect(() => {
  void loadInterfaces();
  const onFocus = () => void loadInterfaces();
  window.addEventListener("focus", onFocus);
  document.addEventListener("visibilitychange", onFocus);
  // 5s：一次 ~4ms，占比 ~0.08%；即使尖峰 80ms 也只有 ~1.6%，可接受
  const id = window.setInterval(() => {
    if (document.visibilityState === "visible") void loadInterfaces();
  }, 5000);
  return () => {
    window.clearInterval(id);
    window.removeEventListener("focus", onFocus);
    document.removeEventListener("visibilitychange", onFocus);
  };
}, []);
```
- **周期取 5s**：拔网线到界面更新 ≤5s，用户感知「近乎实时」；成本可忽略；窗口不可见时不拉，进一步省。
- **必须清理**：`clearInterval` + `removeEventListener`，否则切页（组件卸载，见 §1.2b）会泄漏定时器。
- 先用档位 A 覆盖「拔线 → 下拉更新」这一**用户可见痛点**；档位 B/C 作为体验优化。

### 档位 B（Windows 推送，彻底消除轮询延迟）
用 `windows` crate 的 IP Helper 通知：
- 主：`NotifyIpInterfaceChange`（接口 up/down/新增/删除）
- 备（地址变化，如 DHCP 换 IP）：`NotifyUnicastIpAddressChange`
- 回调在**系统线程**触发（非 UI 线程）→ 在回调里只做「打标/发信号」，实际枚举放到 `tauri::async_runtime::spawn`，再 `app.emit("interfaces-changed", ())`；前端 `listen("interfaces-changed")` 后 `loadInterfaces()`。

**Cargo.toml 片段**（体积影响：`windows` crate 按 feature 裁剪，新增这两个 API 会引入 `Win32_NetworkManagement_IpHelper` + `Win32_Networking_WinSock` + `Win32_Foundation`，通常几十 KB 级增量；注意 app 里已有依赖，**统一版本号避免重复编译**）：
```toml
[dependencies]
windows = { version = "0.58", features = [
  "Win32_Foundation",
  "Win32_Networking_WinSock",
  "Win32_NetworkManagement_IpHelper",
] }
```
**生命周期/清理要点**：
- `NotifyIpInterfaceChange` 返回一个 `HANDLE`（注册句柄），**必须**在应用退出/卸载监听时用 `CancelMibChangeNotify2(handle)` 注销，否则回调可能打到已释放的上下文。
- 回调上下文传 `AppHandle` 的克隆要用 `Box::leak` 或 `Arc`，并保证在 `CancelMibChangeNotify2` **之后**才释放——否则存在「回调已在途、上下文已 drop」的悬垂。
- 回调线程不是 Tauri 的主线程，`emit` 是线程安全的；但**不要**在回调里直接做重活（枚举），只发信号。
- 跨平台：Linux 用 **`netlink`（`RTMGRP_LINK`）** 监听链路事件，macOS 用 **SystemConfiguration / `SCDynamicStore`**（`SCDynamicStoreSetNotificationKeys`）。故档位 B 平台各写一份。

### 档位 C（工程化：去抖 + 指纹去重，避免下拉闪动）
1. **后端缓存快照**进 `AppState`：枚举结果存一份；仅在**与上一份不同**时才 `emit("interfaces-changed")`（变化才推）。这避免「无变化也刷 UI」。
2. **前端指纹比较**：`JSON.stringify` / 或 `name+ip` 排序后拼串做指纹；与上一份相同则**不 setState**，React 不重渲染，**下拉不闪**。
3. **保持用户选中项**：`<select value={prefs.iface}>` 在 `interfaces` 重建后仍以 `prefs.iface` 为受控值；只要选项 value（IP）还在，选中项不丢。

> **推荐档位**：**A 必做**（解决用户痛点、改动小、无新依赖）；**B 可选**（当用户反馈「拔线后仍要等几秒」时再上，Windows 优先）；**C 与 A/B 叠加**（其实 C 的第 3 点是修复 §4 一致性陷阱的前提，建议与 A 一起做）。

---

## 4. 必须处理的一致性陷阱（重点）

### 4.1 陷阱：轮询会让「运行中被拔线」的界面**说谎**

现状 `ServersView.tsx:156-166`：
```tsx
useEffect(() => {
  if (prefs.iface === "0.0.0.0" || interfaces.length === 0) return;
  if (!interfaces.some((it) => it.ip === prefs.iface)) {
    log(`原监听接口 ${prefs.iface} 已不在网卡列表中（IP 可能已变化），已回落到「所有接口 (0.0.0.0)」`, "error");
    set("iface", "0.0.0.0");   // ← 静默改写
  }
}, [interfaces, prefs.iface]);
```
而 `prefs` 会被持久化（`ServersView.tsx:115-117` → `localStorage.setItem`）。

**加轮询后的问题链**：服务**正在运行** → 用户拔线 → 轮询拿到不含该 IP 的列表 → `interfaces` 变化 → 上面这个 effect 触发 → **把 `prefs.iface` 改写成 `0.0.0.0` 并写回 localStorage**。但**后端服务器其实仍然绑在那个已经消失的 IP 上**（`ServerStatus.localAddr` 没变）。结果：**界面在撒谎**——下拉显示 `0.0.0.0`，实际监听的是已消失的地址；而且**用户的偏好被污染**了，下次打开看到的是 `0.0.0.0` 而不是他原本选的接口。

**正确行为（建议）**：
- **运行中（`status.running === true`）：只告警，绝不改写 `prefs.iface`。**
- 下拉里为这条消失的地址**保留一个合成选项**，文案标「（已掉线）」，值为原 IP；选择它时**禁用「启动服务」**，并提示「该接口已不在，需先停止并重新选择」。
- **仅当服务已停止**，且用户**主动**确认（或提供一键回落按钮）时，才允许把 `prefs.iface` 回落到 `0.0.0.0`。
- 更好：**停止服务后**只在下拉中把该项标灰/标注，让用户**自己**决定，而不是自动改写。

**关于「是否还应持久化改写用户偏好」——我的结论：不应该。**
> 理由：`prefs.iface` 表达的是**用户的意图**（我想监听哪个接口）；而「该接口此刻是否在线」是**运行时状态**。两者混写会把「用户偏好」永久降级为「某个瞬间的机器状态」——网线重插后，用户原本选的具体接口**再也回不来**了（已被写成 `0.0.0.0`）。**偏好不应被后端状态污染**：运行时状态用 `ServerStatus`/`interfaces` 表达，`prefs.iface` 保持用户选择不变。**取舍**：代价是下拉里可能出现一个「（已掉线）」的旧选项，需要 UI 明确标注并禁用启动——这是**显式**的代价，优于**静默**地篡改用户偏好。

### 4.2 陷阱：轮询会造成日志刷屏
现状每次回落都 `log(..., "error")`（`ServersView.tsx:159-162`）。加了 5s 轮询后，只要 `prefs.iface` 一直不在列表里，**每 5s 就写一条 error**，且该 effect 依赖 `[interfaces, prefs.iface]`，而 `interfaces` 每次轮询都是**新数组**（`setInterfaces(await ...)` 产生新引用）→ **即使内容一模一样也会触发 effect** → **每轮都刷一条**。

**幂等/去重方案**：
1. **列表指纹去重**（与 §3 档位 C 同）：`loadInterfaces` 里比较新旧指纹，相同则**不 setState**。这样 `interfaces` 引用不变 → 下游 effect 不再空跑。**这是最根本的一刀。**
2. **告警去重**：把「已掉线告警」做成**边沿触发**——只在「从在线变掉线」那一次打日志（用一个 `useRef` 记住上次是否已告警），持续掉线期间不再重复。
3. 级别调整：`error` → `warn`（它不是错误，是可恢复的状态），并把「已回落到 0.0.0.0」的措辞改为「检测到接口掉线（未改动你的选择）」。

---

## 5. 跨平台差异：为什么「只看 up」在两平台表现不一致

### 5.1 拔网线后，两平台的行为不同

| 平台 | 拔线后的表现 | 后果 |
|---|---|---|
| **Windows** | NDIS 会把适配器置为 **Media disconnected**；DHCP 地址被**迅速收回**；`GetAdaptersAddresses` 的 `OperStatus` 变 `IfOperStatusDown` | 重新枚举即正确（本机实测：`以太网 2` 已是 Down，但仍**短暂保留** `192.168.102.161` —— 这正是「刚拔线时它还在列表里」的窗口） |
| **Linux** | 网线拔掉后地址**可能仍保留数秒到数分钟**，取决于 NetworkManager / `dhclient` / `systemd-networkd` 的策略；`carrier=0` 但 `IFF_UP` 可能仍为 `true` | 「只看 `IFF_UP`」会**误判为在线** |
| **Linux WLAN** | **Wi-Fi 未关联**时，`IFF_RUNNING` 可能仍为 `true`（`IFF_UP` 也常为 `true`），只是没有连接 | 只按 up 状态过滤会**把未联网的 Wi-Fi 当成可用接口** |

### 5.2 结论与产品处理
- **「只看 up」在两平台语义不对等**：Windows 的 `OperStatus=Up` 更接近「链路真的可用」；Linux 的 `IFF_UP` 只是「接口被启用」，与「有链路/有地址」不是一回事。
- **建议**：
  - **Linux**：优先看 **`carrier`**（物理链路）**且**有**可用地址**（非链路本地）；`operstate` 作辅助。**不要**只信 `IFF_UP`。
  - **Windows**：用 `OperStatus`（现状已对）**并**结合「是否有非链路本地地址」。
  - **WLAN 特殊处理**：若判定为「接口 up 但未关联/无有效地址」，在 UI 上标注「（未连接）」，**不默认推荐**，但也**不粗暴隐藏**（用户可能就要先用 `0.0.0.0`）。
  - **统一兜底**：凡「地址仍在但链路已 down」的接口，一律用 §4.1 的「（已掉线）」合成选项处理，**既不静默删除也不静默改写偏好**，把判断权交给用户。

---

## 6. 未验证 / 不确定项

- **Linux/macOS 平台行为未在本机执行验证**（无可用 Linux 环境）。§2.2 中 `get_if_addrs` 的行为是**读源码**得出的（Windows 路径**已由本机实测印证**：下行网卡被列出）；§5 的 Linux「地址残留数秒至数分钟」为**已知机制**，需在目标发行版实测确认。
- **`NotifyIpInterfaceChange` 的 `windows` crate 版本/feature 名称**未编译验证（仅给出建议片段）；落地前需按所锁定的 `windows` 版本核对 feature 名。
- 本机「以太网 2」的 `192.168.102.161` 是否为残留 DHCP 地址、抑或静态配置，未进一步确认（不影响结论）。
- 未评估「档位 B 的 Linux netlink / macOS SCDynamicStore」的代码量（仅列为备选）。
