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

// Page metadata is a snapshot too: a late page must not overwrite a newer
// unread/count update received through state synchronization.
export function mergeThreadActivities<T extends { thread_root_id: string }>(current: T[], incoming: T[], baseline: ReadonlyMap<string, T>): T[] {
  const rows = new Map(current.map(row => [row.thread_root_id, row]));
  for (const row of incoming) {
    const live = rows.get(row.thread_root_id);
    if ((!live && !baseline.has(row.thread_root_id)) || live === baseline.get(row.thread_root_id)) rows.set(row.thread_root_id, row);
  }
  return Array.from(rows.values());
}
