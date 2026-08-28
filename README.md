# potato-hue

`potato-hue` runs as a small Linux service and pulses a Philips Hue light when
Pot Potato is snatched. It watches Pot Potato's public state, queues every
snatch it observes, and restores the selected light to its exact previous
state after the pulse batch.

The Hue bridge is contacted only over your home network. The service starts on
boot, restarts if it fails, and saves its queue locally so a restart does not
lose observed snatches.

## Documentation website

The same setup guide is available as a lightweight static website in
[`site/`](site/). It is configured as a Cloudflare Worker static-assets site
in [`wrangler.jsonc`](wrangler.jsonc). In Cloudflare Workers Builds, use `main`
as the production branch, `exit 0` as the build command, and `npx wrangler
deploy` as the deploy command. Non-production builds can keep the default
`npx wrangler versions upload` preview command. Every push to `main` will then
deploy the updated guide. This follows Cloudflare's [Workers static-assets
configuration](https://developers.cloudflare.com/workers/configuration/sites/start-from-worker/).

## Install and start

You need a Linux machine that can reach both the internet and your Hue bridge,
plus `cargo`, `make`, and `sudo`. Run these commands on that Linux machine from
a copy of this repository.

1. Install the binary and service files. This also creates the protected
   configuration file at `/etc/potato-hue/.env`.

   ```sh
   make service-install
   ```

2. Set only your bridge's LAN address in the new config file. Leave the Hue
   key and light ID blank for now.

   ```sh
   sudoedit /etc/potato-hue/.env
   ```

   ```dotenv
   HUE_BRIDGE_IP=192.168.1.50
   HUE_APP_KEY=
   HUE_LIGHT_ID=
   ```

3. Press the physical button on the Hue bridge, then run the authorization
   command and copy the printed `HUE_APP_KEY` into `/etc/potato-hue/.env`.

   ```sh
   make service-authorize
   sudoedit /etc/potato-hue/.env
   ```

4. List the bridge lights, put the selected ID in `HUE_LIGHT_ID`, and make one
   safe test pulse. The test returns the light to the state it had before.

   ```sh
   make service-list-lights
   sudoedit /etc/potato-hue/.env
   make service-test-pulse
   ```

5. Enable the watcher now and after future boots, then view its log.

   ```sh
   sudo systemctl enable --now potato-hue
   make service-logs
   ```

That is it. The first status line records the current holder count as a
baseline; it will pulse after the next snatch. The default check interval is
60 seconds.

## Update or remove the service

After obtaining a newer copy of this repository on the Linux machine, rebuild
and restart the configured service with one command:

```sh
make service-update
```

To remove the service:

```sh
make service-remove
```

Removal stops and disables the service and removes its binary and unit file.
It deliberately keeps `/etc/potato-hue/.env` and its saved queue under
`/var/lib/potato-hue`, so reinstalling does not require another Hue bridge
authorization.

## Configure the pulse

Edit `/etc/potato-hue/.env`, then run `make service-restart` for settings to
take effect:

```dotenv
# Check Pot Potato every 60 seconds by default.
POTATO_POLL_SECONDS=60

# A pulse is on for 450 ms, then dark for 180 ms before a possible next pulse.
PULSE_ON_MS=450
PULSE_GAP_MS=180
PULSE_MAX_BRIGHTNESS=100 # 1–100 percent

# Leave both colour options unset to reuse the light's existing colour.
# For white/colour-temperature lights, use a Kelvin temperature:
PULSE_TEMPERATURE_K=2700

# For colour-capable lights, use this instead of PULSE_TEMPERATURE_K:
# PULSE_COLOR=#FF9A20
```

`PULSE_TEMPERATURE_K` and `PULSE_COLOR` are mutually exclusive. A colour
temperature must be between 2000K and 6535K. The saved original brightness,
colour/temperature, and on/off state are always restored at the end of a
batch.

## Hot-potato holder mode

Turn the selected light into a live hot-potato indicator for either one Chia
address or whoever currently holds the potato. It begins at the configured
starting style when a hold starts, then intensifies until the deadline. In
specific-address mode, it restores the exact previous light state when somebody
else snatches it. In follow-current-holder mode, it stays hot across snatches
and starts again at the new holder's deadline.

```dotenv
# Choose exactly one target:
# Only this address:
WATCHED_ADDRESS=xch1youraddress...

# Or follow whoever currently holds it:
# FOLLOW_CURRENT_HOLDER=true

HOLDER_START_BRIGHTNESS=10 # percentage
HOLDER_END_BRIGHTNESS=100  # percentage

# Used automatically by RGB-capable lights:
HOLDER_START_COLOR=#8B4513 # brown
HOLDER_END_COLOR=#FF0000   # red

# Used automatically by white/colour-temperature lights:
HOLDER_START_TEMPERATURE_K=2200 # warm white
HOLDER_END_TEMPERATURE_K=6500   # cool white
```

All style settings are optional. To enable holder mode, set exactly one of
`WATCHED_ADDRESS` or `FOLLOW_CURRENT_HOLDER=true`; the shown style values are
the defaults. The bridge reports the selected light's capabilities, and the
service automatically uses the brown-to-red RGB gradient for RGB lights or the
warm-to-cold temperature gradient for colour-temperature lights. For a
brightness-only light, it still runs the brightness gradient. `make
service-list-lights` labels each light as `RGB`, `white temperature`, or
`brightness only`.

## What a pulse means

The service captures the light's state once for each queued batch. If the
light is on, it first goes dark. If it is off, it stays off until the pulse.
Each snatch turns it on at the configured brightness and colour, then dark
again. After all queued pulses, it restores the saved state.

So if the holder count increases by 10 between checks, the light pulses 10
times, then returns to precisely the state it was in before the first pulse.

Holder mode and pulse notifications work together: a snatch pulse temporarily
interrupts the holder display, then returns to the active holder display (or,
if ownership changed, to the original pre-holder state).

## Other service commands

```sh
make service-status     # show whether the service is running
make service-logs       # follow its journal log
make service-start      # start it
make service-stop       # stop it
make service-restart    # restart it after configuration changes
make service-authorize  # create a replacement Hue key after pressing the button
make service-list-lights
make service-test-pulse
make help               # show every available target
```

The `systemd` unit uses an unprivileged system-managed identity. Its state is
stored in `/var/lib/potato-hue/watch-state.json`; its secret configuration is
`/etc/potato-hue/.env` with mode `600` and is never stored in the repository.
