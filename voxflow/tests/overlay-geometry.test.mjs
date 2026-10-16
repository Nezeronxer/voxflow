import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const [css, overlaySource, rustSource, capabilitySource, tauriConfigSource] = await Promise.all([
  readFile(new URL("../src/overlay.css", import.meta.url), "utf8"),
  readFile(new URL("../src/Overlay.tsx", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/capabilities/default.json", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
]);

function finalRule(selector) {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const matches = [
    ...css.matchAll(new RegExp(`(?:^|\\n)\\s*${escaped}\\s*\\{([^}]*)\\}`, "g")),
  ];
  assert.ok(matches.length > 0, `missing CSS rule for ${selector}`);
  return matches.at(-1)[1];
}

function declaration(rule, property) {
  const escaped = property.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`(?:^|;)\\s*${escaped}\\s*:\\s*([^;]+)`, "m").exec(rule)?.[1].trim();
}

function px(value) {
  assert.match(value, /^\d+(?:\.\d+)?px$/, `expected px value, got ${value}`);
  return Number.parseFloat(value);
}

function overlayBox(mode) {
  const match = new RegExp(
    `\\b${mode}:\\s*\\{\\s*w:\\s*(\\d+),\\s*h:\\s*(\\d+)\\s*\\}`,
  ).exec(overlaySource);
  assert.ok(match, `missing BOX entry for ${mode}`);
  return { w: Number(match[1]), h: Number(match[2]) };
}

test("capsule is a dark matte pill without the old orb, glow or gradients", () => {
  const pill = finalRule(".aq-pill");
  assert.equal(declaration(pill, "border-radius"), "999px");
  assert.equal(declaration(pill, "background"), "rgba(17, 17, 20, 0.88)");
  assert.equal(declaration(pill, "backdrop-filter"), "blur(20px)");
  assert.doesNotMatch(css, /aq-orb|aq-ring|radial-gradient/);
  assert.doesNotMatch(overlaySource, /aq-orb|glowEl|Слушаю|Улучшаю текст/);
  // Полоски - белые, без градиента.
  assert.equal(declaration(finalRule(".aq-bar"), "background"), "#f2f2f5");
});

test("capsule stays compact and every CSS state fits its Tauri window box", () => {
  const expectedBoxes = {
    idle: { w: 266, h: 60 },
    rec: { w: 180, h: 56 },
    trans: { w: 164, h: 56 },
    stream: { w: 446, h: 82 },
    latch: { w: 306, h: 56 },
    notice: { w: 346, h: 74 },
  };
  assert.deepEqual(
    Object.fromEntries(Object.keys(expectedBoxes).map((mode) => [mode, overlayBox(mode)])),
    expectedBoxes,
  );

  const rustIdle = /const OVERLAY_IDLE_BOX:\s*\(i32, i32\)\s*=\s*\((\d+),\s*(\d+)\)/.exec(
    rustSource,
  );
  assert.ok(rustIdle, "missing Rust idle overlay box");
  assert.deepEqual(
    { w: Number(rustIdle[1]), h: Number(rustIdle[2]) },
    expectedBoxes.idle,
  );
  const tauriOverlay = JSON.parse(tauriConfigSource).app.windows.find(
    (window) => window.label === "overlay",
  );
  assert.deepEqual(
    { w: tauriOverlay?.width, h: tauriOverlay?.height },
    expectedBoxes.idle,
    "Tauri's initial overlay size must not flash the old oversized window",
  );

  const stage = finalRule(".aq-scale-stage");
  assert.equal(declaration(stage, "padding"), "8px 9px 10px");
  const stageHorizontal = 18;
  const stageVertical = 18;
  // Крайний размер каждого режима: с меткой модели для rec/trans, max-* для растущих.
  const contracts = [
    ["idle", ".aq-idle", "width", ".aq-idle", "height"],
    ["rec", ".aq-rec.aq-has-meta", "width", ".aq-rec", "height"],
    ["trans", ".aq-trans.aq-has-meta", "width", ".aq-trans", "height"],
    ["stream", ".aq-stream", "max-width", ".aq-stream", "max-height"],
    ["latch", ".aq-latch", "max-width", ".aq-latch", "height"],
    ["notice", ".aq-notice", "max-width", ".aq-notice", "max-height"],
  ];
  for (const [mode, wSelector, widthProperty, hSelector, heightProperty] of contracts) {
    const contentWidth = px(declaration(finalRule(wSelector), widthProperty));
    const contentHeight = px(declaration(finalRule(hSelector), heightProperty));
    assert.ok(contentWidth + stageHorizontal <= expectedBoxes[mode].w, `${mode} width exceeds BOX`);
    assert.ok(contentHeight + stageVertical <= expectedBoxes[mode].h, `${mode} height exceeds BOX`);
  }

  // Размеры состояний по спецификации §1-§2.
  const size = (selector) => [
    declaration(finalRule(selector), "width"),
    declaration(finalRule(selector), "height"),
  ];
  assert.deepEqual(size(".aq-idle"), ["40px", "10px"]);
  assert.equal(declaration(finalRule(".aq-idle"), "opacity"), "0.55");
  assert.deepEqual(size(".aq-rec"), ["96px", "34px"]);
  assert.deepEqual(size(".aq-trans"), ["64px", "34px"]);
  assert.equal(declaration(finalRule(".aq-stream"), "min-width"), "160px");
  assert.equal(declaration(finalRule(".aq-stream"), "max-width"), "420px");
  assert.equal(declaration(finalRule(".aq-stream"), "min-height"), "34px");
  assert.equal(declaration(finalRule(".aq-stream"), "max-height"), "60px");
  // Однострочный stream = rec по высоте: паддинги + строка 18 + рамка 2.
  const [padY] = declaration(finalRule(".aq-stream"), "padding").split(" ");
  const lineH = px(declaration(finalRule(".aq-text"), "line-height"));
  assert.equal(2 * px(padY) + lineH + 2, 34);
  assert.equal(declaration(finalRule(".aq-offline"), "pointer-events"), "auto");
});

test("idle is a tiny muted capsule and reveals the hotkey hint only on hover", () => {
  assert.equal(declaration(finalRule(".aq-tip"), "opacity"), "0");
  assert.equal(declaration(finalRule(".aq-idle:hover .aq-tip"), "opacity"), "1");
  assert.match(overlaySource, /Зажмите \{hotkeyTip\} - диктовка/);
});

test("live preview survives transcribing and word updates bypass React state", () => {
  const transcribingBranch = /else if \(v === "transcribing"\) \{([\s\S]*?)\n\s*\} else \{/.exec(
    overlaySource,
  );
  assert.ok(transcribingBranch, "missing transcribing status branch");
  assert.doesNotMatch(transcribingBranch[1], /resetTextEngine\(\)/);
  assert.doesNotMatch(overlaySource, /const \[shown,\s*setShown\]/);
  // Слова пишутся в DOM напрямую, совпавший префикс не пересоздаётся.
  assert.match(overlaySource, /host\.appendChild\(el\)/);
  assert.match(overlaySource, /sharedWordPrefix\(tokensRef\.current, tokens\)/);
  // Вставленный финал в плашке не показывается.
  assert.match(overlaySource, /if \(preview == null \|\| preview\.isFinal\) return;/);
  assert.match(overlaySource, /el\.style\.transform = `scaleY/);
  assert.match(overlaySource, /seq\?: number/);
  assert.match(overlaySource, /p\?\.latched === true/);
  assert.match(overlaySource, /if \(seq > currentSeqRef\.current\) currentSeqRef\.current = seq/);
});

test("double-tap latch is the same one-line capsule with plain text", () => {
  assert.equal(declaration(finalRule(".aq-latch"), "height"), declaration(finalRule(".aq-rec"), "height"));
  assert.doesNotMatch(overlaySource, /2×|aq-latch-mark/);
  assert.match(overlaySource, /latchNotice\?\.message \|\| "Без удержания"/);
});

test("status model field drives the meta label and is optional", () => {
  assert.match(overlaySource, /model\?: string \| null/);
  assert.match(overlaySource, /applyModel\(typeof p === "object" && p !== null \? p\.model : undefined\)/);
  assert.match(overlaySource, /overlayMetaLabel\(model, lang\)/);
});

test("overlay honors reduced motion for both CSS and rAF-driven animation", () => {
  assert.match(overlaySource, /matchMedia\("\(prefers-reduced-motion: reduce\)"\)/);
  assert.match(overlaySource, /const reducedMotion = reducedMotionRef\.current/);
  assert.match(overlaySource, /fresh && !reducedMotion \? 0\.08/);
  assert.doesNotMatch(overlaySource, /SPRING_[KC]/);
  assert.match(overlaySource, /1 - Math\.exp\(-dt \/ tau\)/);
  const reducedMotion = /@media \(prefers-reduced-motion: reduce\) \{([\s\S]*)\}\s*$/.exec(css);
  assert.ok(reducedMotion, "missing final reduced-motion cascade");
  assert.match(reducedMotion[1], /\.aq-bar/);
  assert.match(reducedMotion[1], /\.aq-dot/);
  assert.match(reducedMotion[1], /animation:\s*none !important/);
});

test("overlay drag uses one captured-pointer path without a macOS CGEvent poller", () => {
  assert.doesNotMatch(overlaySource, /IS_APPLE_PLATFORM/);
  assert.match(
    overlaySource,
    /cursorStart:\s*\{\s*x:\s*e\.screenX \* pixelRatio,\s*y:\s*e\.screenY \* pixelRatio\s*\}/,
    "pointer-down must synchronously capture a physical-pixel drag baseline",
  );
  assert.match(
    overlaySource,
    /if \(!state\.dragging\) state\.cursorStart = \{ x: cur\.x, y: cur\.y \}/,
    "late cursor IPC must not erase motion that already began",
  );
  assert.match(
    overlaySource,
    /const onPillPointerMove[\s\S]*?if \(p\.dragging\) \{[\s\S]*?scheduleManualDrag\(p\)/,
  );
  assert.match(
    overlaySource,
    /const onPillPointerUp[\s\S]*?await applyManualDrag\(p, false\)[\s\S]*?overlay_commit_position/,
  );
  assert.match(
    overlaySource,
    /const onPillPointerCancel[\s\S]*?applyManualDrag\(p, false\)[\s\S]*?overlay_commit_position/,
  );

  assert.doesNotMatch(rustSource, /spawn_overlay_drag_poller|CGEventSourceButtonState/);
  const capabilities = JSON.parse(capabilitySource).permissions;
  assert.ok(capabilities.includes("core:window:allow-cursor-position"));
  assert.ok(capabilities.includes("core:window:allow-outer-position"));
  assert.ok(capabilities.includes("core:window:allow-set-position"));
});
