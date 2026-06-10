// Dev-only FPS-метр. Маленький моно-бейдж в левом нижнем углу, показывает текущий
// FPS. Считаем кадры через requestAnimationFrame + performance.now(); число
// обновляем НАПРЯМУЮ через ref → node.textContent, без React setState каждый кадр
// (иначе сам метр генерировал бы перерисовку и врал бы о производительности).
// Дисплей освежается не чаще ~3 раз/сек. Рендерится только в DEV или когда в
// localStorage стоит voxfps=1. rAF чистится при размонтировании.

import { useEffect, useRef } from "react";

export default function FpsMeter() {
  const ref = useRef<HTMLDivElement | null>(null);

  // Показываем метр только в dev-сборке либо по ручному флагу voxfps=1.
  const enabled =
    import.meta.env.DEV ||
    (typeof localStorage !== "undefined" &&
      localStorage.getItem("voxfps") === "1");

  useEffect(() => {
    if (!enabled) return;

    let rafId = 0;
    let frames = 0;
    let lastSample = performance.now();
    let lastShown = -1;

    const tick = (now: number) => {
      frames++;
      const elapsed = now - lastSample;
      // Освежаем показ не чаще ~3 раз/сек (каждые ~333мс).
      if (elapsed >= 333) {
        const fps = Math.round((frames * 1000) / elapsed);
        if (fps !== lastShown && ref.current) {
          ref.current.textContent = `${fps} FPS`;
          lastShown = fps;
        }
        frames = 0;
        lastSample = now;
      }
      rafId = requestAnimationFrame(tick);
    };

    rafId = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(rafId);
  }, [enabled]);

  if (!enabled) return null;
  // pointer-events:none и фиксированное позиционирование — в .fps-meter (styles.css).
  return (
    <div ref={ref} className="fps-meter" aria-hidden="true">
      — FPS
    </div>
  );
}
