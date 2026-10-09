# 代码审查 0.3.3（e701518/ce828d8）：tsize 协商 + 传输取消/超时（2026-10-09）

## 审计范围与方法

- **提交**：`e701518`（主提交，29 文件 +745/-219）、`ce828d8`（README 架构图单行）。
  即 0.3.3 发版后的补审（`docs/code-review.md` 审查节点 4）。
- **方法**：逐文件读当前态 + `git show` 新旧对照；`cargo test` / `cargo check --workspace`
  / `npm test` / `npm run typecheck` 全部实跑；定向 `cargo fmt --check` 与
  `e701518^` 逐行比对区分基线；阅读 suppaftp 6.3.0 上游源码核实
  `put_with_stream` / `finalize_put_stream` / `DataStream` 契约。
- **评审人**：独立评审（architect-review agent），非提交作者本轮自审。

## 整体评估

⚠️ 取消机制的分层设计（引擎纯逻辑 / 壳层仅管令牌表 / 前端只持 id）方向正确，
`check`+`chunk` 双保险与 `.part` 原子落盘都经得起推敲，四条验证命令全绿。但存在
**2 个真实缺陷**：取消上传/下载后 FTP 控制通道残留未读响应导致后续命令错位
（High），以及首个 TFTP 应答来源未校验即 `connect`（High，安全）；另有取消路径
不发 `TransferEvent::Error` 导致进度条卡死等 5 项中低危与一批测试缺口。
（全部发现已于同日整改闭环，见 `change-report-033-cancel-tsize-2026-10-09.md`。）

## 发现清单

### 2026-10-09 #1 取消 FTP 上传/下载后控制通道残留未读响应，后续命令错位（🟠 High）

- **位置**：`crates/ftp-core/src/ftp/client.rs`（上传/下载错误分支）
- **分析**：suppaftp 的 `finalize_put_stream` / `finalize_retr_stream`（上游
  async_ftp/mod.rs L530-540、L463-472）都先 `drop(stream)` 再
  `read_response_in(&[ClosingDataConnection, RequestedFileActionOk])` —— 服务器在
  数据连接关闭后**必然**在控制通道回一行 226。取消路径只 `close()` 了数据通道，
  没有读这一行：它滞留在响应缓冲里，下一条命令（如 LIST）把它当成自己的应答 →
  `UnexpectedResponse`，且错位持续传导。**失败用例实测命中**：取消上传后
  `list()` 返回 `Ftp(UnexpectedResponse(226 "File successfully written"))`。
- **修复**：✅ `ae04fb0` —— 错误分支补 `read_response_in`（接受 226/250/426，读错误
  忽略），回归测试 `cancelled_transfer_leaves_control_channel_usable` 钉住
  上传/下载两个方向。

### 2026-10-09 #2 TFTP 客户端接受首个应答的任意来源并直接 `connect`（🟠 High，安全）

- **位置**：`crates/ftp-core/src/tftp/client.rs`（下载/上传握手）
- **分析**：`recv_from` 后未校验 `peer` 直接 `sock.connect(peer)`。同网段主机可
  抢先伪造首个应答（伪造 OACK 谎报 tsize / ERROR），把客户端 socket 导向攻击者：
  下载写入外部控制的内容且 UI 显示正常完成；上传本地文件内容外泄。服务端侧
  无此问题（`peer` 必然是请求方）。历史沿革非本次新引入，但本次扩展了 tsize
  路径并把上传纳入取消体系，风险面扩大。
- **修复**：✅ `576d626` —— 握手期逐包校验来源 IP（RFC 1350 应答来自新 TID，端口
  必不同），伪造包跳过且不重传请求；纯函数 `first_reply_is_from_request_host`
  放 `tftp/mod.rs` 并配 4 例单测（新 TID 放行 / 异主机拒绝 / 主机名解析 / 不可
  解析保守拒绝）。

### 2026-10-09 #3 取消路径不发 `TransferEvent::Error`，前端进度条永久卡住（🟡 Medium）

- **位置**：`crates/ftp-core/src/ftp/client.rs`
- **分析**：SFTP/TFTP 错误路径都发 `Error` 事件，唯独 FTP 客户端不发（旧有缺陷
  被新功能放大）：`App.tsx` 只在 `case "error"` 里 `apply(null)`，无事件 → 进度条
  常驻卡死（取消按钮随 `activeTransferId` 清理消失，但进度条不复位）。
- **修复**：✅ `ae04fb0` —— 上传错误分支、下载外层 `Err` 分支均补发 `Error` 事件，
  与 SFTP 侧行为对齐；前端零改动自愈。

### 2026-10-09 #4 壳层 `register_cancel` 与传输启动之间的竞态窗口（🟡 Medium）

- **位置**：`app/src-tauri/src/lib.rs`（4 个带会话互斥的传输命令）
- **分析**：令牌在拿 `ftp_client`/`sftp_client` 锁**之前**注册：排队中的传输也会
  亮取消按钮（用户以为在取消 A，实际取消尚未开始的 B）；排队期间取消 → 令牌
  `check` 前已生效（结果尚正确）或已 `unregister`（返回"没有找到该传输"的红错）。
  另：`register_cancel` 静默覆盖同 id 旧令牌。`cancels` 的 `std::Mutex` 未跨
  `.await` 持锁（MutexGuard 非 Send，编译器兜底），当前安全。
- **修复**：✅ `c2c2676` —— 4 个互斥命令改为**拿锁后注册**（TFTP 无会话锁、立即
  启动，维持原样）；重复 id 改为 warn 日志 + `old.cancel()`（旧传输保持可取消）；
  `cancels` 字段注释写明"不得跨 await 持锁"约束。

### 2026-10-09 #5 TFTP 上传主循环内 `file.read` 未被空闲超时/取消覆盖（🟡 Medium）

- **位置**：`crates/ftp-core/src/tftp/client.rs`（上传主循环）
- **分析**：块边界有 `check`，但本地文件读是裸 await —— 磁盘读卡住时既不取消也
  不超时，与 `cancel.rs` 模块文档"每个分块操作都有上界"不一致。
- **修复**：✅ `576d626` —— `file.read` 改走 `cancel::chunk`，失败路径
  `emit_err`。

### 2026-10-09 #6 TFTP 经典路径首个短块不发 `Progress` 事件（🟢 Low）

- **位置**：`crates/ftp-core/src/tftp/client.rs`（`first_data` 分支）
- **分析**：对接无选项服务器时，单块小文件下载只有 `Started` + `Done`，无任何
  中间反馈（`Done` 携带 bytes，无数据丢失）。
- **修复**：✅ `576d626` —— `first_data` 分支补发与主循环同形的 `Progress`。

### 2026-10-09 #7 README 架构图断言与 CSS 例外规则作用面（🟢 Low）

- **分析**：`ce828d8` 的"RFC 1350 + RFC 2347/2348/2349"断言成立（`packet.rs` 有
  `negotiated_blksize`(2348)、`negotiated_tsize`/`requests_tsize`(2349)、`Oack`
  opcode 6(2347)），且与「已知限制」的 timeout 说明自洽 —— **文档提交通过**。
  `.card-title + .hint-line` 例外规则逐处核对 6 个使用点：仅 SFTP 设置卡的紧前
  兄弟是 `.card-title`，其余 5 处前置兄弟为 `.card-head`/`.field` 等，**无回归**。
- **处置**：✅ `7fe90f0` —— CSS 注释改为描述规则本身（"任何直接跟在标题后的说明
  行"），不再绑定单一视图举例。

### 2026-10-09 #8 JSX 空白陷阱复核（🟢，未触犯）

- `App.tsx` / `ProgressBar.tsx` 本轮新增 JSX 逐处核对：三段进度文案各自独占一行
  且前段自带尾随空格、后段自带前导空格，不发生粘连；取消按钮文本前无表达式。
  **未触犯红线**。建议在 ProgressBar 补注释防后人误删空格（未做，风险极低）。

### 2026-10-09 #9 TFTP 服务端 `open_rrq` 提前后错误码语义变化（🟢，设计取舍，不修）

- 缺文件从"OACK 后才 ERROR(1)"提前为"握手前 ERROR(1)"，严格更符合 RFC 2348 §4
  （服务器可在 OACK 前拒绝），客户端少等一个 RTT。路径穿越回 2（Access
  violation）而非 1：语义恰当且不泄露文件存在性。WRQ 提前拒绝只终止该会话任务，
  监听主循环与其他会话不受影响（已核实 spawn 结构）。**判定为改进**。
- ✅ `576d626` 顺带把"穿越用 2 而非 1"的理由写进代码注释固化。

### 2026-10-09 #10 取消令牌与 `first_data`/OACK 分叉边界（🟢，已确认无缺陷）

- OACK 后 `last_ack` 初始化为 `Ack{0}`（RFC 2348 要求的重传 ACK(0)）✓；经典路径
  `want` 从 2 起、`last_ack=Ack{1}` 与已发 ACK 一致 ✓；`want.wrapping_sub(1)` 回
  绕安全（65535 块 × 8192 B ≈ 512 MB 与 mod.rs 声明吻合）✓；tsize 非数字时
  `negotiated_tsize` 返回 `None` → 降级为未知大小的转圈进度，不中断传输 ✓
  （`packet.rs` 单测钉住）。**记录以免后续误改**。

## 验证结果（整改前实跑）

| 命令 | 结果 |
| --- | --- |
| `cargo test -p ftp-core` | ✅ 全绿（92→95 例）；**失败用例先行**：`cargo test --test ftp_data_channel` 在整改前以 `UnexpectedResponse(226)` 确诊 #1 |
| `cargo check --workspace` | ✅ Finished，无 warning |
| `npm test` | ✅ 22/22 |
| `npm run typecheck` | ✅ 通过 |
| `cargo fmt --check`（定向） | ✅ 本次改动引擎文件零 diff；`lib.rs` 偏差经与 `e701518^` 比对为既有基线（豁免），未整体 fmt |
| 红线逐条 | IPC camelCase ✅ / `lib/*.ts` 后缀 ✅ / JSX 空白 ✅（#8）/ 服务端生命周期 ✅ / 纯函数边界 ✅ / `prefs.iface` ✅ / 主动模式默认关 ✅ / rcgen feature ✅ / 版本 5 处 ✅ |

## 逐提交结论

| 提交 | 类型 | 结论 |
| --- | --- | --- |
| `e701518` | 代码（29 文件） | ⚠️ 架构方向正确、分层干净，但 #1/#2 两个 High 需整改（已于同日闭环）；#3–#5 限期整改（已闭环）；#9/#10 为已核实的设计取舍/正确边界 |
| `ce828d8` | 纯文档（1 行） | ✅ 通过（#7，断言逐条核实成立） |

## 测试缺口（评审识别，整改情况见 change-report）

1. FTP 取消后再发命令（#1 回归测试，已补）
2. FTP 上传取消 → 远端半截文件处置（未补：依赖服务器实现，已在 UI 提示层面无要求，记录在案）
3. FTP/SFTP 上传与下载的取消用例（SFTP/FTP 上传取消未单独覆盖，机制与已测下载同构）
4. TFTP 上传取消（未补）
5. TFTP 经典路径（无选项服务器）`first_data` 分支（#6 顺带修复，路径测试未补）
6. `cancel.rs` 自身单测（已补：5 例）
7. 壳层 `cancel_transfer` 的 id 生命周期（未补：壳层不便单测，建议后续把令牌表抽成纯结构）
8. TFTP 服务端 WRQ 超限预拒绝 / RRQ 缺文件提前 ERROR(1)（未补）
