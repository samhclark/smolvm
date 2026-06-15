# Smolfile Examples

A Smolfile is the declarative source of truth for a microVM workload. It describes what runs, what resources it needs, and how it behaves — then smolvm uses the same spec for local execution, artifact creation, and future deployment.

## Quick Start

```bash
# Run the OpenClaw gateway from a Smolfile
smolvm machine run -d -s examples/openclaw-app/openclaw.smolfile
curl http://localhost:18789/health

# Run a Python dev environment
smolvm machine run -s examples/python-app/python.smolfile

# Run Doom in a browser
smolvm machine run -d -s examples/doom-web/doom.smolfile
open http://localhost:8080

# Headless Chromium with GPU acceleration
smolvm machine create --name browser -s examples/headless-browser/browser.smolfile
smolvm machine start --name browser
smolvm machine exec --name browser -- \
  chromium --headless=new --no-sandbox --disable-dev-shm-usage \
    --use-gl=angle --use-angle=vulkan \
    --screenshot=/tmp/out.png --window-size=1280,800 \
    https://example.com
smolvm machine exec --name browser -- base64 /tmp/out.png | base64 -d > out.png
```

### Persistent microVMs

```bash
smolvm machine create --name dev -s examples/python-app/python.smolfile
smolvm machine start --name dev
smolvm machine exec --name dev -- python3 --version
smolvm machine stop --name dev
```

## Smolfile Reference

```toml
# Top-level workload fields
image = "ghcr.io/acme/api:1.2.3"    # OCI image (optional — omit for bare Alpine VM)
entrypoint = ["/app/api"]            # executable and fixed leading arguments
cmd = ["serve"]                      # default arguments appended to entrypoint
env = ["PORT=8080"]                  # runtime environment variables
workdir = "/app"                     # working directory

# Resources
cpus = 2                             # vCPUs (default: 4)
memory = 1024                        # MiB (default: 8192)
net = true                           # outbound networking (default: false)
gpu = true                           # GPU acceleration via virtio-gpu/Venus (Vulkan)
gpu_vram = 2048                      # GPU shared-memory region in MiB (default: 4096)
storage = 40                         # storage disk GiB (default: 20)
overlay = 4                          # overlay disk GiB (default: 2)

# Network policy — egress filtering by hostname and/or CIDR
[network]
allow_hosts = ["api.stripe.com", "db.example.com"]  # resolved at VM start
allow_cidrs = ["10.0.0.0/8"]                        # IP/CIDR ranges

# Health checks
[health]
exec = ["curl", "-f", "http://127.0.0.1:8080/health"]
interval = "10s"
timeout = "2s"
retries = 3

# Restart policy (parsed, not yet wired)
[restart]
policy = "always"

# Local development profile
[dev]
volumes = ["./src:/app"]             # host bind mounts
env = ["APP_MODE=development"]       # dev-only env (extends top-level)
init = ["npm install"]               # dev bootstrap commands
workdir = "/app"                     # dev-only workdir override
ports = ["8080:8080"]                # host:guest port forwarding

```

### Merge precedence

CLI flags override Smolfile values. For `machine run`:

```
image:      --image flag > Smolfile image > None (bare Alpine VM)
entrypoint: --entrypoint flag > Smolfile entrypoint > image metadata
cmd:        trailing args (after --) > Smolfile cmd > image metadata
env:        top-level env + [dev].env + CLI -e
init:       [dev].init + CLI --init
volumes:    [dev].volumes + CLI -v
ports:      [dev].ports + CLI -p
```
