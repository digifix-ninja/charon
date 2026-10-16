// A short message at the bottom of the window.

export class Toast {
  constructor(element) {
    this.element = element;
    this.timer = null;
  }

  /** Shows a message for a few seconds. */
  show(text, { error = false } = {}) {
    this.element.textContent = text;
    this.element.classList.toggle("error", error);
    this.element.classList.add("on");
    clearTimeout(this.timer);
    this.timer = setTimeout(() => this.element.classList.remove("on"), error ? 6000 : 3200);
  }

  /** Shows an error from the Rust side (a sentence) or from JavaScript. */
  error(error) {
    this.show(typeof error === "string" ? error : error?.message ?? String(error), { error: true });
  }
}
