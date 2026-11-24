// HarbourTalk board server: a Cloudflare Worker with a D1 database.
//
//   GET  /rooms                  -> {"rooms": [{id, name, count, last}, ...]}
//   GET  /posts?room=R&limit=N   -> {"posts": [{room, id, name, dev, text, time}, ...]} newest first
//   POST /posts                  body {"room", "name", "dev", "text"} -> 201 {"post": {...}}
//
// Every room is public (the ROOMS setting lists them); there are no private messages.
//
// Every request needs X-Harbour-Key (one of the APP_KEYS secret) and X-Harbour-Device (8 hex
// digits). Browsers are let in only from ALLOWED_ORIGINS. One post per device every 30 seconds
// and per address every 10; at most 200 characters.

import { KEEP, MIN_GAP_ADDR, cleanName, cleanText, isDevice, keyOk, limitOf, originOk, parseRooms, wait } from "./board.js";

const json = (status, body, cors) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json; charset=utf-8", "Cache-Control": "no-store", ...cors },
  });

function corsHeaders(origin) {
  if (!origin) return {};
  return {
    "Access-Control-Allow-Origin": origin,
    "Access-Control-Allow-Methods": "GET, POST, OPTIONS",
    "Access-Control-Allow-Headers": "Content-Type, X-Harbour-Key, X-Harbour-Device",
    "Access-Control-Max-Age": "86400",
    Vary: "Origin",
  };
}

/** The poster's address, hashed with a salt (the address itself is never stored). */
async function addressTag(request, salt) {
  const ip = request.headers.get("CF-Connecting-IP") || "local";
  const data = new TextEncoder().encode(`${salt || "harbourtalk"}:${ip}`);
  const hash = new Uint8Array(await crypto.subtle.digest("SHA-256", data));
  return Array.from(hash.slice(0, 12), (b) => b.toString(16).padStart(2, "0")).join("");
}

const shape = (r) => ({ room: r.room, id: String(r.id), name: r.name, dev: r.dev, text: r.text, time: r.time });

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const origin = request.headers.get("Origin");
    if (!originOk(origin, env.ALLOWED_ORIGINS)) return json(403, { error: "The server refused this app" }, {});
    const cors = corsHeaders(origin);
    if (request.method === "OPTIONS") return new Response(null, { status: 204, headers: cors });
    if (url.pathname !== "/posts" && url.pathname !== "/rooms") return json(404, { error: "Not found" }, cors);
    if (!keyOk(request.headers.get("X-Harbour-Key"), env.APP_KEYS)) {
      return json(401, { error: "The server refused this app" }, cors);
    }
    const device = request.headers.get("X-Harbour-Device") || "";
    if (!isDevice(device)) return json(400, { error: "No device id" }, cors);

    const rooms = parseRooms(env.ROOMS);
    const roomOk = (id) => rooms.some((r) => r.id === id);

    if (request.method === "GET" && url.pathname === "/rooms") {
      const { results } = await env.DB.prepare("SELECT room, COUNT(*) AS count, MAX(time) AS last FROM posts GROUP BY room").all();
      const stats = Object.fromEntries(results.map((r) => [r.room, r]));
      const list = rooms.map((r) => ({ ...r, count: stats[r.id]?.count ?? 0, last: stats[r.id]?.last ?? 0 }));
      return json(200, { rooms: list }, cors);
    }
    if (url.pathname !== "/posts") return json(405, { error: "Not allowed" }, cors);

    if (request.method === "GET") {
      const room = url.searchParams.get("room") || "";
      if (!roomOk(room)) return json(404, { error: "No such room" }, cors);
      const { results } = await env.DB.prepare("SELECT room, id, name, dev, text, time FROM posts WHERE room = ? ORDER BY id DESC LIMIT ?")
        .bind(room, limitOf(url.searchParams.get("limit")))
        .all();
      return json(200, { posts: results.map(shape) }, cors);
    }

    if (request.method === "POST") {
      const raw = await request.text();
      if (raw.length > 4096) return json(413, { error: "The post is too long" }, cors);
      let body;
      try {
        body = JSON.parse(raw);
      } catch {
        return json(400, { error: "Not a post" }, cors);
      }
      const name = cleanName(body.name);
      const text = cleanText(body.text);
      if (!name) return json(400, { error: "Type a name" }, cors);
      if (!text) return json(400, { error: "Write something first" }, cors);
      if (body.dev !== device) return json(400, { error: "No device id" }, cors);
      if (!roomOk(body.room)) return json(404, { error: "No such room" }, cors);

      const now = Math.floor(Date.now() / 1000);
      const tag = await addressTag(request, env.SALT);
      const byDev = await env.DB.prepare("SELECT MAX(time) AS t FROM posts WHERE dev = ?").bind(device).first();
      const byAddr = await env.DB.prepare("SELECT MAX(time) AS t FROM posts WHERE ip = ?").bind(tag).first();
      const w = Math.max(wait(byDev?.t ?? 0, now), wait(byAddr?.t ?? 0, now, MIN_GAP_ADDR));
      if (w > 0) return json(429, { error: `Wait ${w} s to post again` }, cors);

      const row = await env.DB.prepare("INSERT INTO posts (room, name, dev, text, time, ip) VALUES (?, ?, ?, ?, ?, ?) RETURNING room, id, name, dev, text, time")
        .bind(body.room, name, device, text, now, tag)
        .first();
      // Keep each room's newest KEEP posts.
      await env.DB.prepare("DELETE FROM posts WHERE room = ? AND id NOT IN (SELECT id FROM posts WHERE room = ? ORDER BY id DESC LIMIT ?)")
        .bind(body.room, body.room, KEEP)
        .run();
      return json(201, { post: shape(row) }, cors);
    }

    return json(405, { error: "Not allowed" }, cors);
  },
};
