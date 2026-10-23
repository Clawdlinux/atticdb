# Live bench dashboard

Prometheus, Grafana and node_exporter for watching a bench run. It's built for one Linux bench VM, not for production.
Everything listens on 127.0.0.1. You view it through an SSH tunnel.

It needs a zerodb server with `GET /metrics` (#31) and Docker with Compose.

## Run it

On the bench VM, with zerodb serving from `/mnt/nvme/z`:

```sh
cd bench/monitoring
ZERODB_SOCKET_DIR=/mnt/nvme/z docker compose up -d
```

Prometheus can't scrape a Unix socket, so `socat` bridges `zerodb.sock` to `127.0.0.1:9425`.
Prometheus scrapes it and node_exporter every second.
If the server restarts, the socket file is replaced and the bridge connects to the new one.

From your machine:

```sh
ssh -N -L 3000:127.0.0.1:3000 azureuser@<vm>
```

Open http://localhost:3000. The "zerodb bench" dashboard is the home page, and anonymous access is on. That's fine only because it's bound to loopback.

Stop it with `docker compose down`.

## Panels

| Panel | Query |
|---|---|
| Committed appends | `rate(zerodb_append_ack_seconds_count[10s])` |
| Append ack latency | p50, p99, p99.9 from `zerodb_append_ack_seconds_bucket` |
| Requests per sync group | `writer_group_requests_total` / `writer_groups_total` |
| Busy replies | `rate(zerodb_busy_replies_total[10s])`, 503 `busy` answers |
| Disk writes, disk write latency | node_exporter, NVMe and SCSI devices |
| CPU, context switches | node_exporter. Context switches track the thread handoffs in #34 |

Quantiles come from histogram buckets of 50 µs to 1 s, so they are approximate within a bucket.
`zerodb load` reports exact percentiles. Use it for results and this dashboard for watching.

## Watch, then measure with it down

The stack changes what it watches. On an L8s v3 with 8 vCPUs, `zerodb load` against local NVMe, every run with all four containers up landed in a slow mode (16 of 16):

| Load | Stack down | Stack up |
|---|---|---|
| 16 writers, no think | 55.8k acks/s, p50 274 µs | 30.2k acks/s, p50 509 µs |
| 64 writers, 5 ms think | p50 132 µs | p50 356 µs |

Without the stack, 2 of about 40 runs hit the same slow mode. No single container caused it. Run `docker compose stop` before runs you publish, and report medians of at least 3.
