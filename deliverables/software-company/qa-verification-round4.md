# QA 独立复核报告 — Round 4（N1/N2/N3 + README 打包命令）

复核者：Edward（QA）｜日期：2026-09-18｜对象：第三轮发现的 N1/N2/N3 修复 + README 打包命令判断
方式：读源码 + 无头 Chromium 实测渲染 + 文件系统/shim 实测。未修改任何仓库源码（临时 harness 已撤回，见 §G）。

> 环境：Bash/PowerShell stdout 捕获仍失效，命令改用「重定向到文件再读」取证。

## 结论表

| # | 项 | 判定 | 证据 |
|---|----|------|------|
| 1 | N1 语法「还与…重叠」是否修好 | **成立** | 两分支实测渲染文本通顺（§1） |
| 2 | N1 JSX 换行是否引入粘连/意外空格 | **有保留（新 N1'）** | 实测「50000-50059重叠」「50000-50059内」缺一个空格 |
| 3 | N1 注释是否记下教训 | **成立** | ServersView.tsx:385-386 |
| 4 | N2 package.json → 0.2.3 | **成立** | app/ui/package.json:4 |
| 5 | N3 README 文件名 → 0.2.3 | **成立** | README.md:17-18 |
| 6 | README 打包命令：CLI 身份判断 | **成立（该保留 CLI 选择）** | shim → @tauri-apps/cli@2.11.4；PATH cargo-tauri=1.0.5 v1 |
| 7 | README 打包命令：相对路径 | **不成立（新 N4）** | 实测 exit 127，应改（§4） |

## 1. N1 —— 语法已修，但引入一个缺空格（N1'）

**代码（`app/ui/src/views/ServersView.tsx:387-403`）**：两支各写整句，已放弃共用中段；注释 `:385-386` 记录教训「上一版…结果 ok 那支拼成「…还与…排除段内」，读不通。宁可重复，不要拼接。」✓

**无头 Chromium 实测**（临时 stub 构造两种后端返回，DOM 抓文本，逐字）：

- `check.ok = true` 且 `managedConflicts` 非空（`#clean`）：
  ```
  提示：该段还与系统托管排除段 50000-50059重叠（Hyper-V / WSL2 常用）。实测这类段仍可绑定，通常不影响 PASV；若列表/传输偶发失败，再换段即可。
  ```
- `check.ok = false` 且真冲突 + 托管重叠都非空（default）：
  ```
  ⚠ 与系统保留段 28385-28385、28390-28390 重叠：落在其中的端口无法用于 PASV，列表/传输会间歇性失败（控制连接仍是正常的）
  另外，该段也落在系统托管排除段 50000-50059内（Hyper-V / WSL2 常用）。实测这类段仍可绑定，通常不影响 PASV，不必为它单独换段。
  ```

**判定：**
- **语法 bug 已修**：「还与…重叠」「也落在…内」均通顺，句子完整、无缺字、无粘连词。
- **但引入 N1'（低 / 纯排版）**：端口段与紧随其后的汉字之间**少了空格** —— `50000-50059重叠` / `50000-50059内`。原因：`重叠`/`内` 被挪到 `{join()}` 之后的**新行**，JSX 会剥掉行首空白并在表达式后**不补空格**（对比同页既有 warn 行 `与系统保留段 … 重叠：` 因写在同一行而**有**空格 → 风格不一致）。第三轮旧版此处是「50000-50059 内」，**有**空格 → 属本次修复引入的小回归。
- 「通常不影响 PASV」的空格正常（行内换行→单空格），`PASV；` 未粘错。仅 `{expr}` 之后那一处丢了空格。

**修法建议**：把范围值与后随词放回同一行（或显式空格），例如
```tsx
提示：该段还与系统托管排除段 {check.managedConflicts.join("、")} 重叠（Hyper-V / WSL2 常用）。…
另外，该段也落在系统托管排除段 {check.managedConflicts.join("、")} 内（Hyper-V / WSL2 常用）。…
```

## 2. N2 —— 成立
`app/ui/package.json:4` `"version": "0.2.3"`（原 0.1.0）✓。与 `crates/ftp-core/Cargo.toml:3`、`app/src-tauri/Cargo.toml:3`、`tauri.conf.json:4` 一致。

## 3. N3 —— 成立
`README.md:17-18` 下载文件名 `ftp-toolbox_0.2.3_x64-setup.exe` / `ftp-toolbox_0.2.3_x64_en-US.msi` ✓。仓库内非 deliverable 源码已无 `0.2.0` 残留（grep 确认）。

## 4. README 打包命令 —— CLI 判断对，路径不对（N4）

**CLI 身份（team-lead 的判断）—— 成立，CLI 选择应保留：**
- `app/ui/node_modules/.bin/tauri`（sh shim）第 13/15 行：`exec node "$basedir/../@tauri-apps/cli/tauri.js" "$@"`；`.cmd` shim 同理指向 `..\@tauri-apps\cli\tauri.js`。
- `app/ui/node_modules/@tauri-apps/cli/package.json:3` → `"version": "2.11.4"`（**v2**）；`tauri.js` 存在。
- `PATH` 上的 `cargo-tauri.exe` 实测 `--version` → **`tauri-cli 1.0.5`**（**v1**）。
- 故用项目内 v2 shim 而**不是** `cargo tauri` 是正确主张 —— 保持 CLI 选择、不要改用 `cargo tauri`。（另注：`package.json` devDeps 也声明 `@tauri-apps/cli ^2.11.4`。）

**相对路径（新 N4，实测证伪 README 现状）—— 应改：**
`README.md:64-66` 写的两步是
```
cd app
../ui/node_modules/.bin/tauri build
```
从 cwd=`<root>/app` 解析，`../ui` = `<root>/ui`，而仓库里 **ui 在 `app/ui`，`<root>/ui` 不存在**。实测（`--version`，不触碰 target 锁）：
```
cwd=/c/WorkBuddy/FTP/app
$ ../ui/node_modules/.bin/tauri --version   → exit 127  No such file or directory
$ ui/node_modules/.bin/tauri   --version   → exit 0    tauri-cli 2.11.4
```
Glob 也确认只有 `app/ui/node_modules/.bin/tauri` 存在。

**结论：这条命令照抄会直接 `No such file or directory`（exit 127）。「改反而可能改坏」的担心只适用于「换成 `cargo tauri`」；单纯修路径段不会改坏。** 建议二选一：
- 保留 `cd app`，把路径改成 `ui/node_modules/.bin/tauri build`（最小改动，推荐）；或
- 保留 `../ui/...`，把 `cd app` 改成 `cd app/src-tauri`。

（tauri.conf.json 里 `build.frontendDist="../ui/dist"`、`beforeBuildCommand.cwd="../ui"` 正是以 `src-tauri` 为基准写的 —— 说明 `../ui` 这条前缀本是按 `app/src-tauri` 时的心智写的，与 README 的 `cd app` 不自洽。）

## G. 临时改动与撤回
- 临时在 `app/ui/index.html` 注入 Tauri IPC stub（仅为无头渲染 N1 两分支）→ 已还原，`sha1` = `60e4bbef97fb81822cfa5ce06bc874819d99241e`（与原始一致）。
- 复核后 `git status`：仅含本轮应改文件（新增 `M README.md`、`M app/ui/package.json`）+ 既有改动；无 `index.html`、无临时探针残留。
- 未执行任何会占用 target 锁的 cargo 命令（`--version` 除外，不触碰锁）。

## 附：证据文件（C:\Users\22534\.workbuddy\build\）
`r4-clean-text.txt` / `r4-conflict-text.txt`（两分支渲染文本）、`r4-buildcmd.txt`（README 路径实测）、`r4-cargotauri.txt`（v1 版本）、`r4-path.txt`、`r4-final.txt`；shim 源码见 `app/ui/node_modules/.bin/{tauri,tauri.cmd}`。
