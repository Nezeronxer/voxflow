#!/usr/bin/env python3
"""Refresh the offline picker from OpenRouter's public catalog (no API key)."""

import datetime
import json
import urllib.request
from pathlib import Path

SOURCE = "https://openrouter.ai/api/v1/models"


def main():
    with urllib.request.urlopen(SOURCE, timeout=30) as response:
        models = json.load(response)["data"]
    options = {}
    for model in models:
        modalities = model.get("architecture", {}).get("output_modalities", [])
        if model.get("is_ready") is False or model["id"].endswith(":batch"):
            continue
        if modalities and "text" not in modalities:
            continue
        try:
            pricing = model["pricing"]
            if min(float(pricing["prompt"]), float(pricing["completion"])) < 0:
                continue
        except (KeyError, TypeError, ValueError):
            continue
        options[model["id"]] = model.get("name") or model["id"]
    if len(options) < 20:
        raise ValueError("Unexpectedly small catalog; keep the existing snapshot")
    snapshot = {
        "source": SOURCE,
        "updated": datetime.date.today().isoformat(),
        "models": [
            {"value": value, "label": label}
            for value, label in sorted(options.items(), key=lambda row: row[1].casefold())
        ],
    }
    target = Path(__file__).resolve().parents[1] / "voxflow/src/openrouter-catalog.json"
    target.write_text(json.dumps(snapshot, ensure_ascii=False, indent=2) + "\n")
    print(f"Saved {len(options)} text models")


if __name__ == "__main__":
    main()
