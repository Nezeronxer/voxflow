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
} from "./types";

// Режим пилюли = статус бэкенда + локальные надстройки (stream/done/notice).
type PillMode = "idle" | "rec" | "stream" | "trans" | "done" | "notice";

// Желаемый размер overlay-окна под каждый режим (ЛОГИЧЕСКИЕ px): пилюля + поля
// под glow/тень; для idle/done — запас под hover-рост (80×20) и тултип сверху.
// Сообщается бэкенду командой overlay_box (реализует интегратор).
const BOX: Record<PillMode, { w: number; h: number }> = {
  idle: { w: 220, h: 80 },
  rec: { w: 140, h: 64 },
  trans: { w: 140, h: 64 },
  stream: { w: 424, h: 168 },
  done: { w: 220, h: 80 }, // как idle — меньше дёрганий окна при переходе done→idle
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
  // D: метка «оффлайн» — облако было недоступно, сработал авто-fallback на
  // локальный whisper ("stt_mode" offline=true). Сбрасывается на новой записи.
  const [offline, setOffline] = useState(false);

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
    // Полный сброс потока печати (новая диктовка / уход в покой).
    const resetTextEngine = () => {
      stopRaf();
      targetTextRef.current = "";
      committedLenRef.current = 0;
      shownFloatRef.current = 0;
      lastFrameRef.current = 0;
      setShownBoth(0);
      setTypingOnce(false);
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
      // Символов/сек: плавный пол ~48, ускоряемся при отставании (чтобы успеть за
      // речью до следующего partial-чанка), но без «вываливания» куска разом.
      const cps = Math.max(48, Math.min(160, pending * 7));
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

    const offStatus = subscribe<string>("status", (e) => {
      const v = e.payload;
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
        // Дедуп по seq порог здесь НЕ трогаем (счётчик монотонный): у новой диктовки
        // seq строго больше, её партиалы пройдут сами, а эхо прошлой отфильтруется.
        resetTextEngine();
        // Уровень прошлой записи не должен «бликовать» в новой.
        rmsRef.current = 0;
        lastLevelAtRef.current = 0;
      } else if (v === "transcribing") {
        clearDone();
        setDone(false);
        // Текстовая зона сворачивается в пилюлю со спиннером — печать дальше не нужна
        // (текст храним, чистим на idle). Бары плавно опадают к минимуму.
        stopRaf();
        setTypingOnce(false);
        kickLevel();
      } else {
        // idle. Если завершилась расшифровка — 900 мс галочка, затем мини-пилюля.
        if (prev === "transcribing") {
          clearDone();
          setDone(true);
          doneTimer.current = setTimeout(() => {
            doneTimer.current = null;
            setDone(false);
          }, 900);
        }
        resetTextEngine();
        kickLevel(); // дать спрингам опасть, цикл сам заснёт
      }
    });

    const offPartial = subscribe<PartialEvent>("partial", (e) => {
      // Дедуп: партиал старее текущей диктовки — это эхо прошлой записи (StrictMode/
      // async-гонки), игнорируем. seq константен внутри диктовки (= поколение) и строго
      // растёт между ними, поэтому "<" пропускает все партиалы текущей и режет эхо прошлой.
      const seq = e.payload?.seq;
      if (seq != null) {
        if (seq < currentSeqRef.current) return;
        currentSeqRef.current = seq;
      }
      const committed = (e.payload.committed ?? "").trim();
      const volatileTail = (e.payload.volatile ?? "").trim();
      // Фоллбэк для старого бэкенда без committed/volatile: весь text как volatile.
      const text =
        committed || volatileTail
          ? (committed + " " + volatileTail).trim()
          : (e.payload.text ?? "").trim();
      targetTextRef.current = text;
      // committed — префикс text; его длина в символах = граница «белое/серое».
      committedLenRef.current = Math.min(committed.length, text.length);
      // Хвост укоротился (whisper переписал короче) — подрезаем показанное, без скачка.
      if (shownFloatRef.current > text.length) shownFloatRef.current = text.length;
      if (shownRef.current > text.length) setShownBoth(text.length);
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

    // D: какой STT реально отработал диктовку. offline=true → облако было недоступно
    // и сработал авто-fallback на локальный whisper. Показываем ненавязчивую метку
    // «оффлайн»; сбрасывается при старте следующей записи (см. status).
    const offSttMode = subscribe<SttModeEvent>("stt_mode", (e) => {
      setOffline(e.payload?.offline === true);
    });

    unlisteners.push(offStatus, offPartial, offLevel, offNoModel, offError, offSttMode);

    return () => {
      stopRaf();
      stopScrollRaf();
      stopLevelRaf();
      if (noticeTimer.current) clearTimeout(noticeTimer.current);
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
      : done
        ? "done"
        : status === "transcribing"
          ? "trans"
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
        data-tauri-drag-region
        title={
          offline && mode === "trans"
            ? "Облако недоступно — локальный whisper"
            : undefined
        }
      >
        {/* Тултип idle-hover: всегда в DOM, виден только в .aq-idle:hover (CSS). */}
        <span className="aq-tip" aria-hidden>
          Зажмите Right Ctrl — диктовка
        </span>

        {mode === "notice" ? (
          <span className="aq-msg">{notice}</span>
        ) : mode === "done" ? (
          // Галочка done: проявляется keyframe'ом, пилюля сама ужмётся в idle через 900 мс.
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
                  <div className="aq-text" ref={scrollRef}>
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
                        title="Облако недоступно — локальный whisper"
                      >
                        офлайн
                      </span>
                    )}
                  </div>
                );
              })()
            ) : (
              // 12 баров визуализатора; высоту/прозрачность пишет rAF-цикл громкости.
              // Пока событий "level" нет (бэкенд не готов) — стоят на CSS-минимуме.
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
            )}
          </>
        ) : null}
      </div>
    </div>
  );
}
