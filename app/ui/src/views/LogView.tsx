import { useEffect, useRef } from "react";
import { LogEntry } from "../App";

interface Props {
  logs: LogEntry[];
  onClear: () => void;
}

export default function LogView({ logs, onClear }: Props) {
  const endRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [logs]);

  return (
    <div className="card log-card">
      <div className="card-head">
        <div className="card-title">日志（{logs.length}）</div>
        <button className="btn small" onClick={onClear}>
          清空
        </button>
      </div>
      <div className="log-list">
        {logs.length === 0 && <div className="log-empty">暂无日志</div>}
        {logs.map((l, i) => (
          <div key={i} className={`log-line ${l.level}`}>
            <span className="log-time">{l.time}</span>
            <span>{l.text}</span>
          </div>
        ))}
        <div ref={endRef} />
      </div>
    </div>
  );
}
