// A pretend Rust side for opening the window in a plain browser: example jobs, simulated
// syncs with live progress, previews and conflicts. Never used inside the real app.

const MIN = 60_000;
const ago = ms => new Date(Date.now() - ms).toISOString();
const later = ms => new Date(Date.now() + ms).toISOString();

const DEFAULT_EXCLUDES = [".DS_Store", "._*", ".Spotlight-V100", ".Trashes", ".fseventsd", "Thumbs.db", "desktop.ini", "$RECYCLE.BIN", "~$*", "*.tmp"];

/** One example job, with the fields the real JobView has. */
function job(id, fields) {
  return {
    id,
    mode: "mirror",
    conflict: "keep-both",
    trigger: { kind: "manual" },
    excludes: DEFAULT_EXCLUDES.slice(0, 6),
    safeDelete: true,
    paused: false,
    createdAt: ago(30 * 24 * 60 * MIN),
    lastRun: { startedAt: ago(5 * MIN), finishedAt: ago(4 * MIN), copied: 12, deleted: 1, bytes: 8_200_000, conflicts: 0, failed: 0, cancelled: false, error: null },
    lastSynced: ago(4 * MIN),
    status: "in-sync",
    pending: null,
    progress: null,
    conflicts: [],
    problem: null,
    sourceOnline: true,
    destinationOnline: true,
    nextRun: null,
    ...fields,
  };
}

export class DemoBackend {
  static create() {
    const backend = new DemoBackend();
    return { core: { invoke: (c, a) => backend.invoke(c, a) }, event: { listen: (e, h) => backend.listen(e, h) } };
  }

  constructor() {
    this.listeners = {};
    this.settings = { appearance: "system", launchAtLogin: true, runInBackground: true, notifications: true, trashDays: 30, pausedAll: false, checkMinutes: 15 };
    this.jobs = [
      job("photos", { name: "Photos → Backup SSD", source: "/Users/andrei/Pictures/Photos", destination: "/Volumes/Backup SSD/Photos", trigger: { kind: "drive-connected" }, lastSynced: ago(26 * MIN) }),
      job("projects", { name: "Projects ⇄ NAS", source: "/Users/andrei/Projects", destination: "/Volumes/NAS/Projects", mode: "two-way", trigger: { kind: "on-change", settleSeconds: 5 } }),
      job("documents", { name: "Documents → Cloud Drive", source: "/Users/andrei/Documents", destination: "/Users/andrei/Cloud Drive/Documents", mode: "backup", trigger: { kind: "interval", minutes: 60 }, status: "pending", pending: { items: 23, bytes: 48_000_000, checkedAt: ago(MIN) }, nextRun: later(17 * MIN), lastSynced: ago(43 * MIN) }),
      job("notes", { name: "Notes ⇄ Laptop", source: "/Users/andrei/Notes", destination: "/Volumes/Laptop/Notes", mode: "two-way", conflict: "ask", trigger: { kind: "on-change", settleSeconds: 3 }, status: "conflict", lastSynced: ago(2 * 60 * MIN), conflicts: [
        { path: "Ideas/charon-website.md", sourceModified: Date.now() - 12 * MIN, destinationModified: Date.now() - 40 * MIN, sourceSize: 4_210, destinationSize: 3_980 },
        { path: "Todo.txt", sourceModified: Date.now() - 3 * MIN, destinationModified: Date.now() - 9 * MIN, sourceSize: 820, destinationSize: 1_044 },
      ] }),
      job("music", { name: "Music → Travel drive", source: "/Users/andrei/Music", destination: "/Volumes/TRAVEL/Music", trigger: { kind: "drive-connected" }, status: "offline", destinationOnline: false, problem: "The destination isn't reachable. Is the drive connected?", lastSynced: ago(6 * 24 * 60 * MIN) }),
      job("archive", { name: "Archive → Cold storage", source: "/Users/andrei/Archive", destination: "/Volumes/Cold/Archive", mode: "backup", trigger: { kind: "daily", hour: 3, minute: 0 }, paused: true, status: "paused", lastSynced: ago(3 * 24 * 60 * MIN) }),
    ];
    setTimeout(() => this.simulateSync("projects", 900, 2_400_000_000), 1200);
  }

  listen(event, handler) {
    (this.listeners[event] ??= []).push(handler);
    return Promise.resolve(() => {});
  }

  emit(event, payload) {
    for (const handler of this.listeners[event] ?? []) handler({ payload: structuredClone(payload) });
  }

  overview() {
    const count = (...s) => this.jobs.filter(j => s.includes(j.status)).length;
    return { jobs: this.jobs.length, syncing: count("syncing"), pending: count("pending"), attention: count("error", "conflict"), offline: count("offline"), pausedAll: this.settings.pausedAll };
  }

  publish(job) {
    this.emit("job", job);
    this.emit("overview", this.overview());
  }

  find(id) {
    const found = this.jobs.find(j => j.id === id);
    if (!found) throw "This sync job no longer exists.";
    return found;
  }

  /** Pretends to carry `items` files of `bytes` in total. */
  simulateSync(id, items, bytes) {
    const j = this.find(id);
    if (j.status === "syncing" || j.status === "offline") return;
    j.status = "syncing";
    j.progress = { phase: "scanning", doneItems: 0, totalItems: 0, doneBytes: 0, totalBytes: 0, current: null };
    this.publish(j);
    const names = ["src/engine/runs.rs", "assets/hero.mp4", "docs/notes.md", "design/river.fig", "photos/IMG_2041.HEIC", "website/index.html"];
    let done = 0;
    setTimeout(() => {
      j.progress = { phase: "copying", doneItems: 0, totalItems: items, doneBytes: 0, totalBytes: bytes, current: names[0] };
      j.timer = setInterval(() => {
        done = Math.min(items, done + Math.ceil(items / 70));
        j.progress = { ...j.progress, doneItems: done, doneBytes: Math.round(bytes * done / items), current: names[done % names.length] };
        this.emit("progress", { id, progress: j.progress });
        if (done >= items) this.finish(j, items, bytes);
      }, 120);
    }, 900);
  }

  finish(j, items, bytes) {
    clearInterval(j.timer);
    j.status = "in-sync";
    j.progress = null;
    j.pending = null;
    j.lastSynced = new Date().toISOString();
    j.lastRun = { startedAt: ago(10_000), finishedAt: j.lastSynced, copied: items, deleted: 0, bytes, conflicts: 0, failed: 0, cancelled: false, error: null };
    this.publish(j);
  }

  async invoke(command, args = {}) {
    await new Promise(r => setTimeout(r, 30));
    switch (command) {
      case "get_state": return structuredClone({ jobs: this.jobs, settings: this.settings, overview: this.overview(), defaultExcludes: DEFAULT_EXCLUDES });
      case "about": return { name: "Charon", version: "0.1.0", appWebsite: "https://charonsync.app", developer: "DigiFix S.à r.l.-S.", website: "https://digifix.ninja", sourceCode: "https://github.com/digifix-ninja/charon", licence: "MIT", copyrightYear: "2026" };
      case "sync_now": return this.simulateSync(args.id, 140 + Math.round(Math.random() * 400), 300_000_000 + Math.random() * 900_000_000);
      case "sync_all": return this.jobs.filter(j => !j.paused).forEach((j, i) => setTimeout(() => this.simulateSync(j.id, 200, 400_000_000), i * 300));
      case "cancel": {
        const j = this.find(args.id);
        clearInterval(j.timer);
        j.status = "pending"; j.progress = null; j.pending = { items: 40, bytes: 90_000_000, checkedAt: new Date().toISOString() };
        return this.publish(j);
      }
      case "set_paused": {
        const j = this.find(args.id);
        j.paused = args.paused;
        j.status = args.paused ? "paused" : "in-sync";
        return this.publish(j);
      }
      case "set_all_paused":
        this.settings.pausedAll = args.paused;
        return this.emit("overview", this.overview());
      case "set_settings":
        this.settings = args.settings;
        return this.emit("overview", this.overview());
      case "check": return;
      case "job_log": return [
        { time: ago(4 * MIN), level: "info", text: "copied 12 files (8.2 MB), removed 1 item" },
        { time: ago(5 * MIN), level: "info", text: "sync automatically after changes" },
        { time: ago(65 * MIN), level: "warn", text: "Invoices/2026-09.pdf: permission denied" },
        { time: ago(66 * MIN), level: "warn", text: "copied 340 files (1.2 GB), 1 file failed" },
        { time: ago(67 * MIN), level: "info", text: "sync on schedule" },
        { time: ago(3 * 24 * 60 * MIN), level: "info", text: "job created" },
      ];
      case "preview": return {
        summary: { copies: 23, copyBytes: 48_000_000, toDestination: 23, toSource: 0, newFolders: 2, deletes: 3, deletesOnDestination: 3, deletesOnSource: 0, keptBoth: 0, conflicts: 0 },
        steps: [
          { kind: "delete", side: "destination", path: "Drafts/old-plan.txt", dir: false, whole: true },
          { kind: "delete", side: "destination", path: "Scans/2019", dir: true, whole: true },
          { kind: "delete", side: "destination", path: "notes.tmp.txt", dir: false, whole: true },
          { kind: "create-dir", side: "destination", path: "Invoices/2026" },
          { kind: "create-dir", side: "destination", path: "Invoices/2026/10" },
          ...Array.from({ length: 23 }, (_, i) => ({ kind: "copy", to: "destination", path: `Invoices/2026/10/invoice-${String(i + 1).padStart(3, "0")}.pdf`, bytes: 1_200_000 + i * 90_000, replaces: i % 5 === 0 })),
        ],
        totalSteps: 28,
      };
      case "resolve_conflict": {
        const j = this.find(args.id);
        j.conflicts = j.conflicts.filter(c => c.path !== args.path);
        if (!j.conflicts.length) j.status = "in-sync";
        return this.publish(j);
      }
      case "pick_folder": return prompt("Folder path (demo):", args.start || "/Users/andrei/");
      case "suggest_name": return `${args.source} → ${args.destination}`;
      case "add_job": {
        const j = job(`job${Date.now()}`, { ...args.setup, name: args.setup.name || "New job", status: "new", lastSynced: null, lastRun: null });
        this.jobs.push(j);
        this.publish(j);
        return structuredClone(j);
      }
      case "update_job": {
        const j = this.find(args.id);
        Object.assign(j, args.setup);
        this.publish(j);
        return structuredClone(j);
      }
      case "duplicate_job": {
        const original = this.find(args.id);
        const j = { ...structuredClone(original), id: `job${Date.now()}`, name: `${original.name} copy`, paused: true, status: "paused" };
        this.jobs.push(j);
        this.publish(j);
        return j;
      }
      case "remove_job":
        this.jobs = this.jobs.filter(j => j.id !== args.id);
        this.emit("job-removed", args.id);
        return this.emit("overview", this.overview());
      case "reveal_path":
      case "open_url":
      case "move_job":
        return;
      default:
        throw `Unknown command ${command}`;
    }
  }
}
