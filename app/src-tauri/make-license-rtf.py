#!/usr/bin/env python3
"""生成安装程序许可页用的 `license.rtf`。

用法（在 `app/src-tauri/` 目录下）：

    python make-license-rtf.py

改许可文案就改下面的 TEXT，然后重跑本脚本。**不要手改 `license.rtf`。**

--------------------------------------------------------------------------
为什么必须是 RTF，而不是纯文本
--------------------------------------------------------------------------
Tauri 把同一个文件喂给两个安装器目标，两边处理方式不同：

* **NSIS**（`tauri-bundler/src/bundle/windows/nsis/mod.rs`）：读原始字节，
  **前置一个 UTF-8 BOM** 后写成 `license_file`，交给 `MUI_PAGE_LICENSE`。
* **MSI/WiX**（`.../windows/msi/mod.rs`）：后缀不是 `.rtf` 时，会把文件
  **当文本读进来**，再套进一个写死 `\\ansicpg1252` 的 RTF 模板。

于是纯文本中文会走成：NSIS 侧正确（BOM 让它按 UTF-8 解析），但 MSI 侧被塞进
1252 代码页的 RTF → **乱码**。用 RTF + `\\uNNNN?` 转义则两边都对。

--------------------------------------------------------------------------
为什么必须写成单行
--------------------------------------------------------------------------
MSI 侧那个包装模板会做 `content.replace('\\n', '\\\\par ')`。如果本文件里带裸换行，
每一行都会被替换成一个 `\\par`，**渲染出来每行之间都多一个空行**（实测总行数
22 → 43）。所以生成结果里不能有任何 `\\r` / `\\n`，全部用 `\\par` 分隔。

--------------------------------------------------------------------------
其他两个已验证的细节
--------------------------------------------------------------------------
* NSIS 跳过 BOM 后按**内容**判断是否 RTF（`Source/script.cpp`：
  `memcmp(ldata, "{\\rtf", 5)`），所以 BOM 不会破坏识别。
* Tauri 那个 `.rtf` 快路径是**死代码**：`license.ends_with(".rtf")` 调的是
  `Path::ends_with`，按路径分量比较，对 `license.rtf` 恒为 false
  （实测 `PathBuf::from("...license.rtf").ends_with(".rtf") == false`）。
  所以文件**一定会**被包装一次 —— 上面的"单行"约束因此是硬要求。
"""

import io
import sys
from pathlib import Path

TEXT = """FTP 工具箱 —— 许可协议

本软件按 MIT 或 Apache-2.0 双许可发布，你可任选其一。

两种许可的完整条款随本安装包一并提供，安装后位于程序目录下的
LICENSE-MIT 与 LICENSE-APACHE 文件；也可访问：

  Apache License 2.0   https://www.apache.org/licenses/LICENSE-2.0
  MIT License          https://opensource.org/licenses/MIT

在遵守所选许可条款的前提下，你可以自由使用、修改、分发本软件，
包括用于商业目的。Apache-2.0 相较 MIT 额外提供明确的专利授权，
若你所在组织对此有要求，可选择 Apache-2.0。

本软件按"现状"提供，不附带任何明示或暗示的担保。

--------------------------------------------------------------------------

FTP Toolbox is dual-licensed under MIT OR Apache-2.0, at your option.
The full text of each license is installed alongside the application."""

HEADER = (
    r"{\rtf1\ansi\ansicpg936\deff0\nouicompat"
    r"{\fonttbl{\f0\fnil\fcharset134 Microsoft YaHei;}}"
    r"{\*\generator FTP Toolbox make-license-rtf.py}\viewkind4\uc1"
    r"\pard\sa160\sl276\slmult1\f0\fs18\lang2052 "
)


def escape(ch: str) -> str:
    """RTF 转义：ASCII 特殊字符加反斜杠；非 ASCII 走 \\uN? 形式。"""
    code = ord(ch)
    if ch in "\\{}":
        return "\\" + ch
    if code < 128:
        return ch
    if code > 0xFFFF:
        raise ValueError(f"U+{code:04X} 超出 BMP，需改用代理对，本脚本不支持")
    # \uc1 下每个 \uN 后必须跟一个占位字符
    return "\\u%d?" % code


def build_rtf(text: str) -> str:
    lines = ["".join(escape(c) for c in line) for line in text.split("\n")]
    # 全部用 \par 分隔，结果里不得出现任何裸换行 —— 见文件头说明
    body = r"\par ".join(lines)
    return HEADER + body + r"\par}"


def main() -> int:
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).with_name("license.rtf")
    rtf = build_rtf(TEXT)
    if "\r" in rtf or "\n" in rtf:
        print("内部错误：生成结果含裸换行，MSI 侧会出现多余空行", file=sys.stderr)
        return 1
    with io.open(out, "wb") as fh:
        fh.write(rtf.encode("ascii"))  # 转义后应全为 ASCII
    print(f"已写出 {out}（{len(rtf)} 字节，单行纯 ASCII）")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
