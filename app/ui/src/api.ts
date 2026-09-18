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
import { open } from "@tauri-apps/plugin-dialog";

export type TransferKind = "upload" | "download";

export interface TransferEvent {
  phase: "started" | "progress" | "done" | "error";
  kind: TransferKind;
  file: string;
  bytes?: number;
  total?: number | null;
  message?: string;
}

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

/** Backend tracing event forwarded over "backend-log". */
export interface BackendLog {
  level: "TRACE" | "DEBUG" | "INFO" | "WARN" | "ERROR";
  target: string;
  message: string;
}

export const api = {
  startFtpServer: (
    root: string,
    addr: string,
    user?: string,
    pass?: string,
    passivePorts?: string
  ) =>
    invoke<string>("start_ftp_server", {
      root,
      addr,
      user: user && user.length > 0 ? user : null,
      pass: pass ?? null,
      passivePorts: passivePorts && passivePorts.length > 0 ? passivePorts : null,
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

  ftpConnect: (addr: string, user: string, pass: string) =>
    invoke<string>("ftp_connect", { addr, user, pass }),
  ftpDisconnect: () => invoke<string>("ftp_disconnect"),
  ftpList: (path?: string) => invoke<string[]>("ftp_list", { path: path ?? null }),
  ftpUpload: (local: string, remote: string) =>
    invoke<string>("ftp_upload", { local, remote }),
  ftpDownload: (remote: string, local: string) =>
    invoke<string>("ftp_download", { remote, local }),

  tftpUpload: (server: string, local: string, remote: string) =>
    invoke<string>("tftp_upload", { server, local, remote }),
  tftpDownload: (server: string, remote: string, local: string) =>
    invoke<string>("tftp_download", { server, remote, local }),

  listInterfaces: () => invoke<NetInterface[]>("list_interfaces"),
};

/** Open the native Windows folder picker; null when cancelled. */
export async function pickFolder(defaultPath?: string): Promise<string | null> {
  const selected = await open({ directory: true, multiple: false, defaultPath });
  return typeof selected === "string" ? selected : null;
}

export function onTransferProgress(cb: (ev: TransferEvent) => void) {
  return listen<TransferEvent>("transfer-progress", (e) => cb(e.payload));
}

export function onBackendLog(cb: (ev: BackendLog) => void) {
  return listen<BackendLog>("backend-log", (e) => cb(e.payload));
}

export type ServerKind = "ftp" | "tftp";

/**
 * 订阅服务器运行态变化（启动、停止、意外退出、会话数增减）。
 *
 * 状态变化是后端主动推的，所以服务自己挂掉也会立刻反映到界面，不需要用户
 * 去点「刷新」或切页触发重新查询。
 */
export function onServerState(kind: ServerKind, cb: (status: ServerStatus) => void) {
  return listen<ServerStatus>(`${kind}-server-state`, (e) => cb(e.payload));
}

export function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 ** 2) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 ** 3) return `${(n / 1024 ** 2).toFixed(1)} MB`;
  return `${(n / 1024 ** 3).toFixed(2)} GB`;
}
