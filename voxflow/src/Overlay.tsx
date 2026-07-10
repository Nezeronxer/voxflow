// Плавающий индикатор диктовки в стиле Aqua Voice (overlay-окно, url #/overlay).
// Фон окна полностью прозрачный; пилюля по центру понизу, перетаскивается мышью
// из всей небольшой overlay-зоны. Короткий tap по самой пилюле вызывает диктовку,
// движение мышью — перенос окна.
//
// Состояния пилюли (классы aq-* в overlay.css):
//   idle   — мини-полоска 55×10, hover → 80×20 + тултип с текущей горячей клавишей;
//   rec    — 110×37: орб с градиентом и glow от громкости + 12 баров ("level");
//   stream — пришёл partial с текстом: до 400×~140, посимвольная печать (rAF);
//   trans  — пилюля scale(.96), кольцо-спиннер поверх орба;
//   final  — короткая вспышка финального preview во время transcribing, без зависания после вставки;
//   latch  — подтверждение двойного тапа: запись зафиксирована без удержания;
//   notice — краткое предупреждение (no_model / error) поверх любого состояния.
//
// Переходы геометрии — CSS-«пружина» cubic-bezier(0.22,1.2,0.36,1) 140 мс:
// анимируются transform/opacity; width/height меняются ОДИН раз на смену
// состояния (одиночный layout — допустимо). Громкость — JS-спринги на rAF,
// пишем style баров/glow напрямую через ref'ы, БЕЗ setState на кадр (60 fps).

import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { cursorPosition, getCurrentWindow, PhysicalPosition } from "@tauri-apps/api/window";
import { getSettings, IS_TAURI_RUNTIME, subscribe } from "./api";
import FpsMeter from "./components/FpsMeter";
import "./overlay.css";
import { DEFAULT_HOTKEY, normalizeOverlayScale } from "./types";
import { IS_APPLE_PLATFORM, prettyHotkey } from "./ui";
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
// "status": legacy-строка (текущий бэкенд) ЛИБО объект { status, lang }.
type StatusPayload = string | { status?: string; lang?: DetectedLang };
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
// под glow/тень; для idle — компактный запас под hover-рост (80×20) и тултип
// сверху, без слишком широкой невидимой drag-зоны вокруг полоски.
// Сообщается бэкенду командой overlay_box (реализует интегратор).
const BOX: Record<PillMode, { w: number; h: number }> = {
  idle: { w: 392, h: 88 },
  rec: { w: 452, h: 92 },
  trans: { w: 392, h: 88 },
  stream: { w: 552, h: 126 },
  latch: { w: 360, h: 92 },
  notice: { w: 480, h: 96 },
};
const FINAL_PREVIEW_HOLD_MS = 360;
const DRAG_HIT_PADDING = 6;

// Раскладка громкости по 12 барам: центр громче краёв (сглаженный «холм» Aqua).
const BAR_WEIGHTS = [0.5, 0.5, 0.7, 0.7, 1, 1, 1, 1, 0.8, 0.8, 0.6, 0.6];
const BAR_COUNT = BAR_WEIGHTS.length;
// Спринг громкости: установление ≈120 мс, лёгкий овершут (живость без дрожи).
const SPRING_K = 420;
const SPRING_C = 30;
// Высота бара: 2..22 px по кривой 2+20·v^1.5; CSS-высота фиксирована 22 px,
// анимируем transform:scaleY (компосит, без layout на кадр).
const BAR_MAX_H = 22;

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
  const [finalHold, setFinalHold] = useState(false);
  const finalHoldRef = useRef(false);
  const finalHoldTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Плавный поток ПОСИМВОЛЬНО (как у Aqua Voice). partial-тики приходят рывками раз
  // в ~700 мс целыми кусками; чтобы текст «втекал» непрерывно, а бегущий кружок-каретка
  // будто «печатал» его, проявляем не слова, а СИМВОЛЫ по одному через rAF.
  // targetText — полный текст последнего partial; committedLen — граница «стабильно/
  // изменчиво» (в символах): slice(0,committedLen) — committed (белый), остаток —
  // volatile (серый хвост). shown — сколько символов уже проявлено.
  const targetTextRef = useRef<string>("");
  const targetCharsRef = useRef<string[]>([]);
  const committedLenRef = useRef(0);
  const shownRef = useRef(0);
  const [shown, setShown] = useState(0);
  const [previewVersion, setPreviewVersion] = useState(0);
  const [typing, setTyping] = useState(false);
  // Дедуп по seq: МОНОТОННЫЙ счётчик (НЕ сбрасывается между диктовками). partial старее
  // currentSeq — это эхо прошлой записи (StrictMode/async-гонки), игнорируем. seq константен
  // внутри диктовки (= её поколение) и строго растёт между ними, поэтому монотонность и
  // принимает все партиалы текущей записи, и режет эхо прошлой без окна для «мигания».
  const currentSeqRef = useRef(-1);
  // PERF (60fps): зеркало React-стейта typing, чтобы выставлять его РОВНО один раз
  // на старте печати и один раз в конце — а не setState каждый кадр rAF.
  const typingRef = useRef(false);
  const rafRef = useRef<number | null>(null);
  // shownFloat — ДРОБНЫЙ аккумулятор показанных символов (время × скорость), shown —
  // его floor. lastFrame — метка прошлого кадра rAF для расчёта dt, чтобы темп печати
  // НЕ зависел от частоты кадров и шёл плавно (ровно в 60 fps).
  const shownFloatRef = useRef(0);
  const lastFrameRef = useRef(0);
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

  // --- Громкость ("level", ~33 мс): спринги баров + glow орба. Состояние спрингов
  // живёт в ref'ах (переживает StrictMode remount), значения пишутся в DOM напрямую.
  const rmsRef = useRef(0); // последний rms с бэкенда (0..1)
  const lastLevelAtRef = useRef(0); // performance.now() последнего "level"
  const levelSeqRef = useRef(-1); // дедуп level отдельным счётчиком (не смешиваем с partial)
  const barPosRef = useRef(new Float64Array(BAR_COUNT));
  const barVelRef = useRef(new Float64Array(BAR_COUNT));
  const glowPosRef = useRef(0);
  const glowVelRef = useRef(0);
  const barEls = useRef<(HTMLSpanElement | null)[]>(new Array(BAR_COUNT).fill(null));
  const glowEl = useRef<HTMLSpanElement | null>(null);
  const levelRafRef = useRef<number | null>(null);
  const levelLastRef = useRef(0);

  useEffect(() => {
    if (IS_TAURI_RUNTIME) return;
    const query = window.location.hash.split("?")[1] ?? "";
    const demoParams = new URLSearchParams(query);
    const demo = demoParams.get("demo");
    const timer = setTimeout(() => {
      const demoScale = Number(demoParams.get("scale"));
      if (Number.isFinite(demoScale)) {
        setOverlayScale(normalizeOverlayScale(demoScale));
      }
      if (demo === "recording" || demo === "stream") {
        statusRef.current = "recording";
        setStatus("recording");
      } else if (demo === "processing") {
        statusRef.current = "transcribing";
        setStatus("transcribing");
      } else if (demo === "error") {
        setNotice("Не удалось вставить текст");
      }
      if (demo === "stream") {
        const text = "Добавь автоматические тесты для Windows";
        const chars = Array.from(text);
        targetTextRef.current = text;
        targetCharsRef.current = chars;
        committedLenRef.current = Array.from("Добавь автоматические тесты").length;
        shownFloatRef.current = chars.length;
        shownRef.current = chars.length;
        setShown(chars.length);
        setLang("ru");
      }
    }, 60);
    return () => clearTimeout(timer);
  }, []);

  useEffect(() => {
    document.body.classList.add("overlay-body");
    const unlisteners: Array<() => void> = [];
    let alive = true;

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

    const setShownBoth = (n: number) => {
      shownRef.current = n;
      setShown(n);
      requestScroll();
    };
    // PERF: typing-стейт пишем в React ТОЛЬКО при реальной смене значения. Иначе
    // tick/kick дёргали бы setTyping каждый кадр → лишняя перерисовка React в 60 fps.
    const setTypingOnce = (v: boolean) => {
      if (typingRef.current === v) return;
      typingRef.current = v;
      setTyping(v);
    };
    const stopRaf = () => {
      if (rafRef.current != null) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = null;
      }
    };
    const clearFinalHold = () => {
      if (finalHoldTimer.current) {
        clearTimeout(finalHoldTimer.current);
        finalHoldTimer.current = null;
      }
      finalHoldRef.current = false;
      setFinalHold(false);
    };
    // Полный сброс потока печати (новая диктовка / уход в покой).
    const resetTextEngine = () => {
      stopRaf();
      targetTextRef.current = "";
      targetCharsRef.current = [];
      committedLenRef.current = 0;
      shownFloatRef.current = 0;
      lastFrameRef.current = 0;
      setShownBoth(0);
      setTypingOnce(false);
      clearFinalHold();
    };

    // Кадр потока: проявляем цель ПОСИМВОЛЬНО, ПОКАДРОВО (каждый кадр rAF ≈ 16.7 мс).
    // Скорость в символах/сек × dt = сколько символов добавить за этот кадр (дробно
    // копится в shownFloat). dt берём из реального времени → темп ровный и не зависит
    // от FPS. Каждый новый символ мягко проявляется через CSS (.aq-ch) → текст «течёт».
    const tick = (now: number) => {
      const total = targetCharsRef.current.length;
      if (shownFloatRef.current > total) shownFloatRef.current = total;
      const last = lastFrameRef.current || now;
      const dt = Math.min(64, now - last); // клампим скачок после простоя/таб-аут
      lastFrameRef.current = now;

      const pending = total - shownFloatRef.current;
      if (pending <= 0.001) {
        rafRef.current = null; // догнали — ждём следующий partial
        lastFrameRef.current = 0;
        setTypingOnce(false); // печать закончилась — один setState на смену
        return;
      }
      // Символов/сек: live-кружок должен поспевать за речью. Небольшой хвост
      // всё ещё проявляется плавно, но при большом отставании резко догоняем.
      const cps = Math.max(180, Math.min(900, pending * 18));
      shownFloatRef.current = Math.min(total, shownFloatRef.current + (cps * dt) / 1000);
      const next = Math.floor(shownFloatRef.current);
      if (next !== shownRef.current) setShownBoth(next);
      rafRef.current = requestAnimationFrame(tick);
    };
    const kick = () => {
      setTypingOnce(true); // печать началась — один setState на смену
      if (rafRef.current == null) {
        lastFrameRef.current = 0; // первый кадр после простоя не делает скачок dt
        rafRef.current = requestAnimationFrame(tick);
      }
    };

    // --- rAF-цикл громкости: спринг на каждый бар + спринг glow орба. Работает,
    // только пока есть свежие "level" или спринги не успокоились; в покое НЕ крутится
    // (нет события — бары на CSS-минимуме, без фейковой анимации). Все записи — только
    // transform/opacity (компосит), ни одного setState на кадр.
    const levelTick = (now: number) => {
      const dt = Math.min(0.064, levelLastRef.current ? (now - levelLastRef.current) / 1000 : 0.0167);
      levelLastRef.current = now;
      // «Свежо» = поток level живой (<250 мс) и идёт запись; иначе цель 0 — опадаем.
      const fresh =
        statusRef.current === "recording" && now - lastLevelAtRef.current < 250;
      const rms = fresh ? rmsRef.current : 0;
      const barPos = barPosRef.current;
      const barVel = barVelRef.current;
      let busy = false;
      // Шиммер ДЕТЕРМИНИРОВАННЫЙ: sin с фазой по индексу бара, период 500 мс,
      // амплитуда 0.08 — никакой случайности; гасится вместе с потоком level.
      const ph = (now / 500) * Math.PI * 2;
      for (let i = 0; i < BAR_COUNT; i++) {
        const shimmer = fresh ? 0.08 * Math.sin(ph + i * 0.9) : 0;
        const target = clamp01(rms * BAR_WEIGHTS[i] + shimmer);
        barVel[i] += (SPRING_K * (target - barPos[i]) - SPRING_C * barVel[i]) * dt;
        barPos[i] += barVel[i] * dt;
        if (Math.abs(target - barPos[i]) > 0.002 || Math.abs(barVel[i]) > 0.002) busy = true;
        const el = barEls.current[i];
        if (el) {
          const v = clamp01(barPos[i]);
          // Высота 2+20·v^1.5 (2..22 px) через scaleY от фиксированных 22 px.
          el.style.transform = `scaleY(${(2 + 20 * Math.pow(v, 1.5)) / BAR_MAX_H})`;
          el.style.opacity = String(0.75 + 0.25 * v);
        }
      }
      // Glow орба: радиус 0.5+5.5·log10(1+3v) px поверх орба радиусом 6.5 px.
      // Элемент glow — круг 26 px (радиус 13, градиент гаснет к 70% ≈ 9.1 px),
      // масштабируем так, чтобы видимый радиус был 6.5+g.
      glowVelRef.current += (SPRING_K * (rms - glowPosRef.current) - SPRING_C * glowVelRef.current) * dt;
      glowPosRef.current += glowVelRef.current * dt;
      if (Math.abs(rms - glowPosRef.current) > 0.002 || Math.abs(glowVelRef.current) > 0.002) busy = true;
      const g = 0.5 + 5.5 * Math.log10(1 + 3 * clamp01(glowPosRef.current));
      const gl = glowEl.current;
      if (gl) {
        gl.style.transform = `scale(${(6.5 + g) / 9.1})`;
        gl.style.opacity = String(clamp01((g - 0.5) / 3.3) * 0.85);
      }
      if (busy || fresh) {
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
    const holdFinalPreview = () => {
      if (finalHoldTimer.current) clearTimeout(finalHoldTimer.current);
      finalHoldRef.current = true;
      setFinalHold(true);
      finalHoldTimer.current = setTimeout(() => {
        finalHoldTimer.current = null;
        finalHoldRef.current = false;
        setFinalHold(false);
        if (statusRef.current === "idle") resetTextEngine();
      }, FINAL_PREVIEW_HOLD_MS);
    };

    // Применить lang из события: поля нет (undefined) — старый бэкенд, ничего
    // не меняем; null/незнакомое значение — язык не определён, бейдж прячем.
    const applyLang = (l: DetectedLang | undefined) => {
      if (l === undefined) return;
      setLang(l === "ru" || l === "en" ? l : null);
    };

    const offStatus = subscribe<StatusPayload>("status", (e) => {
      // Совместимость: текущий бэкенд шлёт строку, следующая волна МОЖЕТ слать
      // объект { status, lang } — принимаем оба варианта (см. StatusPayload).
      const p = e.payload;
      const v = typeof p === "string" ? p : p?.status;
      if (v !== "recording" && v !== "transcribing" && v !== "idle") return;
      const prev = statusRef.current;
      statusRef.current = v;
      setStatus(v);

      if (v === "recording") {
        // Новая диктовка: сбрасываем метку «оффлайн» и поток печати прошлой записи.
        setOffline(false);
        // Язык прошлой диктовки не «бликует» в новой: бейдж скрыт до первого lang.
        setLang(null);
        // Дедуп по seq порог здесь НЕ трогаем (счётчик монотонный): у новой диктовки
        // seq строго больше, её партиалы пройдут сами, а эхо прошлой отфильтруется.
        resetTextEngine();
        // Уровень прошлой записи не должен «бликовать» в новой.
        rmsRef.current = 0;
        lastLevelAtRef.current = 0;
      } else if (v === "transcribing") {
        // Финальная обработка/вставка началась: старое settled-«готово» больше
        // не должно висеть на экране. Пока backend готовит и вставляет текст,
        // показываем только явный spinner «Готовлю».
        resetTextEngine();
        kickLevel();
      } else {
        // idle. После финальной обработки не возвращаем старую текстовую плашку:
        // текст уже вставлен в целевое приложение, overlay должен свернуться.
        clearLatch();
        if (prev === "transcribing") {
          clearFinalHold();
          resetTextEngine();
        } else if (finalHoldRef.current) {
          clearFinalHold();
          resetTextEngine();
        } else {
          resetTextEngine();
        }
        kickLevel(); // дать спрингам опасть, цикл сам заснёт
      }
      // lang из самого события (если бэкенд прислал) — ПОСЛЕ сброса на recording,
      // чтобы lang, пришедший вместе со стартом записи, не был тут же затёрт.
      applyLang(typeof p === "object" && p !== null ? p.lang : undefined);
    });

    const offPartial = subscribe<PartialWithLang>("partial", (e) => {
      // Дедуп: партиал старее текущей диктовки — это эхо прошлой записи (StrictMode/
      // async-гонки), игнорируем. seq константен внутри диктовки (= поколение) и строго
      // растёт между ними, поэтому "<" пропускает все партиалы текущей и режет эхо прошлой.
      const seq = e.payload?.seq;
      if (seq != null) {
        if (seq < currentSeqRef.current) return;
        currentSeqRef.current = seq;
      }
      const isFinalPreview = e.payload?.final === true;
      const isSettledPreview = e.payload?.settled === true;
      if (statusRef.current === "transcribing" && !isFinalPreview) {
        return;
      }
      if (
        (isFinalPreview || isSettledPreview) &&
        statusRef.current !== "recording" &&
        statusRef.current !== "transcribing"
      ) {
        return;
      }
      if (!isFinalPreview && !isSettledPreview && finalHoldRef.current) {
        clearFinalHold();
      }
      // Язык от STT (опционален): обновляем после дедупа — эхо прошлой записи
      // не перетирает бейдж текущей. setState с тем же значением React гасит сам.
      applyLang(e.payload?.lang);
      const committed = (e.payload.committed ?? "").trim();
      const volatileTail = (e.payload.volatile ?? "").trim();
      // Фоллбэк для старого бэкенда без committed/volatile: весь text как volatile.
      const text =
        committed || volatileTail
          ? (committed + " " + volatileTail).trim()
          : (e.payload.text ?? "").trim();
      const chars = Array.from(text);
      const nextCommittedLen = Math.min(Array.from(committed).length, chars.length);
      const previewChanged =
        targetTextRef.current !== text ||
        committedLenRef.current !== nextCommittedLen;
      targetTextRef.current = text;
      targetCharsRef.current = chars;
      // committed — префикс text; его длина в символах = граница «белое/серое».
      committedLenRef.current = nextCommittedLen;
      if (previewChanged) setPreviewVersion((v) => v + 1);
      // Хвост укоротился (whisper переписал короче) — подрезаем показанное, без скачка.
      if (shownFloatRef.current > chars.length) shownFloatRef.current = chars.length;
      if (shownRef.current > chars.length) setShownBoth(chars.length);
      // Если ASR прислал большой новый кусок, не заставляем пользователя ждать
      // посимвольную анимацию всей фразы: держим максимум небольшой live-lag.
      if (previewChanged) {
        const maxLiveLag = isFinalPreview || isSettledPreview ? 0 : 28;
        const lag = chars.length - shownFloatRef.current;
        if (lag > maxLiveLag) {
          shownFloatRef.current = Math.max(0, chars.length - maxLiveLag);
          setShownBoth(Math.floor(shownFloatRef.current));
        }
      }
      if (isFinalPreview || isSettledPreview) holdFinalPreview();
      kick();
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
      const payload = e.payload ?? {};
      setLatchNotice({
        message: payload.message || "Режим без удержания",
        detail: payload.detail || "Двойное нажатие",
      });
      if (latchTimer.current) clearTimeout(latchTimer.current);
      latchTimer.current = setTimeout(() => {
        latchTimer.current = null;
        setLatchNotice(null);
      }, 1150);
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
      stopRaf();
      stopScrollRaf();
      stopLevelRaf();
      if (noticeTimer.current) clearTimeout(noticeTimer.current);
      if (latchTimer.current) clearTimeout(latchTimer.current);
      if (finalHoldTimer.current) clearTimeout(finalHoldTimer.current);
      for (const fn of unlisteners) fn();
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
        : finalHold && shown > 0
        ? "stream"
        : status === "transcribing"
        ? "trans"
        : status === "recording"
            ? shown > 0
              ? "stream"
              : "rec"
            : "idle";

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
    const state: DragPointer = {
      id: e.pointerId,
      x: e.clientX,
      y: e.clientY,
      t: performance.now(),
      dragging: false,
      fromPill,
      raf: null,
      applyChain: null,
    };
    pointerRef.current = state;
    e.currentTarget.setPointerCapture?.(e.pointerId);
    if (IS_APPLE_PLATFORM) {
      void cursorPosition()
        .then((cur) => {
          if (pointerRef.current === state) state.cursorStart = { x: cur.x, y: cur.y };
        })
        .catch(() => {});
    } else {
      void cursorPosition()
        .then((cur) => {
          if (pointerRef.current !== state) return;
          state.cursorStart = { x: cur.x, y: cur.y };
          if (state.dragging) scheduleManualDrag(state);
        })
        .catch(() => {});
    }
  };
  const onPillPointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    const p = pointerRef.current;
    if (!p || p.id !== e.pointerId) return;
    const dx = e.clientX - p.x;
    const dy = e.clientY - p.y;
    if (!p.dragging && Math.hypot(dx, dy) >= 4) {
      p.dragging = true;
    }
    if (p.dragging && !IS_APPLE_PLATFORM) {
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
        if (!IS_APPLE_PLATFORM) {
          await p.applyChain?.catch(() => {});
          await applyManualDrag(p, false);
        }
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
        void (p.applyChain ?? Promise.resolve()).then(() =>
          invoke("overlay_commit_position").catch(() => {}),
        );
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
          data-shown={shown}
          title={
            offline && mode === "trans"
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
            <strong>{hotkeyTip} — говорить</strong>
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
              <strong>{latchNotice?.message || "Режим без удержания"}</strong>
              <small>{latchNotice?.detail || "Двойное нажатие"}</small>
            </span>
          </span>
        ) : showOrb ? (
          <>
            {/* Орб: статичный drop-shadow по спеке + динамический glow-слой, чей
                transform/opacity пишет rAF-цикл громкости напрямую (без setState). */}
            <span className="aq-orbwrap" aria-hidden>
              <span className="aq-orb-glow" ref={glowEl} />
              <span className="aq-orb" />
              {mode === "trans" && <span className="aq-ring" />}
            </span>
            {mode === "stream" ? (
              (() => {
                const chars = targetCharsRef.current;
                const vis = Math.min(shown, chars.length);
                // Граница committed — не дальше показанного.
                const cut = Math.min(committedLenRef.current, vis);
                const committedText = chars.slice(0, cut).join("");
                const volatileText = chars.slice(cut, vis).join("");
                return (
                  <div
                    className="aq-text"
                    ref={scrollRef}
                    data-preview-version={previewVersion}
                  >
                    <span className="aq-chunk committed">{committedText}</span>
                    <span className="aq-chunk volatile">{volatileText}</span>
                    {/* Каретка-кружок — последний inline-элемент: всегда вплотную за
                        текстом и переносится вместе с ним. Пульс при печати, мигание в покое. */}
                    <span
                      className={"aq-caret " + (typing ? "is-typing" : "is-idle")}
                      aria-hidden
                    />
                    {offline && (
                      <span
                        className="aq-offline"
                        title="Облако недоступно — локальное распознавание"
                      >
                        офлайн
                      </span>
                    )}
                    {finalHold && <span className="aq-final-badge">готово</span>}
                  </div>
                );
              })()
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
