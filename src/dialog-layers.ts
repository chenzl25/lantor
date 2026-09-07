type DialogLayer = { root: HTMLElement; panel: HTMLElement; returnFocus: HTMLElement[] };
const layers: DialogLayer[] = [];
const listeners = new Set<() => void>();
const inertElements = new Map<HTMLElement, boolean>();
let bodyOverflow = "";
let pointerOpener: HTMLElement | null = null;

// Safari does not focus buttons on mouse activation. Keep the actual trigger
// for focus restoration; keyboard activation uses document.activeElement.
document.addEventListener("pointerdown", (event) => {
  pointerOpener = event.button === 0 && event.target instanceof Element
    ? event.target.closest<HTMLElement>('button, a[href], input, select, textarea, summary, [tabindex]')
    : null;
}, true);
document.addEventListener("keydown", () => { pointerOpener = null; }, true);

export function dialogOpener() {
  return pointerOpener?.isConnected && !pointerOpener.closest('[inert]')
    ? pointerOpener : document.activeElement instanceof HTMLElement ? document.activeElement : null;
}

export const activeDialog = () => layers[layers.length - 1] ?? null;
export const activeDialogPanel = () => activeDialog()?.panel ?? null;
export function subscribeDialogLayers(listener: () => void) {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

function updateLayers() {
  for (const [element, inert] of inertElements) element.inert = inert;
  inertElements.clear();
  const active = activeDialog();
  if (active) {
    for (const element of Array.from(document.body.children)) {
      if (!(element instanceof HTMLElement) || element === active.root) continue;
      inertElements.set(element, element.inert);
      element.inert = true;
    }
  }
  for (const listener of listeners) listener();
}

export function registerDialogLayer(layer: DialogLayer) {
  if (!layers.length) {
    bodyOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
  }
  layers.push(layer);
  updateLayers();
  return () => {
    const index = layers.indexOf(layer);
    if (index !== -1) layers.splice(index, 1);
    updateLayers();
    if (!layers.length) document.body.style.overflow = bodyOverflow;
  };
}
