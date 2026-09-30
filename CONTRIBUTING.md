# 贡献指南

感谢你考虑为 ftp-toolbox 提交改动。这份文档只讲**这个仓库特有的约定**；
Rust / TypeScript 的通用规范不在此赘述。

## 贡献许可（重要）

本项目以 **MIT 或 Apache-2.0 双许可**发布（SPDX：`MIT OR Apache-2.0`），使用者可任选其一。
全文见 [LICENSE-MIT](LICENSE-MIT) 与 [LICENSE-APACHE](LICENSE-APACHE)。

**除非你明确另行声明，否则你有意向本项目提交的任何贡献（代码、文档、测试等），
均按上述双许可授权，不附加任何额外条款或条件。**

请确保：

- 贡献是你自己的原创作品，或者你有权按上述条款提交；
- 若你的雇主或所在组织对你在职期间的作品主张权利，请先取得提交许可；
- 不要提交来源不明的代码 —— 一旦合并，许可证链条就断了，事后很难补救。

如果你不接受这一条，请不要提交 PR；可以先开 Issue 描述你的想法，由维护者自行实现。

## 开发环境

- Rust stable（≥ 1.89）
- Node.js ≥ 18（前端单测需要 ≥ 22，见下）
- Windows 另需 WebView2（Win10/11 通常已预装）

## 构建与运行

```bash
cd app/ui
npm install
npm run build          # 产出 app/ui/dist

cd ../..               # 回到仓库根目录
cargo run -p ftp-toolbox-app
```

调试构建（`tauri dev` / debug 二进制）走的是 `devUrl`，**必须先起前端 dev server**：

```bash
cd app/ui && npm run dev        # http://localhost:1420
```

## 测试

提交前请确保下面三条都是绿的：

```bash
cargo test -p ftp-core    # 引擎单测 + 回环集成测试
cargo check --workspace

cd app/ui
npm test                  # 前端纯逻辑单测
npm run typecheck         # tsc --noEmit
```

前端单测用的是 **Node 22+ 自带的 `node --test`**（直接加载 `.ts`，不需要 vitest / jest）。
因此 `app/ui/src/lib/*` 里的相对 import **必须带 `.ts` 后缀** —— Node 的 ESM 解析器
不做扩展名补全，Vite 打包不受影响。同理，被测模块**不要 import `api.ts`**
（它顶层 import `@tauri-apps/api`，在 Node 下会直接报错），纯逻辑一律放 `src/lib/`。

## 提交信息

用 [Conventional Commits](https://www.conventionalcommits.org/) 风格，**英文**：

```
feat(sftp): 支持客户端公钥认证
fix(tftp): 修复 blksize 协商失败时的偏移错乱
docs: 补充 .cargo/config.toml 的说明
```

subject 一行说清做了什么，正文可分节展开动机与取舍。

## 版本号

发版时四处必须同步，缺一处会导致产物版本与界面显示不一致：

- `app/src-tauri/tauri.conf.json`
- `app/src-tauri/Cargo.toml`
- `crates/ftp-core/Cargo.toml`
- `app/ui/package.json`

## 几点容易踩的坑

- **IPC 结构体要标 `#[serde(rename_all = "camelCase")]`**。漏标不会报错、不会崩，
  只会让前端读到 `undefined` —— 属于静默丢字段，只有端到端联调能发现。
- **别改仓库里的 `.cargo/config.toml`**。里面只放对所有贡献者都成立的设置；
  机器相关的东西（绝对路径、代理、镜像偏好）写进你自己的 `~/.cargo/config.toml`。
  想把 target 目录挪出仓库请用 `CARGO_TARGET_DIR` 环境变量（详见 README「打包安装包」）。
- `cargo fmt --check` 与 `cargo clippy -p ftp-core --all-targets` 在本仓库**目前不是全绿的**。
  请只对齐你自己新增/修改的代码，不要顺手整体 `cargo fmt` —— 那会产生大量无关 diff。
- 前端日志与进度条相关的纯逻辑请放 `crates/ftp-core`（如 `progress.rs`、`log_fields.rs`），
  这样能被单测覆盖；放 Tauri 壳里就得链接整个应用才能测。
