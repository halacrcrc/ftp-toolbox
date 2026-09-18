# QA 独立复核报告 — Round 2

复核者：Edward（QA）｜日期：2026-09-18｜对象：工作区未提交改动（HEAD = `8729bc1` 上一次布局修复，本轮改动全在 working tree）
立场：验证者而非复述者 —— 所有结论均由本人读代码 + 实际运行命令 + 实测 bind 得出。仓库源码未做任何持久修改（临时文件已撤回，见 §G）。

---

## 0. 结论摘要（逐条判定表）

| # | 复核项 | 判定 | 一句话证据 |
|---|--------|------|-----------|
| A-1 | `cargo test -p ftp-core` 40 用例 | **已证实** | 27+3+1+8+1 = 40 passed，0 failed |
| A-1 | `cargo check --workspace --all-targets` 零 error 零 warning | **已证实** | `cargo clean -p` 后重编译，`Finished` 前无任何 warning |
| A-2 | Taken / Forbidden / AddressUnavailable 三归因 | **已证实** | 本人独立复现三种原始错误码 + 生产 `Error::bind()` 归类 |
| A-2 | `ftp_bind_errors.rs` 是否同义反复 | **已证实（非自证）**；设计上**存疑** | 真 bind、真断言；但 Forbidden 用例存在静默跳过 |
| A-3 | 「托管保留段不阻止绑定」前提 | **已证实（本机当前状态）** | `127.0.0.1:50000/50005/50030/50059` 与 `0.0.0.0:*` 全部绑定成功，且 PASV 全落入托管段仍返回 227 |
| A-3 | libunftp PASV 绑「具体地址」的说法 | **已证实** | `pasv.rs:118` `try_port_range(args.local_addr.ip(), …)` |
| B-4 | 前端 5 条（轮询/指纹/不再改写/边沿告警/loaded） | **已证实** | 见 §B-4 |
| B-5 | ifaceMissing 渲染「（已掉线）」；loaded=false 会否空白 | 前半**已证实**；「空白」**证伪**（但有新发现的问题） | DOM dump 见 §B-5 |
| B-6 | `sort_interfaces` 无隐蔽正确性问题 | **已证实（无 bug）** | 稳定排序 + parse 恒成功；仅 O(n log n) clone |
| B-7 | `JSON.stringify` 指纹可靠 | **已证实** | serde struct 字段序固定（声明序），非 HashMap |
| B-8 | 真冲突 + 托管段同时存在时文案是否自相矛盾 | **存疑（轻微）** | 两条同时出现，措辞并列时易读混，见 §B-8 |
| C-9 | 640/800/1024 布局无横向溢出 | **已证实** | 三档截图，按钮换行、无溢出 |
| C-10 | 回归风险清单 | 见 §C-10 | —— |
| D-11 | 是否必须升版本号 | **必须升** | 见 §D-11（MSI GUID 实证） |

---

## A. 动手实测

### A-1 测试与静态检查

`cargo test -p ftp-core`（以输出 `test result` 行为准）：

```
running 27 tests   (unittests src\lib.rs)
test result: ok. 27 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

running 3 tests    (tests\ftp_bind_errors.rs)
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s

running 1 test     (tests\ftp_data_channel.rs)
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s

running 8 tests    (tests\ftp_server_lifecycle.rs)
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

running 1 test     (tests\tftp_loopback.rs)
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s
```
合计 **40 passed / 0 failed**，与 team-lead 说法一致。

`cargo check --workspace --all-targets`：为排除增量缓存掩盖 warning，先 `cargo clean -p ftp-core -p ftp-toolbox-app` 再全量检查，输出仅：
```
Checking ftp-core v0.2.2 (...)
Compiling ftp-toolbox-app v0.2.2 (...)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 20.92s
```
**零 error、零 warning，已证实。**

### A-2 三种归因的独立复现

本机 `netsh int ipv4 show excludedportrange protocol=tcp` 实测：
```
     28385       28385          (普通)
     28390       28390          (普通)
     50000       50059     *    (托管)
     50131       50131          (普通)
```
本人写了一个临时集成测试（`tests/zz_qa_probe.rs`，用后已删除），直接调用生产 `ftp_core::Error::bind()`，实测：

```
held 127.0.0.1:44784          -> Some(Taken)
plain 127.0.0.1:28385         -> Some(Forbidden)
   渲染: 无法绑定监听地址 127.0.0.1:28385：... (os error 10013)（系统不允许绑定端口 28385，且它并没有被别的程序占用：... netsh int ipv4 show excludedportrange ...）
192.0.2.123                   -> Some(AddressUnavailable)
```

**更重要的是探针分支（`PermissionDenied` → 再探通配地址 → `AddrInUse` → Taken）被我端到端复现了** —— 用一个设置了 `SO_EXCLUSIVEADDRUSE` 的持有者占住 `0.0.0.0:<port>`（FileZilla 占 21 的同款场景），再 bind 具体地址：

```
==== E. exclusive wildcard holder on 0.0.0.0:44902 ====
   bind 127.0.0.1:44902 -> raw=Some(10013) kind=PermissionDenied
   Error::bind(127.0.0.1:44902) cause = Some(Taken)
   wildcard probe -> raw=Some(10048)
```
即 `10013 → 探测得 10048 → Taken` 这条最关键的归因链**是真的、可复现的**，不是「断言 A 等于 A」。

**对 `ftp_bind_errors.rs` 的判断：**
- `a_port_held_by_another_listener_is_reported_as_taken`：真起监听者、真走 `start_server`、真断言成因 = Taken。**真实测试（非自证）**。注意它命中的是「直接 `AddrInUse`」分支，不是探针分支；探针分支由 `error::tests::a_failed_wildcard_probe_means_the_port_is_actually_taken`（合成错误）覆盖，而**探针行为本身我已实测确认**。
- `an_address_that_is_not_local_is_reported_as_unavailable`：真 bind `192.0.2.123` → 10049。**真实**。
- `a_plain_reserved_band_is_reported_as_forbidden`：真读 netsh、真 bind、真断言 Forbidden。本机存在普通段（28385/28390/50131），**本机确实执行了断言**，我独立复现为 Forbidden。

**关于「找不到普通排除段就提前 return」的静默跳过 —— 我的判断：设计上不可接受，但风险可控，建议改。**
- 问题：`let Some(plain) = ... else { println!(...); return; }` 会让用例在「没有普通段」的机器上**以 `ok` 通过但什么都没断言** —— CI 里的绿与「真跑过了」无法区分（`println!` 只有 `--nocapture` 才可见）。
- 缓解：纯函数层已有 `a_plain_exclusion_is_still_a_real_conflict` 用逐字 netsh 表覆盖同一分类逻辑，所以不是「零覆盖」。
- 建议：把跳过做成显式信号（例如：找不到时 `panic!` 并要求测试环境提供普通段，或拆成「合成错误分类单测」+「环境相关冒烟」两层，冒烟失败只警告不绿）。当前写法属**存疑**，但不阻塞发布。

### A-3 【本次最有价值】对抗性复核「托管保留段不阻止绑定」

**实测原始数据（临时测试 `probe_raw_bind_matrix`）：**
```
-- port 50000 (托管*) --   127.0.0.1:50000 OK -> 127.0.0.1:50000   0.0.0.0:50000 OK -> 0.0.0.0:50000
-- port 50005 (托管*) --   127.0.0.1:50005 OK                      0.0.0.0:50005 OK
-- port 50030 (托管*) --   127.0.0.1:50030 OK                      0.0.0.0:50030 OK
-- port 50059 (托管*) --   127.0.0.1:50059 OK                      0.0.0.0:50059 OK
-- port 28385 (普通)  --   127.0.0.1:28385 ERR 10013 PermissionDenied   0.0.0.0:28385 ERR 10013
-- port 28390 (普通)  --   127.0.0.1:28390 ERR 10013 PermissionDenied   0.0.0.0:28390 ERR 10013
-- port 50131 (普通)  --   127.0.0.1:50131 ERR 10013 PermissionDenied   0.0.0.0:50131 ERR 10013
-- port 45999 (空闲)  --   OK / OK
==== B. 非本机地址 ====      192.0.2.123:50005 ERR raw=Some(10049) AddrNotAvailable
```

**端到端 PASV（把被动段强制设为托管段 50000..50006，全部落在 `50000-50059*` 内）：**
```
==== D. end-to-end PASV fully inside managed 50000-50059 ====
   control listening on 127.0.0.1:44785
   greeting: 220 Welcome to ftp-toolbox
   USER anonymous -> 331 Password Required
   PASS anonymous@ -> 230 User logged in, proceed
   PASV -> 227 Entering Passive Mode (127,0,0,1,195,80)
   managed-band PASV result: SUCCESS (227)
```

**结论：托管段（本机 `50000-50059*`）当前不阻止绑定 —— 不仅不阻止「绑具体地址」，连 `0.0.0.0` 通配也放行；本轮把托管段告警从「⚠ 真冲突」降级为「提示」是**正确的**，team-lead 的前提成立。**

两个必须写进结论的限定：
1. **这是「当前状态」结论，不是永恒事实。** 托管排除是 Hyper-V/WSL2/winnat 申请的动态预约，其是否真正生效随版本与 `winnat` 运行状态变化。代码注释已写「version- and winnat-state-dependent，只报 'probably fine' 而非 'safe'」，用词恰当。若要更稳，建议**在运行时对托管段做一次真实 bind 探测**再决定措辞（现在纯靠「带 `*`」这一静态标记推断）。不阻塞发布。
2. **「PASV 绑具体地址」在本实验条件下等价成立。** `libunftp-0.20.3/.../pasv.rs:118` 为 `Pasv::try_port_range(args.local_addr.ip(), args.passive_ports)`，`args.local_addr` 是控制连接的本机地址。所以：
   - 服务器监听 `0.0.0.0:21`、客户端从 `192.168.x.x` 连到本机 LAN IP 时，被 accept 的控制 socket 的 `local_addr` = **该 LAN 具体 IP**（非 0.0.0.0），PASV 就绑这个具体 IP。
   - 本机自测（客户端连 `127.0.0.1`）时它绑 `127.0.0.1`。
   - 两种情况我实测都通过，因此「绑具体地址」说法成立；且本机托管段连通配都放行，结论更宽。
   - 另注：`pasv.rs:59` 调了 `s.set_reuseaddr(true)`，Windows 上 `SO_REUSEADDR` 语义与 Unix 不同（允许抢占已绑定地址），这进一步降低撞段概率 —— 也意味着**「Taken」探针若用在 PASV 端口上会失真**（控制端口上不设此选项，故探针用于控制 bind 是成立的；`probe_wildcard` 注释已声明用于 `Error::bind`）。

---

## B. 读代码核实（文件:行）

### B-4 前端 5 条 —— 全部**已证实**
- **轮询周期与清理**：`ServersView.tsx:458` `INTERFACE_POLL_MS = 3000`；`:493-509` effect：挂载即拉一次 → `setInterval`（回调里 `document.visibilityState === "hidden"` 则跳过）→ 监听 `window focus` 与 `document visibilitychange` 即时补拉 → cleanup `clearInterval` + 两个 `removeEventListener`。`loadInterfaces` 用 `useCallback([log])`，`log` 由 `App.tsx:47 useCallback(..., [])` 提供（稳定），故该 effect 只在挂载/卸载各跑一次，不会随渲染反复重建定时器。**已证实。**
- **指纹去重**：`:469` `lastSnapshot` ref；`:477-481` `JSON.stringify(next)` 与上次比较，相同则不 `setState`。**已证实。**
- **不再静默改写 `prefs.iface`**：全文件 `set("iface"` 仅两处 —— `:292`（`<select>` 的 `onChange`，用户操作）与 `:326`（「改用所有接口」按钮 `onClick`，用户操作）。**没有任何 `useEffect` 调用 `set("iface")`**；`localStorage` 仅出现在 `:21`（`getItem`，`loadPrefs`）与 `:118`（`setItem`，持久化 prefs）。旧逻辑「接口失效→改写为 0.0.0.0→写回」**已彻底移除**。**已证实。**
- **告警边沿去重**：`:176` `warnedFor` ref；`:177-190` effect：`!ifaceMissing` 时清空 ref；否则仅当 `warnedFor.current !== prefs.iface` 才 `log(...)` 并记录。轮询不会刷屏。**已证实。**
- **`interfacesLoaded` 语义**：`:465` 初始 false；`:476` 仅在 `await api.listInterfaces()` 成功后置 true（失败路径只置 `pollFailed`）；`:169-172` `ifaceMissing` 前置 `interfacesLoaded &&`。语义为「是否成功读到过」，与「列表是否为空」解耦。**已证实**（但见 B-5 的副作用）。

### B-5 「已掉线」选项渲染 & 「空白」断言 —— 前半**已证实**，后半**证伪**（附新发现）
JSX `:296`：`{ifaceMissing && <option value={prefs.iface}>{prefs.iface}（已掉线）</option>}`。用真实 Chromium（无头）渲染真实页面，`--dump-dom` 抓到 FTP 卡下拉 DOM：

成功读到列表时（注入失效 `prefs.iface=192.168.102.161`）：
```html
<select>
  <option value="0.0.0.0">所有接口 (0.0.0.0)</option>
  <option value="192.168.102.161">192.168.102.161（已掉线）</option>
  <option value="10.57.25.225">WLAN（10.57.25.225）…</option>
  <option value="192.168.136.1">VMware …（192.168.136.1）…</option>
  <option value="127.0.0.1">本机回环（127.0.0.1）…</option>
</select>
<button class="btn primary" disabled title="监听接口 192.168.102.161 已掉线，请先改用其它接口">启动服务</button>
```
**「（已掉线）」选项确实渲染，且「启动服务」被禁用。已证实。**

从未成功读到列表时（令 `list_interfaces` reject → `interfacesLoaded=false`）：
```html
<select>
  <option value="0.0.0.0">所有接口 (0.0.0.0)</option>
</select>
```
截图显示下拉**并非空白，而是显示第一项「所有接口 (0.0.0.0)」**（Chromium：`value` 无匹配 option 时回落到首项）。**故「会渲染成空白」在本机 Chromium 上证伪。**

> **⚠ 但由此发现一个新问题（本机可复现，需 team-lead 处理）：** 当 `interfacesLoaded=false`（枚举失败，或首帧尚未返回）时，下拉**显示** `0.0.0.0`，而 `prefs.iface` 实际仍是 `192.168.102.161`；此时 `ifaceMissing=false` ⇒「启动服务」**不禁用**。用户点启动，实际执行的是 `192.168.102.161:21`（见 `:205`），与界面显示不一致。这是「不再静默改写」这一正确决定的副作用（旧代码会把 prefs 改写成 0.0.0.0 从而显示一致）。
> 建议：把「选项必须存在以免 select 说谎」与「是否告警」拆开 —— 即只要 `prefs.iface !== "0.0.0.0" && !interfaces.some(...)` 就渲染该 option（无论 `interfacesLoaded`），但仅在 `interfacesLoaded` 时标注「（已掉线）」并告警。这样既不误报、又不出现显示/取值不一致。

### B-6 `sort_interfaces` —— **已证实无 bug**（`app/src-tauri/src/lib.rs:776-785`）
- `sort_by_key` 返回 `(Ipv4Addr, String)`：`Ipv4Addr` 的 `Ord` 与 `to_string()` 生成的点分十进制数值序一致（IPv4 网络字节序即大端，octet 字典序 == u32 数值序），排序确定。
- `it.ip.parse::<Ipv4Addr>().unwrap_or(UNSPECIFIED)`：`ip` 字段由 `v4.to_string()` 生成（`:687`、`:737`），**恒可解析**，`UNSPECIFIED` 回落是理论分支；即便触发，`sort_by_key` 是**稳定排序**，回落项仍按原相对序排，**不会造成不稳定**。
- 唯一代价：比较时 `it.name.clone()` 每次分配 —— 网卡个位数（本机 3~9 个），O(n log n) 次字符串分配完全可忽略。
- 覆盖缺口：`sort_interfaces` **无单测**（app crate 无 `#[cfg(test)]`），只靠集成/人工验证。建议补一个小单测（含「解析失败回落」与「同名不同 IP」两例）。

### B-7 `JSON.stringify` 指纹 —— **已证实可靠**
`NetInterface`（`:619-629`）是普通 `#[derive(serde::Serialize)]` struct，**无 `rename`、非 `HashMap`**，serde 按**声明顺序**（name, desc, ip, loopback）序列化，顺序固定；`serde_json` 不重排。JS 端 `JSON.parse` 得到的对象键序即 JSON 中出现顺序 ⇒ 每次轮询对同一列表产出**逐字相同**的字符串。再叠加后端 `sort_interfaces` 的稳定顺序，指纹不会因字段序变化而失效。**已证实**（Rust 侧 `HashMap` 随机序的隐患在此不存在）。

### B-8 真冲突 + 托管段并存的文案 —— **存疑（轻微）**（`lib.rs:121-138`）
实际拼接文案（读代码得出）：
```
...（{mode}，被动端口 {passive}）
；⚠ 被动端口 {passive} 与系统保留段（{conflicts}）重叠，PASV 可能偶发失败，建议改为 {suggested}
；提示：该段还与系统托管排除段（{managed}）重叠（Hyper-V/WSL2 常用），实测通常仍可绑定，若 PASV 偶发失败再换段即可
```
当被动段**同时**压到普通段与托管段（例如 `28300-50200` 覆盖 28385/28390 与 50000-50059）时，两条都会出现。二者指向**不同**的段，逻辑上不矛盾，但一句话说「可能偶发失败」、紧接着说「通常仍可绑定」，并列读起来容易让人以为在自我否定。前端同理会同时渲染 `.hint-line.warn` 与 `.hint-line.note`（DOM 已见）。**建议**：文案里明确两条各指哪个段（「普通段 X：会失败」/「托管段 Y：通常无碍」），或当 `conflicts` 非空时把托管段提示收进 `suggested` 说明里，避免并列。轻微，不阻塞。

另有两个**轻微一致性**问题（供参考，非本轮引入）：
- `check_passive_ports`（`lib.rs:419-439`）：`ports = lo..hi.saturating_add(1)`，当 `hi == 65535` 时区间为空 ⇒ `ok=true`；而 `start_ftp_server` 走 `passive::parse` 对 `xxx-65535` 会报错。即「检查说没事、启动却失败」的边界不一致。
- `suggest` 会同时避开托管段（保守正确），但也意味着真冲突时给的替代段可能离得很远。

---

## C. 回归与界面

### C-9 布局回归 —— **已证实无横向溢出**
方法：`vite` dev server（`:1420`，后台）+ 本机 Chrome 无头（`--headless=new --disable-gpu --hide-scrollbars --virtual-time-budget=7000`）。为渲染真实「已掉线」告警与被动端口提示，**临时**在 `index.html` 注入 Tauri IPC stub（`window.__TAURI_INTERNALS__.invoke`）并预置失效 `prefs.iface`；**用完已完整撤回**（见 §G）。截图（`C:\Users\22534\.workbuddy\build\`）：
- `r2-servers-640.png` / `r2-servers-800.png` / `r2-servers-1024.png`（正常）
- `r2-servers-faillist-1024.png`（枚举失败的变体）

观察：
- **640**：侧栏收成图标条、卡片单列；`hint-line warn with-action` 的「改用所有接口」按钮**换行到提示下方**（`flex-wrap:wrap` 生效），未被挤出容器；被动段的「改用 49900-49999」与其下的 warn/note 行均在卡片内，**无横向溢出**。
- **800**：单列；同上无溢出。
- **1024**：双列（FTP/TFTP 并排）；FTP 卡内失效接口告警、两条 passive 提示、按钮均正常收束，**无溢出**。
- `styles.css` 新增的 `.hint-line.note`（中性色，`color:var(--text-2)`）与既有 `.hint-line.warn`（`#b45309`）视觉层级区分清楚；`.hint-line.with-action{display:flex;flex-wrap:wrap;gap:4px 10px}`（`:217-222`）是按钮不溢出的关键。

**结论：新增告警行与 `.hint-line.note` 在三档视口均无横向溢出、按钮未被挤出容器。已证实。**

### C-10 回归风险清单（按可能性×影响排序）
1. **下拉顺序变化**（`sort_interfaces` 按 IP 值再按名）：旧顺序来自适配器枚举序，新顺序按数值 IP。**不会丢选中项**（`prefs.iface` 存的是 IP，`<select>` 按 value 匹配），仅用户肌肉记忆变化；「本机回环」现固定排在 `127.0.0.1` 数值位。影响低。
2. **轮询与服务运行态交互**：轮询只改 `interfaces`，不改 `prefs`/`running`（运行态来自后端快照）。服务运行中若接口掉线，`ifaceMissing` 为真但 `running` 为真 ⇒ 只告警不禁用、按钮保留「停止服务」（`:325` 起 `!running` 才显示「改用所有接口」）。**符合设计**；需人工确认的点是「运行中下拉被 disabled（`:292`）故无法切换」——正确。
3. **3s 轮询的常驻开销**：本机 `ipconfig::get_adapters()` 实测 p50≈3.7ms、p95≈4.5ms（前序探针 `qa-probe-run4.log`），3s 一次可忽略；且窗口 hidden 跳过、仅内容变化才 `setState`，无下拉闪烁与下游 effect 抖动。
4. **`interfacesLoaded` 在「先失败后成功」时的表现**：首拉失败 → 保持 false（不误报掉线）；后续轮询成功 → 置 true 并更新列表；此时若 `prefs.iface` 确不在列表，才出现「已掉线」。**逻辑正确**；但「先失败后成功」期间存在 §B-5 的显示/取值不一致窗口。
5. **`#[cfg(not(windows))]` 的 sysfs 路径在本机完全无法验证**：`link_state_for`（`lib.rs:753-769`）非 Windows 分支读 `/sys/class/net/<if>/{operstate,carrier}`，Windows 分支恒返回 true 并打 warn。**必须显式标注为「本机不可验证、仅纯函数单测覆盖」**（见 §F）。纯函数 `net.rs` 的 5 个单测已覆盖语义（两信号皆缺 → true；任一可读为 down → false），但「sysfs 读取 + 路径拼接 + 当次判断」这一层无任何执行证据。
6. **`enumerate_interfaces_fallback` 的 Windows 兜底路径**：本机 `ipconfig` 正常，兜底（`get_if_addrs` + 恒 true + warn）未被触发，回归风险未经实测（同样归入 §F）。
7. **`check_passive_ports` 的 65535 边界**（§B-8）：检查通过、启动失败的不一致，属既有边缘问题。

---

## D. 版本与发布

### D-11 重新出安装包前是否必须升版本号？ —— **必须升（建议 0.2.3）**
证据与理由：
1. **版本当前确实未动**：`app/src-tauri/tauri.conf.json` `"version": "0.2.2"`；`crates/ftp-core/Cargo.toml` 与 `app/src-tauri/Cargo.toml` 均 `0.2.2`。代码改了、版本没改。
2. **UpgradeCode 固定、ProductCode 每次编译随机**（Tauri 生成的 WiX 实证）：
   - `.../ftp-toolbox-target/release/wix/x64/main.wxs`：`<Product Id="*" ... UpgradeCode="6ad3f9f6-e7bd-5d00-a067-73f130d6e758" Version="0.2.2">`，且含 `<MajorUpgrade Schedule="afterInstallInitialize" AllowDowngrades="yes" />`。
   - 对两份已产出的 MSI 做二进制 GUID 差集：
     - 两版**共有**（确定性）：`6AD3F9F6-E7BD-5D00-A067-73F130D6E758`（UpgradeCode）及若干 UUIDv5 组件/快捷方式 GUID；
     - **各自独有**的随机 v4 GUID：0.2.1 有 3 个、0.2.2 有 3 个 → 其中即 **ProductCode**（`Id="*"` 每次编译重新生成）。
3. 后果：Windows Installer 的包身份是 `(ProductCode, ProductVersion)`。**同版本、不同 ProductCode + 同一 UpgradeCode** 时，`MajorUpgrade` 只对「更高版本」生效，同版本不会走升级路径 ⇒ 覆盖安装会命中 **MSI 1638（「另一个版本的此产品已安装」）** 或要求先卸载；`AllowDowngrades="yes"` 也救不了「同版本」这一情形。**版本号这才是走通就地升级（major upgrade）的关键。**
4. 另有产品/支持层面原因：ARP（添加删除程序）显示的是 0.2.2，不升版本无法区分「旧的 0.2.2」与「含本轮修复的 0.2.2」，用户与支持都无法判断是否已更新。NSIS 安装器同理按版本比较。

**结论：重新出包前必须把 `tauri.conf.json`、两个 `Cargo.toml` 的版本升到 0.2.3（前端 `package.json` 现为 0.1.0，可一并校对），否则同号 MSI 无法平滑替换。**

---

## E. 需 team-lead 处理的问题（按优先级）

1. **[中] `interfacesLoaded=false` 时的下拉显示/取值不一致**（§B-5）：显示 `0.0.0.0` 但实际会用 `prefs.iface`，且「启动服务」不禁用。建议：只要 `prefs.iface` 非 0.0.0.0 且不在列表就渲染该 option（无论是否 loaded），但仅在 loaded 时标「已掉线」并告警。
2. **[低] 真冲突 + 托管段并存文案并列易误读**（§B-8）：明确各指哪一段，或合并措辞。
3. **[低] `a_plain_reserved_band_is_reported_as_forbidden` 的静默跳过**（§A-2）：改成可区分的显式跳过/失败信号。
4. **[低] 覆盖缺口**：`sort_interfaces`、`link_state_for`(非 Windows)、`enumerate_interfaces_fallback`(Windows 兜底)、`check_passive_ports` 边界均无单测。
5. **[发布必做] 升版本号到 0.2.3**（§D-11）。
6. **[建议] 托管段可考虑运行时 bind 探测**再定措辞（§A-3 限定 1），非必须。

---

## F. 本机无法验证的部分（须显式标注）

| 项 | 位置 | 为何无法验证 | 现有保障 |
|----|------|-------------|---------|
| 非 Windows sysfs 链路状态 | `lib.rs:753-769` `link_state_for`（`#[cfg(not(windows))]`） | 本机为 Windows，该分支不编译 | 仅 `net.rs` 纯函数单测；**读取+拼接+调度层零执行证据** |
| Windows 兜底枚举 | `lib.rs:718-745` `enumerate_interfaces_fallback` | 本机 `ipconfig` 正常，兜底不触发 | 无（代码可读，行为未实测） |
| 非 Windows Forbidden 文案（root/setcap/SELinux） | `error.rs:107-116` | `cfg` 门控，本机不编译 | 单测 `hints_name_the_port_and_are_platform_appropriate` 的非 Windows 分支仅在他机生效 |
| NSIS 安装器的同版本行为 | 安装包 | 不做真实安装 | 未验证（仅 MSI 侧有 GUID 实证） |

---

## G. 临时改动与撤回声明

本轮为取证做过两处**临时**改动，均已**完整撤回**：
1. `crates/ftp-core/tests/zz_qa_probe.rs`（本人新建的探针集成测试）→ 已 `rm`；`git status` 中不再出现。
2. `app/ui/index.html`（为渲染真实告警注入 Tauri IPC stub）→ 已从备份还原，`sha1` 与原始一致（`60e4bbef97fb81822cfa5ce06bc874819d99241e`）。该文件本不在本轮 modified 列表内，还原后 `git status` 中**不出现**。

除上述两项外，**未修改仓库任何源码**。另：为排除增量缓存，执行过 `cargo clean -p ftp-core -p ftp-toolbox-app`（只清 target 构建产物，不动仓库源码）。

## 附：证据文件
- 截图：`C:\Users\22534\.workbuddy\build\r2-servers-640.png`、`-800.png`、`-1024.png`、`r2-servers-faillist-1024.png`
- 探针原始输出：见本报告 §A-2/§A-3 内联（`zz_qa_probe` 运行输出），及历史 `qa-probe-run*.log`
- netsh：`协议 tcp 端口排除范围 28385/28390/50000-50059(*)/50131`
- WiX：`C:\Users\22534\.workbuddy\build\ftp-toolbox-target\release\wix\x64\main.wxs`
