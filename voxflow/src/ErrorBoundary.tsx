// Граница ошибок для области контента. Без неё исключение при рендере любой
// секции роняет ВЕСЬ React-корень → белый экран и неработающая навигация.
// С ней падает только проблемная секция (показывая текст ошибки), а боковое
// меню остаётся живым — можно уйти на другую вкладку.

import { Component, type ReactNode, type ErrorInfo } from "react";

type Props = { children: ReactNode };
type State = { err: Error | null; stack: string };

export class ErrorBoundary extends Component<Props, State> {
  state: State = { err: null, stack: "" };

  static getDerivedStateFromError(err: Error): State {
    return { err, stack: "" };
  }

  componentDidCatch(err: Error, info: ErrorInfo) {
    // Лог в консоль WebView (видно в devtools) + сохраняем стек для показа.
    console.error("[VoxFlow] раздел упал:", err, info);
    this.setState({ err, stack: info?.componentStack || err.stack || "" });
  }

  render() {
    const { err, stack } = this.state;
    if (!err) return this.props.children;
    return (
      <div className="content-inner">
        <div className="card" style={{ borderColor: "var(--red)" }}>
          <div className="card-head">
            <div className="card-title">Не удалось открыть раздел</div>
            <div className="sub">
              Раздел вызвал ошибку при отображении. Меню слева работает — можно
              вернуться на «Главную» или открыть другой раздел.
            </div>
          </div>
          <pre
            style={{
              whiteSpace: "pre-wrap",
              wordBreak: "break-word",
              fontFamily: "var(--font-mono)",
              fontSize: 12,
              lineHeight: 1.5,
              color: "var(--text)",
              background: "var(--surface-2, rgba(0,0,0,0.04))",
              border: "1px solid var(--border)",
              borderRadius: 6,
              padding: "12px 14px",
              margin: 0,
              maxHeight: "40vh",
              overflow: "auto",
            }}
          >
            {String(err.message || err)}
            {stack ? "\n" + stack : ""}
          </pre>
        </div>
      </div>
    );
  }
}
