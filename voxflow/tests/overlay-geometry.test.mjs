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

test("final v2 cascade preserves the 13px dictation orb contract", () => {
  const orb = finalRule(".aq-orbwrap");
  assert.equal(declaration(orb, "width"), "13px");
  assert.equal(declaration(orb, "height"), "13px");

  const glow = finalRule(".aq-orb-glow");
  assert.equal(declaration(glow, "width"), "26px");
  assert.equal(declaration(glow, "height"), "26px");
  assert.equal(declaration(glow, "margin"), "-13px 0 0 -13px");

  const ring = finalRule(".aq-ring");
  assert.equal(declaration(ring, "width"), "14.5px");
  assert.equal(declaration(ring, "height"), "14.5px");
  assert.equal(declaration(ring, "margin"), "-7.25px 0 0 -7.25px");

  const orbPaint = finalRule(".aq-orb");
  assert.equal(
    declaration(orbPaint, "filter"),
    "drop-shadow(0 0 2px rgba(111, 131, 255, 0.42))",
  );
});

test("floating bar remains compact and every CSS state fits its Tauri window box", () => {
  const expectedBoxes = {
    idle: { w: 266, h: 60 },
    rec: { w: 260, h: 66 },
    trans: { w: 256, h: 64 },
    stream: { w: 384, h: 104 },
    latch: { w: 264, h: 66 },
    notice: { w: 356, h: 70 },
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
  const contracts = [
    ["idle", ".aq-idle", "width", "height"],
    ["rec", ".aq-rec", "width", "height"],
    ["trans", ".aq-trans", "width", "height"],
    ["stream", ".aq-stream", "max-width", "max-height"],
    ["latch", ".aq-latch", "width", "height"],
  ];
  for (const [mode, selector, widthProperty, heightProperty] of contracts) {
    const rule = finalRule(selector);
    const contentWidth = px(declaration(rule, widthProperty));
    const contentHeight = px(declaration(rule, heightProperty));
    assert.ok(
      contentWidth + stageHorizontal <= expectedBoxes[mode].w,
      `${mode} width exceeds BOX`,
    );
    assert.ok(
      contentHeight + stageVertical <= expectedBoxes[mode].h,
      `${mode} height exceeds BOX`,
    );
  }

  assert.equal(declaration(finalRule(".aq-idle"), "width"), "56px");
  assert.equal(declaration(finalRule(".aq-idle"), "height"), "10px");
  assert.equal(declaration(finalRule(".aq-idle:hover"), "width"), "172px");
  assert.equal(declaration(finalRule(".aq-idle:hover"), "height"), "30px");
  assert.ok(
    px(declaration(finalRule(".aq-idle:hover"), "width")) + stageHorizontal <=
      expectedBoxes.idle.w,
    "expanded idle hover must fit the fixed idle Tauri window",
  );
  assert.equal(declaration(finalRule(".aq-stream"), "max-width"), "270px");
  assert.equal(declaration(finalRule(".aq-notice"), "max-width"), "270px");
});

test("idle is a tiny outlined capsule and reveals the rich controls only on hover", () => {
  const idle = finalRule(".aq-idle");
  assert.equal(declaration(idle, "border-radius"), "999px");
  assert.equal(declaration(idle, "overflow"), "hidden");

  const idleCopy = finalRule(".aq-idle-copy");
  assert.equal(declaration(idleCopy, "opacity"), "0");
  assert.equal(declaration(finalRule(".aq-idle:hover .aq-idle-copy"), "opacity"), "1");
});

test("live preview survives transcribing and frame updates bypass React state", () => {
  const transcribingBranch = /else if \(v === "transcribing"\) \{([\s\S]*?)\n\s*\} else \{/.exec(
    overlaySource,
  );
  assert.ok(transcribingBranch, "missing transcribing status branch");
  assert.doesNotMatch(transcribingBranch[1], /resetTextEngine\(\)/);
  assert.match(transcribingBranch[1], /setShownDirect\(targetCharsRef\.current\.length\)/);
  assert.doesNotMatch(overlaySource, /const \[shown,\s*setShown\]/);
  assert.match(overlaySource, /committedTextRef\.current\.textContent/);
  assert.match(overlaySource, /el\.style\.transform = `scaleY/);
  assert.match(overlaySource, /seq\?: number/);
  assert.match(overlaySource, /p\?\.latched === true/);
  assert.match(overlaySource, /if \(seq > currentSeqRef\.current\) currentSeqRef\.current = seq/);
});

test("recording and double-tap latch share geometry without a second pop animation", () => {
  const recording = finalRule(".aq-rec");
  const latch = finalRule(".aq-latch");
  assert.equal(declaration(latch, "width"), declaration(recording, "width"));
  assert.equal(declaration(latch, "height"), declaration(recording, "height"));
  assert.equal(declaration(latch, "animation"), "none");
});

test("overlay honors reduced motion for both CSS and rAF-driven animation", () => {
  assert.match(overlaySource, /matchMedia\("\(prefers-reduced-motion: reduce\)"\)/);
  assert.match(overlaySource, /if \(reducedMotionRef\.current\)/);
  assert.doesNotMatch(overlaySource, /SPRING_[KC]/);
  assert.match(overlaySource, /1 - Math\.exp\(-dt \/ tau\)/);
  const reducedMotion = /@media \(prefers-reduced-motion: reduce\) \{([\s\S]*)\}\s*$/.exec(css);
  assert.ok(reducedMotion, "missing final reduced-motion cascade");
  assert.match(reducedMotion[1], /\.aq-bar/);
  assert.match(reducedMotion[1], /\.aq-orb-glow/);
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
