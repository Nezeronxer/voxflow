export type CaptureStateSetter = (active: boolean) => Promise<void>;

/**
 * Keep native capture transitions in call order. Tauri invokes are asynchronous,
 * so fire-and-forget `true`, `true`, `false` calls may otherwise complete as
 * `true`, `false`, `true` and leave the global hotkey listener paused.
 */
export function createSerializedCaptureSetter(
  setActive: CaptureStateSetter,
): CaptureStateSetter {
  let chain = Promise.resolve();
  return (active) => {
    chain = chain.catch(() => undefined).then(() => setActive(active));
    return chain;
  };
}
