export type ScrollGeometry = { scrollHeight: number; clientHeight: number };

// ResizeObserver runs after layout. Cache geometry here, not in character-data
// observers or row layout effects; a token that doesn't change height needs no
// scroll work. Consumers coalesce writes in rAF without rereading the DOM.
export function observeScrollGeometry(
  viewport: HTMLElement,
  content: HTMLElement,
  onResize: (geometry: ScrollGeometry, viewportOnly: boolean) => void,
) {
  if (typeof ResizeObserver === "undefined") return () => {};
  let previous: ScrollGeometry | null = null;
  const observer = new ResizeObserver(() => {
    const geometry = { scrollHeight: viewport.scrollHeight, clientHeight: viewport.clientHeight };
    const viewportOnly = previous !== null
      && geometry.clientHeight !== previous.clientHeight
      && geometry.scrollHeight <= previous.scrollHeight;
    previous = geometry;
    onResize(geometry, viewportOnly);
  });
  observer.observe(viewport);
  observer.observe(content);
  return () => observer.disconnect();
}
