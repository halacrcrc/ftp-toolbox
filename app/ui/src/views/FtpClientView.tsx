import { useEffect, useRef, useState } from "react";
import { api, newTransferId, pathBase, pickSaveFile, FtpEntry, ServerStatus } from "../api";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import RemoteTree, { RemoteEntry } from "../components/RemoteTree";
import { joinRemote, normalize, parentRemote } from "../lib/remotepath";
import LocalFileField from "../components/LocalFileField";
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
  defaultLocal,
  serverStatus,
}: {
  log: Log;
  onTransferChange?: TransferChange;
  /** 本地默认文件路径（文档目录下），由页面传入。 */
  defaultLocal: string;
  /** 本机 FTP 服务器运行态（App 下推）；用于停服时回落客户端状态。 */
  serverStatus: ServerStatus | null;
}) {
  const [addr, setAddr] = useState("127.0.0.1:2121");
  const [user, setUser] = useState("anonymous");
  const [pass, setPass] = useState("");
  const [connected, setConnected] = useState(false);
  const [busy, setBusy] = useState(false);
  // 远端文件树状态：当前目录 + 该目录条目 + 按路径缓存（返回上一级免重拉）
  const [currentPath, setCurrentPath] = useState("");
  const [entries, setEntries] = useState<RemoteEntry[] | null>(null);
  const [treeLoading, setTreeLoading] = useState(false);
  const treeCache = useRef(new Map<string, RemoteEntry[]>());
  // 手输路径草稿：树导航时同步显示当前位置；回车/「跳转」按草稿打开目录
  const [pathDraft, setPathDraft] = useState("");
  const [local, setLocal] = useState(defaultLocal);
  const [remote, setRemote] = useState("hello.txt");
  // FTPS（显式 TLS）：连上后先 AUTH TLS 再发账号密码，凭据不走明文。
  // 自签服务器（包括本应用自己）需要勾「接受无效证书」。
  const [ftps, setFtps] = useState(false);
  const [acceptInvalidCerts, setAcceptInvalidCerts] = useState(false);

  /** 断开/停服后清空树：缓存一并丢弃，重连后从根重新列出。 */
  const resetTree = () => {
    treeCache.current = new Map();
    setEntries(null);
    setCurrentPath("");
    setPathDraft("");
  };

  /**
   * 打开远端目录并切换树视图。非 force 时命中缓存直接切换（面包屑/返回上一级
   * 免重拉）；force 用于刷新与连上后的首次加载。空路径 = 服务器默认目录
   * （后端 list(None) 落到 "/"）。成功后同步手输框为实际位置。
   */
  const openDir = async (path: string, force: boolean) => {
    if (!force) {
      const hit = treeCache.current.get(path);
      if (hit) {
        setCurrentPath(path);
        setEntries(hit);
        setPathDraft(path);
        return;
      }
    }
    setTreeLoading(true);
    try {
      const items = await api.ftpListDetailed(path || undefined);
      const mapped = items.map(toRemoteEntry);
      treeCache.current.set(path, mapped);
      setEntries(mapped);
      setCurrentPath(path);
      setPathDraft(path);
      log(`列出 ${items.length} 个条目`);
    } catch (e) {
      log(`列目录失败: ${e}`, "error");
    } finally {
      setTreeLoading(false);
    }
  };

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

  /** 点树里的文件节点：弹「保存到…」对话框（预填文件名）后下载。 */
  const downloadEntry = async (entry: RemoteEntry) => {
    if (entry.kind === "dir") return;
    const remotePath = joinRemote(currentPath, entry.name);
    let localPath: string | null;
    try {
      localPath = await pickSaveFile(entry.name);
    } catch (e) {
      log(`打开保存对话框失败: ${e}`, "error");
      return;
    }
    if (!localPath) return;
    const transferId = newTransferId();
    onTransferChange?.(transferId);
    try {
      log(await api.ftpDownload(remotePath, localPath, transferId), "ok");
    } catch (e) {
      log(`下载失败: ${e}`, "error");
    } finally {
      onTransferChange?.(null);
    }
  };

  /** 拖入文件的落点统一为当前目录；多文件串行上传（单连接模型，勿并发）。 */
  const uploadPaths = async (localPaths: string[]) => {
    if (!connected || localPaths.length === 0) return;
    for (const lp of localPaths) {
      const remotePath = joinRemote(currentPath, pathBase(lp));
      const transferId = newTransferId();
      onTransferChange?.(transferId);
      try {
        log(await api.ftpUpload(lp, remotePath, transferId), "ok");
      } catch (e) {
        log(`上传失败: ${e}`, "error");
      } finally {
        onTransferChange?.(null);
      }
    }
    // 全部结束后重拉当前目录，让新文件出现在树里
    await openDir(currentPath, true);
  };

  // Tauri v2 拦截了 HTML5 drop 事件（dragDropEnabled 默认开），拖拽上传只能走
  // webview 原生 onDragDropEvent：drop 事件直接给字符串绝对路径。用落点坐标做
  // 一次「是否落在树区域」的命中检测（elementFromPoint + closest），避免在日志
  // 页/传输卡上误触发。position 是物理像素，除以 devicePixelRatio 换算 CSS 坐标。
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    void getCurrentWebview()
      .onDragDropEvent((event) => {
        if (event.payload.type !== "drop") return;
        const { paths, position } = event.payload;
        const scale = window.devicePixelRatio || 1;
        const el = document.elementFromPoint(position.x / scale, position.y / scale);
        if (!el?.closest(".remote-tree")) return;
        void uploadPaths(paths);
      })
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  });

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
          onDownload={(e) => void downloadEntry(e)}
        />
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
