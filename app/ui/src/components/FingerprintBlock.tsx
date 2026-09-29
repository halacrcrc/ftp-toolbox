import { useState } from "react";

interface Props {
  /** 折叠行文案：「证书详情」（FTPS）/「主机密钥详情」（SFTP）。 */
  label: string;
  /** 展开后的指纹前缀说明，如「证书指纹（SHA-256）」。 */
  fingerprintLabel: string;
  /** 指纹值；null/undefined 表示尚未生成（展开后显示 emptyHint）。 */
  fingerprint?: string | null;
  /** 点击「重新生成」；不传（如服务器运行中）则不渲染按钮。 */
  onRegenerate?: () => void;
  busy?: boolean;
  /** 尚未生成指纹时的提示文案。 */
  emptyHint?: string;
}

/**
 * 折叠指纹区（设计文档 §4.3，FTPS 证书与 SFTP 主机密钥共用）。
 *
 * 默认收起，展开显示指纹（等宽 + break-all）与「重新生成」按钮；
 * 折叠区**永远随卡片渲染**、不随任何开关消失 —— 对端核对身份要随时可查，
 * 「关掉开关再看」会让指纹看起来像是换过证书/密钥。
 * 展开动画用 grid-template-rows 0fr→1fr（内容真实高度驱动，
 * 尊重 prefers-reduced-motion，见 styles.css）。
 */
export default function FingerprintBlock({
  label,
  fingerprintLabel,
  fingerprint,
  onRegenerate,
  busy,
  emptyHint = "尚未生成，首次使用时自动创建",
}: Props) {
  const [open, setOpen] = useState(false);

  return (
    <div className="fingerprint-block">
      <button
        type="button"
        className="fingerprint-toggle"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
      >
        <span>{label}</span>
        {/* chevron 随 aria-expanded 旋转（CSS 属性选择器驱动，无需额外类） */}
        <svg
          className="fingerprint-chevron"
          viewBox="0 0 16 16"
          width="13"
          height="13"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.5"
        >
          <path d="M6 4l4 4-4 4" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      </button>
      <div className={`fingerprint-collapse${open ? " open" : ""}`}>
        {/* overflow:hidden + min-height:0 是 0fr→1fr 动画的必要配套 */}
        <div className="fingerprint-inner">
          {fingerprint ? (
            <div className="fingerprint-body">
              <span>
                {fingerprintLabel}：
                <span className="fingerprint-value">{fingerprint}</span>
              </span>
              {onRegenerate && (
                <button className="btn small" onClick={onRegenerate} disabled={busy}>
                  重新生成
                </button>
              )}
            </div>
          ) : (
            <div className="fingerprint-body">{emptyHint}</div>
          )}
        </div>
      </div>
    </div>
  );
}
