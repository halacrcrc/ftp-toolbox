/**
 * 纯格式化工具。
 *
 * 放在 `src/lib/` 而不是 `api.ts` 里，是为了让它能被 `node --test` 直接加载：
 * `api.ts` 顶层 `import { invoke } from "@tauri-apps/api/core"`，在没有
 * Tauri 运行时的进程里 import 就会炸。这里什么都不 import，因此可单测。
 */

/** 人类可读的字节数：B / KB / MB / GB / TB，1024 进制。 */
export function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 ** 2) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 ** 3) return `${(n / 1024 ** 2).toFixed(1)} MB`;
  if (n < 1024 ** 4) return `${(n / 1024 ** 3).toFixed(2)} GB`;
  return `${(n / 1024 ** 4).toFixed(2)} TB`;
}
