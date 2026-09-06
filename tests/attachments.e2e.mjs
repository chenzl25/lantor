import assert from "node:assert/strict";
import { request as httpRequest } from "node:http";
import { Readable } from "node:stream";
import { setTimeout as delay } from "node:timers/promises";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { readdir, writeFile } from "node:fs/promises";
import { chromium } from "playwright";

const exec = promisify(execFile);
const [base, pid, channelId, root] = process.argv.slice(2);
const baseline = process.env.LANTOR_ATTACHMENT_BASELINE === "1";
const report = { baseline, cache: [], ranges: {}, uploads: [] };
const metadata = JSON.stringify({ channelId, threadRootId: null, body: "Attachment probe", asTask: false });
const png = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aCWQAAAAASUVORK5CYII=", "base64");
async function upload(bytes, name, type) {
  const form = new FormData();
  form.append("request", metadata);
  form.append("attachments", new Blob([bytes], { type }), name);
  const response = await fetch(`${base}/api/send_message`, { method: "POST", body: form });
  assert.equal(response.status, 200, await response.clone().text());
  return (await response.json()).attachments[0];
}
const attachment = await upload(png, "probe.png", "image/png");
const url = `${base}/api/attachments/${attachment.id}`;
assert.deepEqual(Buffer.from(await (await fetch(url)).arrayBuffer()), png);
const binary = await upload(Buffer.from(Array.from({ length: 1024 }, (_, i) => i % 256)), "probe.bin", "application/octet-stream");
const curl = await exec("curl", ["--noproxy", "*", "-sS", "-D", "-", "-r", "0-99", `${base}/api/attachments/${binary.id}`], { encoding: "buffer" });
const divider = curl.stdout.indexOf("\r\n\r\n");
report.ranges = { headers: curl.stdout.subarray(0, divider).toString(), bytes: curl.stdout.length - divider - 4 };
if (!baseline) {
  assert.match(report.ranges.headers, /206 Partial Content/);
  assert.match(report.ranges.headers, /content-range: bytes 0-99\/1024/i);
  assert.equal(report.ranges.bytes, 100);
  assert.deepEqual(curl.stdout.subarray(divider + 4), Buffer.from(Array.from({ length: 100 }, (_, i) => i)));
}
const browser = await chromium.launch({ headless: true });
try {
  const context = await browser.newContext();
  const page = await context.newPage();
  const cdp = await context.newCDPSession(page);
  await cdp.send("Network.enable");
  const memoryCached = new Set();
  cdp.on("Network.requestServedFromCache", ({ requestId }) => memoryCached.add(requestId));
  cdp.on("Network.responseReceived", ({ requestId, response }) => {
    if (response.url === url) report.cache.push({ requestId, status: response.status, fromDiskCache: response.fromDiskCache ?? false, fromServiceWorker: response.fromServiceWorker ?? false, cacheControl: response.headers["cache-control"] ?? response.headers["Cache-Control"] });
  });
  async function render() {
    await page.evaluate(async (src) => {
      const image = document.createElement("img");
      image.src = src;
      document.body.append(image);
      await image.decode();
    }, url);
  }
  await page.goto(`${base}/__test__/blank`);
  await render();
  await page.reload();
  await render();
  await delay(100);
  report.cache = report.cache.map((entry) => ({ ...entry, memoryCacheEvent: memoryCached.has(entry.requestId) }));
  assert.equal(report.cache.length, 2);
  if (!baseline) {
    const second = report.cache[1];
    assert.ok(second.fromDiskCache || second.memoryCacheEvent, "second render must use browser HTTP cache");
    assert.equal(second.fromServiceWorker, false);
    assert.equal(second.cacheControl, "private, max-age=31536000, immutable");
  }
} finally { await browser.close(); }

async function rss() { return Number((await exec("ps", ["-o", "rss=", "-p", pid])).stdout.trim()) / 1024; }
async function measureUpload(contentLength) {
  const boundary = "lantor-rss-probe";
  const prefix = Buffer.from(`--${boundary}\r\nContent-Disposition: form-data; name="request"\r\n\r\n${metadata}\r\n--${boundary}\r\nContent-Disposition: form-data; name="attachments"; filename="100MiB.bin"\r\nContent-Type: application/octet-stream\r\n\r\n`);
  const suffix = Buffer.from(`\r\n--${boundary}--\r\n`);
  const total = 100 * 1024 * 1024;
  let peak = await rss();
  const before = peak;
  let monitoring = true;
  const monitor = (async () => { while (monitoring) { peak = Math.max(peak, await rss()); await delay(10); } })();
  let sentFileBytes = 0;
  let continueReceived = false;
  const start = performance.now();
  const result = await new Promise((resolve, reject) => {
    let stopped = false;
    const headers = { "content-type": `multipart/form-data; boundary=${boundary}` };
    if (contentLength) { headers["content-length"] = prefix.length + total + suffix.length; headers.expect = "100-continue"; }
    const request = httpRequest(`${base}/api/send_message`, { method: "POST", headers }, (response) => {
      stopped = true;
      let body = "";
      response.on("data", (chunk) => { body += chunk; });
      response.on("end", () => { resolve({ status: response.statusCode, body }); request.destroy(); });
    });
    request.on("error", (error) => { if (!stopped) reject(error); });
    function startBody() {
      Readable.from((async function* () {
        yield prefix;
        const chunk = Buffer.alloc(64 * 1024, 123);
        while (sentFileBytes < total && !stopped) {
          sentFileBytes += chunk.length;
          yield chunk;
          await delay(1);
        }
        if (!stopped) yield suffix;
      })()).pipe(request);
    }
    if (contentLength) { request.once("continue", () => { continueReceived = true; startBody(); }); request.flushHeaders(); }
    else startBody();
  });
  peak = Math.max(peak, await rss());
  monitoring = false;
  await monitor;
  const entry = { contentLength, ...result, beforeMiB: before, peakMiB: peak, deltaMiB: peak - before, sentFileMiB: sentFileBytes / 1024 / 1024, continueReceived, elapsedMs: Math.round(performance.now() - start) };
  report.uploads.push(entry);
  if (!baseline) {
    assert.equal(result.status, 413);
    if (contentLength) { assert.equal(sentFileBytes, 0); assert.equal(continueReceived, false); }
    else assert.ok(sentFileBytes <= 65 * 1024 * 1024, "stop around the 64MiB per-file limit");
    assert.ok(entry.deltaMiB < 24, `bounded upload RSS: ${entry.deltaMiB}MiB`);
    assert.deepEqual(await readdir(`${root}/.tmp`), [], "failed uploads leave no staged files");
  }
  await delay(200);
}
await measureUpload(true);
await measureUpload(false);
if (process.env.LANTOR_ATTACHMENT_EVIDENCE_DIR) {
  await writeFile(`${process.env.LANTOR_ATTACHMENT_EVIDENCE_DIR}/${baseline ? "before" : "after"}.json`, JSON.stringify(report, null, 2));
}
console.log(JSON.stringify(report, null, 2));
