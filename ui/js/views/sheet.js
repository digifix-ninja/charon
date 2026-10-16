// A panel that slides in from the right, over a dimmed window.

const scrim = () => document.querySelector("#scrim");

export class Sheet {
  static all = [];

  constructor(element) {
    this.element = element;
    Sheet.all.push(this);
  }

  get isOpen() {
    return this.element.classList.contains("open");
  }

  open() {
    for (const other of Sheet.all) if (other !== this) other.close();
    this.element.classList.add("open");
    this.element.setAttribute("aria-hidden", "false");
    scrim().classList.add("on");
  }

  close() {
    this.element.classList.remove("open");
    this.element.setAttribute("aria-hidden", "true");
    if (!Sheet.all.some(sheet => sheet.isOpen)) scrim().classList.remove("on");
  }

  static closeAll() {
    for (const sheet of Sheet.all) sheet.close();
  }

  static anyOpen() {
    return Sheet.all.some(sheet => sheet.isOpen);
  }
}
