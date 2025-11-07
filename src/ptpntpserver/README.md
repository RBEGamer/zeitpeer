# PTP-Backed NTP Server

This container runs `chronyd` using a Precision Time Protocol (PTP) hardware clock exposed from the host (e.g., `/dev/ptp0`) as its reference. It then serves that disciplined time to downstream NTP clients.

## Requirements

- Host must expose a PHC device such as `/dev/ptp0`. Map it into the container via `devices` in `docker-compose.yml`.
- The container needs permission to interact with the device (Docker adds this when binding the device).

## Run

```bash
docker compose up ptpntpserver
```

By default the service publishes UDP port `2123` on the host and keeps drift data in the `ntp-ptp-state` volume.

You can tweak `ptpntpserver/chrony.conf` if a different device path or chrony parameters are required.
