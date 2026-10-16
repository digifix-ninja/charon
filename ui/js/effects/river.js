// The river behind the window: layered waves, drifting motes of light, and Charon's boat.
// The boat rests at the near shore and crosses as the jobs sync, so overall progress is
// visible at a glance even out of the corner of your eye. Colours come from the theme.

import { Theme } from "../theme.js";

export class River {
  constructor(canvas) {
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d");
    this.time = 0;
    this.progress = 0;       // 0 … 1, where the boat should be
    this.boatX = 0.12;       // where the boat is (eases towards progress)
    this.busy = false;
    this.pointer = { x: -1, y: -1 };
    this.motes = Array.from({ length: 46 }, () => this.newMote(true));
    this.still = matchMedia("(prefers-reduced-motion: reduce)").matches;
    this.readColours();
    document.addEventListener("themechange", () => this.readColours());
  }

  /** Picks up the theme's river, lantern, text and background colours ("r g b" triplets). */
  readColours() {
    const triplet = name => Theme.token(name).split(/\s+/).join(", ");
    this.colours = {
      river: triplet("--river-rgb"),
      lantern: triplet("--lantern-rgb"),
      line: triplet("--ink-rgb"),
      hull: triplet("--panel-rgb"),
      light: Theme.current() === "light",
    };
  }

  /** `rgba()` for one of the theme colours. */
  rgba(name, alpha) {
    return `rgba(${this.colours[name]}, ${alpha})`;
  }

  /** Starts drawing; pauses by itself when the window is hidden. */
  start() {
    const resize = () => {
      const ratio = window.devicePixelRatio || 1;
      this.width = window.innerWidth;
      this.height = window.innerHeight;
      this.canvas.width = this.width * ratio;
      this.canvas.height = this.height * ratio;
      this.ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
    };
    resize();
    window.addEventListener("resize", resize);
    window.addEventListener("pointermove", e => { this.pointer = { x: e.clientX, y: e.clientY }; });
    const frame = () => {
      if (!document.hidden) this.draw();
      requestAnimationFrame(frame);
    };
    requestAnimationFrame(frame);
  }

  /** `busy` speeds the river up; `progress` (0…1) moves the boat across. */
  setActivity(busy, progress) {
    this.busy = busy;
    this.progress = busy ? Math.max(0.04, Math.min(1, progress)) : 0;
  }

  draw() {
    const { ctx, width, height } = this;
    this.time += this.still ? 0 : this.busy ? 0.022 : 0.009;
    ctx.clearRect(0, 0, width, height);

    const base = height - 74;
    this.drawGlow(base);
    for (let layer = 0; layer < 3; layer++) this.drawWave(base, layer);
    this.drawMotes(base);
    this.drawBoat(base);
  }

  /** A low, cold light rising from the water. */
  drawGlow(base) {
    const { ctx, width, height } = this;
    const glow = ctx.createLinearGradient(0, base - 160, 0, height);
    glow.addColorStop(0, this.rgba("river", 0));
    glow.addColorStop(1, this.rgba("river", this.busy ? 0.1 : 0.05));
    ctx.fillStyle = glow;
    ctx.fillRect(0, base - 160, width, height - base + 160);
  }

  /** The height of the water at x, for one layer. */
  waveY(x, base, layer) {
    const t = this.time * (1 + layer * 0.35);
    let y = base + layer * 16
      + Math.sin(x * 0.006 + t * 2 + layer) * (7 - layer * 1.5)
      + Math.sin(x * 0.017 - t * 3 + layer * 2) * 2.5;
    // The pointer stirs the water near it.
    if (this.pointer.y > base - 120) {
      const d = Math.abs(x - this.pointer.x);
      if (d < 140) y += Math.cos((d / 140) * Math.PI / 2) * Math.sin(t * 6 - d * 0.08) * 5;
    }
    return y;
  }

  drawWave(base, layer) {
    const { ctx, width, height } = this;
    ctx.beginPath();
    ctx.moveTo(0, height);
    for (let x = 0; x <= width + 8; x += 8) ctx.lineTo(x, this.waveY(x, base, layer));
    ctx.lineTo(width, height);
    ctx.closePath();
    const alpha = [0.07, 0.05, 0.035][layer] * (this.colours.light ? 1.4 : 1);
    ctx.fillStyle = this.rgba("river", alpha);
    ctx.fill();

    // A thin bright line on the top layer
    if (layer === 0) {
      ctx.beginPath();
      for (let x = 0; x <= width + 8; x += 8) {
        const y = this.waveY(x, base, 0);
        if (x === 0) ctx.moveTo(x, y);
        else ctx.lineTo(x, y);
      }
      ctx.strokeStyle = this.rgba("river", this.busy ? 0.45 : 0.22);
      ctx.lineWidth = 1;
      ctx.stroke();
    }
  }

  newMote(anywhere) {
    return {
      x: anywhere ? Math.random() : -0.02,
      y: Math.random(),
      speed: 0.0004 + Math.random() * 0.0012,
      size: 0.6 + Math.random() * 1.6,
      phase: Math.random() * Math.PI * 2,
    };
  }

  /** Motes drifting downstream, like files being carried across. */
  drawMotes(base) {
    const { ctx, width } = this;
    for (const mote of this.motes) {
      mote.x += this.still ? 0 : mote.speed * (this.busy ? 3.2 : 1);
      if (mote.x > 1.02) Object.assign(mote, this.newMote(false));
      const x = mote.x * width;
      const y = base - 10 + mote.y * 70 + Math.sin(this.time * 3 + mote.phase) * 4;
      const flicker = 0.35 + Math.sin(this.time * 4 + mote.phase) * 0.25;
      ctx.beginPath();
      ctx.arc(x, y, mote.size, 0, Math.PI * 2);
      ctx.fillStyle = this.rgba("river", flicker * (this.busy ? 0.9 : 0.5));
      ctx.fill();
    }
  }

  /** The boat, drawn in the logo's spirit: a hull, a folder, a lantern on a pole. */
  drawBoat(base) {
    const { ctx, width } = this;
    const target = 0.1 + this.progress * 0.78;
    this.boatX += (target - this.boatX) * 0.03;
    const x = this.boatX * width;
    const y = this.waveY(x, base, 0) - 2;
    const tilt = (this.waveY(x + 12, base, 0) - this.waveY(x - 12, base, 0)) / 24;

    ctx.save();
    ctx.translate(x, y);
    ctx.rotate(Math.atan(tilt) * 0.8);

    // Lantern light on the water
    const lightX = -26;
    const glow = ctx.createRadialGradient(lightX, -22, 0, lightX, -22, 60);
    const strength = this.busy ? 0.5 : 0.32;
    glow.addColorStop(0, this.rgba("lantern", strength));
    glow.addColorStop(1, this.rgba("lantern", 0));
    ctx.fillStyle = glow;
    ctx.fillRect(lightX - 60, -82, 120, 120);

    ctx.strokeStyle = this.rgba("line", 0.75);
    ctx.fillStyle = this.rgba("hull", 0.9);
    ctx.lineWidth = 1.6;
    ctx.lineJoin = "round";
    ctx.lineCap = "round";

    // Folder
    ctx.beginPath();
    ctx.moveTo(-10, -6); ctx.lineTo(-10, -26); ctx.lineTo(-1, -26); ctx.lineTo(2, -22); ctx.lineTo(14, -22); ctx.lineTo(14, -6);
    ctx.fill(); ctx.stroke();

    // Hull
    ctx.beginPath();
    ctx.moveTo(-28, -7);
    ctx.quadraticCurveTo(4, -2, 30, -13);
    ctx.quadraticCurveTo(26, 5, 14, 7);
    ctx.lineTo(-20, 7);
    ctx.quadraticCurveTo(-26, 2, -28, -7);
    ctx.fill(); ctx.stroke();

    // Pole and lantern
    ctx.beginPath();
    ctx.moveTo(-20, -6); ctx.lineTo(-20, -34); ctx.quadraticCurveTo(-24, -38, -26, -33);
    ctx.stroke();
    ctx.fillStyle = this.rgba("lantern", 0.75 + Math.sin(this.time * 5) * 0.15);
    ctx.beginPath();
    ctx.roundRect(-29, -31, 6, 8, 1.5);
    ctx.fill();
    ctx.restore();
  }
}
