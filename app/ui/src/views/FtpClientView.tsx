import { useState } from "react";
import { api } from "../api";
import { LogEntry } from "../App";

type Log = (text: string, level?: LogEntry["level"]) => void;

export default function FtpClientView({ log }: { log: Log }) {
  const [addr, setAddr] = useState("127.0.0.1:2121");
  const [user, setUser] = useState("anonymous");
  const [pass, setPass] = useState("");
  const [connected, setConnected] = useState(false);
  const [busy, setBusy] = useState(false);
  const [remotePath, setRemotePath] = useState("");
  const [listing, setListing] = useState<string[] | null>(null);
  const [local, setLocal] = useState("C:\\ftp-root\\hello.txt");
  const [remote, setRemote] = useState("hello.txt");

  const connect = async () => {
    setBusy(true);
    try {
      const msg = await api.ftpConnect(addr, user, pass);
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
    try {
      const msg =
        kind === "upload" ? await api.ftpUpload(local, remote) : await api.ftpDownload(remote, local);
      log(msg, "ok");
    } catch (e) {
      log(`${kind === "upload" ? "上传" : "下载"}失败: ${e}`, "error");
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
          <label className="field" style={{ width: 140 }}>
            <span>用户名</span>
            <input value={user} onChange={(e) => setUser(e.target.value)} disabled={connected} />
          </label>
          <label className="field" style={{ width: 140 }}>
            <span>密码</span>
            <input
              type="password"
              value={pass}
              onChange={(e) => setPass(e.target.value)}
              disabled={connected}
            />
          </label>
        </div>
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
        <label className="field">
          <span>本地文件</span>
          <input value={local} onChange={(e) => setLocal(e.target.value)} disabled={!connected} />
        </label>
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
