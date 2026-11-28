import { setImmediate } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { JSDOM } from 'jsdom';
import { build } from 'vite';

// Run the same entry point as the app in an isolated DOM with a controllable polling clock.
export async function bundle(entry) {
  const [result] = await build({
    configFile: false,
    logLevel: 'silent',
    build: { write: false, target: 'esnext', minify: false, lib: { entry: fileURLToPath(entry), formats: ['es'] } },
  });
  return result.output.find(file => file.type === 'chunk').code;
}

export async function mount(t, code, setup = () => {}) {
  const { window } = new JSDOM('<!doctype html><div id="app"></div>', {
    url: 'https://preview.invalid/', pretendToBeVisual: true, runScripts: 'outside-only',
  });
  t.after(() => window.close());
  const intervals = new Map();
  window.setInterval = (tick, ms) => { intervals.set(ms, tick); return ms; };
  window.structuredClone = structuredClone;
  setup(window);
  await window.eval(`(async () => {\n${code}\n})()`);
  await setImmediate();
  return { window, document: window.document, tick: () => intervals.get(30_000)() };
}
