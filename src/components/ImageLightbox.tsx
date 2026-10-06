import { type ReactNode, useEffect, useRef, useState } from "react";
import { X, ZoomIn, ZoomOut } from "lucide-react";
import { DialogSurface } from "./DialogSurface";
import { useEventCallback } from "../hooks/useEventCallback";
import {
  FIT_VIEW, clampView, isZoomed, maxZoomScale, panView, pinchView, toggleZoomScale, wheelPixels, wheelZoomFactor, zoomAt,
  type ZoomFrame, type ZoomView,
} from "../image-zoom";

type Props = {
  src: string;
  alt: string;
  onClose: () => void;
  // Extra buttons beside close and zoom, such as download.
  actions?: ReactNode;
};

type Point = { x: number; y: number };

// WebKit reports trackpad pinches (macOS) and touch pinches (iOS) as
// gesture events; lib.dom has no type for them.
type WebKitGestureEvent = Event & { scale: number; clientX: number; clientY: number };

const midpoint = (a: Point, b: Point): Point => ({ x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 });
const distance = (a: Point, b: Point) => Math.hypot(b.x - a.x, b.y - a.y);

// Full-screen image preview. Zoom follows a trackpad pinch (ctrl+wheel in
// Chromium and Firefox, gesture events in Safari and the desktop app),
// ctrl+mouse wheel, or a two-finger touch pinch; a zoomed image pans by
// dragging or scrolling. A click or tap (or a double one) toggles between
// fitted and full size; one outside the image closes the preview.
export function ImageLightbox({ src, alt, onClose, actions }: Props) {
  const stageRef = useRef<HTMLDivElement>(null);
  const imageRef = useRef<HTMLImageElement>(null);
  const toggleRef = useRef<() => void>(() => {});
  const [zoomed, setZoomed] = useState(false);
  const close = useEventCallback(onClose);

  // A back gesture leaves the preview.
  useEffect(() => {
    window.addEventListener("popstate", close);
    return () => window.removeEventListener("popstate", close);
  }, [close]);

  useEffect(() => {
    const stage = stageRef.current!;
    const image = imageRef.current!;
    // Wheel and gesture events anywhere on the preview, including its buttons,
    // must not zoom or scroll the page underneath.
    const layer = stage.closest<HTMLElement>("[data-dialog-layer]") ?? stage;
    let view: ZoomView = FIT_VIEW;
    const pointers = new Map<number, Point>();
    // The view and pointer positions when the set of pointers last changed.
    let baseline: { view: ZoomView; points: Point[] } = { view, points: [] };
    let tap: { id: number; start: Point; slop: number } | null = null;
    // A double tap or double click zooms once instead of in and back out.
    let lastTap = { time: -Infinity, point: { x: 0, y: 0 } };
    let gesture: { scale: number } | null = null;

    function frame(): ZoomFrame | null {
      if (!image.offsetWidth || !image.offsetHeight) return null;
      return {
        stageWidth: stage.clientWidth,
        stageHeight: stage.clientHeight,
        left: image.offsetLeft,
        top: image.offsetTop,
        width: image.offsetWidth,
        height: image.offsetHeight,
        maxScale: maxZoomScale(image.naturalWidth, image.offsetWidth),
      };
    }
    function local(event: { clientX: number; clientY: number }): Point {
      const rect = stage.getBoundingClientRect();
      return { x: event.clientX - rect.left, y: event.clientY - rect.top };
    }
    function apply(next: ZoomView, animate = false) {
      view = next;
      image.style.transition = animate ? "transform 180ms ease-out" : "";
      image.style.transform = isZoomed(next) ? `translate(${next.x}px, ${next.y}px) scale(${next.scale})` : "";
      setZoomed(isZoomed(next));
    }
    function rebaseline() {
      baseline = { view, points: [...pointers.values()] };
    }
    function toggle(point: Point | null) {
      const current = frame();
      if (!current) return;
      if (isZoomed(view)) {
        apply(FIT_VIEW, true);
        return;
      }
      const focus = point ?? { x: current.left + current.width / 2, y: current.top + current.height / 2 };
      apply(zoomAt(view, toggleZoomScale(current, image.naturalWidth), focus, current), true);
    }
    toggleRef.current = () => {
      toggle(null);
      rebaseline();
    };

    function onPointerDown(event: PointerEvent) {
      if (event.pointerType === "mouse" && event.button !== 0) return;
      event.preventDefault();
      try {
        stage.setPointerCapture(event.pointerId);
      } catch {
        // Synthetic pointers cannot be captured.
      }
      const point = local(event);
      pointers.set(event.pointerId, point);
      tap = pointers.size === 1 ? { id: event.pointerId, start: point, slop: event.pointerType === "mouse" ? 4 : 10 } : null;
      rebaseline();
    }
    function onPointerMove(event: PointerEvent) {
      if (!pointers.has(event.pointerId)) return;
      const point = local(event);
      pointers.set(event.pointerId, point);
      if (tap?.id === event.pointerId && distance(tap.start, point) > tap.slop) tap = null;
      const current = frame();
      if (!current || tap) return;
      const points = [...pointers.values()];
      if (points.length >= 2 && baseline.points.length >= 2) {
        const [a0, b0] = baseline.points;
        const [a, b] = points;
        const startDistance = distance(a0, b0);
        if (startDistance < 1) return;
        apply(pinchView(baseline.view, baseline.view.scale * distance(a, b) / startDistance, midpoint(a0, b0), midpoint(a, b), current));
      } else if (points.length === 1 && baseline.points.length === 1 && isZoomed(baseline.view)) {
        stage.dataset.dragging = "";
        apply(panView(baseline.view, point.x - baseline.points[0].x, point.y - baseline.points[0].y, current));
      }
    }
    function onPointerEnd(event: PointerEvent) {
      if (!pointers.delete(event.pointerId)) return;
      const tapped = event.type === "pointerup" && tap?.id === event.pointerId;
      tap = null;
      rebaseline();
      if (pointers.size === 0) delete stage.dataset.dragging;
      if (!tapped) return;
      const point = local(event);
      if (event.timeStamp - lastTap.time < 350 && distance(lastTap.point, point) < 30) {
        lastTap.time = -Infinity;
        return;
      }
      lastTap = { time: event.timeStamp, point };
      const rect = image.getBoundingClientRect();
      const onImage = event.clientX >= rect.left && event.clientX <= rect.right
        && event.clientY >= rect.top && event.clientY <= rect.bottom;
      if (!onImage) {
        close();
        return;
      }
      toggle(point);
      rebaseline();
    }
    function onWheel(event: WheelEvent) {
      event.preventDefault();
      const current = frame();
      if (!current || pointers.size > 0) return;
      if (event.ctrlKey) {
        // Safari also reports the pinch as gesture events.
        if (gesture) return;
        apply(zoomAt(view, view.scale * wheelZoomFactor(event.deltaY, event.deltaMode), local(event), current));
      } else if (isZoomed(view)) {
        apply(panView(view, -wheelPixels(event.deltaX, event.deltaMode), -wheelPixels(event.deltaY, event.deltaMode), current));
      }
    }
    function onGestureStart(event: Event) {
      event.preventDefault();
      gesture = { scale: view.scale };
    }
    function onGestureChange(event: Event) {
      event.preventDefault();
      const current = frame();
      // On iOS the pointer events already follow the fingers.
      if (!current || !gesture || pointers.size > 0) return;
      const { scale, clientX, clientY } = event as WebKitGestureEvent;
      const focus = Number.isFinite(clientX) && Number.isFinite(clientY) ? local({ clientX, clientY })
        : { x: current.left + current.width / 2, y: current.top + current.height / 2 };
      apply(zoomAt(view, gesture.scale * scale, focus, current));
    }
    function onGestureEnd(event: Event) {
      event.preventDefault();
      gesture = null;
    }
    // The fitted box changes when the window resizes or the image finishes loading.
    function refit() {
      const current = frame();
      if (current && isZoomed(view)) apply(clampView(view, current));
      rebaseline();
    }

    stage.addEventListener("pointerdown", onPointerDown);
    stage.addEventListener("pointermove", onPointerMove);
    stage.addEventListener("pointerup", onPointerEnd);
    stage.addEventListener("pointercancel", onPointerEnd);
    layer.addEventListener("wheel", onWheel, { passive: false });
    layer.addEventListener("gesturestart", onGestureStart);
    layer.addEventListener("gesturechange", onGestureChange);
    layer.addEventListener("gestureend", onGestureEnd);
    image.addEventListener("load", refit);
    window.addEventListener("resize", refit);
    return () => {
      stage.removeEventListener("pointerdown", onPointerDown);
      stage.removeEventListener("pointermove", onPointerMove);
      stage.removeEventListener("pointerup", onPointerEnd);
      stage.removeEventListener("pointercancel", onPointerEnd);
      layer.removeEventListener("wheel", onWheel);
      layer.removeEventListener("gesturestart", onGestureStart);
      layer.removeEventListener("gesturechange", onGestureChange);
      layer.removeEventListener("gestureend", onGestureEnd);
      image.removeEventListener("load", refit);
      window.removeEventListener("resize", refit);
    };
  }, [close]);

  const zoomLabel = zoomed ? "Fit image to screen" : "View image at full size";
  return (
    <DialogSurface label="Image preview" backdropClassName="attachment-lightbox"
      className="attachment-lightbox-panel" onClose={onClose}>
      <button type="button" className="attachment-lightbox-close" aria-label="Close image preview" onClick={onClose}>
        <X size={18} />
      </button>
      <button type="button" className="attachment-lightbox-zoom" aria-label={zoomLabel} aria-pressed={zoomed}
        onClick={() => toggleRef.current()}>
        {zoomed ? <ZoomOut size={18} /> : <ZoomIn size={18} />}
      </button>
      {actions}
      <div ref={stageRef} className="attachment-lightbox-content" data-zoomed={zoomed ? "" : undefined}>
        <img ref={imageRef} src={src} alt={alt} draggable={false} />
      </div>
    </DialogSurface>
  );
}
