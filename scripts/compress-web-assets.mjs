import { readdir, readFile, writeFile } from "node:fs/promises";
import { extname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { brotliCompress, constants, gzip } from "node:zlib";

const gzipAsync = promisify(gzip);
const brotliAsync = promisify(brotliCompress);
const TEXT_EXTENSIONS = new Set([".js", ".css", ".html", ".json", ".webmanifest", ".svg", ".txt"]);

// Keep originals for clients without compression support and the Tauri asset
// protocol. Skip symlinks, already-compressed sidecars, images and font binaries.
export async function compressWebAssets(directory) {
  let count = 0;
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      count += await compressWebAssets(path);
    } else if (entry.isFile() && TEXT_EXTENSIONS.has(extname(entry.name))) {
      const source = await readFile(path);
      const [gz, br] = await Promise.all([
        gzipAsync(source, { level: 9 }),
        brotliAsync(source, {
          params: {
            [constants.BROTLI_PARAM_MODE]: constants.BROTLI_MODE_TEXT,
            [constants.BROTLI_PARAM_QUALITY]: 11,
          },
        }),
      ]);
      await Promise.all([writeFile(`${path}.gz`, gz), writeFile(`${path}.br`, br)]);
      count += 1;
    }
  }
  return count;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const count = await compressWebAssets(resolve("dist"));
  console.log(`Precompressed ${count} web assets (gzip + Brotli).`);
}
