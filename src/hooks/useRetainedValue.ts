import { useLayoutEffect, useMemo, useRef } from "react";
import { retainEqual } from "../render-identity";

export function useRetainedValue<T>(value: T): T {
  const committed = useRef<T | undefined>(undefined);
  const retained = useMemo(() => retainEqual(committed.current, value), [value]);
  useLayoutEffect(() => { committed.current = retained; }, [retained]);
  return retained;
}
