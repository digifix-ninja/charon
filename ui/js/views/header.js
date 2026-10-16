// The title bar's status line, the "Pause all" button and the overview tiles.

import { Dom } from "../dom.js";
import { GROUPS } from "../store.js";
import { Counter } from "../effects/counter.js";

const TONES = {
  syncing: "var(--syncing)",
  attention: "var(--error)",
  paused: "var(--paused)",
  pending: "var(--pending)",
  calm: "var(--in-sync)",
  empty: "var(--text-faint)",
};

export class Header {
  constructor(store, { pickGroup }) {
    this.store = store;
    this.headline = Dom.$("#headline");
    this.pauseButton = Dom.$("#btn-pause-all");
    this.stats = Dom.$("#stats");
    this.buildTiles();
    this.stats.addEventListener("click", event => {
      const tile = event.target.closest("[data-group]");
      if (tile) pickGroup(tile.dataset.group);
    });
  }

  buildTiles() {
    this.stats.innerHTML = GROUPS.map(g => Dom.html`
      <button class="stat" data-group="${g.key}" style="--tone: ${g.tone}">
        <span class="label"><i></i>${g.label}</span>
        <span class="value" data-value="0">00</span>
      </button>`).join("");
  }

  render() {
    const overview = this.store.overview;
    if (!overview) return;
    this.headline.querySelector(".text").textContent = this.describe(overview);
    this.headline.style.setProperty("--tone", TONES[this.mood(overview)]);

    const paused = overview.pausedAll;
    this.pauseButton.classList.toggle("on", paused);
    this.pauseButton.querySelector(".label").textContent = paused ? "Resume all" : "Pause all";
    this.pauseButton.querySelector(".ico").dataset.icon = paused ? "resume" : "pause";

    const counts = this.store.counts();
    for (const tile of Dom.$$(".stat", this.stats)) {
      tile.classList.toggle("on", tile.dataset.group === this.store.group);
      Counter.roll(tile.querySelector(".value"), counts[tile.dataset.group]);
    }
  }

  mood(o) {
    if (o.jobs === 0) return "empty";
    if (o.syncing) return "syncing";
    if (o.attention) return "attention";
    if (o.pausedAll) return "paused";
    if (o.pending) return "pending";
    return "calm";
  }

  describe(o) {
    if (o.jobs === 0) return "No crossings yet. Create your first sync job.";
    if (o.syncing) return `Ferrying files · ${o.syncing} ${o.syncing === 1 ? "job" : "jobs"} crossing`;
    if (o.attention) return `${o.attention} ${o.attention === 1 ? "job needs" : "jobs need"} your attention`;
    if (o.pausedAll) return "Everything is paused";
    if (o.pending) return `${o.pending} ${o.pending === 1 ? "job has" : "jobs have"} changes waiting`;
    return o.offline ? `All reachable jobs in sync · ${o.offline} offline` : "All in sync. The river is calm.";
  }
}
