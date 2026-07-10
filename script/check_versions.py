#!/usr/bin/env python3
"""Fail when VoxFlow package, Cargo, Tauri, Inno, or tag versions diverge."""

from __future__ import annotations

import argparse
import json
import re
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SEMVER = re.compile(r"^\d+\.\d+\.\d+$")


def load_json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def capture(pattern: str, text: str, label: str) -> str:
    match = re.search(pattern, text, flags=re.MULTILINE)
    if not match:
        raise ValueError(f"cannot read {label}")
    return match.group(1)


def normalized_tag(tag: str) -> str:
    value = tag.strip().rsplit("/", 1)[-1]
    return value[1:] if value.lower().startswith("v") else value


def collect_versions() -> tuple[dict[str, str], str]:
    package = load_json(ROOT / "voxflow/package.json")
    package_lock = load_json(ROOT / "voxflow/package-lock.json")
    tauri = load_json(ROOT / "voxflow/src-tauri/tauri.conf.json")
    cargo = tomllib.loads(
        (ROOT / "voxflow/src-tauri/Cargo.toml").read_text(encoding="utf-8")
    )
    inno_text = (ROOT / "installer/VoxFlow.iss").read_text(encoding="utf-8-sig")

    cargo_package = str(cargo.get("package", {}).get("version", ""))
    inno_version = capture(
        r'^#define\s+AppVersion\s+"([^"]+)"', inno_text, "Inno AppVersion"
    )
    inno_file_version = capture(
        r"^VersionInfoVersion\s*=\s*([^\s;]+)",
        inno_text,
        "Inno VersionInfoVersion",
    )

    versions = {
        "package.json": str(package.get("version", "")),
        "package-lock.json": str(package_lock.get("version", "")),
        "package-lock packages['']": str(
            package_lock.get("packages", {}).get("", {}).get("version", "")
        ),
        "Cargo.toml": cargo_package,
        "tauri.conf.json": str(tauri.get("version", "")),
        "VoxFlow.iss": inno_version,
    }
    return versions, inno_file_version


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--tag",
        default="",
        help="Optional release tag/ref; a leading v and refs/tags/ are accepted.",
    )
    args = parser.parse_args()

    try:
        versions, inno_file_version = collect_versions()
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        print(f"version check failed: {exc}", file=sys.stderr)
        return 2

    expected = versions["package.json"]
    errors: list[str] = []
    if not SEMVER.fullmatch(expected):
        errors.append(f"package.json must use stable x.y.z semver, got {expected!r}")

    for source, version in versions.items():
        if version != expected:
            errors.append(f"{source}: {version!r}, expected {expected!r}")

    expected_file_version = f"{expected}.0"
    if inno_file_version != expected_file_version:
        errors.append(
            "VoxFlow.iss VersionInfoVersion: "
            f"{inno_file_version!r}, expected {expected_file_version!r}"
        )

    if args.tag:
        tag_version = normalized_tag(args.tag)
        if tag_version != expected:
            errors.append(f"release tag: {tag_version!r}, expected {expected!r}")

    if errors:
        print("Version consistency check failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1

    print(f"Version consistency: {expected}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
