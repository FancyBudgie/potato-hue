# potato-hue

A small, local watcher that pulses one Philips Hue light whenever Pot Potato is
snatched. It polls `https://potpotato.xyz/api/state`, remembers the holder
count locally, queues every missed snatch, and restores the light to the state
it had before the pulse batch.

The bridge is contacted only on your home network. The Hue application key is
the only secret; keep it in `.env`, which is ignored by git.

## Mac setup

1. Copy `.env.example` to `.env` and set `HUE_BRIDGE_IP` to the bridge's LAN
   address.
2. Press the physical button on the bridge, then immediately run:

   ```sh
   cargo run -- authorize
   ```

   Copy the printed `HUE_APP_KEY` into `.env`.
3. Find the selected light and set its id in `.env`:

   ```sh
   cargo run -- list-lights
   ```

4. Confirm the exact pulse behavior without waiting for a snatch:

   ```sh
   cargo run -- test-pulse
   ```

5. Start watching:

   ```sh
   cargo run -- watch
   ```

`watch` is also the default command, so `cargo run` works once the setup is
complete.

## Linux service deployment

The repository includes a `systemd` unit which starts on boot, restarts after a
failure, and keeps its pulse queue under `/var/lib/potato-hue`. It uses a
systemd-managed unprivileged identity, so it does not need a dedicated Linux
user.

Build a Linux release binary on the server (or copy a binary built for the
server's CPU architecture), then install the service files:

```sh
cargo build --release
sudo sh deploy/install-systemd.sh
sudoedit /etc/potato-hue/.env
```

Put `HUE_BRIDGE_IP` and `HUE_LIGHT_ID` in `/etc/potato-hue/.env`. Leave
`HUE_APP_KEY` empty for now. Press the physical Hue bridge button, then create
a server-specific key using the service configuration:

```sh
cd /etc/potato-hue
sudo /usr/local/bin/potato-hue authorize
```

Copy the printed `HUE_APP_KEY` into `/etc/potato-hue/.env`, then enable it:

```sh
sudo systemctl enable --now potato-hue
sudo journalctl -u potato-hue -f
```

The watcher state survives service restarts in
`/var/lib/potato-hue/watch-state.json`. Its `.env` is mode `600` and is never
stored in the repository.

## Pulse semantics

The watcher captures the chosen light's state once for each queued batch.
An initially-on light is first turned dark; an initially-off light stays off.
Each queued snatch turns the light on at `PULSE_BRI` for `PULSE_ON_MS`, turns
it off for `PULSE_GAP_MS`, then, after all pulses, restores the saved on/off,
brightness, and active colour-mode settings. A change from holder count 100 to
110 therefore produces ten pulses before the previous state is restored.

## Pulse configuration

All pulse settings are optional and belong in `.env`:

```dotenv
PULSE_ON_MS=450
PULSE_GAP_MS=180
PULSE_MAX_BRIGHTNESS=100 # percentage, from 1 to 100
PULSE_TEMPERATURE_K=2700 # warm-white pulse; only for colour-temperature lights
# PULSE_COLOR=#FF9A20   # RGB pulse; only for colour-capable lights
```

`PULSE_TEMPERATURE_K` and `PULSE_COLOR` cannot be used together. With neither
set, a pulse uses the light's existing colour. The saved original brightness,
colour/temperature, and on/off state are always restored at the end of a
batch.
