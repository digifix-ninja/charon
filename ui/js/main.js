// Charon's window: creates the views and connects them to the Rust side.

import { Api } from "./api.js";
import { Dom } from "./dom.js";
import { paintIcons } from "./icons.js";
import { Platform } from "./platform.js";
import { Theme } from "./theme.js";
import { JobStore } from "./store.js";
import { Toast } from "./toast.js";
import { River } from "./effects/river.js";
import { Header } from "./views/header.js";
import { JobList } from "./views/job-list.js";
import { JobEditor } from "./views/job-editor.js";
import { PreviewDialog } from "./views/preview-dialog.js";
import { SettingsPanel } from "./views/settings-panel.js";
import { AboutPanel } from "./views/about-panel.js";
import { PopMenu } from "./views/pop-menu.js";
import { Sheet } from "./views/sheet.js";

class App {
  constructor() {
    this.store = new JobStore();
    this.toast = new Toast(Dom.$("#toast"));
    this.river = new River(Dom.$("#river"));
    this.menu = new PopMenu(Dom.$("#menu"));
    this.header = new Header(this.store, { pickGroup: group => this.pickGroup(group) });
    this.list = new JobList(this.store, {
      newJob: () => this.editor.create(),
      act: (action, id, target) => this.act(action, id, target),
    });
    this.editor = new JobEditor(this.store, this.toast, { saved: (view, info) => this.saved(view, info) });
    this.preview = new PreviewDialog(this.store, this.toast, { syncNow: id => this.syncNow(id) });
    this.settings = new SettingsPanel(this.store, this.toast);
    this.about = new AboutPanel(this.toast);
    this.store.subscribe(hint => this.render(hint));
  }

  /** Loads the state, then listens for changes. */
  async start() {
    Platform.markBody();
    paintIcons();
    this.river.start();
    this.bindControls();
    this.bindShortcuts();
    await this.listenToRust();
    await this.reload();
    setInterval(() => this.store.changed(), 30_000); // keeps "4 min ago" fresh
  }

  /** Redraws what changed. */
  render(hint) {
    Theme.apply(this.store.settings?.appearance);
    this.header.render();
    paintIcons(Dom.$(".titlebar"));
    this.list.render(hint);
    this.updateRiver();
  }

  /** The boat crosses with the combined progress of every running job. */
  updateRiver() {
    const running = this.store.all().filter(job => job.status === "syncing");
    let done = 0, total = 0;
    for (const job of running) {
      done += job.progress?.doneBytes || job.progress?.doneItems || 0;
      total += job.progress?.totalBytes || job.progress?.totalItems || 0;
    }
    this.river.setActivity(running.length > 0, total ? done / total : 0.05);
  }

  async reload() {
    try {
      this.store.load(await Api.getState());
    } catch (error) {
      this.toast.error(error);
    }
  }

  // ---------------------------------------------------------------- actions

  /** Runs an API call and shows its error, if any. */
  async run(work) {
    try {
      return await work();
    } catch (error) {
      this.toast.error(error);
    }
  }

  syncNow(id) {
    const job = this.store.jobs.get(id);
    if (job) this.store.setProgress(id, { phase: "scanning", doneItems: 0, totalItems: 0, doneBytes: 0, totalBytes: 0 });
    this.run(() => Api.syncNow(id));
  }

  /** A job was saved in the editor. A new job opens its preview: look before crossing. */
  saved(view, { isNew }) {
    this.store.put(view);
    if (isNew) this.preview.show(view.id);
    else this.toast.show("Saved.");
  }

  pickGroup(group) {
    this.store.group = this.store.group === group ? "all" : group;
    this.store.changed();
  }

  /** A button or click inside a job row. */
  act(action, id, target) {
    const job = this.store.jobs.get(id);
    if (!job) return;
    switch (action) {
      case "toggle": return this.toggleRow(id);
      case "sync": return this.syncNow(id);
      case "stop": return this.run(() => Api.cancel(id));
      case "pause": return this.run(() => Api.setPaused(id, true));
      case "resume": return this.run(() => Api.setPaused(id, false));
      case "folders": return this.menu.show(target, [
        { label: `Show source in ${Platform.fileManager()}`, icon: "folder", run: () => this.run(() => Api.reveal(job.source)) },
        { label: `Show destination in ${Platform.fileManager()}`, icon: "folder", run: () => this.run(() => Api.reveal(job.destination)) },
      ]);
      case "more": return this.menu.show(target, [
        { label: "Preview (dry run)", icon: "eye", run: () => this.preview.show(id) },
        { label: "Look for changes", icon: "search", run: () => this.run(() => Api.check(id)) },
        { label: "Activity log", icon: "log", run: () => this.openTab(id, "activity") },
        "-",
        { label: "Edit…", icon: "edit", run: () => this.editor.edit(id) },
        { label: "Duplicate", icon: "copy", run: () => this.run(() => Api.duplicateJob(id)) },
        "-",
        { label: "Delete…", icon: "trash", danger: true, run: () => this.removeJob(job) },
      ]);
      case "tab": return this.openTab(id, target.dataset.tab);
      case "resolve": return this.resolve(id, target);
    }
  }

  /** Expands or folds a row; an expanded row loads its log. */
  toggleRow(id) {
    if (this.store.toggleOpen(id)) this.loadLog(id);
  }

  openTab(id, tab) {
    if (!this.store.open.has(id)) this.store.toggleOpen(id);
    this.store.setTab(id, tab);
    if (tab === "activity") this.loadLog(id);
  }

  async loadLog(id) {
    const lines = await this.run(() => Api.jobLog(id));
    if (lines) this.store.setLog(id, lines);
  }

  async resolve(id, button) {
    button.disabled = true;
    await this.run(() => Api.resolveConflict(id, button.dataset.path, button.dataset.choice));
    this.loadLog(id);
  }

  async removeJob(job) {
    if (!confirm(`Delete “${job.name}”? The folders and files stay where they are.`)) return;
    await this.run(() => Api.removeJob(job.id));
  }

  // ---------------------------------------------------------------- wiring

  bindControls() {
    const on = (selector, event, handler) => Dom.$(selector).addEventListener(event, handler);
    on("#btn-new", "click", () => this.editor.create());
    on("#btn-sync-all", "click", () => this.run(() => Api.syncAll()));
    on("#btn-pause-all", "click", () => this.run(() => Api.setAllPaused(!this.store.overview?.pausedAll)));
    on("#btn-settings", "click", () => this.settings.toggleOpen());
    on("#btn-about", "click", () => (this.about.isOpen ? this.about.close() : this.about.show()));
    on("#scrim", "click", () => this.closeOverlays());
    on("#search", "input", event => {
      this.store.query = event.target.value.trim();
      this.store.changed();
    });
  }

  closeOverlays() {
    this.menu.close();
    this.preview.close();
    Sheet.closeAll();
  }

  /** Esc, ⌘N, ⌘, ⌘F and ⌘⇧S (Ctrl on Windows and Linux). */
  bindShortcuts() {
    document.addEventListener("keydown", event => {
      if (event.key === "Escape") return this.closeOverlays();
      if (!(event.metaKey || event.ctrlKey)) return;
      const key = event.key.toLowerCase();
      const shortcuts = {
        n: () => this.editor.create(),
        ",": () => this.settings.show(),
        f: () => Dom.$("#search").focus(),
        s: () => event.shiftKey && this.run(() => Api.syncAll()),
      };
      if (shortcuts[key]) {
        event.preventDefault();
        shortcuts[key]();
      }
    });
  }

  /** Events from the Rust side. */
  async listenToRust() {
    await Api.on("job", view => {
      this.store.put(view);
      if (this.store.open.has(view.id) && this.store.tabOf(view.id) === "activity" && view.status !== "syncing") this.loadLog(view.id);
    });
    await Api.on("job-removed", id => this.store.drop(id));
    await Api.on("progress", ({ id, progress }) => this.store.setProgress(id, progress));
    await Api.on("overview", overview => this.store.setOverview(overview));
  }
}

new App().start();
