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
    window: { getSelection: () => selection },
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
test("DMG install, permissions and trial have clear instructions", () => {
  assert.ok(
    html.includes(
      "/releases/download/v0.2.7/Veyqa-Voice-v0.2.7-macOS-arm64.dmg",
    ),
  );
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
  for (const text of [
    "Key 保存在本机",
    "音频直接发送至所选语音服务",
    "费用由对应服务商收取",
    "提升识别概率",
    "user_id",
    "seller_id",
    "Qwen Audio 3.0",
  ])
    assert.ok(html.includes(text), text);
  assert.doesNotMatch(html, /不承诺|不等于|≠|零门槛|零配置|不上传云端/);
});
test("security guide distinguishes verification from malware and managed blocks", () => {
  for (const text of [
    "仍要打开",
    "隐私与安全性",
    "恶意软件",
    "文件损坏",
    "公司安全软件",
    "联系管理员",
  ])
    assert.ok(html.includes(text), text);
  assert.ok(html.includes('href="https://support.apple.com/zh-cn/102445"'));
  assert.doesNotMatch(html, /spctl\s+--|xattr\s+-/);
});
