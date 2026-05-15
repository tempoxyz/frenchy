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

```text
GET  /dedicated/server
GET  /dedicated/server/*
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

## Keys

```text
/             filter by displayed server name
enter/esc     finish filtering
backspace     edit filter
ctrl-u        clear filter while filtering
r             refresh servers
j/k or arrows move selection
tab/enter     switch focus
pgup/pgdn     scroll details
i             request HTML5 IPMI access and open the returned URL
R             press twice to restart the selected server
q/esc/ctrl-c  quit
```

The IPMI action calls:

```text
POST /dedicated/server/{serviceName}/features/ipmi/access
GET  /dedicated/server/{serviceName}/features/ipmi/access?type=kvmipHtml5URL
```

and opens the returned URL with the system browser.

The restart action calls:

```text
POST /dedicated/server/{serviceName}/reboot
```
