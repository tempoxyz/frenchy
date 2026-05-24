# frenchy

A small Rust TUI for exploring OVHcloud dedicated servers from the US OVH API.

## Setup

On first run, frenchy creates:

```text
~/.config/frenchy/config.toml
```

Fill it with your OVH API application and consumer key:

```toml
application_key = "..."
application_secret = "..."
consumer_key = "..."

endpoint = "https://api.us.ovhcloud.com/1.0"
ipmi_type = "kvmipHtml5URL"
ipmi_ttl = "15"
```

Environment variables can override the config file:

```sh
export OVH_APPLICATION_KEY=...
export OVH_APPLICATION_SECRET=...
export OVH_CONSUMER_KEY=...
export OVH_ENDPOINT=https://api.us.ovhcloud.com/1.0
export OVH_IPMI_TYPE=kvmipHtml5URL
export OVH_IPMI_TTL=15
```

The consumer key needs read access to dedicated server routes and write access
for IPMI session creation and restarts:

Use this link to create a US OVHcloud API token with the required rights:

```text
https://api.us.ovhcloud.com/createToken/index.cgi?GET=%2Fdedicated%2Fserver&GET=%2Fdedicated%2Fserver%2F%2A&GET=%2Fdedicated%2Fserver%2F%2A%2Fspecifications%2Fhardware&GET=%2Fdedicated%2Fserver%2F%2A%2Fspecifications%2Fnetwork&GET=%2Fdedicated%2Fserver%2F%2A%2FnetworkInterfaceController&GET=%2Fdedicated%2Fserver%2F%2A%2FnetworkInterfaceController%2F%2A&GET=%2Fdedicated%2Fserver%2F%2A%2FvirtualNetworkInterface&GET=%2Fdedicated%2Fserver%2F%2A%2FvirtualNetworkInterface%2F%2A&GET=%2Fdedicated%2Fserver%2F%2A%2FvirtualMac&GET=%2Fdedicated%2Fserver%2F%2A%2FvirtualMac%2F%2A&POST=%2Fdedicated%2Fserver%2F%2A%2Ffeatures%2Fipmi%2Faccess&POST=%2Fdedicated%2Fserver%2F%2A%2Freboot
```

If OVH returns `Invalid account/password`, make sure you are using the API
region that owns the account. For OVH US sub-users, use:

```text
https://us.ovhcloud.com/auth/api/createToken?GET=%2Fdedicated%2Fserver&GET=%2Fdedicated%2Fserver%2F%2A&GET=%2Fdedicated%2Fserver%2F%2A%2Fspecifications%2Fhardware&GET=%2Fdedicated%2Fserver%2F%2A%2Fspecifications%2Fnetwork&GET=%2Fdedicated%2Fserver%2F%2A%2FnetworkInterfaceController&GET=%2Fdedicated%2Fserver%2F%2A%2FnetworkInterfaceController%2F%2A&GET=%2Fdedicated%2Fserver%2F%2A%2FvirtualNetworkInterface&GET=%2Fdedicated%2Fserver%2F%2A%2FvirtualNetworkInterface%2F%2A&GET=%2Fdedicated%2Fserver%2F%2A%2FvirtualMac&GET=%2Fdedicated%2Fserver%2F%2A%2FvirtualMac%2F%2A&POST=%2Fdedicated%2Fserver%2F%2A%2Ffeatures%2Fipmi%2Faccess&POST=%2Fdedicated%2Fserver%2F%2A%2Freboot
```

```text
GET  /dedicated/server
GET  /dedicated/server/*
GET  /dedicated/server/*/specifications/hardware
GET  /dedicated/server/*/specifications/network
GET  /dedicated/server/*/networkInterfaceController
GET  /dedicated/server/*/networkInterfaceController/*
GET  /dedicated/server/*/virtualNetworkInterface
GET  /dedicated/server/*/virtualNetworkInterface/*
GET  /dedicated/server/*/virtualMac
GET  /dedicated/server/*/virtualMac/*
POST /dedicated/server/*/features/ipmi/access
POST /dedicated/server/*/reboot
```

You can also print this from the binary:

```sh
cargo run -- credential-help
```

## Run

```sh
cargo run
```

The server list includes an at-a-glance hardware summary. Selecting a server
shows CPU topology, memory, disk groups, RAID details, motherboard, boot mode,
server id, rack, datacenter, region/availability zone, bandwidth, routing,
vRack/vMAC support, OLA, physical network controllers with public/private MACs,
and virtual network interfaces with their MAC addresses when OVH returns them.

## Keys

```text
/             filter by inventory fields
enter/esc     finish filtering
backspace     edit filter
ctrl-u        clear filter while filtering
r             refresh servers
j/k or arrows move selection
ctrl-d/ctrl-u move selection by half a page
gg/G          jump to first/last server
tab/enter     switch focus
pgup/pgdn     scroll details
i/K           request HTML5 KVM/IPMI access and open the returned URL
R             press twice to restart the selected server
q/esc/ctrl-c  quit
```

The KVM action calls OVH's IPMI access API:

```text
POST /dedicated/server/{serviceName}/features/ipmi/access
GET  /dedicated/server/{serviceName}/features/ipmi/access?type=kvmipHtml5URL
```

and opens the returned HTML5 KVM URL with the system browser.

The restart action calls:

```text
POST /dedicated/server/{serviceName}/reboot
```
