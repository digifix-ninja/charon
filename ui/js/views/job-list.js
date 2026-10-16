// The list of job rows. Rows are redrawn one at a time when their job changes; live
// progress only touches the bar, so hovering and clicking stay smooth during a sync.

import { Dom } from "../dom.js";
import { Logo } from "../logo.js";
import { JobRow } from "./job-row.js";
import { PointerGlow } from "../effects/glow.js";

export class JobList {
  constructor(store, handlers) {
    this.store = store;
    this.handlers = handlers;
    this.element = Dom.$("#jobs");
    this.element.addEventListener("click", event => this.onClick(event));
    PointerGlow.follow(this.element, ".job");
  }

  /** Redraws what `hint` says changed (everything when it says nothing). */
  render(hint = {}) {
    if (hint.overviewOnly) return;
    if (hint.id && this.rowOf(hint.id) && this.store.visible().some(j => j.id === hint.id)) {
      return hint.progressOnly ? this.renderProgress(hint.id) : this.renderRow(hint.id);
    }
    this.renderAll();
  }

  renderAll() {
    const jobs = this.store.visible();
    if (this.store.jobs.size === 0) {
      this.element.innerHTML = `
        <div class="empty-state">
          ${Logo.html()}
          <h2>No crossings yet</h2>
          <p>A sync job connects two folders. Charon carries files across, one way or both ways,
            by hand, on change, on a schedule or when a drive is connected.</p>
          <button class="btn primary" data-action="new">Create your first job</button>
        </div>`;
      return;
    }
    if (jobs.length === 0) {
      this.element.innerHTML = '<div class="no-match">No job matches.</div>';
      return;
    }
    this.element.innerHTML = jobs.map((job, index) => this.rowHtml(job, index)).join("");
  }

  renderRow(id) {
    const old = this.rowOf(id);
    const index = Number(old.style.getPropertyValue("--i")) || 0;
    const wrapper = document.createElement("div");
    wrapper.innerHTML = this.rowHtml(this.store.jobs.get(id), index);
    const row = wrapper.firstElementChild;
    row.style.animation = "none"; // no entrance animation on updates
    for (const prop of ["--mx", "--my"]) row.style.setProperty(prop, old.style.getPropertyValue(prop));
    old.replaceWith(row);
  }

  /** Only the progress block, so the row isn't rebuilt many times a second. */
  renderProgress(id) {
    const row = this.rowOf(id);
    const job = this.store.jobs.get(id);
    if (row.dataset.status !== "syncing") return this.renderRow(id);
    const current = row.querySelector(".progress");
    const wrapper = document.createElement("div");
    wrapper.innerHTML = JobRow.progress(job.progress);
    const next = wrapper.firstElementChild;
    const oldFill = current?.querySelector(".bar:not(.indeterminate) .fill");
    const newFill = next.querySelector(".bar:not(.indeterminate) .fill");
    if (oldFill && newFill) {
      // Same bar: move it, so the width animates instead of jumping.
      oldFill.style.setProperty("--p", newFill.style.getPropertyValue("--p"));
      current.querySelector(".numbers").replaceWith(next.querySelector(".numbers"));
    } else if (current) {
      current.replaceWith(next);
    }
  }

  rowHtml(job, index) {
    const id = job.id;
    return JobRow.render(job, {
      open: this.store.open.has(id),
      tab: this.store.tabOf(id),
      log: this.store.logs.get(id),
      index,
    });
  }

  rowOf(id) {
    return this.element.querySelector(`.job[data-id="${CSS.escape(id)}"]`);
  }

  /** Every click inside the list ends up here. */
  onClick(event) {
    const target = event.target.closest("[data-action]");
    if (!target) return;
    const action = target.dataset.action;
    if (action === "new") return this.handlers.newJob();
    const id = target.closest(".job")?.dataset.id;
    if (!id) return;
    if (action === "toggle" && event.target.closest("button")) return; // buttons inside the row
    this.handlers.act(action, id, target);
  }
}
