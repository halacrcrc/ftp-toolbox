import { useState } from "react";
import { api } from "../api";
import { LogEntry } from "../App";

type Log = (text: string, level?: LogEntry["level"]) => void;

export default function TftpClientView({ log }: { log: Log }) {
  const [server, setServer] = useState("127.0.0.1:6969");
  const [local, setLocal] = useState("C:\\tftp-root\\hello.txt");
  const [remote, setRemote] = useState("hello.txt");

  const transfer = async (kind: "upload" | "download") => {
    try {
      const msg =
        kind === "upload"
          ? await api.tftpUpload(server, local, remote)
          : await api.tftpDownload(server, remote, local);
      log(msg, "ok");
    } catch (e) {
      log(`${kind === "upload" ? "上传" : "下载"}失败: ${e}`, "error");
    }
  };

  return (
    <div className="card" style={{ maxWidth: 640 }}>
      <div className="card-head">
        <div>
          <div className="card-title">TFTP 传输</div>
          <div className="card-desc">无连接协议，直接填写服务器地址即可传输</div>
        </div>
      </div>
      <label className="field">
        <span>服务器地址</span>
        <input value={server} onChange={(e) => setServer(e.target.value)} />
      </label>
      <label className="field">
        <span>本地文件</span>
        <input value={local} onChange={(e) => setLocal(e.target.value)} />
      </label>
      <label className="field">
        <span>远程文件名</span>
        <input value={remote} onChange={(e) => setRemote(e.target.value)} />
      </label>
      <div className="actions">
        <button className="btn primary" onClick={() => transfer("upload")}>
          上传
        </button>
        <button className="btn" onClick={() => transfer("download")}>
          下载
        </button>
      </div>
    </div>
  );
}
