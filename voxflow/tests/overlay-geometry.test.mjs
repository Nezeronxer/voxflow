import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const css = await readFile(new URL("../src/overlay.css", import.meta.url), "utf8");

function finalRule(selector) {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const matches = [...css.matchAll(new RegExp(`${escaped}\\s*\\{([^}]*)\\}`, "g"))];
  assert.ok(matches.length > 0, `missing CSS rule for ${selector}`);
  return matches.at(-1)[1];
}

function declaration(rule, property) {
  const escaped = property.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`(?:^|;)\\s*${escaped}\\s*:\\s*([^;]+)`, "m").exec(rule)?.[1].trim();
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
