// Numbers that roll to their new value instead of jumping.

export class Counter {
  /** Animates `element`'s text from its current number to `value`. */
  static roll(element, value) {
    const from = Number(element.dataset.value ?? 0);
    element.dataset.value = value;
    element.classList.toggle("zero", value === 0);
    if (from === value) {
      element.textContent = String(value).padStart(2, "0");
      return;
    }
    const start = performance.now();
    const duration = 500;
    const step = now => {
      const t = Math.min(1, (now - start) / duration);
      const eased = 1 - Math.pow(1 - t, 3);
      element.textContent = String(Math.round(from + (value - from) * eased)).padStart(2, "0");
      if (t < 1) requestAnimationFrame(step);
    };
    requestAnimationFrame(step);
  }
}
