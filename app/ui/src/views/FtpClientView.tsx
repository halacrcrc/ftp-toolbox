import { useEffect, useRef, useState } from "react";
import { api, newTransferId, pathBase, pathSafeName, pickOpenDirectory, pickOpenFiles, FtpEntry, ServerStatus } from "../api";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import RemoteTree, { RemoteEntry } from "../components/RemoteTree";
import { joinDefault } from "./FtpPage";
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
  // 远端文件树状态：当前目录 + 该目录条目 + 按路径缓存（返回上一级免重拉）
  const [currentPath, setCurrentPath] = useState("");
  const [entries, setEntries] = useState<RemoteEntry[] | null>(null);
  const [treeLoading, setTreeLoading] = useState(false);
  const treeCache = useRef(new Map<string, RemoteEntry[]>());
  // 手输路径草稿：树导航时同步显示当前位置；回车/「跳转」按草稿打开目录
  const [pathDraft, setPathDraft] = useState("");
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

  /**
   * 递归上传一个本地文件夹：远端按父先序逐级建目录后逐文件上传（串行），
   * 返回 [成功文件数, 失败文件数]。mkdir 对已存在目录会报错——重传到既有
   * 结构是常见场景，按提示日志处理、不阻断不计失败，文件错误才是真信号。
   */
  const uploadFolderInto = async (
    localRoot: string,
    remoteBase: string
  ): Promise<[number, number]> => {
    let ok = 0;
    let failed = 0;
    const w = await api.localWalk(localRoot);
    for (const s of w.skipped) log(`跳过链接 ${s}（可能成环，不参与上传）`);
    const mk = async (p: string) => {
      try {
        await api.ftpMkdir(p);
      } catch {
        log(`目录已存在或创建失败，继续: ${p}`);
      }
    };
    await mk(remoteBase);
    for (const d of w.dirs) await mk(joinRemote(remoteBase, d));
    for (const f of w.files) {
      const transferId = newTransferId();
      onTransferChange?.(transferId);
      try {
        log(
          await api.ftpUpload(joinDefault(localRoot, f), joinRemote(remoteBase, f), transferId),
          "ok"
        );
        ok++;
      } catch (e) {
        failed++;
        log(`上传失败: ${f}: ${e}`, "error");
      } finally {
        onTransferChange?.(null);
      }
    }
    return [ok, failed];
  };

  const uploadPaths = async (localPaths: string[]) => {
    if (!connected || localPaths.length === 0) return;
    for (const lp of localPaths) {
      let isDir = false;
      try {
        isDir = await api.localIsDir(lp);
      } catch (e) {
        log(`上传失败: ${pathBase(lp)}: ${e}`, "error");
        continue;
      }
      if (isDir) {
        // 文件夹：远端镜像本地结构（去掉尾随分隔符再取末段当远端目录名）。
        const name = pathBase(lp.replace(/[\\/]+$/, "")) || "文件夹";
        try {
          const [ok, failed] = await uploadFolderInto(lp, joinRemote(currentPath, name));
          log(
            `文件夹上传结束: ${name}（${ok} 个文件${failed > 0 ? `，${failed} 个失败` : ""}）`,
            failed > 0 ? "error" : "ok"
          );
        } catch (e) {
          log(`文件夹上传失败: ${name}: ${e}`, "error");
        }
      } else {
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
    }
    // 全部结束后重拉当前目录，让新内容出现在树里
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

  /** 「传文件夹」按钮：打开系统目录选择器，整个文件夹递归上传到当前目录。 */
  const pickAndUploadFolder = async () => {
    let dir: string | null;
    try {
      dir = await pickOpenDirectory();
    } catch (e) {
      log(`打开目录选择器失败: ${e}`, "error");
      return;
    }
    if (dir) await uploadPaths([dir]);
  };

  /**
   * 批量下载（树里勾选文件/文件夹）：先选一次目标目录。文件直接下到该目录；
   * 文件夹先建本地目录再递归拉取，保持目录结构。串行执行（单连接模型，勿
   * 并发），单个失败不中断，结尾汇总成功/失败数。
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
    let okFiles = 0;
    let failed = 0;
    const runFile = async (remotePath: string, localPath: string) => {
      const transferId = newTransferId();
      onTransferChange?.(transferId);
      try {
        log(await api.ftpDownload(remotePath, localPath, transferId), "ok");
        okFiles++;
      } catch (err) {
        failed++;
        log(`下载失败: ${remotePath}: ${err}`, "error");
      } finally {
        onTransferChange?.(null);
      }
    };
    const walk = async (entries: RemoteEntry[], remoteDir: string, localDir: string) => {
      for (const e of entries) {
        if (e.kind === "file") {
          await runFile(joinRemote(remoteDir, e.name), joinDefault(localDir, pathSafeName(e.name)));
        } else if (e.kind === "dir") {
          const sub = joinDefault(localDir, pathSafeName(e.name));
          try {
            await api.createLocalDir(sub);
          } catch (err) {
            failed++;
            log(`创建目录失败: ${sub}: ${err}`, "error");
            continue;
          }
          try {
            const kids = await api.ftpListDetailed(joinRemote(remoteDir, e.name));
            await walk(kids.map(toRemoteEntry), joinRemote(remoteDir, e.name), sub);
          } catch (err) {
            failed++;
            log(`列目录失败: ${joinRemote(remoteDir, e.name)}: ${err}`, "error");
          }
        } else {
          // 链接/特殊条目不下载不递归：链接语义随服务器而异，还可能成环。
          log(`跳过 ${e.name}（${e.kind === "symlink" ? "链接" : "类型未知"}）`);
        }
      }
    };
    await walk(picked, currentPath, dir);
    if (failed > 0) log(`批量下载结束：${okFiles} 个成功，${failed} 个失败`, "error");
    else log(`批量下载结束：共 ${okFiles} 个文件`, "ok");
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
          onUpload={() => void pickAndUpload()}
          onUploadFolder={() => void pickAndUploadFolder()}
          onDownloadMany={(picked) => void downloadMany(picked)}
        />
      </div>
    </div>
  );
}
