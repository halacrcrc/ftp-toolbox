import { useCallback, useEffect, useRef, useState } from "react";
import { api, NetInterface } from "../api";
import { LogEntry } from "../App";

type Log = (text: string, level?: LogEntry["level"]) => void;

/**
 * 网卡列表的轮询周期。
 *
 * 枚举一次实测 p50≈4ms、尖峰 30-80ms，3s 一次的成本可以忽略；换来的是拔网线
 * 后最多 3s 下拉就更新 —— 对配置类控件而言足够「实时」，而且不依赖任何平台
 * 专有通知机制（Windows 的 NotifyIpInterfaceChange 只覆盖一个平台）。
 * 窗口不可见时不轮询；页面切走时组件卸载，effect 清理会直接停掉定时器。
 */
const INTERFACE_POLL_MS = 3000;

/**
 * 网卡列表：挂载拉一次 + 定时轮询 + focus/重新可见时补拉（原 ServersView 逻辑，
 * 侧栏按协议重组后每个协议页各挂一份 —— 同一时刻只有一个页面挂载，轮询也只有一份）。
 *
 * 返回 `reload` 供「刷新」按钮使用：刷新运行态的同时重读网卡（IP 会随网络环境变化）。
 */
export default function useInterfaces(log: Log): {
  interfaces: NetInterface[];
  interfacesLoaded: boolean;
  reload: () => Promise<void>;
} {
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

  return { interfaces, interfacesLoaded, reload: loadInterfaces };
}
