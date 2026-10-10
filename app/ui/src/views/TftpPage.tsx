import { useCallback } from "react";
import { api, ServerStatus } from "../api";
import ServerCard from "../components/ServerCard";
import useInterfaces from "../hooks/useInterfaces";
import { LogEntry } from "../App";
import TftpClientView from "./TftpClientView";
import { joinDefault } from "./FtpPage";

type Log = (text: string, level?: LogEntry["level"]) => void;

/** 传输开始/结束时上报取消令牌 id（App 据此启用进度条上的「取消」按钮）。 */
type TransferChange = (id: string | null) => void;

interface Props {
  log: Log;
  tftpStatus: ServerStatus | null;
  refresh: () => Promise<void>;
  onTransferChange: TransferChange;
  /** 默认目录的基路径（系统文档目录），见 App.tsx。 */
  defaultBase: string;
}

/** TFTP 页 = 左侧服务器卡 + 右侧客户端卡（窄窗口时 auto-fit 折成单列，服务器在上）。 */
export default function TftpPage({ log, tftpStatus, refresh, onTransferChange, defaultBase }: Props) {
  const { interfaces, interfacesLoaded, reload } = useInterfaces(log);
  // 刷新按钮同时更新运行态和网卡列表（网卡 IP 会随网络环境变化）
  const refreshAll = useCallback(async () => {
    await Promise.all([refresh(), reload()]);
  }, [refresh, reload]);

  return (
    <div className="grid-2">
      <ServerCard
        serverKey="tftp"
        title="TFTP 服务器"
        desc="支持 blksize 协商（单传可达 500+ MB）"
        defaultRoot={joinDefault(defaultBase, "tftp-root")}
        defaultPort="69"
        portHint="默认 69"
        interfaces={interfaces}
        interfacesLoaded={interfacesLoaded}
        status={tftpStatus}
        onStart={(root, addr) => api.startTftpServer(root, addr)}
        onStop={api.stopTftpServer}
        onRefresh={refreshAll}
        log={log}
      />
      <TftpClientView
        log={log}
        onTransferChange={onTransferChange}
        defaultLocal={joinDefault(defaultBase, "tftp-root", "hello.txt")}
      />
    </div>
  );
}
