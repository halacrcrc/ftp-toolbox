# QA 独立复核报告 — Round 3

复核者：Edward（QA）｜日期：2026-09-18｜对象：工作区未提交改动（第二轮修复）
方式：直接读代码 + 复跑命令 + 无头 Chromium 实测渲染（未修改任何仓库源码；临时探针已撤回，见 §G）。

> 环境说明：本轮会话中 Bash/PowerShell 的 stdout 捕获失效（`echo hi` 也无输出），但**命令确实在
> 执行**。故所有命令均**重定向到文件再读取**，结论基于文件内容，未降低证据强度。

## 结论表

| # | 项 | 判定 | 证据 |
|---|----|------|------|
| 1 | B-5 显示值/实际值（枚举失败路径） | **成立** | 源码拆分正确 + 无头浏览器实测（见 §1） |
| 2 | B-5 已掉线分支（读到了列表且 iface 不在） | **成立** | DOM 实测：option 标「（已掉线）」、告警、启动禁用 |
| 3 | B-6 `interface_sort_key` + 3 单测 | **成立** | 30 lib 测试全过；单测真正判别数值序（见 §2） |
| 4 | B-6 `sort_by_cached_key` 稳定性/快照去重 | **成立** | 稳定排序（std 文档保证）；论证见 §2 |
| 5 | B-8 分流消除「两件事要修」歧义 | **成立** | both 分支 DOM 实测文案（见 §3） |
| 6 | B-8 ok 分支文案语法 | **不成立（发现新 bug N1）** | 渲染出「还与…段…内」，语法不通 |
| 7 | D-11 版本三处一致 | **成立** | 3 处=0.2.3，Cargo.lock 已更新 |
| 8 | D-11 无遗漏 | **有保留（N2/N3）** | package.json=0.1.0、README=0.2.0 |
| 9 | cargo check / tsc / vite build | **成立** | 均 exit 0，无 warning |

## 1. B-5（显示值/实际值）—— 成立

**代码（`app/ui/src/views/ServersView.tsx`）：**
- `:182-184`
  ```ts
  const ifaceUnlisted =
    prefs.iface !== "0.0.0.0" && !interfaces.some((it) => it.ip === prefs.iface);
  const ifaceMissing = interfacesLoaded && ifaceUnlisted;
  ```
- `:311-315` `<option>` 用 **ifaceUnlisted** 渲染，标签按 **ifaceMissing** 决定：
  ```tsx
  {ifaceUnlisted && (
    <option value={prefs.iface}>
      {ifaceMissing ? `${prefs.iface}（已掉线）` : prefs.iface}
    </option>
  )}
  ```
- `:336` 告警条、`:452-453` 启动按钮 `disabled={busy || !known || ifaceMissing}`。

**无头 Chromium 实测**（临时 index.html 注入 Tauri IPC stub，令 `list_interfaces` reject 模拟枚举失败；用完撤回）：

枚举失败路径（`#faillist`）—— DOM 提取：
```html
<select>
  <option value="0.0.0.0">所有接口 (0.0.0.0)</option>
  <option value="192.168.102.161">192.168.102.161</option>   <!-- 补回，裸地址，无「已掉线」 -->
</select>
...
<button class="btn primary">启动服务</button>   <!-- 无 disabled 属性 → 可点 -->
```
截图 `r3-faillist-1024.png` 佐证：下拉显示 `192.168.102.161`（**没有**回落到 0.0.0.0），无掉线告警条，启动按钮为可点状态。

**结论：枚举失败时 `<select>` 显示 prefs.iface 而非 0.0.0.0，且启动按钮未被误禁用 —— 成立。**
（同时对照：列表成功读到且 iface 不在列表时，option 变「192.168.102.161（已掉线）」、出现告警条、启动按钮 `disabled` 且带 title —— 见 `r3-clean-x.txt`/`r3-conflict-x.txt`，`ifaceMissing` 分支未被误伤。）

## 2. B-6（sort_interfaces 单测）—— 成立

- 纯函数：`crates/ftp-core/src/net.rs:90`
  ```rust
  pub fn interface_sort_key(ip: &str, name: &str) -> (Ipv4Addr, String) {
      (ip.parse::<Ipv4Addr>().unwrap_or(Ipv4Addr::UNSPECIFIED), name.to_owned())
  }
  ```
- 调用点：`app/src-tauri/src/lib.rs:782`
  ```rust
  items.sort_by_cached_key(|it| ftp_core::net::interface_sort_key(&it.ip, &it.name));
  ```
- 测试：`cargo test -p ftp-core --lib` → **30 passed; 0 failed**（含新增 `addresses_sort_numerically_not_as_text`、`the_name_breaks_ties_between_addresses_of_one_adapter`、`loopback_sorts_first_and_junk_does_not_panic`）。

**单测是否真的验证「数值序而非字符串序」这个要害 —— 是。** `addresses_sort_numerically_not_as_text` 断言排序结果为 `10.0.0.9 < 10.0.0.10 < 192.168.1.2`。若键按字符串比较，`"10.0.0.10" < "10.0.0.9"`（第 3 段 `'1' < '9'`），结果会与断言相反 → **该断言具备判别力，不是同义反复**。

**稳定性/快照去重 —— 成立。** `sort_by_cached_key` 在 std 中是**稳定排序**（文档保证），且键为 `(Ipv4Addr, String)` 全序；`ServersView.tsx:499-503` 用 `JSON.stringify` 逐字比对。即便两个条目键相等（同 ip 同名，实际几乎不可能出现），稳定排序也不会交换其相对序 → 快照字符串稳定 → 去重成立。`sort_by_cached_key` 还消除了原先 `sort_by_key` 每次比较 clone `String` 的 O(n log n) 分配。

## 3. B-8（真冲突 + 托管重叠并存措辞）—— 分流成立；但发现新 bug N1

代码 `ServersView.tsx:385-392` 按 `check.ok` 分流。两分支渲染实测：

- `ok=false`（真冲突 + 托管重叠同时存在，`#conflict`）：
  ```
  ⚠ 与系统保留段 28385-28385、28390-28390 重叠：…
  另外，该段也落在系统托管排除段 50000-50059 内（Hyper-V / WSL2 常用）。实测这类段仍可绑定，通常不影响 PASV，不必为它单独换段。
  ```
  → 第二条以「另外…也落在…内…**不必为它单独换段**」开头，读起来是**补充说明**，不再像第二条告警。**歧义已消除，成立。**

- `ok=true`（仅托管重叠，`#clean`）：
  ```
  提示：该段还与系统托管排除段 50000-50059 内（Hyper-V / WSL2 常用）。实测这类段仍可绑定，通常不影响 PASV；若列表/传输偶发失败，再换段即可。
  ```
  → ⚠ **「提示：该段还与…段…内」语法不通**（`还与` 应接 `…重叠` 或 `…也落在…内`）。原因是两分支共用的中段以「内」结尾，只适配 `也落在…内` 那一支。

**N1（低 / 仅措辞）修法建议**：把共用中段改成中性且两分支都通顺，例如
- 中段统一为「…重叠」：ok → `提示：该段还与管理排除段 {X} 重叠（…），通常不影响 PASV；若偶发失败再换段即可。`；not-ok → `另外，该段也与管理排除段 {X} 重叠（…），但不必为它单独换段。`
- 或整句按 `check.ok` 分两条完整字符串。

## 4. D-11（版本号）—— 三处一致成立；有两处遗漏

`0.2.3` 落位（源码/配置）：
- `crates/ftp-core/Cargo.toml:3` `version = "0.2.3"`
- `app/src-tauri/Cargo.toml:3` `version = "0.2.3"`
- `app/src-tauri/tauri.conf.json:4` `"version": "0.2.3"`
- `Cargo.lock` 已随构建更新：`ftp-core` = 0.2.3、`ftp-toolbox-app` = 0.2.3。

**遗漏 2 处（均低危，且都不是本轮引入）：**
- **N2** `app/ui/package.json:4` `"version": "0.1.0"` —— 前端包版本从未跟随 app 版本。Tauri 打包读 `tauri.conf.json`，**不影响安装包版本号**，但会误导（`npm run build` 打印 `ftp-toolbox-ui@0.1.0`）。建议设为 0.2.3 或从 package.json 移除 version。
- **N3** `README.md:17-18` 下载文件名仍是 `ftp-toolbox_0.2.0_x64-setup.exe` / `..._0.2.0_x64_en-US.msi` —— 早在 0.2.2 时就已过期，本轮未更新。发布前应改为 0.2.3。
- NSIS / WiX：仓库内**无静态模板**（`main.wxs` 由构建时按 `tauri.conf.json` 生成于 target），无需手改。

## 5. 静态检查（独立复跑，均写入文件核对）

- `cargo check --workspace` → `Checking ftp-toolbox-app v0.2.3` + `Finished`，`cargo_check_exit=0`，无 warning。
- `cd app/ui && npx tsc --noEmit` → `tsc_exit=0`，无输出。
- `cd app/ui && npm run build` → `built in 1.13s`，`build_exit=0`。
（team-lead 提示的「`npm run build` 不含 tsc」属实：build 只跑 vite；故两样都跑了。）

## G. 临时改动与撤回声明

- 临时在 `app/ui/index.html` 注入 Tauri IPC stub（仅为在无头浏览器里模拟后端 reject / 各种 check 结果）→ 已从备份还原，`sha1` 与原始一致：`60e4bbef97fb81822cfa5ce06bc874819d99241e`。

撤回后核验：
- `git status --short` 与复核开始前**完全一致**（无新增/删除文件，`index.html` 未出现）。
- `crates/ftp-core/tests/` 仅 4 个既有文件，无临时探针残留。
- `app/ui/dist/index.html` 经本轮重建，**不含** stub（`grep -c "QA HARNESS"` = 0）；且 `dist/` 被 `.gitignore` 忽略，不污染仓库。

**除上述临时文件外，未修改仓库任何源码。**

## 附：证据文件（C:\Users\22534\.workbuddy\build\）
`r3-faillist-dom.html` / `r3-conflict-dom.html` / `r3-clean-dom.html`（三态 DOM dump）、`r3-*-x.txt`（提取片段）、`r3-faillist-1024.png` / `r3-conflict-1024.png` / `r3-clean-1024.png`、`r3-cargo.log` / `r3-tsc.log` / `r3-build.log` / `r3-final.txt`
