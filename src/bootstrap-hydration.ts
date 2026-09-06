// Historical detail responses may overlap live SSE updates. Keep rows changed
// after the request began, while filling the missing historical/detail rows.
export function mergeHydratedRows<T extends { id: string }>(current: T[], incoming: T[], baseline: ReadonlyMap<string, T>): T[] {
  const rows = new Map(current.map((row) => [row.id, row]));
  for (const row of incoming) {
    const live = rows.get(row.id);
    if ((!live && !baseline.has(row.id)) || (live && live === baseline.get(row.id))) rows.set(row.id, row);
  }
  return Array.from(rows.values());
}
