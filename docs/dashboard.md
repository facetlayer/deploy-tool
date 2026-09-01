# Web dashboard

A read-only browser view of one deploy instance: which projects are registered,
what resource each is bound to, what is live, and who shipped it.

It is visibility and nothing else. There is no route that deploys, rolls back,
activates or runs SQL, and no path from a dashboard session to any of those — a
session is checked against `<admin-resource>:admin-read`, which grants none of
them. That matters because of a genuine circular dependency: this server
deploys auth-center, and the dashboard signs in through auth-center. When that
bites, the CLI over SSH still does everything the dashboard could have, so the
dashboard is never on the recovery path.

## How a sign-in works

1. The browser loads `https://deploy.example.com`. The SPA calls
   `/dashboard/api/me`, gets a 401, and shows a sign-in door.
2. `/oauth/login` redirects to auth-center's `/oauth/authorize` with PKCE and a
   single-use `state` recorded in `dashboard_login`.
3. The user signs in at auth-center (or is already signed in there) and is sent
   back to `/oauth/callback` with a code.
4. The server exchanges the code for an access token — server-side, so the
   browser never sees it — confirms the user holds `admin-read`, and files the
   token against a `deploy_session` cookie.
5. Every later request resolves that cookie to the token and runs the *same*
   `authz::authorize` decision an API key does.

Step 5 is the design point. The dashboard is not a second authorization path:
it is an `admin-read` call carrying a different kind of token. It inherits
fail-closed behavior and the 30-second revocation bound from
`auth_center.rs` rather than trusting a local session row until it expires.

The trade is that the access token sits in the server's SQLite file. That file
is root-owned and already gates every deployment on the machine, and the
session expires with the token, so it buys the property above at a cost the
instance was already paying.

## Serving

The built SPA is compiled into the `deploy-server` binary (`rust-embed`,
`crates/deploy-server/dashboard/dist`). One origin means the cookie, the OAuth
callback and the JSON API need no CORS and no second nginx upstream, and it
removes the bootstrap problem of the deploy tool having to deploy its own
dashboard before it can show you anything.

`install/build-release.sh` builds the frontend before the binary. Without node
installed it warns and builds without the bundle: the deploy API is unaffected
and the dashboard serves 404s.

## Setting one up

Register the SSO client in auth-center, which prints the secret once:

```bash
auth-setup create-sso-client "deploy dashboard" --project deploy \
  --redirect-uri https://deploy.example.com/oauth/callback \
  --post-logout-redirect-uri https://deploy.example.com/ \
  --required-scope do2-deploy:admin-read
```

`--required-scope` is auth-center's own gate: a user without it is refused at
the authorize step and never reaches this server. The `admin-read` check here
runs anyway, because a client's required scope is auth-center's policy and this
server does not get to assume it was applied.

Then set the three variables in `/root/secrets/deploy.env` — see
`install/deploy.env.template` — and restart the unit. All three or none; some
but not all is a typo and the server says so and exits.

Grant viewers the role:

```bash
auth-setup update-admin andy --add-role deploy/deploy-viewer
```

## nginx

One vhost, one upstream. The SPA, the OAuth routes and the JSON API are all the
same server.

```nginx
server {
    server_name deploy.example.com;
    location / {
        proxy_pass http://127.0.0.1:4715;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
    }
}
```

Note that `deploy-server serve` still binds `0.0.0.0`, inherited from the old
server. Putting a browser-facing hostname in front of it does not change that:
restrict direct access to 4715 with the host firewall.

## Development

```bash
# terminal 1 — a local server with authorization off
DEPLOY_STATE_DIR=/tmp/deploy-dev deploy-server serve --port 4715 \
  --disable-api-key-check

# terminal 2
cd crates/deploy-server/dashboard && pnpm install && pnpm run dev
```

Vite proxies `/dashboard/api` and `/oauth` to 4715. `--disable-api-key-check`
allows every call, so the dashboard renders without an auth-center at all —
which also means it exercises none of the authorization above. Point
`DEPLOY_AUTH_URL` at a real auth-center to test the sign-in itself.
