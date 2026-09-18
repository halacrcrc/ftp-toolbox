# QA 终验报告 — Round 5（N1' / N4 复核 + 0.2.3 安装包核对）

- 验证人：QA Engineer（Edward / 严过关）
- 对象：FTP Toolbox Rust + Tauri v2，版本 0.2.3
- 范围：**只验证，不改仓库文件、不动 dist**（本轮未跑 cargo；未注入任何临时探针，git 工作树无遗留）
- 结论：**5/5 项全部成立**，无新增缺陷，无阻断项。

---

## 逐项结论

### ① N1'（`{join()}` 后缺空格）—— 成立（已修复）

**声明**：编译产物里 `50000-50059` 与「重叠」/「内」之间应恰好一个空格。

**证据链：**

| 层 | 内容 | 证据 |
|----|------|------|
| 源码 | `app/ui/src/views/ServersView.tsx` 396 行 `…系统托管排除段 {check.managedConflicts.join("、")} 重叠（Hyper-V / WSL2`；401 行 `…排除段 {check.managedConflicts.join("、")} 内（Hyper-V / WSL2` —— `{join()}` 与其后文字**同一行** | Read 378–409 |
| 注释 | 385–391 行注释已固化两条 JSX 空白坑（勿共用中段；`{join()}` 后文字须同行的说明），防回归 | Read 385–391 |
| 编译产物 | `app/ui/dist/assets/index-DNXgkoTx.js`：`grep -o '" 重叠（Hyper-V / WSL2 常用）'` → **命中（前置一个空格）**；`grep -o '" 内（Hyper-V / WSL2 常用）'` → **命中（前置一个空格）** | `r5-master.txt` L7–8 |
| 反例检查 | 粘连形式 `、重叠（` 出现次数 = **0**；`、 重叠（` = 0（因被 `join("、")` 分隔，本就不会出现） | `r5-master.txt` L9–11 |

**判定**：编译器产出的运行串为 `…系统托管排除段 ` + `50000-50059` + ` 重叠（…`，两侧各一空格，无粘连。**成立。**

---

### ② N4（README 打包路径）—— 成立

**证据：**
- `README.md` 64–67 行：
  ```bash
  cd app               # 必须在这个目录（tauri CLI 只在 cwd 往下找配置）
  ui/node_modules/.bin/tauri build
  ```
- 与既有实测一致：cwd=`app/` 时 `ui/node_modules/.bin/tauri` 解析到 `app/ui/node_modules/.bin/tauri` → 退出码 **0**、版本 **tauri-cli 2.11.4**（v2，schema 匹配）；而此前验证的 `../ui/...` 写法 → **exit 127**（路径不存在）。
- 69 行 blockquote 明确警示 `cargo tauri build` 会走全局 `cargo-tauri`(1.x) 并以 v1 schema 校验 v2 配置 → 与本机 `cargo-tauri.exe = tauri-cli 1.0.5` 的事实吻合。

**判定**：路径与 `cd app` 自洽、命令可用。**成立。**

---

### ③ MSI `ProductVersion` 真为 0.2.3（非仅文件名）—— 成立

> WindowsInstaller COM 被安全策略拦截（`New-Object -ComObject WindowsInstaller.Installer` 被阻断），改用**字节级读取 MSI 数据库字符串池 + exe VERSIONINFO** 双路取证。

**证据：**
- **MSI Property 表字符串池**（偏移 ≈6,985,242 的连续可打印串）：
  `…ProductLanguage1033 ProductName ftp-toolbox ProductVersion 0.2.3 {6AD3F9F6-E7BD-5D00-A067-73F130D6E758} …`
  → `ProductVersion` 键**紧邻其值 `0.2.3`**，即 MSI 数据库里登记的 ProductVersion = 0.2.3。见 `r5-msi-context.txt` L4。
- **第二处 `0.2.3`**（偏移 ≈6,984,359）为内嵌应用二进制的版本资源：`ftp-toolbox-app.exe 0.2.3.0`（四段文件版本）。
- **无旧版本残留**：整份 MSI 中 ASCII `0.2.2` 出现次数 = **0**（`r5-msi-strings.txt` L6–7）。
- **交叉验证 exe**：`ftp-toolbox_0.2.3_x64-setup.exe` 的 Windows VERSIONINFO → **FileVersion=0.2.3, ProductVersion=0.2.3, ProductName=ftp-toolbox**（PowerShell 读取，即 Windows 文件属性所示值）。
- 版本来源自洽：`tauri.conf.json:4 "version":"0.2.3"`、`app/src-tauri/Cargo.toml`、`crates/ftp-core/Cargo.toml`、`app/ui/package.json` 均 0.2.3；`frontendDist:"../ui/dist"`（`r5-config.txt` L2–5, L14–16）。

**判定**：MSI 与 exe 的**内部**版本号均为 0.2.3。**成立。**

---

### ④ 内嵌前端即 N1' 修复版（`index-DNXgkoTx.js`），无残留旧 hash —— 成立（含一处方法学说明）

**证据：**
- `app/ui/dist/assets/` 仅含 **`index-DNXgkoTx.js`**（163,285 B）+ `index-BttlOu8X.css`；`index-*.js` 只有一个（`r5-embed.txt` L2–6）。
- `app/ui/dist/index.html` 仅引用 `./assets/index-DNXgkoTx.js` + `./assets/index-BttlOu8X.css`，**无其它 hash 引用**（`r5-embed.txt` L8–9）。
- 该 bundle 内含 N1' 修复后的正确字面量（见①）。
- 时序自洽：`dist/assets/*` mtime **18:40** < 安装包 mtime **18:41** → 先构建前端再打包。
- 指纹：`index-DNXgkoTx.js` sha256 = `48C186F715DCBD870542D6D738B14DFF327CF2254747A290220E310754B6F77D`（`r5-installers.txt` L15）。

**方法学说明（非缺陷）**：安装包内部**无法**直接明文命中 `系统托管排除段` 或 `index-DNXgkoTx`（`r5-embed.txt` L12–16 计数均为 0）。原因是 Tauri 把 `dist` 内嵌进**编译后的 Rust 二进制**，而安装包载荷（NSIS LZMA / MSI cab）被压缩，故明文 grep 必然为 0 —— 属预期行为，非缺陷。因此"安装包内前端=该份 dist"由 **dist 指纹 + 构建时序（18:40→18:41）+ 唯一 hash 文件** 推定，未能从安装包内解包二次比对。

**判定**：`dist` 当前产物唯一且为 N1' 版，无残留旧 hash 文件。**成立。**

---

### ⑤ 安装包体积变化合理 —— 成立

| 产物 | 0.2.2 | 0.2.3 | Δ | Δ% |
|------|-------|-------|---|----|
| `…_x64_en-US.msi` | 6,983,680 | 6,991,872 | **+8,192** | +0.117% |
| `…_x64-setup.exe` | 4,317,466 | 4,319,166 | **+1,700** | +0.039% |

- 两处增量都极小，与"版本号 + 少量 UI 文案改动"完全相称，**无新增依赖/资产**导致的异常膨胀。
- MSI 增量恰为 8 KiB（MSI 为 OLE 复合文档、按扇区/压缩块对齐），属正常量化；exe 增量 ~1.7 KB 亦合理。

**判定**：**成立。**

---

## 附带核查

- **工作树干净**：`git status --porcelain` 仅列预期源码改动 + 新增源码文件 + `deliverables/`；**无**临时探针（如 `zz_qa_probe.rs`）、无 `index.html` 备份残留（`r5-git.txt` L2–26）。本轮验证未修改任何仓库文件。
- **exe 原始字节 grep 不可靠（已弃用）**：对 setup.exe 做 UTF-16LE 明文 `0.2.3`/`0.2.2` 计数得到相同的 8,346（shell 传 null 字节被破坏所致），结论不可用，已改为采信 Windows VERSIONINFO 权威值。

## 未决/风险

- 无阻断项。
- 唯一"未直接取证"点：安装**包内**解包后的前端文件未能逐字节比对（受压缩载荷限制），已由 dist 指纹 + 时序推定，风险低。

## 总结

| 项 | 结论 |
|----|------|
| ① N1' 空格 | **成立** |
| ② N4 README 路径 | **成立** |
| ③ MSI/exe ProductVersion=0.2.3 | **成立** |
| ④ 内嵌前端=N1' 版且无残留 hash | **成立**（含方法学说明） |
| ⑤ 体积变化合理 | **成立** |

**Routing Decision：NoOne（可发布）。** 无源码 bug、无测试 bug、无遗留缺陷。
