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
  name: string;
  ip: string;
}

/** Backend tracing event forwarded over "backend-log". */
export interface BackendLog {
  level: "TRACE" | "DEBUG" | "INFO" | "WARN" | "ERROR";
  target: string;
  message: string;
}

export const api = {
  startFtpServer: (root: string, addr: string, user?: string, pass?: string) =>
    invoke<string>("start_ftp_server", {
      root,
      addr,
      user: user && user.length > 0 ? user : null,
      pass: pass ?? null,
    }),
  stopFtpServer: () => invoke<string>("stop_ftp_server"),
  startTftpServer: (root: string, addr: string) =>
    invoke<string>("start_tftp_server", { root, addr }),
  stopTftpServer: () => invoke<string>("stop_tftp_server"),

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

export function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 ** 2) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 ** 3) return `${(n / 1024 ** 2).toFixed(1)} MB`;
  return `${(n / 1024 ** 3).toFixed(2)} GB`;
}
