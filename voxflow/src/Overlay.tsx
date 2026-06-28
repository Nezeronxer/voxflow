// Плавающий индикатор диктовки в стиле Aqua Voice (overlay-окно, url #/overlay).
// Фон окна полностью прозрачный; пилюля по центру понизу, перетаскивается мышью
// через data-tauri-drag-region на самой пилюле (декоративные дети получают
// pointer-events:none, чтобы целью mousedown была пилюля; текстовая зона при
// streaming — исключение: она интерактивна и drag не запускает).
//
// Состояния пилюли (классы aq-* в overlay.css):
//   idle   — мини-полоска 55×10, hover → 80×20 + тултип «Зажмите Right Ctrl»;
//   rec    — 110×37: орб с градиентом и glow от громкости + 12 баров ("level");
//   stream — пришёл partial с текстом: до 400×~140, посимвольная печать (rAF);
//   trans  — пилюля scale(.96), кольцо-спиннер поверх орба;
//   done   — галочка на 900 мс после расшифровки → плавно обратно в idle;
//   latch  — подтверждение двойного тапа: запись зафиксирована без удержания;
//   notice — краткое предупреждение (no_model / error) поверх любого состояния.
//
// Переходы геометрии — CSS-«пружина» cubic-bezier(0.22,1.2,0.36,1) 140 мс:
// анимируются transform/opacity; width/height меняются ОДИН раз на смену
// состояния (одиночный layout — допустимо). Громкость — JS-спринги на rAF,
// пишем style баров/glow напрямую через ref'ы, БЕЗ setState на кадр (60 fps).

import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { subscribe } from "./api";
import FpsMeter from "./components/FpsMeter";
import "./overlay.css";
import type {
  OverlayStatus,
  PartialEvent,
  NoModelEvent,
  SttModeEvent,
  LevelEvent,
  ErrorEvent as EngineErrorEvent,
  HotkeyLatchEvent,
} from "./types";

// Режим пилюли = статус бэкенда + локальные надстройки (stream/done/notice).
type PillMode = "idle" | "rec" | "stream" | "trans" | "done" | "latch" | "notice";

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

// Желаемый размер overlay-окна под каждый режим (ЛОГИЧЕСКИЕ px): пилюля + поля
// под glow/тень; для idle/done — запас под hover-рост (80×20) и тултип сверху.
// Сообщается бэкенду командой overlay_box (реализует интегратор).
const BOX: Record<PillMode, { w: number; h: number }> = {
  idle: { w: 220, h: 80 },
  rec: { w: 140, h: 64 },
  trans: { w: 176, h: 64 },
  stream: { w: 424, h: 168 },
  done: { w: 220, h: 80 }, // запас под короткое «Готово»
  latch: { w: 300, h: 82 },
  notice: { w: 424, h: 92 },
};

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
  // done: галочка на 900 мс после transcribing→idle; отменяется новым recording.
  const [done, setDone] = useState(false);
  const doneTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  // B3: окно настроек часто скрыто в трее, поэтому дублируем предупреждение
  // («выберите модель» / ошибка движка) в всегда-видимой пилюле (~3 c).
  const [notice, setNotice] = useState<string | null>(null);
  const noticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [latchNotice, setLatchNotice] = useState<HotkeyLatchEvent | null>(null);
  const latchTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
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
    document.body.classList.add("overlay-body");
    const unlisteners: Array<() => void> = [];

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
      const total = targetTextRef.current.length;
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

    const clearDone = () => {
      if (doneTimer.current) {
        clearTimeout(doneTimer.current);
        doneTimer.current = null;
      }
    };
    const clearLatch = () => {
      if (latchTimer.current) {
        clearTimeout(latchTimer.current);
        latchTimer.current = null;
      }
      setLatchNotice(null);
    };
    const holdFinalPreview = () => {
      clearDone();
      setDone(false);
      if (finalHoldTimer.current) clearTimeout(finalHoldTimer.current);
      finalHoldRef.current = true;
      setFinalHold(true);
      finalHoldTimer.current = setTimeout(() => {
        finalHoldTimer.current = null;
        finalHoldRef.current = false;
        setFinalHold(false);
        if (statusRef.current === "idle") resetTextEngine();
      }, 3000);
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
        // Новая диктовка: отменяем done-галочку (двойной тап не мигает галочкой),
        // сбрасываем метку «оффлайн» и поток печати прошлой записи.
        clearDone();
        setDone(false);
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
        clearDone();
        setDone(false);
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
          clearDone();
          setDone(false);
          resetTextEngine();
        } else if (finalHoldRef.current) {
          clearDone();
          setDone(false);
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
      if (statusRef.current === "transcribing") {
        return;
      }
      if ((isFinalPreview || isSettledPreview) && statusRef.current !== "recording") {
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
      const nextCommittedLen = Math.min(committed.length, text.length);
      const previewChanged =
        targetTextRef.current !== text ||
        committedLenRef.current !== nextCommittedLen;
      targetTextRef.current = text;
      // committed — префикс text; его длина в символах = граница «белое/серое».
      committedLenRef.current = nextCommittedLen;
      if (previewChanged) setPreviewVersion((v) => v + 1);
      // Хвост укоротился (whisper переписал короче) — подрезаем показанное, без скачка.
      if (shownFloatRef.current > text.length) shownFloatRef.current = text.length;
      if (shownRef.current > text.length) setShownBoth(text.length);
      // Если ASR прислал большой новый кусок, не заставляем пользователя ждать
      // посимвольную анимацию всей фразы: держим максимум небольшой live-lag.
      if (previewChanged) {
        const maxLiveLag = isFinalPreview || isSettledPreview ? 0 : 28;
        const lag = text.length - shownFloatRef.current;
        if (lag > maxLiveLag) {
          shownFloatRef.current = Math.max(0, text.length - maxLiveLag);
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

    unlisteners.push(offStatus, offPartial, offLevel, offNoModel, offError, offHotkeyLatch, offSttMode);

    return () => {
      stopRaf();
      stopScrollRaf();
      stopLevelRaf();
      if (noticeTimer.current) clearTimeout(noticeTimer.current);
      if (latchTimer.current) clearTimeout(latchTimer.current);
      if (finalHoldTimer.current) clearTimeout(finalHoldTimer.current);
      clearDone();
      for (const fn of unlisteners) fn();
    };
  }, []);

  // Режим пилюли. notice поверх всего (запись при отсутствии модели не стартует,
  // но юзера надо уведомить); done — короткое окно галочки после transcribing.
  // stream — идёт запись И уже есть проявленный текст (shown реактивен).
  const mode: PillMode =
    notice != null
      ? "notice"
      : latchNotice != null
        ? "latch"
        : status === "transcribing"
        ? "trans"
        : finalHold && shown > 0
        ? "stream"
        : done
        ? "done"
        : status === "recording"
            ? shown > 0
              ? "stream"
              : "rec"
            : "idle";

  // Сообщаем бэкенду желаемый размер окна под режим. Команду overlay_box реализует
  // интегратор; до интеграции команды нет — это НЕ ошибка, глушим оба пути отказа.
  useEffect(() => {
    const box = BOX[mode];
    try {
      void invoke("overlay_box", { w: box.w, h: box.h }).catch(() => {});
    } catch {
      /* команда ещё не существует */
    }
  }, [mode]);

  // Репорт hit-зоны: окно по умолчанию click-through, бэкенд включает мышь
  // только когда курсор внутри прямоугольника ПИЛЮЛИ (overlay_hit, CSS px
  // вьюпорта). ResizeObserver ловит смену размеров (режимы, hover-рост),
  // довесок-таймер — смену позиции после ресайза окна (rect мог сдвинуться
  // без изменения размеров пилюли). Debounce 80 мс.
  useEffect(() => {
    const el = pillRef.current;
    if (!el) return;
    let t: ReturnType<typeof setTimeout> | null = null;
    const report = () => {
      if (t) clearTimeout(t);
      t = setTimeout(() => {
        const r = el.getBoundingClientRect();
        try {
          void invoke("overlay_hit", {
            x: r.x,
            y: r.y,
            w: r.width,
            h: r.height,
          }).catch(() => {});
        } catch {
          /* команды может ещё не быть */
        }
      }, 80);
    };
    const ro = new ResizeObserver(report);
    ro.observe(el);
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
      const el = pillRef.current;
      if (!el) return;
      const r = el.getBoundingClientRect();
      try {
        void invoke("overlay_hit", { x: r.x, y: r.y, w: r.width, h: r.height }).catch(() => {});
      } catch {
        /* нет команды — не страшно */
      }
    }, 220);
    return () => clearTimeout(id);
  }, [mode]);

  // ВАЖНО: узел .aq-pill ВСЕГДА в DOM и никогда не размонтируется — все переходы
  // размеров/прозрачности идут CSS-transition по смене класса режима, а не через
  // условный рендер корня (иначе transition не сыграет и пилюля «мигнёт»).
  const showBars = mode === "rec" || mode === "trans";
  const showOrb = showBars || mode === "stream";

  return (
    <div className="aq-root">
      <FpsMeter />
      <div
        className={`aq-pill aq-${mode}`}
        ref={pillRef}
        data-tauri-drag-region
        title={
          offline && mode === "trans"
            ? "Облако недоступно — локальное распознавание"
            : undefined
        }
      >
        {/* Тултип idle-hover: всегда в DOM, виден только в .aq-idle:hover (CSS). */}
        <span className="aq-tip" aria-hidden>
          Зажмите Right Ctrl — диктовка
        </span>

        {/* Бейдж определённого языка: только пока идёт диктовка и бэкенд прислал
            lang. position:absolute в углу пилюли (см. .aq-lang) — не участвует в
            layout, поэтому overlay_box/ResizeObserver и hit-rect не меняются. */}
        {lang != null && (mode === "rec" || mode === "stream" || mode === "trans") && (
          <span className="aq-lang">{lang.toUpperCase()}</span>
        )}

        {mode === "notice" ? (
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
        ) : mode === "done" ? (
          <span className="aq-done-copy">
            <svg className="aq-check" viewBox="0 0 12 12" aria-hidden>
              <path
                d="M2.4 6.4l2.5 2.6 4.7-5.4"
                fill="none"
                stroke="#fff"
                strokeWidth="1.6"
                strokeLinecap="round"
                strokeLinejoin="round"
              />
            </svg>
            <span>Готово</span>
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
                const full = targetTextRef.current;
                const vis = Math.min(shown, full.length);
                // Граница committed — не дальше показанного.
                const cut = Math.min(committedLenRef.current, vis);
                // Каждый видимый символ — отдельный span с ключом = АБСОЛЮТНЫЙ индекс:
                // новый символ монтируется и мягко проявляется (CSS .aq-ch), уже показанные
                // не перемонтируются (не мигают), а смена класса volatile→committed даёт
                // плавный переход цвета серый→белый при фиксации. Array.from — корректно
                // по код-поинтам (кириллица ок).
                const chars = Array.from(full.slice(0, vis));
                return (
                  <div
                    className="aq-text"
                    ref={scrollRef}
                    data-preview-version={previewVersion}
                  >
                    {chars.map((ch, i) => (
                      <span
                        key={i}
                        className={i < cut ? "aq-ch committed" : "aq-ch volatile"}
                      >
                        {ch}
                      </span>
                    ))}
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
                <span className="aq-trans-copy">Готовлю</span>
              ) : (
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
              )
            )}
          </>
        ) : null}
      </div>
    </div>
  );
}
