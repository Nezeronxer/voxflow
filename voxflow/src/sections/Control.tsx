import { useEffect, useState } from "react";
import { checkForUpdate, installUpdate, listAudioDevices, openReleaseUrl } from "../api";
import {
  PageHead,
  SectionShell,
  Field,
  Select,
  Switch,
  HotkeyCapture,
  normalizeTheme,
  HOTKEY_FIELD_HINT,
} from "../ui";
import type { Settings } from "../types";
import type { UpdateInfo } from "../types";
import {
  normalizeOverlayScale,
  OVERLAY_SCALE_MAX,
  OVERLAY_SCALE_MIN,
  OVERLAY_SCALE_STEP,
} from "../types";

// Сегменты темы: значения зеркалят Settings.theme ("system"|"light"|"dark").
// Применение мгновенное (эффект в App.tsx), сохранение — штатным debounce-
// saveSettings через update().
const THEME_OPTIONS = [
  { value: "system", label: "Системная" },
  { value: "light", label: "Светлая" },
  { value: "dark", label: "Тёмная" },
] as const;

/**
 * `scope` делит раздел между двумя страницами хаба: «Диктовка» берёт то, что
 * происходит во время записи, «Программа» — оформление и поведение самого
 * приложения. Раньше это лежало одной кучей в «Основных», и человек искал
 * микрофон там же, где автозапуск.
 */
export default function Control({
  settings,
  update,
  persist,
  scope = "all",
  embedded,
}: {
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
  persist: (settings: Settings) => Promise<boolean>;
  scope?: "all" | "dictation" | "app";
  embedded?: boolean;
}) {
  const [devices, setDevices] = useState<string[]>([]);
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
  const [updateStatus, setUpdateStatus] = useState("");
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [installingUpdate, setInstallingUpdate] = useState(false);
  const overlayScale = normalizeOverlayScale(settings.overlay_scale);
  const overlayPercent = Math.round(overlayScale * 100);

  async function updateHotkey(patch: Pick<Settings, "hotkey"> | Pick<Settings, "improve_hotkey">) {
    const next = { ...settings, ...patch };
    update(patch);
    // Hotkeys are operational settings: commit them before HotkeyCapture
    // re-enables the native listener instead of waiting for the generic debounce.
    return persist(next);
  }

  useEffect(() => {
    // Список устройств нужен только полю микрофона: на странице «Программа»
    // его нет, и опрашивать аудиоподсистему незачем.
    if (scope === "app") return;
    let alive = true;
    listAudioDevices().then((d) => alive && setDevices(d));
    return () => {
      alive = false;
    };
  }, [scope]);

  const deviceOptions = [
    { value: "", label: "По умолчанию" },
    ...devices.map((d) => ({ value: d, label: d })),
  ];

  async function onCheckUpdate() {
    setCheckingUpdate(true);
    setUpdateStatus("Проверяю…");
    const info = await checkForUpdate();
    setCheckingUpdate(false);
    setUpdateInfo(info);
    if (!info) {
      setUpdateStatus("Проверка не удалась");
    } else if (info.available) {
      setUpdateStatus(`Доступна версия ${info.latest_version}`);
    } else {
      setUpdateStatus(`Актуальная версия ${info.current_version}`);
    }
  }

  async function onInstallUpdate() {
    if (!updateInfo?.available) return;
    setInstallingUpdate(true);
    if (!updateInfo.auto_install) {
      const opened = await openReleaseUrl(updateInfo.release_url);
      setInstallingUpdate(false);
      setUpdateStatus(opened ? "Страница релиза открыта" : "Не удалось открыть релиз");
      return;
    }
    setUpdateStatus("Скачиваю обновление…");
    const result = await installUpdate(
      updateInfo.asset_url,
      updateInfo.asset_name,
      updateInfo.asset_size,
      updateInfo.asset_digest,
    );
    setInstallingUpdate(false);
    setUpdateStatus(result?.launched ? result.message : "Не удалось установить обновление");
  }

  const showDictation = scope !== "app";
  const showApp = scope !== "dictation";

  return (
    <SectionShell embedded={embedded}>
      {!embedded && (
        <PageHead
          title="Основные настройки"
          desc="Горячие клавиши, микрофон и поведение приложения."
        />
      )}

      {showDictation && (
      <div className="card">
        <div className="card-head">
          <div className="card-title">Запись</div>
        </div>

        <Field label="Микрофон" hint="Устройство для записи речи">
          <Select
            value={settings.input_device}
            onChange={(v) => update({ input_device: v })}
            options={deviceOptions}
          />
        </Field>

        <Field
          label="Горячая клавиша диктовки"
          hint={HOTKEY_FIELD_HINT}
        >
          <HotkeyCapture
            value={settings.hotkey}
            onChange={(code) => updateHotkey({ hotkey: code })}
            exclude={settings.improve_hotkey}
            excludeLabel="улучшения выделенного"
          />
        </Field>

        <Field
          label="Улучшить выделенное"
          hint="Одиночное нажатие берёт выделенный текст, чистит его и заменяет в активном поле. Esc отменяет незавершённую обработку."
        >
          <HotkeyCapture
            value={settings.improve_hotkey}
            onChange={(code) => updateHotkey({ improve_hotkey: code })}
            exclude={settings.hotkey}
            excludeLabel="диктовки"
          />
        </Field>

        <Field
          label="Режим"
          hint="«Удержание» пишет, пока клавиша зажата. «Переключатель» — нажал/нажал."
        >
          <div className="seg" role="radiogroup" aria-label="Режим диктовки">
            {[
              { value: "hold", label: "Удержание" },
              { value: "toggle", label: "Переключатель" },
            ].map((option) => {
              const active = settings.mode === option.value;
              return (
                <button
                  key={option.value}
                  type="button"
                  role="radio"
                  aria-checked={active}
                  className={`seg-btn${active ? " active" : ""}`}
                  onClick={() => update({ mode: option.value })}
                >
                  {option.label}
                </button>
              );
            })}
          </div>
        </Field>

        <Field
          label="Графическое ускорение"
          hint="Видеокарта ускоряет Whisper (NVIDIA) и встроенный локальный ИИ (NVIDIA/AMD/Intel, Apple Silicon). «Авто» включает её, когда карта есть. GigaAM и Parakeet всегда считают на процессоре."
        >
          <div className="seg" role="radiogroup" aria-label="Графическое ускорение">
            {[
              { value: "auto", label: "Авто" },
              { value: "on", label: "Вкл" },
              { value: "off", label: "Выкл" },
            ].map((option) => {
              const active = (settings.gpu_mode || "auto") === option.value;
              return (
                <button
                  key={option.value}
                  type="button"
                  role="radio"
                  aria-checked={active}
                  className={`seg-btn${active ? " active" : ""}`}
                  onClick={() => update({ gpu_mode: option.value })}
                >
                  {option.label}
                </button>
              );
            })}
          </div>
        </Field>

        <Field
          label="Защёлка двойным тапом"
          hint="В hold-режиме второй быстрый тап запускает запись без удержания. Любое физическое отпускание обрабатывается сразу, без скрытой задержки."
        >
          <Switch
            checked={settings.double_tap_latch}
            onChange={(v) => update({ double_tap_latch: v })}
          />
        </Field>

        {/* Языка здесь нет намеренно: он редактировался и тут, и в «Моделях»,
            причём там полным списком из 16 языков, а тут урезанной тройкой.
            Осталось одно поле — то, что рядом с движком, который язык и выбирает. */}
      </div>
      )}

      {showApp && (
      <>
      <div className="card">
        <div className="card-head">
          <div className="card-title">Вид</div>
        </div>

        <Field
          label="Тема"
          hint="«Системная» следует за оформлением операционной системы. Применяется мгновенно и сохраняется автоматически."
        >
          {/* radiogroup: один выбор из трёх; активный сегмент — инверсия. */}
          <div className="seg" role="radiogroup" aria-label="Тема оформления">
            {THEME_OPTIONS.map((t) => {
              const active = normalizeTheme(settings.theme) === t.value;
              return (
                <button
                  key={t.value}
                  type="button"
                  role="radio"
                  aria-checked={active}
                  className={`seg-btn${active ? " active" : ""}`}
                  onClick={() => update({ theme: t.value })}
                >
                  {t.label}
                </button>
              );
            })}
          </div>
        </Field>

        <Field
          label="Размер плашки"
          hint="Масштаб Flow Bar сохраняется автоматически и применяется без перезапуска."
        >
          {/* Кнопки −/+ убраны: ползунок сам шагает стрелками с клавиатуры,
              так что они дублировали его, а не добавляли доступности. */}
          <div className="overlay-scale-control">
            <input
              type="range"
              min={OVERLAY_SCALE_MIN * 100}
              max={OVERLAY_SCALE_MAX * 100}
              step={OVERLAY_SCALE_STEP * 100}
              value={overlayPercent}
              onChange={(event) =>
                update({ overlay_scale: Number(event.currentTarget.value) / 100 })
              }
              aria-label="Размер плавающей плашки"
              aria-valuetext={`${overlayPercent}%`}
            />
            <output>{overlayPercent}%</output>
            <button
              type="button"
              className="btn btn-sm"
              disabled={overlayPercent === 100}
              onClick={() => update({ overlay_scale: 1 })}
            >
              Сбросить
            </button>
          </div>
        </Field>
      </div>

      <div className="card">
        <div className="card-head">
          <div className="card-title">Приложение</div>
        </div>

        <Field label="Звуки" hint="Сигнал при старте и завершении записи">
          <Switch
            checked={settings.play_sounds}
            onChange={(v) => update({ play_sounds: v })}
          />
        </Field>

        <Field
          label="Приглушать звук во время диктовки"
          hint="Возвращать системную громкость после остановки или Esc"
        >
          <Switch
            checked={settings.auto_mute}
            onChange={(v) => update({ auto_mute: v })}
          />
        </Field>

        <Field label="Запускать при входе" hint="Автоматически открывать VoxFlow вместе с системой">
          <Switch
            checked={settings.autostart}
            onChange={(v) => update({ autostart: v })}
          />
        </Field>

        <Field
          label="Автообновления"
          hint="Проверять новые версии в GitHub Releases при запуске"
        >
          <Switch
            checked={settings.auto_update_check}
            onChange={(v) => update({ auto_update_check: v })}
          />
        </Field>

        <Field label="Версия" hint={updateStatus || "GitHub Releases"}>
          <div className="row-flex">
            <button
              className="btn btn-sm"
              onClick={onCheckUpdate}
              disabled={checkingUpdate || installingUpdate}
            >
              {checkingUpdate ? "Проверяю…" : "Проверить"}
            </button>
            {updateInfo?.available && (
              <button
                className="btn btn-sm btn-primary"
                onClick={onInstallUpdate}
                disabled={checkingUpdate || installingUpdate}
              >
                {installingUpdate
                  ? updateInfo.auto_install
                    ? "Скачиваю…"
                    : "Открываю…"
                  : updateInfo.auto_install
                    ? "Установить"
                    : "Открыть релиз"}
              </button>
            )}
          </div>
        </Field>
      </div>
      </>
      )}
    </SectionShell>
  );
}
