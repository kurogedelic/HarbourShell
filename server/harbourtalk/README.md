# HarbourTalk server

The board behind HarbourTalk (`src/launcher/apps/harbourtalk/`): a Cloudflare Worker with a D1
database. Public rooms, each one timeline of short posts, newest first. Every room is public:
there are no private messages.

```
GET  /rooms                  -> 200 {"rooms": [{"id", "name", "count", "last"}, ...]}
GET  /posts?room=R&limit=N   -> 200 {"posts": [{"room", "id", "name", "dev", "text", "time"}, ...]}  (newest first, N <= 100)
POST /posts                  body {"room", "name", "dev", "text"} -> 201 {"post": {...}}
```

The rooms are the owner's choice: `ROOMS` in `wrangler.toml`, `id=Name` comma separated (ids:
a-z, 0-9, `-`). Users cannot make rooms. Taking a room out of the list hides it and its posts.

Every request needs two headers:

- `X-Harbour-Key`: one of the keys in the `APP_KEYS` secret (comma separated). Harbour sends
  `harbourtalk-1` unless its settings name another key.
- `X-Harbour-Device`: the device id, 8 lower-case hex digits (made by the app on first use).

The key keeps casual visitors out; it is in Harbour's source, so it is not a lock. When it leaks,
put a new key in `APP_KEYS` (old and new side by side while devices update), then drop the old one.

Rules (`board.js`):

- a post is 1 to 200 characters, a name 1 to 16 (no `#`); control characters become spaces
- one post per device every 30 seconds, one per network address every 10 seconds
  (`429 {"error": "Wait N s to post again"}`, which the app counts down)
- the newest 1000 posts of each room are kept
- browsers are let in only from the pages in `ALLOWED_ORIGINS`; curl on the device sends no
  `Origin` and passes on the key alone
- network addresses are stored only as a salted hash (`SALT` secret), for the address gap

## Deploy

```sh
cd server/harbourtalk
wrangler login
wrangler d1 create harbourtalk                      # put the database_id it prints in wrangler.toml
wrangler d1 execute harbourtalk --remote --file=schema.sql
wrangler secret put APP_KEYS                        # e.g. harbourtalk-1
wrangler secret put SALT                            # any long random text
# wrangler.toml [vars] ALLOWED_ORIGINS: the web demo's address (comma separated)
wrangler deploy                                     # prints https://harbourtalk.<account>.workers.dev
```

The standard board is deployed at https://harbourtalk.kurogedelic.workers.dev (`DEFAULT_SERVER` in
`src/launcher/apps/harbourtalk/net.rs`). Another board: HarbourTalk > menu > Server... (or `server`
in `~/.config/harbour/harbourtalk.json`); `demo` there gives a board on the device only.

## Try it on this computer

```sh
node --test board.test.js           # the rules
node dev.js 8787                    # the Worker on http://localhost:8787, Node's SQLite in place of D1
```

and set the server to `http://localhost:8787` in HarbourTalk. `dev.js` keeps the posts in memory
(`DEV_DB=file.sqlite` keeps them in a file). `APP_KEYS` and `ALLOWED_ORIGINS` come from the
environment.
