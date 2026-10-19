import { readFileSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import test from "node:test";
import assert from "node:assert/strict";

const site = new URL("../website/", import.meta.url);
const html = readFileSync(new URL("index.html", site), "utf8");
const script = readFileSync(new URL("app.js", site), "utf8");
const prompt = html.match(
  /<textarea[^>]*id="agent-prompt"[^>]*>([\s\S]*?)<\/textarea\s*>/,
)[1];
const brew = "brew install --cask wjc-dev/veyqa/veyqa-voice";

// Test the actual client script with a minimal DOM. No browser or real clipboard.
function fixture(clipboard) {
  const details = { open: false };
  const status = { hidden: true, textContent: "" };
  const command = { textContent: brew, closest: () => null };
  const agent = {
    value: prompt,
    closest: () => details,
    focus() {
      this.focused = true;
    },
    select() {
      this.selected = true;
    },
  };
  const buttons = ["brew-command", "agent-prompt"].map((id) => ({
    dataset: { copy: id },
    addEventListener(event, callback) {
      this.click = callback;
    },
  }));
  const selection = {
    removeAllRanges() {},
    addRange() {
      this.selected = true;
    },
  };
  const elements = {
    "copy-status": status,
    "brew-command": command,
    "agent-prompt": agent,
  };
  vm.runInNewContext(script, {
    document: {
      getElementById: (id) => elements[id],
      querySelectorAll: () => buttons,
      createRange: () => ({ selectNodeContents() {} }),
    },
    navigator: { clipboard },
    window: { getSelection: () => selection },
    setTimeout: () => 1,
    clearTimeout() {},
  });
  return { buttons, details, status, agent, selection };
}

test("installation command is copied exactly, without the shell prompt", async () => {
  let copied;
  const f = fixture({
    writeText: async (text) => {
      copied = text;
    },
  });
  await f.buttons[0].click();
  assert.equal(copied, brew);
  assert.equal(f.status.hidden, false);
  assert.match(f.status.textContent, /已复制/);
});
test("full agent prompt is copied, not a shortened description", async () => {
  let copied;
  const f = fixture({
    writeText: async (text) => {
      copied = text;
    },
  });
  await f.buttons[1].click();
  assert.equal(copied, prompt.trim());
  for (const boundary of [
    "不使用 --zap",
    "不直接覆盖",
    "不禁用 Gatekeeper",
    "自行填写",
    "三次",
    "右 Option",
  ]) {
    assert.ok(copied.includes(boundary));
  }
});
test("rejected clipboard opens and selects the prompt without claiming success", async () => {
  const f = fixture({
    writeText: async () => {
      throw new Error("denied");
    },
  });
  await f.buttons[1].click();
  assert.equal(f.details.open, true);
  assert.ok(f.agent.focused && f.agent.selected);
  assert.match(f.status.textContent, /手动复制/);
  assert.doesNotMatch(f.status.textContent, /已复制/);
});
test("missing clipboard falls back to selected command text", async () => {
  const f = fixture(undefined);
  await f.buttons[0].click();
  assert.equal(f.selection.selected, true);
  assert.match(f.status.textContent, /手动复制/);
});
test("local assets and navigation anchors exist", () => {
  for (const [, path] of html.matchAll(/(?:src|href)="\.\/([^"]+)"/g)) {
    assert.ok(existsSync(fileURLToPath(new URL(path.split("?")[0], site))), path);
  }
  for (const [, id] of html.matchAll(/href="#([^"]+)"/g)) {
    assert.ok(html.includes(`id="${id}"`), id);
  }
});
test("website is accessible static content, not a credential collection form", () => {
  assert.match(html, /lang="zh-CN"/);
  assert.match(html, /name="viewport"/);
  assert.match(html, /role="status"\s+aria-live="polite"/);
  assert.match(html, /textarea\s+readonly/);
  assert.doesNotMatch(html, /<form|<input|https?:[^"']+\.(?:js|css)["']/i);
  assert.ok(html.includes("/releases/tag/v0.2.6"));
});

test("direct DMG installation is primary and setup instructions match the app", () => {
  assert.ok(html.includes("/releases/download/v0.2.6/Veyqa-Voice-v0.2.6-macOS-arm64.dmg"));
  assert.match(html, /不需要 Homebrew、Python\s+或开发环境/);
  assert.ok(html.includes("连接并继续"));
  assert.ok(html.includes("试用文本框"));
  assert.ok(prompt.includes("不要为了此应用额外安装 Homebrew"));
  assert.ok(prompt.includes("不代我开户"));
  assert.doesNotMatch(html, /完成，开始使用|零配置|零门槛/);
});

test("security instructions separate a personal exception from malware and managed-device blocks", () => {
  for (const text of ["仍要打开", "隐私与安全性", "恶意软件", "文件损坏", "公司安全软件", "不代替我点击安全例外"]) {
    assert.ok(prompt.includes(text), text);
  }
  assert.ok(html.includes('href="https://support.apple.com/zh-cn/102445"'));
  assert.doesNotMatch(prompt, /spctl\s+--|xattr\s+-/);
});
