/* No analytics, network requests, or key collection. Copy only on user action. */
"use strict";
// Keep shared help links useful even when their answers start collapsed.
function revealLinkedHelp() {
  const target = document.getElementById(window.location.hash.slice(1));
  const details = target?.closest("details");
  if (details) details.open = true;
}
window.addEventListener("hashchange", revealLinkedHelp);
revealLinkedHelp();
let feedbackTimer;
const status = document.getElementById("copy-status");
function feedback(message) {
  clearTimeout(feedbackTimer);
  status.hidden = false;
  status.textContent = message;
  feedbackTimer = setTimeout(() => {
    status.hidden = true;
  }, 6000);
}
document.querySelectorAll("[data-copy]").forEach((button) => {
  button.addEventListener("click", async () => {
    const target = document.getElementById(button.dataset.copy);
    const text = (target.value ?? target.textContent).trim();
    try {
      if (!navigator.clipboard?.writeText)
        throw new Error("Clipboard unavailable");
      await navigator.clipboard.writeText(text);
      feedback("安装命令已复制。");
    } catch {
      const details = target.closest("details");
      if (details) details.open = true;
      if (typeof target.select === "function") {
        target.focus();
        target.select();
      } else {
        const range = document.createRange();
        range.selectNodeContents(target);
        const selection = window.getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
      }
      feedback("浏览器未允许复制，已选中文本，请手动复制。");
    }
  });
});
