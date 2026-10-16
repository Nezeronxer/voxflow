/**
 * Карточка «Доступы»: что показывать по статусу, который отдаёт Rust
 * (`permissions_status`). Чистая логика без JSX, чтобы её гонял `node --test`.
 */
export type PermissionKind = "microphone" | "accessibility" | "input_monitoring";

export type PermissionsStatus = {
  microphone: "granted" | "notDetermined" | "denied";
  accessibility: boolean;
  inputMonitoring: boolean;
};

export type PermissionRow = {
  kind: PermissionKind;
  title: string;
  hint: string;
  granted: boolean;
  /** Подпись кнопки. */
  action: string;
};

/** Карточка видна, только пока какая-то строка не выдана: тот же источник, что у строк. */
export function showPermissionsCard(status: PermissionsStatus | null): boolean {
  return status !== null && permissionRows(status).some((row) => !row.granted);
}

export function permissionRows(status: PermissionsStatus): PermissionRow[] {
  return [
    {
      kind: "microphone",
      title: "Микрофон",
      hint: "Чтобы слышать вашу речь.",
      granted: status.microphone === "granted",
      // После отказа система окно не покажет - ведём в настройки.
      action: status.microphone === "notDetermined" ? "Разрешить" : "Открыть настройки",
    },
    {
      kind: "accessibility",
      title: "Универсальный доступ",
      hint: "Чтобы вставлять текст в другие программы.",
      granted: status.accessibility,
      action: "Открыть настройки",
    },
    {
      kind: "input_monitoring",
      title: "Мониторинг ввода",
      hint: "Чтобы работала горячая клавиша.",
      granted: status.inputMonitoring,
      action: "Открыть настройки",
    },
  ];
}
