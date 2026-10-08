# 代码审查：09-30 之后全部提交 + 0.3.2 工作区（2026-10-08）

## 审计范围与方法

- **区间**：`bf00c11..3dbba0b` 共 9 个提交（2026-09-30 21:01 最后一轮评审报告之后
  未经独立评审的部分），另加**工作区未提交的 0.3.2**（FTP 主动模式开关，安装包
  已随记忆记录发版）。
- **方法**：逐提交 diff 审读；可验证断言逐条核对（版本 5 处 grep、许可继承链、
  个人 cargo 配置接管、上游死代码声明）；全套测试复跑。
- **基准**：本次同步建立 [`docs/code-review.md`](../../docs/code-review.md)
  （成文审查标准与流程，补齐 2026-10-08 上午核实的两个 🔴 缺口）。

## 整体评估

✅ **Good** —— 9 个提交无 Critical/High 发现：唯一代码提交 bf00c11 的两处修复
均正确且被测试钉住；配置与文档类提交证据链完整。0.3.2 工作区代码质量同样过关
（默认关闭、双层告警、正反两例测试、存量 prefs 兼容）。关键风险是**流程性**的：
0.3.2 已发版但源码未提交，以及两处文档漂移（本次已修正其一）。

## 发现清单

### 2026-10-08 #1 版本同步清单少第 5 处 🟡

- **位置**：`CONTRIBUTING.md`「版本号」节（修改前 L77-L82）
- **分析**：实际发版流程是 **5 处**同步（`ff17061` 与 0.3.2 都改了 README 下载段
  文件名），CONTRIBUTING 只列 4 处。按文档执行的人必然漏改 README 下载段，
  造成「产物已更新、文档指旧包」的漂移。
- **修复**：✅ 本次已落地（含同版本 MSI 覆盖 1638 提示），并新增「代码审查」章节
  引用 `docs/code-review.md`。

```markdown
<!-- FILEPATH: C:/WorkBuddy/FTP/CONTRIBUTING.md -->

<!-- ------ ORIGINAL CODE ------>
## 版本号

发版时四处必须同步，缺一处会导致产物版本与界面显示不一致：

- `app/src-tauri/tauri.conf.json`
- `app/src-tauri/Cargo.toml`
- `crates/ftp-core/Cargo.toml`
- `app/ui/package.json`
<!-- --------------------------
<!-- ------ NEW CODE ----------
## 版本号

发版时**五处**必须同步，缺一处会导致产物版本与界面显示不一致：

- `app/src-tauri/tauri.conf.json`
- `app/src-tauri/Cargo.toml`
- `crates/ftp-core/Cargo.toml`
- `app/ui/package.json`
- `README.md` 下载段里的安装包文件名

同版本号重复出 MSI 会触发覆盖安装错误（1638），版本号只进不退。

## 代码审查

审查标准与流程见 [`docs/code-review.md`](docs/code-review.md)，要点：
（提交前四条命令全绿 / 红线清单 / 发版门禁 / 发现编号与闭环）
<!-- --------------------------
```

### 2026-10-08 #2 0.3.2 已发版但源码未提交 🟡

- **位置**：工作区 `git status`（11 个修改文件 + 新增 `tests/ftp_active_mode.rs`）
- **分析**：0.3.2 安装包已产出并按记忆记录分发给用户（含交换机 502 故障的处置
  指引），但对应源码仍停留在未提交状态。一旦本机故障，**该已分发版本无法从
  仓库复现**；且本地 main 停在 0.3.1，任何人 clone 得到的是不含修复的代码。
  这违反本次成文的「先提交再发版」门禁（`docs/code-review.md` 审查节点 3）。
- **修复建议**：立即提交（经本次审计确认全部验证绿，无阻塞项）：

```bash
# FILEPATH: 仓库根，待执行（需维护者确认）

# ------ ORIGINAL CODE ------
# （现状：0.3.2 改动散落在工作区，main 停在 3dbba0b = 0.3.1）
git status --short
#  M Cargo.lock  M README.md  M app/src-tauri/Cargo.toml  M app/src-tauri/src/lib.rs
#  M app/src-tauri/tauri.conf.json  M app/ui/package.json  M app/ui/src/api.ts
#  M app/ui/src/views/ServersView.tsx  M crates/ftp-core/Cargo.toml
#  M crates/ftp-core/src/ftp/server.rs
# ?? CODEBUDDY.md  ?? crates/ftp-core/tests/ftp_active_mode.rs
# --------------------------
# ------ NEW CODE ----------
# 方案一（推荐，单个 feat 提交 + 单独的 AI 指南文件提交）：
git add Cargo.lock README.md app/src-tauri/Cargo.toml app/src-tauri/src/lib.rs \
        app/src-tauri/tauri.conf.json app/ui/package.json app/ui/src/api.ts \
        app/ui/src/views/ServersView.tsx crates/ftp-core/Cargo.toml \
        crates/ftp-core/src/ftp/server.rs crates/ftp-core/tests/ftp_active_mode.rs
git commit -m "feat(ftp): add opt-in active mode (PORT) for legacy clients"
git add CODEBUDDY.md
git commit -m "docs: add CodeBuddy project guidance"
# 推送需代理可用时再执行（见 .workbuddy/memory/2026-10-08.md 网络事实）
# git push
# --------------------------
```

### 2026-10-08 #3 记忆/指南中 target-dir 表述与仓库配置相反 🟡

- **位置**：`.workbuddy/memory/MEMORY.md` L7（硬性环境约定首条）；未提交的 `CODEBUDDY.md`
- **分析**：两处均声称「仓库级 `.cargo/config.toml` 提供 target-dir 指向
  `C:/Users/22534/.workbuddy/build/ftp-toolbox-target`」，但 `510c3eb` 已把该路径
  移出仓库配置（本审计已核实本机 `~/.cargo/config.toml` 接管了 `[build] target-dir`）。
  照旧表述执行的人会误以为仓库配置仍含个人路径。`MEMORY.md` 与 `CODEBUDDY.md`
  （后者经用户确认后一并修正，并顺带补上对 `docs/code-review.md` 的引用）
  本次均已落地。
- **修复**：✅ MEMORY.md 与 CODEBUDDY.md 均已落地：

```markdown
<!-- FILEPATH: C:/WorkBuddy/FTP/.workbuddy/memory/MEMORY.md -->

<!-- ------ ORIGINAL CODE ------>
- cargo 用项目内 `.cargo/config.toml`（rsproxy 镜像 + target-dir 指向
  `C:/Users/22534/.workbuddy/build/ftp-toolbox-target`，绝不可放项目里——
  OneDrive 会搞坏构建脚本）。
<!-- --------------------------
<!-- ------ NEW CODE ----------
- cargo 镜像配置在项目内 `.cargo/config.toml`（rsproxy + git-fetch-with-cli；
  **无 target-dir**——510c3eb 已把个人路径移出仓库）。target-dir 指向
  `C:/Users/22534/.workbuddy/build/ftp-toolbox-target` 的配置在本机
  `~/.cargo/config.toml`（绝不可放回项目——OneDrive 会搞坏构建脚本；
  2026-10-08 已核实存在）。
<!-- --------------------------
```

### 观察项（不修，记录在案）🟢

- `app/src-tauri/src/lib.rs` 启动消息里主动模式提示先于 FTPS 拼接
  （`mode_note` 顺序），纯文案顺序，不影响语义。
- `crates/ftp-core/src/sftp/keys.rs` `unique_temp_path` 用 `with_extension`：
  若目标名本身含点（如 `known.hosts`）会截断 stem——当前目标固定为
  `known_hosts`，不受影响；若日后改名需连带注意。
- `app/src-tauri/make-license-rtf.py` 仅支持 BMP 内字符（>0xFFFF 显式报错）——
  许可文案当前无此需求，报错优于静默乱码。

## 验证结果（2026-10-08 实跑）

| 命令 | 结果 |
| --- | --- |
| `cargo test -p ftp-core` | **92 passed / 0 failed**（68 单测 + ftp_active_mode 2 + ftp 3 + tftp 单测 1 + ftp_loopback 8 + ftps_loopback 4 + sftp_loopback 5 + tftp_loopback 1） |
| `cd app/ui && npm test` | **22 / 22 通过** |
| `cd app/ui && npm run typecheck` | 通过（无输出） |
| `cargo check --workspace` | `Finished dev profile in 10.24s` |
| `git grep 0.3.1`（ff17061） | 5 处全部命中（含 README 下载段两行） |
| 许可继承链（a84f1b5） | 两 crate 均 `license.workspace = true`，root 为 `MIT OR Apache-2.0` |
| 个人 cargo 配置（510c3eb 配套） | `~/.cargo/config.toml` 含 `[build] target-dir` 指向既有路径 |

## 逐提交结论

| 提交 | 类型 | 结论 |
| --- | --- | --- |
| `bf00c11` fix(sftp) | 代码 | ✅ 0 长度 READ 回空 `Data`（seek 前返回，不扰动偏移/计数）；临时文件名 PID+seq 唯一化，错误分支已有 `remove_file` 清理，无残留问题。2 例测试钉住行为。 |
| `ff17061` chore(release) | 版本 | ✅ 0.3.1 五处同步核对通过 |
| `99cc0ad` docs | 文档 | ✅ 二轮评审与交接归档，命名合规 |
| `510c3eb` chore(cargo) | 配置 | ✅ 个人路径移出仓库配置，接管配置已核实存在 |
| `1f45daf` docs | 文档 | ✅ CONTRIBUTING 建立（本次补充审查章节与第 5 处版本同步） |
| `a540277` docs | 文档 | ✅ 指引澄清，无与代码事实冲突 |
| `a84f1b5` docs(license) | 许可 | ✅ SPDX 表达式、crate 继承、徽章、贡献条款同步；再许可前提（唯一版权人）已在提交信息核实 |
| `4a88439` feat(bundle) | 配置+脚本 | ✅ `licenseFile` + `resources` 满足 Apache-2.0 §4(a) 随附要求；RTF 单行/`\uN?` 约束有上游源码依据（含 `Path::ends_with` 死代码发现） |
| `3dbba0b` docs | 归档 | ✅ 截图 + 13 行 NSIS 复现脚本无问题 |
| （工作区）0.3.2 主动模式 | 代码 | ✅ 引擎默认关 + warn 日志；UI hint-line 告警；测试正反两例；`loadPrefs` 用 fallback 合并兼容存量缺键；`allowActiveMode` 命名与 `ftpsEnabled` 惯例一致 —— **待提交**（见 #2） |

## 整改闭环

- **#1、#3**：本次审计中已落地（CONTRIBUTING.md / MEMORY.md / CODEBUDDY.md）。
- **#2**：待维护者确认后执行提交（修复建议中的命令序列已就绪）。
- 成文审查机制（原 🔴 两个缺口）：`docs/code-review.md` 已建立，CONTRIBUTING.md
  已接入索引。
