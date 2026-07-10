import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import Overlay from "./Overlay";
import { initTheme } from "./ui";
import "./assets/fonts.css";
import "./styles.css";

// Тема ДО рендера: применяем localStorage-кэш "vf-theme" на <html> синхронно,
// чтобы окно не мигало светлым при старте в тёмной теме. Заодно подписки:
// смена системной темы (режим "system") и storage-событие из соседнего окна
// (так overlay перекрашивается мгновенно при смене темы в настройках).
initTheme();

// На декоративной поверхности браузерное меню WebView не нужно. В полях
// ввода оно, наоборот, даёт нативные Copy/Paste, замены и spelling-подсказки.
function keepNativeContextMenu(target: EventTarget | null): boolean {
  const element = target instanceof Element ? target : null;
  if (!element) return false;
  if (element.closest("input, textarea")) return true;

  const editable = element.closest("[contenteditable]");
  return editable instanceof HTMLElement && editable.isContentEditable;
}

window.addEventListener(
  "contextmenu",
  (event) => {
    if (keepNativeContextMenu(event.target)) return;
    event.preventDefault();
    event.stopPropagation();
  },
  { capture: true },
);

const isOverlay = window.location.hash.includes("overlay");

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>{isOverlay ? <Overlay /> : <App />}</React.StrictMode>,
);
