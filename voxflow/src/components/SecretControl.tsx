import { useEffect, useState } from "react";
import { clearSecret, getSecretStatus, subscribe } from "../api";
import type { SecretKind, SecretStatus } from "../types";

const EMPTY_STATUS: SecretStatus = {
  ai_api_key: false,
  oai_stt_key: false,
  deepgram_key: false,
  rewrite_key: false,
};

export default function SecretControl({
  kind,
  value,
  onChange,
  width = 260,
}: {
  kind: SecretKind;
  value: string;
  onChange: (value: string) => void;
  width?: number;
}) {
  const [status, setStatus] = useState<SecretStatus>(EMPTY_STATUS);
  const [clearing, setClearing] = useState(false);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let alive = true;
    void getSecretStatus().then((next) => {
      if (alive) setStatus(next);
    });
    const off = subscribe<SecretStatus>("secret_status", (event) => {
      if (event.payload) setStatus(event.payload);
    });
    return () => {
      alive = false;
      off();
    };
  }, []);

  async function onClear() {
    setClearing(true);
    setFailed(false);
    const ok = await clearSecret(kind);
    setClearing(false);
    if (!ok) {
      setFailed(true);
      return;
    }
    onChange("");
    setStatus((previous) => ({ ...previous, [kind]: false }));
  }

  return (
    <div className="secret-control" style={{ width }}>
      <input
        type="password"
        aria-label="API-ключ"
        placeholder={status[kind] && !value ? "Ключ сохранён" : "Вставьте ключ"}
        value={value}
        onChange={(event) => {
          setFailed(false);
          onChange(event.currentTarget.value);
        }}
      />
      {status[kind] && !value && (
        <button type="button" onClick={onClear} disabled={clearing}>
          {clearing ? "Удаляю…" : "Удалить"}
        </button>
      )}
      {failed && <span role="alert">Не удалось удалить ключ</span>}
    </div>
  );
}
