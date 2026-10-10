/**
 * Typed bridge to the Rust backend.
 *
 * - Every `api.*` method maps 1:1 to a #[tauri::command] in
 *   app/src-tauri/src/lib.rs; keep names and argument casing in sync
 *   (Tauri passes JS camelCase objects straight to Rust snake_case params).
 * - Transfer progress is pushed, not polled: the backend emits
 *   "transfer-progress" events shaped like `TransferEvent`.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { documentDir } from "@tauri-apps/api/path";
import { open, save } from "@tauri-apps/plugin-dialog";
// 传输相关类型与文案搬到了 `src/lib/transfer.ts`（纯逻辑、可单测），这里只做
// 转出，方便调用方继续从 api 一处拿齐后端契约。
import type { TransferEvent, TransferKind } from "./lib/transfer";

export type { TransferEvent, TransferKind };

export interface NetInterface {
  /** 友好名：Windows 上为「以太网 / WLAN」，Linux/macOS 上为 eth0 / en0。 */
  name: string;
  /** 适配器描述（型号），同名网卡时用来区分；可能为空。 */
  desc: string;
  ip: string;
  /** 本机回环地址，仅本机可访问。 */
  loopback: boolean;
}

/**
 * 服务器运行态快照，由后端返回 —— 界面上的「运行中 / 已停止」只认这个，
 * 不再依赖本地 state（否则切页卸载组件就会把状态丢掉）。后端既支持查询，
 * 也会在状态变化时通过事件推一份同样结构的数据过来。
 */
export interface ServerStatus {
  running: boolean;
  /** 配置的监听地址（界面里填的） */
  addr: string | null;
  /** 实际绑定到的地址 */
  localAddr: string | null;
  root: string | null;
  /** 当前活跃会话数（FTP 控制连接 / TFTP 传输） */
  sessions: number;
  /** 一句话说明，可直接显示 */
  detail: string;
}

/**
 * SFTP 服务器运行态快照：镜像 ServerStatus，外加主机密钥信息
 * （密钥 load-or-generate 持久化在 app-data/keys/，与运行状态解耦，
 * 折叠指纹区靠它展示，见设计文档 §4.3）。
 */
export interface SftpServerStatus extends ServerStatus {
  hostKey: HostKeyInfo | null;
}

/** 被动端口段与系统保留段的比对结果。 */
export interface PassivePortCheck {
  /** 与「普通排除段」无重叠 —— 只有这类才会真正挡住绑定 */
  ok: boolean;
  /** 真正会挡住绑定的保留段，形如 "28385-28385" */
  conflicts: string[];
  /**
   * 系统托管排除段（netsh 里带 `*` 的，Hyper-V/WSL2 常用）。
   * 实测这类段不阻止绑定到具体地址，而 PASV 正是绑具体地址，所以只作提示。
   */
  managedConflicts: string[];
  /** 同长度、避开所有保留段的建议段，形如 "49900-49999" */
  suggested: string | null;
  /** 系统当前保留的 TCP 段数量（非 Windows 或读取失败时为 0） */
  reservedCount: number;
}

/** FTPS 自签证书信息。指纹变了 = 身份变了，对端会重新校验。 */
export interface CertInfo {
  /** SHA-256 指纹，冒号分隔的大写十六进制（OpenSSH 风格）。 */
  fingerprint: string;
  certPath: string;
  keyPath: string;
}

// ---------- SFTP（契约：docs/sftp-design.md §3；DTO 按现有惯例 camelCase 序列化） ----------

/** SFTP 主机密钥信息（服务端折叠区展示 / TOFU 校验用）。 */
export interface HostKeyInfo {
  /** 密钥算法，如 "ssh-ed25519" */
  algorithm: string;
  /** OpenSSH 风格指纹 "SHA256:<base64>"（与 `ssh-keygen -lf` 一致，便于跨工具核对） */
  fingerprint: string;
}

/** TOFU 主机密钥校验结果（sftp_client_check_host_key）。 */
export type HostKeyStatusKind = "known" | "unknown" | "changed";

export interface HostKeyStatus {
  status: HostKeyStatusKind;
  /** unknown / changed 时为服务器当前指纹；known 时为 null */
  fingerprint: string | null;
}

/** 一条已信任主机记录（known_hosts 行，sftp_client_list_known_hosts）。 */
export interface KnownHostRecord {
  /** `host:port`，host 已小写归一 */
  endpoint: string;
  fingerprint: string;
}

/** start_sftp_server 的选项，与后端 SftpServerOptions 字段一一对应。 */
export interface SftpServerOptions {
  bindAddr: string;
  port: number;
  username: string;
  password: string;
  /** OpenSSH 格式公钥行（ssh-ed25519 / ssh-rsa / ecdsa-*），与密码任一通过即放行 */
  authorizedKeys: string[];
  rootDir: string;
  readOnly: boolean;
}

export interface SftpServerInfo {
  port: number;
  addr: string;
  hostKey: HostKeyInfo | null;
}

/** SFTP 远端目录条目；字段与 FTP 客户端的列表行对齐，便于复用行渲染。 */
export interface SftpEntry {
  name: string;
  /** 条目类型（目录/文件/链接等），取值以后端实现为准 */
  fileType: string;
  size: number;
  /** 修改时间（Unix 秒）；未知为 null */
  mtime: number | null;
}

/** FTP 结构化列目录条目（MLSD facts / LIST 兜底解析，ftp_list_detailed）。 */
export interface FtpEntry {
  name: string;
  /** "file" | "dir" | "symlink" | "other" */
  kind: string;
  /** 字节；服务器没说（LIST 兜底切不出）为 null */
  size: number | null;
  /** Unix 秒；未知为 null */
  mtime: number | null;
}

/** local_walk 的返回：本地文件夹递归清单（文件夹上传用），相对路径用 POSIX 分隔符。 */
export interface LocalWalk {
  /** 目录（父先序：父目录总排在子目录前面）。 */
  dirs: string[];
  files: string[];
  /** 跳过的符号链接（可能成环，与下载侧策略对称）。 */
  skipped: string[];
}

/** Backend tracing event forwarded over "backend-log". */
export interface BackendLog {
  level: "TRACE" | "DEBUG" | "INFO" | "WARN" | "ERROR";
  target: string;
  message: string;
  /**
   * Structured tracing fields as `[name, value]` pairs (e.g. `bytes`,
   * `blocks`, `elapsed_ms`). Only the `message` used to be forwarded, so these
   * never reached the UI log view. Optional: some events carry no fields.
   */
  fields?: [string, string][];
}

export const api = {
  startFtpServer: (
    root: string,
    addr: string,
    user?: string,
    pass?: string,
    passivePorts?: string,
    ftps?: boolean,
    allowActiveMode?: boolean
  ) =>
    invoke<string>("start_ftp_server", {
      root,
      addr,
      user: user && user.length > 0 ? user : null,
      pass: pass ?? null,
      passivePorts: passivePorts && passivePorts.length > 0 ? passivePorts : null,
      ftpsEnabled: ftps ?? false,
      allowActiveMode: allowActiveMode ?? false,
    }),
  stopFtpServer: () => invoke<string>("stop_ftp_server"),
  ftpServerStatus: () => invoke<ServerStatus>("ftp_server_status"),
  startTftpServer: (root: string, addr: string) =>
    invoke<string>("start_tftp_server", { root, addr }),
  stopTftpServer: () => invoke<string>("stop_tftp_server"),
  tftpServerStatus: () => invoke<ServerStatus>("tftp_server_status"),
  /** 被动端口段（闭区间）是否与系统保留段冲突；Windows 之外总是 ok。 */
  checkPassivePorts: (start: number, end: number) =>
    invoke<PassivePortCheck>("check_passive_ports", { start, end }),

  /** 当前自签证书信息（首次调用会生成证书文件）。 */
  ftpsCertInfo: () => invoke<CertInfo>("ftps_cert_info"),
  /** 重新生成自签证书：指纹会变，已校验过旧指纹的对端会察觉。 */
  ftpsRegenerateCert: () => invoke<CertInfo>("ftps_regenerate_cert"),

  ftpConnect: (
    addr: string,
    user: string,
    pass: string,
    ftps?: boolean,
    acceptInvalidCerts?: boolean
  ) =>
    invoke<string>("ftp_connect", {
      addr,
      user,
      pass,
      ftps: ftps ?? false,
      acceptInvalidCerts: acceptInvalidCerts ?? false,
    }),
  ftpDisconnect: () => invoke<string>("ftp_disconnect"),
  ftpList: (path?: string) => invoke<string[]>("ftp_list", { path: path ?? null }),
  /** 结构化列目录（MLSD 优先，LIST 兜底），远端文件树数据源。 */
  ftpListDetailed: (path?: string) =>
    invoke<FtpEntry[]>("ftp_list_detailed", { path: path ?? null }),
  ftpUpload: (local: string, remote: string, transferId?: string) =>
    invoke<string>("ftp_upload", { local, remote, transferId: transferId ?? null }),
  ftpDownload: (remote: string, local: string, transferId?: string) =>
    invoke<string>("ftp_download", { remote, local, transferId: transferId ?? null }),
  /** 递归创建本地目录（批量下载时按远端结构建子目录）。 */
  createLocalDir: (path: string) => invoke<string>("create_local_dir", { path }),
  /** 本地路径是否是目录（拖拽上传分流：文件夹递归，文件直接传）。 */
  localIsDir: (path: string) => invoke<boolean>("local_is_dir", { path }),
  /** 递归遍历本地文件夹（文件夹上传用）。 */
  localWalk: (path: string) => invoke<LocalWalk>("local_walk", { path }),
  /** 远端创建一级目录（父目录须已存在；已存在会报错，前端不阻断）。 */
  ftpMkdir: (path: string) => invoke<string>("ftp_mkdir", { path }),

  tftpUpload: (server: string, local: string, remote: string, transferId?: string) =>
    invoke<string>("tftp_upload", { server, local, remote, transferId: transferId ?? null }),
  tftpDownload: (server: string, remote: string, local: string, transferId?: string) =>
    invoke<string>("tftp_download", { server, remote, local, transferId: transferId ?? null }),

  // ---------- SFTP server ----------
  // 命令契约见 docs/sftp-design.md §3；opts 对象字段按后端 DTO 的 camelCase
  // serde 惯例传（与顶层命令参数的自动蛇形转换无关，嵌套结构原样反序列化）。
  startSftpServer: (opts: SftpServerOptions) =>
    invoke<SftpServerInfo>("start_sftp_server", { opts }),
  stopSftpServer: () => invoke<string>("stop_sftp_server"),
  /** 镜像 ftp_server_status：挂载时查询初始快照，之后靠 sftp-server-state 事件推送。 */
  sftpServerStatus: () => invoke<SftpServerStatus>("sftp_server_status"),
  /** 重新生成主机密钥：指纹会变，已信任过旧指纹的对端会察觉。 */
  sftpServerRegenerateHostKey: () =>
    invoke<HostKeyInfo>("sftp_server_regenerate_host_key"),

  // ---------- SFTP client（TOFU 流程见设计文档 §2.4 / §4.4） ----------
  sftpClientCheckHostKey: (host: string, port: number) =>
    invoke<HostKeyStatus>("sftp_client_check_host_key", { host, port }),
  sftpClientConnect: (
    host: string,
    port: number,
    username: string,
    password: string,
    trustNewHost: boolean
  ) =>
    invoke<string>("sftp_client_connect", {
      host,
      port,
      username,
      password,
      trustNewHost,
    }),
  sftpClientDisconnect: () => invoke<string>("sftp_client_disconnect"),
  sftpClientList: (path?: string) =>
    invoke<SftpEntry[]>("sftp_client_list", { path: path ?? null }),
  /** 远端创建一级目录（父目录须已存在；已存在会报错，前端不阻断）。 */
  sftpClientMkdir: (path: string) => invoke<string>("sftp_client_mkdir", { path }),
  sftpClientUpload: (localPath: string, remotePath: string, transferId?: string) =>
    invoke<string>("sftp_client_upload", { localPath, remotePath, transferId: transferId ?? null }),
  sftpClientDownload: (remotePath: string, localPath: string, transferId?: string) =>
    invoke<string>("sftp_client_download", { remotePath, localPath, transferId: transferId ?? null }),
  /** 主机密钥变化后，用户显式确认才允许覆盖 known_hosts 记录。 */
  sftpClientUpdateKnownHost: (host: string, port: number) =>
    invoke<string>("sftp_client_update_known_host", { host, port }),
  /** 列出本机已信任的主机记录（TOFU known_hosts），供管理界面勾选。 */
  sftpClientListKnownHosts: () =>
    invoke<KnownHostRecord[]>("sftp_client_list_known_hosts"),
  /** 移除勾选的已信任主机记录（按 `host:port` 匹配）。 */
  sftpClientRemoveKnownHosts: (endpoints: string[]) =>
    invoke<string>("sftp_client_remove_known_hosts", { endpoints }),

  /** 请求取消一个进行中的传输（引擎在下一个分块边界落地）。 */
  cancelTransfer: (transferId: string) =>
    invoke<string>("cancel_transfer", { transferId }),

  listInterfaces: () => invoke<NetInterface[]>("list_interfaces"),
  /** 系统文档目录（Windows 为 %USERPROFILE%\Documents）。默认共享目录的基路径。 */
  documentsDir: () => documentDir(),
};

/** Open the native Windows folder picker; null when cancelled. */
export async function pickFolder(defaultPath?: string): Promise<string | null> {
  const selected = await open({ directory: true, multiple: false, defaultPath });
  return typeof selected === "string" ? selected : null;
}

/** Open the native Windows file picker (existing file); null when cancelled. */
export async function pickFile(defaultPath?: string): Promise<string | null> {
  const selected = await open({ directory: false, multiple: false, defaultPath });
  return typeof selected === "string" ? selected : null;
}

/** Open the native Windows "save as" dialog; null when cancelled. */
export async function pickSaveFile(defaultPath?: string): Promise<string | null> {
  const selected = await save({ defaultPath });
  return typeof selected === "string" ? selected : null;
}

/** Open the native Windows file picker (multi-select); null when cancelled. */
export async function pickOpenFiles(): Promise<string[] | null> {
  const selected = await open({ multiple: true });
  if (selected === null) return null;
  return Array.isArray(selected) ? selected : [selected];
}

/** Open the native Windows directory picker; null when cancelled. */
export async function pickOpenDirectory(): Promise<string | null> {
  const selected = await open({ directory: true });
  return typeof selected === "string" ? selected : null;
}

/** Directory part of a Windows/POSIX path, with the trailing separator kept. */
export function pathDir(p: string): string {
  const i = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
  return i > 0 ? p.slice(0, i + 1) : "";
}

/** Final segment of a Windows/POSIX path. */
export function pathBase(p: string): string {
  const i = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
  return i >= 0 ? p.slice(i + 1) : p;
}

/**
 * 远端条目名的本地落盘净化：把路径分隔符换成下划线。远端文件名是服务器
 * 说了算（Windows 明令禁止的 `\` `/` 也可能出现在异构服务器的列表里），
 * 拼接本地路径前不处理，恶意/异常服务器就能借文件名逃出用户选的目标目录。
 */
export function pathSafeName(name: string): string {
  // "." 与 ".." 不是合法文件名，会被路径解析吃掉——畸形服务器返回这种名字时
  // 防止批量下载写出目标目录之外（评审 #32）。
  if (name === "." || name === "..") return "_";
  return name.replace(/[\\/]/g, "_");
}

export function onTransferProgress(cb: (ev: TransferEvent) => void) {
  return listen<TransferEvent>("transfer-progress", (e) => cb(e.payload));
}

/** Frontend-generated id for one transfer invocation (cancellation key). */
export function newTransferId(): string {
  return crypto.randomUUID();
}

export function onBackendLog(cb: (ev: BackendLog) => void) {
  return listen<BackendLog>("backend-log", (e) => cb(e.payload));
}

export type ServerKind = "ftp" | "tftp" | "sftp";

/**
 * 订阅服务器运行态变化（启动、停止、意外退出、会话数增减）。
 *
 * 状态变化是后端主动推的，所以服务自己挂掉也会立刻反映到界面，不需要用户
 * 去点「刷新」或切页触发重新查询。SFTP 的快照比其余两种多一个 hostKey 字段
 * （SftpServerStatus），故做成泛型。
 */
export function onServerState<T extends ServerStatus = ServerStatus>(
  kind: ServerKind,
  cb: (status: T) => void
) {
  return listen<T>(`${kind}-server-state`, (e) => cb(e.payload));
}
