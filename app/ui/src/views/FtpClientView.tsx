import { useEffect, useState } from "react";
import { api, FtpEntry, ServerStatus } from "../api";
import RemoteTree, { RemoteEntry } from "../components/RemoteTree";
import useRemoteTree from "../hooks/useRemoteTree";
import { joinRemote, normalize, parentRemote } from "../lib/remotepath";
import { LogEntry } from "../App";

type Log = (text: string, level?: LogEntry["level"]) => void;

/** 传输开始/结束时上报取消令牌 id（App 据此启用进度条上的「取消」按钮）。 */
type TransferChange = (id: string | null) => void;

/**
 * FtpEntry → RemoteEntry（协议无关树条目）。kind 在后端 MLSD/LIST 解析时已
 * 归一为 "file"|"dir"|"symlink"|"other"，这里只做窄化；未识别值按 "other" 显示
 * （LIST 兜底切不出类型的行正是这一类，大小/时间渲染 "—"）。
 */
function toRemoteEntry(e: FtpEntry): RemoteEntry {
  const kind: RemoteEntry["kind"] =
    e.kind === "file" || e.kind === "dir" || e.kind === "symlink" ? e.kind : "other";
  return { name: e.name, kind, size: e.size ?? undefined, mtime: e.mtime ?? undefined };
}

export default function FtpClientView({
  log,
  onTransferChange,
  serverStatus,
}: {
  log: Log;
  onTransferChange?: TransferChange;
  /** 本机 FTP 服务器运行态（App 下推）；用于停服时回落客户端状态。 */
  serverStatus: ServerStatus | null;
}) {
  const [addr, setAddr] = useState("127.0.0.1:2121");
  const [user, setUser] = useState("anonymous");
  const [pass, setPass] = useState("");
  const [connected, setConnected] = useState(false);
  const [busy, setBusy] = useState(false);
  // FTPS（显式 TLS）：连上后先 AUTH TLS 再发账号密码，凭据不走明文。
  // 自签服务器（包括本应用自己）需要勾「接受无效证书」。
  const [ftps, setFtps] = useState(false);
  const [acceptInvalidCerts, setAcceptInvalidCerts] = useState(false);
  // 远端文件树状态与批量传输动作统一由 hook 持有（与 SFTP 页同构，评审 #31）；
  // 这里只把 FTP 的 IPC 命令接进去，条目映射在本文件 toRemoteEntry。
  const {
    entries,
    currentPath,
    treeLoading,
    batchBusy,
    pathDraft,
    setPathDraft,
    openDir,
    resetTree,
    pickAndUpload,
    pickAndUploadFolder,
    downloadMany,
  } = useRemoteTree(
    {
      list: (p) => api.ftpListDetailed(p).then((es) => es.map(toRemoteEntry)),
      upload: api.ftpUpload,
      download: api.ftpDownload,
      mkdir: api.ftpMkdir,
    },
    log,
    connected,
    onTransferChange
  );

  /** 手输路径跳转：normalize 后按该路径打开；失败时草稿留在框里可改。 */
  const gotoDraft = () => {
    void openDir(normalize(pathDraft.trim()), true);
  };

  const connect = async () => {
    setBusy(true);
    try {
      const msg = await api.ftpConnect(addr, user, pass, ftps, acceptInvalidCerts);
      log(msg, "ok");
      setConnected(true);
      // 连上即列根目录，树可直接下钻（失败时 openDir 内部已记日志）
      await openDir("", true);
    } catch (e) {
      log(`连接失败: ${e}`, "error");
    } finally {
      setBusy(false);
    }
  };

  const disconnect = async () => {
    setBusy(true);
    try {
      log(await api.ftpDisconnect());
    } catch (e) {
      log(`断开失败: ${e}`, "error");
    } finally {
      setConnected(false);
      resetTree();
      setBusy(false);
    }
  };

  // 本机服务器停止时，客户端若正连着它，立即回落「未连接」。停服时服务器
  // 会关闭已建立的会话（spawn_session 订阅 stop 信号），控制连接真实断开，
  // 但 FTP 协议没有服务端推送、客户端也没有控制通道后台读取器——不主动
  // 比对就一直显示「已连接」（2026-10-10 用户实测）。
  // 只按端口匹配：localAddr 可能是 0.0.0.0:x 而客户端填 127.0.0.1:x；
  // 恰好连着同端口远程服务器且本机同端口服务器恰好停止才会误重置，可接受。
  // 远程服务器自身崩溃仍要等下次操作报错——协议层没有通知，可后续加 NOOP 保活。
  // 后端死会话不在此清理：下一次 ftpConnect 会整体替换它。
  useEffect(() => {
    if (!connected) return;
    if (!serverStatus || serverStatus.running || !serverStatus.localAddr) return;
    const ourPort = serverStatus.localAddr.split(":").pop();
    const myPort = addr.trim().split(":").pop();
    if (ourPort && ourPort === myPort) {
      setConnected(false);
      resetTree();
      log(`本机服务器已停止（${serverStatus.localAddr}），连接已断开`, "error");
    }
  }, [serverStatus, connected, addr, log]);

  return (
    <div className="stack">
      <div className="card">
        <div className="card-head">
          <div className="card-title">连接</div>
          <span className={`status-chip${connected ? " running" : ""}`}>
            <span className="dot" />
            {connected ? "已连接" : "未连接"}
          </span>
        </div>
        <div className="row">
          <label className="field grow">
            <span>服务器地址</span>
            <input value={addr} onChange={(e) => setAddr(e.target.value)} disabled={connected} />
          </label>
          <label className="field medium">
            <span>用户名</span>
            <input value={user} onChange={(e) => setUser(e.target.value)} disabled={connected} />
          </label>
          <label className="field medium">
            <span>密码</span>
            <input
              type="password"
              value={pass}
              onChange={(e) => setPass(e.target.value)}
              disabled={connected}
            />
          </label>
        </div>
        <div className="radio-row">
          <label className="radio">
            <input
              type="checkbox"
              checked={ftps}
              onChange={(e) => setFtps(e.target.checked)}
              disabled={connected}
            />
            FTPS（显式 TLS）
          </label>
          {ftps && (
            <label className="radio">
              <input
                type="checkbox"
                checked={acceptInvalidCerts}
                onChange={(e) => setAcceptInvalidCerts(e.target.checked)}
                disabled={connected}
              />
              接受自签/无效证书
            </label>
          )}
        </div>
        {ftps && acceptInvalidCerts && (
          <div className="hint-line warn">
            ⚠ 勾选「接受自签/无效证书」后将不校验服务器身份，连接可能被中间人冒充；
            只建议在自测或信任的局域网内使用。对端证书指纹可在左侧服务器卡的「证书详情」中查看。
          </div>
        )}
        <div className="actions">
          {connected ? (
            <button className="btn" onClick={disconnect} disabled={busy}>
              断开连接
            </button>
          ) : (
            <button className="btn primary" onClick={connect} disabled={busy}>
              连接
            </button>
          )}
        </div>
      </div>

      <div className="card">
        <div className="card-head">
          <div className="card-title">远程目录</div>
          <button className="btn small" onClick={gotoDraft} disabled={!connected || treeLoading}>
            跳转到路径
          </button>
        </div>
        {/* 手输路径是树的补充（老用法保留）：回车或「跳转到路径」直达；树内
            点按/面包屑导航后此框同步为当前位置。 */}
        <label className="field">
          <span>路径（回车跳转，留空为根）</span>
          <input
            value={pathDraft}
            onChange={(e) => setPathDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") gotoDraft();
            }}
            disabled={!connected}
            placeholder="/"
          />
        </label>
        <RemoteTree
          entries={entries}
          currentPath={currentPath}
          loading={treeLoading}
          disabled={!connected}
          onNavigate={(d) => void openDir(joinRemote(currentPath, d), false)}
          onCrumb={(p) => void openDir(p, false)}
          onUp={() => void openDir(parentRemote(currentPath), true)}
          onRefresh={() => void openDir(currentPath, true)}
          onUpload={() => void pickAndUpload()}
          onUploadFolder={() => void pickAndUploadFolder()}
          onDownloadMany={(picked) => void downloadMany(picked)}
          actionsDisabled={batchBusy}
        />
      </div>
    </div>
  );
}
