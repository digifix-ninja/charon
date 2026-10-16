// The Charon logo, drawn without a background so it sits seamlessly on the window:
// white lines in dark mode, black lines in light mode. CSS shows the one that fits.

export class Logo {
  /** Markup for the logo; size it with CSS on `.logo`. */
  static html(label = "") {
    const name = label ? `role="img" aria-label="${label}"` : `aria-hidden="true"`;
    return `<span class="logo" ${name}>`
      + `<img class="for-dark" src="img/logo-dark.svg" alt="" draggable="false">`
      + `<img class="for-light" src="img/logo-light.svg" alt="" draggable="false">`
      + `</span>`;
  }
}
