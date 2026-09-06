# Browser netplay rooms

This Cloudflare Worker exchanges WebRTC connection descriptions between two
players. Each SQLite Durable Object holds one invitation for at most 15 minutes.
Game inputs use WebRTC directly or through TURN; ROMs, disks and snapshots never
pass through this service. The static site remains on GitHub Pages.

## Deploy

Use Node 24 or later. Install the pinned tools with `npm ci`, authenticate with
`npx wrangler login`, and review the allowed site origins in `wrangler.jsonc`.
Create a TURN key in Cloudflare Realtime and store its values as Worker secrets:

```sh
npx wrangler secret put TURN_KEY_ID
npx wrangler secret put TURN_KEY_API_TOKEN
npm run deploy
```

The first deployment creates the SQLite Durable Object namespace. Put the
deployed HTTPS URL in the site shell:

```html
<meta name="copperline-netplay-service" content="https://YOUR-WORKER.workers.dev">
```

The URL is public configuration. Never put the TURN key or its API token in the
site, source, logs or GitHub variables exposed to browsers. Temporary credentials
are issued for each player and last 24 hours. They are not refreshed; longer
games need a new session. Keep `REQUIRE_TURN` enabled in production: if TURN
credential issuance fails, room creation reports an error rather than presenting
a connection with no relay fallback.
The service removes Cloudflare's alternate port 53 URLs, which browsers can
block and leave address discovery waiting until it times out. Other relay
ports, including TLS on port 443, remain available.
Browsers that reject TURN transport queries retry with default UDP and TLS URLs;
plain TCP entries are omitted only for those browsers.

The WASM publishing workflow copies all netplay modules and the QR license to
the site. It leaves the site's HTML, service URL and service worker intact.
Worker changes are deployed separately with `npm run deploy`.

## Limits and operations

Invitations carry 128 random bits and admit one guest. Separate owner and guest
tokens authorize changes; knowing the room ID alone does not grant host access.
The first guest reservation wins, so share links only with the intended player.
Setup expires after 15 minutes and DELETE removes it early. An established game
continues independently of the signaling record. Closing a page attempts cleanup;
expiry handles interrupted requests or suspended devices.

Bodies are limited to 100 KiB and connection codes to 96 KiB. The service permits
6 room creations and 120 total room requests per minute per client IP, using
Cloudflare's per-location rate limiter. The browser polls every 1.5 seconds only
while waiting for an answer. Shared networks share these quotas. CORS restricts
browser origins; it does not authenticate non-browser clients. Avoid treating
the quota as a global spending cap.

Application logging is disabled and exceptions do not include provider messages
or credentials. The Worker provider still handles request metadata. Check
`GET /health` with an allowed `Origin` header for the service version and whether
TURN secrets are configured; this does not test credential validity. A room
creation and a relay-only browser session verify the actual relay path.

Cloudflare's [Realtime pricing](https://developers.cloudflare.com/realtime/sfu/pricing/)
has a shared SFU/TURN free allowance, followed by usage charges. Review current
account limits and billing before enabling paid usage. The Worker and Durable
Objects have their own quotas. There is no payment or plan upgrade in the deploy
script.

## Local checks

```sh
npm ci
npm test
npx wrangler deploy --dry-run
npm run dev -- --env local
```

Local mode allows `http://127.0.0.1:8765`, disables TURN and makes no credential
requests. Serve the site there, then run `node tools/check-web-netplay-rooms.mjs`
from the repository root. The tool supplies the local service URL without editing
the deployable page. Its `NETPLAY_SERVICE` and `NETPLAY_RELAY_ONLY=1` options test
an explicitly selected deployed service and require a selected relay candidate.

The Node tests run the Worker and Durable Objects in Miniflare, checking role
boundaries, guest reservation, cleanup, request limits and TURN failure handling.
An independent decoder checks the vendored QR encoder. Browser helper tests live
in `crates/copperline-web/www`; run `npm test` there too.
