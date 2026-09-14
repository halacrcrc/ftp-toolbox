import { useEffect, useState } from "react";
import { api, pickFolder, NetInterface } from "../api";
import { LogEntry } from "../App";

type Log = (text: string, level?: LogEntry["level"]) => void;

// ---------- persisted per-server preferences ----------

interface ServerPrefs {
  root: string;
  iface: string;
  port: string;
  authMode: "anonymous" | "account";
  user: string;
}

function loadPrefs(key: string, fallback: ServerPrefs): ServerPrefs {
  try {
    const raw = localStorage.getItem(key);
    if (raw) return { ...fallback, ...JSON.parse(raw) };
  } catch {
    // corrupted storage -> fall back to defaults
  }
  return fallback;
}

// ---------- server card ----------

interface ServerCardProps {
  /** storage namespace, e.g. "ftp" -> localStorage key "ftp-toolbox:server:ftp" */
  serverKey: string;
  title: string;
  desc: string;
  defaultRoot: string;
  defaultPort: string;
  portHint: string;
  withAuth?: boolean;
  interfaces: NetInterface[];
  onStart: (root: string, addr: string, user?: string, pass?: string) => Promise<string>;
  onStop: () => Promise<string>;
  log: Log;
}

function ServerCard({
  serverKey, title, desc, defaultRoot, defaultPort, portHint,
  withAuth, interfaces, onStart, onStop, log,
}: ServerCardProps) {
  const storageKey = `ftp-toolbox:server:${serverKey}`;
  const [prefs, setPrefs] = useState<ServerPrefs>(() =>
    loadPrefs(storageKey, {
      root: defaultRoot,
      iface: "0.0.0.0",
      port: defaultPort,
      authMode: "anonymous",
      user: "admin",
    })
  );
  const [pass, setPass] = useState(""); // password intentionally NOT persisted
  const [running, setRunning] = useState(false);
  const [busy, setBusy] = useState(false);

  // persist every change (except password and runtime state)
  useEffect(() => {
    localStorage.setItem(storageKey, JSON.stringify(prefs));
  }, [storageKey, prefs]);

  const set = <K extends keyof ServerPrefs>(k: K, v: ServerPrefs[K]) =>
    setPrefs((p) => ({ ...p, [k]: v }));

  const browse = async () => {
    try {
      const dir = await pickFolder(prefs.root);
      if (dir) set("root", dir);
    } catch (e) {
      log(`打开文件夹选择框失败: ${e}`, "error");
    }
  };

  const start = async () => {
    setBusy(true);
    try {
      const useAccount = withAuth && prefs.authMode === "account";
      const addr = `${prefs.iface}:${prefs.port}`;
      const msg = await onStart(
        prefs.root,
        addr,
        useAccount ? prefs.user : undefined,
        useAccount ? pass : undefined
      );
      log(msg, "ok");
      setRunning(true);
    } catch (e) {
      log(`${title}启动失败: ${e}`, "error");
    } finally {
      setBusy(false);
    }
  };

  const stop = async () => {
    setBusy(true);
    try {
      log(await onStop());
      setRunning(false);
    } catch (e) {
      log(`${title}停止失败: ${e}`, "error");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="card">
      <div className="card-head">
        <div>
          <div className="card-title">{title}</div>
          <div className="card-desc">{desc}</div>
        </div>
        <span className={`status-chip${running ? " running" : ""}`}>
          <span className="dot" />
          {running ? "运行中" : "已停止"}
        </span>
      </div>

      <label className="field">
        <span>共享目录</span>
        <div className="input-row">
          <input
            className="grow"
            value={prefs.root}
            onChange={(e) => set("root", e.target.value)}
            disabled={running}
          />
          <button className="btn small" onClick={browse} disabled={running}>
            浏览…
          </button>
        </div>
      </label>

      <div className="row">
        <label className="field grow">
          <span>监听接口</span>
          <select value={prefs.iface} onChange={(e) => set("iface", e.target.value)} disabled={running}>
            <option value="0.0.0.0">所有接口 (0.0.0.0)</option>
            {interfaces.map((it) => (
              <option key={`${it.name}-${it.ip}`} value={it.ip}>
                {it.name} ({it.ip})
              </option>
            ))}
          </select>
        </label>
        <label className="field" style={{ width: 110 }}>
          <span>端口（{portHint}）</span>
          <input
            type="number"
            min={1}
            max={65535}
            value={prefs.port}
            onChange={(e) => set("port", e.target.value)}
            disabled={running}
          />
        </label>
      </div>

      {withAuth && (
        <>
          <label className="field">
            <span>认证方式</span>
            <div className="radio-row">
              <label className="radio">
                <input
                  type="radio"
                  checked={prefs.authMode === "anonymous"}
                  onChange={() => set("authMode", "anonymous")}
                  disabled={running}
                />
                匿名访问
              </label>
              <label className="radio">
                <input
                  type="radio"
                  checked={prefs.authMode === "account"}
                  onChange={() => set("authMode", "account")}
                  disabled={running}
                />
                账号密码
              </label>
            </div>
          </label>
          {prefs.authMode === "account" && (
            <div className="row">
              <label className="field grow">
                <span>用户名</span>
                <input value={prefs.user} onChange={(e) => set("user", e.target.value)} disabled={running} />
              </label>
              <label className="field grow">
                <span>密码（不保存）</span>
                <input
                  type="password"
                  value={pass}
                  onChange={(e) => setPass(e.target.value)}
                  disabled={running}
                />
              </label>
            </div>
          )}
        </>
      )}

      <div className="actions">
        {running ? (
          <button className="btn danger" onClick={stop} disabled={busy}>
            停止服务
          </button>
        ) : (
          <button className="btn primary" onClick={start} disabled={busy}>
            启动服务
          </button>
        )}
      </div>
    </div>
  );
}

// ---------- view ----------

export default function ServersView({ log }: { log: Log }) {
  const [interfaces, setInterfaces] = useState<NetInterface[]>([]);

  useEffect(() => {
    api
      .listInterfaces()
      .then(setInterfaces)
      .catch((e) => log(`读取网卡列表失败: ${e}`, "error"));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="grid-2">
      <ServerCard
        serverKey="ftp"
        title="FTP 服务器"
        desc="支持匿名或账号密码认证，被动端口 50000-50099"
        defaultRoot="C:\\ftp-root"
        defaultPort="21"
        portHint="默认 21"
        withAuth
        interfaces={interfaces}
        onStart={api.startFtpServer}
        onStop={api.stopFtpServer}
        log={log}
      />
      <ServerCard
        serverKey="tftp"
        title="TFTP 服务器"
        desc="支持 blksize 协商（单传可达 500+ MB）"
        defaultRoot="C:\\tftp-root"
        defaultPort="69"
        portHint="默认 69"
        interfaces={interfaces}
        onStart={(root, addr) => api.startTftpServer(root, addr)}
        onStop={api.stopTftpServer}
        log={log}
      />
    </div>
  );
}
