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
