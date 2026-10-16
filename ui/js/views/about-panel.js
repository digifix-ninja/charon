// About Charon: version, links, developer and licence.

import { Api } from "../api.js";
import { Dom } from "../dom.js";
import { Logo } from "../logo.js";
import { Icons } from "../icons.js";
import { Sheet } from "./sheet.js";

const { html, raw } = Dom;

export class AboutPanel extends Sheet {
  constructor(toast) {
    super(Dom.$("#about"));
    this.toast = toast;
    this.element.addEventListener("click", event => {
      const target = event.target.closest("[data-do], [data-url]");
      if (!target) return;
      if (target.dataset.do === "close") this.close();
      if (target.dataset.url) Api.openUrl(target.dataset.url).catch(error => this.toast.error(error));
    });
  }

  async show() {
    const about = await Api.about().catch(() => null);
    if (!about) return;
    this.element.innerHTML = html`
      <header>
        <span class="eyebrow"><span class="num">03</span> About</span>
        <button class="icon-btn" data-do="close" title="Close"><span class="ico">${raw(Icons.close)}</span></button>
      </header>
      <div class="scroll"><div class="about">
        ${raw(Logo.html("The Charon logo: a boat with a lantern carrying a folder"))}
        <h2>CHARON</h2>
        <span class="version">Version ${about.version}</span>
        <p>The ferryman for your files. Charon carries them between folders, one way or both ways,
          and never loses one on the crossing.</p>
        <div class="links">
          <button class="btn" data-url="${about.appWebsite}">charonsync.app</button>
          ${raw(about.sourceCode ? html`<button class="btn" data-url="${about.sourceCode}">Source code</button>` : "")}
          <button class="btn ghost" data-url="${about.website}">${about.developer}</button>
        </div>
        <p class="legal">© ${about.copyrightYear} ${about.developer} · ${about.licence} licence</p>
      </div></div>`;
    this.open();
  }
}
