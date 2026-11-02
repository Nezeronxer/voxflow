import test from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { build } from "esbuild";

// Render the real settings section before its asynchronous catalog has loaded.
const built = await build({
  stdin: {
    contents: `import React from 'react';
      import {renderToStaticMarkup} from 'react-dom/server';
      import Ai from './src/sections/Ai';
      import {DEFAULT_SETTINGS} from './src/types';
      export function render(patch) {
        return renderToStaticMarkup(React.createElement(Ai, {
          settings: {...DEFAULT_SETTINGS, ai_backend: 'openai_compat',
            rewrite_base_url: 'https://openrouter.ai/api/v1', ...patch},
          update: () => {}, part: 'main'
        }));
      }`,
    resolveDir: new URL("..", import.meta.url).pathname,
  },
  bundle: true, platform: "node", format: "cjs", packages: "external", write: false,
});
const mod = { exports: {} };
new Function("require", "module", "exports", built.outputFiles[0].text)(
  createRequire(import.meta.url), mod, mod.exports,
);
const { render } = mod.exports;

function picker(html) {
  return html.match(/<select[^>]*aria-label="Модель"[^>]*>([\s\S]*?)<\/select>/)?.[1];
}

test("OpenRouter has a model dropdown before the catalog arrives", () => {
  const html = render({rewrite_model: "anthropic/claude-haiku-5.5"});
  assert.ok(picker(html), "OpenRouter must keep a model Select while loading or offline");
  assert.ok((picker(html).match(/<option /g) ?? []).length > 20, "offline catalog must offer real alternatives");
  assert.match(picker(html), /value="anthropic\/claude-haiku-5.5" selected/);
  assert.doesNotMatch(html, /list="rewrite-model-options"/);
});

test("an existing custom OpenRouter model survives a missing catalog", () => {
  const options = picker(render({rewrite_model: "vendor/saved-model"}));
  assert.ok(options);
  assert.match(options, /value="vendor\/saved-model" selected/);
});

test("empty model has an honest placeholder and a catalog retry action", () => {
  const html = render({rewrite_model: ""});
  assert.match(picker(html) ?? "", /value="" selected/);
  assert.match(html, /Обновить список/);
});
