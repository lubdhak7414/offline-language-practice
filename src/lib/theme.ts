/**
 * Apply the saved theme to the page. "system" removes the override so the
 * stylesheet's `prefers-color-scheme` rules decide. Called at launch (from the
 * app shell) as well as when the setting changes, so a chosen theme does not
 * wait for Settings to be opened.
 */
export function applyTheme(theme: string): void {
  if (typeof document === "undefined") return;
  if (theme === "light" || theme === "dark") {
    document.documentElement.setAttribute("data-theme", theme);
  } else {
    document.documentElement.removeAttribute("data-theme");
  }
}
