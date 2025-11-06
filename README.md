# Zeitpeer

## Goal 
Design and evaluation of a decentralized, UDP-based time consensus protocol for the Internet that achieves sub-millisecond accuracy compared to classic NTP through many independent peers, robust asymmetry filtering, and drift modeling — without dedicated WAN-PTP infrastructure.
Reference peers have Linux PHC (/dev/ptpX); clients estimate local drift against a PHC time source.

## Problem

Classic protocols such as NTPv4 (RFC 5905) suffer from asymmetric one-way delays, variable jitter, and path changes on the open Internet → systematic offsets in the ms range.
PTP (IEEE 1588-2019) requires deterministic networks/hardware timestamping and is not intended for the open Internet.
There is a lack of a decentralized, statistically robust approach that actively merges many peers and models drift cleanly. 
