// Talking to the Rust side. Every command and event the window uses is listed here.
//
// Opened in a plain browser (no Tauri), the window talks to a simulated backend with
// example jobs instead, which is handy for working on the interface.

let backend = window.__TAURI__ ?? null;

export class Api {
  /** The real Rust side, or the simulation outside the app. */
  static async backend() {
    if (!backend) backend = (await import("./demo/backend.js")).DemoBackend.create();
    return backend;
  }

  /** Calls a Rust command. */
  static async call(command, args) {
    const tauri = await this.backend();
    return tauri.core.invoke(command, args);
  }

  /** Runs `handler(payload)` whenever the Rust side sends `event`. */
  static async on(event, handler) {
    const tauri = await this.backend();
    return tauri.event.listen(event, ({ payload }) => handler(payload));
  }

  // ---- reading
  static getState() { return this.call("get_state"); }
  static about() { return this.call("about"); }
  static jobLog(id) { return this.call("job_log", { id }); }
  static suggestName(source, destination, mode) { return this.call("suggest_name", { source, destination, mode }); }

  // ---- jobs
  static addJob(setup) { return this.call("add_job", { setup }); }
  static updateJob(id, setup) { return this.call("update_job", { id, setup }); }
  static duplicateJob(id) { return this.call("duplicate_job", { id }); }
  static removeJob(id) { return this.call("remove_job", { id }); }
  static moveJob(id, toIndex) { return this.call("move_job", { id, toIndex }); }
  static setPaused(id, paused) { return this.call("set_paused", { id, paused }); }
  static setAllPaused(paused) { return this.call("set_all_paused", { paused }); }

  // ---- running
  static syncNow(id) { return this.call("sync_now", { id }); }
  static syncAll() { return this.call("sync_all"); }
  static cancel(id) { return this.call("cancel", { id }); }
  static check(id) { return this.call("check", { id }); }
  static preview(id) { return this.call("preview", { id }); }
  static resolveConflict(id, path, choice) { return this.call("resolve_conflict", { id, path, choice }); }

  // ---- settings
  static setSettings(settings) { return this.call("set_settings", { settings }); }

  // ---- the desktop
  static pickFolder(start) { return this.call("pick_folder", { start: start || null }); }
  static reveal(path) { return this.call("reveal_path", { path }); }
  static openUrl(url) { return this.call("open_url", { url }); }
}
