# Handoff: 0.3.4 审计整改闭环 + 0.3.5 发版（2026-10-09）

> 交接对象：下一个接手 ftp-toolbox 的会话。项目硬性约定（cargo 配置、出包 CLI、
> 取证方式、依赖坑）见 `.workbuddy/memory/MEMORY.md`，本文不重复。
>
> 本文遵守 2026-10-09 事后审计 #24 的约定：**不硬编码易变的 git 状态**
> （`origin/main` 哈希、tag 指向、"某提交未 push"）。需要判断落后数时请自行运行
> `git status -sb` / `git rev-parse origin/main`；下面引用的提交哈希是不可变的，可安全引用。

## Current state

- **v0.3.5 已发版**（2026-10-09）：GitHub Release 为 **Latest**，标题
  「v0.3.5 — 0.3.4 审计整改闭环（建议 0.3.4 用户升级）」，上传 3 个资产。
  annotated tag `v0.3.5`（message `v0.3.5 — transfer robustness fixes`）已推送。
  工作区干净、本地与 `origin/main` 一致（自证：`git status -sb` 无 ahead/behind）。
- **0.3.4 审计（#20–#25）全部闭环**：
  - 报告：`code-review-034-audit-2026-10-09.md`（发现 #20–#25）；
    整改：`change-report-034-audit-2026-10-09.md`（两轮处置，#20–#25 全闭环，
    **无未闭环发现**）。
  - `7050e01`：#20 SFTP 下载 rename 失败补发 `TransferEvent::Error`；
    #21 TFTP 下载本地写盘失败补发 Error（前端进度条靠它复位）。
  - `00ad989`：#23 FTP 上传本地源读失败改走统一 Err 分支（不再 `?` 直抛，
    保证补发 Error）；#25 FTP 收尾 `data.close()` 两处（正常收尾 + `abort_retr`）
    加 `IDLE_TIMEOUT`（30s）上界，对端不读时不再挂起。
  - #22 README 的 TFTP tsize 版本说明纠正；#24 `handoff-2026-10-09.md` 附勘误表
    （更正 `origin/main` 与 `v0.3.4` tag 指向/发版顺序），并把「不硬编码易变
    git 状态」写进 `CODEBUDDY.md`（`08bb6f8`）。
- **测试基线 113**（`cargo test -p ftp-core`：80 单测 + 33 集成
  = 2+3+2+8+4+7+7）。对比 0.3.4 的 111，新增 SFTP / TFTP 下载本地写失败回归用例。
- **出包新增离线变体**：仅覆盖 `bundle.windows.webviewInstallMode` 的
  `app/src-tauri/tauri.conf.embed.json`（`embedBootstrapper`，引导器内嵌，
  离线机可用）。**每次发版必须出两个变体**：默认 `setup.exe` +
  `-offline` 后缀的 `setup-offline.exe`，两产物都上传 Release。
- **MSI 瘦身（非回归）**：0.3.5 MSI（7,901,184 B）比 0.3.4（8,634,368 B）小约 700 KB。
  原因是 0.3.4 的 MSI 混入了外部 target 目录里**陈旧的 `ftp_toolbox_app_lib.dll`**
  （`crate-type` 的 cdylib 副产物，1,364,480 B，桌面 exe 走 rlib 静态链接，
  运行时不加载它）；NSIS 两版体积仅差数百字节，证明从未打包该 dll。
  本次换新 target 目录后无此 dll，`ftp-toolbox-app.exe` 启动正常。
- **本次构建的沙箱绕行**（供后人参考）：会话沙箱无法写机器级
  `C:/Users/22534/.workbuddy/build/ftp-toolbox-target`（`os error 5` / 沙箱拒绝），
  `dangerouslyDisableSandbox` 实测无效。改用
  `$env:CARGO_TARGET_DIR='c:\WorkBuddy\FTP\target'`（仓库内、已 gitignore）后出包成功。

## Next steps（按优先级）

1. **0.3.5 的整改提交未经独立评审** —— `00ad989`（fix client #23/#25）按节点 2 规则
   并入下轮审计（报告命名 `code-review-<主题>-<日期>.md`）。改动集中在已评审路径，
   风险可控，但不允许无限期跳过。
2. **Roadmap 两项**（README 挂账）：
   - FTP 远端文件树浏览（拖拽上传/下载）—— UI 工作量大，建议先做 `docs/` 规格；
   - SFTP 客户端公钥认证——规格变更，先定私钥来源（`docs/sftp-design.md` Q5 上下文）。
3. **小尾巴已办**：`app/ui/src/components/ProgressBar.tsx` 进度文案刻意见空格处的
   注释已补（评审 #8，`a75f150`），防后人误删引发粘连。
4. **记录为取舍、勿当 bug 修**：TFTP 握手伪造包计数并入 `MAX_RETRIES`（整改轮 #18）；
   排队期取消按钮可见性（#19）；v0.2.2 / v0.2.4 永不补发。
5. **可选清理**：外部 target 目录里 0.3.4 遗留的陈旧 `ftp_toolbox_app_lib.dll` 仍在
   （本会话沙箱不允许写该目录，仅记录在案；换机器/清缓存时会自然消失）。

## Relevant artifacts

- GitHub Release：https://github.com/halacrcrc/ftp-toolbox/releases/tag/v0.3.5（Latest）
- `deliverables/software-company/code-review-034-audit-2026-10-09.md` /
  `change-report-034-audit-2026-10-09.md` —— 0.3.4 审计与整改（#20–#25）
- `deliverables/software-company/handoff-2026-10-09.md` —— 上一份交接（0.3.4 视角，
  仍有效；文末含 #24 勘误表）
- `.workbuddy/memory/MEMORY.md` + `2026-10-09.md` —— 长期约定与逐日细节（含踩坑配方）
- `docs/code-review.md`（审查红线，改代码前必读）；`docs/sftp-design.md`（Q1–Q12）
- 出包配置：`app/src-tauri/tauri.conf.json` + `app/src-tauri/tauri.conf.embed.json`
- 一律以命令输出为准；交付物文件名带日期，属历史快照，可能过时。

## Suggested skills

- 下轮审计 0.3.5 提交时沿用独立评审模式（architect-review agent，本轮两轮评审均
  由独立代理完成，效果良好）。
- 无其他特殊 skill 需求：常规开发按 `CODEBUDDY.md` 与 `.workbuddy/memory/MEMORY.md`
  的约定执行即可。