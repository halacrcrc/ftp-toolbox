import { fmtBytes } from "../lib/format";
import { Progress, transferLabel } from "../lib/transfer";

export default function ProgressBar({ progress }: { progress: Progress | null }) {
  if (!progress) return <footer className="progress-footer empty" />;
  const pct =
    progress.total && progress.total > 0
      ? Math.min(100, Math.round((progress.bytes / progress.total) * 100))
      : null;
  return (
    <footer className="progress-footer">
      <span className="progress-label">
        {transferLabel(progress.kind)}中 · {progress.file}
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
        {/* Speed is the only usable signal when the size is unknown (TFTP
            downloads), and it is EMA-smoothed upstream. Hidden until the first
            real sample exists — a leading "0 B/s" is just noise. */}
        {progress.speed > 0 ? ` · ${fmtBytes(progress.speed)}/s` : ""}
      </span>
    </footer>
  );
}
