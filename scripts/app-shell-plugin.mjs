import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";

const digest = (bytes) => createHash("sha256").update(bytes).digest("base64");

// Only emitted, content-addressed resources: never public uploads, API data,
// source maps, compression sidecars, or arbitrary runtime fetches.
export function isShellAsset(path) {
  return /^assets\/[^/]+-[\w-]{8,}\.(?:js|css|woff2?|ttf|otf|png|svg|webp|avif|ico)$/.test(path);
}

export function appShellPlugin() {
  let directory;
  return {
    name: "lantor-app-shell",
    apply: "build",
    enforce: "post",
    configResolved(config) { directory = resolve(config.root, config.build.outDir); },
    async writeBundle(_options, bundle) {
      const worker = await readFile(new URL("./app-shell-worker.js", import.meta.url), "utf8");
      const htmlPath = resolve(directory, "index.html");
      const originalHtml = await readFile(htmlPath, "utf8");
      const entries = await Promise.all(Object.keys(bundle).filter(isShellAsset).sort().map(async (path) => ({
        url: `/${path}`, integrity: `sha256-${digest(await readFile(resolve(directory, path)))}`,
      })));
      const version = createHash("sha256").update(originalHtml).update(worker).update(JSON.stringify(entries)).digest("hex").slice(0, 20);
      const html = originalHtml.replace("</head>", `    <meta name="lantor-shell-version" content="${version}" />\n  </head>`);
      entries.unshift({ url: "/index.html", integrity: `sha256-${digest(html)}` });
      await writeFile(htmlPath, html);
      await writeFile(resolve(directory, "sw.js"), worker.replace("self.__LANTOR_APP_SHELL__", JSON.stringify({ version, entries })));
    },
  };
}
