# 整改报告：0.3.3 评审发现（2026-10-09）

对应评审：`code-review-033-cancel-tsize-2026-10-09.md`（2026-10-09 #1–#10）。
全部代码改动为 4 个提交，均已复验。

## 逐条闭环

| # | 严重度 | 标题 | 处置 | 提交 | 复验 |
| --- | --- | --- | --- | --- | --- |
| #1 | 🟠 | FTP 取消后控制通道错位 | 修复：abort 路径 `read_response_in` 排空 226/426/550 | `ae04fb0` | ✅ 失败用例先行（整改前实测红：`UnexpectedResponse(226 "File successfully written")`），修复后 `cancelled_transfer_leaves_control_channel_usable` 两方向（取消上传 + 取消下载 → LIST 成功）转绿 |
| #2 | 🟠 | TFTP 首包来源未校验 | 修复：握手逐包校验来源 IP，伪造包跳过不重传 | `576d626` | ✅ `first_reply_is_from_request_host` 4 例单测（同主机新 TID / 异主机 / 主机名 / 不可解析）；回环全绿 |
| #3 | 🟡 | FTP 取消不发 Error 事件 | 修复：上传错误分支 + 下载外层 Err 分支补发 `TransferEvent::Error` | `ae04fb0` | ✅ 前端零改动自愈（`case "error"` → `apply(null)`）；编译 + 全套测试绿 |
| #4 | 🟡 | 拿锁前注册令牌的竞态窗口 | 修复：4 个互斥命令改为拿锁后注册；重复 id warn + 旧令牌 cancel；锁约束入注释 | `c2c2676` | ✅ `cargo check --workspace` 绿（MutexGuard 非 Send 兜底未破）；TFTP 无会话锁维持即时注册 |
| #5 | 🟡 | TFTP 上传 `file.read` 无上界 | 修复：改走 `cancel::chunk` | `576d626` | ✅ 全套测试绿 |
| #6 | 🟢 | 经典路径无 Progress | 修复：`first_data` 分支补发 | `576d626` | ✅ 全套测试绿 |
| #7 | 🟢 | CSS 注释绑定单一视图举例 | 注释一般化 | `7fe90f0` | ✅ 纯注释 |
| #8 | 🟢 | JSX 空白复核 | 不修（未触犯） | — | typecheck + 22 前端测试绿 |
| #9 | 🟢 | open_rrq 错误码时机 | 不修（判定为改进）；理由入注释 | `576d626` | 回环绿 |
| #10 | 🟢 | OACK/经典分叉边界 | 不修（已确认正确）；记录在案 | — | 既有回环断言覆盖 |

## 复验（2026-10-09 实跑，全部提交后）

| 命令 | 结果 |
| --- | --- |
| `cargo test -p ftp-core --all-targets` | **105 passed / 0 failed**（单测 78：含 cancel.rs 新增 5 例、tftp/mod.rs 来源校验 4 例；回环与数据通道 27：含 `cancelled_transfer_leaves_control_channel_usable`） |
| `cargo check --workspace` | Finished |
| `cd app/ui && npm test` | 22/22 |
| `cd app/ui && npm run typecheck` | 通过 |

## 遗留（记录在案，不阻塞）

- 测试缺口 #2/#3/#4/#5/#7/#8（见评审报告「测试缺口」清单）：FTP/SFTP 上传取消、
  TFTP 上传取消、经典路径端到端、壳层令牌表单测化、TFTP 服务端 WRQ 超限与
  RRQ 缺文件用例 —— 机制均与已测路径同构，列入下轮补测清单。
- `ProgressBar` 进度文案的三段刻意见空格建议补注释（#8，极低风险）。
- 本轮整改提交（`ae04fb0`/`576d626`/`c2c2676`/`7fe90f0`）未经独立评审；
  按节点 2 规则并入下轮审计。
