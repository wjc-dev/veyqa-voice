import { readFileSync } from "node:fs";
import vm from "node:vm";
import test from "node:test";
import assert from "node:assert/strict";

const script = readFileSync(
  new URL("../website/demo.js", import.meta.url),
  "utf8",
);
function fixture(reducedMotion = false) {
  const elements = Object.fromEntries(
    ["demo", "demo-play", "demo-output", "demo-preview", "demo-status"].map(
      (id) => [
        id,
        {
          dataset: {},
          textContent: "",
          hidden: true,
          addEventListener(type, fn) {
            this[type] = fn;
          },
        },
      ],
    ),
  );
  const timers = new Map();
  let serial = 0;
  let observe;
  const reduced = {
    matches: reducedMotion,
    addEventListener(type, fn) {
      this[type] = fn;
    },
  };
  const document = {
    hidden: false,
    getElementById: (id) => elements[id],
    addEventListener(type, fn) {
      this[type] = fn;
    },
  };
  class Observer {
    constructor(fn) {
      observe = fn;
    }
    observe() {}
  }
  vm.runInNewContext(script, {
    document,
    window: { matchMedia: () => reduced, IntersectionObserver: Observer },
    IntersectionObserver: Observer,
    setTimeout: (fn) => {
      timers.set(++serial, fn);
      return serial;
    },
    clearTimeout: (id) => timers.delete(id),
  });
  return {
    elements,
    timers,
    document,
    reduced,
    visible(value) {
      observe([{ isIntersecting: value }]);
    },
    tick() {
      const [id, fn] = timers.entries().next().value;
      timers.delete(id);
      fn();
    },
    phase() {
      return elements.demo.dataset.phase;
    },
  };
}
test("illustration previews first and commits only after release", () => {
  const f = fixture();
  f.visible(true);
  assert.equal(f.phase(), "ready");
  f.tick();
  assert.equal(f.phase(), "listening");
  assert.equal(f.elements["demo-output"].textContent, "");
  assert.ok(f.elements["demo-preview"].textContent);
  f.tick();
  f.tick();
  assert.equal(f.phase(), "processing");
  assert.equal(f.elements["demo-output"].textContent, "");
  f.tick();
  assert.equal(f.phase(), "done");
  assert.ok(f.elements["demo-output"].textContent.includes("seller_id"));
  assert.equal(f.timers.size, 0);
});
test("autoplay runs once and replay never stacks timers", () => {
  const f = fixture();
  f.visible(true);
  f.visible(true);
  assert.equal(f.timers.size, 1);
  for (let i = 0; i < 4; i++) f.tick();
  f.visible(false);
  f.visible(true);
  assert.equal(f.timers.size, 0);
  f.elements["demo-play"].click();
  assert.equal(f.timers.size, 1);
  f.elements["demo-play"].click();
  assert.equal(f.timers.size, 0);
});
test("background and offscreen demonstrations stop all scheduled work", () => {
  const f = fixture();
  f.visible(true);
  f.tick();
  f.document.hidden = true;
  f.document.visibilitychange();
  assert.equal(f.timers.size, 0);
  assert.equal(f.phase(), "done");
  f.document.hidden = false;
  f.elements["demo-play"].click();
  f.visible(false);
  assert.equal(f.timers.size, 0);
});
test("reduced motion renders a static result, including a live preference change", () => {
  const f = fixture(true);
  f.visible(true);
  f.elements["demo-play"].click();
  assert.equal(f.timers.size, 0);
  assert.equal(f.phase(), "done");
  f.reduced.matches = false;
  f.elements["demo-play"].click();
  assert.equal(f.timers.size, 1);
  f.reduced.matches = true;
  f.reduced.change();
  assert.equal(f.timers.size, 0);
});
test("the demo never accesses media, credentials, analytics, or networking", () => {
  assert.doesNotMatch(
    script,
    /getUserMedia|MediaRecorder|fetch\(|XMLHttpRequest|localStorage|setInterval/,
  );
});
