export const SECRET_FIELDS = [
  "ai_api_key",
  "oai_stt_key",
  "deepgram_key",
  "rewrite_key",
] as const;

const SECRET_FIELD_SET = new Set<string>(SECRET_FIELDS);

export function stableSerialize(value: unknown): string {
  return JSON.stringify(value, (_key, item) =>
    item && typeof item === "object" && !Array.isArray(item)
      ? Object.keys(item as Record<string, unknown>)
          .sort()
          .reduce<Record<string, unknown>>((result, key) => {
            result[key] = (item as Record<string, unknown>)[key];
            return result;
          }, {})
      : item,
  );
}

/** A renderer fingerprint never contains secrets; backend events are redacted. */
export function settingsFingerprint<T extends object>(settings: T): string {
  const comparable = { ...(settings as Record<string, unknown>) };
  for (const field of SECRET_FIELDS) {
    if (field in comparable) comparable[field] = "";
  }
  return stableSerialize(comparable);
}

/**
 * Three-way merge for tray/backend changes. Fields edited locally since the
 * last acknowledged backend snapshot win; untouched fields accept the remote
 * value. Redacted secret fields are always kept locally.
 */
export function mergeRendererSettings<T extends object>(
  base: T,
  current: T,
  incoming: T,
  keys: ReadonlyArray<string> = Object.keys(incoming),
): T {
  const baseRecord = base as Record<string, unknown>;
  const currentRecord = current as Record<string, unknown>;
  const incomingRecord = incoming as Record<string, unknown>;
  const merged = { ...currentRecord };

  for (const key of keys) {
    if (SECRET_FIELD_SET.has(key)) continue;
    if (stableSerialize(currentRecord[key]) === stableSerialize(baseRecord[key])) {
      merged[key] = incomingRecord[key];
    }
  }
  return merged as T;
}
