// Run the Worker on this computer without Cloudflare: node dev.js [port]
// D1 is stood in for by Node's built-in SQLite (in memory, or DEV_DB=file.sqlite), the secrets come
// from the environment (APP_KEYS, default "harbourtalk-1"; ROOMS, as in wrangler.toml). For trying the app against a real server.
import http from "node:http";
import { readFileSync } from "node:fs";
import { DatabaseSync } from "node:sqlite";
import worker from "./worker.js";

const db = new DatabaseSync(process.env.DEV_DB || ":memory:");
db.exec(readFileSync(new URL("./schema.sql", import.meta.url), "utf8"));

const D1 = {
  prepare(sql) {
    let args = [];
    const st = db.prepare(sql);
    const q = {
      bind: (...a) => ((args = a), q),
      all: async () => ({ results: st.all(...args) }),
      first: async () => st.get(...args) ?? null,
      run: async () => st.run(...args),
    };
    return q;
  },
};

const env = {
  DB: D1,
  APP_KEYS: process.env.APP_KEYS || "harbourtalk-1",
  ALLOWED_ORIGINS: process.env.ALLOWED_ORIGINS || "http://localhost:8017",
  ROOMS: process.env.ROOMS || "lobby=ロビー,cardputer=Cardputer,music=音楽,chat=雑談",
  SALT: "dev",
};

const port = Number(process.argv[2] || 8787);
http
  .createServer(async (req, res) => {
    const chunks = [];
    for await (const c of req) chunks.push(c);
    const body = chunks.length ? Buffer.concat(chunks) : undefined;
    const request = new Request(`http://localhost:${port}${req.url}`, {
      method: req.method,
      headers: req.headers,
      body: ["GET", "HEAD"].includes(req.method) ? undefined : body,
    });
    const r = await worker.fetch(request, env);
    res.writeHead(r.status, Object.fromEntries(r.headers));
    res.end(Buffer.from(await r.arrayBuffer()));
    console.log(req.method, req.url, r.status);
  })
  .listen(port, () => console.log(`HarbourTalk dev server on http://localhost:${port}`));
