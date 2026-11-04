# Automatic updates on TrueNAS

[deploy/truenas-auto-update.sh](../deploy/truenas-auto-update.sh) pulls the
proxy's image every 15 minutes. If it is new, the script backs up `/data`,
redeploys the app through the TrueNAS middleware and waits until `/admin/`
answers. If that does not work within 2 minutes, it restores the backup,
starts the old image and marks the new one as faulty; only a newer image is
tried again. Then it pulls the image of the code benchmark (service
`code-bench`) and redeploys the app if that is new, without backup or
rollback, since that service has no data and the proxy runs without it.

On other systems, pull and restart by hand:
`docker compose pull && docker compose up -d`.

## Prerequisites

- The app is called `strata-proxy` and was installed from
  [deploy/truenas-app.yaml](../deploy/truenas-app.yaml), with
  `pull_policy: missing` (otherwise the rollback pulls the faulty image
  again).
- TrueNAS SCALE 24.10 or newer.

## Setting it up

1. Put the script in its own directory on the pool, not in the app's data
   directory. The log and the backups end up there too:
   ```sh
   sudo mkdir -p /mnt/POOL/apps/strata-proxy-update
   cd /mnt/POOL/apps/strata-proxy-update
   sudo curl -fsSL -o truenas-auto-update.sh \
     https://raw.githubusercontent.com/FaunKr/Strata-Home-Proxy/main/deploy/truenas-auto-update.sh
   sudo chmod 700 truenas-auto-update.sh
   sudo ./truenas-auto-update.sh   # once by hand: without a new image nothing happens
   ```
2. *System -> Advanced Settings -> Cron Jobs -> Add*:
   - Command: `/mnt/POOL/apps/strata-proxy-update/truenas-auto-update.sh`
   - Run As User: `root`
   - Schedule: Custom, `*/15 * * * *`
   - *Hide Standard Output* on, *Hide Standard Error* off
3. Optional: write a Home Assistant webhook URL into the file
   `auto-update.webhook` next to the script. On a failed update,
   `{"event":"nas_update_failed","data":{"app":"strata-proxy","from":...,"to":...,"rolled_back":true}}`
   goes there.

A different app name, a different `LISTEN_ADDR` or a different path: set them
as variables before the command in the cron job, for example
`STRATA_PROXY_APP_NAME=proxy STRATA_PROXY_HEALTH_URL=http://127.0.0.1:9000/admin/ /mnt/.../truenas-auto-update.sh`.
All variables are listed at the top of the script. It reads the data
directory from the container (the mount on `/data`).

## Files next to the script

- `auto-update.log`: every update and every error (rotated into
  `auto-update.log.1` at 200 KB).
- `backups/data-*.tar.gz`: the last 10 backups of `/data`, with the password
  hash and API keys, so readable only by root.
- `auto-update.bad-images`: images that did not come up. Delete a line so
  that the image is tried once more.
- `failed-data-*`: the data as the faulty image left it.

## When the rollback fails

If the log says `ROLLBACK FAILED`, roll back by hand:

1. Stop the app under *Apps*.
2. Replace the contents of the data directory with the latest backup:
   `sudo tar -C /mnt/POOL/apps/strata-proxy --numeric-owner -xzpf backups/data-....tar.gz`
3. `sudo docker tag ghcr.io/faunkr/strata-home-proxy:previous ghcr.io/faunkr/strata-home-proxy:latest`
4. Start the app.
