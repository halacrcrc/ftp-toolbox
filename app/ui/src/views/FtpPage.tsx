import { useCallback } from "react";
import { api, ServerStatus } from "../api";
import ServerCard from "../components/ServerCard";
import useInterfaces from "../hooks/useInterfaces";
import { LogEntry } from "../App";
import FtpClientView from "./FtpClientView";

type Log = (text: string, level?: LogEntry["level"]) => void;

/** 传输开始/结束时上报取消令牌 id（App 据此启用进度条上的「取消」按钮）。 */
type TransferChange = (id: string | null) => void;

interface Props {
  log: Log;
  ftpStatus: ServerStatus | null;
  refresh: () => Promise<void>;
  onTransferChange: TransferChange;
  /** 默认目录的基路径（系统文档目录），见 App.tsx。 */
  defaultBase: string;
}

/**
 * 默认路径拼接：Windows 基路径用 `\`，其余用 `/`；基路径为空（解析失败的
 * 非 Windows 兜底）时返回空串，由 ServerCard 的占位提示与客户端空输入兜底。
 */
export function joinDefault(base: string, ...parts: string[]): string {
  if (!base) return "";
  const sep = base.includes("\\") ? "\\" : "/";
  return [base, ...parts].join(sep);
}

/** FTP 页 = 左侧服务器卡 + 右侧客户端卡（窄窗口时 auto-fit 折成单列，服务器在上）。 */
export default function FtpPage({ log, ftpStatus, refresh, onTransferChange, defaultBase }: Props) {
  const { interfaces, interfacesLoaded, reload } = useInterfaces(log);
  // 刷新按钮同时更新运行态和网卡列表（网卡 IP 会随网络环境变化）
  const refreshAll = useCallback(async () => {
    await Promise.all([refresh(), reload()]);
  }, [refresh, reload]);

  return (
    <div className="grid-2">
      <ServerCard
        serverKey="ftp"
        title="FTP 服务器"
        desc="支持匿名或账号密码认证"
        defaultRoot={joinDefault(defaultBase, "ftp-root")}
        defaultPort="21"
        portHint="默认 21"
        withAuth
        withPassive
        withFtps
        withActiveMode
        // 与 ftp-core 的 DEFAULT_PASSIVE_PORTS (50000..50100) 保持一致
        defaultPassive="50000-50099"
        interfaces={interfaces}
        interfacesLoaded={interfacesLoaded}
        status={ftpStatus}
        onStart={(root, addr, user, pass, passive, ftps, _sftp, allowActive) =>
          api.startFtpServer(root, addr, user, pass, passive, ftps, allowActive)
        }
        onStop={api.stopFtpServer}
        onRefresh={refreshAll}
        log={log}
      />
      <FtpClientView
        log={log}
        onTransferChange={onTransferChange}
        serverStatus={ftpStatus}
      />
    </div>
  );
}
