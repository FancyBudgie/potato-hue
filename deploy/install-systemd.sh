#!/usr/bin/env sh
set -eu

# Run from the repository root, after building target/release/potato-hue.
install -d -m 700 /etc/potato-hue
if [ ! -f /etc/potato-hue/.env ]; then
  install -m 600 .env.example /etc/potato-hue/.env
  echo "Created /etc/potato-hue/.env; edit it before starting the service."
fi
install -m 755 target/release/potato-hue /usr/local/bin/potato-hue
install -m 644 deploy/potato-hue.service /etc/systemd/system/potato-hue.service
systemctl daemon-reload
echo "Installed. Authorize the bridge, then run: systemctl enable --now potato-hue"
