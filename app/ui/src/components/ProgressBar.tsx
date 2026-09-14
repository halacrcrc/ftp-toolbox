import { fmtBytes } from "../api";
import { Progress } from "../App";

export default function ProgressBar({ progress }: { progress: Progress | null }) {
  if (!progress) return <footer className="progress-footer empty" />;
  const pct =
    progress.total && progress.total > 0
      ? Math.min(100, Math.round((progress.bytes / progress.total) * 100))
      : null;
  return (
    <footer className="progress-footer">
      <span className="progress-label">
        {progress.kind === "upload" ? "上传中" : "下载中"} · {progress.file}
      </span>
      <div className="progress-track">
        <div
          className={`progress-fill${pct === null ? " indeterminate" : ""}`}
          style={pct === null ? undefined : { width: `${pct}%` }}
        />
      </div>
      <span className="progress-value">
        {pct !== null ? `${pct}% · ` : ""}
        {fmtBytes(progress.bytes)}
        {progress.total ? ` / ${fmtBytes(progress.total)}` : ""}
      </span>
    </footer>
  );
}
