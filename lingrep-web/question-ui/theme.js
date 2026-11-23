// The light/dark choice, shared by every page. It runs before the first
// paint so that a stored choice does not flash the other theme.
(() => {
  const KEY = "lingrep:theme";
  const root = document.documentElement;
  const system = matchMedia("(prefers-color-scheme: dark)");
  const stored = () => {
    try {
      const value = localStorage.getItem(KEY);
      return value === "light" || value === "dark" ? value : null;
    } catch {
      return null;
    }
  };
  const current = () => root.dataset.theme ?? (system.matches ? "dark" : "light");
  const describe = () => {
    const dark = current() === "dark";
    const text = root.lang === "ja"
      ? (dark ? "ライトテーマに切り替える" : "ダークテーマに切り替える")
      : (dark ? "Switch to light theme" : "Switch to dark theme");
    for (const button of document.querySelectorAll("[data-theme-toggle]")) {
      button.setAttribute("aria-label", text);
      button.title = text;
    }
  };
  const choice = stored();
  if (choice) root.dataset.theme = choice;
  document.addEventListener("click", (event) => {
    if (!event.target.closest?.("[data-theme-toggle]")) return;
    const next = current() === "dark" ? "light" : "dark";
    root.dataset.theme = next;
    try {
      localStorage.setItem(KEY, next);
    } catch {
      // The choice then lasts for this page only.
    }
    describe();
  });
  document.addEventListener("DOMContentLoaded", describe);
  system.addEventListener("change", describe);
  new MutationObserver(describe).observe(root, { attributes: true, attributeFilter: ["lang"] });
})();
