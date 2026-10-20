// The markup of one job row: status orb, name, shores, progress or info, flags, actions,
// and the expandable detail (conflicts, places that couldn't be read, activity, details).

import { Dom } from "../dom.js";
import { Format } from "../format.js";
import { Icons } from "../icons.js";

const { html, raw } = Dom;

const PRIVACY_TEXT = "macOS didn't let Charon read this. Allow Charon in System Settings › Privacy & Security › Full Disk Access.";

export class JobRow {
  /** The whole row. */
  static render(job, { open, tab, log, index, scan, trashDays }) {
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
              ${raw(job.fullAutomation ? '<span class="tag auto" title="Full automation: this job changes files without asking you first">Full auto</span>' : "")}
            </div>
            <div class="shores">
              ${raw(JobRow.shore(job.source, job.sourceVolume, job.sourceOnline))}
              <span class="arrow">${Format.arrow(job.mode)}</span>
              ${raw(JobRow.shore(job.destination, job.destinationVolume, job.destinationOnline))}
            </div>
            ${raw(job.status === "syncing" ? JobRow.progress(job.progress, scan) : JobRow.info(job))}
            ${raw(JobRow.flags(job))}
            ${raw(job.stale ? html`<div class="stale"><span class="ico">${raw(Icons.warn)}</span>${job.stale}</div>` : "")}
          </div>
          ${raw(JobRow.actions(job))}
        </div>
        <div class="detail"><div class="inner">${raw(open ? JobRow.detail(job, tab, log, trashDays) : "")}</div></div>
      </article>`;
  }

  /** One folder, and the drive it's on when known: "on Backup SSD (exFAT)". */
  static shore(path, volume, online) {
    return html`<span class="shore ${online ? "" : "off"}" title="${path}"><bdi>${Format.path(path)}</bdi></span>
      ${raw(volume ? html`<span class="vol">on ${Format.volume(volume)}</span>` : "")}`;
  }

  /** Progress bar and numbers while syncing. */
  static progress(p, scan) {
    if (!p || p.phase === "scanning" || p.totalItems === 0) {
      const counts = scan && p?.phase !== "finishing"
        ? html`<span class="counts">source: ${Format.count(scan.source)} · destination: ${Format.count(scan.destination)}</span>`
        : "";
      return html`<div class="progress"><div class="bar indeterminate"><div class="fill"></div></div>
        <div class="numbers"><span>${p?.phase === "finishing" ? "Finishing…" : "Looking at both shores…"}</span>${raw(counts)}</div></div>`;
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
        parts.push("Not synced yet", "Nothing crosses until you confirm the list");
        break;
      case "review":
        if (!job.waiting) parts.push(html`<span class="warn">The next sync waits for your OK</span>`);
        parts.push(synced);
        break;
      case "pending":
        parts.push(html`<span class="warn">${Format.plural(job.pending?.items ?? 0, "change")} queued${job.pending?.bytes ? ` (${Format.bytes(job.pending.bytes)})` : ""}</span>`, synced);
        break;
      case "conflict":
        parts.push(html`<span class="warn">${Format.plural(job.conflicts.length, "conflict")} to settle</span>`, synced);
        break;
      case "error":
      case "offline":
        parts.push(html`<span class="warn">${job.problem ?? "Something went wrong"}</span>`);
        break;
      case "paused": {
        // Paused shows first, but a failed last run still says what went wrong.
        const problem = job.problem ?? job.lastRun?.error;
        parts.push("Paused", problem ? html`<span class="bad">${problem}</span>` : synced);
        break;
      }
      default:
        parts.push(synced);
        if (job.lastRun && job.lastRun.copied + job.lastRun.deleted > 0) {
          parts.push(`last run carried ${Format.plural(job.lastRun.copied, "file")}${job.lastRun.deleted ? `, removed ${job.lastRun.deleted}` : ""}`);
        }
    }
    if (!job.paused && job.status !== "offline") parts.push(Dom.escape(Format.schedule(job)));
    return `<div class="info">${parts.join('<span class="sep">·</span>')}</div>`;
  }

  /** Chips that show whatever the status: changes waiting for an OK, places not read. */
  static flags(job) {
    const flags = [];
    if (job.waiting) {
      flags.push(html`<button class="flag review" data-action="review" title="Look at the list of changes">
        <span class="ico">${raw(Icons.review)}</span><span>${Format.waiting(job.waiting)}</span></button>`);
    }
    if (job.unreadable?.length) {
      const n = job.unreadable.length;
      flags.push(html`<button class="flag unreadable" data-action="unreadable" title="Show the places">
        <span class="ico">${raw(Icons.warn)}</span><span>${n === 1 ? "1 place couldn't be read" : `${Format.count(n)} places couldn't be read`}</span></button>`);
    }
    return flags.length ? `<div class="flags">${flags.join("")}</div>` : "";
  }

  /** The main button (Stop, Sync now, or Review and sync), then folders, pause and more. */
  static actions(job) {
    const running = job.status === "syncing";
    const offline = !job.sourceOnline || !job.destinationOnline;
    let main;
    if (running) {
      main = html`<button class="btn row-main" data-action="main"><span class="ico">${raw(Icons.stop)}</span>Stop</button>`;
    } else if (job.runsByItself && !job.waiting) {
      main = html`<button class="btn row-main" data-action="main" ${offline ? "disabled" : ""}><span class="ico">${raw(Icons.play)}</span>Sync now</button>`;
    } else {
      main = html`<button class="btn row-main ${job.waiting ? "wants" : ""}" data-action="main" ${offline ? "disabled" : ""}
        title="See the list of changes, then confirm it"><span class="ico">${raw(Icons.review)}</span>Review and sync</button>`;
    }
    return html`<div class="row-actions">
        ${raw(main)}
        <button class="icon-btn" data-action="folders" data-menu-anchor title="Open folder"><span class="ico">${raw(Icons.folder)}</span></button>
        <button class="icon-btn ${job.paused ? "on" : ""}" data-action="${job.paused ? "resume" : "pause"}" title="${job.paused ? "Resume" : "Pause"}">
          <span class="ico">${raw(job.paused ? Icons.resume : Icons.pause)}</span></button>
        <button class="icon-btn" data-action="more" data-menu-anchor title="More"><span class="ico">${raw(Icons.more)}</span></button>
      </div>`;
  }

  // ---------------------------------------------------------------- detail

  static detail(job, tab, log, trashDays) {
    const tabs = [["activity", "Activity"], ["details", "Details"]];
    if (job.unreadable?.length) tabs.unshift(["unreadable", "Couldn't read", job.unreadable.length]);
    if (job.conflicts.length) tabs.unshift(["conflicts", "Conflicts", job.conflicts.length]);
    const body = tab === "conflicts" && job.conflicts.length ? JobRow.conflicts(job)
      : tab === "unreadable" && job.unreadable?.length ? JobRow.unreadable(job)
      : tab === "details" ? JobRow.facts(job, trashDays)
      : JobRow.log(log);
    return html`<div class="panel">
        <div class="tabs">${tabs.map(([key, label, badge]) => raw(html`<button data-action="tab" data-tab="${key}" class="${tab === key ? "on" : ""}">${label}${raw(badge ? html`<span class="badge ${key}">${badge}</span>` : "")}</button>`))}</div>
        ${raw(body)}
      </div>`;
  }

  static log(lines) {
    if (!lines) return '<div class="log"><div class="none">Loading…</div></div>';
    if (!lines.length) return '<div class="log"><div class="none">Nothing has happened yet.</div></div>';
    return `<div class="log">${lines.map(l => html`<div class="line ${l.level}"><span class="time">${Format.stamp(l.time)}</span><span class="text">${l.text}</span></div>`).join("")}</div>`;
  }

  static facts(job, trashDays) {
    const fact = (k, v, wide = false) => html`<div class="fact ${wide ? "wide" : ""}"><div class="k">${k}</div><div class="v">${raw(v)}</div></div>`;
    const esc = Dom.escape;
    const folder = (path, volume) => `<code>${esc(path)}</code>${volume ? ` <span class="faint">on ${esc(Format.volume(volume))}</span>` : ""}`;
    const kept = trashDays > 0 ? `kept ${Format.plural(trashDays, "day")}` : "kept until emptied";
    return `<div class="facts">
      ${fact("Mode", esc(Format.modeName(job.mode)))}
      ${fact("When", esc(Format.trigger(job.trigger, job.runsByItself)))}
      ${fact("Removed files", job.safeDelete ? `To <code>.charon-trash</code>, ${esc(kept)}` : '<span class="bad">Gone at once</span>')}
      ${fact("Conflicts", esc(job.mode === "two-way" ? Format.ruleName(job.conflict) : "—"))}
      ${fact("Source", folder(job.source, job.sourceVolume), true)}
      ${fact("Destination", folder(job.destination, job.destinationVolume), true)}
      ${fact("Excluded", job.excludes.length ? job.excludes.map(e => `<code>${esc(e)}</code>`).join(", ") : "Nothing", true)}
      ${fact("Full automation", job.fullAutomation ? `<span class="bad">On</span>${job.needsReview ? esc(", but the next sync waits for your OK") : ""}` : "Off: every sync waits for your OK")}
      ${fact("Next", esc(job.nextRun ? `${job.runsByItself ? "Sync" : "Look"} ${Format.upcoming(job.nextRun)}` : job.paused ? "Paused" : "When triggered"))}
      ${fact("Last run", esc(job.lastRun ? Format.ago(job.lastRun.finishedAt) : "Never"))}
    </div>`;
  }

  /** Each conflict with its two versions; a choice opens the review of what it would do. */
  static conflicts(job) {
    const version = (label, kind, modified, size) => {
      if (kind == null) return `${label}: removed`;
      if (kind === "dir") return `${label}: folder`;
      const when = modified == null ? "" : `, ${Format.ago(new Date(modified).toISOString())}`;
      if (kind === "link") return `${label}: link${when}`;
      return `${label}: ${size == null ? "file" : Format.bytes(size)}${when}`;
    };
    return `<div class="conflict-list">
      <p class="tab-hint">Choose which version to keep. Charon shows you exactly what that does first; nothing changes until you confirm it.</p>
      ${job.conflicts.map(c => html`
      <div class="conflict">
        <div>
          <div class="path">${c.path}</div>
          <div class="versions">${version("Source", c.sourceKind, c.sourceModified, c.sourceSize)} · ${version("Destination", c.destinationKind, c.destinationModified, c.destinationSize)}</div>
          ${raw(c.whole ? html`<div class="versions">This is ${c.whole}: its files only work together, so one side's copy is kept as a whole, and what it replaces goes to .charon-trash.</div>` : "")}
        </div>
        <div class="choices">
          <button class="btn" data-action="resolve" data-key="${c.key}" data-choice="keep-source">Keep source</button>
          <button class="btn" data-action="resolve" data-key="${c.key}" data-choice="keep-destination">Keep destination</button>
          ${raw(!c.whole && c.sourceKind === "file" && c.destinationKind === "file" ? html`<button class="btn" data-action="resolve" data-key="${c.key}" data-choice="keep-both">Keep both</button>` : "")}
        </div>
      </div>`).join("")}</div>`;
  }

  /** Places the last look couldn't read. Nothing in them is synced until they can be. */
  static unreadable(job) {
    return `<div class="place-list">
      <p class="tab-hint">Nothing in these places is synced until Charon can read them. This isn't an error: everything else crosses as usual.</p>
      ${job.unreadable.map((place, i) => html`
      <div class="place">
        <div>
          <div class="path">${place.path}</div>
          <div class="versions">On the ${place.side} · ${place.reason}</div>
          ${raw(place.privacy ? html`<div class="privacy"><span class="ico">${raw(Icons.lock)}</span>${PRIVACY_TEXT}</div>` : "")}
        </div>
        <div class="choices">
          ${raw(place.privacy ? '<button class="btn" data-action="privacy">Open Privacy settings</button>' : "")}
          <button class="btn" data-action="leave-out" data-index="${i}">Leave this folder out</button>
        </div>
      </div>`).join("")}</div>`;
  }
}
