import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  pickFolder,
  CertInfo,
  HostKeyInfo,
  NetInterface,
  PassivePortCheck,
  ServerStatus,
  SftpServerStatus,
} from "../api";
import FingerprintBlock from "../components/FingerprintBlock";
import { LogEntry } from "../App";

type Log = (text: string, level?: LogEntry["level"]) => void;

// Windows 优先的默认共享目录；其他平台没有 C 盘概念，留空让用户自填或用
// 「浏览…」选择（后端启动前会校验目录存在性，空值会得到明确报错而非神秘失败）。
const IS_WINDOWS = navigator.userAgent.includes("Windows");
const DEFAULT_FTP_ROOT = IS_WINDOWS ? "C:\\ftp-root" : "";
const DEFAULT_TFTP_ROOT = IS_WINDOWS ? "C:\\tftp-root" : "";
const DEFAULT_SFTP_ROOT = IS_WINDOWS ? "C:\\sftp-root" : "";

// ---------- persisted per-server preferences ----------

interface ServerPrefs {
  root: string;
  iface: string;
  port: string;
  authMode: "anonymous" | "account";
  user: string;
  /** 被动数据端口段，界面写法是闭区间："50000-50099" */
  passive: string;
  /** 启用 FTPS（显式 TLS；明文客户端仍可连 —— 服务端是可选 TLS）。 */
  ftps: boolean;
  /** SFTP：授权公钥行（OpenSSH 格式）。公钥不是机密，随其它配置一起持久化。 */
  authorizedKeys: string[];
  /** SFTP：只读模式（拒绝写入/删除/重命名等修改操作）。 */
  readOnly: boolean;
}

function loadPrefs(key: string, fallback: ServerPrefs): ServerPrefs {
  try {
    const raw = localStorage.getItem(key);
    if (raw) return { ...fallback, ...JSON.parse(raw) };
  } catch {
    // corrupted storage -> fall back to defaults
  }
  return fallback;
}

/**
 * 解析界面里填的被动端口段，返回闭区间 [start, end]。
 *
 * 只做形状检查；真正的合法性（起止顺序、是否低于 1024）由后端在启动时判定，
 * 免得两边各写一套规则慢慢跑偏。
 */
function parsePassive(spec: string): [number, number] | null {
  const text = spec.trim();
  const closed = /^(\d{1,5})\s*-\s*(\d{1,5})$/.exec(text);
  if (closed) return [Number(closed[1]), Number(closed[2])];
  const halfOpen = /^(\d{1,5})\s*\.\.\s*(\d{1,5})$/.exec(text);
  if (halfOpen) return [Number(halfOpen[1]), Number(halfOpen[2]) - 1];
  if (/^\d{1,5}$/.test(text)) {
    const port = Number(text);
    return [port, port];
  }
  return null;
}

/** Dropdown label: friendly name first — never the internal adapter GUID. */
function interfaceLabel(it: NetInterface): string {
  const name = it.name || "未知网卡";
  const desc = it.desc.length > 36 ? `${it.desc.slice(0, 36)}…` : it.desc;
  const parts = [`${name}（${it.ip}）`];
  if (desc) parts.push(desc);
  if (it.loopback) parts.push("仅本机");
  return parts.join(" · ");
}

// 仅接受 OpenSSH 公钥行（设计文档 Q10）：算法名 + base64 主体 + 可选注释尾。
// authorized_keys 里常见的注释、空行与 # 注释由调用方先行过滤。
const KEY_LINE_RE = /^(ssh-ed25519|ssh-rsa|ecdsa-sha2-[a-z0-9-]+)\s+([A-Za-z0-9+/=]+)(\s+.*)?$/;

/**
 * 公钥行截断展示：算法 + 缩略主体 + 注释（title 悬浮可看全文）。
 * RSA 公钥主体可达数百字符，整行铺开会让卡片失去可读性。
 */
function formatKeyLine(line: string): string {
  const m = KEY_LINE_RE.exec(line);
  if (!m) return line.length > 64 ? `${line.slice(0, 64)}…` : line;
  const [, type, body, comment] = m;
  const short = body.length > 32 ? `${body.slice(0, 24)}…${body.slice(-8)}` : body;
  return comment ? `${type} ${short} ${comment.trim()}` : `${type} ${short}`;
}

// ---------- server card ----------

interface ServerCardProps {
  /** storage namespace, e.g. "ftp" -> localStorage key "ftp-toolbox:server:ftp" */
  serverKey: string;
  title: string;
  desc: string;
  defaultRoot: string;
  defaultPort: string;
  portHint: string;
  withAuth?: boolean;
  /** 是否暴露被动数据端口段（只有 FTP 需要）。 */
  withPassive?: boolean;
  defaultPassive?: string;
  /** 是否暴露 FTPS（显式 TLS）开关与证书折叠区（只有 FTP 需要）。 */
  withFtps?: boolean;
  /**
   * 是否为 SFTP 卡片：单用户名+密码、授权公钥、只读开关、主机密钥折叠区。
   * 与 FTP 的「可选匿名」不同，用户名密码始终显示（密码可留空走纯公钥认证）。
   */
  withSftp?: boolean;
  /** SFTP：当前主机密钥（来自状态快照/事件推送，后端 load-or-generate 持久化）。 */
  hostKey?: HostKeyInfo | null;
  interfaces: NetInterface[];
  /** 网卡列表是否已成功读到过 —— 与「列表是不是空的」是两件事。 */
  interfacesLoaded: boolean;
  /** Backend snapshot; `null` while the first status query is in flight. */
  status: ServerStatus | null;
  onStart: (
    root: string,
    addr: string,
    user?: string,
    pass?: string,
    passivePorts?: string,
    ftps?: boolean,
    /** withSftp 时的附加启动选项。 */
    sftp?: { authorizedKeys: string[]; readOnly: boolean }
  ) => Promise<string>;
  onStop: () => Promise<string>;
  /** Re-read server state (and interfaces) from the backend. */
  onRefresh: () => Promise<void>;
  log: Log;
}

function ServerCard({
  serverKey, title, desc, defaultRoot, defaultPort, portHint,
  withAuth, withPassive, defaultPassive = "", withFtps, withSftp, hostKey: hostKeyProp,
  interfaces, interfacesLoaded, status, onStart, onStop, onRefresh, log,
}: ServerCardProps) {
  const storageKey = `ftp-toolbox:server:${serverKey}`;
  const [prefs, setPrefs] = useState<ServerPrefs>(() =>
    loadPrefs(storageKey, {
      root: defaultRoot,
      iface: "0.0.0.0",
      port: defaultPort,
      authMode: "anonymous",
      user: "admin",
      passive: defaultPassive,
      ftps: false,
      authorizedKeys: [],
      readOnly: false,
    })
  );
  const [pass, setPass] = useState(""); // password intentionally NOT persisted
  const [busy, setBusy] = useState(false);
  const [check, setCheck] = useState<PassivePortCheck | null>(null);
  // FTPS 自签证书指纹。首次启用时后端会生成证书；这里挂载时读一次，
  // 「重新生成」后再读一次。读取失败只影响展示，不拦启动。
  const [cert, setCert] = useState<CertInfo | null>(null);
  // SFTP 主机密钥：状态推送里带过来（hostKeyProp），「重新生成」后用返回值
  // 立即刷新。prop 为 null 时不回退 —— 后端停止态快照可能不带密钥信息，
  // 而指纹要「一直可查」，保留最后已知值。
  const [hostKey, setHostKey] = useState<HostKeyInfo | null>(null);
  useEffect(() => {
    if (hostKeyProp) setHostKey(hostKeyProp);
  }, [hostKeyProp]);
  // 授权公钥文件选择器：原生 <input type="file"> 在 webview 内直接读内容，
  // 不需要后端命令或 fs 权限（dialog 插件只回路径，读不到内容）。
  const keyFileInput = useRef<HTMLInputElement>(null);

  // Run state is owned by the backend, never by this component: switching pages
  // unmounts the card, which used to reset a local `running` flag and made the
  // "启动服务" button reappear while the server was still up.
  const running = status?.running ?? false;
  const known = status !== null;

  // persist every change (except password and runtime state)
  useEffect(() => {
    localStorage.setItem(storageKey, JSON.stringify(prefs));
  }, [storageKey, prefs]);

  const set = <K extends keyof ServerPrefs>(k: K, v: ServerPrefs[K]) =>
    setPrefs((p) => ({ ...p, [k]: v }));

  // 指纹是给对端核对身份用的，必须一直可查，与 FTPS 开关状态无关 —— 折叠区
  // （FingerprintBlock）永远随卡片渲染，「关掉开关再看」不会让指纹消失，
  // 也就不会看起来像换了证书。
  useEffect(() => {
    if (!withFtps) return;
    let cancelled = false;
    api
      .ftpsCertInfo()
      .then((info) => {
        if (!cancelled) setCert(info);
      })
      .catch(() => {
        if (!cancelled) setCert(null);
      });
    return () => {
      cancelled = true;
    };
  }, [withFtps]);

  const regenerateCert = async () => {
    setBusy(true);
    try {
      const info = await api.ftpsRegenerateCert();
      setCert(info);
      log(`已重新生成 FTPS 证书，新指纹 ${info.fingerprint}`);
    } catch (e) {
      log(`重新生成证书失败: ${e}`, "error");
    } finally {
      setBusy(false);
    }
  };

  const regenerateHostKey = async () => {
    setBusy(true);
    try {
      const info = await api.sftpServerRegenerateHostKey();
      setHostKey(info);
      log(`已重新生成 SFTP 主机密钥，新指纹 ${info.fingerprint}`);
    } catch (e) {
      log(`重新生成主机密钥失败: ${e}`, "error");
    } finally {
      setBusy(false);
    }
  };

  /**
   * 载入选中的公钥文件（可多选 .pub / authorized_keys）。每文件按行解析，
   * 只收 OpenSSH 格式公钥行（KEY_LINE_RE），空行与 # 注释跳过；已在列表里的
   * 不重复添加。解析结果写日志，跳过多少行一目了然。
   */
  const addKeyFiles = async (files: FileList | null) => {
    if (!files || files.length === 0) return;
    const picked: string[] = [];
    let skipped = 0;
    for (const f of Array.from(files)) {
      const text = await f.text();
      for (const raw of text.split(/\r?\n/)) {
        const line = raw.trim();
        if (!line || line.startsWith("#")) continue;
        if (KEY_LINE_RE.test(line)) picked.push(line);
        else skipped++;
      }
    }
    const existing = new Set(prefs.authorizedKeys);
    const fresh = picked.filter((k) => !existing.has(k));
    if (fresh.length > 0) {
      setPrefs((p) => ({ ...p, authorizedKeys: [...p.authorizedKeys, ...fresh] }));
    }
    const parts = [`新增 ${fresh.length} 条`];
    const dup = picked.length - fresh.length;
    if (dup > 0) parts.push(`重复 ${dup} 条`);
    if (skipped > 0) parts.push(`跳过非 OpenSSH 格式 ${skipped} 行`);
    log(`载入授权公钥：${parts.join("、")}`);
  };

  const removeKey = (index: number) =>
    setPrefs((p) => ({
      ...p,
      authorizedKeys: p.authorizedKeys.filter((_, i) => i !== index),
    }));

  // Windows keeps blocks of TCP ports for itself (Hyper-V/WSL2 reserve
  // 50000-50059 on this machine, right inside the old default range). libunftp
  // picks a random port from the passive band and retries a few times, so a
  // partial overlap means PASV fails roughly 1% of the time — invisible until
  // a colleague reports "列表偶尔失败". Checking up front turns that into a
  // visible warning with a one-click fix.
  useEffect(() => {
    if (!withPassive) return;
    const parsed = parsePassive(prefs.passive);
    if (!parsed) {
      setCheck(null);
      return;
    }
    let cancelled = false;
    // debounce: the user is probably still typing
    const timer = setTimeout(() => {
      api
        .checkPassivePorts(parsed[0], parsed[1])
        .then((result) => {
          if (!cancelled) setCheck(result);
        })
        .catch(() => {
          if (!cancelled) setCheck(null);
        });
    }, 250);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [withPassive, prefs.passive]);

  // A saved interface IP can disappear (DHCP change, cable unplugged, adapter
  // disabled). Two things this must *not* do:
  //
  //  1. rewrite `prefs.iface` — while the server is running it is still bound
  //     to that very address, so rewriting would make the UI lie about what is
  //     being listened on;
  //  2. persist the rewrite — the user's preference should survive a cable
  //     being out for a minute, and must not be overwritten by whatever the
  //     network happens to look like this second.
  //
  // So the stale entry stays in the dropdown (marked 已掉线) and we warn, but
  // the *user* decides when to switch. This also matters more than it used to:
  // the list is now refetched on a timer, so an auto-rewrite would fire on its
  // own the moment the cable came out.
  // Two separate questions that used to be one:
  //
  //   1. "Can the <select> show the value we will actually use?" — unconditional.
  //      If no <option> matches `prefs.iface`, the browser displays the *first*
  //      option instead, so the UI would read "所有接口 (0.0.0.0)" while
  //      `start()` still sends `prefs.iface`. Display must never disagree with
  //      what is used.
  //   2. "Can we claim it is offline?" — only once we have actually read the
  //      list; an enumeration failure also yields an empty list, and calling
  //      that "已掉线" would be a guess.
  //
  // So: render the option per (1), but only add the 已掉线 label / warning /
  // disabled start button per (2).
  const ifaceUnlisted =
    prefs.iface !== "0.0.0.0" && !interfaces.some((it) => it.ip === prefs.iface);
  const ifaceMissing = interfacesLoaded && ifaceUnlisted;

  // Warn on the *edge* only: `interfaces` is refetched every few seconds, and
  // logging on every poll would bury everything else.
  const warnedFor = useRef<string | null>(null);
  useEffect(() => {
    if (!ifaceMissing) {
      warnedFor.current = null;
      return;
    }
    if (warnedFor.current === prefs.iface) return;
    warnedFor.current = prefs.iface;
    log(
      running
        ? `监听接口 ${prefs.iface} 已掉线（拔线或网卡禁用）：服务器仍绑在该地址上，插回网线即可恢复；若要换地址请先停止服务`
        : `监听接口 ${prefs.iface} 已不在网卡列表中（拔线或 IP 变化），启动前请重新选择`,
      "error"
    );
  }, [ifaceMissing, prefs.iface, running, log]);

  const browse = async () => {
    try {
      const dir = await pickFolder(prefs.root);
      if (dir) set("root", dir);
    } catch (e) {
      log(`打开文件夹选择框失败: ${e}`, "error");
    }
  };

  const start = async () => {
    setBusy(true);
    try {
      const useAccount = withAuth && prefs.authMode === "account";
      const addr = `${prefs.iface}:${prefs.port}`;
      const msg = await onStart(
        prefs.root,
        addr,
        // SFTP 无匿名模式：用户名密码始终上送（密码可留空走纯公钥认证）
        withSftp ? prefs.user : useAccount ? prefs.user : undefined,
        withSftp ? pass : useAccount ? pass : undefined,
        withPassive ? prefs.passive : undefined,
        withFtps ? prefs.ftps : undefined,
        withSftp ? { authorizedKeys: prefs.authorizedKeys, readOnly: prefs.readOnly } : undefined
      );
      log(msg, "ok");
    } catch (e) {
      // 端口占用、地址失效等都会走到这里（后端在绑定完成前不返回成功）
      log(`${title}启动失败: ${e}`, "error");
    } finally {
      await onRefresh(); // 无论成败都以后端为准
      setBusy(false);
    }
  };

  const stop = async () => {
    setBusy(true);
    try {
      log(await onStop());
    } catch (e) {
      log(`${title}停止失败: ${e}`, "error");
    } finally {
      await onRefresh();
      setBusy(false);
    }
  };

  const refresh = async () => {
    setBusy(true);
    try {
      await onRefresh();
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="card">
      <div className="card-head">
        <div>
          <div className="card-title">{title}</div>
          <div className="card-desc">{desc}</div>
          {running && status?.localAddr && (
            <div className="card-desc">
              已监听 {status.localAddr}
              {status.sessions > 0 ? ` · ${status.sessions} 个活动连接` : ""}
            </div>
          )}
        </div>
        <div className="card-head-right">
          <span className={`status-chip${running ? " running" : ""}`}>
            <span className="dot" />
            {running
              ? status && status.sessions > 0
                ? `运行中 · ${status.sessions}`
                : "运行中"
              : known
                ? "已停止"
                : "读取中…"}
          </span>
          <button className="btn small" onClick={refresh} disabled={busy}>
            刷新
          </button>
        </div>
      </div>

      <label className="field">
        <span>共享目录</span>
        <div className="input-row">
          <input
            className="grow"
            value={prefs.root}
            onChange={(e) => set("root", e.target.value)}
            placeholder={defaultRoot ? undefined : "选择或输入共享目录，如 /srv/ftp"}
            disabled={running}
          />
          <button className="btn small" onClick={browse} disabled={running}>
            浏览…
          </button>
        </div>
      </label>

      <div className="row">
        <label className="field grow">
          <span>监听接口</span>
          <select value={prefs.iface} onChange={(e) => set("iface", e.target.value)} disabled={running}>
            <option value="0.0.0.0">所有接口 (0.0.0.0)</option>
            {/* 选中的地址不在列表里时必须把它补回 <option>，否则 <select> 会渲染成
                第一个选项（「所有接口」），界面显示的值与 start() 实际使用的值不一致。
                这一条与「列表是否读取成功」无关，所以要按 ifaceUnlisted 渲染。
                至于要不要打「已掉线」这个标签，得先确定列表真的读到了 —— 读失败
                同样是空列表，那时候只能说「不确定」，不能替用户下结论。 */}
            {ifaceUnlisted && (
              <option value={prefs.iface}>
                {ifaceMissing ? `${prefs.iface}（已掉线）` : prefs.iface}
              </option>
            )}
            {interfaces.map((it) => (
              <option key={`${it.name}-${it.ip}`} value={it.ip}>
                {interfaceLabel(it)}
              </option>
            ))}
          </select>
        </label>
        <label className="field narrow">
          <span>端口（{portHint}）</span>
          <input
            type="number"
            min={1}
            max={65535}
            value={prefs.port}
            onChange={(e) => set("port", e.target.value)}
            disabled={running}
          />
        </label>
      </div>

      {ifaceMissing && (
        <div className="hint-line warn with-action">
          <span>
            ⚠ 监听接口 {prefs.iface} 已掉线（网线拔出或网卡已禁用）。
            {running
              ? "服务器仍绑在该地址上，插回网线即可恢复；要换地址请先停止服务。"
              : "可以先改用「所有接口」，或等网线插回后再启动。"}
          </span>
          {!running && (
            <button className="btn small" onClick={() => set("iface", "0.0.0.0")}>
              改用所有接口
            </button>
          )}
        </div>
      )}

      {withPassive && (
        <>
          <label className="field">
            <span>被动数据端口段（PASV/EPSV 用，起止用 - 连接）</span>
            <div className="input-row">
              <input
                className="grow"
                value={prefs.passive}
                onChange={(e) => set("passive", e.target.value)}
                disabled={running}
                placeholder={defaultPassive}
              />
              {check && !check.ok && check.suggested && (
                <button
                  className="btn small"
                  onClick={() => set("passive", check.suggested ?? "")}
                  disabled={running}
                >
                  改用 {check.suggested}
                </button>
              )}
            </div>
          </label>
          {check && !check.ok && (
            <div className="hint-line warn">
              ⚠ 与系统保留段 {check.conflicts.join("、")} 重叠：落在其中的端口无法用于
              PASV，列表/传输会间歇性失败（控制连接仍是正常的）
            </div>
          )}
          {/* netsh 里带 * 的「托管排除段」是另一回事：实测它不阻止绑定到具体
              地址，而 PASV 正是绑具体地址，所以这类重叠通常无害，不值得报警。
              真冲突和托管重叠同时出现时，措辞要区分开，否则用户会以为有两件事
              要修 —— 托管那条得读起来像补充说明，而不是第二条告警。
              这个块踩过两次坑，都跟 JSX 的空白处理有关，改的时候注意：
              1) 别让两支共用一段中段去拼接 —— 上一版两支共用以「内」结尾的中段，
                 ok 那支就拼成「…还与…排除段内」，读不通。宁可重复整句。
              2) `{join()}` 后面的字必须和它**同一行**。JSX 会把跨行的文本折叠成一个
                 空格，但 HTML 标签后面紧跟的换行（即 JSXText 的首行为空）是被直接
                 剥掉的，不补空格 —— 分开写就会渲染成「50000-50059重叠」。
                 同页 warn 行的 `{check.conflicts.join("、")} 重叠：` 是正确写法。 */}
          {check && check.managedConflicts.length > 0 && (
            <div className="hint-line note">
              {check.ok ? (
                <>
                  提示：该段还与系统托管排除段 {check.managedConflicts.join("、")} 重叠（Hyper-V / WSL2
                  常用）。实测这类段仍可绑定，通常不影响 PASV；若列表/传输偶发失败，再换段即可。
                </>
              ) : (
                <>
                  另外，该段也落在系统托管排除段 {check.managedConflicts.join("、")} 内（Hyper-V / WSL2
                  常用）。实测这类段仍可绑定，通常不影响 PASV，不必为它单独换段。
                </>
              )}
            </div>
          )}
        </>
      )}

      {withFtps && (
        <>
          <label className="field">
            <span>加密（FTPS）</span>
            <div className="radio-row">
              <label className="radio">
                <input
                  type="checkbox"
                  checked={prefs.ftps}
                  onChange={(e) => set("ftps", e.target.checked)}
                  disabled={running}
                />
                启用 FTPS（显式 TLS：支持加密的客户端用 AUTH TLS 升级，明文客户端不受影响）
              </label>
            </div>
          </label>
          {/* 证书折叠区（Q12 改造）：默认收起、永远随卡片渲染，与开关无关 ——
              对端核对服务器身份随时可查；指纹长串不再常驻卡片。 */}
          <FingerprintBlock
            label="证书详情"
            fingerprintLabel="证书指纹（SHA-256）"
            fingerprint={cert?.fingerprint ?? null}
            onRegenerate={running ? undefined : regenerateCert}
            busy={busy}
            emptyHint="证书尚未生成，首次启用 FTPS 时自动创建"
          />
        </>
      )}

      {withSftp && (
        <>
          <div className="row">
            <label className="field grow">
              <span>用户名</span>
              <input value={prefs.user} onChange={(e) => set("user", e.target.value)} disabled={running} />
            </label>
            <label className="field grow">
              <span>密码（不保存，可留空走公钥认证）</span>
              <input
                type="password"
                value={pass}
                onChange={(e) => setPass(e.target.value)}
                disabled={running}
              />
            </label>
          </div>
          <div className="field">
            <span>授权公钥（可选，OpenSSH 格式；与密码任一通过即可登录）</span>
            <div className="input-row">
              <button
                className="btn small"
                onClick={() => keyFileInput.current?.click()}
                disabled={running}
              >
                添加公钥文件…
              </button>
              <span className="hint-inline">
                {prefs.authorizedKeys.length > 0 ? `共 ${prefs.authorizedKeys.length} 条` : "未配置"}
              </span>
            </div>
            {/* 原生文件选择器只在这里用：读内容必须留在 webview 里（dialog
                插件只回路径），hidden + 按钮触发保持界面统一 */}
            <input
              ref={keyFileInput}
              type="file"
              multiple
              hidden
              onChange={(e) => {
                void addKeyFiles(e.target.files);
                // 清空 value，否则同一文件第二次选择不触发 change
                e.target.value = "";
              }}
            />
            {prefs.authorizedKeys.length > 0 && (
              <ul className="key-list">
                {prefs.authorizedKeys.map((k, i) => (
                  <li key={`${i}-${k.slice(0, 32)}`} className="key-item">
                    <span className="key-line" title={k}>
                      {formatKeyLine(k)}
                    </span>
                    <button className="btn small" onClick={() => removeKey(i)} disabled={running}>
                      移除
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </div>
          <label className="field">
            <span>访问模式</span>
            <div className="radio-row">
              <label className="radio">
                <input
                  type="checkbox"
                  checked={prefs.readOnly}
                  onChange={(e) => set("readOnly", e.target.checked)}
                  disabled={running}
                />
                只读模式（拒绝写入/删除/重命名等修改操作）
              </label>
            </div>
          </label>
          {/* 主机密钥折叠区：默认收起、永远随卡片渲染 —— 对端核对身份随时可查，
              指纹只在折叠区与 tracing 日志出现（启动成功消息不带指纹，Q12）。 */}
          <FingerprintBlock
            label="主机密钥详情"
            fingerprintLabel="主机密钥指纹（SHA-256）"
            fingerprint={hostKey?.fingerprint ?? null}
            onRegenerate={running ? undefined : regenerateHostKey}
            busy={busy}
            emptyHint="主机密钥尚未生成，启动服务时自动创建"
          />
        </>
      )}

      {withAuth && (
        <>
          <label className="field">
            <span>认证方式</span>
            <div className="radio-row">
              <label className="radio">
                <input
                  type="radio"
                  checked={prefs.authMode === "anonymous"}
                  onChange={() => set("authMode", "anonymous")}
                  disabled={running}
                />
                匿名访问
              </label>
              <label className="radio">
                <input
                  type="radio"
                  checked={prefs.authMode === "account"}
                  onChange={() => set("authMode", "account")}
                  disabled={running}
                />
                账号密码
              </label>
            </div>
          </label>
          {prefs.authMode === "account" && (
            <div className="row">
              <label className="field grow">
                <span>用户名</span>
                <input value={prefs.user} onChange={(e) => set("user", e.target.value)} disabled={running} />
              </label>
              <label className="field grow">
                <span>密码（不保存）</span>
                <input
                  type="password"
                  value={pass}
                  onChange={(e) => setPass(e.target.value)}
                  disabled={running}
                />
              </label>
            </div>
          )}
        </>
      )}

      <div className="actions">
        {running ? (
          <button className="btn danger" onClick={stop} disabled={busy}>
            停止服务
          </button>
        ) : (
          // 选中的接口已掉线时不允许启动：否则用户只会拿到一条
          // EADDRNOTAVAIL 绑定失败，而这本来是可以提前避免的。
          <button
            className="btn primary"
            onClick={start}
            disabled={busy || !known || ifaceMissing}
            title={ifaceMissing ? `监听接口 ${prefs.iface} 已掉线，请先改用其它接口` : undefined}
          >
            启动服务
          </button>
        )}
      </div>
    </div>
  );
}

// ---------- view ----------

interface ServersViewProps {
  log: Log;
  ftpStatus: ServerStatus | null;
  tftpStatus: ServerStatus | null;
  sftpStatus: SftpServerStatus | null;
  refresh: () => Promise<void>;
}

/**
 * 网卡列表的轮询周期。
 *
 * 枚举一次实测 p50≈4ms、尖峰 30-80ms，3s 一次的成本可以忽略；换来的是拔网线
 * 后最多 3s 下拉就更新 —— 对配置类控件而言足够「实时」，而且不依赖任何平台
 * 专有通知机制（Windows 的 NotifyIpInterfaceChange 只覆盖一个平台）。
 * 窗口不可见时不轮询；页面切走时组件卸载，effect 清理会直接停掉定时器。
 */
const INTERFACE_POLL_MS = 3000;

export default function ServersView({ log, ftpStatus, tftpStatus, sftpStatus, refresh }: ServersViewProps) {
  const [interfaces, setInterfaces] = useState<NetInterface[]>([]);
  // 「列表是否已经成功读到过」与「列表是不是空的」是两件事：枚举失败时
  // `interfaces` 会是空数组，但那时说用户选的接口「已掉线」是错的。所以只有
  // 成功读回来才敢下判断；一直没读到就维持原始的空下拉状态。
  const [interfacesLoaded, setInterfacesLoaded] = useState(false);
  // 只在列表真的变了才 setState：否则每次轮询都会重渲染（下拉会闪），
  // 依赖 interfaces 的 effect 也会被无谓地重新触发。
  // 后端已按 IP 稳定排序、字段顺序也固定，所以这份快照可以逐字比较。
  const lastSnapshot = useRef("");
  const pollFailed = useRef(false);

  const loadInterfaces = useCallback(async () => {
    try {
      const next = await api.listInterfaces();
      pollFailed.current = false;
      setInterfacesLoaded(true);
      const snapshot = JSON.stringify(next);
      if (snapshot !== lastSnapshot.current) {
        lastSnapshot.current = snapshot;
        setInterfaces(next);
      }
    } catch (e) {
      // 轮询失败只报一次，别把日志刷屏
      if (!pollFailed.current) {
        pollFailed.current = true;
        log(`读取网卡列表失败: ${e}`, "error");
      }
    }
  }, [log]);

  // 网卡列表要跟着网线插拔走：挂载时拉一次，之后定时轮询，并在窗口重新获得
  // 焦点 / 重新可见时立刻补拉，不必等下一个周期。
  useEffect(() => {
    void loadInterfaces();
    const timer = setInterval(() => {
      if (document.visibilityState === "hidden") return;
      void loadInterfaces();
    }, INTERFACE_POLL_MS);
    const onWake = () => {
      if (document.visibilityState === "visible") void loadInterfaces();
    };
    window.addEventListener("focus", onWake);
    document.addEventListener("visibilitychange", onWake);
    return () => {
      clearInterval(timer);
      window.removeEventListener("focus", onWake);
      document.removeEventListener("visibilitychange", onWake);
    };
  }, [loadInterfaces]);

  // 刷新按钮同时更新运行态和网卡列表（网卡 IP 会随网络环境变化）
  const refreshAll = async () => {
    await Promise.all([refresh(), loadInterfaces()]);
  };

  return (
    <div className="grid-2">
      <ServerCard
        serverKey="ftp"
        title="FTP 服务器"
        desc="支持匿名或账号密码认证"
        defaultRoot={DEFAULT_FTP_ROOT}
        defaultPort="21"
        portHint="默认 21"
        withAuth
        withPassive
        withFtps
        // 与 ftp-core 的 DEFAULT_PASSIVE_PORTS (50000..50100) 保持一致
        defaultPassive="50000-50099"
        interfaces={interfaces}
        interfacesLoaded={interfacesLoaded}
        status={ftpStatus}
        onStart={api.startFtpServer}
        onStop={api.stopFtpServer}
        onRefresh={refreshAll}
        log={log}
      />
      <ServerCard
        serverKey="tftp"
        title="TFTP 服务器"
        desc="支持 blksize 协商（单传可达 500+ MB）"
        defaultRoot={DEFAULT_TFTP_ROOT}
        defaultPort="69"
        portHint="默认 69"
        interfaces={interfaces}
        interfacesLoaded={interfacesLoaded}
        status={tftpStatus}
        onStart={(root, addr) => api.startTftpServer(root, addr)}
        onStop={api.stopTftpServer}
        onRefresh={refreshAll}
        log={log}
      />
      <ServerCard
        serverKey="sftp"
        title="SFTP 服务器"
        desc="SSH 文件传输；密码或公钥任一通过即可登录"
        defaultRoot={DEFAULT_SFTP_ROOT}
        defaultPort="2222"
        portHint="默认 2222"
        withSftp
        interfaces={interfaces}
        interfacesLoaded={interfacesLoaded}
        status={sftpStatus}
        hostKey={sftpStatus?.hostKey ?? null}
        onStart={(root, addr, user, pass, _passive, _ftps, sftp) => {
          // ServerCard 统一用 "iface:port" 传地址，SFTP 命令要分开的
          // bindAddr/port —— 这里拆开（下拉里只有 IPv4，lastIndexOf 够用）
          const i = addr.lastIndexOf(":");
          const bindAddr = i < 0 ? addr : addr.slice(0, i);
          const port = i < 0 ? 2222 : Number(addr.slice(i + 1)) || 2222;
          return api
            .startSftpServer({
              bindAddr,
              port,
              username: user ?? "",
              password: pass ?? "",
              authorizedKeys: sftp?.authorizedKeys ?? [],
              rootDir: root,
              readOnly: sftp?.readOnly ?? false,
            })
            // 启动成功消息按契约不带指纹（Q12）；addr 若已含端口则不再重复拼
            .then((info) => {
              const target = info.addr.includes(":") ? info.addr : `${info.addr}:${info.port}`;
              return `SFTP 服务器已启动：sftp://${target}`;
            });
        }}
        onStop={api.stopSftpServer}
        onRefresh={refreshAll}
        log={log}
      />
    </div>
  );
}
