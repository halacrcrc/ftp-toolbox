import { useCallback, useEffect, useState } from "react";
import { fmtBytes, onBackendLog, onTransferProgress } from "./api";
import Sidebar, { ViewKey } from "./components/Sidebar";
import ProgressBar from "./components/ProgressBar";
import ServersView from "./views/ServersView";
import FtpClientView from "./views/FtpClientView";
import TftpClientView from "./views/TftpClientView";
import LogView from "./views/LogView";

export interface LogEntry {
  time: string;
  level: "info" | "ok" | "error";
  text: string;
}

export interface Progress {
  file: string;
  kind: "upload" | "download";
  bytes: number;
  total: number | null;
}

const TITLES: Record<ViewKey, string> = {
  servers: "服务器",
  ftp: "FTP 客户端",
  tftp: "TFTP 客户端",
  logs: "运行日志",
};

export default function App() {
  const [view, setView] = useState<ViewKey>("servers");
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [progress, setProgress] = useState<Progress | null>(null);

  const log = useCallback((text: string, level: LogEntry["level"] = "info") => {
    // keep at most 500 entries so the log view never grows unbounded
    setLogs((ls) => [
      ...ls.slice(-499),
      { time: new Date().toLocaleTimeString(), level, text },
    ]);
  }, []);

  // Single global subscription: the backend pushes TransferEvents for all
  // transfers; we mirror them into the footer progress bar and the log.
  useEffect(() => {
    const unlisten = onTransferProgress((ev) => {
      switch (ev.phase) {
        case "started":
          setProgress({ file: ev.file, kind: ev.kind, bytes: 0, total: ev.total ?? null });
          log(`${ev.kind === "upload" ? "上传" : "下载"}开始: ${ev.file}`);
          break;
        case "progress":
          setProgress({
            file: ev.file,
            kind: ev.kind,
            bytes: ev.bytes ?? 0,
            total: ev.total ?? null,
          });
          break;
        case "done":
          log(`完成: ${ev.file}（${fmtBytes(ev.bytes ?? 0)}）`, "ok");
          setProgress(null);
          break;
        case "error":
          log(`传输错误: ${ev.file}: ${ev.message ?? "未知错误"}`, "error");
          setProgress(null);
          break;
      }
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, [log]);

  // Backend tracing events (engine + libunftp): the detailed half of the
  // log view — server sessions, per-transfer byte/block/elapsed stats, etc.
  useEffect(() => {
    const unlisten = onBackendLog((ev) => {
      if (!ev.message) return;
      const level = ev.level === "ERROR" || ev.level === "WARN" ? "error" : "info";
      // shorten "ftp_core::tftp::server" style targets for readability
      const target = ev.target.replace(/^ftp_core::/, "").replace(/^ftp_toolbox_app.*/, "app");
      log(`[${target}] ${ev.message}`, level);
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
          {view === "servers" && <ServersView log={log} />}
          {view === "ftp" && <FtpClientView log={log} />}
          {view === "tftp" && <TftpClientView log={log} />}
          {view === "logs" && <LogView logs={logs} onClear={() => setLogs([])} />}
        </main>
        <ProgressBar progress={progress} />
      </div>
    </div>
  );
}
