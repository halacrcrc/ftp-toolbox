import { test } from "node:test";
import assert from "node:assert/strict";
import { baseRemote, crumbsRemote, isRoot, joinRemote, parentRemote } from "./remotepath.ts";

test("joinRemote: 常规拼接与根/空父目录", () => {
  assert.equal(joinRemote("/a", "b"), "/a/b");
  assert.equal(joinRemote("/", "b"), "/b");
  assert.equal(joinRemote("", "b"), "/b");
  assert.equal(joinRemote("/a/b", "c.txt"), "/a/b/c.txt");
});

test("joinRemote: 清理重复斜杠、点段与首尾斜杠", () => {
  assert.equal(joinRemote("//a//", "b"), "/a/b");
  assert.equal(joinRemote("/a", "/b/"), "/a/b");
  assert.equal(joinRemote("/a", "."), "/a");
  assert.equal(joinRemote("/a", ".."), "/");
  assert.equal(joinRemote("/a", "../c"), "/c");
  assert.equal(joinRemote("/a/b", "../../x"), "/x");
  // 越过根的 .. 停在根（后端 list(None) 即 "/"，根的绝对路径语义），无 ".." 残留
  assert.equal(joinRemote("/", "../x"), "/x");
  assert.equal(joinRemote("", ".."), "/");
});

test("joinRemote: 名字里的空格与中文原样保留", () => {
  assert.equal(joinRemote("/docs", "我的 文件.txt"), "/docs/我的 文件.txt");
  assert.equal(joinRemote("/a b", "c d"), "/a b/c d");
});

test("parentRemote: 逐级上溯，根的上级是空串", () => {
  assert.equal(parentRemote("/a/b"), "/a");
  assert.equal(parentRemote("/a"), "/");
  assert.equal(parentRemote("/"), "");
  assert.equal(parentRemote(""), "");
  assert.equal(parentRemote("/a/b/c.txt"), "/a/b");
});

test("baseRemote: 取最后一段，根返回空串", () => {
  assert.equal(baseRemote("/a/b/c.txt"), "c.txt");
  assert.equal(baseRemote("/a"), "a");
  assert.equal(baseRemote("/"), "");
  assert.equal(baseRemote(""), "");
});

test("baseRemote: 空格与中文名完整返回", () => {
  assert.equal(baseRemote("/docs/我的 文件.txt"), "我的 文件.txt");
});

test("isRoot: 空串与斜杠都算根", () => {
  assert.equal(isRoot("/"), true);
  assert.equal(isRoot(""), true);
  assert.equal(isRoot("/a"), false);
  assert.equal(isRoot("/a/b"), false);
});

test("crumbsRemote: 根到当前的面包屑序列", () => {
  assert.deepEqual(crumbsRemote("/"), [{ name: "/", path: "/" }]);
  assert.deepEqual(crumbsRemote("/a/b"), [
    { name: "/", path: "/" },
    { name: "a", path: "/a" },
    { name: "b", path: "/a/b" },
  ]);
  // 空串与 "/" 等价（根）
  assert.deepEqual(crumbsRemote(""), crumbsRemote("/"));
});
