import { crumbsRemote, isRoot, RemoteCrumb } from "../lib/remotepath";
import { fmtBytes } from "../lib/format";

/** 远端目录条目（协议无关）：由各 View 把后端条目映射成本结构。 */
export interface RemoteEntry {
  name: string;
  kind: "file" | "dir" | "symlink" | "other";
  /** 字节；未知（后端没回）则渲染 "—"。 */
  size?: number;
  /** Unix 秒；未知则渲染 "—"。 */
  mtime?: number;
}

interface RemoteTreeProps {
  /** 当前目录的孩子条目；null 表示尚未列过（连接后未加载）。 */
  entries: RemoteEntry[] | null;
  /** 当前远端目录（"" 与 "/" 都视为根，与后端 list(None)→"/" 的语义一致）。 */
  currentPath: string;
  /** 正在拉取目录时为 true，行区显示加载态。 */
  loading: boolean;
  /** 未连接/断开中：禁用全部交互。 */
  disabled: boolean;
  /** 点目录行 → 父组件去列该子目录。 */
  onNavigate(dirName: string): void;
  /** 面包屑点击 → 按完整路径跳转（根节点名 "/" 不是子目录名，须走路径）。 */
  onCrumb(path: string): void;
  /** 返回上级（根时组件自行禁用）。 */
  onUp(): void;
  /** 强制重拉当前目录。 */
  onRefresh(): void;
  /** 点文件行触发下载（M1 仅占位，M2 接保存对话框）。 */
  onDownload(entry: RemoteEntry): void;
  /** 可选：点「上传」按钮打开系统文件选择器（父组件接手后续上传流程）。 */
  onUpload?(): void;
}

/** 行首类型标记，与旧 `<pre>` 列表的 d/-/l 记法保持一致。 */
function kindMark(kind: RemoteEntry["kind"]): string {
  switch (kind) {
    case "dir":
      return "d";
    case "symlink":
      return "l";
    case "file":
      return "-";
    default:
      return "?";
  }
}

/** 目录在前、同类按名称排序，树的可读性优先。 */
function sortEntries(entries: RemoteEntry[]): RemoteEntry[] {
  return [...entries].sort((a, b) => {
    if ((a.kind === "dir") !== (b.kind === "dir")) return a.kind === "dir" ? -1 : 1;
    return a.name.localeCompare(b.name);
  });
}

/**
 * 协议无关的远端文件树（受控组件，不感知 FTP/SFTP，也不 import 后端 API）：
 * 面包屑 + 上级/刷新 + 条目列表。目录行点按下钻，文件行点按触发 onDownload。
 */
export default function RemoteTree({
  entries,
  currentPath,
  loading,
  disabled,
  onNavigate,
  onCrumb,
  onUp,
  onRefresh,
  onDownload,
  onUpload,
}: RemoteTreeProps) {
  const crumbs: RemoteCrumb[] = crumbsRemote(currentPath);
  const atRoot = isRoot(currentPath);

  return (
    <div className="remote-tree">
      <div className="remote-tree-bar">
        <div className="remote-tree-crumbs">
          {crumbs.map((c, i) => (
            <span key={c.path} className="remote-tree-crumb">
              {i > 0 && <span className="remote-tree-sep">/</span>}
              <button
                type="button"
                className="remote-tree-crumb-btn"
                disabled={disabled}
                onClick={() => onCrumb(c.path)}
                title={c.path}
              >
                {c.name}
              </button>
            </span>
          ))}
        </div>
        <div className="remote-tree-actions">
          <button
            type="button"
            className="btn small"
            disabled={disabled || atRoot}
            onClick={onUp}
          >
            上级
          </button>
          <button
            type="button"
            className="btn small"
            disabled={disabled}
            onClick={onRefresh}
          >
            刷新
          </button>
          {onUpload && (
            <button
              type="button"
              className="btn small"
              disabled={disabled}
              onClick={onUpload}
            >
              上传
            </button>
          )}
        </div>
      </div>
      <div className="remote-tree-body">
        {!disabled && (
          <div className="remote-tree-hint">
            点「上传」选择文件、或把文件拖到此区域，即可上传到当前目录；点文件名下载。
          </div>
        )}
        {loading ? (
          <div className="remote-tree-empty">加载中…</div>
        ) : entries === null ? (
          <div className="remote-tree-empty">尚未列出目录内容。</div>
        ) : entries.length === 0 ? (
          <div className="remote-tree-empty">（空目录）</div>
        ) : (
          sortEntries(entries).map((e) => (
            <div
              key={e.name}
              className={`remote-tree-row${e.kind === "dir" ? " is-dir" : ""}`}
              role={e.kind === "dir" || e.kind === "file" ? "button" : undefined}
              tabIndex={disabled || (e.kind !== "dir" && e.kind !== "file") ? -1 : 0}
              onClick={() => {
                if (disabled) return;
                if (e.kind === "dir") onNavigate(e.name);
                else if (e.kind === "file") onDownload(e);
              }}
              onKeyDown={(ev) => {
                if (ev.key !== "Enter" && ev.key !== " ") return;
                ev.preventDefault();
                if (disabled) return;
                if (e.kind === "dir") onNavigate(e.name);
                else if (e.kind === "file") onDownload(e);
              }}
              title={e.name}
            >
              <span className="remote-tree-mark">{kindMark(e.kind)}</span>
              <span className="remote-tree-name">{e.name}</span>
              <span className="remote-tree-size">{e.size === undefined ? "—" : fmtBytes(e.size)}</span>
              <span className="remote-tree-time">
                {e.mtime === undefined ? "—" : new Date(e.mtime * 1000).toLocaleString()}
              </span>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
