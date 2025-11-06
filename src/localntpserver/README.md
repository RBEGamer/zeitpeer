# NTP Sidecar

This container runs a minimal `chronyd` server that serves time directly from the host clock. It is useful when you need a local NTP source for the other services in `docker-compose.yml` without relying on the public internet.

## Build & Run

```bash
docker compose up ntp
```

The service exposes UDP port `123` and persists the chrony drift data inside the `ntp-state` volume.

## Notes

- The container does **not** sync with upstream NTP servers. It simply serves whatever time the host kernel reports via the `local` clock configuration.
- Adjust the `allow` statements in `ntp/chrony.conf` if you want to restrict which clients can query the server.
