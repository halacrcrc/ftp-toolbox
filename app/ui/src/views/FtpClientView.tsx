import { useState } from "react";
import { api, newTransferId, pathBase } from "../api";
import LocalFileField from "../components/LocalFileField";
import { LogEntry } from "../App";

type Log = (text: string, level?: LogEntry["level"]) => void;

/** 传输开始/结束时上报取消令牌 id（App 据此启用进度条上的「取消」按钮）。 */
type TransferChange = (id: string | null) => void;

export default function FtpClientView({
  log,
  onTransferChange,
  defaultLocal,
}: {
  log: Log;
  onTransferChange?: TransferChange;
  /** 本地默认文件路径（文档目录下），由页面传入。 */
  defaultLocal: string;
}) {
  const [addr, setAddr] = useState("127.0.0.1:2121");
  const [user, setUser] = useState("anonymous");
  const [pass, setPass] = useState("");
  const [connected, setConnected] = useState(false);
  const [busy, setBusy] = useState(false);
  const [remotePath, setRemotePath] = useState("");
  const [listing, setListing] = useState<string[] | null>(null);
  const [local, setLocal] = useState(defaultLocal);
  const [remote, setRemote] = useState("hello.txt");
  // FTPS（显式 TLS）：连上后先 AUTH TLS 再发账号密码，凭据不走明文。
  // 自签服务器（包括本应用自己）需要勾「接受无效证书」。
  const [ftps, setFtps] = useState(false);
  const [acceptInvalidCerts, setAcceptInvalidCerts] = useState(false);

  const connect = async () => {
    setBusy(true);
    try {
      const msg = await api.ftpConnect(addr, user, pass, ftps, acceptInvalidCerts);
      log(msg, "ok");
      setConnected(true);
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
      setListing(null);
      setBusy(false);
    }
  };

  const refresh = async () => {
    try {
      const items = await api.ftpList(remotePath || undefined);
      setListing(items);
      log(`列出 ${items.length} 个条目`);
    } catch (e) {
      log(`列目录失败: ${e}`, "error");
    }
  };

  const transfer = async (kind: "upload" | "download") => {
    // 前端生成取消令牌 id：后端引擎在分块边界检查，落地即中止。
    const transferId = newTransferId();
    onTransferChange?.(transferId);
    try {
      const msg =
        kind === "upload"
          ? await api.ftpUpload(local, remote, transferId)
          : await api.ftpDownload(remote, local, transferId);
      log(msg, "ok");
    } catch (e) {
      log(`${kind === "upload" ? "上传" : "下载"}失败: ${e}`, "error");
    } finally {
      onTransferChange?.(null);
    }
  };

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
          <button className="btn small" onClick={refresh} disabled={!connected}>
            刷新列表
          </button>
        </div>
        <label className="field">
          <span>路径（留空为当前目录）</span>
          <input
            value={remotePath}
            onChange={(e) => setRemotePath(e.target.value)}
            disabled={!connected}
            placeholder="/"
          />
        </label>
        {listing !== null && (
          <pre className="listing">{listing.length ? listing.join("\n") : "（空目录）"}</pre>
        )}
      </div>

      <div className="card">
        <div className="card-title">文件传输</div>
        <LocalFileField
          value={local}
          onChange={setLocal}
          onPicked={(p) => setRemote(pathBase(p))}
          saveName={remote}
          disabled={!connected}
          log={log}
        />
        <label className="field">
          <span>远程文件名</span>
          <input value={remote} onChange={(e) => setRemote(e.target.value)} disabled={!connected} />
        </label>
        <div className="actions">
          <button className="btn primary" onClick={() => transfer("upload")} disabled={!connected}>
            上传
          </button>
          <button className="btn" onClick={() => transfer("download")} disabled={!connected}>
            下载
          </button>
        </div>
      </div>
    </div>
  );
}
