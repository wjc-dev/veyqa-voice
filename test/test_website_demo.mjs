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
    [
      "demo",
      "demo-play",
      "demo-output",
      "demo-preview",
      "demo-status",
      "demo-topic",
      "demo-count",
    ].map((id) => [
      id,
      {
        dataset: {},
        textContent: "",
        hidden: true,
        addEventListener(type, fn) {
          this[type] = fn;
        },
      },
    ]),
  );
  const timers = new Map();
  const delays = [];
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
    setTimeout: (fn, delay) => {
      delays.push(delay);
      timers.set(++serial, fn);
      return serial;
    },
    clearTimeout: (id) => timers.delete(id),
  });
  return {
    elements,
    timers,
    delays,
    document,
    reduced,
    visible(
      value,
      ratio = value ? 1 : 0,
      cardHeight = 600,
      viewportHeight = 800,
    ) {
      observe([
        {
          isIntersecting: value,
          intersectionRatio: ratio,
          boundingClientRect: { height: cardHeight, width: 350 },
          intersectionRect: {
            height: cardHeight * ratio,
            width: value ? 350 : 0,
          },
          rootBounds: { height: viewportHeight, width: 390 },
        },
      ]);
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
  assert.equal(f.timers.size, 1);
});
test("examples rotate and wrap without stacking timers", () => {
  const f = fixture();
  f.visible(true);
  const sentences = [];
  for (let scene = 0; scene < 4; scene++) {
    for (let i = 0; i < 4; i++) f.tick();
    sentences.push(f.elements["demo-output"].textContent);
    assert.equal(f.timers.size, 1);
    f.tick();
  }
  assert.match(sentences[0], /DataWorks.*user_id.*seller_id/);
  assert.match(sentences[1], /ODPS.*DSW/);
  assert.match(sentences[2], /GMV.*灰度发布/);
  assert.equal(sentences[3], sentences[0]);
});
test("offscreen re-entry restarts, while explicit pause persists", () => {
  const f = fixture();
  f.visible(true);
  f.visible(true);
  assert.equal(f.timers.size, 1);
  for (let i = 0; i < 4; i++) f.tick();
  f.visible(false);
  assert.equal(f.timers.size, 0);
  f.visible(true);
  assert.equal(f.phase(), "ready");
  assert.equal(f.timers.size, 1);
  f.elements["demo-play"].click();
  assert.equal(f.timers.size, 0);
  f.visible(false);
  f.visible(true);
  assert.equal(f.timers.size, 0);
  f.elements["demo-play"].click();
  assert.equal(f.phase(), "ready");
  assert.equal(f.timers.size, 1);
});
test("background and offscreen demonstrations stop all scheduled work", () => {
  const f = fixture();
  f.visible(true);
  f.tick();
  f.document.hidden = true;
  f.document.visibilitychange();
  assert.equal(f.timers.size, 0);
  assert.equal(f.phase(), "ready");
  f.document.hidden = false;
  f.document.visibilitychange();
  assert.equal(f.phase(), "ready");
  assert.equal(f.timers.size, 1);
  f.visible(false);
  assert.equal(f.timers.size, 0);
});
test("a sliver does not start autoplay; majority visibility reveals before progress", () => {
  const f = fixture();
  assert.equal(f.phase(), "ready");
  f.visible(true, 0.01);
  f.visible(true, 0.4);
  assert.equal(f.timers.size, 0);
  f.visible(true, 0.7);
  assert.equal(f.timers.size, 1);
  assert.equal(f.elements.demo.dataset.reveal, "visible");
  assert.ok(f.delays.at(-1) >= 1000);
  f.tick();
  f.visible(true, 0.5);
  assert.equal(f.phase(), "listening");
  assert.equal(f.timers.size, 1);
  f.visible(true, 0.1);
  assert.equal(f.timers.size, 0);
  assert.equal(f.elements.demo.dataset.playing, "false");
});
test("a card taller than the mobile viewport can still start", () => {
  const f = fixture();
  f.visible(true, 0.1, 900, 400);
  assert.equal(f.timers.size, 0);
  f.visible(true, 0.35, 900, 400);
  assert.equal(f.timers.size, 1);
});
test("fast scroll and hidden pages cancel pending entrance work", () => {
  const f = fixture();
  f.visible(true);
  f.visible(false);
  assert.equal(f.timers.size, 0);
  f.document.hidden = true;
  f.visible(true);
  assert.equal(f.timers.size, 0);
  f.document.hidden = false;
  f.document.visibilitychange();
  assert.equal(f.phase(), "ready");
  assert.equal(f.timers.size, 1);
  f.elements["demo-play"].click();
  f.document.hidden = true;
  f.document.visibilitychange();
  f.document.hidden = false;
  f.document.visibilitychange();
  assert.equal(f.timers.size, 0);
});
test("reduced motion renders a static result, including a live preference change", () => {
  const f = fixture(true);
  f.visible(true);
  f.elements["demo-play"].click();
  assert.equal(f.timers.size, 0);
  assert.equal(f.phase(), "done");
  assert.equal(f.elements["demo-play"].textContent, "下个示例 →");
  const first = f.elements["demo-output"].textContent;
  f.elements["demo-play"].click();
  assert.notEqual(f.elements["demo-output"].textContent, first);
  assert.equal(f.timers.size, 0);
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
