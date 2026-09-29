#!/usr/bin/env python3
"""Launch the Tauri app, watch its WebView2 child processes, print command lines.

Also dumps command lines of an already-running reference WebView2 app
(nyaterm / SearchHost) for comparison.

Run this OUTSIDE the sandbox so the child does not inherit the sandbox env.
"""
import os
import subprocess
import sys
import time

import psutil


def ancestors(proc):
    seen = []
    try:
        for p in proc.parents():
            seen.append(p)
    except Exception:
        pass
    return seen


def webviews_of(root_pid):
    out = []
    for p in psutil.process_iter(["pid", "name", "ppid", "cmdline", "create_time"]):
        if (p.info["name"] or "").lower() != "msedgewebview2.exe":
            continue
        if p.info["pid"] == root_pid:
            continue
        if any(a.pid == root_pid for a in ancestors(p)):
            out.append(p)
    return out


def ptype(cmdline):
    for a in cmdline:
        if a.startswith("--type="):
            return a.split("=", 1)[1]
    return "browser"


def show(label, procs):
    print(f"--- {label}: {len(procs)} process(es) ---")
    for p in sorted(procs, key=lambda x: x.info["pid"]):
        cl = p.info["cmdline"] or []
        print(f"  pid={p.info['pid']:<7} type={ptype(cl):<12} argc={len(cl)}")
        if p.info["pid"] == min(q.info["pid"] for q in procs) if procs else False:
            pass
        # print the interesting flags only
        interesting = [
            a for a in cl
            if a.startswith(("--type=", "--user-data-dir=", "--no-sandbox",
                             "--disable-", "--enable-", "--single-process",
                             "--in-process-gpu", "--headless", "--app="))
        ]
        if interesting:
            print("        " + " ".join(interesting)[:400])
    print()


def main():
    exe = sys.argv[1]
    dur = float(sys.argv[2]) if len(sys.argv) > 2 else 35.0

    print("=== reference: currently running WebView2 hosts ===")
    for p in psutil.process_iter(["pid", "name"]):
        if (p.info["name"] or "").lower() in ("nyaterm.exe",):
            print(f"  {p.info['name']} pid={p.info['pid']}")
    ref = [p for p in psutil.process_iter(["pid", "name", "cmdline"])
           if (p.info["name"] or "").lower() == "msedgewebview2.exe"]
    print(f"  total msedgewebview2.exe on box: {len(ref)}")
    for p in ref[:3]:
        cl = p.info["cmdline"] or []
        print(f"    pid={p.info['pid']} type={ptype(cl)} flags="
              + " ".join(a for a in cl if a.startswith("--"))[:300])
    print()

    print(f"=== launching {exe} ===")
    proc = subprocess.Popen([exe], cwd=os.path.dirname(os.path.abspath(exe)))
    root = proc.pid
    print(f"app pid = {root}")

    t0 = time.time()
    last_seen = {}
    while time.time() - t0 < dur:
        if not psutil.pid_exists(root):
            print(f"[{time.time()-t0:5.1f}s] APP PROCESS EXITED (code={proc.poll()})")
            break
        wv = webviews_of(root)
        cur = {p.info["pid"]: ptype(p.info["cmdline"] or []) for p in wv}
        for pid, t in cur.items():
            if pid not in last_seen:
                print(f"[{time.time()-t0:5.1f}s] + webview pid={pid} type={t}")
        for pid in list(last_seen):
            if pid not in cur:
                print(f"[{time.time()-t0:5.1f}s] - webview pid={pid} type={last_seen[pid]} DIED")
        last_seen = cur
        time.sleep(0.5)

    print()
    show("final webviews of app", webviews_of(root))
    if psutil.pid_exists(root):
        print("app still alive -> terminating")
        try:
            psutil.Process(root).terminate()
        except Exception as e:
            print("terminate failed:", e)


if __name__ == "__main__":
    main()
