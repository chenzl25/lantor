// Structural sharing for JSON-shaped presentation data. Equal subtrees stop at
// identity; changed snapshots retain unchanged rows without serializing bodies.
// Keep Maps, Sets, React elements and callbacks outside this data boundary.
export function retainEqual<T>(previous: T | undefined, next: T): T {
  if (Object.is(previous, next)) return previous as T;
  if (!previous || !next || typeof previous !== "object" || typeof next !== "object") return next;
  const array = Array.isArray(next);
  if (array !== Array.isArray(previous)) return next;
  const before = previous as Record<string, unknown>;
  const after = next as Record<string, unknown>;
  const keys = Object.keys(after);
  let equal = Object.keys(before).length === keys.length;
  const shared = (array ? [] : {}) as Record<string, unknown>;
  for (const key of keys) {
    const value = retainEqual(before[key], after[key]);
    Object.defineProperty(shared, key, { value, enumerable: true, configurable: true, writable: true });
    if (!Object.prototype.hasOwnProperty.call(before, key) || !Object.is(value, before[key])) equal = false;
  }
  return equal ? previous : shared as T;
}
