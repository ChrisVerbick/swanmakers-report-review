# DesignCraft Review

The review portal for reports made with DesignCraft (report-craft): a share link opens the
documents under review; the client's team reads them in the browser, highlights and strikes
text, suggests replacements and pins comments; the comments flow back into the layout app as
notes and tracked changes. One Rust binary, one SQLite file, one folder of review bundles.

Status: the **service** (`server`) and the **reviewer's client** (`web`) are done; the app's
round trip (`review.publish` / `review.sync`) comes next. The bundle the app uploads is
described in the app's `docs/review-bundle.md`.

## Run it locally

```sh
ADMIN_TOKEN=$(openssl rand -base64 32) PUBLIC_ORIGIN=http://localhost:8080 cargo run -p server
```

Without `SMTP_URL`/`MAIL_FROM`, mail is written to the log instead of sent, magic links
included. Data goes to `./data` (`DATA_DIR`). `cargo test` walks the whole surface below.

## The client

`web` is a Leptos app (Rust compiled to WebAssembly, no JavaScript of its own) that the service
serves for every path that isn't the API. `/r/{token}` is the share page: the documents and
their versions, signing in by mail, joining a public link. `/r/{token}/d/{document}?v=…` is the
viewer: the pages as images with an invisible, selectable text layer on top (so selection,
find and screen readers work natively), a toolbar on the selection (Highlight · Comment ·
Strike · Replace), a pin where the page is clicked, markers for every comment, and a panel with
threads, resolve and filters. Other reviewers' comments arrive live (server-sent events), and a
comment written while offline is kept in the browser and sent when the connection is back.

```sh
cargo install trunk --locked          # once
cd web && trunk build --release       # → web/dist (about 250 KB of WebAssembly, gzipped)
WEB_DIR=web/dist … cargo run -p server
```

The service hashes the page's inline loader script into its Content-Security-Policy at
startup, so restart it after every `trunk build`. For development, `trunk serve` in `web/`
proxies `/api` and `/auth` to a service on port 8080 (`web/Trunk.toml`).

## The API

Owner side (the app), `Authorization: Bearer $ADMIN_TOKEN`:

| Call | Does |
|---|---|
| `POST /api/projects {name, client?}` · `GET /api/projects` · `GET /api/projects/{id}` · `PATCH /api/projects/{id} {name?, client?, allowDownload?}` | projects; the detail has documents, versions, members, links and the last 200 audit events |
| `POST /api/projects/{id}/members {email, name?, role: owner\|reviewer\|viewer}` · `DELETE …/members/{member}` | who may log in through invite links |
| `POST /api/projects/{id}/share-links {mode: invite\|public, password?, expiresDays?}` → `{token, url}` · `DELETE /api/share-links/{id}` | share links; the token is stored hashed, so copy the URL when it is returned |
| `POST /api/projects/{id}/versions` (body: the review bundle as a zip) | a new version of the document named in the bundle's manifest; a document belongs to the project it was first uploaded to (409 names it) |
| `GET /api/projects/{id}/comments.json` | every comment with its anchors, for `review.sync` |
| `PATCH /api/comments/{id} {status: open\|resolved\|applied\|orphaned}` | what the app did with a comment |

Reviewer side, under the share link's token:

| Call | Does |
|---|---|
| `POST /api/auth/request {email, link}` | mails a magic link to a member (strangers get the same 200 and no mail); 5 per address and 20 per IP per 15 minutes |
| `GET /auth?token=…&to=/r/…` → `POST /api/auth/consume` | the mail's landing page: a form posts the token (so a mail scanner can't use it up); 15-minute, single-use; sets the `__Host-sid` cookie |
| `POST /api/auth/guest {link, name, email, password?}` | join a public link as a guest |
| `POST /api/auth/logout` | |
| `GET /api/r/{token}` | the share page: project, mode, `me`, documents and their versions; 401 `{error: "login"}` when a login is needed |
| `GET /api/r/{token}/v/{version}/manifest.json \| text.json \| pages/NNN.webp \| document.pdf` | the bundle (the PDF only while the project allows downloads) |
| `GET /api/r/{token}/comments?document=&version=` · `POST /api/r/{token}/comments` · `PATCH …/comments/{id}` · `POST …/comments/{id}/replies` | comments: `kind` pin \| highlight \| strike \| replace \| note, with `page`, and for text kinds `story`, `cell?`/`footnote?`, `start`, `end`, `quote {exact, prefix, suffix}`, `rects`; `replacement` for replace; authors edit their own; members (not guests) resolve |
| `GET /api/r/{token}/events?since=` (SSE, `Last-Event-ID` honoured) | the project's audit events, backlog then live |

Roles: `owner`, `reviewer`, `viewer` (reads only), `guest` (public links; can't resolve).

## Deploy on a Hetzner VPS

Everything stays in the EU: Hetzner Falkenstein for the server and the object storage, Scaleway
Transactional Email (Paris) or SES eu-central-1 for mail.

1. **Server**: Hetzner Cloud CX23 (2 vCPU, 4 GB) in Falkenstein or Nuremberg, Debian 12, your SSH
   key. Add a Cloud Firewall: 22 from your own IP only, 80 and 443 from anywhere.
2. **Base hardening**: `apt install unattended-upgrades docker.io docker-compose-plugin crowdsec
   crowdsec-firewall-bouncer-nftables`; `PasswordAuthentication no` in `sshd_config`; a non-root
   user in the `docker` group.
3. **DNS**: an A (and AAAA) record for `review.example.com` to the server.
4. **Object storage**: a Hetzner Object Storage bucket in the same location, with its own access
   key; it holds the Litestream replica and the bundle backups.
5. **Mail**: a sending domain at Scaleway TEM or SES with SPF, DKIM and DMARC records, and SMTP
   credentials.
6. **Configure**: clone this repo on the server, `cp deploy/.env.example deploy/.env`, fill it in
   (`ADMIN_TOKEN` from `openssl rand -base64 32`).
7. **Start**: `cd deploy && docker compose up -d --build`. Caddy fetches the certificate; the
   service listens only inside the Compose network; Litestream starts replicating.
8. **Check**: `curl https://review.example.com/` says "DesignCraft Review"; `docker compose logs
   review` shows the listening line; create a project with the admin token and open a share link.
9. **Bundles backup**: the `versions/` folder isn't in the SQLite file. Nightly, from cron:
   `docker run --rm -v deploy_data:/data rclone/rclone sync /data/versions :s3:$BUCKET/review/versions
   --s3-provider Other --s3-endpoint $ENDPOINT --s3-access-key-id … --s3-secret-access-key …`.

Updating: `git pull && docker compose up -d --build`. Watch the Caddy, Litestream and Debian
security notices; `unattended-upgrades` keeps the OS current.

## Restore drill (do it once a quarter; write the date here)

On a scratch VPS or your own machine with Docker:

1. `docker run --rm -v scratch:/data -e LITESTREAM_ACCESS_KEY_ID=… -e LITESTREAM_SECRET_ACCESS_KEY=…
   litestream/litestream:0.5 restore -o /data/db.sqlite s3://$BUCKET/review/db` (add
   `-endpoint`/`-region` flags, or point `-config` at `deploy/litestream.yml`).
2. `docker run --rm -v scratch:/data rclone/rclone sync :s3:$BUCKET/review/versions /data/versions …`.
3. Start the service against that volume with the production `.env` (a different `REVIEW_HOST`
   if DNS still points at production): `docker compose up -d`.
4. Open a share link from `GET /api/projects/{id}` (links are hashed, so make a new one) and
   check a document, its pages and its comments are all there.
5. Note the time it took and what was missing. The replica lags production by about a second;
   the bundles by up to a day (the cron above).

Last drill: not yet.

## Security notes

- Share, login and session tokens are 256 random bits and stored only as SHA-256 hashes; a
  copy of the database gives nobody a session. Public-link passwords are Argon2id hashes.
- Sessions: `__Host-sid`, HttpOnly, Secure, SameSite=Lax; 12 hours absolute, 60 minutes idle.
- Cookie-authenticated writes must come from `PUBLIC_ORIGIN` (Origin / Sec-Fetch-Site check).
  The owner API uses the bearer token and never a cookie.
- Headers on every response: CSP `default-src 'self'`, `frame-ancestors 'none'`, nosniff,
  `Referrer-Policy: no-referrer`; HSTS from Caddy.
- Rate limits on login requests, link consumption and guest joins (per IP and per address).
- Bundle uploads: zips are unpacked by an allow-list of file names (`manifest.json`, `text.json`,
  `document.pdf`, `pages/NNN.webp`), with entry and size caps; bodies are capped at 64 MB.
- The container runs as an unprivileged user with a read-only root file system; only `/data`
  and `/tmp` are writable.
- Every change is an audit event with actor, IP and time (`GET /api/projects/{id}` shows the
  last 200; the `event` table holds all of them).

## Data protection

Processors: Hetzner Online GmbH (hosting and object storage, DE), the mail provider (Scaleway
SAS, FR, or AWS eu-central-1), and nobody else when the Cloudflare proxy isn't used. Sign the
data-processing agreements in each account. Personal data held: members' names, e-mail addresses
and roles; comments and replies; audit events with IP addresses. Retention and deletion of whole
projects is manual for now (`sqlite3` on the server); a retention job and a per-project erasure
route are planned.

## Licence

MIT or Apache-2.0, at your option.
