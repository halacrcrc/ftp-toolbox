// GUI 端到端联调驱动：通过 CDP 操作真实 DOM（点击/填表），
// 而不是绕过 UI 直接调后端命令 —— 这样才测到 React + IPC + Rust 整条链路。
//
// 用法: node gui-e2e.mjs <inspect|start|status|stop|shot> [args...]
//   需要应用已带 --remote-debugging-port=9222 启动。
import { writeFileSync } from "node:fs";

const CDP_HTTP = "http://127.0.0.1:9222/json";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// 页面内注入的辅助函数：React 受控组件必须走原生 setter + input 事件，
// 直接赋 value 不会触发 onChange。
const HELPERS = `
window.__e2e = {
  byText(tag, text, root) {
    const nodes = [...(root || document).querySelectorAll(tag)];
    return nodes.find(n => (n.innerText || '').trim() === text)
        || nodes.find(n => (n.innerText || '').includes(text));
  },
  setInput(el, value) {
    const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, value);
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
  },
  setSelect(el, value) {
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(el, value);
    el.dispatchEvent(new Event('change', { bubbles: true }));
  },
  click(el) {
    el.scrollIntoView({ block: 'center' });
    el.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, view: window }));
  },
  // 找到标题为 title 的卡片容器
  card(title) {
    const cards = [...document.querySelectorAll('.card, section, article, div')];
    const hit = cards.find(c => {
      const h = c.querySelector('h1,h2,h3,h4,.card-title,.title');
      return h && (h.innerText || '').trim() === title;
    });
    return hit || null;
  },
  // 在卡片内按 label 文本找输入框
  fieldInput(cardEl, labelIncludes) {
    for (const f of cardEl.querySelectorAll('label.field, .field')) {
      const sp = f.querySelector('span');
      if (sp && (sp.innerText || '').includes(labelIncludes)) {
        const inp = f.querySelector('input:not([type=file]):not([type=checkbox]):not([type=radio]), select, textarea');
        if (inp) return inp;
      }
    }
    return null;
  }
};
`;

async function connect() {
  const t0 = Date.now();
  let page = null;
  while (Date.now() - t0 < 30000) {
    try {
      const targets = await (await fetch(CDP_HTTP)).json();
      page = targets.find((t) => t.type === "page");
      if (page) break;
    } catch { /* not up yet */ }
    await sleep(400);
  }
  if (!page) throw new Error("30s 内没有等到 CDP page 目标");

  const ws = new WebSocket(page.webSocketDebuggerUrl);
  let id = 0;
  const pending = new Map();
  const events = [];
  ws.addEventListener("message", (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.id && pending.has(msg.id)) {
      pending.get(msg.id)(msg.result ?? msg.error);
      pending.delete(msg.id);
      return;
    }
    if (msg.method === "Runtime.exceptionThrown") {
      const d = msg.params.exceptionDetails;
      events.push(`[EXCEPTION] ${d.exception?.description ?? d.text}`);
    } else if (msg.method === "Log.entryAdded" && msg.params.entry.level === "error") {
      events.push(`[log.error] ${msg.params.entry.text.slice(0, 200)}`);
    }
  });
  await new Promise((r) => ws.addEventListener("open", r));
  const send = (method, params = {}) => {
    const msgId = ++id;
    ws.send(JSON.stringify({ id: msgId, method, params }));
    return new Promise((resolve) => pending.set(msgId, resolve));
  };
  await send("Runtime.enable");
  await send("Log.enable");
  await send("Page.enable");
  await sleep(1500);
  await evalJS(HELPERS, send);

  return { send, events, ws };
}

async function evalJS(expression, send) {
  const r = await send("Runtime.evaluate", {
    expression,
    returnByValue: true,
    awaitPromise: true,
  });
  if (r?.exceptionDetails) {
    throw new Error("页面内求值异常: " + JSON.stringify(r.exceptionDetails).slice(0, 400));
  }
  return r?.result?.value;
}

const CARD = `window.__e2e.card('SFTP 服务器')`;

// Git Bash / MSYS 会把 Windows 路径参数改写成 POSIX 形式，这里统一转回来。
function toWin(p) {
  if (!p) return p;
  const m = /^\/([a-zA-Z])\/(.*)$/.exec(p);
  if (m) return `${m[1].toUpperCase()}:\\` + m[2].replace(/\//g, "\\");
  return p.replace(/\//g, "\\");
}

async function main() {
  const cmd = process.argv[2];
  const { send, events } = await connect();
  const js = (e) => evalJS(e, send);

  if (cmd === "inspect") {
    const out = await js(`(() => {
      const c = ${CARD};
      if (!c) return JSON.stringify({ error: 'SFTP 服务器 卡片未找到' });
      return JSON.stringify({
        found: true,
        className: c.className,
        headings: [...c.querySelectorAll('h1,h2,h3,h4,.card-title,.title')].map(h=>h.innerText.trim()),
        fields: [...c.querySelectorAll('label.field, .field')].map(f => {
          const sp = f.querySelector('span');
          const inp = f.querySelector('input, select, textarea');
          return {
            label: sp ? sp.innerText.trim() : null,
            tag: inp ? inp.tagName : null,
            type: inp ? (inp.getAttribute('type') || null) : null,
            value: inp && inp.tagName !== 'SELECT' ? (inp.value ?? null) : null,
            placeholder: inp ? inp.getAttribute('placeholder') : null,
          };
        }),
        buttons: [...c.querySelectorAll('button')].map(b => b.innerText.trim()),
        statusText: (c.innerText.match(/(已停止|运行中|已启动)[^\\n]*/) || [null])[0],
      }, null, 2);
    })()`);
    console.log(out);
  } else if (cmd === "start") {
    const root = toWin(process.argv[3]);
    const [port, user, pass] = process.argv.slice(4);
    const out = await js(`(() => {
      const c = ${CARD};
      if (!c) return 'ERR: 卡片未找到';
      const steps = [];
      const setF = (label, val) => {
        const el = window.__e2e.fieldInput(c, label);
        if (!el) { steps.push('MISS:' + label); return; }
        window.__e2e.setInput(el, val);
        steps.push('SET:' + label + '=' + val);
      };
      setF('共享目录', ${JSON.stringify(root)});
      setF('端口', ${JSON.stringify(port)});
      setF('用户名', ${JSON.stringify(user)});
      setF('密码', ${JSON.stringify(pass)});
      const iface = window.__e2e.fieldInput(c, '监听接口');
      if (iface) { window.__e2e.setSelect(iface, '127.0.0.1'); steps.push('SET:监听接口=127.0.0.1'); }
      else steps.push('MISS:监听接口');
      const btn = [...c.querySelectorAll('button')].find(b => b.innerText.trim() === '启动服务');
      if (!btn) { steps.push('MISS:启动服务按钮'); return steps.join(' | '); }
      window.__e2e.click(btn);
      steps.push('CLICK:启动服务');
      return steps.join(' | ');
    })()`);
    console.log("操作: " + out);
    await sleep(4000);
    const st = await js(`(() => {
      const c = ${CARD};
      const logs = [...document.querySelectorAll('*')].filter(e => e.children.length === 0 && /SFTP/.test(e.innerText||'')).map(e=>e.innerText.trim()).slice(0,20);
      return JSON.stringify({
        cardText: c ? c.innerText.slice(0, 700) : null,
        buttons: c ? [...c.querySelectorAll('button')].map(b=>b.innerText.trim()) : null,
        sftpLogs: logs
      }, null, 2);
    })()`);
    console.log(st);
  } else if (cmd === "addkey") {
    // 模拟用户在 GUI 里点「添加公钥文件…」选文件：
    // 用 CDP 把文件灌进隐藏的 input[type=file]，触发 React 的 onChange。
    const keyPath = toWin(process.argv[3]);
    await send("DOM.enable");
    const obj = await send("Runtime.evaluate", {
      expression: `(() => { const c = ${CARD}; return c ? c.querySelector('input[type=file]') : null; })()`,
    });
    if (!obj?.result?.objectId) {
      console.log("ERR: SFTP 卡片里找不到 input[type=file]  obj=" + JSON.stringify(obj));
    } else {
      console.log("DEBUG evaluate -> objectId=" + obj.result.objectId);
      // hidden 元素不在 DOM 节点树里，requestNode 会给 0；
      // 用 describeNode 拿 backendNodeId 再设文件。
      const desc = await send("DOM.describeNode", { objectId: obj.result.objectId });
      const backendId = desc?.node?.backendNodeId;
      console.log("DEBUG describeNode -> backendNodeId=" + backendId);
      const setRes = await send("DOM.setFileInputFiles", { files: [keyPath], backendNodeId: backendId });
      console.log("DEBUG setFileInputFiles -> " + JSON.stringify(setRes) + "  file=" + keyPath);
      // CDP 设好 files 后不一定触发 React 的 onChange，手动补一次事件。
      const fired = await js(`(() => {
        const c = ${CARD};
        const el = c ? c.querySelector('input[type=file]') : null;
        if (!el) return 'no-input';
        el.dispatchEvent(new Event('input', { bubbles: true }));
        el.dispatchEvent(new Event('change', { bubbles: true }));
        return 'files=' + el.files.length;
      })()`);
      console.log("已注入公钥文件: " + keyPath + "  (" + fired + ")");
      await sleep(1500);
      const after = await js(`(() => {
        const c = ${CARD};
        if (!c) return null;
        const hint = c.querySelector('.hint-inline');
        const keys = [...c.querySelectorAll('.key-line')].map(k => k.innerText.trim().slice(0, 70));
        return JSON.stringify({ keyHint: hint ? hint.innerText.trim() : null, keys });
      })()`);
      console.log(after);
    }
  } else if (cmd === "hostkey") {
    // 先点「刷新」拉状态，再展开「主机密钥详情」读指纹
    const r1 = await js(`(() => {
      const c = ${CARD};
      if (!c) return 'ERR: 卡片未找到';
      const btn = [...c.querySelectorAll('button')].find(b => b.innerText.trim() === '刷新');
      if (btn) { window.__e2e.click(btn); return 'CLICK:刷新'; }
      return 'ERR: 没有刷新按钮';
    })()`);
    console.log(r1);
    await sleep(2500);
    const out = await js(`(() => {
      const c = ${CARD};
      if (!c) return 'ERR: 卡片未找到';
      const btn = [...c.querySelectorAll('button')].find(b => /主机密钥详情/.test(b.innerText));
      if (btn) window.__e2e.click(btn);
      return 'CLICK:主机密钥详情';
    })()`);
    console.log(out);
    await sleep(1200);
    const fp = await js(`(() => {
      const c = ${CARD};
      if (!c) return 'ERR';
      const m = (c.innerText.match(/SHA256:[A-Za-z0-9+/=]+/g) || []);
      const block = c.querySelector('.fingerprint-block, .collapse, .collapsible');
      return JSON.stringify({
        fingerprints: m,
        blockText: block ? block.innerText : null,
        excerpt: c.innerText.slice(-350)
      });
    })()`);
    console.log(fp);
  } else if (cmd === "eval") {
    // 在页面里求值任意 JS（用于直接观察 IPC 返回值）
    const out = await js(process.argv[3]);
    console.log(typeof out === "string" ? out : JSON.stringify(out, null, 2));
  } else if (cmd === "status") {
    const out = await js(`(() => {
      const c = ${CARD};
      return c ? c.innerText.slice(0, 800) : 'ERR: 卡片未找到';
    })()`);
    console.log(out);
  } else if (cmd === "stop") {
    const out = await js(`(() => {
      const c = ${CARD};
      if (!c) return 'ERR: 卡片未找到';
      const btn = [...c.querySelectorAll('button')].find(b => /停止/.test(b.innerText));
      if (!btn) return 'ERR: 没有停止按钮; buttons=' + [...c.querySelectorAll('button')].map(b=>b.innerText.trim()).join(',');
      window.__e2e.click(btn);
      return 'CLICK:' + btn.innerText.trim();
    })()`);
    console.log(out);
  } else if (cmd === "shot") {
    const shot = await send("Page.captureScreenshot", { format: "png", captureBeyondViewport: true });
    writeFileSync(process.argv[3] || "shot.png", Buffer.from(shot.data, "base64"));
    console.log("saved " + (process.argv[3] || "shot.png"));
  } else {
    console.log("用法: node gui-e2e.mjs <inspect|start|status|stop|shot> [args]");
  }

  if (events.length) {
    console.log("\n=== 页面错误事件 ===");
    for (const e of [...new Set(events)].slice(0, 15)) console.log("  " + e);
  }
  process.exit(0);
}

main().catch((e) => {
  console.error("失败:", e.message);
  process.exit(1);
});
