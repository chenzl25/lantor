const XHTML_NS = "http://www.w3.org/1999/xhtml";
const CANVAS_PADDING_X = 48;
const CANVAS_PADDING_Y = 36;
const CONTENT_BOTTOM_SPACER_HEIGHT = 16;

function escapeXml(value: string) {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function fileSafeName(value: string) {
  return value
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 80) || "thread";
}

function collectDocumentCss() {
  const chunks: string[] = [];
  for (const sheet of Array.from(document.styleSheets)) {
    try {
      for (const rule of Array.from(sheet.cssRules)) {
        chunks.push(rule.cssText);
      }
    } catch {
      // Ignore cross-origin sheets. Lantor's own Vite CSS is same-origin.
    }
  }
  return chunks.join("\n");
}

function makeScrollableContentVisible(clone: HTMLElement, panelWidth: number) {
  clone.style.width = `${panelWidth}px`;
  clone.style.minWidth = `${panelWidth}px`;
  clone.style.maxWidth = `${panelWidth}px`;
  clone.style.margin = "0";
  clone.style.position = "relative";
  clone.style.inset = "auto";
  clone.style.transform = "none";
  clone.style.display = "block";
  clone.style.height = "auto";
  clone.style.maxHeight = "none";
  clone.style.minHeight = "0";
  clone.style.overflow = "visible";

  const selectors = [
    ".thread-focus",
    ".thread-scroll-shell",
    ".thread-scroll",
    ".reply-list",
    ".task-execution-timeline",
  ];
  for (const selector of selectors) {
    clone.querySelectorAll<HTMLElement>(selector).forEach((element) => {
      element.style.height = "auto";
      element.style.maxHeight = "none";
      element.style.minHeight = "0";
      element.style.overflow = "visible";
    });
  }
}

function removeTransientUi(clone: HTMLElement) {
  const selectors = [
    ".thread-resize-handle",
    ".thread-back-to-bottom",
    ".thread-progress-layer",
    ".message-hover-actions",
    ".mobile-message-save-tag",
    ".message-expand-button",
    ".reply-composer",
  ];
  for (const selector of selectors) {
    clone.querySelectorAll(selector).forEach((element) => element.remove());
  }
}

function addExportBottomSpacer(clone: HTMLElement) {
  const spacer = document.createElement("div");
  spacer.setAttribute("aria-hidden", "true");
  spacer.setAttribute("data-thread-export-spacer", "true");
  spacer.style.height = `${CONTENT_BOTTOM_SPACER_HEIGHT}px`;
  spacer.style.minHeight = `${CONTENT_BOTTOM_SPACER_HEIGHT}px`;
  spacer.style.pointerEvents = "none";
  const target = clone.querySelector<HTMLElement>(".thread-scroll") ?? clone;
  target.append(spacer);
}

function serializeThreadElement(source: HTMLElement) {
  const panelWidth = Math.max(360, Math.ceil(source.getBoundingClientRect().width));
  const clone = source.cloneNode(true) as HTMLElement;
  makeScrollableContentVisible(clone, panelWidth);
  removeTransientUi(clone);
  addExportBottomSpacer(clone);
  clone.setAttribute("data-exported-thread", "true");
  return { clone, panelWidth };
}

async function waitForRenderableAssets(source: HTMLElement) {
  const fontReady = "fonts" in document
    ? document.fonts.ready.catch(() => undefined)
    : Promise.resolve();
  const imageReady = Array.from(source.querySelectorAll("img")).map(async (image) => {
    if (image.complete && image.naturalWidth > 0) return;
    try {
      await image.decode();
    } catch {
      // Broken or cross-origin images should not block export.
    }
  });
  await Promise.all([fontReady, ...imageReady]);
}

function buildDocumentExportOverrides(panelWidth: number) {
  return `
      html,
      body {
        margin: 0;
        width: ${panelWidth}px;
        min-width: ${panelWidth}px;
        overflow: visible;
        background: var(--bg-panel);
      }
    `;
}

function buildThreadExportOverrides(panelWidth: number) {
  return `
      .thread[data-exported-thread="true"] {
        position: relative !important;
        inset: auto !important;
        grid-column: auto !important;
        grid-row: auto !important;
        width: ${panelWidth}px !important;
        min-width: ${panelWidth}px !important;
        max-width: ${panelWidth}px !important;
        height: auto !important;
        min-height: 0 !important;
        margin: 0 !important;
        overflow: visible !important;
        container-type: normal !important;
        transform: none !important;
      }

      .thread[data-exported-thread="true"] .thread-focus,
      .thread[data-exported-thread="true"] .thread-scroll-shell,
      .thread[data-exported-thread="true"] .thread-scroll,
      .thread[data-exported-thread="true"] .reply-list,
      .thread[data-exported-thread="true"] .task-execution-timeline {
        display: block !important;
        height: auto !important;
        max-height: none !important;
        min-height: 0 !important;
        overflow: visible !important;
      }

      .thread[data-exported-thread="true"] .thread-progress-layer,
      .thread[data-exported-thread="true"] .thread-back-to-bottom,
      .thread[data-exported-thread="true"] .thread-resize-handle,
      .thread[data-exported-thread="true"] .message-hover-actions,
      .thread[data-exported-thread="true"] .mobile-message-save-tag,
      .thread[data-exported-thread="true"] .message-expand-button,
      .thread[data-exported-thread="true"] .reply-composer {
        display: none !important;
      }
    `;
}

function buildExportStyles(panelWidth: number, options: { includeDocumentRules: boolean }) {
  return [
    collectDocumentCss(),
    options.includeDocumentRules ? buildDocumentExportOverrides(panelWidth) : "",
    buildThreadExportOverrides(panelWidth),
  ].join("\n");
}

function exportedDocumentMarkup(clone: HTMLElement, panelWidth: number) {
  const wrapper = document.createElement("div");
  wrapper.setAttribute("xmlns", XHTML_NS);
  wrapper.style.width = `${panelWidth}px`;
  wrapper.style.minWidth = `${panelWidth}px`;
  wrapper.style.maxWidth = `${panelWidth}px`;
  wrapper.style.margin = "0";
  wrapper.style.overflow = "visible";
  const style = document.createElement("style");
  style.textContent = buildExportStyles(panelWidth, { includeDocumentRules: true });
  wrapper.append(style, clone);
  return new XMLSerializer().serializeToString(wrapper);
}

function measureExportHeight(clone: HTMLElement, panelWidth: number) {
  const measuringHost = document.createElement("div");
  measuringHost.style.position = "fixed";
  measuringHost.style.left = "-100000px";
  measuringHost.style.top = "0";
  measuringHost.style.width = `${panelWidth}px`;
  measuringHost.style.minWidth = `${panelWidth}px`;
  measuringHost.style.overflow = "visible";
  measuringHost.style.pointerEvents = "none";
  const style = document.createElement("style");
  style.textContent = buildExportStyles(panelWidth, { includeDocumentRules: false });
  measuringHost.append(style, clone.cloneNode(true));
  document.body.appendChild(measuringHost);
  const exportedThread = measuringHost.querySelector<HTMLElement>(".thread[data-exported-thread='true']");
  const threadRect = exportedThread?.getBoundingClientRect();
  let contentBottom = threadRect?.bottom || 0;
  if (exportedThread && threadRect) {
    exportedThread.querySelectorAll<HTMLElement>("*").forEach((element) => {
      const styles = getComputedStyle(element);
      if (styles.display === "none" || styles.visibility === "hidden") return;
      const rect = element.getBoundingClientRect();
      contentBottom = Math.max(contentBottom, rect.bottom);
    });
  }
  const measuredHeight = threadRect
    ? Math.max(exportedThread?.scrollHeight || 0, exportedThread?.offsetHeight || 0, contentBottom - threadRect.top)
    : 0;
  const height = Math.max(240, Math.ceil(measuredHeight));
  measuringHost.remove();
  return height;
}

function svgCanvasFill() {
  const styles = getComputedStyle(document.body);
  return styles.backgroundColor && styles.backgroundColor !== "rgba(0, 0, 0, 0)"
    ? styles.backgroundColor
    : getComputedStyle(document.documentElement).backgroundColor || "transparent";
}

function assertValidSvg(svg: string) {
  const parsed = new DOMParser().parseFromString(svg, "image/svg+xml");
  const parserError = parsed.querySelector("parsererror");
  if (parserError) {
    throw new Error(`Thread SVG export produced invalid XML: ${parserError.textContent || "parsererror"}`);
  }
}

function downloadSvg(svg: string, surfaceLabel: string) {
  assertValidSvg(svg);
  const blob = new Blob([svg], { type: "image/svg+xml;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = `${fileSafeName(surfaceLabel)}.svg`;
  document.body.appendChild(link);
  link.click();
  link.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 0);
}

export async function downloadThreadPanelSvg(source: HTMLElement, surfaceLabel: string) {
  await waitForRenderableAssets(source);
  const { clone, panelWidth } = serializeThreadElement(source);
  const panelHeight = measureExportHeight(clone, panelWidth);
  const serializedThread = exportedDocumentMarkup(clone, panelWidth);
  const canvasWidth = panelWidth + CANVAS_PADDING_X * 2;
  const canvasHeight = panelHeight + CANVAS_PADDING_Y * 2;
  const canvasFill = escapeXml(svgCanvasFill());

  const svg = `<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="${canvasWidth}" height="${canvasHeight}" viewBox="0 0 ${canvasWidth} ${canvasHeight}" role="img" aria-label="${escapeXml(surfaceLabel)}" style="display:block;margin-inline:auto;">
  <rect width="100%" height="100%" fill="${canvasFill}"/>
  <foreignObject x="${CANVAS_PADDING_X}" y="${CANVAS_PADDING_Y}" width="${panelWidth}" height="${panelHeight}">
    ${serializedThread}
  </foreignObject>
</svg>
`;
  downloadSvg(svg, surfaceLabel);
}
