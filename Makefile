.DEFAULT_GOAL := help

.PHONY: help build-release service-install service-update service-remove service-start service-stop service-restart service-status service-logs service-authorize service-list-lights service-test-pulse

help:
	@printf '%s\n' \
	  'make build-release       Build the Linux-ready release binary' \
	  'make service-install     Install the binary, config template, and systemd unit' \
	  'make service-update      Rebuild and restart an already-configured service' \
	  'make service-remove      Stop and remove the installed service (keeps config and state)' \
	  'make service-start       Start the service' \
	  'make service-stop        Stop the service' \
	  'make service-restart     Restart the service' \
	  'make service-status      Show service status' \
	  'make service-logs        Follow the service log' \
	  'make service-authorize   Request a new Hue key after pressing the bridge button' \
	  'make service-list-lights List bridge lights and their ids' \
	  'make service-test-pulse  Test one pulse on the selected light'

build-release:
	cargo build --release

service-install: build-release
	sudo sh deploy/install-systemd.sh
	@printf '%s\n' 'Next: edit /etc/potato-hue/.env, press the Hue bridge button, then run make service-authorize.'

service-update: build-release
	sudo systemctl stop potato-hue
	sudo install -m 755 target/release/potato-hue /usr/local/bin/potato-hue
	sudo install -m 644 deploy/potato-hue.service /etc/systemd/system/potato-hue.service
	sudo systemctl daemon-reload
	sudo systemctl start potato-hue
	sudo systemctl --no-pager --full status potato-hue

service-remove:
	sudo systemctl disable --now potato-hue
	sudo rm -f /usr/local/bin/potato-hue /etc/systemd/system/potato-hue.service
	sudo systemctl daemon-reload
	@printf '%s\n' 'Removed the service. Kept /etc/potato-hue/.env and /var/lib/potato-hue so it can be reinstalled without reauthorizing.'

service-start:
	sudo systemctl start potato-hue

service-stop:
	sudo systemctl stop potato-hue

service-restart:
	sudo systemctl restart potato-hue

service-status:
	sudo systemctl --no-pager --full status potato-hue

service-logs:
	sudo journalctl -u potato-hue -f

service-authorize:
	@printf '%s\n' 'Press the physical Hue bridge button now, then press Enter.'
	@read _
	sudo systemctl stop potato-hue
	sudo sh -c 'cd /etc/potato-hue && /usr/local/bin/potato-hue authorize'
	@printf '%s\n' 'Add the printed HUE_APP_KEY to /etc/potato-hue/.env, then run make service-start.'

service-list-lights:
	sudo sh -c 'cd /etc/potato-hue && /usr/local/bin/potato-hue list-lights'

service-test-pulse:
	sudo sh -c 'cd /etc/potato-hue && /usr/local/bin/potato-hue test-pulse'
