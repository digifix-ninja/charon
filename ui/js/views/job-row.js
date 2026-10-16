// The markup of one job row: status orb, name, shores, progress or info, actions,
// and the expandable detail (activity log, conflicts, details).

import { Dom } from "../dom.js";
import { Format } from "../format.js";
import { Icons } from "../icons.js";

const { html, raw } = Dom;

export class JobRow {
  /** The whole row. */
  static render(job, { open, tab, log, index }) {
    return html`
      <article class="job ${open ? "open" : ""}" data-id="${job.id}" data-status="${job.status}" style="--i: ${index}">
        <div class="edge"></div>
        <div class="main" data-action="toggle">
          <div class="orb" title="${Format.statusName(job.status)}">
            <span class="ico">${raw(Icons.status[job.status] ?? "")}</span>
          </div>
          <div class="body">
            <div class="title-line">
              <span class="name">${job.name}</span>
              <span class="tag status">${Format.statusName(job.status)}</span>
              <span class="tag">${Format.modeName(job.mode)}</span>
            </div>
            <div class="shores">
              <span class="shore ${job.sourceOnline ? "" : "off"}" title="${job.source}"><bdi>${Format.path(job.source)}</bdi></span>
              <span class="arrow">${Format.arrow(job.mode)}</span>
              <span class="shore ${job.destinationOnline ? "" : "off"}" title="${job.destination}"><bdi>${Format.path(job.destination)}</bdi></span>
            </div>
            ${raw(job.status === "syncing" ? JobRow.progress(job.progress) : JobRow.info(job))}
          </div>
          ${raw(JobRow.actions(job))}
        </div>
        <div class="detail"><div class="inner">${raw(open ? JobRow.detail(job, tab, log) : "")}</div></div>
      </article>`;
  }

  /** Progress bar and numbers while syncing. */
  static progress(p) {
    if (!p || p.phase === "scanning" || p.totalItems === 0) {
      return html`<div class="progress"><div class="bar indeterminate"><div class="fill"></div></div>
        <div class="numbers"><span>${p?.phase === "finishing" ? "Finishing…" : "Looking at both shores…"}</span></div></div>`;
    }
    const share = p.totalBytes > 0 ? p.doneBytes / p.totalBytes : p.doneItems / p.totalItems;
    const percent = Math.min(100, Math.round(share * 1000) / 10);
    return html`<div class="progress">
        <div class="bar"><div class="fill" style="--p: ${percent}%"></div></div>
        <div class="numbers">
          <span>${Format.count(p.doneItems)} / ${Format.count(p.totalItems)} · ${Format.bytes(p.doneBytes)} of ${Format.bytes(p.totalBytes)}</span>
          <span class="current"><bdi>${p.current ?? ""}</bdi></span>
        </div>
      </div>`;
  }

  /** The one-line summary under the shores. */
  static info(job) {
    const parts = [];
    const synced = job.lastSynced ? `Synced ${Format.ago(job.lastSynced)}` : "Never synced";
    switch (job.status) {
      case "new":
        parts.push("Not synced yet", "Preview first to see what will cross");
        break;
      case "pending":
        parts.push(html`<span class="warn">${Format.plural(job.pending.items, "change")} waiting${job.pending.bytes ? ` (${Format.bytes(job.pending.bytes)})` : ""}</span>`, synced);
        break;
      case "conflict":
        parts.push(html`<span class="warn">${Format.plural(job.conflicts.length, "conflict")} to resolve</span>`, synced);
        break;
      case "error":
      case "offline":
        parts.push(html`<span class="warn">${job.problem ?? "Something went wrong"}</span>`);
        break;
      case "paused":
        parts.push("Paused", synced);
        break;
      default:
        parts.push(synced);
        if (job.lastRun && job.lastRun.copied + job.lastRun.deleted > 0) {
          parts.push(`last run carried ${Format.plural(job.lastRun.copied, "file")}${job.lastRun.deleted ? `, removed ${job.lastRun.deleted}` : ""}`);
        }
    }
    parts.push(job.nextRun ? `next ${Format.upcoming(job.nextRun)}` : Format.trigger(job.trigger));
    return `<div class="info">${parts.join('<span class="sep">·</span>')}</div>`;
  }

  static actions(job) {
    const running = job.status === "syncing";
    const offline = job.status === "offline";
    return html`<div class="row-actions">
        ${raw(running
          ? html`<button class="icon-btn sync" data-action="stop" title="Stop"><span class="ico">${raw(Icons.stop)}</span></button>`
          : html`<button class="icon-btn sync" data-action="sync" title="Sync now" ${offline ? "disabled" : ""}><span class="ico">${raw(Icons.play)}</span></button>`)}
        <button class="icon-btn" data-action="folders" data-menu-anchor title="Open folder"><span class="ico">${raw(Icons.folder)}</span></button>
        <button class="icon-btn ${job.paused ? "on" : ""}" data-action="${job.paused ? "resume" : "pause"}" title="${job.paused ? "Resume" : "Pause"}">
          <span class="ico">${raw(job.paused ? Icons.resume : Icons.pause)}</span></button>
        <button class="icon-btn" data-action="more" data-menu-anchor title="More"><span class="ico">${raw(Icons.more)}</span></button>
      </div>`;
  }

  // ---------------------------------------------------------------- detail

  static detail(job, tab, log) {
    const tabs = [["activity", "Activity"], ["details", "Details"]];
    if (job.conflicts.length) tabs.unshift(["conflicts", "Conflicts"]);
    const body = tab === "conflicts" && job.conflicts.length ? JobRow.conflicts(job)
      : tab === "details" ? JobRow.facts(job)
      : JobRow.log(log);
    return html`<div class="panel">
        <div class="tabs">${tabs.map(([key, label]) => raw(html`<button data-action="tab" data-tab="${key}" class="${tab === key ? "on" : ""}">${label}${raw(key === "conflicts" ? `<span class="badge">${job.conflicts.length}</span>` : "")}</button>`))}</div>
        ${raw(body)}
      </div>`;
  }

  static log(lines) {
    if (!lines) return '<div class="log"><div class="none">Loading…</div></div>';
    if (!lines.length) return '<div class="log"><div class="none">Nothing has happened yet.</div></div>';
    return `<div class="log">${lines.map(l => html`<div class="line ${l.level}"><span class="time">${Format.stamp(l.time)}</span><span class="text">${l.text}</span></div>`).join("")}</div>`;
  }

  static facts(job) {
    const fact = (k, v, wide = false) => html`<div class="fact ${wide ? "wide" : ""}"><div class="k">${k}</div><div class="v">${raw(v)}</div></div>`;
    const esc = Dom.escape;
    return `<div class="facts">
      ${fact("Mode", esc(Format.modeName(job.mode)))}
      ${fact("Runs", esc(Format.trigger(job.trigger)))}
      ${fact("Deletes", job.safeDelete ? "To <code>.charon-trash</code>" : "Permanent")}
      ${fact("Conflicts", esc(job.mode === "two-way" ? Format.ruleName(job.conflict) : "—"))}
      ${fact("Source", `<code>${esc(job.source)}</code>`, true)}
      ${fact("Destination", `<code>${esc(job.destination)}</code>`, true)}
      ${fact("Excluded", job.excludes.length ? job.excludes.map(e => `<code>${esc(e)}</code>`).join(", ") : "Nothing", true)}
      ${fact("Next run", esc(job.nextRun ? Format.upcoming(job.nextRun) : job.paused ? "Paused" : "When triggered"))}
      ${fact("Last run", esc(job.lastRun ? Format.ago(job.lastRun.finishedAt) : "Never"))}
    </div>`;
  }

  static conflicts(job) {
    const version = (label, modified, size) =>
      modified == null ? `${label}: deleted` : `${label}: ${Format.bytes(size)}, ${Format.ago(new Date(modified).toISOString())}`;
    return `<div class="conflict-list">${job.conflicts.map(c => html`
      <div class="conflict">
        <div>
          <div class="path">${c.path}</div>
          <div class="versions">${version("Source", c.sourceModified, c.sourceSize)} · ${version("Destination", c.destinationModified, c.destinationSize)}</div>
        </div>
        <div class="choices">
          <button class="btn" data-action="resolve" data-path="${c.path}" data-choice="keep-source">Keep source</button>
          <button class="btn" data-action="resolve" data-path="${c.path}" data-choice="keep-destination">Keep destination</button>
          <button class="btn" data-action="resolve" data-path="${c.path}" data-choice="keep-both">Keep both</button>
        </div>
      </div>`).join("")}</div>`;
  }
}
