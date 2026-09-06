import { useCallback, useLayoutEffect, useRef } from "react";

// Event handlers keep their identity while reading the latest committed props.
// Do not use this for callbacks invoked during render.
export function useEventCallback<Args extends unknown[], Result>(callback: (...args: Args) => Result) {
  const latest = useRef(callback);
  useLayoutEffect(() => { latest.current = callback; });
  return useCallback((...args: Args) => latest.current(...args), []);
}
