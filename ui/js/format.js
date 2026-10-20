// Turning data into the words and numbers the window shows.

const MODE_NAMES = { mirror: "Mirror", backup: "Backup", "two-way": "Two-way" };
const STATUS_NAMES = {
  "in-sync": "In sync",
  syncing: "Syncing",
  pending: "Pending",
  review: "Needs your OK",
  paused: "Paused",
  conflict: "Conflict",
  error: "Error",
  offline: "Offline",
  new: "Ready",
};
const RULE_NAMES = { "keep-both": "Keep both", "newest-wins": "Newest wins", ask: "Ask me" };

/** What each mode does, in one sentence. */
const MODE_SENTENCES = {
  mirror: "The destination becomes an exact copy: files removed from the source are removed there too.",
  backup: "Files removed from the source stay. Changed files replace the older copy (the older copy goes to .charon-trash).",
  "two-way": "Changes flow both ways. Deleting a file on either side deletes it on the other.",
};

/** What each conflict rule does, in one sentence. */
const RULE_SENTENCES = {
  "newest-wins": "The version saved last replaces the other. This relies on both computers' clocks; the replaced version goes to .charon-trash.",
  "keep-both": "Both versions are kept: the destination's gets a “(conflict …)” name.",
  ask: "Nothing happens to the file until you choose.",
};

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

  /** "14:00", "tomorrow 03:00", "Mon 03:00": when something comes due. */
  static when(iso) {
    const when = new Date(iso);
    const day = Format.dayOf(when);
    return day ? `${day} ${Format.time(when)}` : Format.time(when);
  }

  /** "at 14:00", "tomorrow at 03:00", "on Mon at 03:00" */
  static at(iso) {
    const when = new Date(iso);
    const day = Format.dayOf(when);
    if (!day) return `at ${Format.time(when)}`;
    return day === "tomorrow" ? `tomorrow at ${Format.time(when)}` : `on ${day} at ${Format.time(when)}`;
  }

  /** "" for today, "tomorrow", or the weekday ("Mon") or date further away. */
  static dayOf(date) {
    if (new Date().toDateString() === date.toDateString()) return "";
    if (new Date(Date.now() + 86400000).toDateString() === date.toDateString()) return "tomorrow";
    if (Math.abs(date - Date.now()) < 6 * 86400000) return date.toLocaleDateString("en-GB", { weekday: "short" });
    return date.toLocaleDateString("en-GB", { day: "numeric", month: "short" });
  }

  /** "16:20" */
  static time(date) {
    return date.toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" });
  }

  /** "03:00" from hour and minute. */
  static clock(hour, minute) {
    return `${String(hour).padStart(2, "0")}:${String(minute).padStart(2, "0")}`;
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

  /** "Backup SSD (exFAT)" */
  static volume(volume) {
    if (!volume) return "";
    return volume.kind ? `${volume.name} (${volume.kind})` : volume.name;
  }

  /** "source", "destination" or "both sides". */
  static side(side) {
    return side === "source" || side === "destination" ? side : "both sides";
  }

  static modeName(mode) { return MODE_NAMES[mode] ?? mode; }
  static statusName(status) { return STATUS_NAMES[status] ?? status; }
  static ruleName(rule) { return RULE_NAMES[rule] ?? rule; }
  static modeSentence(mode) { return MODE_SENTENCES[mode] ?? ""; }
  static ruleSentence(rule) { return RULE_SENTENCES[rule] ?? ""; }

  /** "→" for one-way jobs, "⇄" for two-way. */
  static arrow(mode) {
    return mode === "two-way" ? "⇄" : "→";
  }

  /** What happens to removed and replaced files, for the safe deletes switch. */
  static trashSentence(days) {
    const kept = days > 0 ? `for ${Format.plural(days, "day")}` : "until you empty it";
    return `Removed and replaced files go to a hidden .charon-trash folder on the same drive and are kept ${kept}.`;
  }

  /** "every hour", "every 30 minutes", "every 2 hours" */
  static every(minutes) {
    if (minutes % 60 === 0) {
      const hours = minutes / 60;
      return hours === 1 ? "every hour" : `every ${hours} hours`;
    }
    return minutes === 1 ? "every minute" : `every ${minutes} minutes`;
  }

  /**
   * When a trigger fires, as words to put after "Looks" or "Syncs": "every hour",
   * "daily at 03:00", "when the drive is connected", "a few seconds after changes".
   * Null for manual jobs. `past` puts it in the past tense ("when the drive was connected").
   */
  static triggerPhrase(trigger, { past = false } = {}) {
    switch (trigger?.kind) {
      case "on-change": {
        const s = trigger.settleSeconds;
        if (s <= 30) return "a few seconds after changes";
        if (s >= 120 && s % 60 === 0) return `${s / 60} minutes after changes`;
        return `${s} seconds after changes`;
      }
      case "interval": return Format.every(trigger.minutes);
      case "daily": return `daily at ${Format.clock(trigger.hour, trigger.minute)}`;
      case "drive-connected": return past ? "when the drive was connected" : "when the drive is connected";
      default: return null;
    }
  }

  /**
   * What a job does on its schedule: "Looks every hour and asks you", or with full
   * automation "Syncs every hour". Manual jobs: "Only when you press Review and sync".
   */
  static trigger(trigger, runsByItself = false) {
    const phrase = Format.triggerPhrase(trigger);
    if (!phrase) return runsByItself ? "Only when you press Sync now" : "Only when you press Review and sync";
    return runsByItself ? `Syncs ${phrase}` : `Looks ${phrase} and asks you`;
  }

  /** The row's schedule: "next sync 14:00" or "looks at 14:00 and asks you". */
  static schedule(job) {
    if (!job.nextRun) return Format.trigger(job.trigger, job.runsByItself);
    return job.runsByItself ? `next sync ${Format.when(job.nextRun)}` : `looks ${Format.at(job.nextRun)} and asks you`;
  }

  /** "23 changes wait for your OK · 3 removals", or why a full-automation run was held. */
  static waiting(waiting) {
    if (waiting.held) return `Held for your OK: ${waiting.held}`;
    const text = waiting.items === 1 ? "1 change waits for your OK" : `${Format.count(waiting.items)} changes wait for your OK`;
    const removals = (waiting.removals?.source || 0) + (waiting.removals?.destination || 0);
    return removals ? `${text} · ${Format.plural(removals, "removal")}` : text;
  }

  /**
   * What a review's tally adds up to, both sides together: files copied (new ones and
   * kept-both copies), replaced (including newest-wins settlements), removed.
   */
  static changeCounts(tally) {
    const sides = [tally.source, tally.destination];
    const sum = key => sides.reduce((n, side) => n + (side?.[key] || 0), 0);
    return {
      copy: sum("newFiles") + (tally.keepBoth || 0),
      replace: sum("replaced") + (tally.newestWins || 0),
      remove: sum("removed"),
      removeFolders: sum("removedFolders"),
    };
  }

  /** True when confirming would replace or remove something. */
  static isDestructive(tally) {
    const c = Format.changeCounts(tally);
    return c.replace + c.remove + c.removeFolders > 0;
  }

  /** "copy 23, replace 2, remove 3" ("" when there's nothing to copy, replace or remove). */
  static changeList(tally) {
    const c = Format.changeCounts(tally);
    const parts = [];
    if (c.copy) parts.push(`copy ${Format.count(c.copy)}`);
    if (c.replace) parts.push(`replace ${Format.count(c.replace)}`);
    if (c.remove) parts.push(`remove ${Format.count(c.remove)}`);
    else if (c.removeFolders) parts.push(`remove ${Format.plural(c.removeFolders, "folder")}`);
    return parts.join(", ");
  }
}
