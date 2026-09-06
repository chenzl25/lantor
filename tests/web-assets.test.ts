import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { brotliDecompressSync, gunzipSync } from "node:zlib";

import { compressWebAssets } from "../scripts/compress-web-assets.mjs";

test("web build sidecars round-trip, preserve originals and skip binary files", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "lantor-web-assets-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  await mkdir(join(directory, "assets"));
  const text = "export const message = 'Lantor';\n".repeat(100);
  for (const path of ["index.html", "manifest.webmanifest", "assets/main.js", "assets/main.css"]) {
    await writeFile(join(directory, path), text);
  }
  const binary = Buffer.from([0x89, 0x50, 0x4e, 0x47]);
  await writeFile(join(directory, "icon.png"), binary);
  await writeFile(join(directory, "assets/font.woff2"), binary);

  assert.equal(await compressWebAssets(directory), 4);
  for (const path of ["index.html", "manifest.webmanifest", "assets/main.js", "assets/main.css"]) {
    assert.equal(await readFile(join(directory, path), "utf8"), text);
    const gz = await readFile(join(directory, `${path}.gz`));
    const br = await readFile(join(directory, `${path}.br`));
    assert.equal(gunzipSync(gz).toString(), text);
    assert.equal(brotliDecompressSync(br).toString(), text);
    assert.ok(gz.length < Buffer.byteLength(text));
    assert.ok(br.length < Buffer.byteLength(text));
  }
  assert.deepEqual(await readFile(join(directory, "icon.png")), binary);
  assert.ok(!(await readdir(directory)).includes("icon.png.gz"));
  assert.ok(!(await readdir(join(directory, "assets"))).includes("font.woff2.br"));
  assert.equal(await compressWebAssets(directory), 4, "reruns do not compress sidecars again");
});

test("PWA and page icons are local PNG assets with the declared dimensions", async () => {
  const root = new URL("../", import.meta.url);
  const html = await readFile(new URL("index.html", root), "utf8");
  const manifest = JSON.parse(await readFile(new URL("public/manifest.webmanifest", root), "utf8"));
  assert.ok(!html.includes("raw.githubusercontent.com"));
  const icons = [
    ...manifest.icons,
    ...Array.from(html.matchAll(/<link\s+[^>]*rel="(?:icon|apple-touch-icon)"[^>]*>/g), ([tag]) => ({
      src: tag.match(/href="([^"]+)"/)?.[1],
      sizes: tag.match(/sizes="([^"]+)"/)?.[1],
    })),
  ];
  assert.equal(icons.length, 4);
  for (const icon of icons) {
    assert.match(icon.src, /^\/(?!\/)[^?#]+\.png$/);
    const png = await readFile(new URL(`public${icon.src}`, root));
    assert.equal(png.subarray(0, 8).toString("hex"), "89504e470d0a1a0a");
    assert.equal(`${png.readUInt32BE(16)}x${png.readUInt32BE(20)}`, icon.sizes);
  }
});
