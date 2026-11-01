// Lazily imported: Shiki and every grammar stay out of the eager bundle.
import {
  createCssVariablesTheme,
  createHighlighterCore,
  type HighlighterCore,
  type LanguageRegistration,
  type ThemedToken,
} from "shiki/core";
import { createJavaScriptRegexEngine } from "shiki/engine/javascript";
import type { HighlightLanguage } from "./code-blocks";

export type { ThemedToken };

const THEME = "lantor-code";
// Colors resolve through --shiki-* tokens in styles.css, so themes stay in CSS.
const theme = createCssVariablesTheme({ name: THEME, variablePrefix: "--shiki-", fontStyle: true });

type LanguageModule = { default: LanguageRegistration[] };
const languageModules: Record<HighlightLanguage, () => Promise<LanguageModule>> = {
  c: () => import("shiki/langs/c.mjs"),
  css: () => import("shiki/langs/css.mjs"),
  docker: () => import("shiki/langs/docker.mjs"),
  go: () => import("shiki/langs/go.mjs"),
  html: () => import("shiki/langs/html.mjs"),
  ini: () => import("shiki/langs/ini.mjs"),
  java: () => import("shiki/langs/java.mjs"),
  javascript: () => import("shiki/langs/javascript.mjs"),
  json: () => import("shiki/langs/json.mjs"),
  kotlin: () => import("shiki/langs/kotlin.mjs"),
  make: () => import("shiki/langs/make.mjs"),
  markdown: () => import("shiki/langs/markdown.mjs"),
  proto: () => import("shiki/langs/proto.mjs"),
  python: () => import("shiki/langs/python.mjs"),
  rust: () => import("shiki/langs/rust.mjs"),
  scala: () => import("shiki/langs/scala.mjs"),
  shellscript: () => import("shiki/langs/shellscript.mjs"),
  shellsession: () => import("shiki/langs/shellsession.mjs"),
  sql: () => import("shiki/langs/sql.mjs"),
  toml: () => import("shiki/langs/toml.mjs"),
  tsx: () => import("shiki/langs/tsx.mjs"),
  typescript: () => import("shiki/langs/typescript.mjs"),
  xml: () => import("shiki/langs/xml.mjs"),
  yaml: () => import("shiki/langs/yaml.mjs"),
};

let highlighter: HighlighterCore | null = null;
let highlighterReady: Promise<HighlighterCore> | null = null;
const loadedLanguages = new Set<HighlightLanguage>();
const languageLoads = new Map<HighlightLanguage, Promise<void>>();

function ensureHighlighter() {
  highlighterReady ??= createHighlighterCore({
    themes: [theme],
    langs: [],
    // Unsupported regex features degrade a token instead of failing the block.
    engine: createJavaScriptRegexEngine({ forgiving: true }),
  }).then((instance) => (highlighter = instance));
  return highlighterReady;
}

export function loadHighlightLanguage(language: HighlightLanguage): Promise<void> {
  let load = languageLoads.get(language);
  if (!load) {
    load = Promise.all([ensureHighlighter(), languageModules[language]()])
      .then(([instance, module]) => instance.loadLanguage(module.default))
      .then(() => { loadedLanguages.add(language); });
    languageLoads.set(language, load);
  }
  return load;
}

/** Returns tokens synchronously once the language is loaded, otherwise null. */
export function tokenizeLoaded(code: string, language: HighlightLanguage): ThemedToken[][] | null {
  if (!highlighter || !loadedLanguages.has(language)) return null;
  return highlighter.codeToTokensBase(code, { lang: language, theme: THEME });
}
