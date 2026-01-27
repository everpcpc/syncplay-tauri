export type ThemePreference = "dark" | "light";

export const normalizeTheme = (value?: string): ThemePreference =>
  value === "light" ? "light" : "dark";

export const applyTheme = (value?: string) => {
  const theme = normalizeTheme(value);
  const root = document.documentElement;
  root.classList.toggle("theme-light", theme === "light");
  root.classList.toggle("theme-dark", theme !== "light");
};
