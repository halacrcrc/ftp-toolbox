import { test } from "node:test";
import assert from "node:assert/strict";
import { fmtBytes } from "./format.ts";

test("fmtBytes: 不足 1 KB 时按字节显示", () => {
  assert.equal(fmtBytes(0), "0 B");
  assert.equal(fmtBytes(1), "1 B");
  assert.equal(fmtBytes(1023), "1023 B");
});

test("fmtBytes: 边界值落在正确的量级上", () => {
  // 1024 是 KB 的下边界，1023 仍属于 B —— 差 1 字节不能跳档。
  assert.equal(fmtBytes(1024), "1.0 KB");
  assert.equal(fmtBytes(1024 ** 2), "1.0 MB");
  assert.equal(fmtBytes(1024 ** 3), "1.00 GB");
  assert.equal(fmtBytes(1024 ** 4), "1.00 TB");
});

test("fmtBytes: KB/MB 一位小数，GB/TB 两位小数", () => {
  assert.equal(fmtBytes(1536), "1.5 KB");
  assert.equal(fmtBytes(8283759), "7.9 MB");
  assert.equal(fmtBytes(1024 ** 3 * 2.5), "2.50 GB");
});

test("fmtBytes: 速度也是字节数，同一函数渲染", () => {
  // 进度条把 speed 直接丢给 fmtBytes，所以 MB/s 那一档必须同样可读。
  assert.equal(fmtBytes(3_300_000), "3.1 MB");
});
