// Cards light up around the pointer: sets --mx / --my on the card under it.

export class PointerGlow {
  /** Follows the pointer inside `container` for children matching `selector`. */
  static follow(container, selector) {
    container.addEventListener("pointermove", event => {
      const card = event.target.closest(selector);
      if (!card) return;
      const box = card.getBoundingClientRect();
      card.style.setProperty("--mx", `${event.clientX - box.left}px`);
      card.style.setProperty("--my", `${event.clientY - box.top}px`);
    });
  }
}
