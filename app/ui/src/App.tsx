import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  onBackendLog,
  onServerState,
  onTransferProgress,
  ServerStatus,
  SftpServerStatus,
} from "./api";
// 传输的样本推进与日志文案都在 src/lib/transfer.ts（纯函数，带单测）。
import {
  advanceSample,
  doneSuffix,
  elapsedSeconds,
  isSameStream,
  newSample,
  sizeSuffix,
  transferLabel,
} from "./lib/transfer";
import type { Progress } from "./lib/transfer";
import Sidebar, { ViewKey } from "./components/Sidebar";
import ProgressBar from "./components/ProgressBar";
import FtpPage from "./views/FtpPage";
import TftpPage from "./views/TftpPage";
import SftpPage from "./views/SftpPage";
import LogView from "./views/LogView";

export interface LogEntry {
  id: number;
  time: string;
  level: "info" | "ok" | "error";
  text: string;
}

const TITLES: Record<ViewKey, string> = {
  ftp: "FTP",
  tftp: "TFTP",
  sftp: "SFTP",
  logs: "运行日志",
};

// 文档目录解析失败时的兜底基路径（documentDir 在 Windows 上几乎不会失败，
// 这是极端情况的保底；非 Windows 留空走「让用户自填」流程）。
const IS_WINDOWS = navigator.userAgent.includes("Windows");

// 单调递增的日志 id：slice(-499) 截断后行内容整体平移，若用数组下标作 key
// 会导致所有行的 key 错位。id 跟随行本身，重渲染即稳定。
let nextLogId = 1;

export default function App() {
  const [view, setView] = useState<ViewKey>("ftp");
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [progress, setProgress] = useState<Progress | null>(null);
  // Read synchronously by the progress handler (see the effect below).
  const progressRef = useRef<Progress | null>(null);
  // Cancellation key of the transfer currently in flight (set by the client
  // views around their invoke; cleared when it settles). Non-null enables
  // the footer cancel button.
  const [activeTransferId, setActiveTransferId] = useState<string | null>(null);
  // Server run state lives here (App never unmounts) and is pushed by the
  // backend. The protocol pages themselves stay mounted across navigation
  // (hidden via [hidden], see render below) so component-local client state —
  // connection flag, form inputs, listings — survives view switches: the
  // backend holds ftp/sftp client sessions in global AppState, so losing the
  // UI flag used to present as "connection dropped" after visiting the log
  // page, and TFTP inputs snapped back to defaults on remount.
  const [ftpStatus, setFtpStatus] = useState<ServerStatus | null>(null);
  const [tftpStatus, setTftpStatus] = useState<ServerStatus | null>(null);
  const [sftpStatus, setSftpStatus] = useState<SftpServerStatus | null>(null);
  // 默认目录的基路径（系统文档目录）。documentDir 是异步的，而页面只在挂载
  // 时读一次默认值（useState 初值、localStorage 兜底），所以解析完成前先不
  // 挂载协议页 —— 常驻挂载下晚挂载没有任何代价。null = 还在解析。
  const [defaultBase, setDefaultBase] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    api
      .documentsDir()
      .then((dir) => {
        // 结尾的分隔符去掉，后面统一用 joinDefault 拼子路径
        if (!cancelled) setDefaultBase(dir.replace(/[\\/]+$/, ""));
      })
      .catch(() => {
        if (!cancelled) setDefaultBase(IS_WINDOWS ? "C:\\Users\\Public" : "");
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const log = useCallback((text: string, level: LogEntry["level"] = "info") => {
    // keep at most 500 entries so the log view never grows unbounded
    setLogs((ls) => [
      ...ls.slice(-499),
      { id: nextLogId++, time: new Date().toLocaleTimeString(), level, text },
    ]);
  }, []);

  // Ask the backend what is actually running (single source of truth), one
  // protocol at a time: each page only refreshes its own server status.
  const refreshFtp = useCallback(async () => {
    try {
      setFtpStatus(await api.ftpServerStatus());
    } catch (e) {
      log(`读取 FTP 服务器状态失败: ${e}`, "error");
    }
  }, [log]);

  const refreshTftp = useCallback(async () => {
    try {
      setTftpStatus(await api.tftpServerStatus());
    } catch (e) {
      log(`读取 TFTP 服务器状态失败: ${e}`, "error");
    }
  }, [log]);

  const refreshSftp = useCallback(async () => {
    try {
      setSftpStatus(await api.sftpServerStatus());
    } catch (e) {
      log(`读取 SFTP 服务器状态失败: ${e}`, "error");
    }
  }, [log]);

  useEffect(() => {
    if (view === "ftp") void refreshFtp();
    else if (view === "tftp") void refreshTftp();
    else if (view === "sftp") void refreshSftp();
  }, [view, refreshFtp, refreshTftp, refreshSftp]);

  // Run state is *pushed* by the backend, so it stays correct without a manual
  // refresh: a server that dies on its own, or clients connecting and
  // disconnecting, show up as they happen.
  useEffect(() => {
    const unlisten = [
      onServerState("ftp", setFtpStatus),
      onServerState("tftp", setTftpStatus),
      onServerState<SftpServerStatus>("sftp", setSftpStatus),
    ];
    return () => {
      unlisten.forEach((p) => p.then((f) => f()));
    };
  }, []);

  // Single global subscription: the backend pushes TransferEvents for all
  // transfers; we mirror them into the footer progress bar and the log.
  //
  // `progressRef` shadows the state because this effect is built once (`[log]`
  // deps) and the speed/elapsed maths needs the *current* sample — React state
  // read from the closure would be the one captured when the effect ran.
  useEffect(() => {
    const apply = (next: Progress | null) => {
      progressRef.current = next;
      setProgress(next);
    };
    const unlisten = onTransferProgress((ev) => {
      // An event for a different file means either a fresh transfer whose
      // "started" we missed, or two streams interleaving into this single slot
      // (the loopback case). Never fold its byte count into the previous
      // file's rate — that produces a nonsense speed spike.
      const prev = isSameStream(progressRef.current, ev) ? progressRef.current : null;
      switch (ev.phase) {
        case "started":
          apply(newSample(ev, Date.now()));
          log(`${transferLabel(ev.kind)}开始: ${ev.file}${sizeSuffix(ev.total ?? null)}`);
          break;
        case "progress": {
          const now = Date.now();
          // Instantaneous rate + EMA smoothing, plus the "same stream" guard,
          // all live in advanceSample/newSample (see src/lib/transfer.ts).
          apply(advanceSample(prev ?? newSample(ev, now), ev, now));
          break;
        }
        case "done": {
          const bytes = ev.bytes ?? 0;
          const elapsed = elapsedSeconds(prev, Date.now());
          log(`完成: ${ev.file}（${doneSuffix(bytes, elapsed)}）`, "ok");
          apply(null);
          break;
        }
        case "error":
          log(`传输错误: ${ev.file}: ${ev.message ?? "未知错误"}`, "error");
          apply(null);
          break;
      }
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, [log]);

  /** Ask the backend to abort the current transfer; engine lands the abort
   *  at the next chunk boundary. Errors (e.g. already finished) just log. */
  const cancelActiveTransfer = useCallback(() => {
    const id = activeTransferId;
    if (!id) return;
    log("已请求取消当前传输…");
    api.cancelTransfer(id).catch((e) => log(`取消传输失败: ${e}`, "error"));
  }, [activeTransferId, log]);

  // Backend tracing events (engine + libunftp): the detailed half of the
  // log view — server sessions, per-transfer byte/block/elapsed stats, etc.
  useEffect(() => {
    const unlisten = onBackendLog((ev) => {
      if (!ev.message) return;
      const level = ev.level === "ERROR" || ev.level === "WARN" ? "error" : "info";
      // shorten "ftp_core::tftp::server" style targets for readability
      const target = ev.target.replace(/^ftp_core::/, "").replace(/^ftp_toolbox_app.*/, "app");
      // Structured fields used to be dropped by the backend, so e.g. TFTP only
      // ever showed "tftp send complete" here with no byte count.
      const fields = ev.fields?.length
        ? `  ${ev.fields.map(([k, v]) => `${k}=${v}`).join(" ")}`
        : "";
      log(`[${target}] ${ev.message}${fields}`, level);
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, [log]);

  return (
    <div className="layout">
      <Sidebar active={view} onNavigate={setView} />
      <div className="main">
        <header className="header">
          <h1>{TITLES[view]}</h1>
        </header>
        <main className="content">
          {/* 协议页常驻挂载，仅切换可见性：客户端连接标志与表单输入都是组件
              内部状态，卸载即丢（后端会话仍在，但 UI 回到「未连接」、TFTP
              地址还原默认）。日志页数据源在 App，无需常驻。
              defaultBase 解析完成前整块不渲染（见其注释）。 */}
          {defaultBase !== null && (
            <>
              <div hidden={view !== "ftp"}>
                <FtpPage
                  log={log}
                  ftpStatus={ftpStatus}
                  refresh={refreshFtp}
                  onTransferChange={setActiveTransferId}
                  defaultBase={defaultBase}
                />
              </div>
              <div hidden={view !== "tftp"}>
                <TftpPage
                  log={log}
                  tftpStatus={tftpStatus}
                  refresh={refreshTftp}
                  onTransferChange={setActiveTransferId}
                  defaultBase={defaultBase}
                />
              </div>
              <div hidden={view !== "sftp"}>
                <SftpPage
                  log={log}
                  sftpStatus={sftpStatus}
                  refresh={refreshSftp}
                  onTransferChange={setActiveTransferId}
                  defaultBase={defaultBase}
                />
              </div>
              {view === "logs" && <LogView logs={logs} onClear={() => setLogs([])} />}
            </>
          )}
        </main>
        <ProgressBar
          progress={progress}
          cancellable={activeTransferId !== null}
          onCancel={cancelActiveTransfer}
        />
      </div>
    </div>
  );
}
