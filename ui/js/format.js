// Turning data into the words and numbers the window shows.

const MODE_NAMES = { mirror: "Mirror", backup: "Backup", "two-way": "Two-way" };
const STATUS_NAMES = {
  "in-sync": "In sync",
  syncing: "Syncing",
  pending: "Pending",
  paused: "Paused",
  conflict: "Conflict",
  error: "Error",
  offline: "Offline",
  new: "Ready",
};
const RULE_NAMES = { "keep-both": "Keep both", "newest-wins": "Newest wins", ask: "Ask me" };

export class Format {
  /** "8.2 MB" */
  static bytes(n) {
    if (!n) return "0 bytes";
    const units = ["bytes", "KB", "MB", "GB", "TB"];
    let unit = 0;
    let value = n;
    while (value >= 1000 && unit < units.length - 1) { value /= 1000; unit++; }
    return unit === 0 ? `${n} bytes` : `${value.toFixed(value < 10 ? 1 : 0)} ${units[unit]}`;
  }

  /** "1,240" */
  static count(n) {
    return Number(n || 0).toLocaleString("en-GB");
  }

  /** "3 files" */
  static plural(n, word) {
    return `${Format.count(n)} ${word}${n === 1 ? "" : "s"}`;
  }

  /** "just now", "4 min ago", "yesterday 18:20" */
  static ago(iso) {
    if (!iso) return "never";
    const then = new Date(iso);
    const seconds = Math.round((Date.now() - then) / 1000);
    if (seconds < 45) return "just now";
    const minutes = Math.round(seconds / 60);
    if (minutes < 60) return `${minutes} min ago`;
    const hours = Math.round(minutes / 60);
    if (hours < 24) return `${hours} h ago`;
    const days = Math.round(hours / 24);
    if (days === 1) return `yesterday ${Format.time(then)}`;
    if (days < 7) return `${days} days ago`;
    return then.toLocaleDateString("en-GB", { day: "numeric", month: "short", year: "numeric" });
  }

  /** "in 12 min", "today 03:15", "tomorrow 03:15" */
  static upcoming(iso) {
    if (!iso) return "";
    const when = new Date(iso);
    const minutes = Math.round((when - Date.now()) / 60000);
    if (minutes <= 0) return "now";
    if (minutes < 60) return `in ${minutes} min`;
    const today = new Date().toDateString() === when.toDateString();
    const tomorrow = new Date(Date.now() + 86400000).toDateString() === when.toDateString();
    if (today) return `today ${Format.time(when)}`;
    if (tomorrow) return `tomorrow ${Format.time(when)}`;
    return when.toLocaleString("en-GB", { weekday: "short", hour: "2-digit", minute: "2-digit" });
  }

  /** "16:20" */
  static time(date) {
    return date.toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" });
  }

  /** "16 Oct 16:20:01" */
  static stamp(iso) {
    const d = new Date(iso);
    return `${d.toLocaleDateString("en-GB", { day: "2-digit", month: "short" })} ${d.toLocaleTimeString("en-GB")}`;
  }

  /** "~/Pictures" instead of "/Users/andrei/Pictures". */
  static path(path) {
    if (!path) return "";
    return String(path).replace(/^\/Users\/[^/]+/, "~").replace(/^\/home\/[^/]+/, "~");
  }

  static modeName(mode) { return MODE_NAMES[mode] ?? mode; }
  static statusName(status) { return STATUS_NAMES[status] ?? status; }
  static ruleName(rule) { return RULE_NAMES[rule] ?? rule; }

  /** "→" for one-way jobs, "⇄" for two-way. */
  static arrow(mode) {
    return mode === "two-way" ? "⇄" : "→";
  }

  /** "Every 30 min", "Daily at 03:15", "When the drive is connected" … */
  static trigger(trigger) {
    switch (trigger?.kind) {
      case "on-change": return `On change (after ${trigger.settleSeconds}s)`;
      case "interval": return trigger.minutes % 60 === 0
        ? `Every ${trigger.minutes / 60 === 1 ? "hour" : `${trigger.minutes / 60} hours`}`
        : `Every ${trigger.minutes} min`;
      case "daily": return `Daily at ${String(trigger.hour).padStart(2, "0")}:${String(trigger.minute).padStart(2, "0")}`;
      case "drive-connected": return "When the drive is connected";
      default: return "Manual";
    }
  }
}
