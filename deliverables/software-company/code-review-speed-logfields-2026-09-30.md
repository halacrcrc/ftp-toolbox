# 代码审查报告 — 传输速度显示 + 结构化日志字段 + 上轮审查整改（aee7ae4..33d83d3）

- 日期：2026-09-30（第二轮）
- 范围：`aee7ae4` 之后的 8 个提交——整改 `69aa7c5`、整改报告 `9c13ce7`、README `98d374c`、
  速度与日志字段 `d650ad9`、log_fields 下沉 `26c5cd6`、测速窗口修复 `7f9f302`、
  前端单测 `390af64`、round6 验证 `33d83d3`
- 基线：`main` @ `33d83d3`，工作区干净
- 验证：`cargo test -p ftp-core` **88 passed / 0 failed**（2026-09-30 实测）；
  `npm test` **22 passed**；`tsc --noEmit` 干净
- 前序：`code-review-sftp-2026-09-30.md`（第一轮，10 项发现）

---

## 一、结论

✅ **Approve**。上轮 3 个 🟡 与全部可执行 🟢 已正确闭环；本轮新代码（速度测量、日志字段）
质量高，根因分析和回归测试都做到了位。仅剩 3 个 🟢 小瑕疵，均不阻塞。

## 二、上轮发现的闭环核验（逐项）

| 上轮 # | 整改 | 核验结果 |
|---|---|---|
| 🟡1 read 裸分配 | `MAX_READ_CHUNK = 1 MiB` + `read_chunk_len()` 封顶 | ✅ 正确且合法：SFTP v3 允许短读（EOF 是独立状态），对端按下一 offset 重试；行为级测试验证"请求 4 GiB 返回真实文件长度" |
| 🟡2 connect 无超时 | `CONNECT_TIMEOUT = 15s` 包住 `connect_inner` | ✅ **选型正确**：用的是 `tokio::time::timeout` 死线而非 russh 的 `inactivity_timeout`——后者是**会话级**定时器，而本应用把长生命周期 `SftpClient` 存在 `AppState` 跨命令复用，挂 inactivity 定时器会杀掉"连上后用户去挑本地文件"的健康空闲会话。注释把这条推理写明了。另有"拒绝连接必须立刻失败而非等超时"的反向回归测试 |
| 🟡3 符号链接逃逸 | 代码注释 + `docs/sftp-design.md` §七 | ✅ 按建议落入规格"已知限制"，并写明威胁模型（服务端无建链入口，实际风险 = 共享目录本地属主放的链接） |
| 🟢5 RW 句柄方向混报 | `transfer_kind_for(write)` 三处共享 | ✅ open/read/close 方向一致，专门测试钉死 |
| 🟢6 空用户名必拒无提示 | `start_sftp_server` 开端口前拒绝 | ✅ 判定用 `is_empty()` 而非 `trim()`，与认证门**逐字对齐**，杜绝"启动放行但登录必拒"的错位——这个细节想得周到 |
| 🟢9 known_hosts 非原子写 | 临时文件 + rename，失败回退原地写 | ✅ rename 失败（杀软占用等）回退原地写并清临时文件，有"无 .tmp 残留"测试。见 §三🟢2 一个并发小瑕疵 |
| 🟢4/7/10 | 注释/文档/过时注释 | ✅ 均已处理 |

## 三、本轮新代码审查（速度与日志字段）

### 关键逻辑

1. **测速窗口（`app/ui/src/lib/transfer.ts`）**：`MIN_SPEED_WINDOW_MS = 200`。
   窗口内只推进 `bytes`、不动 `speedAt/speedBytes`；攒够后按整窗差值算速率，再叠
   EMA（0.7/0.3）。这是对 `Date.now()` 1ms 分辨率的**正确解法**：回环上 8192 B 块
   间隔约 0.3ms，旧代码 `Math.max(1, dt)` 把速率钉死在 `8192 B/1ms = 8.19 MB/s`
   （正好等于曾显示的 7.8 MiB/s——根因分析精确到能对上数字）。
2. **`progressRef` 模式（`App.tsx`）**：进度 effect 以 `[log]` 依赖只建一次，
   速度计算需要**当前**样本，闭包里的 React state 是 effect 运行时的旧值——用 ref
   同步读写是标准解法，注释解释了为什么。
3. **`isSameStream` 守卫**：文件或方向对不上就重开样本，挡住"两条流共用页脚单槽位"
   时把新文件字节折进旧文件速率的荒唐尖峰。
4. **`ftp_core::log_fields::event_parts`**：message/字段拆分从 Tauri 壳的私有 visitor
   下沉到引擎层（与 `progress.rs` 同一理由：前端管道属引擎，可单测）。`record_debug`
   与 `record_str` 双实现、Display 无引号 / Debug 有引号的差异都有测试钉死。
5. **前端单测零框架**：`node --test` 直接跑 `.ts`（Node 22 原生类型剥离），
   `@types/node` 仅为 tsc。三条硬约束（import 带 `.ts` 后缀、`npm test` 显式列文件、
   被测模块不得 import `api.ts`）都写进了文件头注释和 README。

### 🟢 发现（均不阻塞）

1. **`read_chunk_len(0)` 返回 0 → 服务端把 0 长度 READ 应答成 `SSH_FX_EOF`**
   （`server.rs::read`）。文件读到一半收到 len=0 请求会回 EOF，严格说是协议误导。
   实际无影响：没有任何真实客户端会发 len=0（那是无意义请求）。可选处理：len=0
   时回空 `Data` 或 `BadMessage`。一行的事，下次碰这块代码时顺手即可。
2. **known_hosts 临时文件名固定**（`known_hosts.tmp`）：两次并发 upsert 会在同一
   临时文件上互相踩。单用户桌面应用 + 命令皆由用户点击驱动 + 有回退兜底，实际
   几乎不可触发；加 PID/随机后缀即可闭合。
3. **回环同向双流仍会合并样本**：`isSameStream` 按 (file, kind) 分流，但"本应用客户端
   连本应用服务端"的上传中，两端进度事件**同文件同方向**（都是 SftpUpload），
   两个计数器仍折进同一个样本；计数器交替时字节回退 → 瞬时 0 → EMA 吸收（衰减
   到 70%）。这是单槽位页脚的固有折衷（上轮已记录），不影响正确性，知情即可。

### 🎉 值得点名

- 测速修复的"实测值对不上显示值 → 精确到 `8192 B/ms == 7.81 MiB/s` 的数字吻合"的
  定位方式，以及 600ms 仿真误差 <1% 的回归测试——这类时钟分辨率 bug 最容易
  "修了个大概"，这里没有。
- `advanceSample` 不改动入参、字节回退按 0 计、首窗直接取值不从 0 爬——边界
  行为全部有测试，测试名即文档。
- 前端文案阈值（<100ms 不报平均速度、"大小未知"明说）都是"噪声假装成精度"的
  正确取舍。
- `69aa7c5` 对整改项的逐项对应关系写在提交正文和 `change-report-sftp-2026-09-30.md`，
  审查者可以逐条核对，闭环成本低。

## 四、维护注意

- 改 `src/lib/` 内模块的 import 时**必须保留 `.ts` 后缀**（Node ESM 不补扩展名），
  tsconfig 已开 `allowImportingTsExtensions`；新增测试文件要加进 `package.json` 的
  `node --test` 列表（不支持目录发现）。
- `MIN_SPEED_WINDOW_MS` 的注释警告了"别把 speedBytes/speedAt 改成每条事件刷新"——
  那是退回 7.8 MB/s 假值的直接路径。
- 仓库 clippy / rustfmt 基线本来就是红的（既有告警），本轮改动文件本身干净；
  别整体 `cargo fmt`。
