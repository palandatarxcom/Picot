// ABOUTME: Resolves a provisional sidebar row's runtime session id against the
// ABOUTME: persisted JSONL paths whose file names carry that same Pi session id.

/**
 * A provisional (unpersisted) row is keyed by the runtime session id forwarded
 * through the route, while a scanned row is keyed by the absolute JSONL path.
 * Those two identities never compare equal, so the placeholder has to be
 * matched through the one artifact that carries the runtime id verbatim: Pi
 * names each session file `<iso-timestamp>_<session-id>.jsonl`.
 *
 * `filePath` must be a path-shaped scanned value (a mirrored `filePath`-keyed
 * placeholder is already compared verbatim by the caller) and `sessionId` must
 * be a bare runtime id — a path-shaped key is not an id and never matches.
 */
export function provisionalMatchesPersistedFile(filePath, sessionId) {
  if (typeof filePath !== "string" || typeof sessionId !== "string") return false;
  const id = sessionId.trim();
  if (!id || id.includes("/") || id.includes("\\")) return false;
  const name = filePath.split(/[\\/]/).pop() || "";
  return name === `${id}.jsonl` || name.endsWith(`_${id}.jsonl`);
}
