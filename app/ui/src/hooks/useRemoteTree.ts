import { useEffect, useRef, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { api, newTransferId, pathBase, pathSafeName, pickOpenDirectory, pickOpenFiles } from "../api";
import type { RemoteEntry } from "../components/RemoteTree";
import { joinDefault } from "../views/FtpPage";
import { joinRemote } from "../lib/remotepath";
import type { LogEntry } from "../App";

type Log = (text: string, level?: LogEntry["level"]) => void;

/** 传输开始/结束时上报取消令牌 id（App 据此启用进度条上的「取消」按钮）。 */
type TransferChange = (id: string | null) => void;

/**
 * 远端文件树的协议差异收口：FTP 与 SFTP 客户端页的树状态、目录缓存、批量
 * 上传/下载、拖拽上传完全同构，仅 IPC 命令名不同（评审 #31）。各 View 把
 * 自己的命令包成这四个操作传进来（list 需先把协议条目映射成 RemoteEntry），
 * 传输/浏览行为由此 hook 统一持有。
 */
export interface RemoteTreeOps {
  /** 列目录；空路径 = 服务器默认目录（后端 list(None) 落到 "/"）。 */
  list(path?: string): Promise<RemoteEntry[]>;
  upload(local: string, remote: string, transferId: string): Promise<string>;
  download(remote: string, local: string, transferId: string): Promise<string>;
  /** 建远端目录（递归上传用）；已存在会报错，由 hook 内复核后继续。 */
  mkdir(path: string): Promise<string>;
}

/**
 * 远端文件树完整状态与动作。批量传输（上传/下载/文件夹递归）进行中：
 * - 头部动作按钮禁用（actionsDisabled = batchBusy），浏览不受影响；
 * - 后端会话锁保证协议命令严格排队，单控制连接不会被交错命令破坏，
 *   但两批交错会互相覆盖取消令牌、日志交错难读，所以入口直接拒重入；
 * - openDir 守卫见其注释。
 */
export default function useRemoteTree(
  ops: RemoteTreeOps,
  log: Log,
  connected: boolean,
  onTransferChange?: TransferChange
) {
  // 远端文件树状态：当前目录 + 该目录条目 + 按路径缓存（返回上一级免重拉）
  const [currentPath, setCurrentPath] = useState("");
  const [entries, setEntries] = useState<RemoteEntry[] | null>(null);
  const [treeLoading, setTreeLoading] = useState(false);
  const treeCache = useRef(new Map<string, RemoteEntry[]>());
  // 手输路径草稿（FTP 页的路径输入框用；SFTP 页无输入框，跟随维护无成本）
  const [pathDraft, setPathDraft] = useState("");
  // 批量传输（上传/下载文件夹或多选）进行中：忽略新的批量触发。后端会话锁
  // 虽保证不损坏连接，但两批交错会互相覆盖取消令牌、日志交错难读。
  const [batchBusy, setBatchBusy] = useState(false);
  // 与 state 同步的 ref：openDir 的守卫读它。批量收尾在 finally 里先清 ref
  // 再刷新目录——state 更新是异步的，openDir 读渲染闭包里的 state 会把
  // 自己的收尾刷新也挡掉。
  const batchBusyRef = useRef(false);
  const setBatch = (v: boolean) => {
    batchBusyRef.current = v;
    setBatchBusy(v);
  };

  /** 断开/停服后清空树：缓存一并丢弃，重连后从根重新列出。 */
  const resetTree = () => {
    treeCache.current = new Map();
    setEntries(null);
    setCurrentPath("");
    setPathDraft("");
  };

  /**
   * 打开远端目录并切换树视图。非 force 时命中缓存直接切换（面包屑/返回上一级
   * 免重拉）；force 用于刷新与连上后的首次加载。空路径 = 服务器默认目录。
   * 成功后同步手输框为实际位置（SFTP 页忽略该草稿）。
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
    // 批量传输持有会话锁，LIST 会排队到传输结束（FTP 单控制连接跑不了
    // 并发命令）——与其树卡「加载中」几分钟，不如保持当前内容继续浏览。
    // 已列过的目录在上面缓存命中处即时切换，不受影响。
    if (batchBusyRef.current) {
      log("批量传输进行中，暂不能列出新目录，结束后可刷新");
      return;
    }
    setTreeLoading(true);
    try {
      const mapped = await ops.list(path || undefined);
      treeCache.current.set(path, mapped);
      setEntries(mapped);
      setCurrentPath(path);
      setPathDraft(path);
      log(`列出 ${mapped.length} 个条目`);
    } catch (e) {
      log(`列目录失败: ${e}`, "error");
    } finally {
      setTreeLoading(false);
    }
  };

  /**
   * 递归上传一个本地文件夹：远端按父先序逐级建目录后逐文件上传（串行），
   * 返回 [成功文件数, 失败文件数]。mkdir 对已存在目录会报错——重传到既有
   * 结构是常见场景，不能直接吞：mkdir 失败后用一次列目录复核，能列出 =
   * 目录确实在（继续）；列不出 = 真不可用（权限拒绝等），其下文件全部计
   * 失败跳过，不再静默（评审 #34/#35，失败口径统一为「文件数」）。
   */
  const uploadFolderInto = async (
    localRoot: string,
    remoteBase: string
  ): Promise<[number, number]> => {
    let ok = 0;
    let failed = 0;
    const w = await api.localWalk(localRoot);
    for (const s of w.skipped) log(`跳过链接 ${s}（可能成环，不参与上传）`);
    const okDirs = new Set<string>();
    const mk = async (rel: string, p: string): Promise<boolean> => {
      try {
        await ops.mkdir(p);
        return true;
      } catch {
        try {
          await ops.list(p);
          log(`目录已存在，继续: ${p}`);
          return true;
        } catch (e) {
          log(`目录不可用，其下文件跳过: ${p}: ${e}`, "error");
          return false;
        }
      }
    };
    if (await mk("", remoteBase)) okDirs.add("");
    for (const d of w.dirs) {
      if (await mk(d, joinRemote(remoteBase, d))) okDirs.add(d);
    }
    for (const f of w.files) {
      // 文件的直接父目录不可用 → 该文件无法上传（建目录是父先序，直接父
      // 可用即整条链可用）。
      const i = Math.max(f.lastIndexOf("/"), f.lastIndexOf("\\"));
      const dirRel = i > 0 ? f.slice(0, i) : "";
      if (!okDirs.has(dirRel)) {
        failed++;
        log(`跳过 ${f}（所在目录创建失败）`, "error");
        continue;
      }
      const transferId = newTransferId();
      onTransferChange?.(transferId);
      try {
        log(
          await ops.upload(joinDefault(localRoot, f), joinRemote(remoteBase, f), transferId),
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

  /** 批量上传入口：文件直接传，文件夹递归镜像本地结构。 */
  const uploadPaths = async (localPaths: string[]) => {
    if (batchBusy) {
      log("已有批量传输进行中，本次上传已忽略");
      return;
    }
    if (!connected || localPaths.length === 0) return;
    setBatch(true);
    try {
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
            log(await ops.upload(lp, remotePath, transferId), "ok");
          } catch (e) {
            log(`上传失败: ${e}`, "error");
          } finally {
            onTransferChange?.(null);
          }
        }
      }
    } finally {
      setBatch(false);
      // 全部结束后重拉当前目录，让新内容出现在树里；先清 busy 再刷新，
      // 否则 openDir 的传输守卫会把它挡掉。
      await openDir(currentPath, true);
    }
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

  /** 「上传文件夹」按钮：打开系统目录选择器，整个文件夹递归上传到当前目录。 */
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
    if (batchBusy) {
      log("已有批量传输进行中，本次下载已忽略");
      return;
    }
    if (picked.length === 0) return;
    let dir: string | null;
    try {
      dir = await pickOpenDirectory();
    } catch (e) {
      log(`打开目录选择器失败: ${e}`, "error");
      return;
    }
    if (!dir) return;
    setBatch(true);
    try {
      let okFiles = 0;
      let failed = 0;
      const runFile = async (remotePath: string, localPath: string) => {
        const transferId = newTransferId();
        onTransferChange?.(transferId);
        try {
          log(await ops.download(remotePath, localPath, transferId), "ok");
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
              // 建不出来不 continue：子树照常递归，文件会因本地目录缺失
              // 各自失败计数——口径保持「失败文件数」（评审 #35）。
              log(`创建目录失败（其下文件将逐一失败）: ${sub}: ${err}`, "error");
            }
            try {
              const kids = await ops.list(joinRemote(remoteDir, e.name));
              await walk(kids, joinRemote(remoteDir, e.name), sub);
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
    } finally {
      setBatch(false);
    }
  };

  // Tauri v2 拦截了 HTML5 drop 事件（dragDropEnabled 默认开），拖拽上传只能走
  // webview 原生 onDragDropEvent：drop 事件直接给字符串绝对路径。用落点坐标做
  // 一次「是否落在树区域」的命中检测（elementFromPoint + closest），避免在日志
  // 页/其它区域误触发。position 是物理像素，除以 devicePixelRatio 换算 CSS 坐标。
  // 无依赖数组：每次渲染重订阅，保证 uploadPaths 闭包拿到最新状态。
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

  return {
    entries,
    currentPath,
    treeLoading,
    batchBusy,
    pathDraft,
    setPathDraft,
    openDir,
    resetTree,
    uploadPaths,
    pickAndUpload,
    pickAndUploadFolder,
    downloadMany,
  };
}
