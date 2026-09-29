import { pathBase, pathDir, pickFile, pickSaveFile } from "../api";
import { LogEntry } from "../App";

type Log = (text: string, level?: LogEntry["level"]) => void;

interface Props {
  value: string;
  onChange: (v: string) => void;
  /**
   * 选中已有文件后的附加动作。FTP/TFTP 客户端用它把「远程文件名」
   * 同步成刚选中的文件名 —— 上传时用户几乎总是想要这个名字。
   */
  onPicked?: (path: string) => void;
  /** 「保存到…」对话框里预填的文件名（通常传远程文件名）。 */
  saveName?: string;
  disabled?: boolean;
  log: Log;
}

/**
 * 「本地文件」输入框 + Windows 原生对话框按钮。
 *
 * 这个字段在两个方向上含义不同：上传时它是**来源**（选已有文件），
 * 下载时它是**目的地**（另存为），所以给两个入口而不是一个笼统的「浏览」。
 * 输入框本身仍可手填 —— 对话框只是加速，不是限制。
 */
export default function LocalFileField({
  value,
  onChange,
  onPicked,
  saveName,
  disabled,
  log,
}: Props) {
  const browse = async () => {
    try {
      // 从当前路径所在目录开始找，比每次都从「最近使用」开始顺手
      const picked = await pickFile(value ? pathDir(value) || undefined : undefined);
      if (!picked) return;
      onChange(picked);
      onPicked?.(picked);
    } catch (e) {
      log(`打开文件选择框失败: ${e}`, "error");
    }
  };

  const saveAs = async () => {
    try {
      // 预填「当前目录 + 远程文件名」；两者都没有就只带一个空名让系统自己兜底
      const base = (saveName ? pathBase(saveName) : "") || pathBase(value) || "";
      const suggested = value ? pathDir(value) + base : base;
      const picked = await pickSaveFile(suggested || undefined);
      if (!picked) return;
      onChange(picked);
    } catch (e) {
      log(`打开保存对话框失败: ${e}`, "error");
    }
  };

  return (
    <label className="field">
      <span>本地文件</span>
      <div className="input-row">
        <input
          className="grow"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          disabled={disabled}
        />
        <button className="btn small" onClick={browse} disabled={disabled}>
          选择文件…
        </button>
        <button className="btn small" onClick={saveAs} disabled={disabled}>
          保存到…
        </button>
      </div>
    </label>
  );
}
