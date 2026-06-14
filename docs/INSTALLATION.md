# Installation Guide

This guide covers installation of pg-loganalyze from pre-built packages for various Linux distributions.

## Quick Start

### Debian/Ubuntu (.deb packages)

```bash
# Download the appropriate package for your architecture
wget https://github.com/egeapak/pg-loganalyze/releases/latest/download/pg-loganalyze_<version>_amd64.deb
wget https://github.com/egeapak/pg-loganalyze/releases/latest/download/pg-loganalyze-exporter_<version>_amd64.deb

# Install packages
sudo dpkg -i pg-loganalyze_<version>_amd64.deb pg-loganalyze-exporter_<version>_amd64.deb

# Fix dependencies if needed
sudo apt-get install -f

# Verify installation
pg-loganalyze --version
systemctl status pg-loganalyze-exporter.service
```

### RHEL/Fedora/CentOS (.rpm packages)

```bash
# Download the appropriate package for your architecture
wget https://github.com/egeapak/pg-loganalyze/releases/latest/download/pg-loganalyze-<version>.x86_64.rpm
wget https://github.com/egeapak/pg-loganalyze/releases/latest/download/pg-loganalyze-exporter-<version>.x86_64.rpm

# Install packages (choose one method)

# Method 1: Using rpm (basic)
sudo rpm -i pg-loganalyze-<version>.x86_64.rpm pg-loganalyze-exporter-<version>.x86_64.rpm

# Method 2: Using dnf (recommended - handles dependencies)
sudo dnf install pg-loganalyze-<version>.x86_64.rpm pg-loganalyze-exporter-<version>.x86_64.rpm

# Method 3: Using yum (older systems)
sudo yum install pg-loganalyze-<version>.x86_64.rpm pg-loganalyze-exporter-<version>.x86_64.rpm

# Verify installation
pg-loganalyze --version
systemctl status pg-loganalyze-exporter.service
```

## Supported Platforms

### Architectures

| Architecture | Debian/Ubuntu | RHEL/Fedora | Description |
|--------------|---------------|-------------|-------------|
| amd64/x86_64 | ✅ | ✅ | Intel/AMD 64-bit (most common) |
| arm64/aarch64 | ✅ | ✅ | ARM 64-bit (servers, modern ARM) |
| armhf/armv7 | ✅ | ✅ | ARM 32-bit (Raspberry Pi, embedded) |
| i386/i686 | ✅ | ✅ | Intel/AMD 32-bit (legacy systems) |

### Distribution Support

#### Debian Family
- **Debian**: 10 (Buster), 11 (Bullseye), 12 (Bookworm)
- **Ubuntu**: 18.04 LTS, 20.04 LTS, 22.04 LTS, 24.04 LTS
- **Linux Mint**: 20.x, 21.x, 22.x
- **Elementary OS**: 6.x, 7.x

#### Red Hat Family
- **RHEL**: 8.x, 9.x
- **Fedora**: 37, 38, 39, 40
- **CentOS**: 8, 9 (Stream)
- **Rocky Linux**: 8.x, 9.x
- **AlmaLinux**: 8.x, 9.x

## Components

The pg-loganalyze suite consists of two main components:

### 1. pg-loganalyze (TUI Application)
- **Package**: `pg-loganalyze`
- **Binary**: `/usr/bin/pg-loganalyze`
- **Purpose**: Interactive terminal interface for analyzing PostgreSQL logs
- **Usage**: `pg-loganalyze /path/to/postgresql.log`

### 2. pg-loganalyze-exporter (Prometheus Exporter)
- **Package**: `pg-loganalyze-exporter`
- **Binary**: `/usr/bin/pg-loganalyze-exporter`
- **Service**: `pg-loganalyze-exporter.service`
- **Purpose**: Prometheus metrics exporter for continuous monitoring
- **Config**: `/etc/pg-loganalyze-exporter/`

### 3. pg_loganalyze (PostgreSQL extension)
- **Purpose**: in-database query-statistics capture (like `pg_stat_statements`),
  no log file required
- **Install**: built with `pgrx` (PostgreSQL 13–18) — see
  [PostgreSQL extension](#postgresql-extension-in-database-capture) below
- **Config**: `loganalyze.*` GUCs — see [CONFIGURATION.md](CONFIGURATION.md)

## PostgreSQL extension (in-database capture)

Separate from the TUI/exporter above, `pg_loganalyze` is also a **PostgreSQL
extension** that captures cumulative query statistics *inside the server* (like
`pg_stat_statements`), with no log file. Supported majors: **PostgreSQL 13–18**.

### Build & install

The extension is built with [`pgrx`](https://github.com/pgcentralfoundation/pgrx):

```bash
cargo install cargo-pgrx --locked --version 0.18.1
cargo pgrx init --pg16 "$(which pg_config)"     # or point at your server's pg_config

cd crates/pg_extension
# Compile + install into the cluster that owns that pg_config:
cargo pgrx install --release --no-default-features --features pg16 -c "$(which pg_config)"
```

Use the matching `--features pgNN` (`pg13`…`pg18`) and the
`postgresql-server-dev-NN` headers for your major. To build a throwaway server
image with the extension baked in (handy for trying it), see
`crates/pg_extension/docker/Dockerfile.bench`.

### Enable

```ini
# postgresql.conf  (requires a restart — it preloads a worker + executor hooks)
shared_preload_libraries = 'pg_loganalyze'
loganalyze.capture_mode  = 'hook'      # in-process capture (no auto_explain)
```

```sql
-- in the database named by loganalyze.database (default: postgres)
CREATE EXTENSION pg_loganalyze;
SELECT * FROM loganalyze.statements_summary ORDER BY total_time_ms DESC;
```

`CREATE EXTENSION` works without `shared_preload_libraries` too (manual
`loganalyze_ingest()` and all SQL functions), but automatic `hook`/`log` capture
needs the preload. Full knob reference: [CONFIGURATION.md](CONFIGURATION.md);
SQL views/functions: [the extension README](../crates/pg_extension/README.md).

## Post-Installation Setup

### Systemd Service (Exporter)

After installing the exporter package, configure and enable the service:

```bash
# Check service status
sudo systemctl status pg-loganalyze-exporter.service

# Configure the exporter (edit configuration file)
sudo nano /etc/pg-loganalyze-exporter/config.toml

# Enable and start the service
sudo systemctl enable pg-loganalyze-exporter.service
sudo systemctl start pg-loganalyze-exporter.service

# View logs
sudo journalctl -u pg-loganalyze-exporter.service -f
```

### User and Permissions

The exporter runs as the `pg-loganalyze` user, which is automatically created during installation. Ensure your PostgreSQL log files are readable by this user:

```bash
# Add pg-loganalyze user to postgres group (if needed)
sudo usermod -a -G postgres pg-loganalyze

# Set appropriate permissions on log directory
sudo chmod 755 /var/log/postgresql/
sudo chmod 644 /var/log/postgresql/*.log
```

## Troubleshooting

### Common Issues

#### Package Installation Fails
```bash
# Debian/Ubuntu: Fix broken dependencies
sudo apt-get update
sudo apt-get install -f

# RHEL/Fedora: Check for conflicts
sudo dnf check
sudo dnf install --skip-broken
```

#### Service Won't Start
```bash
# Check service logs
sudo journalctl -u pg-loganalyze-exporter.service --no-pager -l

# Verify configuration
sudo -u pg-loganalyze pg-loganalyze-exporter --config /etc/pg-loganalyze-exporter/config.toml --dry-run

# Check file permissions
ls -la /etc/pg-loganalyze-exporter/
ls -la /var/log/postgresql/
```

#### Binary Not Found
```bash
# Refresh PATH
hash -r

# Check installation
dpkg -l | grep pg-loganalyze   # Debian/Ubuntu
rpm -qa | grep pg-loganalyze   # RHEL/Fedora

# Reinstall if necessary
sudo apt-get reinstall pg-loganalyze pg-loganalyze-exporter   # Debian/Ubuntu
sudo dnf reinstall pg-loganalyze pg-loganalyze-exporter       # RHEL/Fedora
```

## Uninstallation

### Debian/Ubuntu
```bash
# Remove packages but keep configuration
sudo apt-get remove pg-loganalyze pg-loganalyze-exporter

# Complete removal including configuration
sudo apt-get purge pg-loganalyze pg-loganalyze-exporter

# Clean up dependencies
sudo apt-get autoremove
```

### RHEL/Fedora/CentOS
```bash
# Remove packages
sudo dnf remove pg-loganalyze pg-loganalyze-exporter
# or
sudo rpm -e pg-loganalyze pg-loganalyze-exporter

# Manual cleanup (if needed)
sudo userdel pg-loganalyze
sudo rm -rf /etc/pg-loganalyze-exporter/
```

## Alternative Installation Methods

### From Source
See [DEVELOPMENT.md](DEVELOPMENT.md) for building from source.

### Container Images
```bash
# Docker (if available)
docker pull ghcr.io/egeapak/pg-loganalyze:latest
docker run -v /path/to/logs:/logs pg-loganalyze /logs/postgresql.log
```

## Getting Help

- **Issues**: https://github.com/egeapak/pg-loganalyze/issues
- **Discussions**: https://github.com/egeapak/pg-loganalyze/discussions
- **Documentation**: https://github.com/egeapak/pg-loganalyze/tree/main/docs