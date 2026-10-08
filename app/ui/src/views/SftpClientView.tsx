import { useState } from "react";
import { api, newTransferId, pathBase, HostKeyStatus, SftpEntry } from "../api";
import { fmtBytes } from "../lib/format";
import LocalFileField from "../components/LocalFileField";
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
 * SftpEntry → 列表行。与 FTP 客户端的 LIST 原始行不同，SFTP 拿到的是结构化
 * 条目，这里拼成同风格的短行。
 *
 * `fileType` 的契约已落地为 `"file" | "dir" | "symlink" | "other"`
 * （`crates/ftp-core/src/sftp/client.rs::SftpEntry`）。这里刻意保留前缀/子串
 * 的宽容匹配（而不是枚举硬比对），是为了后端将来扩展取值时不至于把未知类型
 * 渲染错 —— 未匹配上的一律按普通文件 `-` 显示。
 */
function formatEntry(e: SftpEntry): string {
  const t = e.fileType.toLowerCase();
  const mark = t.startsWith("d") ? "d" : t.startsWith("l") || t.includes("link") ? "l" : "-";
  const time = e.mtime ? ` ${new Date(e.mtime * 1000).toLocaleString()}` : "";
  return `${mark} ${e.name}（${fmtBytes(e.size)}）${time}`;
}

export default function SftpClientView({
  log,
  onTransferChange,
}: {
  log: Log;
  onTransferChange?: TransferChange;
}) {
  // 连接远端用标准 SSH 端口 22（本应用自己的服务器默认 2222，输入框可改）
  const [host, setHost] = useState("127.0.0.1");
  const [port, setPort] = useState("22");
  const [user, setUser] = useState("");
  const [pass, setPass] = useState("");
  const [connected, setConnected] = useState(false);
  const [busy, setBusy] = useState(false);
  const [remotePath, setRemotePath] = useState("");
  const [listing, setListing] = useState<SftpEntry[] | null>(null);
  const [local, setLocal] = useState("C:\\sftp-root\\hello.txt");
  const [remote, setRemote] = useState("hello.txt");
  const [prompt, setPrompt] = useState<HostKeyPrompt | null>(null);
  // 「清除已信任主机」两步确认：第一次点击进入确认态，再点才真正执行
  const [confirmClear, setConfirmClear] = useState(false);

  const portNum = () => Number(port) || 22;

  /** 真正发起连接；trustNewHost 仅在用户确认过 unknown 指纹后为 true。 */
  const doConnect = async (trustNewHost: boolean) => {
    setBusy(true);
    try {
      const msg = await api.sftpClientConnect(host, portNum(), user, pass, trustNewHost);
      log(msg, "ok");
      setConnected(true);
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
      setListing(null);
      setBusy(false);
    }
  };

  const refresh = async () => {
    try {
      const items = await api.sftpClientList(remotePath || undefined);
      setListing(items);
      log(`列出 ${items.length} 个条目`);
    } catch (e) {
      log(`列目录失败: ${e}`, "error");
    }
  };

  const transfer = async (kind: "upload" | "download") => {
    const transferId = newTransferId();
    onTransferChange?.(transferId);
    try {
      const msg =
        kind === "upload"
          ? await api.sftpClientUpload(local, remote, transferId)
          : await api.sftpClientDownload(remote, local, transferId);
      log(msg, "ok");
    } catch (e) {
      log(`${kind === "upload" ? "上传" : "下载"}失败: ${e}`, "error");
    } finally {
      onTransferChange?.(null);
    }
  };

  const clearKnownHosts = async () => {
    try {
      log(await api.sftpClientClearKnownHosts(), "ok");
    } catch (e) {
      log(`清除已信任主机失败: ${e}`, "error");
    } finally {
      setConfirmClear(false);
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
          <button className="btn small" onClick={refresh} disabled={!connected}>
            刷新列表
          </button>
        </div>
        <label className="field">
          <span>路径（留空为当前目录）</span>
          <input
            value={remotePath}
            onChange={(e) => setRemotePath(e.target.value)}
            disabled={!connected}
            placeholder="/"
          />
        </label>
        {listing !== null && (
          <pre className="listing">
            {listing.length ? listing.map(formatEntry).join("\n") : "（空目录）"}
          </pre>
        )}
      </div>

      <div className="card">
        <div className="card-title">文件传输</div>
        <LocalFileField
          value={local}
          onChange={setLocal}
          onPicked={(p) => setRemote(pathBase(p))}
          saveName={remote}
          disabled={!connected}
          log={log}
        />
        <label className="field">
          <span>远程文件名</span>
          <input value={remote} onChange={(e) => setRemote(e.target.value)} disabled={!connected} />
        </label>
        <div className="actions">
          <button className="btn primary" onClick={() => transfer("upload")} disabled={!connected}>
            上传
          </button>
          <button className="btn" onClick={() => transfer("download")} disabled={!connected}>
            下载
          </button>
        </div>
      </div>

      <div className="card">
        <div className="card-title">设置</div>
        <div className="hint-line">
          已信任的主机密钥记录（TOFU）保存在本机；清除后所有服务器都会重新走首连确认流程。
        </div>
        <div className="actions">
          {confirmClear ? (
            <>
              <button className="btn danger" onClick={clearKnownHosts} disabled={busy}>
                确认清除
              </button>
              <button className="btn" onClick={() => setConfirmClear(false)} disabled={busy}>
                取消
              </button>
            </>
          ) : (
            <button className="btn" onClick={() => setConfirmClear(true)} disabled={busy}>
              清除已信任主机…
            </button>
          )}
        </div>
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
