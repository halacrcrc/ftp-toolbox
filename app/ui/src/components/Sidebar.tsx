import iconUrl from "../assets/icon.png";

export type ViewKey = "ftp" | "tftp" | "sftp" | "logs";

interface Props {
  active: ViewKey;
  onNavigate: (v: ViewKey) => void;
}

const ITEMS: { key: ViewKey; label: string; icon: JSX.Element }[] = [
  {
    key: "ftp",
    label: "FTP",
    icon: (
      <svg viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.5">
        <path d="M2 4.5A1.5 1.5 0 0 1 3.5 3h3l1.5 2h4.5A1.5 1.5 0 0 1 14 6.5v5A1.5 1.5 0 0 1 12.5 13h-9A1.5 1.5 0 0 1 2 11.5v-7Z" />
      </svg>
    ),
  },
  {
    key: "tftp",
    label: "TFTP",
    icon: (
      <svg viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.5">
        <path d="M5 2v8m0 0L2.5 7.5M5 10l2.5-2.5M11 14V6m0 0 2.5 2.5M11 6 8.5 8.5" strokeLinecap="round" strokeLinejoin="round" />
      </svg>
    ),
  },
  {
    key: "sftp",
    label: "SFTP",
    icon: (
      // 终端提示符造型：SFTP 跑在 SSH 之上，与另外两个「文件夹/传输」图标区分开
      <svg viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.5">
        <rect x="2" y="2.5" width="12" height="11" rx="1.5" />
        <path d="M4.5 6l2 2-2 2M8 10.5h3.5" strokeLinecap="round" strokeLinejoin="round" />
      </svg>
    ),
  },
  {
    key: "logs",
    label: "运行日志",
    icon: (
      <svg viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.5">
        <path d="M4 2.5h8A1.5 1.5 0 0 1 13.5 4v8a1.5 1.5 0 0 1-1.5 1.5H4A1.5 1.5 0 0 1 2.5 12V4A1.5 1.5 0 0 1 4 2.5Z" />
        <path d="M5.5 6h5M5.5 8.5h5M5.5 11h3" strokeLinecap="round" />
      </svg>
    ),
  },
];

export default function Sidebar({ active, onNavigate }: Props) {
  return (
    <aside className="sidebar">
      <div className="brand">
        {/* 品牌标直接用打包图标本身（icons/icon.ico 的 128px 帧），
            保证侧栏里的图标和 exe / 安装包永远长得一样 */}
        <span className="brand-mark">
          <img src={iconUrl} alt="FTP 工具箱" width={30} height={30} />
        </span>
        <span className="brand-name">FTP 工具箱</span>
      </div>
      <nav>
        {ITEMS.map((it) => (
          <button
            key={it.key}
            className={`nav-item${active === it.key ? " active" : ""}`}
            onClick={() => onNavigate(it.key)}
          >
            {it.icon}
            <span>{it.label}</span>
          </button>
        ))}
      </nav>
      <div className="sidebar-footer">Tauri · React · Rust</div>
    </aside>
  );
}
