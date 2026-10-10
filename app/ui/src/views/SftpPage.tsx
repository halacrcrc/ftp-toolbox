import { useCallback } from "react";
import { api, SftpServerStatus } from "../api";
import ServerCard from "../components/ServerCard";
import useInterfaces from "../hooks/useInterfaces";
import { LogEntry } from "../App";
import SftpClientView from "./SftpClientView";
import { joinDefault } from "./FtpPage";

type Log = (text: string, level?: LogEntry["level"]) => void;

/** 传输开始/结束时上报取消令牌 id（App 据此启用进度条上的「取消」按钮）。 */
type TransferChange = (id: string | null) => void;

interface Props {
  log: Log;
  sftpStatus: SftpServerStatus | null;
  refresh: () => Promise<void>;
  onTransferChange: TransferChange;
  /** 默认目录的基路径（系统文档目录），见 App.tsx。 */
  defaultBase: string;
}

/** SFTP 页 = 左侧服务器卡 + 右侧客户端卡（窄窗口时 auto-fit 折成单列，服务器在上）。 */
export default function SftpPage({ log, sftpStatus, refresh, onTransferChange, defaultBase }: Props) {
  const { interfaces, interfacesLoaded, reload } = useInterfaces(log);
  // 刷新按钮同时更新运行态和网卡列表（网卡 IP 会随网络环境变化）
  const refreshAll = useCallback(async () => {
    await Promise.all([refresh(), reload()]);
  }, [refresh, reload]);

  return (
    <div className="grid-2">
      <ServerCard
        serverKey="sftp"
        title="SFTP 服务器"
        desc="SSH 文件传输；密码或公钥任一通过即可登录"
        defaultRoot={joinDefault(defaultBase, "sftp-root")}
        defaultPort="2222"
        portHint="默认 2222"
        withSftp
        interfaces={interfaces}
        interfacesLoaded={interfacesLoaded}
        status={sftpStatus}
        hostKey={sftpStatus?.hostKey ?? null}
        onStart={(root, addr, user, pass, _passive, _ftps, sftp) => {
          // ServerCard 统一用 "iface:port" 传地址，SFTP 命令要分开的
          // bindAddr/port —— 这里拆开（下拉里只有 IPv4，lastIndexOf 够用）
          const i = addr.lastIndexOf(":");
          const bindAddr = i < 0 ? addr : addr.slice(0, i);
          const port = i < 0 ? 2222 : Number(addr.slice(i + 1)) || 2222;
          return api
            .startSftpServer({
              bindAddr,
              port,
              username: user ?? "",
              password: pass ?? "",
              authorizedKeys: sftp?.authorizedKeys ?? [],
              rootDir: root,
              readOnly: sftp?.readOnly ?? false,
            })
            // 启动成功消息按契约不带指纹（Q12）；addr 若已含端口则不再重复拼
            .then((info) => {
              const target = info.addr.includes(":") ? info.addr : `${info.addr}:${info.port}`;
              return `SFTP 服务器已启动：sftp://${target}`;
            });
        }}
        onStop={api.stopSftpServer}
        onRefresh={refreshAll}
        log={log}
      />
      <SftpClientView
        log={log}
        onTransferChange={onTransferChange}
        serverStatus={sftpStatus}
      />
    </div>
  );
}
