// Small reusable presentational components and inline icons.

import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";
import { setHotkeyCaptureActive } from "./api";
import { createSerializedCaptureSetter } from "./hotkeyCapture";

/* ---------- Тема (light / dark / system) ----------
   Источник истины — Settings.theme в БД. localStorage "vf-theme" — только кэш:
   1) main.tsx применяет его ДО первого рендера (окно не мигает светлым);
   2) событие storage прилетает в СОСЕДНИЕ окна того же origin — overlay
      подхватывает смену темы без перезапуска и без правок Overlay.tsx. */

export type ThemePref = "system" | "light" | "dark";
const THEME_KEY = "vf-theme";
const DARK_MQ = "(prefers-color-scheme: dark)";

// Любое мусорное значение (старые БД, опечатки) сводим к "system".
export function normalizeTheme(v: string | null | undefined): ThemePref {
  return v === "light" || v === "dark" ? v : "system";
}

// Чтение кэша; localStorage может бросать (квота/приватный режим) — кэш необязателен.
function readThemeCache(): ThemePref {
  try {
    return normalizeTheme(localStorage.getItem(THEME_KEY));
  } catch {
    return "system";
  }
}

// "system" → фактическая тема из ОС, иначе — как сказано.
function resolveTheme(pref: ThemePref): "light" | "dark" {
  return pref === "system"
    ? window.matchMedia(DARK_MQ).matches
      ? "dark"
      : "light"
    : pref;
}

// Ставит data-theme на <html>. Светлая = базовый :root (атрибут "light" безвреден,
// тёмные переопределения матчатся только на [data-theme="dark"]).
function setThemeAttr(pref: ThemePref) {
  document.documentElement.dataset.theme = resolveTheme(pref);
}

// Применить выбор пользователя: мгновенно + обновить кэш (storage-событие
// донесёт смену до overlay-окна).
export function applyTheme(pref: ThemePref) {
  try {
    localStorage.setItem(THEME_KEY, pref);
  } catch {
    /* кэш не записался — тема всё равно применится ниже */
  }
  setThemeAttr(pref);
}

// Вызывается из main.tsx ДО ReactDOM.render: применяем кэш и подписываемся
// на смену системной темы (актуально при "system") и на storage из другого окна.
export function initTheme() {
  setThemeAttr(readThemeCache());
  // ОС переключила светлое/тёмное — реагируем только в режиме «системная».
  window.matchMedia(DARK_MQ).addEventListener("change", () => {
    if (readThemeCache() === "system") setThemeAttr("system");
  });
  // Главное окно сменило тему → overlay ловит storage и перекрашивается.
  window.addEventListener("storage", (e) => {
    if (e.key === THEME_KEY) setThemeAttr(normalizeTheme(e.newValue));
  });
}

/* ---------- Toggle switch ---------- */
export function Switch({
  checked,
  onChange,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <label className="switch">
      <input
        type="checkbox"
        checked={checked}
        onChange={(e) => onChange(e.currentTarget.checked)}
      />
      <span className="track">
        <span className="thumb" />
      </span>
    </label>
  );
}

/* ---------- Labelled settings row ---------- */
export function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: ReactNode;
}) {
  return (
    <div className="field">
      <div>
        <div className="field-label">{label}</div>
        {hint && <div className="field-hint">{hint}</div>}
      </div>
      <div className="field-control">{children}</div>
    </div>
  );
}

/* ---------- Select helper ---------- */
export function Select({
  value,
  onChange,
  options,
}: {
  value: string;
  onChange: (v: string) => void;
  options: { value: string; label: string }[];
}) {
  return (
    <select value={value} onChange={(e) => onChange(e.currentTarget.value)}>
      {options.map((o) => (
        <option key={o.value} value={o.value}>
          {o.label}
        </option>
      ))}
    </select>
  );
}

/* ---------- Hotkey capture ---------- */

// Канонические имена = KeyboardEvent.code из вебвью. Они же — строки, которые
// принимает Rust-парсер hotkey.rs::parse_key. SUPPORTED_HOTKEYS ниже — ЗЕРКАЛО
// parse_key: меняешь там — меняй и тут, иначе захваченная клавиша молча не
// сработает в глобальном хуке.
export const IS_APPLE_PLATFORM =
  typeof navigator !== "undefined" && /Mac|iPhone|iPad|iPod/.test(navigator.platform);

export const HOTKEY_FIELD_HINT = IS_APPLE_PLATFORM
  ? "Назначается одна физическая клавиша. Для удержания лучше Cmd, Option или F-клавиши."
  : "Назначается одна физическая клавиша. Для удержания лучше Ctrl, Alt или F-клавиши.";

export const HOTKEY_LABELS: Record<string, string> = {
  ControlLeft: IS_APPLE_PLATFORM ? "Left Control" : "Left Ctrl",
  ControlRight: IS_APPLE_PLATFORM ? "Right Control" : "Right Ctrl",
  ShiftLeft: "Left Shift", ShiftRight: "Right Shift",
  AltLeft: IS_APPLE_PLATFORM ? "Left Option" : "Left Alt",
  AltRight: IS_APPLE_PLATFORM ? "Right Option" : "Right Alt",
  MetaLeft: IS_APPLE_PLATFORM ? "Left Cmd" : "Left Win",
  MetaRight: IS_APPLE_PLATFORM ? "Right Cmd" : "Right Win",
  CapsLock: "Caps Lock", Insert: "Insert", ScrollLock: "Scroll Lock",
  Pause: "Pause", PrintScreen: "Print Screen", NumLock: "Num Lock",
  Enter: "Enter", Space: "Space", Tab: "Tab", Backspace: "Backspace", Delete: "Delete",
  Home: "Home", End: "End", PageUp: "Page Up", PageDown: "Page Down",
  ArrowUp: "↑", ArrowDown: "↓", ArrowLeft: "←", ArrowRight: "→",
  NumpadEnter: "Num Enter", NumpadAdd: "Num +", NumpadSubtract: "Num −",
  NumpadMultiply: "Num *", NumpadDivide: "Num /", NumpadDecimal: "Num .",
  Minus: "-", Equal: "=", BracketLeft: "[", BracketRight: "]",
  Backslash: "\\", IntlBackslash: "\\", Semicolon: ";", Quote: "'",
  Backquote: "`", Comma: ",", Period: ".", Slash: "/",
  F1: "F1", F2: "F2", F3: "F3", F4: "F4", F5: "F5", F6: "F6",
  F7: "F7", F8: "F8", F9: "F9", F10: "F10", F11: "F11", F12: "F12",
};

// Зеркало hotkey.rs::parse_key — какие event.code реально распознаёт глобальный хук.
// Любая нормальная клавиша проходит; экзотику (медиа, ContextMenu, Fn) хук не ловит —
// такую при захвате не сохраняем, а показываем подсказку (без тихого отказа).
const SUPPORTED_HOTKEYS = new Set<string>([
  "ControlLeft", "ControlRight", "ShiftLeft", "ShiftRight",
  "AltLeft", "AltRight", "MetaLeft", "MetaRight",
  "CapsLock", "NumLock",
  ...(IS_APPLE_PLATFORM ? [] : ["Insert", "ScrollLock", "Pause", "PrintScreen"]),
  "Enter", "Space", "Tab", "Backspace", "Delete",
  "Home", "End", "PageUp", "PageDown",
  "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight",
  "Minus", "Equal", "BracketLeft", "BracketRight", "Backslash", "IntlBackslash",
  "Semicolon", "Quote", "Backquote", "Comma", "Period", "Slash",
  "NumpadAdd", "NumpadSubtract", "NumpadMultiply", "NumpadDivide",
  "NumpadDecimal", "NumpadEnter",
  ...Array.from({ length: 26 }, (_, i) => "Key" + String.fromCharCode(65 + i)),
  ...Array.from({ length: 10 }, (_, i) => "Digit" + i),
  ...Array.from({ length: 10 }, (_, i) => "Numpad" + i),
  ...Array.from({ length: 12 }, (_, i) => "F" + (i + 1)),
]);

// Печатная клавиша в режиме «Удержание» будет ещё и печатать символ в активном окне.
const PRINTABLE_HOTKEY =
  /^(Key[A-Z]|Digit[0-9]|Numpad[0-9]|Minus|Equal|Bracket(Left|Right)|Backslash|IntlBackslash|Semicolon|Quote|Backquote|Comma|Period|Slash|Space|Numpad(Add|Subtract|Multiply|Divide|Decimal))$/;
export function isPrintableHotkey(code: string): boolean {
  return PRINTABLE_HOTKEY.test(code);
}

export function prettyHotkey(h: string): string {
  if (HOTKEY_LABELS[h]) return HOTKEY_LABELS[h];
  // Буквы/цифры в HOTKEY_LABELS не держим — разворачиваем кодами явно (ветки НЕ
  // через `??`, иначе обычная цифра Digit5 показалась бы как «Num 5»).
  const m = /^(?:Key([A-Z])|Digit([0-9])|Numpad([0-9]))$/.exec(h);
  if (m) {
    if (m[1]) return m[1];
    if (m[2]) return m[2];
    if (m[3]) return "Num " + m[3];
  }
  return h || "—";
}

// Поле захвата хоткея: клик → «Нажмите клавишу…» → физическое нажатие пишется в
// настройку. Заменяет прежний текстовый input, в который имя клавиши приходилось
// печатать руками (клик ставил курсор, а не ловил клавишу).
export function HotkeyCapture({
  value,
  onChange,
  exclude,
  excludeLabel,
}: {
  value: string;
  onChange: (code: string) => boolean | void | Promise<boolean | void>;
  exclude?: string;
  excludeLabel?: string;
}) {
  const [capturing, setCapturing] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const ref = useRef<HTMLButtonElement>(null);
  const activationCodeRef = useRef<string | null>(null);
  const captureCodeRef = useRef<(code: string) => void>(() => undefined);
  const savingRef = useRef(false);
  // Tauri commands may complete out of order. Serializing capture transitions
  // guarantees that a late `true` can never overwrite the final `false` and
  // leave the global listener permanently paused after reassignment.
  const nativeCaptureSetterRef = useRef<ReturnType<typeof createSerializedCaptureSetter> | null>(
    null,
  );
  if (!nativeCaptureSetterRef.current) {
    nativeCaptureSetterRef.current = createSerializedCaptureSetter(setHotkeyCaptureActive);
  }
  const setNativeCapture = nativeCaptureSetterRef.current;

  function beginCapture(activationCode: string | null = null) {
    activationCodeRef.current = activationCode;
    setError(null);
    // Стартуем нативную паузу до следующего физического нажатия. Это
    // единственный `true`: effect ниже только подписывается и снимает паузу.
    void setNativeCapture(true);
    setCapturing(true);
  }

  async function captureCode(code: string) {
    if (code === "Escape") {
      setError(null);
      setCapturing(false);
      ref.current?.blur();
      return;
    }
    if (!code) return; // мёртвые клавиши без кода
    if (!SUPPORTED_HOTKEYS.has(code)) {
      // Экзотика (медиа/ContextMenu/Fn) — глобальный хук её не поймает: не сохраняем,
      // остаёмся в захвате, чтобы пользователь нажал другую.
      setError(
        `${prettyHotkey(code)} не поддерживается глобальным хуком — нажмите другую клавишу.`,
      );
      return;
    }
    if (exclude && code === exclude) {
      setError(
        `${prettyHotkey(code)} уже назначена для ${excludeLabel || "другого действия"}.`,
      );
      return;
    }
    if (savingRef.current) return;

    savingRef.current = true;
    setSaving(true);
    try {
      // Держим глобальный listener на паузе, пока backend не принял
      // новую binding. Иначе первое быстрое нажатие матчилось ещё
      // со старой клавишей и выглядело как тихий отказ.
      const saved = await onChange(code);
      if (saved === false) {
        setError("Не удалось сохранить клавишу — повторите назначение.");
        return;
      }
      setError(null);
      setCapturing(false);
      ref.current?.blur();
    } finally {
      savingRef.current = false;
      setSaving(false);
    }
  }

  captureCodeRef.current = (code) => {
    void captureCode(code);
  };

  useEffect(() => {
    if (!capturing) return;
    const onWindowKeyDown = (e: globalThis.KeyboardEvent) => {
      // WebKit на macOS иногда не отдаёт modifier-key keydown именно кнопке,
      // даже если она была сфокусирована кликом. Глобальный capture-слушатель
      // живёт только во время назначения и ловит Ctrl/Alt/F-клавиши надёжно.
      e.preventDefault();
      e.stopPropagation();
      // Назначение фиксируем на keyup: нативный listener остаётся выключенным
      // до физического отпускания и не примет release без соответствующего press.
      if (e.code === "Escape") captureCodeRef.current(e.code);
    };
    const onWindowKeyUp = (e: globalThis.KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (activationCodeRef.current === e.code) {
        activationCodeRef.current = null;
        return;
      }
      captureCodeRef.current(e.code);
    };
    window.addEventListener("keydown", onWindowKeyDown, { capture: true });
    window.addEventListener("keyup", onWindowKeyUp, { capture: true });
    return () => {
      window.removeEventListener("keydown", onWindowKeyDown, { capture: true });
      window.removeEventListener("keyup", onWindowKeyUp, { capture: true });
      activationCodeRef.current = null;
      void setNativeCapture(false);
    };
  }, [capturing]);

  function onKeyDown(e: ReactKeyboardEvent<HTMLButtonElement>) {
    if (!capturing) {
      // Enter/Space «нажимают» сфокусированную кнопку — входим в режим захвата,
      // не давая браузеру сразу же сгенерить click.
      if (e.code === "Enter" || e.code === "Space") {
        e.preventDefault();
        beginCapture(e.code);
      }
      return;
    }
  }

  const printableWarn = !capturing && !error && isPrintableHotkey(value);
  const safeHoldKeys = IS_APPLE_PLATFORM ? "Cmd / Option / F-клавиша" : "Ctrl / Alt / F-клавиша";

  return (
    <div className="hotkey-capture">
      <button
        ref={ref}
        type="button"
        className={`input-mono hotkey-btn${capturing ? " capturing" : ""}`}
        onClick={() => beginCapture()}
        onKeyDown={onKeyDown}
        onBlur={() => setCapturing(false)}
        aria-label="Назначить горячую клавишу"
      >
        {saving ? "Сохраняю…" : capturing ? "Нажмите любую клавишу…" : prettyHotkey(value)}
      </button>
      {error ? (
        <div className="hotkey-error">{error}</div>
      ) : printableWarn ? (
        <div className="hotkey-error">
          Печатная клавиша: в режиме «Удержание» будет печататься символ в активном
          окне. Надёжнее {safeHoldKeys}.
        </div>
      ) : null}
    </div>
  );
}

/* ---------- Toast / banner notice ---------- */

// Зеркала CSS (styles.css): toastLife 6s / .toast-leaving 200ms.
// Меняешь тут — меняй и в keyframes/transition.
const TOAST_LIFE_MS = 6000;
const TOAST_EXIT_MS = 200;

export function Toast({
  message,
  variant = "warning",
  actionLabel,
  onAction,
  onClose,
}: {
  message: string;
  // info/success уходят сами через 6с (с полоской-прогрессом по нижней кромке);
  // warning/error — только вручную: пользователь должен успеть прочитать/нажать.
  variant?: "info" | "success" | "warning" | "error";
  actionLabel?: string;
  onAction?: () => void;
  onClose?: () => void;
}) {
  const autoClose = (variant === "info" || variant === "success") && !!onClose;
  // leaving: сперва играем выход (transform+opacity, класс .toast-leaving),
  // и только потом зовём onClose (удаление узла) — иначе тост пропадал бы резко.
  const [leaving, setLeaving] = useState(false);
  // onClose в ref: таймер выхода не должен перезапускаться от смены пропа.
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  // Тот же узел может получить НОВОЕ сообщение (App переиспользует <Toast> без
  // key) — сбрасываем «уход» и перезапускаем жизнь, чтобы свежий текст не
  // унаследовал старое автозакрытие на середине.
  useEffect(() => {
    setLeaving(false);
  }, [message, variant]);

  // Автозакрытие info/success. Cleanup гасит таймер при размонтировании
  // (StrictMode-двойной маунт безопасен) и при смене сообщения.
  useEffect(() => {
    if (!autoClose) return;
    const t = setTimeout(() => setLeaving(true), TOAST_LIFE_MS);
    return () => clearTimeout(t);
  }, [autoClose, message, variant]);

  // Фактическое удаление — после завершения CSS-выхода.
  useEffect(() => {
    if (!leaving) return;
    const t = setTimeout(() => onCloseRef.current?.(), TOAST_EXIT_MS);
    return () => clearTimeout(t);
  }, [leaving]);

  return (
    <div
      className={`toast toast-${variant}${leaving ? " toast-leaving" : ""}`}
      // error/warning требуют внимания (alert), info/success — фоновый статус.
      role={variant === "error" || variant === "warning" ? "alert" : "status"}
    >
      <span className="toast-msg">{message}</span>
      {actionLabel && onAction && (
        <button className="toast-action" onClick={onAction}>
          {actionLabel}
        </button>
      )}
      {onClose && (
        <button
          className="toast-close"
          onClick={() => setLeaving(true)}
          aria-label="Закрыть"
        >
          ×
        </button>
      )}
      {/* Полоска-прогресс автозакрытия; в .toast-leaving её уже не показываем. */}
      {autoClose && !leaving && <span className="toast-life" aria-hidden />}
    </div>
  );
}

/* ---------- Page header ---------- */
export function PageHead({ title, desc }: { title: string; desc?: string }) {
  return (
    <div className="page-head">
      <h1 className="page-title">{title}</h1>
      {desc && <p className="page-desc">{desc}</p>}
    </div>
  );
}

/* ---------- Icons ---------- */
type IconProps = { className?: string };

export const Icon = {
  Home: (p: IconProps) => (
    <svg className={p.className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M3 9.5 12 3l9 6.5" /><path d="M5 10v10h14V10" /></svg>
  ),
  Cube: (p: IconProps) => (
    <svg className={p.className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M12 2 21 7v10l-9 5-9-5V7l9-5Z" /><path d="m3 7 9 5 9-5" /><path d="M12 12v10" /></svg>
  ),
  Wand: (p: IconProps) => (
    <svg className={p.className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="m15 4 5 5L8 21l-5 1 1-5L15 4Z" /><path d="m14 5 5 5" /></svg>
  ),
  Sliders: (p: IconProps) => (
    <svg className={p.className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M4 6h10M18 6h2M4 12h2M10 12h10M4 18h12M20 18h0" /><circle cx="16" cy="6" r="2" /><circle cx="8" cy="12" r="2" /><circle cx="18" cy="18" r="2" /></svg>
  ),
  Book: (p: IconProps) => (
    <svg className={p.className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M4 5a2 2 0 0 1 2-2h13v18H6a2 2 0 0 1-2-2V5Z" /><path d="M19 17H6a2 2 0 0 0-2 2" /></svg>
  ),
  Code: (p: IconProps) => (
    <svg className={p.className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="m9 8-4 4 4 4M15 8l4 4-4 4" /></svg>
  ),
  Clock: (p: IconProps) => (
    <svg className={p.className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><circle cx="12" cy="12" r="9" /><path d="M12 7v5l3 2" /></svg>
  ),
  Mic: (p: IconProps) => (
    <svg className={p.className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><rect x="9" y="2" width="6" height="12" rx="3" /><path d="M5 11a7 7 0 0 0 14 0M12 18v4M8 22h8" /></svg>
  ),
  Trash: (p: IconProps) => (
    <svg className={p.className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M3 6h18M8 6V4h8v2M6 6l1 14h10l1-14" /></svg>
  ),
  Download: (p: IconProps) => (
    <svg className={p.className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M12 3v12M7 11l5 5 5-5M5 21h14" /></svg>
  ),
  Plus: (p: IconProps) => (
    <svg className={p.className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round"><path d="M12 5v14M5 12h14" /></svg>
  ),
  Refresh: (p: IconProps) => (
    <svg className={p.className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M20 11a8 8 0 0 0-14.3-4.9L4 8" /><path d="M4 4v4h4" /><path d="M4 13a8 8 0 0 0 14.3 4.9L20 16" /><path d="M20 20v-4h-4" /></svg>
  ),
  Sparkles: (p: IconProps) => (
    <svg className={p.className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M12 3l1.8 4.7L18.5 9.5 13.8 11.3 12 16l-1.8-4.7L5.5 9.5l4.7-1.8L12 3Z" /><path d="M19 14l.7 1.8 1.8.7-1.8.7-.7 1.8-.7-1.8-1.8-.7 1.8-.7L19 14Z" /></svg>
  ),
  Check: (p: IconProps) => (
    <svg className={p.className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round"><path d="M20 6 9 17l-5-5" /></svg>
  ),
};
