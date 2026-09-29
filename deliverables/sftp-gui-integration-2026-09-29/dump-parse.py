#!/usr/bin/env python3
"""Minimal minidump parser: header, streams, exception record, module lookup.

No third-party deps. Enough to answer "what faulted and in which module".
"""
import struct
import sys

STREAM_NAMES = {
    3: "ThreadList",
    4: "ModuleList",
    5: "MemoryList",
    6: "Exception",
    7: "SystemInfo",
    9: "Memory64List",
    11: "MiscInfo",
    15: "MemoryInfoList",
    0x43500001: "CrashpadInfo",
}


def read_string(d, rva):
    n = struct.unpack_from("<I", d, rva)[0]
    raw = d[rva + 4: rva + 4 + n]
    return raw.decode("utf-16-le", errors="replace")


def parse(path):
    with open(path, "rb") as f:
        d = f.read()

    sig, ver, nstreams, sdir, chk, tds, flags = struct.unpack_from("<IIIIIIQ", d, 0)
    print(f"file        : {path}")
    print(f"signature   : {sig:#x} ({'MDMP ok' if sig == 0x504D444D else 'BAD'})")
    print(f"streams     : {nstreams}  dir@{sdir:#x}")
    print(f"timestamp   : {tds}  flags={flags:#x}")
    print()

    streams = {}
    for i in range(nstreams):
        off = sdir + i * 12
        st, sz, rva = struct.unpack_from("<III", d, off)
        streams[st] = (sz, rva)
        print(f"  stream {st:>10} ({STREAM_NAMES.get(st, '?'):<15}) size={sz:<8} rva={rva:#x}")

    print()

    # SystemInfo (7)
    if 7 in streams:
        _, rva = streams[7]
        arch, level, rev, ncpu, prodtype, major, minor, build, plat = struct.unpack_from(
            "<HHHHIIIII", d, rva)
        print(f"OS          : {major}.{minor}.{build}  arch={arch} cpus={ncpu}")
        print()

    # Exception (6)
    if 6 in streams:
        _, rva = streams[6]
        tid, _al = struct.unpack_from("<II", d, rva)
        (code, eflags, rec, addr, nparam, _al2) = struct.unpack_from(
            "<IIQQII", d, rva + 8)
        params = struct.unpack_from("<15Q", d, rva + 8 + 8 + 24)
        print(f"EXCEPTION   : code={code:#010x}  flags={eflags:#x}  thread={tid}")
        print(f"  address   : {addr:#018x}")
        print(f"  nparams   : {nparam}")
        for j in range(nparam):
            print(f"    param[{j}] = {params[j]:#018x}")
        print()

        # Module lookup (4)
        if 4 in streams:
            _, mrva = streams[4]
            nmod = struct.unpack_from("<I", d, mrva)[0]
            best = None
            for k in range(nmod):
                moff = mrva + 4 + k * 108
                base, size = struct.unpack_from("<QI", d, moff)
                namerva = struct.unpack_from("<I", d, moff + 20)[0]
                if base <= addr < base + size:
                    best = (read_string(d, namerva), base, size, addr - base)
            print(f"modules     : {nmod}")
            if best:
                name, base, size, off = best
                print(f"FAULT MODULE: {name}")
                print(f"  base={base:#018x} size={size:#x} offset={off:#x} ({off})")
            else:
                print("FAULT MODULE: <not inside any listed module>")
            print()
    else:
        print("no exception stream")


if __name__ == "__main__":
    parse(sys.argv[1])
