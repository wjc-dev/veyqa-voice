import { readFileSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import test from "node:test";
import assert from "node:assert/strict";

const site = new URL("../website/", import.meta.url);
const html = readFileSync(new URL("index.html", site), "utf8");
const script = readFileSync(new URL("app.js", site), "utf8");
const css = readFileSync(new URL("style.css", site), "utf8");
const brew = "brew install --cask wjc-dev/veyqa/veyqa-voice";
const releaseVersion = html.match(/releases\/download\/v([\d.]+)\/Veyqa-Voice-v/m)[1];

test("live preview wraps without truncation and stays in document flow", () => {
  const preview = css.match(/#demo-preview\s*\{([^}]+)\}/)[1];
  const strip = css.match(/\.voice-strip\s*\{([^}]+)\}/)[1];
  assert.doesNotMatch(preview, /nowrap|ellipsis|overflow:\s*hidden|line-clamp/);
  assert.match(preview, /white-space:\s*normal/);
  assert.match(preview, /overflow-wrap:\s*anywhere/);
  assert.doesNotMatch(strip, /position:\s*absolute/);
});

function fixture(clipboard) {
  const status = { hidden: true, textContent: "" };
  const command = { textContent: brew, closest: () => null };
  const button = {
    dataset: { copy: "brew-command" },
    addEventListener(_, cb) {
      this.click = cb;
    },
  };
  const selection = {
    removeAllRanges() {},
    addRange() {
      this.selected = true;
    },
  };
  vm.runInNewContext(script, {
    document: {
      getElementById: (id) => (id === "copy-status" ? status : command),
      querySelectorAll: () => [button],
      createRange: () => ({ selectNodeContents() {} }),
    },
    navigator: { clipboard },
    window: {
      getSelection: () => selection,
      location: { hash: "" },
      addEventListener() {},
    },
    setTimeout: () => 1,
    clearTimeout() {},
  });
  return { button, status, selection };
}
test("installation command is copied exactly", async () => {
  let copied;
  const f = fixture({
    writeText: async (text) => {
      copied = text;
    },
  });
  await f.button.click();
  assert.equal(copied, brew);
  assert.match(f.status.textContent, /已复制/);
});
test("clipboard refusal selects command without claiming success", async () => {
  for (const clipboard of [
    undefined,
    {
      writeText: async () => {
        throw Error("denied");
      },
    },
  ]) {
    const f = fixture(clipboard);
    await f.button.click();
    assert.equal(f.selection.selected, true);
    assert.match(f.status.textContent, /手动复制/);
    assert.doesNotMatch(f.status.textContent, /已复制/);
  }
});
test("single card and valid local assets and navigation anchors", () => {
  assert.doesNotMatch(css, /\.demo\s*::?(?:before|after)\b/);
  for (const [, path] of html.matchAll(/(?:src|href)="\.\/([^"]+)"/g))
    assert.ok(
      existsSync(fileURLToPath(new URL(path.split("?")[0], site))),
      path,
    );
  for (const [, id] of html.matchAll(/href="#([^"]+)"/g))
    assert.ok(html.includes(`id="${id}"`), id);
});
test("static accessible website with no agent prompt or credential form", () => {
  assert.match(html, /lang="zh-CN"/);
  assert.match(html, /name="viewport"/);
  assert.match(html, /role="status"\s+aria-live="polite"/);
  assert.doesNotMatch(
    html,
    /<form|<input|<textarea|agent-prompt|LET YOUR AGENT|给明天的自己/i,
  );
  assert.doesNotMatch(html, /https?:[^"']+\.(?:js|css)["']/i);
});
test("one direct hero download replaces the large installation section", () => {
  assert.ok(
    html.includes(
      `/releases/download/v${releaseVersion}/Veyqa-Voice-v${releaseVersion}-macOS-arm64.dmg`,
    ),
  );
  const hero = html.split('class="hero-copy"')[1].split("<figure")[0];
  assert.equal((hero.match(/class="button primary"/g) || []).length, 1);
  assert.match(
    hero,
    /href="https:\/\/github.com\/wjc-dev\/veyqa-voice\/releases\/download\//,
  );
  assert.doesNotMatch(html, /看看怎么用|GET STARTED|install-content|key-steps/);
  for (const text of [
    "Applications（应用程序）",
    "连接并继续",
    "试用文本框",
    "自动检查",
    "菜单栏",
    "Dock",
  ])
    assert.ok(html.includes(text), text);
});
test("precise service facts and concise vocabulary copy", () => {
  const copy = html.replace(/\s+/g, " ");
  for (const text of [
    "Key 保存在本机",
    "音频直接发送至所选语音服务",
    "费用由对应服务商收取",
    "提升识别概率",
    "user_id",
    "seller_id",
    "Qwen Audio",
    "无需填写 API Key",
    "费用由 Veyqa Voice 承担",
  ])
    assert.ok(copy.includes(text), text);
  assert.doesNotMatch(html, /不承诺|不等于|≠|零门槛|零配置|不上传云端/);
});
test("first-open and update help are collapsed in FAQ with legacy anchors", () => {
  for (const text of ["仍要打开", "隐私与安全性"])
    assert.ok(html.includes(text), text);
  assert.ok(html.includes('href="https://support.apple.com/zh-cn/102445"'));
  assert.match(html, /<details[^>]*id="safety"[^>]*>/);
  assert.match(html, /<details[^>]*id="api-key"[^>]*>/);
  assert.match(html, /id="install"/);
  assert.doesNotMatch(html, /<details[^>]*\sopen(?:\s|>)/);
  assert.match(html, /常见问题/);
  assert.doesNotMatch(html, /safety-steps|FIRST OPEN|首次打开，跟着这三步/);
  assert.doesNotMatch(html, /spctl\s+--|xattr\s+-/);
});
test("Homebrew remains copyable within FAQ", () => {
  assert.ok(html.includes('id="faq"'));
  assert.ok(html.indexOf('class="brew-details"') > html.indexOf('id="faq"'));
  assert.match(html, /使用 Homebrew 安装/);
  assert.doesNotMatch(html, /Homebrew ＋/);
  assert.match(css, /details\[open\].*\.disclosure-chevron/);
});

test("shared help links open the right FAQ without opening unrelated answers", () => {
  const answers = { safety: { open: false }, "api-key": { open: false } };
  const listeners = {};
  const browser = {
    location: { hash: "#safety" },
    addEventListener: (name, handler) => {
      listeners[name] = handler;
    },
  };
  vm.runInNewContext(script, {
    document: {
      getElementById: (id) =>
        answers[id] ? { closest: () => answers[id] } : null,
      querySelectorAll: () => [],
    },
    window: browser,
  });
  assert.equal(answers.safety.open, true);
  assert.equal(answers["api-key"].open, false);
  browser.location.hash = "#api-key";
  listeners.hashchange();
  assert.equal(answers["api-key"].open, true);
  browser.location.hash = "#install";
  assert.doesNotThrow(() => listeners.hashchange());
});

test("three peer engine cards replace separate API setup and engine sections", () => {
  const cards = html.split('class="engine-cards"')[1].split("</section>")[0];
  assert.equal((cards.match(/<article\b/g) || []).length, 3);
  for (const label of ["千问语音", "豆包语音识别", "自定义 OpenAI 兼容接口"])
    assert.ok(cards.includes(label), label);
  assert.match(cards, /Base URL、Key\s+和模型型号/);
  assert.match(cards, /录音结束后转写/);
  assert.doesNotMatch(html, /不翻车|极佳的识别率|超低延迟/);
  assert.match(
    css,
    /\.engine-cards\s*\{[^}]*repeat\(3,\s*minmax\(0,\s*1fr\)\)/,
  );
});
