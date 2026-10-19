// New versions of Charon: the Rust side asks GitHub, this shows the answer. A glowing
// "Update" button appears in the title bar when a newer version is out.

import { Api } from "./api.js";
import { Dom } from "./dom.js";

export class Updates {
  constructor(toast) {
    this.toast = toast;
    this.info = null;
    this.checking = false;
    this.listeners = new Set();
    this.button = Dom.$("#btn-update");
  }

  /** Picks up an answer that came before the window opened, then listens for new ones. */
  async start() {
    await Api.on("update", info => this.receive(info));
    const known = await Api.updateInfo().catch(() => null);
    if (known) this.receive(known);
  }

  /** Runs `handler()` whenever the answer or the checking state changes. */
  subscribe(handler) {
    this.listeners.add(handler);
    return () => this.listeners.delete(handler);
  }

  /** Asks GitHub now. Errors are shown as a toast. */
  async check() {
    if (this.checking) return;
    this.checking = true;
    this.changed();
    try {
      this.receive(await Api.checkUpdates());
    } catch (error) {
      this.toast.error(error);
    } finally {
      this.checking = false;
      this.changed();
    }
  }

  /** Opens the installer for this computer (or the release page). */
  download() {
    if (!this.info?.available) return;
    Api.openUrl(this.info.download || this.info.page).catch(error => this.toast.error(error));
  }

  /** Opens the release page with its notes. */
  releaseNotes() {
    if (this.info) Api.openUrl(this.info.page).catch(error => this.toast.error(error));
  }

  receive(info) {
    const isNews = info.available && info.latest !== this.info?.latest;
    this.info = info;
    if (isNews) this.toast.show(`Charon ${info.latest} is out. Press Update to download it.`);
    this.changed();
  }

  changed() {
    const available = Boolean(this.info?.available);
    this.button.hidden = !available;
    if (available) this.button.title = `Charon ${this.info.latest} is out (you have ${this.info.current})`;
    for (const handler of this.listeners) handler();
  }
}
