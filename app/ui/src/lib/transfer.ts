/**
 * 传输进度/日志文案的纯逻辑层。
 *
 * 这里刻意不 import `api.ts`（那会牵进 `@tauri-apps/api`，无法在 node 下加载），
 * 只依赖同样纯净的 `./format.ts`。注意：下面的 import 带 `.ts` 后缀是**必需的**
 * —— Node 的 ESM 解析器不做扩展名补全，`node --test` 直接加载本文件时写
 * `./format` 会报 ERR_MODULE_NOT_FOUND。Vite 打包不受影响。
 */
import { fmtBytes } from "./format.ts";

/**
 * 后端 `TransferKind` 枚举序列化后的字符串（progress 事件复用，SFTP 变体见
 * 设计文档 §3）。后端枚举当前是 `rename_all = "lowercase"`，故 SftpUpload/
 * SftpDownload 序列化为 "sftpupload"/"sftpdownload"；若后端改用 camelCase 则
 * 是 "sftpUpload"/"sftpDownload" —— 两种拼写都收录，展示统一走 transferLabel。
 */
export type TransferKind =
  | "upload"
  | "download"
  | "sftpupload"
  | "sftpdownload"
  | "sftpUpload"
  | "sftpDownload";

/** 后端推来的 `TransferEvent`（"transfer-progress" 事件载荷）。 */
export interface TransferEvent {
  phase: "started" | "progress" | "done" | "error";
  kind: TransferKind;
  file: string;
  bytes?: number;
  total?: number | null;
  message?: string;
}

/** 传输方向 → 中文标签（日志 / 进度条用）。 */
export function transferLabel(kind: string): "上传" | "下载" {
  return kind.toLowerCase().endsWith("download") ? "下载" : "上传";
}

/** 页脚进度条的当前状态；由 `newSample` 建立、`advanceSample` 推进。 */
export interface Progress {
  file: string;
  kind: TransferKind;
  bytes: number;
  total: number | null;
  /** 起始时间（ms epoch）—— 已用时长与平均速度的基准。 */
  startedAt: number;
  /**
   * 当前测速窗口的起点：在 `speedAt` 时刻已传输 `speedBytes` 字节。
   *
   * 注意这两个字段**只在窗口攒够时**才前移（见 `MIN_SPEED_WINDOW_MS`），
   * 不是"上一次收到的事件"。若改成每条事件都刷新，就会退回那个把
   * 24 MB/s 显示成 7.8 MB/s 的老 bug。
   */
  speedBytes: number;
  speedAt: number;
  /** 吞吐（字节/秒），经 EMA 平滑（见 `advanceSample`）。 */
  speed: number;
}

/**
 * 测速窗口的下限，短于这个间隔一律不算速率。
 *
 * 回环上传时 8192 字节的块间隔只有约 0.3 ms，而 `Date.now()` 的分辨率是 1 ms
 * ——绝大多数事件量出的 `now - speedAt` 就是 0，被"至少 1 ms"夹住之后
 * `8192 / 0.001 = 8.19 MB/s`，EMA 再把这个假值锁死：实测 24.3 MB/s 的传输
 * 稳定显示成 7.8 MB/s（正好 = 8192 B/ms）。
 *
 * 攒够窗口再算就没有这个问题：期间只推进字节数、不动窗口起点，于是下一次的
 * 差值天然覆盖整个窗口，时钟分辨率带来的误差被稀释到 1/200 以下。
 */
export const MIN_SPEED_WINDOW_MS = 200;

/**
 * "（7.9 MB）"——started 事件用的大小后缀。
 *
 * 大小确实未知时（TFTP 下载没有 `tsize` 协商、FTP 服务器不支持 `SIZE`）
 * 明说一句，比让用户干瞪着转圈好。
 */
export function sizeSuffix(total: number | null): string {
  return total && total > 0 ? `（${fmtBytes(total)}）` : "（大小未知）";
}

/**
 * "7.9 MB · 2.4s · 平均 3.3 MB/s"——done 事件用的后缀。
 *
 * 已用时长与平均速度在 100 ms 以下一律省略：4 KB 文件走回环会算出
 * "0.0s · 平均 51 MB/s"，那是噪声假装成精度。
 */
export function doneSuffix(bytes: number, elapsed: number | null): string {
  const parts = [fmtBytes(bytes)];
  if (elapsed !== null && elapsed >= 0.1) {
    parts.push(`${elapsed.toFixed(1)}s`);
    parts.push(`平均 ${fmtBytes(bytes / elapsed)}/s`);
  }
  return parts.join(" · ");
}

/**
 * 两条进度是否属于同一条流。文件与方向都对上才算，缺一个就重开样本 ——
 * 这挡住了回环场景：两条流共用页脚那一个槽位，若把新文件的字节数折进
 * 旧文件的速率，会算出一个荒唐的速度尖峰。
 */
export function isSameStream(prev: Progress | null, ev: TransferEvent): boolean {
  return prev !== null && prev.file === ev.file && prev.kind === ev.kind;
}

/** 为一个 started / 首次出现的事件建立新样本。 */
export function newSample(ev: TransferEvent, now: number): Progress {
  return {
    file: ev.file,
    kind: ev.kind,
    bytes: 0,
    total: ev.total ?? null,
    startedAt: now,
    speedBytes: 0,
    speedAt: now,
    speed: 0,
  };
}

/**
 * 用一条 progress 事件推进样本，返回新样本（不改动入参）。
 *
 * 窗口内的平均速率再叠一层 EMA（0.7/0.3）：单个卡住的块不至于把读数打到 0，
 * 首个窗口（`base.speed === 0`）直接取该窗口的平均值，不从 0 慢慢爬。
 *
 * 窗口没攒够就只推进 `bytes`（进度条照常走），速度与窗口起点都不动 ——
 * 理由见 `MIN_SPEED_WINDOW_MS`。
 */
export function advanceSample(base: Progress, ev: TransferEvent, now: number): Progress {
  const bytes = ev.bytes ?? 0;
  const advanced: Progress = { ...base, bytes, total: ev.total ?? base.total };
  const span = now - base.speedAt;
  if (span < MIN_SPEED_WINDOW_MS) return advanced;
  // 字节数回退（服务端重发更旧的累计值）时按 0 计，不能出现负速率。
  const instant = Math.max(0, bytes - base.speedBytes) / (span / 1000);
  const speed = base.speed > 0 ? base.speed * 0.7 + instant * 0.3 : instant;
  return { ...advanced, speed, speedBytes: bytes, speedAt: now };
}

/** 已用秒数；没有 started 样本（只有 done）时为 null。 */
export function elapsedSeconds(prev: Progress | null, now: number): number | null {
  return prev ? (now - prev.startedAt) / 1000 : null;
}
