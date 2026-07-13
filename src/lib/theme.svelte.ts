// App-global light/dark theme. Persists the user's choice in localStorage and
// reflects the resolved theme onto <html data-theme> so app.css token blocks
// take over. "system" tracks the OS preference live.
export type ThemeMode = "light" | "dark" | "system";

const KEY = "ken-theme";
const mql = window.matchMedia("(prefers-color-scheme: dark)");

function read(): ThemeMode {
  const v = localStorage.getItem(KEY);
  return v === "light" || v === "dark" || v === "system" ? v : "system";
}

function resolve(mode: ThemeMode): "light" | "dark" {
  if (mode === "system") return mql.matches ? "dark" : "light";
  return mode;
}

function apply(mode: ThemeMode) {
  document.documentElement.setAttribute("data-theme", resolve(mode));
}

class ThemeStore {
  mode = $state<ThemeMode>(read());

  set(mode: ThemeMode) {
    this.mode = mode;
    localStorage.setItem(KEY, mode);
    apply(mode);
  }

  /** Apply the stored theme and start tracking OS changes. Call once at boot. */
  init() {
    apply(this.mode);
    mql.addEventListener("change", () => {
      if (this.mode === "system") apply(this.mode);
    });
  }
}

export const theme = new ThemeStore();
