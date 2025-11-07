# Public NTP Server Container

This container runs `chronyd` configured to sync time from several public NTP providers (`pool.ntp.org`, Google, Cloudflare, Meta). It can act as a local NTP source for the rest of the docker-compose stack while staying disciplined against real upstream references.

## Run

```bash
docker compose up ntp_public
```

Port UDP 123 is published by default. Persistent drift data lives in the `ntp-public-state` named volume so the daemon can keep calibration details between restarts.

Adjust `publicntpserver/chrony.conf` if you prefer different upstream pools or need to restrict downstream access with narrower `allow` CIDRs.
