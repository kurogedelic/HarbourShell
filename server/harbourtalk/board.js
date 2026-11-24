// HarbourTalk board rules, kept apart from the Worker so they can be tested with `node --test`.

export const MAX_TEXT = 200;
export const MAX_NAME = 16;
export const MIN_GAP = 30; // seconds between two posts from one device
export const MIN_GAP_ADDR = 10; // ... from one address (devices on one home network share it)
export const KEEP = 1000; // posts kept on the server
export const MAX_LIMIT = 100; // posts in one answer

const chars = (s) => Array.from(s);

/** The text as stored: control characters as spaces, trimmed, at most MAX_TEXT characters. */
export function cleanText(s) {
  if (typeof s !== "string") return "";
  const t = chars(s).map((c) => (/[\u0000-\u001f\u007f-\u009f\u2028\u2029]/.test(c) ? " " : c)).join("").trim();
  return chars(t).slice(0, MAX_TEXT).join("");
}

/** The handle name as stored: no control characters or '#', trimmed, at most MAX_NAME. */
export function cleanName(s) {
  if (typeof s !== "string") return "";
  const t = chars(s).filter((c) => !/[\u0000-\u001f\u007f-\u009f#]/.test(c)).join("").trim();
  return chars(t).slice(0, MAX_NAME).join("");
}

/**
 * The public rooms from the ROOMS setting, `id=Name,id=Name` (ids: a-z, 0-9, '-', up to 24).
 * There are no private rooms or messages.
 */
export function parseRooms(spec) {
  const rooms = [];
  for (const part of (spec || "lobby=Lobby").split(",")) {
    const [id, ...rest] = part.split("=");
    const name = rest.join("=").trim();
    const key = (id || "").trim();
    if (/^[a-z0-9-]{1,24}$/.test(key) && name && !rooms.some((r) => r.id === key)) rooms.push({ id: key, name });
  }
  return rooms;
}

export const isDevice = (d) => typeof d === "string" && /^[0-9a-f]{8}$/.test(d);

/** Seconds still to wait before another post (0 = may post now). */
export function wait(lastTime, now, gap = MIN_GAP) {
  if (!lastTime) return 0;
  return Math.max(0, Math.min(gap, lastTime + gap - now));
}

/** How many posts to answer with, from `?limit=`. */
export function limitOf(param) {
  const n = parseInt(param ?? "", 10);
  return Number.isFinite(n) ? Math.max(1, Math.min(MAX_LIMIT, n)) : 50;
}

/** True when the key is one of the comma-separated `keys`. */
export function keyOk(key, keys) {
  if (!key || !keys) return false;
  return keys.split(",").map((k) => k.trim()).filter(Boolean).includes(key);
}

/**
 * CORS: browsers send Origin; only the listed ones are let in. Requests without Origin (curl on
 * the device) pass this check and still need the key.
 */
export function originOk(origin, allowed) {
  if (!origin) return true;
  return (allowed || "").split(",").map((o) => o.trim()).filter(Boolean).includes(origin);
}
