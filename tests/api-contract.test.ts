import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { API_COMMANDS } from "../src/api-contract";

const REPO_ROOT = fileURLToPath(new URL("..", import.meta.url));

function source(path: string) {
  return readFileSync(`${REPO_ROOT}/${path}`, "utf8");
}

test("shared API command contract matches the Axum routes", () => {
  const webSource = source("src-tauri/src/web.rs");
  const infrastructureRoutes = new Set(["health", "events", "attachments", "avatars"]);
  const routeCommands = Array.from(
    webSource.matchAll(/\.route\(\s*"\/api\/([a-z_]+)/g),
    (match) => match[1],
  ).filter((command) => !infrastructureRoutes.has(command));

  assert.deepEqual(
    [...new Set(routeCommands)].sort(),
    [...API_COMMANDS].sort(),
  );
});

test("every shared API command is registered with the Tauri transport", () => {
  const mainSource = source("src-tauri/src/main.rs");
  const handlerBlock = mainSource.match(
    /tauri::generate_handler!\[([\s\S]*?)\]\)/,
  )?.[1];
  assert.ok(handlerBlock, "Tauri handler registration was not found");

  const registeredCommands = new Set(
    handlerBlock
      .split(",")
      .map((command) => command.trim())
      .filter(Boolean),
  );
  for (const command of API_COMMANDS) {
    assert.ok(
      registeredCommands.has(command),
      `${command} is missing from the Tauri handler`,
    );
  }
});
