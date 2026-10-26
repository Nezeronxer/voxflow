import type { OverlayStatus } from "./types";

export type OverlayPreviewPayload = {
  text?: string;
  committed?: string;
  volatile?: string;
  seq?: number;
  final?: boolean;
  settled?: boolean;
};

export type ResolvedOverlayPreview = {
  text: string;
  committedLen: number;
  isFinal: boolean;
  isSettled: boolean;
  holdFinal: boolean;
};

export type OverlayPreviewResolution = {
  currentSeq: number;
  finalSeq: number;
  preview: ResolvedOverlayPreview | null;
};

export function resolveOverlayPreviewEvent(
  status: OverlayStatus,
  currentSeq: number,
  finalSeq: number,
  payload: OverlayPreviewPayload,
): OverlayPreviewResolution {
  const seq = payload.seq;
  let nextSeq = currentSeq;
  if (seq != null) {
    if (seq < currentSeq) return { currentSeq, finalSeq, preview: null };
    nextSeq = seq;
  }

  const isFinal = payload.final === true;
  const isSettled = payload.settled === true;
  // A detached worker can finish after the final event for the same generation.
  // Once finalSeq is latched, neither a live nor a settled speculative preview
  // may replace the exact string that was inserted.
  if (!isFinal && seq != null && seq === finalSeq) {
    return { currentSeq: nextSeq, finalSeq, preview: null };
  }
  if (status === "transcribing" && !isFinal && !isSettled) {
    return { currentSeq: nextSeq, finalSeq, preview: null };
  }
  const isSameSeqFinalAfterIdle =
    isFinal && status === "idle" && seq != null && seq === nextSeq;
  if (
    (isFinal || isSettled) &&
    status !== "recording" &&
    status !== "transcribing" &&
    !isSameSeqFinalAfterIdle
  ) {
    return { currentSeq: nextSeq, finalSeq, preview: null };
  }

  const committed = (payload.committed ?? "").trim();
  const volatileTail = (payload.volatile ?? "").trim();
  const combinedFallback = (committed + " " + volatileTail).trim();
  const payloadText = typeof payload.text === "string" ? payload.text : undefined;
  // A final payload carries the exact inserted string. Preserve every space
  // and line break instead of rebuilding it from the display fragments.
  const text = isFinal
    ? (payloadText ?? combinedFallback)
    : (payloadText || combinedFallback).trim();
  const chars = Array.from(text);
  const committedLen = isFinal
    ? chars.length
    : Math.min(Array.from(committed).length, chars.length);

  return {
    currentSeq: nextSeq,
    finalSeq: isFinal && seq != null ? seq : finalSeq,
    preview: {
      text,
      committedLen,
      isFinal,
      isSettled,
      holdFinal: isFinal || isSettled,
    },
  };
}

export function previewPillMode(
  status: OverlayStatus,
  hasPreview: boolean,
  finalHold: boolean,
): "idle" | "rec" | "stream" | "trans" {
  if (finalHold && hasPreview) return "stream";
  if (status === "transcribing") return hasPreview ? "stream" : "trans";
  if (status === "recording") return hasPreview ? "stream" : "rec";
  return "idle";
}

export function shouldResetFinalPreviewAfterHold(status: OverlayStatus): boolean {
  return status === "idle";
}

// Слово вместе с хвостовыми пробелами/переносом — единица анимации потока.
export function wordTokens(text: string): string[] {
  return text.match(/\S+\s*/g) ?? [];
}

// Сколько слов с начала совпало: их узлы плашка оставляет как есть, остальные
// пересоздаёт с анимацией. Хвостовой пробел не в счёт — у последнего слова он
// появляется, когда следом приходит новое.
export function sharedWordPrefix(prev: string[], next: string[]): number {
  let same = 0;
  while (
    same < prev.length &&
    same < next.length &&
    prev[same].trimEnd() === next[same].trimEnd()
  ) {
    same++;
  }
  return same;
}

// Метка ИИ-обработки из "status".ai: kind - local | cloud | off | missing.
export type OverlayAi = { kind?: string; name?: string | null };

// Итог перепишет нейросеть - сырое распознавание после отпускания клавиши
// с ним расходится, и плашка его не показывает до финала (D-064).
export function hidesRawUntilFinal(ai: OverlayAi | null | undefined): boolean {
  return ai?.kind === "local" || ai?.kind === "cloud";
}
export type OverlayMetaPart = { text: string; warn: boolean };

// Подпись капсулы «GigaAM · ИИ gemma4:e2b»: распознавание из "status".model и
// ИИ-обработка из "status".ai. model: undefined - старый бэкенд (части нет),
// null/пусто - модели нет («Нет модели», тревожно). Нет обеих частей - null.
export function overlayMetaLabel(
  model: string | null | undefined,
  ai: OverlayAi | null | undefined,
): OverlayMetaPart[] | null {
  const parts: OverlayMetaPart[] = [];
  if (model !== undefined) {
    const m = typeof model === "string" ? model.trim() : "";
    parts.push(m ? { text: m, warn: false } : { text: "Нет модели", warn: true });
  }
  const name = typeof ai?.name === "string" ? ai.name.trim() : "";
  switch (ai?.kind) {
    case "local":
      parts.push({ text: name ? `ИИ ${name}` : "ИИ локально", warn: false });
      break;
    case "cloud":
      parts.push({ text: "ИИ облако", warn: false });
      break;
    case "off":
      parts.push({ text: "ИИ выкл.", warn: false });
      break;
    case "missing":
      parts.push({ text: "ИИ не найдена", warn: true });
      break;
  }
  return parts.length ? parts : null;
}
