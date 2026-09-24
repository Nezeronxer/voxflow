import { useEffect, useRef, useState } from "react";
import { rewritePromptWithInstruction, subscribe, toggleDictation } from "../api";
import { PageHead, SectionShell, Field, Icon, Select, Switch } from "../ui";
import type {
  ErrorEvent as VoxErrorEvent,
  NoRecogEvent,
  Settings,
  TranscriptEvent,
} from "../types";

function compactInstructionSource(value: string) {
  return value
    .replace(/\s+/g, " ")
    .replace(/[“”«»]/g, '"')
    .trim();
}

function buildSmartPromptInstruction(source: string) {
  const cleaned = compactInstructionSource(source);
  if (!cleaned) return "";
  const styleLine = /[.!?…]$/.test(cleaned)
    ? `Пользовательский стиль/задача: ${cleaned}`
    : `Пользовательский стиль/задача: ${cleaned}.`;
  return [
    styleLine,
    "Каждую диктовку превращай в готовый печатный текст именно под эту задачу: сохрани факты, намерение и язык оригинала, убери запинки, повторы и брошенные формулировки.",
    "Если диктовка звучит как задание для нейросети или разработчика, оформи её как ясный промпт: действие, контекст, требования к результату и ограничения.",
    "Сбивчивые устные конструкции превращай в естественные письменные формулировки: «я объясни мне» → «Объясни мне», «а что ещё я хочу сказать» → «Также учти».",
    "Сохраняй контекст соседних фраз: короткое продолжение объединяй с предыдущей мыслью и продолжай предложение; новый абзац делай только при смене темы, перечислении или явной команде.",
    "Не отвечай на диктовку и не добавляй фактов от себя; меняй только форму подачи.",
  ].join(" ");
}

type SpeechRecognitionAlternativeLike = {
  transcript?: string;
};

type SpeechRecognitionResultLike = {
  readonly length: number;
  readonly isFinal?: boolean;
  [index: number]: SpeechRecognitionAlternativeLike | undefined;
};

type SpeechRecognitionResultListLike = {
  readonly length: number;
  [index: number]: SpeechRecognitionResultLike | undefined;
};

type SpeechRecognitionEventLike = {
  readonly resultIndex: number;
  readonly results: SpeechRecognitionResultListLike;
};

type SpeechRecognitionErrorEventLike = {
  readonly error?: string;
};

type SpeechRecognitionLike = {
  lang: string;
  continuous: boolean;
  interimResults: boolean;
  maxAlternatives: number;
  start: () => void;
  stop: () => void;
  abort: () => void;
  onresult: ((event: SpeechRecognitionEventLike) => void) | null;
  onerror: ((event: SpeechRecognitionErrorEventLike) => void) | null;
  onend: (() => void) | null;
};

type SpeechRecognitionCtor = new () => SpeechRecognitionLike;
type VoiceCaptureMode = "voxflow" | "web-speech";

function getSpeechRecognitionCtor(): SpeechRecognitionCtor | null {
  if (typeof window === "undefined") return null;
  const w = window as Window & {
    SpeechRecognition?: SpeechRecognitionCtor;
    webkitSpeechRecognition?: SpeechRecognitionCtor;
  };
  return w.SpeechRecognition ?? w.webkitSpeechRecognition ?? null;
}

function isTauriRuntime(): boolean {
  if (typeof window === "undefined") return false;
  const w = window as Window & {
    __TAURI__?: unknown;
    __TAURI_INTERNALS__?: unknown;
  };
  return Boolean(w.__TAURI__ || w.__TAURI_INTERNALS__);
}

function transcriptFromSpeechEvent(event: SpeechRecognitionEventLike): string {
  const parts: string[] = [];
  for (let i = event.resultIndex; i < event.results.length; i += 1) {
    const result = event.results[i];
    const transcript = result?.[0]?.transcript?.trim();
    if (transcript) parts.push(transcript);
  }
  return parts.join(" ").trim();
}

function speechLanguage(settingsLanguage: string): string {
  if (settingsLanguage === "en") return "en-US";
  return "ru-RU";
}

/**
 * `part`: main — очистка речи и инструкция для нейросети; advanced — способ
 * вставки, потоковая вставка, обучающие записи (свёрнутое «Дополнительно»).
 */
export default function Recognition({
  settings,
  update,
  embedded,
  part,
}: {
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
  embedded?: boolean;
  part?: "main" | "advanced";
}) {
  const promptText = settings.smart_prompt_source;
  const promptReady =
    settings.smart_prompt_enabled && promptText.trim().length > 0;
  const aiReady = settings.ai_backend !== "off";
  const [speechSupported, setSpeechSupported] = useState(
    () => getSpeechRecognitionCtor() !== null,
  );
  const [isRecordingInstruction, setIsRecordingInstruction] = useState(false);
  const [isRewritingPrompt, setIsRewritingPrompt] = useState(false);
  const [voiceInstruction, setVoiceInstruction] = useState("");
  const [voiceStatus, setVoiceStatus] = useState("");
  const [voiceError, setVoiceError] = useState("");
  const [rewritePreview, setRewritePreview] = useState("");
  const recognitionRef = useRef<SpeechRecognitionLike | null>(null);
  const voiceInstructionRef = useRef<HTMLTextAreaElement | null>(null);
  const voiceCaptureActiveRef = useRef(false);
  const voiceCaptureModeRef = useRef<VoiceCaptureMode>("voxflow");
  const hasBuiltInVoiceInput = isTauriRuntime();

  useEffect(() => {
    setSpeechSupported(getSpeechRecognitionCtor() !== null);
    return () => {
      recognitionRef.current?.abort();
      recognitionRef.current = null;
      if (
        voiceCaptureActiveRef.current &&
        voiceCaptureModeRef.current === "voxflow"
      ) {
        void toggleDictation();
      }
      voiceCaptureActiveRef.current = false;
    };
  }, []);

  function updateSmartPrompt(source: string) {
    update({
      smart_prompt_source: source,
      smart_prompt_instruction: buildSmartPromptInstruction(source),
    });
  }

  async function rewritePromptFromInstruction(instruction: string) {
    const originalPrompt = promptText.trim();
    const cleanInstruction = instruction.trim();
    setVoiceError("");
    setRewritePreview("");

    if (!originalPrompt) {
      setVoiceError("Сначала напишите базовый prompt для переработки.");
      setVoiceStatus("");
      return;
    }
    if (!cleanInstruction) {
      setVoiceError("Сначала продиктуйте или введите инструкцию.");
      setVoiceStatus("");
      return;
    }

    setIsRewritingPrompt(true);
    setVoiceStatus("Перерабатываю prompt...");
    try {
      const result = await rewritePromptWithInstruction(
        originalPrompt,
        cleanInstruction,
      );
      if (result.ok && result.text.trim()) {
        setRewritePreview(result.text.trim());
        setVoiceStatus("Готово. Проверьте preview перед применением.");
      } else {
        setVoiceError(result.message || "Не удалось переработать prompt.");
        setVoiceStatus("");
      }
    } catch (error) {
      setVoiceError(
        error instanceof Error
          ? error.message
          : "Не удалось переработать prompt.",
      );
      setVoiceStatus("");
    } finally {
      setIsRewritingPrompt(false);
    }
  }

  useEffect(() => {
    const offs = [
      subscribe<TranscriptEvent>("transcript", (event) => {
        if (
          !voiceCaptureActiveRef.current ||
          voiceCaptureModeRef.current !== "voxflow"
        ) {
          return;
        }
        voiceCaptureActiveRef.current = false;
        setIsRecordingInstruction(false);
        const transcript = event.payload?.text?.trim() ?? "";
        if (!transcript) {
          setVoiceError("Речь не распознана. Попробуйте ещё раз или введите инструкцию.");
          setVoiceStatus("");
          return;
        }
        setVoiceInstruction(transcript);
        void rewritePromptFromInstruction(transcript);
      }),
      subscribe<NoRecogEvent>("norecog", (event) => {
        if (
          !voiceCaptureActiveRef.current ||
          voiceCaptureModeRef.current !== "voxflow"
        ) {
          return;
        }
        voiceCaptureActiveRef.current = false;
        setIsRecordingInstruction(false);
        setVoiceStatus("");
        setVoiceError(
          event.payload?.message ||
            "Речь не распознана. Попробуйте ещё раз или введите инструкцию.",
        );
      }),
      subscribe<VoxErrorEvent>("error", (event) => {
        if (
          !voiceCaptureActiveRef.current ||
          voiceCaptureModeRef.current !== "voxflow"
        ) {
          return;
        }
        voiceCaptureActiveRef.current = false;
        setIsRecordingInstruction(false);
        setVoiceStatus("");
        setVoiceError(
          event.payload?.message ||
            "Не удалось записать голосовую инструкцию.",
        );
      }),
    ];
    return () => offs.forEach((off) => off());
  }, [promptText]);

  function stopVoiceInstruction() {
    if (voiceCaptureModeRef.current === "web-speech") {
      recognitionRef.current?.stop();
      setIsRecordingInstruction(false);
      setVoiceStatus("Завершаю запись...");
      return;
    }
    void toggleDictation();
    setVoiceStatus("Распознаю инструкцию...");
  }

  async function startVoxFlowInstruction() {
    voiceCaptureModeRef.current = "voxflow";
    voiceCaptureActiveRef.current = true;
    setVoiceInstruction("");
    setVoiceError("");
    setRewritePreview("");
    setVoiceStatus("Слушаю инструкцию через VoxFlow...");
    setIsRecordingInstruction(true);
    window.setTimeout(() => voiceInstructionRef.current?.focus(), 0);
    await toggleDictation();
  }

  function startWebSpeechInstruction() {
    const originalPrompt = promptText.trim();
    if (!originalPrompt) {
      setVoiceError("Сначала напишите базовый prompt для переработки.");
      setVoiceStatus("");
      return;
    }

    const RecognitionCtor = getSpeechRecognitionCtor();
    if (!RecognitionCtor) {
      setSpeechSupported(false);
      setVoiceError(
        "Голосовой ввод недоступен в этом WebView. Введите инструкцию текстом.",
      );
      setVoiceStatus("");
      return;
    }

    recognitionRef.current?.abort();
    voiceCaptureModeRef.current = "web-speech";
    voiceCaptureActiveRef.current = false;
    setSpeechSupported(true);
    setVoiceInstruction("");
    setVoiceError("");
    setRewritePreview("");
    setVoiceStatus("Слушаю инструкцию...");
    setIsRecordingInstruction(true);

    const recognition = new RecognitionCtor();
    let receivedResult = false;
    recognition.lang = speechLanguage(settings.language);
    recognition.continuous = false;
    recognition.interimResults = false;
    recognition.maxAlternatives = 1;
    recognition.onresult = (event) => {
      const transcript = transcriptFromSpeechEvent(event);
      receivedResult = true;
      setIsRecordingInstruction(false);
      if (!transcript) {
        setVoiceError("Речь не распознана. Попробуйте ещё раз или введите инструкцию.");
        setVoiceStatus("");
        return;
      }
      setVoiceInstruction(transcript);
      void rewritePromptFromInstruction(transcript);
    };
    recognition.onerror = (event) => {
      receivedResult = true;
      setIsRecordingInstruction(false);
      setVoiceStatus("");
      setVoiceError(
        event.error === "not-allowed"
          ? "Разрешите доступ к микрофону для голосовой инструкции."
          : "Не удалось распознать голосовую инструкцию. Можно ввести её текстом.",
      );
    };
    recognition.onend = () => {
      recognitionRef.current = null;
      setIsRecordingInstruction(false);
      if (!receivedResult && !isRewritingPrompt) {
        setVoiceStatus("");
      }
    };
    recognitionRef.current = recognition;

    try {
      recognition.start();
    } catch {
      recognitionRef.current = null;
      setIsRecordingInstruction(false);
      setVoiceStatus("");
      setVoiceError("Не удалось начать запись. Попробуйте ввести инструкцию текстом.");
    }
  }

  function startVoiceInstruction() {
    const originalPrompt = promptText.trim();
    if (!originalPrompt) {
      setVoiceError("Сначала напишите базовый prompt для переработки.");
      setVoiceStatus("");
      return;
    }
    if (hasBuiltInVoiceInput) {
      void startVoxFlowInstruction();
      return;
    }
    startWebSpeechInstruction();
  }

  function applyRewritePreview() {
    if (!rewritePreview.trim()) return;
    updateSmartPrompt(rewritePreview);
    setRewritePreview("");
    setVoiceStatus("Preview применён к основному prompt.");
    setVoiceError("");
  }

  function cancelRewritePreview() {
    setRewritePreview("");
    setVoiceStatus("Preview отменён.");
    setVoiceError("");
  }

  const main = (
    <>
      <div className="card">
        <div className="card-head">
          <div className="card-title">Очистка речи</div>
        </div>

        <Field
          label="Дословно"
          hint="Вставлять как сказано, без правок"
        >
          <Switch
            checked={settings.verbatim}
            onChange={(v) => update({ verbatim: v })}
          />
        </Field>

        <Field
          label="Убирать слова-паразиты"
          hint="«Эээ», «ну», «как бы»"
        >
          <Switch
            checked={settings.remove_fillers}
            onChange={(v) => update({ remove_fillers: v })}
          />
        </Field>

        <Field
          label="Автопунктуация"
          hint="Точки, запятые и заглавные буквы"
        >
          <Switch
            checked={settings.auto_punct}
            onChange={(v) => update({ auto_punct: v })}
          />
        </Field>

        <Field
          label="Исправления голосом"
          hint="Удалять сказанное до «то есть», «нет», «точнее». Может срезать нужный текст"
        >
          <Switch
            checked={settings.aggressive_self_correction}
            onChange={(v) => update({ aggressive_self_correction: v })}
          />
        </Field>
      </div>
      {aiReady && (
      <div className="card">
        <div className="card-head">
          <div className="card-title">Инструкция для нейросети</div>
        </div>

        <Field
          label="Своя инструкция"
          hint="Применяется к каждой диктовке, например «пиши коротко и без воды»"
        >
          <Switch
            checked={settings.smart_prompt_enabled}
            onChange={(v) => update({ smart_prompt_enabled: v })}
          />
        </Field>

        <div className="prompt-builder">
          <label className="prompt-label" htmlFor="smart-prompt-source">
            Prompt для обработки
          </label>
          <textarea
            id="smart-prompt-source"
            value={promptText}
            onChange={(e) => updateSmartPrompt(e.currentTarget.value)}
            placeholder='Например: я хочу, чтобы это звучало как печатный текст. Делай из сбивчивой диктовки готовый промпт для нейросети: коротко, структурно, без воды.'
            rows={4}
          />
          <div
            className={[
              "prompt-status",
              promptReady && aiReady ? "is-ok" : "",
              promptReady && !aiReady ? "is-warn" : "",
            ]
              .filter(Boolean)
              .join(" ")}
          >
            {promptReady && aiReady
              ? "Инструкция сохранится автоматически и будет применяться при следующей диктовке."
              : promptReady
                ? "Инструкция сохранена, но для нейросетевого переписывания включите бэкенд ИИ."
                : "Напишите инструкцию обычным языком. VoxFlow сам превратит её во внутренний prompt."}
          </div>
          <div className="prompt-actions">
            <button
              type="button"
              className={[
                "btn",
                isRecordingInstruction ? "voice-recording" : "",
              ]
                .filter(Boolean)
                .join(" ")}
              onClick={
                isRecordingInstruction
                  ? stopVoiceInstruction
                  : startVoiceInstruction
              }
              disabled={isRewritingPrompt}
            >
              <Icon.Mic className="ico" />
              {isRecordingInstruction ? "Остановить" : "Голосовая правка"}
            </button>
            <button
              type="button"
              className="btn btn-primary"
              onClick={() =>
                update({
                  smart_prompt_instruction: buildSmartPromptInstruction(
                    promptText,
                  ),
                })
              }
              disabled={!promptText.trim()}
            >
              <Icon.Sparkles className="ico" />
              Обновить prompt
            </button>
          </div>
          <div className="voice-rewrite-panel">
            <div className="voice-rewrite-top">
              <span>Инструкция для переработки</span>
              <button
                type="button"
                className="btn btn-sm btn-ghost"
                onClick={() => void rewritePromptFromInstruction(voiceInstruction)}
                disabled={isRecordingInstruction || isRewritingPrompt}
              >
                <Icon.Sparkles className="ico" />
                {isRewritingPrompt ? "Работаю..." : "Переработать"}
              </button>
            </div>
            <textarea
              ref={voiceInstructionRef}
              value={voiceInstruction}
              onChange={(e) => setVoiceInstruction(e.currentTarget.value)}
              placeholder='Скажите или введите: "сделай более структурированным", "сократи", "добавь детали", "сделай для нейросети".'
              rows={2}
            />
            {!hasBuiltInVoiceInput && !speechSupported && (
              <div className="prompt-status is-warn">
                Голосовой ввод не поддерживается этим браузером/WebView. Текстовая инструкция работает без ограничений.
              </div>
            )}
            {voiceStatus && <div className="prompt-status is-ok">{voiceStatus}</div>}
            {voiceError && <div className="prompt-status is-warn">{voiceError}</div>}
            {rewritePreview && (
              <div className="voice-rewrite-result">
                <label className="prompt-label" htmlFor="voice-rewrite-preview">
                  Preview
                </label>
                <textarea
                  id="voice-rewrite-preview"
                  value={rewritePreview}
                  onChange={(e) => setRewritePreview(e.currentTarget.value)}
                  rows={5}
                />
                <div className="voice-rewrite-actions">
                  <button
                    type="button"
                    className="btn btn-primary"
                    onClick={applyRewritePreview}
                  >
                    <Icon.Check className="ico" />
                    Применить
                  </button>
                  <button
                    type="button"
                    className="btn"
                    onClick={cancelRewritePreview}
                  >
                    Отменить
                  </button>
                </div>
              </div>
            )}
          </div>
          <label className="prompt-label" htmlFor="smart-prompt-instruction">
            Внутренняя инструкция модели
          </label>
          <textarea
            id="smart-prompt-instruction"
            value={settings.smart_prompt_instruction}
            onChange={(e) =>
              update({ smart_prompt_instruction: e.currentTarget.value })
            }
            placeholder="Заполняется автоматически из prompt выше; можно вручную уточнить, если нужен более строгий стиль"
            rows={5}
          />
        </div>
      </div>
      )}
    </>
  );

  const advanced = (
    <>
        <Field
          label="Способ вставки"
          hint="Через буфер обмена или имитацией набора"
        >
          <Select
            value={settings.paste_method}
            onChange={(v) => update({ paste_method: v })}
            options={[
              { value: "clipboard", label: "Буфер обмена" },
              { value: "type", label: "Набор клавишами" },
            ]}
          />
        </Field>
        <Field
          label="Печатать в поле во время речи"
          hint="«Нет» — текст вставится, когда отпустите клавишу"
        >
          <Select
            value={settings.stream_mode}
            onChange={(v) => update({ stream_mode: v })}
            options={[
              { value: "never", label: "Нет" },
              { value: "auto", label: "Готовую часть фразы" },
              { value: "always", label: "Всё сразу (дёргано)" },
            ]}
          />
        </Field>
        <Field
          label="Сохранять записи для обучения"
          hint="Пары звук ↔ текст, только на этом компьютере"
        >
          <Switch
            checked={settings.personalize}
            onChange={(v) => update({ personalize: v })}
          />
        </Field>
    </>
  );

  if (part === "main") return main;
  if (part === "advanced") return advanced;
  return (
    <SectionShell embedded={embedded}>
      {!embedded && (
        <PageHead
          title="Распознавание"
          desc="Как обрабатывать распознанный текст перед вставкой."
        />
      )}
      {main}
      <div className="card">{advanced}</div>
    </SectionShell>
  );
}
