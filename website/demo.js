/* Scripted examples; a single timer runs only while the demo is visible. */
"use strict";
(() => {
  const root = document.getElementById("demo");
  if (!root) return;
  const play = document.getElementById("demo-play");
  const output = document.getElementById("demo-output");
  const preview = document.getElementById("demo-preview");
  const status = document.getElementById("demo-status");
  const topic = document.getElementById("demo-topic");
  const count = document.getElementById("demo-count");
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)");
  const scenes = [
    [
      "数据查询",
      "在 DataWorks 中按 user_id 和 seller_id 关联订单表，汇总昨天各渠道的成交金额和退款率。",
    ],
    [
      "开发协作",
      "先确认 ODPS 分区已经产出，再到 DSW 检查特征分布，下午把评估结果同步到钉钉。",
    ],
    [
      "项目评审",
      "这次评审先对齐 GMV 和转化率口径，确认埋点数据后，再安排下一轮灰度发布。",
    ],
  ];
  let timer = null;
  let running = false;
  let visited = false;
  let step = 0;
  let scene = 0;
  function frames() {
    const sentence = scenes[scene][1];
    return [
      ["ready", "", "准备听写", "01 / 按住右 Option", 700],
      [
        "listening", "", sentence.slice(0, sentence.indexOf("，")),
        "02 / 说话时，预览逐步出现", 1700,
      ],
      ["listening", "", sentence, "02 / 直接转写，保留原话", 1800],
      ["processing", "", "正在完成识别…", "03 / 松开按键，完成识别", 600],
      ["done", sentence, "已写入输入框", "完成输入，由你确认发送。", 6500],
    ];
  }
  function render(frame) {
    root.dataset.phase = frame[0];
    output.textContent = frame[1];
    preview.textContent = frame[2];
    status.textContent = frame[3];
    topic.textContent = scenes[scene][0];
    count.textContent = `${scene + 1} / ${scenes.length}`;
  }
  function stop() {
    clearTimeout(timer);
    timer = null;
    running = false;
    render(frames().at(-1));
    play.textContent = reduced.matches ? "下个示例 →" : "播放演示 ↻";
  }
  function next() {
    if (!running) return;
    let current = frames();
    if (step === current.length) {
      scene = (scene + 1) % scenes.length;
      step = 0;
      current = frames();
    }
    const frame = current[step++];
    render(frame);
    timer = setTimeout(next, frame[4]);
  }
  function start() {
    visited = true;
    stop();
    if (reduced.matches || document.hidden) return;
    running = true;
    step = 0;
    play.textContent = "暂停演示 Ⅱ";
    next();
  }
  play.hidden = false;
  stop();
  play.addEventListener("click", () => {
    if (reduced.matches) {
      scene = (scene + 1) % scenes.length;
      stop();
    } else if (running) stop();
    else start();
  });
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
