#!/usr/bin/env node
/*
 * Layer 2 composer input latency benchmark.
 *
 * This is the user-perceived latency benchmark, not the Layer 1 SSR mechanism
 * guard in composer-input-latency.mjs. It runs the production web bundle in a
 * headed Chromium browser, drives real textareas through Playwright keyboard
 * input or simulated composition events, and measures input/composition event
 * time to the next animation-frame paint boundary.
 *
 * Reported numbers are intended to explain "typing still feels laggy" by
 * correlating p95/p99 input-to-frame latency with long tasks, long animation
 * frames when the browser exposes them, dropped-frame budget misses, and React
 * Profiler commits. Do not publish before/after claims from this benchmark
 * until paired runs are stable enough for the profile being discussed.
 */

import { spawn } from "node:child_process";
import { existsSync, mkdirSync } from "node:fs";
import { resolve } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { chromium } from "playwright";

const DEFAULTS = {
  runs: 3,
  warmup: 1,
  keystrokes: 120,
  port: 4173,
  profile: "all",
  headed: true,
  trace: true,
};

const PROFILES = [
  {
    name: "main-plain-stress",
    surface: "main",
    mode: "plain",
    description: "main composer, plain typing, stress channel",
    text: "abcdefghijklmnopqrstuvwxyz".repeat(8),
  },
  {
    name: "thread-plain-stress",
    surface: "thread",
    mode: "plain",
    description: "thread composer, plain typing, stress thread",
    text: "thread latency sample ".repeat(12),
  },
  {
    name: "main-agent-mention-stress",
    surface: "main",
    mode: "agent-mention",
    description: "main composer, @mention picker, stress roster",
    text: "@agent000 please check ".repeat(8),
  },
  {
    name: "main-channel-autocomplete-stress",
    surface: "main",
    mode: "channel-mention",
    description: "main composer, #channel picker, stress channel list",
    text: "#bench-channel-000 update ".repeat(8),
  },
  {
    name: "main-ime-composition-stress",
    surface: "main",
    mode: "ime-composition",
    description: "main composer, simulated composition events, stress channel",
    text: "输入法组合测试".repeat(16),
  },
  {
    name: "main-streaming-plain-stress",
    surface: "main",
    mode: "streaming-plain",
    streaming: true,
    description: "main composer, plain typing while streaming refresh events fire",
    text: "streaming updates while typing ".repeat(8),
  },
];

function optionString(name, fallback) {
  const index = process.argv.indexOf(`--${name}`);
  if (index === -1) return fallback;
  return process.argv[index + 1] || fallback;
}

function optionNumber(name, fallback) {
  const index = process.argv.indexOf(`--${name}`);
  if (index === -1) return fallback;
  const value = Number(process.argv[index + 1]);
  return Number.isFinite(value) && value >= 0 ? Math.floor(value) : fallback;
}

function hasFlag(name) {
  return process.argv.includes(`--${name}`);
}

const config = {
  runs: optionNumber("runs", DEFAULTS.runs),
  warmup: optionNumber("warmup", DEFAULTS.warmup),
  keystrokes: optionNumber("keystrokes", DEFAULTS.keystrokes),
  port: optionNumber("port", DEFAULTS.port),
  profile: optionString("profile", DEFAULTS.profile),
  url: optionString("url", ""),
  headed: hasFlag("headless") ? false : DEFAULTS.headed,
  trace: hasFlag("no-trace") ? false : DEFAULTS.trace,
  json: hasFlag("json"),
};

function selectedProfiles() {
  if (config.profile === "all") return PROFILES;
  const profile = PROFILES.find((candidate) => candidate.name === config.profile);
  if (!profile) {
    throw new Error(`Unknown profile "${config.profile}". Use one of: all, ${PROFILES.map((item) => item.name).join(", ")}`);
  }
  return [profile];
}

function percentile(values, fraction) {
  const sorted = [...values].sort((left, right) => left - right);
  const index = Math.min(sorted.length - 1, Math.floor((sorted.length - 1) * fraction));
  return sorted[index] ?? 0;
}

function summarize(values) {
  return {
    p50: percentile(values, 0.5),
    p95: percentile(values, 0.95),
    p99: percentile(values, 0.99),
    max: Math.max(...values),
  };
}

function relativeStdDev(values) {
  const mean = values.reduce((sum, value) => sum + value, 0) / values.length;
  if (mean === 0) return 0;
  const variance = values.reduce((sum, value) => sum + (value - mean) ** 2, 0) / values.length;
  return Math.sqrt(variance) / mean;
}

function id(index) {
  return `00000000-0000-4000-8000-${String(index).padStart(12, "0")}`;
}

function timestamp(index) {
  return new Date(Date.UTC(2026, 0, 1, 12, 0, index)).toISOString();
}

function makeAgents(count) {
  return Array.from({ length: count }, (_, index) => ({
    id: id(1000 + index),
    handle: `agent${String(index).padStart(3, "0")}`,
    display_name: `Agent ${String(index).padStart(3, "0")}`,
    role: "agent",
    status: index < 8 ? "running" : "idle",
    runtime: index % 2 === 0 ? "codex" : "claude",
    model: index % 2 === 0 ? "gpt-5.5" : "opus",
    reasoning_effort: "medium",
    service_tier: "",
    avatar: "",
    description: "Synthetic benchmark agent",
    launch_command: "",
    working_directory: "",
    workspace_exists: false,
    workspace_memory_path: "",
    workspace_memory_exists: false,
    workspace_entries: [],
    daily_budget_micros: 0,
  }));
}

function makeChannel(index, kind = "channel", dmAgentId = null) {
  return {
    id: id(2000 + index),
    name: kind === "dm" ? `dm:${id(3000 + index)}` : `bench-channel-${String(index).padStart(3, "0")}`,
    description: kind === "dm" ? "Direct message" : "Synthetic stress channel",
    kind,
    dm_agent_id: dmAgentId,
    unread_count: 0,
  };
}

function messageBody(index) {
  const code = index % 6 === 0 ? "\n\n```ts\nconst value = computeSomethingExpensive(input);\n```\n" : "";
  const list = index % 4 === 0 ? "\n\n- alpha\n- beta\n- gamma\n" : "";
  return `Synthetic message ${index} with markdown, links, and enough text to create realistic layout work.${list}${code}\n\n${"detail ".repeat(20 + (index % 10))}`;
}

function makeMessage(index, channelId, threadRootId = null, sender = "agent") {
  const agentId = sender === "agent" ? id(1000 + (index % 80)) : null;
  return {
    id: id(4000 + index),
    channel_id: channelId,
    thread_root_id: threadRootId,
    sender_agent_id: agentId,
    sender_name: sender === "owner" ? "Martin" : `agent${String(index % 80).padStart(3, "0")}`,
    sender_role: sender,
    body: messageBody(index),
    is_task: false,
    thread_followed: index % 8 === 0,
    delivery_state: "complete",
    stream_key: "",
    task_number: null,
    task_status: null,
    attachments: [],
    artifacts: [],
    created_at: timestamp(index),
    updated_at: timestamp(index),
  };
}

function makeBootstrap(profile, tick = 0) {
  const agents = makeAgents(80);
  const channels = Array.from({ length: 80 }, (_, index) => makeChannel(index));
  const channelId = channels[0].id;
  const messages = Array.from({ length: 360 }, (_, index) =>
    makeMessage(index, channelId, null, index % 11 === 0 ? "owner" : "agent"));
  const threadRoot = messages[0];
  const replies = Array.from({ length: 80 }, (_, index) =>
    makeMessage(700 + index, channelId, threadRoot.id, index % 7 === 0 ? "owner" : "agent"));
  const streamingMessage = profile.streaming ? [{
    ...makeMessage(950, channelId, null, "agent"),
    id: id(4950),
    body: `Streaming synthetic progress tick ${tick}\n\n${"partial output ".repeat(80 + tick)}`,
    delivery_state: "streaming",
    stream_key: "bench-stream",
    updated_at: new Date().toISOString(),
  }] : [];
  return {
    db_url: "synthetic://composer-e2e",
    web_base_url: null,
    owner_profile: {
      display_name: "Martin",
      avatar: "dicebear:dylan:owner",
      description: "Synthetic owner",
    },
    channels,
    channel_members: agents.slice(0, 20).map((agent) => ({
      channel_id: channelId,
      agent_id: agent.id,
      agent_handle: agent.handle,
      agent_display_name: agent.display_name,
      created_at: timestamp(1),
    })),
    agents,
    messages: [...messages, ...replies, ...streamingMessage],
    saved_messages: [],
    dismissed_inbox_items: {},
    read_inbox_items: {},
    artifacts: [],
    tasks: [],
    reminders: [],
    agent_schedules: [],
    agent_runs: profile.streaming ? agents.slice(0, 8).map((agent, index) => ({
      id: id(6000 + index),
      agent_id: agent.id,
      agent_handle: agent.handle,
      work_item_id: id(6200 + index),
      command: "synthetic streaming run",
      working_directory: "",
      status: "running",
      pid: 10000 + index,
      exit_code: null,
      log: "",
      input_tokens: 0,
      output_tokens: 0,
      cost_micros: 0,
      started_at: timestamp(10 + index),
      stopped_at: null,
    })) : [],
    agent_work_items: [],
    agent_activities: profile.streaming ? agents.slice(0, 8).map((agent, index) => ({
      id: id(6300 + index),
      agent_id: agent.id,
      agent_handle: agent.handle,
      run_id: id(6000 + index),
      kind: "thinking",
      phase: "thinking",
      status: "running",
      title: "Synthetic streaming activity",
      summary: `Tick ${tick}`,
      detail: `Streaming update ${tick} for ${agent.handle}`,
      metadata: {},
      created_at: new Date().toISOString(),
    })) : [],
    supervisor: { pid: null, status: "stopped", updated_at: null },
    launch_agent: { label: "", plist_path: "", installed: false, loaded: false },
  };
}

async function waitForHttp(url) {
  for (let attempt = 0; attempt < 80; attempt += 1) {
    try {
      const response = await fetch(url);
      if (response.ok) return;
    } catch {
      // retry
    }
    await sleep(250);
  }
  throw new Error(`Timed out waiting for ${url}`);
}

function spawnPreviewServer() {
  if (!existsSync(resolve("dist/index.html"))) {
    throw new Error("dist/index.html is missing. Run `npm run build` before `npm run bench:composer:e2e`.");
  }
  const child = spawn("npx", ["vite", "preview", "--host", "127.0.0.1", "--port", String(config.port), "--strictPort"], {
    stdio: ["ignore", "pipe", "pipe"],
    env: { ...process.env, BROWSER: "none" },
  });
  child.stdout.on("data", (chunk) => {
    if (!config.json) process.stderr.write(chunk);
  });
  child.stderr.on("data", (chunk) => {
    if (!config.json) process.stderr.write(chunk);
  });
  return child;
}

async function installPageHarness(page) {
  await page.addInitScript(() => {
    window.__LANTOR_E2E__ = {
      longTasks: [],
      longAnimationFrames: [],
      mutationCount: 0,
      reset() {
        this.longTasks = [];
        this.longAnimationFrames = [];
        this.mutationCount = 0;
        window.__LANTOR_BENCH_PROFILER__?.reset();
      },
      armInputProbe() {
        const record = { start: performance.now(), keydown: null, beforeinput: null, input: null, frame: null };
        return new Promise((resolve) => {
          let resolved = false;
          const cleanup = () => {
            window.removeEventListener("keydown", onKeydown, true);
            window.removeEventListener("beforeinput", onBeforeInput, true);
            window.removeEventListener("input", onInput, true);
          };
          const finish = () => {
            if (resolved) return;
            resolved = true;
            requestAnimationFrame(() => {
              requestAnimationFrame(() => {
                record.frame = performance.now();
                cleanup();
                resolve(record);
              });
            });
          };
          const onKeydown = () => {
            record.keydown ??= performance.now();
          };
          const onBeforeInput = () => {
            record.beforeinput ??= performance.now();
          };
          const onInput = () => {
            record.input ??= performance.now();
            finish();
          };
          window.addEventListener("keydown", onKeydown, true);
          window.addEventListener("beforeinput", onBeforeInput, true);
          window.addEventListener("input", onInput, true);
        });
      },
      measureComposition(selector, value) {
        const textarea = document.querySelector(selector);
        if (!(textarea instanceof HTMLTextAreaElement)) throw new Error(`Missing textarea ${selector}`);
        const started = performance.now();
        textarea.dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true, data: "" }));
        textarea.dispatchEvent(new CompositionEvent("compositionupdate", { bubbles: true, data: value }));
        textarea.value += value;
        textarea.dispatchEvent(new InputEvent("input", {
          bubbles: true,
          data: value,
          inputType: "insertCompositionText",
          isComposing: true,
        }));
        textarea.dispatchEvent(new CompositionEvent("compositionend", { bubbles: true, data: value }));
        return new Promise((resolve) => {
          requestAnimationFrame(() => {
            requestAnimationFrame(() => resolve({
              start: started,
              input: started,
              frame: performance.now(),
            }));
          });
        });
      },
    };

    if (PerformanceObserver.supportedEntryTypes?.includes("longtask")) {
      new PerformanceObserver((list) => {
        window.__LANTOR_E2E__.longTasks.push(...list.getEntries().map((entry) => ({
          name: entry.name,
          startTime: entry.startTime,
          duration: entry.duration,
        })));
      }).observe({ entryTypes: ["longtask"] });
    }
    if (PerformanceObserver.supportedEntryTypes?.includes("long-animation-frame")) {
      new PerformanceObserver((list) => {
        window.__LANTOR_E2E__.longAnimationFrames.push(...list.getEntries().map((entry) => ({
          startTime: entry.startTime,
          duration: entry.duration,
        })));
      }).observe({ type: "long-animation-frame", buffered: true });
    }
    new MutationObserver((mutations) => {
      window.__LANTOR_E2E__.mutationCount += mutations.length;
    }).observe(document.documentElement, {
      attributes: true,
      childList: true,
      characterData: true,
      subtree: true,
    });
  });
}

async function installSyntheticBackend(page, profile) {
  let tick = 0;
  await page.route("**/api/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/api/bootstrap") {
      tick += 1;
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify(makeBootstrap(profile, tick)),
      });
      return;
    }
    if (path === "/api/events") {
      await route.fulfill({
        status: 200,
        contentType: "text/event-stream",
        body: "",
      });
      return;
    }
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ ok: true }),
    });
  });
}

async function installStreamingEventSource(page) {
  await page.addInitScript(() => {
    const NativeEventSource = window.EventSource;
    window.EventSource = class BenchEventSource extends EventTarget {
      constructor(url) {
        super();
        this.url = url;
        this.readyState = NativeEventSource.OPEN;
        this.timer = window.setInterval(() => {
          this.dispatchEvent(new MessageEvent("lantor", { data: "bench-streaming-refresh" }));
        }, 250);
      }
      close() {
        window.clearInterval(this.timer);
        this.readyState = NativeEventSource.CLOSED;
      }
    };
  });
}

async function prepareSurface(page, profile) {
  await page.goto(`${baseUrl()}/?lantorBenchProfiler=1`, { waitUntil: "networkidle" });
  await page.locator(".composer textarea").waitFor({ state: "visible", timeout: 15000 });
  if (profile.surface === "thread") {
    await page.locator('[data-message-id="00000000-0000-4000-8000-000000004000"] button[aria-label*="thread"]').first().click({ force: true });
    await page.locator(".reply-composer textarea").waitFor({ state: "visible", timeout: 15000 });
  }
}

function baseUrl() {
  return config.url || `http://127.0.0.1:${config.port}`;
}

function textForRun(profile) {
  return profile.text.slice(0, config.keystrokes);
}

async function measureRun(page, profile) {
  const selector = profile.surface === "thread" ? ".reply-composer textarea" : ".composer textarea";
  const textarea = page.locator(selector);
  await textarea.click();
  await page.evaluate(() => window.__LANTOR_E2E__.reset());
  const samples = [];
  const text = textForRun(profile);

  if (profile.mode === "ime-composition") {
    for (const char of [...text]) {
      const sample = await page.evaluate(({ selector, char }) =>
        window.__LANTOR_E2E__.measureComposition(selector, char), { selector, char });
      samples.push(sample.frame - sample.input);
    }
  } else {
    for (const char of [...text]) {
      const samplePromise = page.evaluate(() => window.__LANTOR_E2E__.armInputProbe());
      await page.keyboard.type(char);
      const sample = await samplePromise;
      const start = sample.keydown ?? sample.beforeinput ?? sample.start;
      samples.push(sample.frame - start);
    }
  }

  const diagnostics = await page.evaluate(() => {
    const commits = window.__LANTOR_BENCH_PROFILER__?.commits ?? [];
    const longTasks = window.__LANTOR_E2E__.longTasks;
    const longAnimationFrames = window.__LANTOR_E2E__.longAnimationFrames;
    return {
      commitCount: commits.length,
      commitMs: commits.reduce((sum, commit) => sum + commit.actualDuration, 0),
      domMutationCount: window.__LANTOR_E2E__.mutationCount,
      longTaskCount: longTasks.length,
      longTaskMs: longTasks.reduce((sum, task) => sum + task.duration, 0),
      longAnimationFrameCount: longAnimationFrames.length,
      longAnimationFrameMs: longAnimationFrames.reduce((sum, frame) => sum + frame.duration, 0),
    };
  });
  return { samples, diagnostics };
}

async function measureProfile(browser, profile) {
  const context = await browser.newContext({
    viewport: { width: 1440, height: 920 },
    deviceScaleFactor: 1,
  });
  const tracePath = resolve("artifacts", `composer-e2e-${profile.name}-${Date.now()}.zip`);
  if (config.trace) {
    mkdirSync(resolve("artifacts"), { recursive: true });
    await context.tracing.start({ screenshots: true, snapshots: true });
  }

  const page = await context.newPage();
  await installPageHarness(page);
  if (profile.streaming) await installStreamingEventSource(page);
  await installSyntheticBackend(page, profile);
  await prepareSurface(page, profile);

  const runs = [];
  for (let run = 0; run < config.warmup + config.runs; run += 1) {
    await page.locator(profile.surface === "thread" ? ".reply-composer textarea" : ".composer textarea").fill("");
    const result = await measureRun(page, profile);
    if (run >= config.warmup) runs.push(result);
  }

  if (config.trace) await context.tracing.stop({ path: tracePath });
  await context.close();

  const allSamples = runs.flatMap((run) => run.samples);
  const p95s = runs.map((run) => summarize(run.samples).p95);
  const diagnostics = runs.reduce((total, run) => ({
    commitCount: total.commitCount + run.diagnostics.commitCount,
    commitMs: total.commitMs + run.diagnostics.commitMs,
    domMutationCount: total.domMutationCount + run.diagnostics.domMutationCount,
    longTaskCount: total.longTaskCount + run.diagnostics.longTaskCount,
    longTaskMs: total.longTaskMs + run.diagnostics.longTaskMs,
    longAnimationFrameCount: total.longAnimationFrameCount + run.diagnostics.longAnimationFrameCount,
    longAnimationFrameMs: total.longAnimationFrameMs + run.diagnostics.longAnimationFrameMs,
  }), {
    commitCount: 0,
    commitMs: 0,
    domMutationCount: 0,
    longTaskCount: 0,
    longTaskMs: 0,
    longAnimationFrameCount: 0,
    longAnimationFrameMs: 0,
  });

  return {
    profile,
    tracePath: config.trace ? tracePath : null,
    samples: allSamples.length,
    summary: {
      ...summarize(allSamples),
      relativeStdDev: relativeStdDev(p95s),
      commitsPerKey: diagnostics.commitCount / allSamples.length,
      commitMsPerKey: diagnostics.commitMs / allSamples.length,
      domMutationsPerKey: diagnostics.domMutationCount / allSamples.length,
      longTasksPer100Keys: diagnostics.longTaskCount / allSamples.length * 100,
      longTaskMsPerKey: diagnostics.longTaskMs / allSamples.length,
      longAnimationFramesPer100Keys: diagnostics.longAnimationFrameCount / allSamples.length * 100,
      longAnimationFrameMsPerKey: diagnostics.longAnimationFrameMs / allSamples.length,
    },
  };
}

async function main() {
  let server = null;
  let browser = null;
  if (!config.url) {
    server = spawnPreviewServer();
    await waitForHttp(baseUrl());
  }

  try {
    browser = await chromium.launch({
      headless: !config.headed,
      args: ["--disable-background-timer-throttling", "--disable-renderer-backgrounding"],
    });
    const results = [];
    for (const profile of selectedProfiles()) {
      results.push(await measureProfile(browser, profile));
    }

    const output = { config, results };
    if (config.json) {
      console.log(JSON.stringify(output, null, 2));
    } else {
      console.log("Composer Layer 2 input latency benchmark");
      console.log(`runs=${config.runs} warmup=${config.warmup} keystrokes=${config.keystrokes} profile=${config.profile} headed=${config.headed}`);
      console.log("Metric is input/composition event to next animation-frame paint boundary. Lower is better.");
      console.log("");
      console.table(Object.fromEntries(results.map((result) => [result.profile.name, {
        surface: result.profile.surface,
        mode: result.profile.mode,
        "p50 ms/key": result.summary.p50.toFixed(2),
        "p95 ms/key": result.summary.p95.toFixed(2),
        "p99 ms/key": result.summary.p99.toFixed(2),
        "max ms/key": result.summary.max.toFixed(2),
        "commits/key": result.summary.commitsPerKey.toFixed(2),
        "commit ms/key": result.summary.commitMsPerKey.toFixed(2),
        "DOM mutations/key": result.summary.domMutationsPerKey.toFixed(2),
        "long tasks/100": result.summary.longTasksPer100Keys.toFixed(2),
        "long task ms/key": result.summary.longTaskMsPerKey.toFixed(2),
        "LoAF/100": result.summary.longAnimationFramesPer100Keys.toFixed(2),
        "rstd": `${(result.summary.relativeStdDev * 100).toFixed(1)}%`,
        trace: result.tracePath ?? "",
      }])));
    }
  } finally {
    await browser?.close();
    if (server) server.kill("SIGTERM");
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
