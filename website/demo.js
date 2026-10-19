/* A bounded product illustration, never a recording or a speed benchmark. */
"use strict";
(() => {
  const root = document.getElementById("demo");
  if (!root) return;
  const play = document.getElementById("demo-play");
  const output = document.getElementById("demo-output");
  const preview = document.getElementById("demo-preview");
  const status = document.getElementById("demo-status");
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)");
  const sentence = "先把想法记下来，明天再把它变成现实。";
  let timer = null;
  let running = false;
  let visited = false;
  let step = 0;
  const frames = [
    ["ready", "", "准备听写", "01 / 按住右 Option", 700],
    ["listening", "", "先把想法记下来", "02 / 说话时，预览逐步出现", 1100],
    ["listening", "", sentence, "02 / 原话保留，不默认 AI 润色", 1300],
    ["processing", "", "正在完成识别…", "03 / 松开按键，完成识别", 600],
    ["done", sentence, "已写入，未发送", "你的原话，一次写入。", 0],
  ];
  function render(frame) {
    root.dataset.phase = frame[0];
    output.textContent = frame[1];
    preview.textContent = frame[2];
    status.textContent = frame[3];
  }
  function stop() {
    clearTimeout(timer);
    timer = null;
    running = false;
    render(frames.at(-1));
    play.textContent = "重新播放 ↻";
  }
  function next() {
    if (!running) return;
    const frame = frames[step++];
    render(frame);
    if (frame[4]) timer = setTimeout(next, frame[4]);
    else stop();
  }
  function start() {
    visited = true;
    stop();
    if (reduced.matches || document.hidden) return;
    running = true;
    step = 0;
    play.textContent = "停止演示 Ⅱ";
    next();
  }
  play.hidden = false;
  play.addEventListener("click", () => (running ? stop() : start()));
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) stop();
  });
  reduced.addEventListener("change", () => {
    if (reduced.matches) stop();
  });
  if ("IntersectionObserver" in window) {
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (!entry.isIntersecting) stop();
        else if (!visited && !reduced.matches) start();
      },
      { threshold: 0.35 },
    );
    observer.observe(root);
  }
})();
