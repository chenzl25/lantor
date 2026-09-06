import assert from "node:assert/strict";
import { test } from "node:test";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { appShellPlugin } from "../scripts/app-shell-plugin.mjs";

test("app-shell build is content-versioned and precaches only verified emitted static assets", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "lantor-shell-build-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  await mkdir(join(directory, "assets"));
  const html = '<html><head></head><body><script src="/assets/app-abcdefgh.js"></script></body></html>';
  const files = {
    "assets/app-abcdefgh.js": "console.log('build one')",
    "assets/font-abcdefgh.woff2": "font fixture bytes",
    "assets/app-abcdefgh.js.map": "source map",
    "assets/app-abcdefgh.js.gz": "sidecar",
    "api/bootstrap": "private workspace",
    "attachments/file-abcdefgh.png": "private upload",
    "extra.js": "unversioned public file",
  };
  for (const [path, body] of Object.entries(files).filter(([path]) => path.startsWith("assets/"))) await writeFile(join(directory, path), body);
  const plugin = appShellPlugin();
  plugin.configResolved({ root: directory, build: { outDir: "." } });
  async function build() {
    await writeFile(join(directory, "index.html"), html);
    await plugin.writeBundle({}, Object.fromEntries(Object.keys(files).map((path) => [path, {}])));
    const worker = await readFile(join(directory, "sw.js"), "utf8");
    return JSON.parse(worker.match(/const \{ version, entries \} = (.*);/)![1]);
  }
  const first = await build();
  assert.deepEqual(first.entries.map((entry: { url: string }) => entry.url), ["/index.html", "/assets/app-abcdefgh.js", "/assets/font-abcdefgh.woff2"]);
  for (const entry of first.entries) {
    const bytes = await readFile(join(directory, entry.url.slice(1)));
    assert.equal(entry.integrity, `sha256-${createHash("sha256").update(bytes).digest("base64")}`);
  }
  assert.match(await readFile(join(directory, "index.html"), "utf8"), new RegExp(`name="lantor-shell-version" content="${first.version}"`));
  assert.equal((await build()).version, first.version, "identical inputs keep the worker version stable");
  await writeFile(join(directory, "assets/app-abcdefgh.js"), "console.log('build two')");
  assert.notEqual((await build()).version, first.version, "even same-name content changes invalidate the manifest");
});
