import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { after, test } from "node:test";
import { resolveReal } from "../src/paths.ts";

// Inside the project, like Cargo's target dir for the Rust tests.
const cache = join(import.meta.dirname, "..", "node_modules", ".cache");
mkdirSync(cache, { recursive: true });
const root = realpathSync(mkdtempSync(join(cache, "paths-")));
const work = join(root, "work");
const outside = join(root, "outside");
mkdirSync(join(work, "src"), { recursive: true });
mkdirSync(outside);
writeFileSync(join(work, "notes.md"), "");
symlinkSync(outside, join(work, "link"));
after(() => rmSync(root, { recursive: true }));

test("relative paths resolve against the workdir", () => {
  assert.equal(resolveReal(work, "notes.md"), join(work, "notes.md"));
  assert.equal(resolveReal(work, "./src/../notes.md"), join(work, "notes.md"));
  assert.equal(resolveReal(work, "."), work);
});

test("dot-dot leaves the workdir visibly", () => {
  assert.equal(resolveReal(work, "../outside"), outside);
});

test("symlinks are followed", () => {
  assert.equal(resolveReal(work, "link"), outside);
});

test("a new file resolves through its nearest existing ancestor", () => {
  assert.equal(resolveReal(work, "src/new/file.ts"), join(work, "src", "new", "file.ts"));
  // A symlinked directory cannot carry a new file out unnoticed.
  assert.equal(resolveReal(work, "link/new.txt"), join(outside, "new.txt"));
});

test("a file in the middle of the path is an error, not a guess", () => {
  assert.throws(() => resolveReal(work, "notes.md/x"), { code: "ENOTDIR" });
});
