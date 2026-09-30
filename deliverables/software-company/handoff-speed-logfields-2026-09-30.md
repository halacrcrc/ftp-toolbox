# 交接文档 — 传输速度 / 日志字段 / 前端单测（2026-09-30，第四篇）

> 面向接手本项目的下一个 agent / 开发者。续篇，只记录 `handoff-2026-09-30.md`
> （第三篇，HEAD=d650ad9）之后的增量。前三篇与规格的位置见文末导航。

---

## 一、当前状态（截至 2026-09-30 20:50）

| 项 | 值 |
|---|---|
| 本地 HEAD | `33d83d3`（docs: round 6 verification） |
| 远端 `origin/main` | `98d374c` —— **本地领先 5 个提交未 push**：`d650ad9` `26c5cd6` `7f9f302` `390af64` `33d83d3` |
| 测试 | `cargo test -p ftp-core` **88 passed / 0 failed**（实测）；`npm test` **22 passed**；`tsc --noEmit` 干净 |
| GitHub Release | v0.3.0 已发布，但资产**确定不含**速度显示与日志字段增强。已用时间戳定论（不需要网络）：`dist/ftp-toolbox_0.3.0_*` 构建于 **09-30 11:18**，而 `d650ad9` 提交于 **19:56** —— 晚了 8.6 小时。资产实际来自 `69aa7c5`（11:00），`98d374c`（11:57）是纯 README 提交，对二进制无影响 |
| 审查 | 本轮审查报告 `code-review-sftp-2026-09-30.md`（上轮）的全部 10 项发现已闭环；本轮新审查 `code-review-speed-logfields-2026-09-30.md` 结论 ✅ Approve，剩 3 个 🟢 |

## 二、d650ad9 之后的 4 个提交做了什么

1. **`26c5cd6` log_fields 下沉** — message/字段拆分从 Tauri 壳（私有 visitor，无法单测）
   移到 `ftp_core::log_fields::event_parts()`，与 `progress.rs` 同一理由：前端管道属引擎层。
   新增 `tracing-subscriber` dev-dependency（与 workspace 同版本）。UI 日志现在带
   `k=v` 结构化字段（TFTP 传输完成不再只剩一句 "tftp send complete"）。
2. **`7f9f302` 测速修复**（核心 bug 修复）— 回环上速度曾稳定低估 3.1 倍（显示 7.8、
   实际 24.3 MB/s）。根因：`Date.now()` 分辨率 1ms，而 8192 B 块间隔 ~0.3ms，
   `Math.max(1, dt)` 把速率钉死成 `8192 B/ms = 8.19 MB/s`。修法：`MIN_SPEED_WINDOW_MS
   = 200`，窗口内只推进 bytes、不动窗口起点。纯逻辑抽到 `app/ui/src/lib/{transfer,format}.ts`。
3. **`390af64` 前端单测** — `node --test` 零框架直跑 `.ts`（Node 22 原生类型剥离），
   22 个测试覆盖格式化、文案、样本推进（含 600ms 仿真误差 <1% 的回归）。
   `package.json` 新增 `test` 与 `typecheck` 脚本。
4. **`33d83d3` round 6 验证 + 截图** — `deliverables/software-company/qa-verification-round6.md`
   与 `deliverables/software-company/transfer-speed-log-2026-09-30/` 截图
   （`01-log-view-size-elapsed-fields.png`、`02-footer-progress-speed.png`）。

## 三、接手必读：三条硬约束（违反即坏）

1. **`src/lib/` 内的 import 必须带 `.ts` 后缀**（Node ESM 不补扩展名）；
   tsconfig 已开 `allowImportingTsExtensions`（要求 `noEmit`，已满足）。
2. **`npm test` 显式列出测试文件**（`node --test <目录>` 不做目录发现，会
   MODULE_NOT_FOUND）；新增测试文件记得加进 `package.json`。
3. **被测模块不得 import `api.ts`**（顶层 `@tauri-apps/api` 在无 Tauri 运行时的
   进程里一加载就炸）——纯逻辑一律放 `src/lib/`。

另：`transfer.ts` 里 `speedBytes/speedAt` **只在窗口攒够时前移**，别"顺手"改成每条
事件刷新——那是退回 7.8 MB/s 假值的直接路径（注释已警告）。

## 四、待办

1. **发布面**：确认 v0.3.0 release 资产是否包含 d650ad9+（速度与日志增强）。
   若不包含：要么替换资产（重打包，`cd app && ui/node_modules/.bin/tauri build`，
   约 9 分钟，产物在仓库外 target-dir，手动复制进 `dist/`），要么留给 v0.3.1。
   ⚠️ `git push` / `gh` 必须走 xray 代理（`HTTPS_PROXY=http://127.0.0.1:10808`），
   且推送前 `git status -sb` 确认真正落后的提交数。
2. 5 个未推送提交 → 问用户。
3. 本轮 3 个 🟢（`code-review-speed-logfields-2026-09-30.md` §三）：read len=0 回 EOF、
   known_hosts 固定临时文件名、回环同向双流样本合并——均一行级，下次碰相关代码顺手即可。

## 五、关键文件导航（本轮新增）

| 文件 | 作用 |
|---|---|
| `crates/ftp-core/src/log_fields.rs` | tracing 事件 → (message, fields) 拆分，引擎层可单测 |
| `app/ui/src/lib/transfer.ts` | 测速窗口 + EMA + 传输文案（纯函数） |
| `app/ui/src/lib/format.ts` | `fmtBytes`（从 api.ts 搬来，纯函数） |
| `app/ui/src/lib/*.test.ts` | node --test 单测（22 个） |
| `app/ui/src/App.tsx` | 进度事件处理：`progressRef` 模式 + `isSameStream` 守卫 + 日志字段渲染 |
| `app/src-tauri/src/lib.rs`（FrontendLogLayer） | 现在调 `ftp_core::event_parts`，多转发 `fields` |

## 六、交接文档系列（按时间顺序，读法：先一后二，按需看三四）

1. `handoff-sftp-0.3.0-2026-09-29.md` — SFTP v0.3.0 实现、本机环境坑、验证配方
2. `handoff-sftp-2026-09-30.md` — 审查发现与待办（其 10 项已全部闭环）
3. `handoff-2026-09-30.md` — 发版流程、README、git/gh 代理配方、release 现状
4. 本文 — 速度显示 / 日志字段 / 前端单测体系

规格始终看 `docs/sftp-design.md`。

## 七、建议下一个 agent 加载的技能（Skill 工具）

- `code-review-skill` — 对待办 #3 的一行级改动做 Rust/TS 审查。
- `handoff` — 处理完发布后出第五篇增量交接。
- `webview2-tauri-headless-debug` — 若重新做 GUI 联调（发布前验证速度显示）。
