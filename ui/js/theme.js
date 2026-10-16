// Light or dark: follows the system unless a choice is saved in Settings.

const systemDark = matchMedia("(prefers-color-scheme: dark)");

export class Theme {
  static choice = "system";
  static windowChoice = null;

  /** Applies "system", "light" or "dark", and keeps following the system when asked to. */
  static apply(choice = "system") {
    Theme.choice = choice;
    if (choice !== Theme.windowChoice) {
      Theme.windowChoice = choice;
      Theme.syncWindow(choice);
    }
    Theme.paint();
  }

  /** The theme actually shown: "light" or "dark". */
  static current() {
    if (Theme.choice === "light" || Theme.choice === "dark") return Theme.choice;
    return systemDark.matches ? "dark" : "light";
  }

  /** Sets data-theme on <html> and tells canvas effects to pick up the new colours. */
  static paint() {
    const theme = Theme.current();
    if (document.documentElement.dataset.theme === theme) return;
    document.documentElement.dataset.theme = theme;
    document.dispatchEvent(new CustomEvent("themechange", { detail: theme }));
  }

  /** Native window parts (macOS title bar buttons, scrollbars) follow the same choice. */
  static syncWindow(choice) {
    const win = window.__TAURI__?.window?.getCurrentWindow?.();
    win?.setTheme(choice === "system" ? null : choice).catch(() => {});
  }

  /** CSS custom property as a string, for canvas drawing. */
  static token(name) {
    return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  }
}

systemDark.addEventListener("change", () => Theme.paint());
Theme.paint();
