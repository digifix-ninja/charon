// Settings: appearance, start-up, background, notifications, trash and checks. Saved on every change.

import { Api } from "../api.js";
import { Dom } from "../dom.js";
import { Icons } from "../icons.js";
import { Sheet } from "./sheet.js";
import { Theme } from "../theme.js";

const { html, raw } = Dom;

export class SettingsPanel extends Sheet {
  constructor(store, toast) {
    super(Dom.$("#settings"));
    this.store = store;
    this.toast = toast;
    this.element.addEventListener("change", event => this.onChange(event));
    this.element.addEventListener("click", event => {
      if (event.target.closest("[data-do=close]")) this.close();
      const appearance = event.target.closest("[data-appearance]");
      if (appearance) this.save("appearance", appearance.dataset.appearance);
    });
  }

  show() {
    this.render();
    this.open();
  }

  toggleOpen() {
    this.isOpen ? this.close() : this.show();
  }

  render() {
    const s = this.store.settings;
    if (!s) return;
    const toggle = (field, title, text) => html`<label class="toggle">
        <span class="copy"><strong>${title}</strong><small>${text}</small></span>
        <input type="checkbox" data-field="${field}" ${s[field] ? "checked" : ""}><span class="switch"></span>
      </label>`;
    const number = (field, title, text, unit, min, max) => html`<div class="setting-row">
        <span class="copy"><strong>${title}</strong><small>${text}</small></span>
        <span class="unit"><input class="input small" type="number" min="${min}" max="${max}" data-field="${field}" value="${s[field]}">${unit}</span>
      </div>`;
    const appearance = s.appearance || "system";
    const choice = (value, label) => html`<button type="button" data-appearance="${value}" class="${appearance === value ? "on" : ""}">${label}</button>`;
    this.element.innerHTML = html`
      <header>
        <div><span class="eyebrow"><span class="num">02</span> Settings</span><h2>The ferryman's rules</h2></div>
        <button class="icon-btn" data-do="close" title="Close"><span class="ico">${raw(Icons.close)}</span></button>
      </header>
      <div class="scroll"><div class="settings-list">
        <div class="setting-row">
          <span class="copy"><strong>Appearance</strong><small>Dark like the river at night, light like paper, or whatever your system uses.</small></span>
          <span class="segmented">${raw(choice("system", "System"))}${raw(choice("light", "Light"))}${raw(choice("dark", "Dark"))}</span>
        </div>
        ${raw(toggle("launchAtLogin", "Launch at login", "Charon starts quietly in the menu bar when you log in."))}
        ${raw(toggle("runInBackground", "Keep running when the window is closed", "Automatic jobs keep working from the menu bar or system tray."))}
        ${raw(toggle("notifications", "Notifications", "Errors, conflicts and big syncs. Small routine syncs stay silent."))}
        ${raw(toggle("pausedAll", "Pause everything", "No job runs by itself. Handy on a hotspot or while presenting."))}
        ${raw(number("trashDays", "Keep deleted files", "How long safe deletes stay in .charon-trash. 0 keeps them for ever.", "days", 0, 3650))}
        ${raw(number("checkMinutes", "Look for changes", "How often jobs check for pending changes, besides watching the folders.", "min", 1, 1440))}
      </div></div>`;
  }

  onChange(event) {
    const el = event.target;
    const field = el.dataset.field;
    if (!field) return;
    this.save(field, el.type === "checkbox" ? el.checked : Math.max(Number(el.min) || 0, Number(el.value) || 0));
  }

  /** Saves one setting; appearance changes show at once. */
  async save(field, value) {
    const settings = { ...this.store.settings, [field]: value };
    try {
      await Api.setSettings(settings);
      this.store.settings = settings;
      if (field === "appearance") {
        Theme.apply(value);
        this.render();
      }
    } catch (error) {
      this.toast.error(error);
      this.render();
    }
  }
}
