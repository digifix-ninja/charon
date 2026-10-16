// The dry run: what the next sync would do, before anything is touched.

import { Api } from "../api.js";
import { Dom } from "../dom.js";
import { Format } from "../format.js";

const { html, raw } = Dom;

export class PreviewDialog {
  constructor(store, toast, { syncNow }) {
    this.store = store;
    this.toast = toast;
    this.syncNow = syncNow;
    this.element = Dom.$("#preview");
    this.element.addEventListener("click", event => {
      const action = event.target.closest("[data-do]")?.dataset.do;
      if (action === "close") this.close();
      if (action === "sync") { this.close(); this.syncNow(this.jobId); }
    });
  }

  get isOpen() {
    return this.element.classList.contains("open");
  }

  /** Opens the dialog and scans both folders. */
  async show(id) {
    const job = this.store.jobs.get(id);
    if (!job) return;
    this.jobId = id;
    this.render(job, null);
    this.element.classList.add("open");
    this.element.setAttribute("aria-hidden", "false");
    Dom.$("#scrim").classList.add("on");
    try {
      const preview = await Api.preview(id);
      if (this.jobId === id && this.isOpen) this.render(job, preview);
    } catch (error) {
      this.close();
      this.toast.error(error);
    }
  }

  close() {
    this.element.classList.remove("open");
    this.element.setAttribute("aria-hidden", "true");
    if (!document.querySelector(".sheet.open")) Dom.$("#scrim").classList.remove("on");
  }

  render(job, preview) {
    this.element.innerHTML = html`
      <header>
        <span class="eyebrow"><span class="num">DRY RUN</span> Nothing is changed yet</span>
        <h2>${job.name}</h2>
        <p class="mono" style="font-size: 11.5px">${Format.path(job.source)} ${Format.arrow(job.mode)} ${Format.path(job.destination)}</p>
      </header>
      ${raw(preview ? this.tally(preview.summary, job) : "")}
      ${raw(preview ? this.steps(preview, job) : '<div class="scanning"><div class="radar"></div>Looking at both shores…</div>')}
      <footer>
        <span class="note">${preview ? `${Format.plural(preview.totalSteps, "step")} planned` : ""}</span>
        <span class="spacer"></span>
        <button class="btn ghost" data-do="close">Close</button>
        <button class="btn primary" data-do="sync" ${preview ? "" : "disabled"}>Sync now</button>
      </footer>`;
  }

  tally(s, job) {
    const twoWay = job.mode === "two-way";
    const copySub = twoWay ? `${s.toDestination} → · ${s.toSource} ←` : Format.bytes(s.copyBytes);
    const delSub = job.safeDelete ? "to the trash" : "permanently";
    return html`<div class="tally">
        <div class="copy"><div class="v">${Format.count(s.copies + s.keptBoth)}</div><div class="k">Copy</div><div class="sub">${copySub}</div></div>
        <div class="del"><div class="v">${Format.count(s.deletes)}</div><div class="k">Remove</div><div class="sub">${s.deletes ? delSub : "nothing"}</div></div>
        <div class="dir"><div class="v">${Format.count(s.newFolders)}</div><div class="k">New folders</div><div class="sub">&nbsp;</div></div>
        <div class="cf"><div class="v">${Format.count(s.conflicts + s.keptBoth)}</div><div class="k">Conflicts</div><div class="sub">${s.keptBoth ? "both kept" : s.conflicts ? "you decide" : "none"}</div></div>
      </div>`;
  }

  steps(preview, job) {
    if (preview.totalSteps === 0) return '<div class="steps"><div class="settled">Both shores already match. Nothing to carry.</div></div>';
    const rows = preview.steps.slice(0, 400).map((step, i) => this.stepHtml(step, job, i)).join("");
    const more = preview.totalSteps > preview.steps.length
      ? html`<div class="more">… and ${Format.count(preview.totalSteps - preview.steps.length)} more</div>` : "";
    return `<div class="steps">${rows}${more}</div>`;
  }

  stepHtml(step, job, i) {
    const towards = side => (side === "destination" ? "→ destination" : "← source");
    let symbol = "", note = "";
    switch (step.kind) {
      case "copy": symbol = step.replaces ? "↻" : "+"; note = `${Format.bytes(step.bytes)} ${job.mode === "two-way" ? towards(step.to) : ""}`; break;
      case "create-dir": symbol = "▸"; note = job.mode === "two-way" ? `folder ${towards(step.side)}` : "folder"; break;
      case "delete": symbol = "−"; note = `${step.dir ? "folder" : "file"} on ${step.side}${job.safeDelete ? " · to trash" : ""}`; break;
      case "keep-both": symbol = "⑂"; note = "both versions kept"; break;
      case "conflict": symbol = "?"; note = "you decide"; break;
    }
    return html`<div class="s ${step.kind}" style="animation-delay: ${Math.min(i, 30) * 12}ms"><span class="sym">${symbol}</span><span class="p">${step.path}</span><span class="m">${note}</span></div>`;
  }
}
