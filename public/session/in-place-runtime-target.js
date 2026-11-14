// ABOUTME: Resolves the exact native runtime selected by a prepared session transition.
// ABOUTME: Keeps same-workspace session switches in the current WebView.

export function resolvePreparedRuntimeTarget(instances, prepared) {
  const workspaceId = prepared?.targetWorkspaceId;
  const sessionId = prepared?.targetSessionId;
  const instanceId = prepared?.targetInstanceId;
  if (!workspaceId || !sessionId) return null;
  // The prepare reply carries the full triple it spawned/selected: trust it
  // directly. A freshly spawned runtime is invisible to runtime_instances
  // until Pi's first get_state lands (a session-<uuid> id never matches a
  // persisted JSONL header), so the summary lookup below only serves older
  // prepare replies and would race every fresh spawn into a false "missing".
  if (typeof instanceId === "string" && instanceId) {
    return { workspaceId, sessionId, instanceId };
  }
  return (
    instances?.find(
      (instance) =>
        instance?.workspaceId === workspaceId &&
        instance?.sessionId === sessionId &&
        typeof instance?.instanceId === "string" &&
        instance.instanceId,
    ) ?? null
  );
}
