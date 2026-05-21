#!/usr/bin/env node
// Style token gate: fails CI on any new raw color literal in src/styles.css
// that is not in the baseline snapshot.
//
// Rationale: dark theme regressions keep happening because new components
// land with hardcoded `#fff` / `rgba(255,255,255,…)` etc. and only later get
// patched via :root[data-theme="dark"] overrides. This script enforces that
// (a) the set of raw color literals can only shrink (or stay equal),
// (b) any new literal must either be removed/replaced with a token, or
// explicitly snapshotted via `npm run lint:css-tokens -- --update-baseline`.
//
// Fingerprint: { selector, property, literal, n } where n is the occurrence
// index of that literal under the same (selector, property). Line numbers
// are surfaced for human debugging but are NOT part of the fingerprint, so
// inserting/removing unrelated rules does not invalidate the baseline.
//
// Excluded keywords: currentColor, transparent, inherit, initial, unset,
// revert, none. Tokens via var(--…) are inherently safe and never matched.

import fs from "node:fs";
import path from "node:path";
import url from "node:url";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, "..");
const TARGET = path.join(REPO_ROOT, "src/styles.css");
const BASELINE_PATH = path.join(REPO_ROOT, "scripts/styles-raw-color-baseline.json");

const NAMED_COLORS = new Set([
  "aliceblue","antiquewhite","aqua","aquamarine","azure","beige","bisque","black","blanchedalmond",
  "blue","blueviolet","brown","burlywood","cadetblue","chartreuse","chocolate","coral","cornflowerblue",
  "cornsilk","crimson","cyan","darkblue","darkcyan","darkgoldenrod","darkgray","darkgreen","darkgrey",
  "darkkhaki","darkmagenta","darkolivegreen","darkorange","darkorchid","darkred","darksalmon",
  "darkseagreen","darkslateblue","darkslategray","darkslategrey","darkturquoise","darkviolet",
  "deeppink","deepskyblue","dimgray","dimgrey","dodgerblue","firebrick","floralwhite","forestgreen",
  "fuchsia","gainsboro","ghostwhite","gold","goldenrod","gray","green","greenyellow","grey",
  "honeydew","hotpink","indianred","indigo","ivory","khaki","lavender","lavenderblush","lawngreen",
  "lemonchiffon","lightblue","lightcoral","lightcyan","lightgoldenrodyellow","lightgray","lightgreen",
  "lightgrey","lightpink","lightsalmon","lightseagreen","lightskyblue","lightslategray","lightslategrey",
  "lightsteelblue","lightyellow","lime","limegreen","linen","magenta","maroon","mediumaquamarine",
  "mediumblue","mediumorchid","mediumpurple","mediumseagreen","mediumslateblue","mediumspringgreen",
  "mediumturquoise","mediumvioletred","midnightblue","mintcream","mistyrose","moccasin","navajowhite",
  "navy","oldlace","olive","olivedrab","orange","orangered","orchid","palegoldenrod","palegreen",
  "paleturquoise","palevioletred","papayawhip","peachpuff","peru","pink","plum","powderblue",
  "purple","rebeccapurple","red","rosybrown","royalblue","saddlebrown","salmon","sandybrown",
  "seagreen","seashell","sienna","silver","skyblue","slateblue","slategray","slategrey","snow",
  "springgreen","steelblue","tan","teal","thistle","tomato","turquoise","violet","wheat",
  "white","whitesmoke","yellow","yellowgreen",
]);

// Properties whose values are expected to contain colors. Limits false
// positives like `font-family: "Snow Display"` accidentally matching `snow`.
const COLOR_PROPS = new Set([
  "color","background","background-color","background-image","border","border-color",
  "border-top","border-right","border-bottom","border-left","border-top-color",
  "border-right-color","border-bottom-color","border-left-color","outline","outline-color",
  "box-shadow","text-shadow","fill","stroke","caret-color","column-rule","column-rule-color",
  "text-decoration","text-decoration-color","accent-color","scrollbar-color","-webkit-text-fill-color",
]);

function stripComments(css) {
  // Replace comment bodies with spaces of the same length so line numbers stay stable.
  return css.replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, " "));
}

function lineOf(text, idx) {
  let line = 1;
  for (let i = 0; i < idx; i++) if (text.charCodeAt(i) === 10) line++;
  return line;
}

// Walk a CSS source and yield rule bodies with their full selector chain.
// `selectorChain` is an array; outer at-rules contribute their preamble
// (e.g. `@media (max-width: 760px)`), normal selectors contribute themselves.
function* iterRules(css) {
  const stack = []; // entries: { selector, bodyStart }
  let buf = "";
  let bufStart = 0;
  let i = 0;
  while (i < css.length) {
    const ch = css[i];
    if (ch === "{") {
      const sel = buf.trim();
      buf = "";
      bufStart = i + 1;
      stack.push({ selector: sel, bodyStart: i + 1 });
      i++;
      continue;
    }
    if (ch === "}") {
      const frame = stack.pop();
      if (frame) {
        const body = css.slice(frame.bodyStart, i);
        // Only yield rules whose body has no further nested `{}` —
        // those are "leaf" rule bodies that can contain declarations.
        if (!body.includes("{")) {
          const chain = stack.map((s) => s.selector);
          chain.push(frame.selector);
          yield { chain, body, bodyStart: frame.bodyStart };
        }
      }
      buf = "";
      bufStart = i + 1;
      i++;
      continue;
    }
    if (buf === "" && (ch === " " || ch === "\t" || ch === "\n" || ch === "\r" || ch === ";")) {
      // Skip leading whitespace and stray semicolons at top level between rules.
      bufStart = i + 1;
      i++;
      continue;
    }
    buf += ch;
    i++;
  }
}

// Parse a rule body into declarations. Returns array of { prop, value, valueOffset }
// where valueOffset is the offset of the value within the body.
function parseDecls(body) {
  const decls = [];
  // Split on `;` at depth 0 (outside parens). CSS doesn't have `{}` inside a
  // declaration value so this is safe.
  let depth = 0;
  let start = 0;
  for (let i = 0; i <= body.length; i++) {
    const ch = body[i];
    if (ch === "(") depth++;
    else if (ch === ")") depth--;
    if (i === body.length || (ch === ";" && depth === 0)) {
      const chunk = body.slice(start, i);
      const colonIdx = chunk.indexOf(":");
      if (colonIdx > 0) {
        const prop = chunk.slice(0, colonIdx).trim().toLowerCase();
        const valueOffset = start + colonIdx + 1;
        const value = chunk.slice(colonIdx + 1);
        if (prop && value && !prop.startsWith("--")) {
          decls.push({ prop, value, valueOffset });
        }
      }
      start = i + 1;
    }
  }
  return decls;
}

// Find color literals in a property value. Returns array of { literal, offsetInValue }.
function findColorLiterals(value) {
  const hits = [];

  // 1. Hex colors: #abc, #abcd, #aabbcc, #aabbccdd
  const hexRe = /#[0-9a-fA-F]{3,8}\b/g;
  let m;
  while ((m = hexRe.exec(value))) {
    const lit = m[0];
    const len = lit.length - 1; // minus the '#'
    if (len === 3 || len === 4 || len === 6 || len === 8) {
      hits.push({ literal: lit.toLowerCase(), offsetInValue: m.index });
    }
  }

  // 2. rgb(), rgba(), hsl(), hsla(), hwb(), lab(), lch(), oklab(), oklch(), color()
  const fnRe = /\b(rgb|rgba|hsl|hsla|hwb|lab|lch|oklab|oklch|color)\s*\([^()]*\)/gi;
  while ((m = fnRe.exec(value))) {
    // Normalize whitespace inside the literal for stable fingerprinting.
    const lit = m[0].replace(/\s+/g, " ").toLowerCase();
    hits.push({ literal: lit, offsetInValue: m.index });
  }

  // 3. Named colors. Only consider identifier-shaped tokens; skip ones inside
  // strings (font-family, content) by stripping quoted regions first.
  const cleaned = value.replace(/"[^"]*"|'[^']*'/g, (s) => " ".repeat(s.length));
  const wordRe = /\b([a-zA-Z]{3,20})\b/g;
  while ((m = wordRe.exec(cleaned))) {
    const word = m[1].toLowerCase();
    if (NAMED_COLORS.has(word)) {
      hits.push({ literal: word, offsetInValue: m.index });
    }
  }

  // Sort by offset to keep occurrence indices stable across runs.
  hits.sort((a, b) => a.offsetInValue - b.offsetInValue);
  return hits;
}

function collectFingerprints(css) {
  const stripped = stripComments(css);
  const entries = []; // { selector, prop, literal, n, line }
  const counts = new Map(); // key `${selector}\t${prop}\t${literal}` -> count

  for (const rule of iterRules(stripped)) {
    // Skip @keyframes step selectors like `from`, `to`, `50%` —
    // their declarations may carry colors but are usually animation noise;
    // still, treat them like normal rules under the @keyframes chain.
    const selector = rule.chain.map((s) => s.replace(/\s+/g, " ")).join(" >> ");
    if (!selector) continue;
    if (selector.startsWith("@") && rule.chain.length === 1) continue; // pure at-rule preamble, no decls
    const decls = parseDecls(rule.body);
    for (const decl of decls) {
      if (!COLOR_PROPS.has(decl.prop)) continue;
      const hits = findColorLiterals(decl.value);
      for (const hit of hits) {
        const key = `${selector}\t${decl.prop}\t${hit.literal}`;
        const n = counts.get(key) || 0;
        counts.set(key, n + 1);
        const absOffset = rule.bodyStart + decl.valueOffset + hit.offsetInValue;
        entries.push({
          selector,
          prop: decl.prop,
          literal: hit.literal,
          n,
          line: lineOf(stripped, absOffset),
        });
      }
    }
  }
  return entries;
}

function fingerprintOf(e) {
  return `${e.selector}\t${e.prop}\t${e.literal}\t${e.n}`;
}

function loadBaseline() {
  if (!fs.existsSync(BASELINE_PATH)) return { fingerprints: [] };
  return JSON.parse(fs.readFileSync(BASELINE_PATH, "utf8"));
}

function saveBaseline(entries) {
  // Sort for deterministic diff
  const sorted = entries
    .map((e) => ({ selector: e.selector, prop: e.prop, literal: e.literal, n: e.n }))
    .sort((a, b) =>
      a.selector.localeCompare(b.selector) ||
      a.prop.localeCompare(b.prop) ||
      a.literal.localeCompare(b.literal) ||
      a.n - b.n
    );
  fs.writeFileSync(
    BASELINE_PATH,
    JSON.stringify(
      {
        $schema: "raw-color-baseline/v1",
        description:
          "Snapshot of raw color literals tolerated in src/styles.css. Each entry is a (selector, property, literal, occurrence_index) tuple. CI fails when an entry not in this list appears in src/styles.css. Shrink this list as components migrate to semantic tokens (see docs/theme-tokens.md). Regenerate via: npm run lint:css-tokens -- --update-baseline",
        fingerprints: sorted,
      },
      null,
      2
    ) + "\n"
  );
}

function main() {
  const args = process.argv.slice(2);
  const update = args.includes("--update-baseline");

  if (!fs.existsSync(TARGET)) {
    console.error(`check-raw-colors: target not found: ${TARGET}`);
    process.exit(2);
  }
  const css = fs.readFileSync(TARGET, "utf8");
  const entries = collectFingerprints(css);

  if (update) {
    saveBaseline(entries);
    console.log(`✓ baseline updated: ${entries.length} fingerprint(s) → ${path.relative(REPO_ROOT, BASELINE_PATH)}`);
    return;
  }

  const baseline = loadBaseline();
  const baselineSet = new Set(
    (baseline.fingerprints || []).map((e) => `${e.selector}\t${e.prop}\t${e.literal}\t${e.n}`)
  );
  const currentSet = new Set(entries.map(fingerprintOf));

  const newOnes = entries.filter((e) => !baselineSet.has(fingerprintOf(e)));
  const removed = [...baselineSet].filter((fp) => !currentSet.has(fp));

  if (newOnes.length === 0 && removed.length === 0) {
    console.log(`✓ style token gate: ${entries.length} known raw color literal(s); none new.`);
    return;
  }

  if (newOnes.length > 0) {
    console.error("");
    console.error("✗ style token gate: new raw color literal(s) detected in src/styles.css.");
    console.error("");
    console.error("  Raw color literals should be replaced with semantic CSS tokens");
    console.error("  (e.g. var(--bg-panel), var(--ink-primary), var(--border-subtle)) so");
    console.error("  light and dark themes resolve from a single source of truth.");
    console.error("");
    for (const e of newOnes) {
      console.error(`  src/styles.css:${e.line}`);
      console.error(`    selector : ${e.selector}`);
      console.error(`    property : ${e.prop}`);
      console.error(`    literal  : ${e.literal}${e.n > 0 ? `  (occurrence #${e.n + 1})` : ""}`);
      console.error("");
    }
    console.error("  If this literal is truly unavoidable, add it explicitly by running:");
    console.error("    npm run lint:css-tokens -- --update-baseline");
    console.error("  and including the resulting baseline diff in your PR with justification.");
    console.error("");
  }

  if (removed.length > 0) {
    // Removals are good — they mean a literal was migrated to a token.
    // But the baseline still references them, so we ask the author to refresh it.
    console.error("✓ style token gate: detected literals removed (good!) but the baseline still");
    console.error(`  references ${removed.length} stale fingerprint(s). Please refresh the baseline:`);
    console.error("    npm run lint:css-tokens -- --update-baseline");
    console.error("");
  }

  process.exit(newOnes.length > 0 ? 1 : 0);
}

main();
