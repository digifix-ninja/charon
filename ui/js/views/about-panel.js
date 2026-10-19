// About Charon: version and updates, the app's links, and who makes it.

import { Api } from "../api.js";
import { Dom } from "../dom.js";
import { Logo } from "../logo.js";
import { Icons } from "../icons.js";
import { Sheet } from "./sheet.js";

const { html, raw } = Dom;

/** "https://github.com/steanart/charon" → "github.com/steanart/charon" */
const bare = url => url.replace(/^[a-z]+:(\/\/)?/, "").replace(/\/$/, "");

export class AboutPanel extends Sheet {
  constructor(updates, toast) {
    super(Dom.$("#about"));
    this.updates = updates;
    this.toast = toast;
    this.about = null;
    updates.subscribe(() => this.renderUpdate());
    this.element.addEventListener("click", event => {
      const target = event.target.closest("[data-do], [data-url]");
      if (!target) return;
      const actions = {
        close: () => this.close(),
        check: () => this.updates.check(),
        download: () => this.updates.download(),
        notes: () => this.updates.releaseNotes(),
      };
      if (target.dataset.do) actions[target.dataset.do]?.();
      if (target.dataset.url) Api.openUrl(target.dataset.url).catch(error => this.toast.error(error));
    });
  }

  async show() {
    this.about = await Api.about().catch(() => null);
    if (!this.about) return;
    this.render();
    this.open();
  }

  render() {
    const a = this.about;
    const link = (icon, label, url, text = bare(url)) => html`
      <button class="link-row" data-url="${url}">
        <span class="ico">${raw(Icons[icon])}</span>
        <span class="k">${label}</span>
        <span class="v">${text}</span>
      </button>`;
    this.element.innerHTML = html`
      <header>
        <span class="eyebrow"><span class="num">03</span> About</span>
        <button class="icon-btn" data-do="close" title="Close"><span class="ico">${raw(Icons.close)}</span></button>
      </header>
      <div class="scroll"><div class="about">
        ${raw(Logo.html("The Charon logo: a boat with a lantern carrying a folder"))}
        <h2>CHARON</h2>
        <span class="version">Version ${a.version}</span>
        <div class="update-line"></div>
        <p>The ferryman for your files. Charon carries them between folders, one way or both ways,
          and never loses one on the crossing.</p>

        <section class="links">
          <span class="eyebrow">The app</span>
          ${raw(link("globe", "Website", a.appWebsite))}
          ${raw(link("code", "GitHub", a.sourceCode))}
        </section>

        <section class="links">
          <span class="eyebrow">Developer</span>
          ${raw(link("person", "Made by", a.developerWebsite, a.developer))}
          ${raw(link("globe", "Website", a.developerWebsite))}
          ${raw(link("mail", "E-mail", `mailto:${a.developerEmail}`, a.developerEmail))}
        </section>

        <p class="legal">© ${a.copyrightYear} ${a.developer} · ${a.licence} licence</p>
      </div></div>`;
    this.renderUpdate();
  }

  /** The line under the version: a check button, "checking", up to date, or a new version. */
  renderUpdate() {
    const line = this.element.querySelector(".update-line");
    if (!line) return;
    const { info, checking } = this.updates;
    if (checking) {
      line.innerHTML = html`<span class="note"><span class="pulse"></span>Asking GitHub…</span>`;
    } else if (info?.available) {
      line.innerHTML = html`
        <span class="note news">Charon ${info.latest} is out</span>
        <button class="btn primary" data-do="download"><span class="ico">${raw(Icons.download)}</span>Download</button>
        <button class="btn ghost" data-do="notes">What's new</button>`;
    } else {
      const status = info ? html`<span class="note">You have the newest version.</span>` : "";
      line.innerHTML = html`${raw(status)}<button class="btn ghost" data-do="check"><span class="ico">${raw(Icons.sync)}</span>Check for updates</button>`;
    }
  }
}
