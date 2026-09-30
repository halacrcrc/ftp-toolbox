import { test } from "node:test";
import assert from "node:assert/strict";
import {
  advanceSample,
  doneSuffix,
  elapsedSeconds,
  isSameStream,
  MIN_SPEED_WINDOW_MS,
  newSample,
  sizeSuffix,
  transferLabel,
} from "./transfer.ts";
import type { Progress, TransferEvent } from "./transfer.ts";

function base(over: Partial<Progress> = {}): Progress {
  return {
    file: "a.txt",
    kind: "upload",
    bytes: 0,
    total: 1000,
    startedAt: 0,
    speedBytes: 0,
    speedAt: 0,
    speed: 0,
    ...over,
  };
}

function ev(over: Partial<TransferEvent> = {}): TransferEvent {
  return { phase: "progress", kind: "upload", file: "a.txt", bytes: 0, total: null, ...over };
}

// ---------- 文案 ----------

test("transferLabel: 按方向而不是按协议分词", () => {
  assert.equal(transferLabel("upload"), "上传");
  assert.equal(transferLabel("download"), "下载");
  assert.equal(transferLabel("sftpupload"), "上传");
  assert.equal(transferLabel("sftpdownload"), "下载");
  // 后端若把枚举改成 camelCase，两种拼写都要认。
  assert.equal(transferLabel("sftpUpload"), "上传");
  assert.equal(transferLabel("sftpDownload"), "下载");
});

test("sizeSuffix: 大小未知时明说，而不是留个转圈", () => {
  // TFTP 下载没有 tsize 协商、FTP 服务器可能不支持 SIZE —— 这两种都真的没有大小。
  assert.equal(sizeSuffix(null), "（大小未知）");
  assert.equal(sizeSuffix(0), "（大小未知）");
  assert.equal(sizeSuffix(8283759), "（7.9 MB）");
});

test("doneSuffix: 短于 100 ms 的不报时长和平均速度", () => {
  // 4 KB 文件走回环会算出"0.0s · 平均 51 MB/s"，那是噪声假装成精度。
  assert.equal(doneSuffix(1024, null), "1.0 KB");
  assert.equal(doneSuffix(1024, 0.05), "1.0 KB");
});

test("doneSuffix: 够长就带上时长与平均速度", () => {
  assert.equal(doneSuffix(1024, 0.1), "1.0 KB · 0.1s · 平均 10.0 KB/s");
  assert.equal(doneSuffix(2048, 1), "2.0 KB · 1.0s · 平均 2.0 KB/s");
});

// ---------- 样本推进 ----------

test("newSample: started 事件建立零起点样本", () => {
  const s = newSample(ev({ phase: "started", file: "b.bin", kind: "download", total: 4096 }), 1000);
  assert.deepEqual(s, {
    file: "b.bin",
    kind: "download",
    bytes: 0,
    total: 4096,
    startedAt: 1000,
    speedBytes: 0,
    speedAt: 1000,
    speed: 0,
  });
});

test("newSample: 没有 total 时记为 null（大小未知）", () => {
  const s = newSample(ev({ total: undefined }), 0);
  assert.equal(s.total, null);
});

test("isSameStream: 文件与方向都对上才算同一条流", () => {
  assert.equal(isSameStream(null, ev()), false);
  assert.equal(isSameStream(base(), ev()), true);
  assert.equal(isSameStream(base(), ev({ file: "b.txt" })), false);
  // 回环场景两条流共用页脚那一个槽位，同文件不同方向也不能合并。
  assert.equal(isSameStream(base(), ev({ kind: "download" })), false);
});

test("advanceSample: 首个窗口直接取平均速率，不从 0 慢慢爬", () => {
  const s = advanceSample(base({ speedAt: 0 }), ev({ bytes: 500_000 }), 1000);
  assert.equal(s.speed, 500_000);
  assert.equal(s.bytes, 500_000);
});

test("advanceSample: 匀速传输上报的速度保持平稳", () => {
  const first = advanceSample(base({ speedAt: 0 }), ev({ bytes: 500_000 }), 1000);
  const second = advanceSample(first, ev({ bytes: 1_000_000 }), 2000);
  assert.equal(second.speed, 500_000);
});

test("advanceSample: 单独一个卡住的块不会把速度打到 0", () => {
  // EMA 0.7/0.3：一次 0 只衰减到 70%，而不是瞬间归零。
  const running = base({ speedBytes: 1_000_000, speedAt: 2000, speed: 500_000, bytes: 1_000_000 });
  const stalled = advanceSample(running, ev({ bytes: 1_000_000 }), 3000);
  assert.equal(stalled.speed, 350_000);
});

test("advanceSample: 窗口没攒够就不算速率，但字节数照常推进", () => {
  // 进度条读 bytes，所以哪怕不算速率，字节数也必须更新。
  const s = advanceSample(base({ speedAt: 0 }), ev({ bytes: 1000 }), MIN_SPEED_WINDOW_MS - 1);
  assert.equal(s.bytes, 1000);
  assert.equal(s.speed, 0);
  assert.equal(s.speedAt, 0);
  assert.equal(s.speedBytes, 0);
});

test("advanceSample: 同一毫秒内连来的事件不会算出虚高速率", () => {
  // 回归：`8192 字节 ÷ 被夹到 1 ms 的间隔 = 8.19 MB/s` 就是这么来的。
  const s = advanceSample(base({ speedAt: 1000 }), ev({ bytes: 1000 }), 1000);
  assert.ok(Number.isFinite(s.speed));
  assert.equal(s.speed, 0);
});

test("advanceSample: 速率按整个窗口的差值算，中途不刷新窗口起点", () => {
  // 若实现把 speedBytes/speedAt 每条事件都刷新，这里会算出 400 而不是 1200。
  let s = newSample(ev({ phase: "started" }), 0);
  s = advanceSample(s, ev({ bytes: 100 }), 10);
  s = advanceSample(s, ev({ bytes: 200 }), 20);
  s = advanceSample(s, ev({ bytes: 300 }), 250);
  assert.equal(s.speed, 1200);
});

test("advanceSample: 回环上块间隔远小于时钟分辨率时不再系统性低估", () => {
  // 真实场景：8192 字节的块、约 0.3 ms 一块（≈24.6 MB/s）。Date.now() 只能
  // 分辨到 1 ms，所以同一毫秒里会连着来好几条事件。
  let s = newSample(ev({ phase: "started" }), 0);
  let bytes = 0;
  for (let ms = 0; ms <= 600; ms++) {
    for (let k = 0; k < 3; k++) {
      bytes += 8192;
      s = advanceSample(s, ev({ bytes }), ms);
    }
  }
  const trueRate = bytes / 0.6; // 24_576_000 B/s
  const err = Math.abs(s.speed - trueRate) / trueRate;
  assert.ok(err < 0.01, `速度应贴近真实速率 ${trueRate}，实际 ${s.speed}`);
});

test("advanceSample: 字节数回退时速度取 0，不会出现负数", () => {
  // 服务端重发一个更旧的累计值时不该让速率变负。
  const s = advanceSample(
    base({ speedBytes: 500_000, speedAt: 1000, speed: 100 }),
    ev({ bytes: 400_000 }),
    2000
  );
  assert.equal(s.speed, 70);
});

test("advanceSample: 事件没带 total 时沿用上一个样本的大小", () => {
  assert.equal(advanceSample(base({ total: 999 }), ev({ total: null }), 100).total, 999);
  assert.equal(advanceSample(base({ total: 999 }), ev({ total: 2048 }), 100).total, 2048);
});

test("advanceSample: 不改动传入的样本", () => {
  const b = base({ speedBytes: 100, speedAt: 1000, speed: 400, bytes: 100 });
  advanceSample(b, ev({ bytes: 900 }), 2000);
  assert.equal(b.speed, 400);
  assert.equal(b.bytes, 100);
  assert.equal(b.speedBytes, 100);
});

test("elapsedSeconds: 没有 started 样本时返回 null", () => {
  assert.equal(elapsedSeconds(null, 5000), null);
  assert.equal(elapsedSeconds(base({ startedAt: 1000 }), 3500), 2.5);
});
