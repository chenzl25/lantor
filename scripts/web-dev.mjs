import { spawn } from "node:child_process";
import { watch } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const restartDelayMs = 250;
const vitePort = process.env.LANTOR_WEB_VITE_PORT || "5173";

let backend = null;
let vite = null;
let shuttingDown = false;
let restartTimer = null;
let backendRestarting = false;

function spawnProcess(name, command, args) {
  const child = spawn(command, args, {
    cwd: repoRoot,
    env: process.env,
    stdio: "inherit",
  });

  child.on("exit", (code, signal) => {
    if (!shuttingDown) {
      console.log(`[web:dev] ${name} exited with ${signal ?? code}`);
    }
  });

  return child;
}

function startBackend() {
  backend = spawnProcess("backend", "cargo", [
    "run",
    "--manifest-path",
    "src-tauri/Cargo.toml",
    "--",
    "--web-only",
  ]);
}

function startVite() {
  vite = spawnProcess("vite", "npm", ["run", "dev", "--", "--host", "127.0.0.1", "--port", vitePort]);
}

function stopProcess(child) {
  if (!child || child.killed) return;
  child.kill("SIGTERM");
}

function restartBackend() {
  if (shuttingDown) return;
  clearTimeout(restartTimer);
  restartTimer = setTimeout(() => {
    if (backendRestarting) return;
    backendRestarting = true;
    console.log("[web:dev] restarting backend after src-tauri change");
    const currentBackend = backend;
    if (!currentBackend || currentBackend.killed) {
      startBackend();
      backendRestarting = false;
      return;
    }
    currentBackend.once("exit", () => {
      if (!shuttingDown) startBackend();
      backendRestarting = false;
    });
    stopProcess(currentBackend);
  }, restartDelayMs);
}

function shutdown(signal) {
  shuttingDown = true;
  clearTimeout(restartTimer);
  stopProcess(vite);
  stopProcess(backend);
  process.once("exit", () => {});
  setTimeout(() => process.exit(signal === "SIGINT" ? 130 : 143), 100);
}

startBackend();
startVite();

const watchedPaths = [
  join(repoRoot, "src-tauri", "src"),
  join(repoRoot, "src-tauri", "Cargo.toml"),
  join(repoRoot, "src-tauri", "tauri.conf.json"),
];

const watchers = watchedPaths.flatMap((path) => {
  try {
    return [watch(path, { recursive: true }, restartBackend)];
  } catch (err) {
    console.warn(`[web:dev] unable to watch ${path}: ${err.message}`);
    return [];
  }
});

process.on("SIGINT", () => shutdown("SIGINT"));
process.on("SIGTERM", () => shutdown("SIGTERM"));
process.on("exit", () => watchers.forEach((watcher) => watcher.close()));
