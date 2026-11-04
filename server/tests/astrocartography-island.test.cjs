// Run with: node --experimental-vm-modules --test server/tests/astrocartography-island.test.cjs
// Exercise the shipped boot script and its DOM events with a strict map adapter.
const assert = require("node:assert/strict");
const { readFileSync } = require("node:fs");
const { resolve } = require("node:path");
const { test } = require("node:test");
const vm = require("node:vm");
const source = readFileSync(resolve(__dirname, "../public/js/astrocartography-island.js"), "utf8");

class Element {
  constructor() { this.children = []; this.listeners = {}; this.dataset = {}; }
  appendChild(child) { this.children.push(child); }
  addEventListener(name, handler) { this.listeners[name] = handler; }
  querySelector(selector) {
    return this.children.find(child => selector === "." + child.className) || null;
  }
  change(checked) { this.checked = checked; this.listeners.change(); }
  descendants() { return this.children.flatMap(child => [child, ...child.descendants()]); }
}

async function boot(payload, errorBeforeLoad = false) {
  let ready;
  const mount = new Element();
  mount.dataset.moduleSrc = "test-map-module";
  const sun = new Element();
  sun.value = "sun";
  sun.checked = true;
  const maps = [];
  class Map {
    constructor() { this.layers = new globalThis.Map(); this.sources = []; this.handlers = {}; maps.push(this); }
    on(event, handler) {
      this.handlers[event] = handler;
      if (event === "load") queueMicrotask(() => {
        if (errorBeforeLoad) this.handlers.error();
        handler();
      });
    }
    addSource(id, source) { this.sources.push(source); }
    addLayer(layer) {
      assert.equal(layer.layout["line-dasharray"], undefined, "dash arrays are paint properties");
      assert.ok(Object.keys(layer.layout).every(key => ["line-cap", "line-join"].includes(key)));
      this.layers.set(layer.id, layer);
    }
    getLayer(id) { return this.layers.get(id); }
    setLayoutProperty(id, key, value) { this.getLayer(id).layout[key] = value; }
  }
  // A real module namespace satisfies vm's dynamic-import contract without
  // rewriting the production source or exposing its private functions.
  const key = "__astroTestMap" + Math.random().toString(36).slice(2);
  globalThis[key] = Map;
  const moduleUrl = "data:text/javascript," + encodeURIComponent(
    `export const Map = globalThis[${JSON.stringify(key)}];`
  );
  const document = {
    readyState: "loading",
    getElementById: () => ({ textContent: payload }),
    querySelectorAll: selector => selector.startsWith("input") ? [sun] : [mount],
    addEventListener: (name, handler) => { ready = handler; },
    createElement: () => new Element(),
    createTextNode: text => Object.assign(new Element(), { textContent: text })
  };
  const script = new vm.Script(source, { importModuleDynamically: () => import(moduleUrl) });
  script.runInNewContext({ window: {}, document, console });
  ready();
  await new Promise(resolve => setImmediate(resolve));
  delete globalThis[key];
  return { mount, sun, map: maps[0] };
}

function persons() {
  return ["first", "second"].map(id => ({
    id, label: id,
    geojson: { type: "FeatureCollection", features: ["mc", "ic", "asc", "dsc"].map(angle => ({
      type: "Feature", properties: { body: "sun", angle },
      geometry: { type: "MultiLineString", coordinates: [[[0, 0], [1, 1]]] }
    })) }
  }));
}

test("all four angles load with valid paint and combined body, angle and person visibility", async () => {
  const { mount, sun, map } = await boot(JSON.stringify({ persons: persons() }));
  assert.equal(mount.dataset.live, "true");
  assert.equal(map.layers.size, 8);
  assert.deepEqual(Array.from(map.layers.values()).filter(layer => layer.paint["line-dasharray"]).length, 6);
  const control = (name, value) => mount.descendants().find(el => el.name === name && el.value === value);
  const mc = control("astro-map-island-angle", "mc");
  const first = control("astro-map-island-person", "first");
  const visibility = id => map.getLayer(id).layout.visibility;
  sun.change(false);
  mc.change(false);
  mc.change(true);
  assert.equal(visibility("astro-lines-first-sun-mc"), "none", "angle cannot reveal a hidden body");
  sun.change(true);
  mc.change(false);
  sun.change(false);
  sun.change(true);
  assert.equal(visibility("astro-lines-first-sun-mc"), "none", "body cannot reveal a hidden angle");
  first.change(false);
  mc.change(true);
  assert.equal(visibility("astro-lines-first-sun-mc"), "none", "angle cannot reveal a hidden person");
  assert.equal(visibility("astro-lines-second-sun-mc"), "visible");
  first.change(true);
  assert.equal(visibility("astro-lines-first-sun-mc"), "visible");
});

test("hydration wrappers do not alter comment-like text inside JSON values", async () => {
  const data = persons();
  data[0].label = "<!-- preserved data -->";
  const payload = JSON.stringify({ persons: data });
  const { mount, map } = await boot("<!--node-id0-->" + payload + "<!--node-id1-->");
  assert.equal(mount.dataset.live, "true");
  assert.equal(map.layers.size, 8);
  assert.ok(mount.descendants().some(el => el.textContent === "<!-- preserved data -->"));
});

test("tile errors restore the static fallback before and after map load", async () => {
  for (const early of [false, true]) {
    const { mount, map } = await boot(JSON.stringify({ persons: persons() }), early);
    if (!early) {
      assert.equal(mount.dataset.live, "true");
      map.handlers.error();
    }
    assert.notEqual(mount.dataset.live, "true", "CSS must reveal the SVG fallback");
    assert.match(mount.querySelector(".astro-map-island__status").textContent, /static map below remains available/);
    assert.equal(map.layers.size, 8, "calculated layers remain available in the degraded map");
  }
});
