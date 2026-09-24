// Плавающий индикатор диктовки в стиле Aqua Voice (overlay-окно, url #/overlay).
// Фон окна полностью прозрачный; пилюля по центру понизу, перетаскивается мышью
// из всей небольшой overlay-зоны. Короткий tap по самой пилюле вызывает диктовку,
// движение мышью — перенос окна.
//
// Состояния пилюли (классы aq-* в overlay.css):
//   idle   — компактная полоска с горячей клавишей и языком;
//   rec    — орб с градиентом и glow от громкости + 12 баров ("level");
//   stream — пришёл partial с текстом: до 360×82, новые слова всплывают по одному;
//   trans  — компактная пилюля с кольцом-спиннером поверх орба;
//   latch  — подтверждение двойного тапа: запись зафиксирована без удержания;
//   notice — краткое предупреждение (no_model / error) поверх любого состояния.
//
// Переходы геометрии — короткая CSS-анимация:
// анимируются transform/opacity; width/height меняются ОДИН раз на смену
// состояния (одиночный layout — допустимо). Громкость и live-текст обновляются
// через ref'ы/rAF напрямую, БЕЗ setState на кадр (60 fps).

import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { cursorPosition, getCurrentWindow, PhysicalPosition } from "@tauri-apps/api/window";
import { getSettings, IS_TAURI_RUNTIME, subscribe } from "./api";
import FpsMeter from "./components/FpsMeter";
import "./overlay.css";
import {
  previewPillMode,
  resolveOverlayPreviewEvent,
  sharedWordPrefix,
  wordTokens,
} from "./overlayPreviewState";
import { DEFAULT_HOTKEY, normalizeOverlayScale } from "./types";
import { prettyHotkey } from "./ui";
import type {
  OverlayStatus,
  PartialEvent,
  NoModelEvent,
  SttModeEvent,
  LevelEvent,
  ErrorEvent as EngineErrorEvent,
  HotkeyLatchEvent,
  Settings,
} from "./types";

// Режим пилюли = статус бэкенда + локальные надстройки (stream/notice).
type PillMode = "idle" | "rec" | "stream" | "trans" | "latch" | "notice";

// Контракт мультиязычности (RU/EN/auto): события "partial" и "status" МОГУТ
// нести опциональное поле lang: "ru" | "en" | null — язык, определённый STT.
// Бэкенд начнёт слать его следующей волной; до неё (и при lang:null) фронт
// работает как раньше — бейдж просто скрыт. types.ts правит другая волна,
// поэтому расширение типизировано локально, поверх существующих контрактов.
type DetectedLang = "ru" | "en" | null;
type PartialWithLang = PartialEvent & {
  lang?: DetectedLang;
  final?: boolean;
  settled?: boolean;
  processed?: boolean;
};
// "status": legacy-строка ЛИБО атомарный объект. seq отсекает late partial
// прошлой диктовки ещё до первого partial новой; latched не даёт кратко показать
// обычное удержание перед подтверждением двойного нажатия.
type StatusPayload =
  | string
  | {
      status?: string;
      lang?: DetectedLang;
      seq?: number;
      latched?: boolean;
    };
type DragPointer = {
  id: number;
  x: number;
  y: number;
  t: number;
  dragging: boolean;
  fromPill: boolean;
  cursorStart?: { x: number; y: number };
  raf?: number | null;
  applyChain?: Promise<void> | null;
};

// Желаемый размер overlay-окна под каждый режим (ЛОГИЧЕСКИЕ px): пилюля + поля
// под glow/тень. Цифры синхронизированы с финальным v2-каскадом overlay.css;
// небольшой запас не даёт тени/кольцу обрезаться на целом и дробном scale.
// Сообщается бэкенду командой overlay_box (реализует интегратор).
const BOX: Record<PillMode, { w: number; h: number }> = {
  idle: { w: 266, h: 60 },
  rec: { w: 260, h: 66 },
  trans: { w: 256, h: 64 },
  stream: { w: 384, h: 104 },
  latch: { w: 264, h: 66 },
  notice: { w: 356, h: 70 },
};
const DRAG_HIT_PADDING = 6;

// Раскладка громкости по 12 барам: центр громче краёв (сглаженный «холм» Aqua).
const BAR_WEIGHTS = [0.5, 0.5, 0.7, 0.7, 1, 1, 1, 1, 0.8, 0.8, 0.6, 0.6];
const BAR_COUNT = BAR_WEIGHTS.length;
// Экспоненциальное сглаживание устойчиво даже после пропущенного кадра WebView:
// атака быстрая, спад чуть мягче — волна следует голосу без рывка/«догоняния».
const LEVEL_ATTACK_S = 0.045;
const LEVEL_RELEASE_S = 0.11;
// Высота бара: 2..18 px по кривой 2+16·v^1.5; CSS-высота фиксирована 18 px,
// анимируем transform:scaleY (компосит, без layout на кадр).
const BAR_MAX_H = 18;

function compactHotkeyLabel(label: string) {
  const normalized = label.trim().toLowerCase().replace(/^(right|left)\s+/, "");
  if (normalized === "option") return "⌥";
  if (normalized === "control" || normalized === "ctrl") return "Ctrl";
  if (normalized === "command" || normalized === "cmd") return "⌘";
  if (normalized === "shift") return "Shift";
  if (normalized === "alt") return "Alt";
  if (normalized === "win") return "Win";
  return label;
}

const clamp01 = (x: number) => (x < 0 ? 0 : x > 1 ? 1 : x);

export default function Overlay() {
  const [status, setStatus] = useState<OverlayStatus>("idle");
  // Зеркало статуса для rAF-цикла громкости (без пересоздания цикла на setState).
  const statusRef = useRef<OverlayStatus>("idle");
  // B3: окно настроек часто скрыто в трее, поэтому дублируем предупреждение
  // («выберите модель» / ошибка движка) в всегда-видимой пилюле (~3 c).
  const [notice, setNotice] = useState<string | null>(null);
  const noticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [latchNotice, setLatchNotice] = useState<HotkeyLatchEvent | null>(null);
  const latchTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [hotkeyTip, setHotkeyTip] = useState(prettyHotkey(DEFAULT_HOTKEY));
  const [overlayScale, setOverlayScale] = useState(1);
  // D: метка «оффлайн» — облако было недоступно, сработал авто-fallback на
  // локальное распознавание ("stt_mode" offline=true). Сбрасывается на новой записи.
  const [offline, setOffline] = useState(false);
  // Язык текущей диктовки от бэкенда (lang в "partial"/"status"). null =
  // не определён / старый бэкенд без поля → бейдж скрыт. Сброс на новой записи.
  const [lang, setLang] = useState<DetectedLang>(null);

  // Поток ПО СЛОВАМ: partial-тики приходят раз в 220–420 мс целыми кусками, и
  // распознавание переписывает хвост. Общий префикс слов остаётся на месте,
  // изменившиеся и новые слова вставляются span'ами и всплывают CSS-анимацией
  // (.aq-w). textRef/committedLenRef — последний partial; committedLen — граница
  // «стабильно/изменчиво» в символах (белое/серое).
  const textRef = useRef("");
  const committedLenRef = useRef(0);
  const tokensRef = useRef<string[]>([]);
  const wordsHostRef = useRef<HTMLSpanElement | null>(null);
  const hasPreviewRef = useRef(false);
  const [hasPreview, setHasPreview] = useState(false);
  // Дедуп по seq: МОНОТОННЫЙ счётчик (НЕ сбрасывается между диктовками). partial старее
  // currentSeq — это эхо прошлой записи (StrictMode/async-гонки), игнорируем. seq константен
  // внутри диктовки (= её поколение) и строго растёт между ними, поэтому монотонность и
  // принимает все партиалы текущей записи, и режет эхо прошлой без окна для «мигания».
  const currentSeqRef = useRef(-1);
  // После принятого final обычный partial того же seq уже не имеет права менять
  // кружок: это может быть только запоздавший результат детачнутого live-worker.
  const finalSeqRef = useRef(-1);
  // Скролл-контейнер: держим показанным «хвост» (последнее надиктованное).
  const scrollRef = useRef<HTMLDivElement>(null);
  // Корневой узел пилюли — для замеров hit-rect (см. sendHit ниже).
  const pillRef = useRef<HTMLDivElement>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  const pointerRef = useRef<DragPointer | null>(null);
  // D (FPS): автоскролл хвоста — НЕ в useEffect([shown]) (там запись scrollTop на
  // КАЖДЫЙ символ форсит синхронный reflow). Вместо этого ставим флаг и сбрасываем
  // его одним rAF-тиком (≤1 запись scrollTop за кадр), коалесцируя пачку символов.
  const needScrollRef = useRef(false);
  const scrollRafRef = useRef<number | null>(null);

  // --- Громкость ("level", ~33 мс): сглаженные бары + glow орба. Анимационное
  // состояние живёт в ref'ах, значения пишутся в DOM напрямую.
  const rmsRef = useRef(0); // последний rms с бэкенда (0..1)
  const lastLevelAtRef = useRef(0); // performance.now() последнего "level"
  const levelSeqRef = useRef(-1); // дедуп level отдельным счётчиком (не смешиваем с partial)
  const barPosRef = useRef(new Float64Array(BAR_COUNT));
  const glowPosRef = useRef(0);
  const barEls = useRef<(HTMLSpanElement | null)[]>(new Array(BAR_COUNT).fill(null));
  const glowEl = useRef<HTMLSpanElement | null>(null);
  const orbEl = useRef<HTMLSpanElement | null>(null);
  const levelRafRef = useRef<number | null>(null);
  const levelLastRef = useRef(0);
  const reducedMotionRef = useRef(false);

  // Перерисовать поток слов в DOM напрямую (без React-рендера на каждый partial).
  // Совпавшие слова остаются теми же узлами — не мигают; с первого отличия
  // хвост пересоздаётся, и новые span'ы всплывают CSS-анимацией.
  const paintWords = () => {
    const tokens = wordTokens(textRef.current);
    const host = wordsHostRef.current;
    if (host) {
      const same = Math.min(
        sharedWordPrefix(tokensRef.current, tokens),
        host.childNodes.length,
      );
      while (host.childNodes.length > same) host.lastChild?.remove();
      let offset = 0;
      tokens.forEach((token, idx) => {
        const cls =
          "aq-w " + (offset < committedLenRef.current ? "committed" : "volatile");
        offset += Array.from(token).length;
        if (idx < same) {
          const el = host.childNodes[idx] as HTMLSpanElement;
          if (el.textContent !== token) el.textContent = token;
          if (el.className !== cls) el.className = cls;
          return;
        }
        const el = document.createElement("span");
        el.className = cls;
        el.textContent = token;
        host.appendChild(el);
      });
      tokensRef.current = tokens;
    }
    const nextHasPreview = tokens.length > 0;
    if (hasPreviewRef.current !== nextHasPreview) {
      hasPreviewRef.current = nextHasPreview;
      setHasPreview(nextHasPreview);
    }
  };

  // Стабильный ref-колбэк: inline-функция менялась бы каждый рендер, React
  // переподключал бы узел, и все слова заново всплывали бы на любой setState.
  // Узел монтируется при входе в stream — рисуем в него уже пришедший текст.
  const attachWordsHost = useRef((el: HTMLSpanElement | null) => {
    wordsHostRef.current = el;
    tokensRef.current = [];
    if (el) paintWords();
  }).current;

  useEffect(() => {
    if (IS_TAURI_RUNTIME) return;
    const query = window.location.hash.split("?")[1] ?? "";
    const demoParams = new URLSearchParams(query);
    const demo = demoParams.get("demo");
    const timer = setTimeout(() => {
      const demoScaleParam = demoParams.get("scale");
      const demoScale = Number(demoScaleParam);
      if (demoScaleParam !== null && Number.isFinite(demoScale)) {
        setOverlayScale(normalizeOverlayScale(demoScale));
      }
      if (demo === "recording" || demo === "stream") {
        statusRef.current = "recording";
        setStatus("recording");
      } else if (demo === "processing" || demo === "stream-processing") {
        statusRef.current = "transcribing";
        setStatus("transcribing");
      } else if (demo === "error") {
        setNotice("Не удалось вставить текст");
      }
      if (demo === "stream" || demo === "stream-processing") {
        textRef.current = "Добавь автоматические тесты для Windows";
        committedLenRef.current = Array.from("Добавь автоматические тесты").length;
        paintWords();
        setLang("ru");
      }
    }, 60);
    return () => clearTimeout(timer);
  }, []);

  useEffect(() => {
    document.body.classList.add("overlay-body");
    const unlisteners: Array<() => void> = [];
    let alive = true;
    const motionQuery = window.matchMedia("(prefers-reduced-motion: reduce)");
    const syncMotionPreference = () => {
      reducedMotionRef.current = motionQuery.matches;
    };
    syncMotionPreference();
    motionQuery.addEventListener("change", syncMotionPreference);

    getSettings().then((s) => {
      if (!alive) return;
      setHotkeyTip(prettyHotkey(s.hotkey));
      setOverlayScale(normalizeOverlayScale(s.overlay_scale));
    });

    const offSettings = subscribe<Settings>("settings_changed", (e) => {
      const hotkey = e.payload?.hotkey;
      if (typeof hotkey === "string") setHotkeyTip(prettyHotkey(hotkey));
      setOverlayScale(normalizeOverlayScale(e.payload?.overlay_scale));
    });

    // Один rAF-тик автоскролла: пишем scrollTop максимум раз в кадр, даже если за
    // кадр проявилось несколько символов. Так forced layout случается ≤60 раз/сек,
    // а не на каждый символ. Хвост всё равно остаётся видимым (scroll-behavior:smooth).
    const requestScroll = () => {
      needScrollRef.current = true;
      if (scrollRafRef.current != null) return;
      scrollRafRef.current = requestAnimationFrame(() => {
        scrollRafRef.current = null;
        if (!needScrollRef.current) return;
        needScrollRef.current = false;
        const el = scrollRef.current;
        if (el) el.scrollTop = el.scrollHeight;
      });
    };
    const stopScrollRaf = () => {
      if (scrollRafRef.current != null) {
        cancelAnimationFrame(scrollRafRef.current);
        scrollRafRef.current = null;
      }
      needScrollRef.current = false;
    };

    const showText = (text: string, committedLen: number) => {
      textRef.current = text;
      committedLenRef.current = committedLen;
      paintWords();
      requestScroll();
    };
    // Полный сброс потока (новая диктовка / уход в покой).
    const resetTextEngine = () => showText("", 0);

    // --- rAF-цикл громкости: устойчивое экспоненциальное сглаживание баров/glow.
    // Работает,
    // только пока есть свежие "level" или уровни не успокоились; в покое НЕ крутится
    // (нет события — бары на CSS-минимуме, без фейковой анимации). Все записи — только
    // transform/opacity (компосит), ни одного setState на кадр.
    const levelTick = (now: number) => {
      const dt = Math.min(0.05, levelLastRef.current ? (now - levelLastRef.current) / 1000 : 0.0167);
      levelLastRef.current = now;
      // «Свежо» = поток level живой (<250 мс) и идёт запись; иначе цель 0 — опадаем.
      const fresh =
        statusRef.current === "recording" && now - lastLevelAtRef.current < 250;
      const rms = fresh ? rmsRef.current : 0;
      const barPos = barPosRef.current;
      const reducedMotion = reducedMotionRef.current;
      let busy = false;
      // Шиммер ДЕТЕРМИНИРОВАННЫЙ: sin с фазой по индексу бара, период 500 мс,
      // амплитуда 0.08 — никакой случайности; гасится вместе с потоком level.
      const ph = (now / 500) * Math.PI * 2;
      for (let i = 0; i < BAR_COUNT; i++) {
        const shimmer = fresh ? 0.08 * rms * Math.sin(ph + i * 0.9) : 0;
        const target = clamp01(rms * BAR_WEIGHTS[i] + shimmer);
        const tau = target > barPos[i] ? LEVEL_ATTACK_S : LEVEL_RELEASE_S;
        const alpha = reducedMotion ? 1 : 1 - Math.exp(-dt / tau);
        barPos[i] += (target - barPos[i]) * alpha;
        if (!reducedMotion && Math.abs(target - barPos[i]) > 0.002) busy = true;
        const el = barEls.current[i];
        if (el) {
          const v = clamp01(barPos[i]);
          // Высота 2+16·v^1.5 (2..18 px) через scaleY от фиксированных 18 px.
          el.style.transform = `scaleY(${(2 + 16 * Math.pow(v, 1.5)) / BAR_MAX_H})`;
          el.style.opacity = String(0.75 + 0.25 * v);
        }
      }
      // Glow орба: радиус 0.5+5.5·log10(1+3v) px поверх орба радиусом 6.5 px.
      // Элемент glow — круг 26 px (радиус 13, градиент гаснет к 70% ≈ 9.1 px),
      // масштабируем так, чтобы видимый радиус был 6.5+g.
      const glowTau = rms > glowPosRef.current ? LEVEL_ATTACK_S : LEVEL_RELEASE_S;
      const glowAlpha = reducedMotion ? 1 : 1 - Math.exp(-dt / glowTau);
      glowPosRef.current += (rms - glowPosRef.current) * glowAlpha;
      if (!reducedMotion && Math.abs(rms - glowPosRef.current) > 0.002) busy = true;
      const g = 0.5 + 5.5 * Math.log10(1 + 3 * clamp01(glowPosRef.current));
      const gl = glowEl.current;
      if (gl) {
        gl.style.transform = `scale(${(6.5 + g) / 9.1})`;
        gl.style.opacity = String(clamp01((g - 0.5) / 3.3) * 0.85);
      }
      // Сам орб дышит голосом (как у Aqua): тишина — 1×, громко — до 1.45×.
      const orb = orbEl.current;
      if (orb) {
        const v = reducedMotion ? 0 : clamp01(glowPosRef.current);
        orb.style.transform = `scale(${1 + 0.45 * Math.sqrt(v)})`;
      }
      if (!reducedMotion && (busy || fresh)) {
        levelRafRef.current = requestAnimationFrame(levelTick);
      } else {
        levelRafRef.current = null; // успокоились — спим до следующего "level"
        levelLastRef.current = 0;
      }
    };
    const kickLevel = () => {
      if (levelRafRef.current == null) {
        levelLastRef.current = 0;
        levelRafRef.current = requestAnimationFrame(levelTick);
      }
    };
    const stopLevelRaf = () => {
      if (levelRafRef.current != null) {
        cancelAnimationFrame(levelRafRef.current);
        levelRafRef.current = null;
      }
      levelLastRef.current = 0;
    };

    const clearLatch = () => {
      if (latchTimer.current) {
        clearTimeout(latchTimer.current);
        latchTimer.current = null;
      }
      setLatchNotice(null);
    };
    const showLatch = (payload?: HotkeyLatchEvent) => {
      setLatchNotice({
        message: payload?.message || "Без удержания",
        detail: payload?.detail || "Двойное нажатие",
      });
      if (latchTimer.current) clearTimeout(latchTimer.current);
      latchTimer.current = setTimeout(() => {
        latchTimer.current = null;
        setLatchNotice(null);
      }, 1150);
    };
    // Применить lang из события: поля нет (undefined) — старый бэкенд, ничего
    // не меняем; null/незнакомое значение — язык не определён, бейдж прячем.
    const applyLang = (l: DetectedLang | undefined) => {
      if (l === undefined) return;
      setLang(l === "ru" || l === "en" ? l : null);
    };

    const offStatus = subscribe<StatusPayload>("status", (e) => {
      // Совместимость: текущий бэкенд шлёт строку, следующая волна МОЖЕТ слать
      // объект { status, lang, seq?, latched? } — принимаем оба варианта.
      const p = e.payload;
      const v = typeof p === "string" ? p : p?.status;
      if (v !== "recording" && v !== "transcribing" && v !== "idle") return;
      if (typeof p === "object" && p !== null && Number.isFinite(p.seq)) {
        const seq = p.seq as number;
        if (seq > currentSeqRef.current) currentSeqRef.current = seq;
        if (seq > levelSeqRef.current) levelSeqRef.current = seq;
      }
      if (v === "recording" && typeof p === "object" && p?.latched === true) {
        showLatch();
      }
      const prev = statusRef.current;
      statusRef.current = v;
      if (prev !== v) setStatus(v);

      if (v === "recording") {
        // Повторный status=recording внутри одной диктовки не имеет права стирать
        // уже показанный partial. Полный сброс делаем только на реальном входе.
        if (prev !== "recording") {
          setOffline(false);
          setLang(null);
          // Дедуп по seq порог здесь НЕ трогаем (счётчик монотонный): у новой
          // диктовки seq строго больше, а эхо прошлой отфильтруется.
          resetTextEngine();
          rmsRef.current = 0;
          lastLevelAtRef.current = 0;
        }
      } else if (v === "transcribing") {
        // Не стираем live-preview во время финального распознавания: пользователь
        // продолжает видеть сказанное, а кольцо на орбе показывает обработку.
        kickLevel();
      } else {
        // Готовый текст в плашке не показываем: после вставки она сворачивается.
        clearLatch();
        resetTextEngine();
        kickLevel(); // дать уровням опасть, цикл сам заснёт
      }
      // lang из самого события (если бэкенд прислал) — ПОСЛЕ сброса на recording,
      // чтобы lang, пришедший вместе со стартом записи, не был тут же затёрт.
      applyLang(typeof p === "object" && p !== null ? p.lang : undefined);
    });

    const offPartial = subscribe<PartialWithLang>("partial", (e) => {
      const resolved = resolveOverlayPreviewEvent(
        statusRef.current,
        currentSeqRef.current,
        finalSeqRef.current,
        e.payload,
      );
      currentSeqRef.current = resolved.currentSeq;
      finalSeqRef.current = resolved.finalSeq;
      const preview = resolved.preview;
      // Финал (вставленный текст) в плашке не показываем — только то, что слышно.
      if (preview == null || preview.isFinal) return;
      // Язык от STT (опционален): обновляем после дедупа — эхо прошлой записи
      // не перетирает бейдж текущей. setState с тем же значением React гасит сам.
      applyLang(e.payload?.lang);
      if (textRef.current === preview.text && committedLenRef.current === preview.committedLen) {
        return;
      }
      showText(preview.text, preview.committedLen);
    });

    // Громкость микрофона (~33 мс при записи). Дедуп отдельным счётчиком: level
    // старее текущей диктовки (эхо прошлой) — игнорируем, бары не дёргаются в покое.
    const offLevel = subscribe<LevelEvent>("level", (e) => {
      const rms = e.payload?.rms;
      if (typeof rms !== "number" || !isFinite(rms)) return;
      const seq = e.payload?.seq;
      if (seq != null) {
        if (seq < levelSeqRef.current) return;
        levelSeqRef.current = seq;
      }
      rmsRef.current = clamp01(rms);
      lastLevelAtRef.current = performance.now();
      kickLevel();
    });

    const offNoModel = subscribe<NoModelEvent>("no_model", (e) => {
      const msg = e.payload?.message || "Выберите модель";
      setNotice(msg);
      if (noticeTimer.current) clearTimeout(noticeTimer.current);
      noticeTimer.current = setTimeout(() => setNotice(null), 3000);
    });

    // Общая ошибка движка (микрофон/сервер/прочее) — показываем кратко в пилюле.
    const offError = subscribe<EngineErrorEvent>("error", (e) => {
      const msg = e.payload?.message || "Ошибка движка";
      setNotice(msg);
      if (noticeTimer.current) clearTimeout(noticeTimer.current);
      noticeTimer.current = setTimeout(() => setNotice(null), 3000);
    });

    const offHotkeyLatch = subscribe<HotkeyLatchEvent>("hotkey_latch", (e) => {
      showLatch(e.payload);
    });

    // D: какой STT реально отработал диктовку. offline=true → облако было недоступно
    // и сработал авто-fallback на локальное распознавание. Показываем ненавязчивую метку
    // «оффлайн»; сбрасывается при старте следующей записи (см. status).
    const offSttMode = subscribe<SttModeEvent>("stt_mode", (e) => {
      setOffline(e.payload?.offline === true);
    });

    unlisteners.push(
      offSettings,
      offStatus,
      offPartial,
      offLevel,
      offNoModel,
      offError,
      offHotkeyLatch,
      offSttMode,
    );

    return () => {
      alive = false;
      stopScrollRaf();
      stopLevelRaf();
      if (noticeTimer.current) clearTimeout(noticeTimer.current);
      if (latchTimer.current) clearTimeout(latchTimer.current);
      for (const fn of unlisteners) fn();
      motionQuery.removeEventListener("change", syncMotionPreference);
    };
  }, []);

  // Режим пилюли. notice поверх всего (запись при отсутствии модели не стартует,
  // но юзера надо уведомить). stream — идёт запись И уже есть проявленный текст
  // или пришёл короткий финальный preview во время transcribing.
  const mode: PillMode =
    notice != null
      ? "notice"
      : latchNotice != null
        ? "latch"
        : previewPillMode(status, hasPreview, false);
  const compactHotkeyTip = compactHotkeyLabel(hotkeyTip);

  const pillHitRect = () => {
    const rect = pillRef.current?.getBoundingClientRect();
    if (!rect) return null;
    const pad = DRAG_HIT_PADDING;
    const x = Math.max(0, rect.left - pad);
    const y = Math.max(0, rect.top - pad);
    const right = Math.min(window.innerWidth, rect.right + pad);
    const bottom = Math.min(window.innerHeight, rect.bottom + pad);
    return { x, y, w: Math.max(1, right - x), h: Math.max(1, bottom - y) };
  };
  const reportPillHit = () => {
    const hit = pillHitRect();
    if (!hit) return;
    try {
      void invoke("overlay_hit", hit).catch(() => {});
    } catch {
      /* команды может ещё не быть */
    }
  };
  const pointInPillHit = (x: number, y: number) => {
    const hit = pillHitRect();
    return !!hit && x >= hit.x && x <= hit.x + hit.w && y >= hit.y && y <= hit.y + hit.h;
  };

  // Сообщаем бэкенду желаемый размер окна под режим. Команду overlay_box реализует
  // интегратор; до интеграции команды нет — это НЕ ошибка, глушим оба пути отказа.
  useEffect(() => {
    const box = BOX[mode];
    try {
      void invoke("overlay_box", {
        w: box.w * overlayScale,
        h: box.h * overlayScale,
      }).catch(() => {});
    } catch {
      /* команда ещё не существует */
    }
  }, [mode, overlayScale]);

  // Репорт hit-зоны: окно по умолчанию click-through, бэкенд включает мышь,
  // когда курсор рядом с самой плашкой, а не внутри всего overlay-окна.
  // Так прозрачные края не ловят случайные drag/click, но полоску всё ещё
  // можно схватить без хирургической точности.
  // Короткий tap по прозрачной зоне игнорируется; диктовку запускает tap по пилюле.
  useEffect(() => {
    const rootEl = rootRef.current;
    const pillEl = pillRef.current;
    if (!rootEl || !pillEl) return;
    let t: ReturnType<typeof setTimeout> | null = null;
    const report = () => {
      if (t) clearTimeout(t);
      t = setTimeout(() => {
        reportPillHit();
      }, 80);
    };
    const ro = new ResizeObserver(report);
    ro.observe(rootEl);
    ro.observe(pillEl);
    report();
    return () => {
      ro.disconnect();
      if (t) clearTimeout(t);
    };
  }, []);

  // После смены режима окно меняет размер (overlay_box) → позиция пилюли во
  // вьюпорте съезжает при том же размере — перемеряем по таймеру за CSS-переход.
  useEffect(() => {
    const id = setTimeout(() => {
      reportPillHit();
    }, 220);
    return () => clearTimeout(id);
  }, [mode, overlayScale]);

  // ВАЖНО: узел .aq-pill ВСЕГДА в DOM и никогда не размонтируется — все переходы
  // размеров/прозрачности идут CSS-transition по смене класса режима, а не через
  // условный рендер корня (иначе transition не сыграет и пилюля «мигнёт»).
  const showBars = mode === "rec" || mode === "trans";
  const showOrb = showBars || mode === "stream";
  const isProcessing = status === "transcribing";
  const applyManualDrag = async (p: DragPointer, requireActive = true) => {
    if (!p.cursorStart) return;
    const overlayWindow = getCurrentWindow();
    const [win, cur] = await Promise.all([
      overlayWindow.outerPosition().catch(() => null),
      cursorPosition().catch(() => null),
    ]);
    if (!win || !cur || (requireActive && pointerRef.current !== p)) return;
    const x = Math.round(win.x + (cur.x - p.cursorStart.x));
    const y = Math.round(win.y + (cur.y - p.cursorStart.y));
    try {
      await overlayWindow.setPosition(new PhysicalPosition(x, y));
      // Incremental baseline: if overlay_box resized/re-anchored the window
      // between frames, the next delta starts from that current position instead
      // of restoring the stale top-left captured at pointer-down.
      p.cursorStart = { x: cur.x, y: cur.y };
    } catch {
      /* keep the previous baseline so the movement can be retried */
    }
  };
  const scheduleManualDrag = (p: DragPointer) => {
    if (p.raf != null) return;
    p.raf = requestAnimationFrame(() => {
      p.raf = null;
      const previous = p.applyChain ?? Promise.resolve();
      const current = previous.then(() => applyManualDrag(p));
      p.applyChain = current;
      void current.finally(() => {
        if (p.applyChain === current) p.applyChain = null;
      });
    });
  };
  const onPillPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    if (!pointInPillHit(e.clientX, e.clientY)) return;
    const fromPill = !!pillRef.current?.contains(e.target as Node);
    const pixelRatio = window.devicePixelRatio || 1;
    const state: DragPointer = {
      id: e.pointerId,
      x: e.clientX,
      y: e.clientY,
      t: performance.now(),
      dragging: false,
      fromPill,
      // PointerEvent.screen* is synchronous CSS-screen state, while Tauri
      // window positions are physical pixels. This fallback preserves even a
      // very fast press→move→release completed before cursorPosition() IPC.
      cursorStart: { x: e.screenX * pixelRatio, y: e.screenY * pixelRatio },
      raf: null,
      applyChain: null,
    };
    pointerRef.current = state;
    e.currentTarget.setPointerCapture?.(e.pointerId);
    // Один pointer-путь для macOS и Windows. Tauri cursorPosition +
    // setPosition не требуют macOS Input Monitoring и, в отличие от
    // прежнего CGEvent-поллера, не конкурирует с pointer capture WebView.
    void cursorPosition()
      .then((cur) => {
        if (pointerRef.current !== state) return;
        // Replace the synchronous approximation only while the pointer has
        // not moved. Otherwise changing the baseline would lose early motion.
        if (!state.dragging) state.cursorStart = { x: cur.x, y: cur.y };
        if (state.dragging) scheduleManualDrag(state);
      })
      .catch(() => {});
  };
  const onPillPointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    const p = pointerRef.current;
    if (!p || p.id !== e.pointerId) return;
    const dx = e.clientX - p.x;
    const dy = e.clientY - p.y;
    if (!p.dragging && Math.hypot(dx, dy) >= 4) {
      p.dragging = true;
    }
    if (p.dragging) {
      e.preventDefault();
      scheduleManualDrag(p);
    }
  };
  const onPillPointerUp = (e: ReactPointerEvent<HTMLDivElement>) => {
    const p = pointerRef.current;
    if (!p || p.id !== e.pointerId) return;
    pointerRef.current = null;
    if (p.raf != null) {
      cancelAnimationFrame(p.raf);
      p.raf = null;
    }
    e.currentTarget.releasePointerCapture?.(e.pointerId);
    const moved = Math.hypot(e.clientX - p.x, e.clientY - p.y);
    const elapsed = performance.now() - p.t;
    const finish = async () => {
      const cursor = await cursorPosition().catch(() => null);
      const physicalMoved =
        cursor && p.cursorStart
          ? Math.hypot(cursor.x - p.cursorStart.x, cursor.y - p.cursorStart.y)
          : moved;
      if (p.dragging || physicalMoved >= 5) {
        await p.applyChain?.catch(() => {});
        await applyManualDrag(p, false);
        await invoke("overlay_commit_position").catch(() => {});
      } else if (p.fromPill && elapsed < 550) {
        await invoke("overlay_click").catch(() => {});
      }
    };
    void finish();
  };
  const onPillPointerCancel = (e: ReactPointerEvent<HTMLDivElement>) => {
    const p = pointerRef.current;
    if (p?.id === e.pointerId) {
      if (p.raf != null) cancelAnimationFrame(p.raf);
      pointerRef.current = null;
      if (p.dragging) {
        void (p.applyChain ?? Promise.resolve())
          .catch(() => {})
          .then(() => applyManualDrag(p, false))
          .then(() => invoke("overlay_commit_position").catch(() => {}));
      }
    }
  };

  return (
    <div
      className="aq-root"
      ref={rootRef}
      onPointerDown={onPillPointerDown}
      onPointerMove={onPillPointerMove}
      onPointerUp={onPillPointerUp}
      onPointerCancel={onPillPointerCancel}
    >
      <FpsMeter />
      <div
        className="aq-scale-stage"
        style={{ transform: `scale(${overlayScale})` }}
        data-scale={overlayScale.toFixed(2)}
      >
        <div
          className={`aq-pill aq-${mode}`}
          ref={pillRef}
          data-mode={mode}
          data-status={status}
          title={
            offline && isProcessing
              ? "Облако недоступно — локальное распознавание"
              : undefined
          }
        >
        {/* Тултип idle-hover: всегда в DOM, виден только в .aq-idle:hover (CSS). */}
        <span className="aq-tip" aria-hidden>
          Зажмите {hotkeyTip} — диктовка
        </span>

        {/* Бейдж определённого языка: только пока идёт диктовка и бэкенд прислал
            lang. position:absolute в углу пилюли (см. .aq-lang) — не участвует в
            layout, поэтому overlay_box/ResizeObserver и hit-rect не меняются. */}
        {lang != null && (mode === "rec" || mode === "stream" || mode === "trans") && (
          <span className="aq-lang">{lang.toUpperCase()}</span>
        )}

        {mode === "idle" ? (
          <span className="aq-idle-copy">
            <span className="aq-logo-wave" aria-hidden><i /><i /><i /><i /><i /></span>
            <strong>{compactHotkeyTip} — говорить</strong>
            <span className="aq-idle-lang">Авто</span>
          </span>
        ) : mode === "notice" ? (
          <span className="aq-msg">{notice}</span>
        ) : mode === "latch" ? (
          <span className="aq-latch-copy">
            <span className="aq-latch-mark" aria-hidden>
              2×
            </span>
            <span>
              <strong>{latchNotice?.message || "Без удержания"}</strong>
              <small>{latchNotice?.detail || "Двойное нажатие"}</small>
            </span>
          </span>
        ) : showOrb ? (
          <>
            {/* Орб: статичный drop-shadow по спеке + динамический glow-слой, чей
                transform/opacity пишет rAF-цикл громкости напрямую (без setState). */}
            <span className="aq-orbwrap" aria-hidden>
              <span className="aq-orb-glow" ref={glowEl} />
              <span className="aq-orb" ref={orbEl} />
              {isProcessing && <span className="aq-ring" />}
            </span>
            {mode === "stream" ? (
              <div className="aq-text" ref={scrollRef}>
                {/* Слова пишет paintWords напрямую; React детей не рендерит. */}
                <span ref={attachWordsHost} />
                {offline && (
                  <span
                    className="aq-offline"
                    title="Облако недоступно — локальное распознавание"
                  >
                    офлайн
                  </span>
                )}
              </div>
            ) : (
              // 12 баров визуализатора; высоту/прозрачность пишет rAF-цикл громкости.
              // Пока событий "level" нет (бэкенд не готов) — стоят на CSS-минимуме.
              mode === "trans" ? (
                <span className="aq-trans-copy">Улучшаю текст…</span>
              ) : (
                <>
                  <span className="aq-rec-copy">Слушаю</span>
                  <span className="aq-bars" aria-hidden>
                    {BAR_WEIGHTS.map((_, i) => (
                      <span
                        key={i}
                        className="aq-bar"
                        ref={(el) => {
                          barEls.current[i] = el;
                        }}
                      />
                    ))}
                  </span>
                </>
              )
            )}
          </>
        ) : null}
        </div>
      </div>
    </div>
  );
}
