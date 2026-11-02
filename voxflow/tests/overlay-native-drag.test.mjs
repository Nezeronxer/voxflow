import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import * as ts from "typescript";

const source = await readFile(new URL("../src/Overlay.tsx", import.meta.url), "utf8");
const start = source.indexOf("  const onPillPointerDown =");
const end = source.indexOf("\n  return (", start);
assert.ok(start > 0 && end > start);
const compiled = ts.transpileModule(source.slice(start, end), {
  compilerOptions: { target: ts.ScriptTarget.ES2020 },
}).outputText;

function nativeHandlers() {
  const calls = [];
  const pointerRef = { current: null };
  const forbidden = () => { throw new Error("native drag used global cursor/window IPC"); };
  const factory = new Function(
    "nativeDragRef", "pointerRef", "pillRef", "pointInPillHit", "window",
    "cursorPosition", "scheduleManualDrag", "applyManualDrag", "invoke", "layerPositionRef", "moveLayerPill", "saveLayerPosition", "clampLayerToViewport",
    `${compiled}\nreturn { onPillPointerDown, onPillPointerMove, onPillPointerUp, onPillPointerCancel };`,
  );
  const handlers = factory(
    { current: true }, pointerRef, { current: { contains: () => true, getBoundingClientRect: () => ({left:100,right:140,top:40,bottom:50}) } }, () => true,
    { devicePixelRatio: 2 }, forbidden, forbidden, forbidden,
    async (command) => { calls.push(command); },
    { current: { x: 0, y: -156 } }, () => { calls.push("local-move"); }, () => { calls.push("local-save"); }, () => {},
  );
  const event = (x, y) => ({
    button: 0, pointerId: 1, clientX: x, clientY: y, screenX: 0, screenY: 0,
    target: {}, currentTarget: { setPointerCapture() {}, releasePointerCapture() {} },
    preventDefault() {},
  });
  return { handlers, calls, pointerRef, event };
}

test("Wayland layer short tap still toggles dictation without global cursor IPC", async () => {
  const { handlers, calls, event } = nativeHandlers();
  handlers.onPillPointerDown(event(120, 45));
  handlers.onPillPointerUp(event(120, 45));
  await Promise.resolve();
  assert.deepEqual(calls, ["overlay_click"]);
});

test("Wayland local drag never starts dictation or commits a fake global position", async () => {
  for (const finish of ["onPillPointerUp", "onPillPointerCancel"]) {
    const { handlers, calls, pointerRef, event } = nativeHandlers();
    handlers.onPillPointerDown(event(120, 45));
    handlers.onPillPointerMove(event(132, 52));
    handlers[finish](event(132, 52));
    await Promise.resolve();
    assert.equal(pointerRef.current, null);
    assert.deepEqual(calls, finish === "onPillPointerUp" ? ["local-move", "local-move", "local-save"] : ["local-move", "local-save"]);
  }
});


const dragSource = await readFile(new URL("../src/overlayDragState.ts", import.meta.url), "utf8");
const dragCompiled = ts.transpileModule(dragSource, { compilerOptions: { module: ts.ModuleKind.ESNext } }).outputText;
const { layerDragPosition } = await import(`data:text/javascript;base64,${Buffer.from(dragCompiled).toString("base64")}`);

test("queued pointer motions use the fixed grab instead of adding old offsets", () => {
  const grab = { position: {x:0,y:-156}, pointer: {x:120,y:600}, rect: {left:100,right:140,top:590,bottom:610} };
  const viewport = {width:1920,height:1080};
  assert.deepEqual(layerDragPosition(grab, {x:124,y:602}, viewport), {x:4,y:-154});
  assert.deepEqual(layerDragPosition(grab, {x:126,y:603}, viewport), {x:6,y:-153});
  assert.deepEqual(layerDragPosition(grab, {x:120,y:600}, viewport), {x:0,y:-156});
  assert.deepEqual(layerDragPosition(grab, {x:9999,y:9999}, viewport), {x:1772,y:306});
});

test("mode growth and monitor resize re-clamp the capsule, preserving active grabs", () => {
  const from = source.indexOf("  const clampLayerToViewport =");
  const to = source.indexOf("  const moveLayerPill =", from);
  assert.ok(from > 0 && to > from);
  const code = ts.transpileModule(source.slice(from, to), { compilerOptions: {target: ts.ScriptTarget.ES2020} }).outputText;
  const pointerRef = { current: null };
  let applied;
  const clamp = new Function("nativeDragRef", "pointerRef", "pillRef", "layerPositionRef", "layerDragPosition", "window", "applyLayerPosition", `${code}\nreturn clampLayerToViewport;`)(
    {current: true}, pointerRef,
    {current: {getBoundingClientRect: () => ({left:-20,right:76,top:-16,bottom:18})}},
    {current: {x:-932,y:-900}}, layerDragPosition, {innerWidth:1920,innerHeight:1080},
    (position) => {applied = position;},
  );
  clamp();
  assert.deepEqual(applied, {x:-904,y:-876});
  applied = null;
  pointerRef.current = {};
  clamp();
  assert.equal(applied, null);
});
