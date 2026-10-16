// The window's data: jobs, settings and what the person is looking at.

/** The overview tiles and which statuses each one counts. */
export const GROUPS = [
  { key: "all", label: "Jobs", tone: "var(--text-dim)", statuses: null },
  { key: "in-sync", label: "In sync", tone: "var(--in-sync)", statuses: ["in-sync"] },
  { key: "syncing", label: "Crossing", tone: "var(--syncing)", statuses: ["syncing"] },
  { key: "waiting", label: "Waiting", tone: "var(--pending)", statuses: ["pending", "new", "paused", "offline"] },
  { key: "attention", label: "Need you", tone: "var(--error)", statuses: ["error", "conflict"] },
];

export class JobStore {
  constructor() {
    this.jobs = new Map();
    this.order = [];
    this.settings = null;
    this.overview = null;
    this.defaultExcludes = [];
    this.group = "all";
    this.query = "";
    this.open = new Set();      // ids of expanded rows
    this.tabs = new Map();      // id → open tab in the expanded row
    this.logs = new Map();      // id → log lines
    this.listeners = [];
  }

  /** Takes the full state from Rust. */
  load(state) {
    this.jobs = new Map(state.jobs.map(job => [job.id, job]));
    this.order = state.jobs.map(job => job.id);
    this.settings = state.settings;
    this.overview = state.overview;
    this.defaultExcludes = state.defaultExcludes;
    this.changed();
  }

  /** A new or changed row. */
  put(view) {
    if (!this.jobs.has(view.id)) this.order.push(view.id);
    this.jobs.set(view.id, view);
    this.changed({ id: view.id });
  }

  /** A removed row. */
  drop(id) {
    this.jobs.delete(id);
    this.order = this.order.filter(other => other !== id);
    this.open.delete(id);
    this.changed();
  }

  /** Live progress of a running sync (doesn't redraw the whole list). */
  setProgress(id, progress) {
    const job = this.jobs.get(id);
    if (!job) return;
    job.progress = progress;
    job.status = "syncing";
    this.changed({ id, progressOnly: true });
  }

  setOverview(overview) {
    this.overview = overview;
    this.changed({ overviewOnly: true });
  }

  setLog(id, lines) {
    this.logs.set(id, lines);
    this.changed({ id });
  }

  /** Expands or folds a row. */
  toggleOpen(id) {
    if (this.open.has(id)) this.open.delete(id);
    else this.open.add(id);
    this.changed({ id });
    return this.open.has(id);
  }

  /** The open tab of an expanded row. */
  tabOf(id) {
    const job = this.jobs.get(id);
    return this.tabs.get(id) ?? (job?.conflicts?.length ? "conflicts" : "activity");
  }

  setTab(id, tab) {
    this.tabs.set(id, tab);
    this.changed({ id });
  }

  /** All jobs in list order. */
  all() {
    return this.order.map(id => this.jobs.get(id)).filter(Boolean);
  }

  /** The jobs the current tile and search let through. */
  visible() {
    const group = GROUPS.find(g => g.key === this.group);
    const query = this.query.toLowerCase();
    return this.all().filter(job => {
      if (group?.statuses && !group.statuses.includes(job.status)) return false;
      if (!query) return true;
      return [job.name, job.source, job.destination].some(text => String(text).toLowerCase().includes(query));
    });
  }

  /** How many jobs each tile counts. */
  counts() {
    const jobs = this.all();
    return Object.fromEntries(GROUPS.map(g => [g.key, g.statuses ? jobs.filter(j => g.statuses.includes(j.status)).length : jobs.length]));
  }

  subscribe(listener) {
    this.listeners.push(listener);
  }

  /** Tells the views something changed. `hint` says what, so they can redraw less. */
  changed(hint = {}) {
    for (const listener of this.listeners) listener(hint);
  }
}
