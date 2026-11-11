# Hosting AtticDB for remote agents

This runbook stands up 1 hosted store on 1 Azure VM. Agents on any machine reach it at `https://<host>/mcp` with a bearer token. Nothing gets installed where the agent runs. The design is [0010](design/0010-hosted-mcp-endpoint.md).

The pieces:
- `atticdb serve` binds loopback only. It answers MCP at `POST /mcp`.
- Caddy on the same VM holds the certificate for `<host>`. It terminates TLS and proxies `/mcp` to `127.0.0.1:<port>`.
- The data dir lives on a Premium SSD v2 managed disk.

Placeholders: `<rg>`, `<region>`, `<zone>`, `<vm>`, `<disk>`, `<admin>`, `<your-ip>`, `<host>`, `<port>`, `<tenant>` and `<token>`. Never commit real values. `<port>` can be `7425`, the default.

Before you create anything:
- Get the owner's go-ahead and a spend cap. See [Cost and deallocation](#8-cost-and-deallocation).
- Azure budgets only alert. They never stop spend. So deallocate the VM when a run ends.

## 1. VM

1 Azure Linux 3 VM in 1 availability zone. The data dir goes on a Premium SSD v2 data disk.

Never put the data dir on a local NVMe or temp disk. Azure wipes it on deallocate. `Standard_D2as_v5` has no local temp disk, so there's nothing to confuse.

Premium SSD v2 attaches only to a zonal VM in most regions. It can't be an OS disk. Check that your region and zone take it:

```sh
az vm list-skus --resource-type disks \
  --query "[?name=='PremiumV2_LRS'].{Region:locationInfo[0].location, Zones:locationInfo[0].zones}" -o table
```

Create the group, the disk and the VM. 3,000 IOPS and 125 MB/s are the free baseline.

```sh
az group create -n <rg> -l <region>

az disk create -g <rg> -n <disk> -l <region> --zone <zone> \
  --sku PremiumV2_LRS --size-gb 64 \
  --disk-iops-read-write 3000 --disk-mbps-read-write 125

az vm create -g <rg> -n <vm> -l <region> --zone <zone> \
  --image MicrosoftCBLMariner:azure-linux-3:azure-linux-3-gen2:latest \
  --size Standard_D2as_v5 --os-disk-size-gb 32 \
  --admin-username <admin> --ssh-key-values <path to your .pub key> \
  --public-ip-sku Standard \
  --attach-data-disks <disk>
```

Open 80 and 443 in the VM's network security group. Caddy needs 80 for its certificate. Then limit SSH to your address:

```sh
az vm open-port -g <rg> -n <vm> --port 80,443 --priority 1010
az network nsg rule update -g <rg> --nsg-name <vm>NSG -n default-allow-ssh \
  --source-address-prefixes <your-ip>/32
```

Azure Linux also has its own firewall. Its default rules drop every inbound port but 22. If `systemctl is-active iptables` prints `active`, open 80 and 443 there too:

```sh
sudo sed -i '/--dport 22 -j ACCEPT/a -A INPUT -p tcp -m tcp --dport 80 -j ACCEPT\n-A INPUT -p tcp -m tcp --dport 443 -j ACCEPT' \
  /etc/systemd/scripts/ip4save
sudo systemctl restart iptables
```

On the VM, format and mount the data disk. `lsblk` shows it as the 64G disk with no partitions. Call it `/dev/<dev>` below.

```sh
lsblk
sudo mkfs.ext4 -L atticdb /dev/<dev>
sudo mkdir -p /srv/atticdb
echo 'LABEL=atticdb /srv/atticdb ext4 defaults,nofail 0 2' | sudo tee -a /etc/fstab
sudo mount /srv/atticdb
findmnt /srv/atticdb   # must show /dev/<dev>, not the OS disk
```

Make the service user and give it the disk. Then install `atticdb`. Use a release that has `POST /mcp`, and pin it with `ATTICDB_VERSION`.

```sh
sudo useradd --system --home-dir /srv/atticdb --shell /usr/sbin/nologin atticdb
sudo chown atticdb:atticdb /srv/atticdb
sudo chmod 700 /srv/atticdb

curl -fsSLO https://github.com/Clawdlinux/atticdb/releases/latest/download/install.sh
sudo env ATTICDB_INSTALL_DIR=/usr/local/bin ATTICDB_VERSION=<version> sh install.sh
atticdb --version
```

## 2. DNS

Point `<host>` at the VM's public IP. Get the IP:

```sh
az vm show -d -g <rg> -n <vm> --query publicIps -o tsv
```

Then add an `A` record for `<host>` at your DNS provider. A Standard public IP is static, so it survives deallocate.

No domain? Add `--public-ip-address-dns-name <label>` to `az vm create` in step 1. That gives `<label>.<region>.cloudapp.azure.com`. Use that as `<host>`.

Wait until `dig +short <host>` prints the IP. Caddy can't get a certificate before that.

## 3. Caddy

Azure Linux has no Caddy package. Install the release binary and check it against Caddy's SHA-512 sums. Steps are from [Caddy's manual install](https://caddyserver.com/docs/running#manual-installation).

```sh
V=<caddy version, such as 2.11.9>
F=caddy_${V}_linux_amd64.tar.gz
curl -fsSLO https://github.com/caddyserver/caddy/releases/download/v$V/$F
curl -fsSLO https://github.com/caddyserver/caddy/releases/download/v$V/caddy_${V}_checksums.txt
grep " $F\$" caddy_${V}_checksums.txt | sha512sum -c
tar xzf $F caddy
sudo install -m 0755 caddy /usr/bin/caddy

sudo groupadd --system caddy
sudo useradd --system --gid caddy --create-home --home-dir /var/lib/caddy \
  --shell /usr/sbin/nologin caddy
sudo curl -fsSL -o /etc/systemd/system/caddy.service \
  https://raw.githubusercontent.com/caddyserver/dist/master/init/caddy.service
```

Write `/etc/caddy/Caddyfile`:

```
<host> {
	handle /mcp {
		request_body {
			max_size 8MiB
		}
		reverse_proxy 127.0.0.1:<port> {
			header_up Host 127.0.0.1
			request_buffers 8MiB
		}
	}
	handle {
		respond 404
	}
}
```

Why each line:
- `header_up Host 127.0.0.1`: `serve` refuses a `Host` that isn't loopback. Caddy passes the client's `Host` through unless told.
- `request_buffers 8MiB`: an HTTP/2 client may send a body with no length. Caddy then forwards it chunked, and `serve` answers 501. Buffering sets `Content-Length` instead. A client that sends a chunked HTTP/1.1 body still gets 501.
- `max_size 8MiB`: `serve` caps a body at 8 MiB. Caddy answers 413 above that.
- `respond 404`: only `/mcp` is public. `serve`'s other routes stay on the VM.

Don't add a `log` directive. An access log would record each `Mcp-Session-Id`.

```sh
sudo caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
sudo systemctl daemon-reload
sudo systemctl enable --now caddy
```

## 4. `atticdb serve` as a systemd unit

Write `/etc/systemd/system/atticdb.service`:

```ini
[Unit]
Description=AtticDB hosted store
After=network-online.target
Wants=network-online.target
RequiresMountsFor=/srv/atticdb

[Service]
User=atticdb
Group=atticdb
ExecStart=/usr/local/bin/atticdb serve --config /etc/atticdb/atticdb.json \
  --data-dir /srv/atticdb/data --listen 127.0.0.1:<port> --exit-on-poison
Restart=always
RestartSec=1
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ReadWritePaths=/srv/atticdb

[Install]
WantedBy=multi-user.target
```

- `--listen 127.0.0.1:<port>`: loopback only. `serve` refuses any other address.
- `--exit-on-poison`: a poisoned engine exits 70. `Restart=always` starts it again, and a fresh start recovers.
- `RequiresMountsFor`: `serve` never starts on the OS disk when the data disk is missing.
- `serve` creates `/srv/atticdb/data` with mode 0700, and its Unix socket in there.

Start it after you add the first tenant in step 5.

## 5. Add a tenant

A tenant is 1 entry in `/etc/atticdb/atticdb.json` with a random token. The token picks the tenant.

The first tenant: `atticdb init` writes a new 0600 config and prints the token once. It never overwrites a file.

```sh
sudo install -d -o atticdb -g atticdb -m 700 /etc/atticdb
sudo -u atticdb atticdb init --config /etc/atticdb/atticdb.json --tenant <tenant>
sudo systemctl daemon-reload
sudo systemctl enable --now atticdb
```

Each later tenant is a hand edit. Make a token:

```sh
head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n'; echo
```

Add it to the `tenants` list. Tokens must be unique, 32 to 1024 visible ASCII bytes:

```json
{
  "tenants": [
    {"name": "<tenant>", "token": "<token>"},
    {"name": "<tenant 2>", "token": "<token 2>"}
  ]
}
```

Edit as the `atticdb` user, so the file keeps its owner and mode. Then restart:

```sh
sudo -u atticdb vi /etc/atticdb/atticdb.json
sudo stat -c '%U %a' /etc/atticdb/atticdb.json   # must print: atticdb 600
sudo systemctl restart atticdb
```

`serve` reads the config only at start. A restart drops every `/mcp` session. Clients get 404 on their next call and start a new session.

Give each person their `<token>` through a password manager. Never send it in chat or email, or put it in a repo.

## 6. Rotate a token

1. Make a new token as in step 5.
2. Replace that tenant's `token` in `/etc/atticdb/atticdb.json`. Check the owner and mode.
3. `sudo systemctl restart atticdb`.
4. Put the new token in each of that person's apps.

The old token gets 401 right after the restart. The tenant's data stays. Other tenants' clients only see a 404 and start a new session.

## 7. First-user check

Check the endpoint from a terminal first. Keep the token out of your shell history with a header file:

```sh
(umask 077; printf 'Authorization: Bearer %s\n' '<token>' > auth.txt)
curl -sS -D - https://<host>/mcp -H @auth.txt -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"curl","version":"1"}}}'
```

Expect `200` and an `Mcp-Session-Id` header. Then send it back:

```sh
SID=<the Mcp-Session-Id value>
call() { curl -sS https://<host>/mcp -H @auth.txt -H "Mcp-Session-Id: $SID" \
  -H 'MCP-Protocol-Version: 2025-11-25' -H 'Content-Type: application/json' -d "$1"; echo; }
call '{"jsonrpc":"2.0","method":"notifications/initialized"}'   # 202, empty
call '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'           # 7 tools
call '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"fact_set","arguments":{"key":"user/hosted-check/curl","content":"ok"}}}'
call '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"fact_get","arguments":{"key":"user/hosted-check/curl"}}}'
rm auth.txt
```

`fact_set` prints `committed lsn=<n>`, and `fact_get` prints `ok`.

Then connect the 5 apps with their [snippets](../README.md#hosted): Copilot CLI, VS Code, Claude Code, Codex and Gemini CLI. In each, start a chat and ask:

> Use the atticdb-hosted tools. Call fact_set with key user/hosted-check/<app> and content ok. Then call fact_get on that key.

Pass when `fact_set` commits and `fact_get` returns `ok`. A 403 means the app sent a web `Origin`. Note which app and file an issue: the allowlist in 0010's call 2 is the fix.

The check keys are tiny. Leave them, or ask the agent to call `fact_set` with `delete: true`.

## 8. Cost and deallocation

Prices from the [Azure Retail Prices API](https://prices.azure.com/api/retail/prices) on 2026-11-11. East US, pay as you go, 730 h a month:

| Item | Price | Per month |
|---|---|---|
| `Standard_D2as_v5` Linux VM | $0.086/h | $62.78 |
| Premium SSD v2, 64 GiB, baseline IOPS and MB/s | $0.00011/GiB/h | $5.14 |
| OS disk, 32 GiB Premium SSD (P4) | $5.28/month | $5.28 |
| Standard static public IP | $0.005/h | $3.65 |
| **Running** | | **about $77** |
| **Deallocated**: disks and IP only | | **about $14** |

Egress isn't included. Check prices for your region before you start.

Deallocate when a run ends. The disks, the IP and the DNS name stay. Sessions don't, so clients start new ones.

```sh
az vm deallocate -g <rg> -n <vm>
az vm start -g <rg> -n <vm>      # Caddy and atticdb start on boot
```

Delete everything when you're done. This deletes the data disk too, so export first if you need the data:

```sh
az group delete -n <rg>
```
