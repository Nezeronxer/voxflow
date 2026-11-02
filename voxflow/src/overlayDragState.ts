export type LayerPosition = { x: number; y: number };
export type LayerGrab = {
  position: LayerPosition;
  pointer: LayerPosition;
  rect: { left: number; right: number; top: number; bottom: number };
};

// A Wayland layer surface stays fixed. Every sample is absolute relative to
// the same grab, so queued motions cannot accumulate old displacements.
export function layerDragPosition(
  grab: LayerGrab,
  pointer: LayerPosition,
  viewport: { width: number; height: number },
): LayerPosition {
  const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(Math.max(lo, hi), v));
  return {
    x: grab.position.x + clamp(pointer.x - grab.pointer.x, 8 - grab.rect.left, viewport.width - 8 - grab.rect.right),
    y: grab.position.y + clamp(pointer.y - grab.pointer.y, 8 - grab.rect.top, viewport.height - 8 - grab.rect.bottom),
  };
}
