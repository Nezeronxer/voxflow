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

// Tauri/WebView2 по умолчанию показывает браузерное контекстное меню на правый
// клик. Для десктопного UI VoxFlow оно не нужно и выглядит как случайная утечка
// браузера, поэтому глушим его до монтирования React в главном окне и overlay.
window.addEventListener(
  "contextmenu",
  (event) => {
    event.preventDefault();
    event.stopPropagation();
  },
  { capture: true },
);

const isOverlay = window.location.hash.includes("overlay");

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>{isOverlay ? <Overlay /> : <App />}</React.StrictMode>,
);
