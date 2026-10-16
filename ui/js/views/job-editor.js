// The job editor: a numbered walk through everything a job needs.
// 01 Shores · 02 Mode · 03 Conflicts (two-way only) · 04 When · 05 Leave out · 06 Safety · 07 Name

import { Api } from "../api.js";
import { Dom } from "../dom.js";
import { Format } from "../format.js";
import { Icons } from "../icons.js";
import { Sheet } from "./sheet.js";

const { html, raw } = Dom;

const MODES = [
  { key: "mirror", title: "Mirror", text: "The destination becomes an exact copy. Deletions carry over." },
  { key: "backup", title: "Backup", text: "New and changed files are copied. Nothing is ever deleted there." },
  { key: "two-way", title: "Two-way", text: "Changes flow both ways. Charon remembers what was synced." },
];
const RULES = [
  { key: "keep-both", label: "Keep both" },
  { key: "newest-wins", label: "Newest wins" },
  { key: "ask", label: "Ask me" },
];
const TRIGGERS = [
  { key: "manual", label: "Manual", make: () => ({ kind: "manual" }) },
  { key: "on-change", label: "On change", make: () => ({ kind: "on-change", settleSeconds: 5 }) },
  { key: "interval", label: "Every…", make: () => ({ kind: "interval", minutes: 60 }) },
  { key: "daily", label: "Daily", make: () => ({ kind: "daily", hour: 3, minute: 0 }) },
  { key: "drive-connected", label: "Drive connected", make: () => ({ kind: "drive-connected" }) },
];

export class JobEditor extends Sheet {
  constructor(store, toast, { saved }) {
    super(Dom.$("#editor"));
    this.store = store;
    this.toast = toast;
    this.saved = saved;
    this.element.addEventListener("click", event => this.onClick(event));
    this.element.addEventListener("input", event => this.onInput(event));
    this.element.addEventListener("keydown", event => this.onKey(event));
  }

  /** Opens an empty editor for a new job. */
  create() {
    this.editingId = null;
    this.nameTouched = false;
    this.draft = {
      name: "",
      source: "",
      destination: "",
      mode: "mirror",
      conflict: "keep-both",
      trigger: { kind: "manual" },
      excludes: [...this.store.defaultExcludes],
      safeDelete: true,
    };
    this.render();
    this.open();
  }

  /** Opens the editor with an existing job. */
  edit(id) {
    const job = this.store.jobs.get(id);
    if (!job) return;
    this.editingId = id;
    this.nameTouched = true;
    const { name, source, destination, mode, conflict, trigger, excludes, safeDelete } = job;
    this.draft = structuredClone({ name, source, destination, mode, conflict, trigger, excludes, safeDelete });
    this.render();
    this.open();
  }

  // ---------------------------------------------------------------- drawing

  render() {
    const d = this.draft;
    const editing = Boolean(this.editingId);
    this.element.innerHTML = html`
      <header>
        <div>
          <span class="eyebrow"><span class="num">${editing ? "EDIT" : "NEW"}</span> Sync job</span>
          <h2>${editing ? d.name : "Plan a crossing"}</h2>
        </div>
        <button class="icon-btn" data-do="close" title="Close"><span class="ico">${raw(Icons.close)}</span></button>
      </header>
      <div class="scroll">
        ${raw(this.step(1, "Shores", "Where files come from, and where they go.", this.shoresHtml()))}
        ${raw(this.step(2, "Mode", "How files cross.", this.modesHtml()))}
        ${raw(this.step(3, "When both sides change", "Two-way only: what happens if a file was edited on both sides.", this.segmented("conflict", RULES, d.conflict), d.mode !== "two-way"))}
        ${raw(this.step(4, "When", "Charon runs the job by itself, or waits for you.", this.triggerHtml()))}
        ${raw(this.step(5, "Leave out", "Names like node_modules or *.tmp match anywhere; a/b matches from the top folder. Press Enter to add.", this.chipsHtml()))}
        ${raw(this.step(6, "Safety", "", this.safetyHtml()))}
        ${raw(this.step(7, "Name", "", html`<input class="input" data-field="name" value="${d.name}" placeholder="${this.suggestedName() || "Photos → Backup SSD"}" spellcheck="false">`))}
      </div>
      <footer>
        ${raw(editing ? html`<button class="btn danger" data-do="delete">Delete</button>` : "")}
        <span class="spacer"></span>
        <button class="btn ghost" data-do="close">Cancel</button>
        <button class="btn primary" data-do="save">${editing ? "Save" : "Save and preview"}</button>
      </footer>`;
  }

  step(number, title, hint, body, hidden = false) {
    return html`<section class="step ${hidden ? "hidden" : ""}">
        <span class="n">${String(number).padStart(2, "0")}</span>
        <div class="what">
          <h3>${title}</h3>
          ${raw(hint ? html`<p class="hint">${hint}</p>` : "")}
          ${raw(body)}
        </div>
      </section>`;
  }

  shoresHtml() {
    const pick = (field, label) => {
      const path = this.draft[field];
      return html`<div class="field"><span class="field-label">${label}</span>
        <div class="path-pick ${path ? "filled" : ""}" data-do="pick" data-field="${field}">
          <span class="ico">${raw(Icons.folder)}</span>
          <span class="path ${path ? "" : "blank"}">${path || "Choose a folder…"}</span>
          <span class="btn ghost">Choose</span>
        </div></div>`;
    };
    return html`<div class="shores-pick">
        ${raw(pick("source", "Source"))}
        <button class="icon-btn swap" data-do="swap" title="Swap direction"><span class="ico">${raw(Icons.swap)}</span></button>
        ${raw(pick("destination", "Destination"))}
      </div>`;
  }

  modesHtml() {
    return html`<div class="cards">${MODES.map(m => raw(html`
      <button class="card-choice ${this.draft.mode === m.key ? "on" : ""}" data-do="mode" data-value="${m.key}">
        <span class="diagram">${raw(Icons.mode[m.key])}</span>
        <strong>${m.title}</strong>
        <small>${m.text}</small>
      </button>`))}</div>`;
  }

  segmented(field, options, current) {
    return html`<div class="segmented">${options.map(o => raw(html`
      <button data-do="choose" data-field="${field}" data-value="${o.key}" class="${current === o.key ? "on" : ""}">${o.label}</button>`))}</div>`;
  }

  triggerHtml() {
    const t = this.draft.trigger;
    let extra = "";
    if (t.kind === "on-change") {
      extra = html`Wait <input class="input small" type="number" min="1" max="3600" data-trigger="settleSeconds" value="${t.settleSeconds}"> seconds after the last change`;
    } else if (t.kind === "interval") {
      extra = html`Every <input class="input small" type="number" min="1" data-trigger="minutes" value="${t.minutes}"> minutes`;
    } else if (t.kind === "daily") {
      const time = `${String(t.hour).padStart(2, "0")}:${String(t.minute).padStart(2, "0")}`;
      extra = html`At <input class="input small" type="time" data-trigger="time" value="${time}" style="width: 110px"> every day`;
    } else if (t.kind === "drive-connected") {
      extra = "As soon as both folders are reachable, for example when you plug in the backup drive.";
    }
    return html`${raw(this.segmented("trigger", TRIGGERS, t.kind))}<div class="trigger-extra">${raw(extra)}</div>`;
  }

  chipsHtml() {
    return html`<div class="chips" data-do="focus-chips">
        ${this.draft.excludes.map((pattern, i) => raw(html`<span class="chip">${pattern}<button data-do="unchip" data-index="${i}" title="Remove">×</button></span>`))}
        <input data-field="chip" placeholder="Add a pattern" spellcheck="false">
      </div>`;
  }

  safetyHtml() {
    return html`<label class="toggle">
        <span class="copy"><strong>Safe deletes</strong>
          <small>Removed or overwritten files go to a .charon-trash folder for ${this.store.settings?.trashDays || 30} days instead of disappearing.</small></span>
        <input type="checkbox" data-field="safeDelete" ${this.draft.safeDelete ? "checked" : ""}><span class="switch"></span>
      </label>`;
  }

  /** "Photos → Backup SSD", from the chosen folders. */
  suggestedName() {
    const { source, destination, mode } = this.draft;
    if (!source || !destination) return "";
    const base = p => p.split(/[\\/]/).filter(Boolean).pop() || p;
    return `${base(source)} ${Format.arrow(mode)} ${base(destination)}`;
  }

  /** Redraws one step's body without losing focus elsewhere. */
  refresh() {
    const focused = document.activeElement?.dataset?.field;
    this.render();
    if (focused === "chip") Dom.$('[data-field="chip"]', this.element)?.focus();
  }

  // ---------------------------------------------------------------- input

  async onClick(event) {
    const target = event.target.closest("[data-do]");
    if (!target) return;
    const d = this.draft;
    switch (target.dataset.do) {
      case "close": return this.close();
      case "pick": {
        const field = target.dataset.field;
        const folder = await Api.pickFolder(d[field] || d.source || d.destination);
        if (folder) {
          d[field] = folder;
          this.autoName();
          this.refresh();
        }
        return;
      }
      case "swap":
        [d.source, d.destination] = [d.destination, d.source];
        this.autoName();
        return this.refresh();
      case "mode":
        d.mode = target.dataset.value;
        this.autoName();
        return this.refresh();
      case "choose": {
        const { field, value } = target.dataset;
        if (field === "trigger") d.trigger = TRIGGERS.find(t => t.key === value).make();
        else d[field] = value;
        return this.refresh();
      }
      case "unchip":
        d.excludes.splice(Number(target.dataset.index), 1);
        return this.refresh();
      case "focus-chips":
        if (event.target === target) Dom.$('[data-field="chip"]', this.element).focus();
        return;
      case "save": return this.save(target);
      case "delete": return this.remove();
    }
  }

  onInput(event) {
    const el = event.target;
    const d = this.draft;
    if (el.dataset.field === "name") {
      d.name = el.value;
      this.nameTouched = el.value.trim() !== "";
    } else if (el.dataset.field === "safeDelete") {
      d.safeDelete = el.checked;
    } else if (el.dataset.trigger === "time") {
      const [hour, minute] = el.value.split(":").map(Number);
      Object.assign(d.trigger, { hour: hour || 0, minute: minute || 0 });
    } else if (el.dataset.trigger) {
      d.trigger[el.dataset.trigger] = Math.max(1, Number(el.value) || 1);
    }
  }

  onKey(event) {
    const el = event.target;
    if (el.dataset.field === "chip" && (event.key === "Enter" || event.key === ",")) {
      event.preventDefault();
      const pattern = el.value.trim().replace(/,$/, "");
      if (pattern && !this.draft.excludes.includes(pattern)) this.draft.excludes.push(pattern);
      this.refresh();
    } else if (el.dataset.field === "chip" && event.key === "Backspace" && el.value === "" && this.draft.excludes.length) {
      this.draft.excludes.pop();
      this.refresh();
    } else if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      this.save(Dom.$('[data-do="save"]', this.element));
    }
  }

  /** Keeps the name in step with the folders until the person types their own. */
  autoName() {
    if (!this.nameTouched) this.draft.name = this.suggestedName();
  }

  async save(button) {
    const pending = Dom.$('[data-field="chip"]', this.element)?.value.trim();
    if (pending && !this.draft.excludes.includes(pending)) this.draft.excludes.push(pending);
    button.disabled = true;
    try {
      const view = this.editingId
        ? await Api.updateJob(this.editingId, this.draft)
        : await Api.addJob(this.draft);
      this.close();
      this.saved(view, { isNew: !this.editingId });
    } catch (error) {
      this.toast.error(error);
    } finally {
      button.disabled = false;
    }
  }

  async remove() {
    const id = this.editingId;
    if (!confirm(`Delete “${this.draft.name}”? The folders and files stay where they are.`)) return;
    try {
      await Api.removeJob(id);
      this.close();
    } catch (error) {
      this.toast.error(error);
    }
  }
}
