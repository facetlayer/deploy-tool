# install/

Assets for putting a `deploy-server` instance on a host. The procedure is in
[../docs/server-setup.md](../docs/server-setup.md); these are the files it
refers to.

| File | Install as | Notes |
|---|---|---|
| `build-release.sh` | — | Cross-compiles for `x86_64-unknown-linux-gnu.2.39` with `cargo zigbuild`. Ubuntu 24.04, no Rust toolchain on the droplets. |
| `deploy-server.service` | `/etc/systemd/system/deploy-server.service` | `Type=simple`, `Restart=always`, port 4715, secrets via `EnvironmentFile=`. Keep the copy in this repo as the source of truth and re-install after editing. |
| `deploy.env.template` | `/root/secrets/deploy.env` | Root-owned, `0600`, created by hand over SSH. Carries the three required auth-center variables, including the secret `DEPLOY_AUTH_KEY`; never commit a filled-in copy. |

```bash
install/build-release.sh
scp target/x86_64-unknown-linux-gnu/release/deploy-server root@host:/root/bin/deploy-server
scp install/deploy-server.service root@host:/etc/systemd/system/deploy-server.service
ssh root@host 'install -m 600 /dev/null /root/secrets/deploy.env && vi /root/secrets/deploy.env'
ssh root@host 'systemd-analyze verify /etc/systemd/system/deploy-server.service && \
  systemctl daemon-reload && systemctl enable --now deploy-server'
```

`DEPLOY_AUTH_URL`, `DEPLOY_AUTH_KEY` and `DEPLOY_ADMIN_RESOURCE` are all
required: the server refuses to start with any of them missing, so fill in the
env file before enabling the unit.

The existing hosts run the old server as unit `deploy` on the same port and
against the same database (`/root/.local/state/deploy/db.sqlite`) — stop and
disable that unit before starting this one. This server does not read the old
schema; cutting an instance over means rebuilding or importing the database,
per "Cutover" in [../docs/server-setup.md](../docs/server-setup.md).
