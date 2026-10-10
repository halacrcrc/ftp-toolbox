/**
 * 远端路径工具：FTP/SFTP 的远端路径都用 `/` 分隔。纯函数、无副作用，
 * 供 RemoteTree 的导航/面包屑使用。
 *
 * 约定：空串与 "/" 都表示「根」（应用里远端路径输入留空 = 服务器默认目录，
 * 树里同样以 "" 为根起点）；返回结果保证不含重复 `/`、`.`、`..`。
 */

/** 去掉重复 `/` 与 `.`/`..` 段。`..` 越过根时停在根（符合远端 chroot 语义）。 */
export function normalize(p: string): string {
  const abs = p.startsWith("/");
  const out: string[] = [];
  for (const seg of p.split("/")) {
    if (!seg || seg === ".") continue;
    if (seg === "..") {
      if (out.length > 0) out.pop();
      continue;
    }
    out.push(seg);
  }
  return (abs ? "/" : "") + out.join("/");
}

/** 拼接子路径：`joinRemote("/a", "b")` → `/a/b`；`joinRemote("", "b")` → `/b`。 */
export function joinRemote(parent: string, child: string): string {
  return normalize(`${parent}/${child}`);
}

/** 上一级目录；已在根（"/" 或 ""）时返回 ""（调用方据此禁用「上级」）。 */
export function parentRemote(path: string): string {
  const p = normalize(path);
  if (p === "" || p === "/") return "";
  const i = p.lastIndexOf("/");
  return i <= 0 ? "/" : p.slice(0, i);
}

/** 最后一段（文件名/目录名）；根返回 ""。 */
export function baseRemote(path: string): string {
  const p = normalize(path);
  if (p === "" || p === "/") return "";
  return p.slice(p.lastIndexOf("/") + 1);
}

/** 是否为根（"" 或 "/"，含仅由 `..`/`.` 构成的退化路径）。 */
export function isRoot(path: string): boolean {
  const p = normalize(path);
  return p === "" || p === "/";
}

/** 面包屑节点：name 用于展示，path 用于跳转。 */
export interface RemoteCrumb {
  name: string;
  path: string;
}

/** 路径展开为根→当前的面包屑序列：`crumbsRemote("/a/b")` → ["/", "a", "b"]。 */
export function crumbsRemote(path: string): RemoteCrumb[] {
  const crumbs: RemoteCrumb[] = [{ name: "/", path: "/" }];
  let acc = "";
  for (const seg of normalize(path).split("/")) {
    if (!seg) continue;
    acc += `/${seg}`;
    crumbs.push({ name: seg, path: acc });
  }
  return crumbs;
}
