import { useEffect, useRef, useState } from "react";
import { api, newTransferId, pathBase, pickOpenDirectory, pickOpenFiles, HostKeyStatus, KnownHostRecord, SftpEntry, SftpServerStatus } from "../api";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import RemoteTree, { RemoteEntry } from "../components/RemoteTree";
import { joinDefault } from "./FtpPage";
import { joinRemote, parentRemote } from "../lib/remotepath";
import { LogEntry } from "../App";

type Log = (text: string, level?: LogEntry["level"]) => void;

/** 传输开始/结束时上报取消令牌 id（App 据此启用进度条上的「取消」按钮）。 */
type TransferChange = (id: string | null) => void;

/** 等待用户确认的主机密钥（TOFU 首连 / 密钥变化，设计文档 §2.4）。 */
interface HostKeyPrompt {
  kind: "unknown" | "changed";
  fingerprint: string;
}

/**
 * SftpEntry → RemoteEntry（协议无关树条目）。`fileType` 沿用列表渲染时代的
 * 宽容匹配（后端契约已定为 "file"|"dir"|"symlink"|"other"，但保留前缀/子串
 * 判断可兜住未来取值扩展），未识别的一律按 "other" 显示。
 */
function toRemoteEntry(e: SftpEntry): RemoteEntry {
  const t = e.fileType.toLowerCase();
  const kind: RemoteEntry["kind"] = t.startsWith("d")
    ? "dir"
    : t.startsWith("l") || t.includes("link")
      ? "symlink"
      : t.startsWith("f") || t.startsWith("-")
        ? "file"
        : "other";
  return { name: e.name, kind, size: e.size, mtime: e.mtime ?? undefined };
}

export default function SftpClientView({
  log,
  onTransferChange,
  serverStatus,
}: {
  log: Log;
  onTransferChange?: TransferChange;
  /** 本机 SFTP 服务器运行态（App 下推）；用于停服时回落客户端状态。 */
  serverStatus: SftpServerStatus | null;
}) {
  // 连接远端用标准 SSH 端口 22（本应用自己的服务器默认 2222，输入框可改）
  const [host, setHost] = useState("127.0.0.1");
  const [port, setPort] = useState("22");
  const [user, setUser] = useState("");
  const [pass, setPass] = useState("");
  const [connected, setConnected] = useState(false);
  const [busy, setBusy] = useState(false);
  // 远端文件树状态：当前目录 + 该目录条目 + 按路径缓存（返回上一级免重拉）
  const [currentPath, setCurrentPath] = useState("");
  const [entries, setEntries] = useState<RemoteEntry[] | null>(null);
  const [treeLoading, setTreeLoading] = useState(false);
  const treeCache = useRef(new Map<string, RemoteEntry[]>());
  const [prompt, setPrompt] = useState<HostKeyPrompt | null>(null);
  // 「管理已信任主机」面板：null = 收起；展开时展示记录列表供勾选移除
  const [knownHosts, setKnownHosts] = useState<KnownHostRecord[] | null>(null);
  const [selectedHosts, setSelectedHosts] = useState<Set<string>>(new Set());
  const [manageBusy, setManageBusy] = useState(false);

  const portNum = () => Number(port) || 22;

  /** 断开/停服后清空树：缓存一并丢弃，重连后从根重新列出。 */
  const resetTree = () => {
    treeCache.current = new Map();
    setEntries(null);
    setCurrentPath("");
  };

  /**
   * 打开远端目录并切换树视图。非 force 时命中缓存直接切换（面包屑/返回上一级
   * 免重拉）；force 用于刷新与连上后的首次加载。空路径 = 服务器默认目录
   * （后端 list(None) 落到 "/"）。
   */
  const openDir = async (path: string, force: boolean) => {
    if (!force) {
      const hit = treeCache.current.get(path);
      if (hit) {
        setCurrentPath(path);
        setEntries(hit);
        return;
      }
    }
    setTreeLoading(true);
    try {
      const items = await api.sftpClientList(path || undefined);
      const mapped = items.map(toRemoteEntry);
      treeCache.current.set(path, mapped);
      setEntries(mapped);
      setCurrentPath(path);
      log(`列出 ${items.length} 个条目`);
    } catch (e) {
      log(`列目录失败: ${e}`, "error");
    } finally {
      setTreeLoading(false);
    }
  };

  /** 真正发起连接；trustNewHost 仅在用户确认过 unknown 指纹后为 true。 */
  const doConnect = async (trustNewHost: boolean) => {
    setBusy(true);
    try {
      const msg = await api.sftpClientConnect(host, portNum(), user, pass, trustNewHost);
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

  /**
   * TOFU 连接流程（§2.4）：先 check_host_key，再决定怎么连。
   * known → 直接连（连接时自动比对，一致即静默通过）；
   * unknown → 弹指纹确认框，确认后带 trust_new_host=true 重连；
   * changed → 弹警告框，确认后先 update_known_host 覆盖记录再连。
   */
  const connect = async () => {
    setBusy(true);
    let check: HostKeyStatus;
    try {
      check = await api.sftpClientCheckHostKey(host, portNum());
    } catch (e) {
      log(`检查主机密钥失败: ${e}`, "error");
      setBusy(false);
      return;
    }
    setBusy(false);
    if (check.status === "known") {
      await doConnect(false);
      return;
    }
    setPrompt({ kind: check.status, fingerprint: check.fingerprint ?? "（未获取到指纹）" });
  };

  const confirmPrompt = async () => {
    const p = prompt;
    setPrompt(null);
    if (!p) return;
    try {
      if (p.kind === "changed") {
        // 显式覆盖 known_hosts 记录（connect 对 changed 一律拒绝，只能走这里）。
        // 之后带 trust 重连：无论后端「更新即记录新指纹」还是「更新只清空记录」，
        // 两种实现都能顺利通过。
        await api.sftpClientUpdateKnownHost(host, portNum());
        log("已更新主机密钥记录");
      }
      await doConnect(true);
    } catch (e) {
      log(`更新已信任主机失败: ${e}`, "error");
    }
  };

  const disconnect = async () => {
    setBusy(true);
    try {
      log(await api.sftpClientDisconnect());
    } catch (e) {
      log(`断开失败: ${e}`, "error");
    } finally {
      setConnected(false);
      resetTree();
      setBusy(false);
    }
  };

  // 本机服务器停止时，客户端若正连着它，立即回落「未连接」。停服会话由服务
  // 器关闭（spawn_ssh_session 收到 stop 后走 handle.disconnect 断开 SSH 连
  // 接），但 SSH 协议没有服务端推送、
  // 客户端也没有后台读取器，不主动比对就一直显示「已连接」。只按端口匹配
  // （host 可能写 127.0.0.1 也可能写本机其它地址）；远程服务器自身崩溃仍要
  // 等下次操作报错，可后续加 SSH keepalive。后端死会话由下一次连接整体替换。
  useEffect(() => {
    if (!connected) return;
    if (!serverStatus || serverStatus.running || !serverStatus.localAddr) return;
    const ourPort = serverStatus.localAddr.split(":").pop();
    if (ourPort && ourPort === port.trim()) {
      setConnected(false);
      resetTree();
      log(`本机服务器已停止（${serverStatus.localAddr}），连接已断开`, "error");
    }
  }, [serverStatus, connected, port, log]);

  /** 拖入文件的落点统一为当前目录；多文件串行上传（单连接模型，勿并发）。 */
  const uploadPaths = async (localPaths: string[]) => {
    if (!connected || localPaths.length === 0) return;
    for (const lp of localPaths) {
      const remotePath = joinRemote(currentPath, pathBase(lp));
      const transferId = newTransferId();
      onTransferChange?.(transferId);
      try {
        log(await api.sftpClientUpload(lp, remotePath, transferId), "ok");
      } catch (e) {
        log(`上传失败: ${e}`, "error");
      } finally {
        onTransferChange?.(null);
      }
    }
    // 全部结束后重拉当前目录，让新文件出现在树里
    await openDir(currentPath, true);
  };

  /** 「上传」按钮：打开系统文件选择器（可多选），选中的文件上传到当前目录。 */
  const pickAndUpload = async () => {
    let files: string[] | null;
    try {
      files = await pickOpenFiles();
    } catch (e) {
      log(`打开文件选择器失败: ${e}`, "error");
      return;
    }
    if (files) await uploadPaths(files);
  };

  /**
   * 批量下载（树里勾选多个文件）：先选一次目标目录，勾选的文件依次下到该
   * 目录（保留原文件名），失败不中断后续文件。
   */
  const downloadMany = async (picked: RemoteEntry[]) => {
    if (picked.length === 0) return;
    let dir: string | null;
    try {
      dir = await pickOpenDirectory();
    } catch (e) {
      log(`打开目录选择器失败: ${e}`, "error");
      return;
    }
    if (!dir) return;
    for (const e of picked) {
      const transferId = newTransferId();
      onTransferChange?.(transferId);
      try {
        log(
          await api.sftpClientDownload(
            joinRemote(currentPath, e.name),
            joinDefault(dir, e.name),
            transferId
          ),
          "ok"
        );
      } catch (err) {
        log(`下载失败: ${e.name}: ${err}`, "error");
      } finally {
        onTransferChange?.(null);
      }
    }
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

  const openKnownHosts = async () => {
    setManageBusy(true);
    try {
      setKnownHosts(await api.sftpClientListKnownHosts());
      setSelectedHosts(new Set());
    } catch (e) {
      log(`读取已信任主机记录失败: ${e}`, "error");
    } finally {
      setManageBusy(false);
    }
  };

  const toggleKnownHost = (endpoint: string) => {
    setSelectedHosts((prev) => {
      const next = new Set(prev);
      if (next.has(endpoint)) next.delete(endpoint);
      else next.add(endpoint);
      return next;
    });
  };

  const removeSelectedHosts = async () => {
    if (selectedHosts.size === 0) return;
    setManageBusy(true);
    try {
      log(await api.sftpClientRemoveKnownHosts([...selectedHosts]), "ok");
      const remaining = (knownHosts ?? []).filter((r) => !selectedHosts.has(r.endpoint));
      setKnownHosts(remaining);
      setSelectedHosts(new Set());
    } catch (e) {
      log(`移除已信任主机记录失败: ${e}`, "error");
    } finally {
      setManageBusy(false);
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
            <input value={host} onChange={(e) => setHost(e.target.value)} disabled={connected} />
          </label>
          <label className="field narrow">
            <span>端口（默认 22）</span>
            <input
              type="number"
              min={1}
              max={65535}
              value={port}
              onChange={(e) => setPort(e.target.value)}
              disabled={connected}
              placeholder="22"
            />
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
        {/* 首连/密钥变化走模态确认框，这里只放一句常规说明 */}
        <div className="hint-line">
          首次连接会要求确认服务器主机密钥指纹（TOFU）；指纹变化时会明确警告。
          本应用 SFTP 服务器默认端口为 2222。
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
        </div>
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
          onDownloadMany={(picked) => void downloadMany(picked)}
        />
      </div>

      <div className="card">
        <div className="card-title">设置</div>
        <div className="hint-line">
          已信任的主机密钥记录（TOFU）保存在本机；可按需移除单条记录，移除后对应服务器会重新走首连确认流程。
        </div>
        <div className="actions">
          <button
            className="btn"
            onClick={knownHosts ? () => setKnownHosts(null) : openKnownHosts}
            disabled={manageBusy}
          >
            {knownHosts ? "收起已信任主机列表" : "管理已信任主机…"}
          </button>
        </div>
        {knownHosts && (
          <div className="known-host-list">
            {knownHosts.length === 0 ? (
              <div className="hint-line">暂无已信任的主机记录。</div>
            ) : (
              <>
                <label className="known-host-item known-host-head">
                  <input
                    type="checkbox"
                    checked={selectedHosts.size === knownHosts.length}
                    onChange={(e) =>
                      setSelectedHosts(
                        e.target.checked ? new Set(knownHosts.map((r) => r.endpoint)) : new Set()
                      )
                    }
                  />
                  <span>全选</span>
                </label>
                {knownHosts.map((r) => (
                  <label key={r.endpoint} className="known-host-item">
                    <input
                      type="checkbox"
                      checked={selectedHosts.has(r.endpoint)}
                      onChange={() => toggleKnownHost(r.endpoint)}
                    />
                    <span className="known-host-endpoint">{r.endpoint}</span>
                    <span className="known-host-fingerprint">{r.fingerprint}</span>
                  </label>
                ))}
                <div className="actions">
                  <button
                    className="btn danger"
                    onClick={removeSelectedHosts}
                    disabled={manageBusy || selectedHosts.size === 0}
                  >
                    移除所选（{selectedHosts.size}）
                  </button>
                  <button className="btn" onClick={() => setKnownHosts(null)} disabled={manageBusy}>
                    取消
                  </button>
                </div>
              </>
            )}
          </div>
        )}
      </div>

      {/* TOFU 主机密钥确认框（§4.4）：unknown 首连确认 / changed 变更警告 */}
      {prompt && (
        <div className="modal-overlay" role="dialog" aria-modal="true">
          <div className="card modal-card">
            <div className="card-head">
              <div className="card-title">
                {prompt.kind === "changed" ? "警告：主机密钥已变化" : "确认新的主机密钥"}
              </div>
            </div>
            {prompt.kind === "unknown" ? (
              <div className="hint-line">
                首次连接该服务器。请通过可信渠道（服务器控制台、管理员）核对以下主机密钥指纹，
                确认无误后再信任；本机将记录该指纹，之后连接会自动比对。
              </div>
            ) : (
              <div className="hint-line warn">
                ⚠ 该服务器返回的主机密钥与上次记录的不一致 —— 它可能不是上次那台服务器
                （系统重装、密钥重新生成，或连接正被冒充）。仅当你确知变化原因时才应覆盖记录并继续。
              </div>
            )}
            <div className="modal-fingerprint">{prompt.fingerprint}</div>
            <div className="actions">
              <button
                className={prompt.kind === "changed" ? "btn danger" : "btn primary"}
                onClick={confirmPrompt}
                disabled={busy}
              >
                {prompt.kind === "changed" ? "覆盖记录并连接" : "信任并连接"}
              </button>
              <button className="btn" onClick={() => setPrompt(null)} disabled={busy}>
                取消
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
