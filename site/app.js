const storageKey = "potato-hue-theme";
const buttons = document.querySelectorAll("[data-theme]");

function resolvedTheme(theme) {
  return theme === "auto"
    ? window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light"
    : theme;
}

function setTheme(theme) {
  document.documentElement.dataset.theme = resolvedTheme(theme);
  buttons.forEach((button) => button.setAttribute("aria-pressed", String(button.dataset.theme === theme)));
}

const savedTheme = localStorage.getItem(storageKey) || "auto";
setTheme(savedTheme);

buttons.forEach((button) => {
  button.addEventListener("click", () => {
    const theme = button.dataset.theme;
    localStorage.setItem(storageKey, theme);
    setTheme(theme);
  });
});

window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
  if ((localStorage.getItem(storageKey) || "auto") === "auto") setTheme("auto");
});
