# Installation Guide

This guide covers installation of pg-plansight from pre-built packages for various Linux distributions.

## Quick Start

### Debian/Ubuntu (.deb packages)

```bash
# Download the appropriate package for your architecture
wget https://github.com/egeapak/pg-plansight/releases/latest/download/pg-plansight_<version>_amd64.deb
wget https://github.com/egeapak/pg-plansight/releases/latest/download/pg-plansight-exporter_<version>_amd64.deb

# Install packages
sudo dpkg -i pg-plansight_<version>_amd64.deb pg-plansight-exporter_<version>_amd64.deb

# Fix dependencies if needed
sudo apt-get install -f

# Verify installation
pg-plansight --version
systemctl status pg-plansight-exporter.service
```

### RHEL/Fedora/CentOS (.rpm packages)

```bash
# Download the appropriate package for your architecture
wget https://github.com/egeapak/pg-plansight/releases/latest/download/pg-plansight-<version>.x86_64.rpm
wget https://github.com/egeapak/pg-plansight/releases/latest/download/pg-plansight-exporter-<version>.x86_64.rpm

# Install packages (choose one method)

# Method 1: Using rpm (basic)
sudo rpm -i pg-plansight-<version>.x86_64.rpm pg-plansight-exporter-<version>.x86_64.rpm

# Method 2: Using dnf (recommended - handles dependencies)
sudo dnf install pg-plansight-<version>.x86_64.rpm pg-plansight-exporter-<version>.x86_64.rpm

# Method 3: Using yum (older systems)
sudo yum install pg-plansight-<version>.x86_64.rpm pg-plansight-exporter-<version>.x86_64.rpm

# Verify installation
pg-plansight --version
systemctl status pg-plansight-exporter.service
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

The pg-plansight suite consists of two main components:

### 1. pg-plansight (TUI Application)
- **Package**: `pg-plansight`
- **Binary**: `/usr/bin/pg-plansight`
- **Purpose**: Interactive terminal interface for analyzing PostgreSQL logs
- **Usage**: `pg-plansight /path/to/postgresql.log`

### 2. pg-plansight-exporter (Prometheus Exporter)
- **Package**: `pg-plansight-exporter`
- **Binary**: `/usr/bin/pg-plansight-exporter`
- **Service**: `pg-plansight-exporter.service`
- **Purpose**: Prometheus metrics exporter for continuous monitoring
- **Config**: `/etc/pg-plansight-exporter/`

### 3. pg_plansight (PostgreSQL extension)
- **Purpose**: in-database query-statistics capture (like `pg_stat_statements`),
  no log file required
- **Install**: built with `pgrx` (PostgreSQL 13–18) — see
  [PostgreSQL extension](#postgresql-extension-in-database-capture) below
- **Config**: `plansight.*` GUCs — see [CONFIGURATION.md](CONFIGURATION.md)

## PostgreSQL extension (in-database capture)

Separate from the TUI/exporter above, `pg_plansight` is also a **PostgreSQL
extension** that captures cumulative query statistics *inside the server* (like
`pg_stat_statements`), with no log file. Supported majors: **PostgreSQL 13–18**.

### Build & install

The extension is built with [`pgrx`](https://github.com/pgcentralfoundation/pgrx):

```bash
cargo install cargo-pgrx --locked --version 0.19.1
cargo pgrx init --pg16 "$(which pg_config)"     # or point at your server's pg_config

cd crates/pg_extension
# Compile + install into the cluster that owns that pg_config:
cargo pgrx install --release --no-default-features --features pg16 -c "$(which pg_config)"
```

Use the matching `--features pgNN` (`pg13`…`pg18`) and the
`postgresql-server-dev-NN` headers for your major. To build a throwaway server
image with the extension baked in (handy for trying it), see
`crates/pg_extension/docker/Dockerfile.bench`.

**Or build a deb/rpm to ship to a server** — `just ext-package 16` produces
versioned packages (`postgresql-16-plansight_<version>_<arch>.deb` /
`plansight_16-<version>.<arch>.rpm`) you copy and `dpkg -i` / `rpm -Uvh`. See
[PACKAGING.md](PACKAGING.md) (including cross-arch and how updates work).

### Enable

```ini
# postgresql.conf  (requires a restart — it preloads a worker + executor hooks)
shared_preload_libraries = 'pg_plansight'
plansight.capture_mode  = 'hook'      # in-process capture (no auto_explain)
```

```sql
-- in the database named by plansight.database (default: postgres)
CREATE EXTENSION pg_plansight;
SELECT * FROM plansight.statements_summary ORDER BY total_time_ms DESC;
```

`CREATE EXTENSION` works without `shared_preload_libraries` too (manual
`plansight_ingest()` and all SQL functions), but automatic `hook`/`log` capture
needs the preload. Full knob reference: [CONFIGURATION.md](CONFIGURATION.md);
SQL views/functions: [the extension README](../crates/pg_extension/README.md).

## Post-Installation Setup

### Systemd Service (Exporter)

After installing the exporter package, configure and enable the service:

```bash
# Check service status
sudo systemctl status pg-plansight-exporter.service

# Configure the exporter (edit configuration file)
sudo nano /etc/pg-plansight-exporter/config.toml

# Enable and start the service
sudo systemctl enable pg-plansight-exporter.service
sudo systemctl start pg-plansight-exporter.service

# View logs
sudo journalctl -u pg-plansight-exporter.service -f
```

### User and Permissions

The exporter runs as the `pg-plansight` user, which is automatically created during installation. Ensure your PostgreSQL log files are readable by this user:

```bash
# Add pg-plansight user to the postgres group. This is the correct way to
# grant log access — the package's postinst already does it when the group
# exists at install time.
sudo usermod -a -G postgres pg-plansight

# Verify the exporter can actually read a log file:
sudo -u pg-plansight head -c1 /var/log/postgresql/postgresql.log >/dev/null \
  && echo "log access OK"
```

> **Do not `chmod 644` the log files.** PostgreSQL query logs contain full SQL
> text including literal values (emails, tokens, personal data). Making them
> world-readable exposes that to every local user. Grant access via group
> membership as above; if the log directory's group is not `postgres`, adjust
> the group rather than the world bits.

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
sudo journalctl -u pg-plansight-exporter.service --no-pager -l

# Verify configuration
sudo -u pg-plansight pg-plansight-exporter --config /etc/pg-plansight-exporter/config.toml check-config

# Check file permissions
ls -la /etc/pg-plansight-exporter/
ls -la /var/log/postgresql/
```

#### Binary Not Found
```bash
# Refresh PATH
hash -r

# Check installation
dpkg -l | grep pg-plansight   # Debian/Ubuntu
rpm -qa | grep pg-plansight   # RHEL/Fedora

# Reinstall if necessary
sudo apt-get reinstall pg-plansight pg-plansight-exporter   # Debian/Ubuntu
sudo dnf reinstall pg-plansight pg-plansight-exporter       # RHEL/Fedora
```

## Uninstallation

### Debian/Ubuntu
```bash
# Remove packages but keep configuration
sudo apt-get remove pg-plansight pg-plansight-exporter

# Complete removal including configuration
sudo apt-get purge pg-plansight pg-plansight-exporter

# Clean up dependencies
sudo apt-get autoremove
```

### RHEL/Fedora/CentOS
```bash
# Remove packages
sudo dnf remove pg-plansight pg-plansight-exporter
# or
sudo rpm -e pg-plansight pg-plansight-exporter

# Manual cleanup (if needed)
sudo userdel pg-plansight
sudo rm -rf /etc/pg-plansight-exporter/
```

### PostgreSQL extension (removal / rollback)

> **Order matters. Removing the package first will prevent PostgreSQL from
> starting.**
>
> `shared_preload_libraries` is read at postmaster startup and a missing
> library is a **fatal** error. If you uninstall the extension package while
> `pg_plansight` is still listed there, the cluster will refuse to start on its
> next restart — including an unplanned one — with:
>
> ```
> FATAL:  could not access file "pg_plansight": No such file or directory
> ```

Roll back in this order:

```bash
# 1. Drop the extension in every database where it was created.
#    This removes the plansight schema and all captured data.
psql -d myapp -c 'DROP EXTENSION IF EXISTS pg_plansight;'

# 2. Remove pg_plansight from shared_preload_libraries.
#    Edit postgresql.conf (or the relevant include file) and drop it from the
#    list. Leave any other libraries in place.
sudo -u postgres psql -c 'SHOW shared_preload_libraries;'   # confirm what's set
sudo vi /etc/postgresql/16/main/postgresql.conf             # Debian/Ubuntu
# sudo vi /var/lib/pgsql/16/data/postgresql.conf            # RHEL-family

# 3. Restart PostgreSQL. shared_preload_libraries cannot be reloaded;
#    a restart is required for the change to take effect.
sudo systemctl restart postgresql

# 4. Confirm the library is no longer loaded, THEN remove the package.
sudo -u postgres psql -c 'SHOW shared_preload_libraries;'
sudo apt-get remove postgresql-16-plansight   # or: sudo dnf remove ...
```

To disable capture **without** uninstalling — the usual first step when
investigating a problem — you do not need a restart:

```sql
-- Takes effect immediately for new statements; no restart needed.
ALTER SYSTEM SET plansight.capture_mode = 'off';
SELECT pg_reload_conf();
```

That is the fastest rollback and should be your first move if the extension is
suspected in an incident. Leaving the library preloaded but inert has
negligible cost.

> If you drop the extension but leave it in `shared_preload_libraries`, the
> background worker keeps running and will log errors about missing tables in
> `plansight.database`. Set `plansight.capture_mode = 'off'` as above until you
> can schedule the restart.

## Alternative Installation Methods

### From Source
See [DEVELOPMENT.md](DEVELOPMENT.md) for building from source.

### Container Images

Container images are not published yet — see
[PRODUCTION_FEATURES.md](PRODUCTION_FEATURES.md). Build locally from source in
the meantime.

## Getting Help

- **Issues**: https://github.com/egeapak/pg-plansight/issues
- **Discussions**: https://github.com/egeapak/pg-plansight/discussions
- **Documentation**: https://github.com/egeapak/pg-plansight/tree/main/docs