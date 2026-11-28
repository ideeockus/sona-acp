export type ThemeChoice = "dark" | "light" | "system";

const media = matchMedia("(prefers-color-scheme: light)");

export function themeChoice(): ThemeChoice {
  try {
    const stored = localStorage.getItem("theme");
    if (stored === "light" || stored === "system") return stored;
  } catch {}
  return "dark";
}

export function applyTheme(choice: ThemeChoice) {
  try {
    localStorage.setItem("theme", choice);
  } catch {}
  const light = choice === "light" || (choice === "system" && media.matches);
  document.documentElement.dataset.theme = light ? "light" : "dark";
}

media.addEventListener("change", () => applyTheme(themeChoice()));
