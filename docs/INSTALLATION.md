# Installation Guide

This guide covers installation of pg-plansight from pre-built packages for various Linux distributions.

## Quick Start

### Debian/Ubuntu (.deb packages)

Asset names carry a Debian revision after the version, so the file for 0.2.0 is
`pg-plansight_0.2.0-1_amd64.deb` — not `pg-plansight_0.2.0_amd64.deb`. Substitute
the whole `<version>-1` string below, or copy the link from the
[releases page](https://github.com/egeapak/pg-plansight/releases).

```bash
# Download the appropriate package for your architecture
wget https://github.com/egeapak/pg-plansight/releases/latest/download/pg-plansight_<version>-1_amd64.deb
wget https://github.com/egeapak/pg-plansight/releases/latest/download/pg-plansight-exporter_<version>-1_amd64.deb

# Install packages
sudo dpkg -i pg-plansight_<version>-1_amd64.deb pg-plansight-exporter_<version>-1_amd64.deb

# Fix dependencies if needed
sudo apt-get install -f

# Verify installation
pg-plansight --version
systemctl status pg-plansight-exporter.service
```

### RHEL/Fedora/CentOS (.rpm packages)

RPM assets carry a release number too: `pg-plansight-0.2.0-1.x86_64.rpm`.

```bash
# Download the appropriate package for your architecture
wget https://github.com/egeapak/pg-plansight/releases/latest/download/pg-plansight-<version>-1.x86_64.rpm
wget https://github.com/egeapak/pg-plansight/releases/latest/download/pg-plansight-exporter-<version>-1.x86_64.rpm

# Install packages (choose one method)

# Method 1: Using rpm (basic)
sudo rpm -i pg-plansight-<version>-1.x86_64.rpm pg-plansight-exporter-<version>-1.x86_64.rpm

# Method 2: Using dnf (recommended - handles dependencies)
sudo dnf install pg-plansight-<version>-1.x86_64.rpm pg-plansight-exporter-<version>-1.x86_64.rpm

# Method 3: Using yum (older systems)
sudo yum install pg-plansight-<version>-1.x86_64.rpm pg-plansight-exporter-<version>-1.x86_64.rpm

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

### libc requirement

| Package | Needs | Covers |
|---------|-------|--------|
| `pg-plansight`, `pg-plansight-exporter` | nothing (static musl binaries) | any Linux kernel 3.2+, including Alpine |
| `pg_plansight` (the PostgreSQL extension) | glibc **2.17** or newer | RHEL/CentOS 7+, Debian 9+, Ubuntu 16.04+ |

The CLI and exporter are statically linked against musl, so they run on any
distribution regardless of its C library, and the packages declare no libc
dependency. The extension is different: PostgreSQL loads it into its own
process, so it has to use the C library PostgreSQL itself is linked against.
Its module is compiled against each PostgreSQL major's own server headers,
inside an `ubuntu:18.04` container with PostgreSQL compiled from source, and
linked by zig against glibc 2.17 symbol stubs. The few newer glibc functions
Rust's standard library can use (`getrandom`, `copy_file_range`) are optional:
it calls them where they exist and falls back where they do not.

The extension floor is declared in its packages — `libc6 (>= 2.17)` for DEB,
`glibc >= 2.17` for RPM — so `dpkg` and `rpm` refuse an unsupported system
rather than installing a module that cannot load. Check what you have with
`ldd --version`.

Name resolution in the static exporter goes through musl, which reads
`/etc/hosts` and `/etc/resolv.conf` directly and ignores `/etc/nsswitch.conf`.
Hostnames that only resolve through an NSS module (LDAP, mDNS, or
`nss-resolve` without the `127.0.0.53` stub in `resolv.conf`) are not found;
use an IP address or a name DNS can answer.

> **v0.1.0 packages do not honour this and should not be used.** The CLI and
> exporter packages let `dpkg-shlibdeps` derive the dependency, which went
> wrong differently on each architecture: amd64 demanded `libc6 (>= 2.39)` and
> was *right* to (the binary really does fail to load on glibc 2.36), but only
> because it had been built on an Ubuntu 24.04 runner; the arm64 and armhf
> packages declared no libc dependency at all, because a cross-compiled binary
> gives `dpkg-shlibdeps` nothing to inspect; and the i386 package named
> `libc6-i386`/`lib32gcc-s1`, which exist only on amd64 multilib systems,
> making it uninstallable on real i386. The extension packages declared no libc
> dependency either, so they installed on Debian 12 and then could not be
> loaded — with `pg_plansight` in `shared_preload_libraries`, that stops the
> cluster from starting.
>
> From 0.2.0 the CLI and exporter are static musl binaries with no libc
> dependency, the extension is built in an Ubuntu 18.04 container with its floor
> fixed in the crate manifests, and the release workflow asserts the shipped
> binaries honour what they declare (`just validate-static`,
> `just validate-glibc-ext`).

### Distribution Support

The CLI and exporter packages install on any of these and on older or
non-glibc systems too. The extension needs glibc 2.17 or newer. The oldest
releases only have older PostgreSQL majors in PGDG's archives (frozen, so expect
stale minor releases): Ubuntu 18.04 and CentOS 7 up to PG15 (CentOS 7 aarch64
also PG16), Debian 9 up to PG14 and Ubuntu 16.04 up to PG13, both amd64 only.

#### Debian Family
- **Debian**: 9 (Stretch, glibc 2.24), 10 (Buster), 11 (Bullseye), 12 (Bookworm), 13 (Trixie)
- **Ubuntu**: 16.04 LTS (glibc 2.23), 18.04 LTS, 20.04 LTS, 22.04 LTS, 24.04 LTS
- **Linux Mint**: 20.x, 21.x, 22.x

#### Red Hat Family
- **RHEL**: 7.x (glibc 2.17), 8.x, 9.x, 10.x
- **Fedora**: 37 and newer
- **CentOS**: 7, 8, 9 (Stream)
- **Rocky Linux**: 8.x, 9.x
- **AlmaLinux**: 8.x, 9.x

The extension RPMs
install into the PGDG layout (`/usr/pgsql-N/lib`, `/usr/pgsql-N/share/extension`),
so they need PostgreSQL from the PGDG yum repository; the distro's own
`postgresql` packages (Fedora, the RHEL AppStream module) look elsewhere
(`/usr/lib64/pgsql`) and are not covered.

> RPM requirements are declared, not derived. `cargo-generate-rpm` derives them
> with `ldd`, which cannot read a cross-compiled binary — so it produced
> meaningful output for one architecture and silence for the other three.
> Automatic discovery is therefore disabled and requirements (the extension's
> glibc floor) are stated explicitly in the manifests, which is
> architecture-independent and does not need `ldd`.

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
- **Install**: built with `pgrx` (PostgreSQL 12–18) — see
  [PostgreSQL extension](#postgresql-extension-in-database-capture) below
- **Config**: `plansight.*` GUCs — see [CONFIGURATION.md](CONFIGURATION.md)

## PostgreSQL extension (in-database capture)

Separate from the TUI/exporter above, `pg_plansight` is also a **PostgreSQL
extension** that captures cumulative query statistics *inside the server* (like
`pg_stat_statements`), with no log file. Supported majors: **PostgreSQL 12–18**.

### Build & install

The extension is built with [`pgrx`](https://github.com/pgcentralfoundation/pgrx):

```bash
cargo install cargo-pgrx --locked --version "=0.19.1"
cargo pgrx init --pg16 "$(which pg_config)"     # or point at your server's pg_config

cd crates/pg_extension
# Compile + install into the cluster that owns that pg_config:
cargo pgrx install --release --no-default-features --features pg16 -c "$(which pg_config)"
```

Use the matching `--features pgNN` (`pg13`…`pg18`) and the
`postgresql-server-dev-NN` headers for your major. **PostgreSQL 12** builds from
`crates/pg_extension/pg12` instead (`cd crates/pg_extension/pg12`, install
`cargo-pgrx` `=0.13.1`, `--features pg12`): pgrx 0.19 dropped PG12, so it uses a
sibling manifest on pgrx 0.13.1 over the same sources. To build a throwaway server
image with the extension baked in (handy for trying it), see
`crates/pg_extension/docker/Dockerfile.bench`.

**Or build a deb/rpm to ship to a server** — `just ext-package 16` produces
versioned packages (`postgresql-16-plansight_<version>-1_<arch>.deb` /
`plansight_16-<version>-1.<arch>.rpm`) you copy and `dpkg -i` / `rpm -Uvh`. See
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

## Upgrading from 0.1.0

Three things need attention. Install the new packages first, then work through
the list.

### 1. The exporter config is repaired for you, but check the result

v0.1.0's **DEB** post-install script wrote
`/etc/pg-plansight-exporter/config.toml` with `[logs]` and `[database]` sections
that do not exist in the schema, so the service could never start. `dpkg`
preserves an existing config across an upgrade, so that file survives.

The v0.1.0 **RPM** had no maintainer scriptlets wired into its metadata at all,
so an RPM install of v0.1.0 created no config, no `pg-plansight` user, and never
enabled the service. Upgrading such an install gets a working config, but the
unit stays disabled — see the note below.

From 0.2.0 the post-install script validates the config with `check-config`.
If the installed binary rejects it, the file is moved to
`/etc/pg-plansight-exporter/config.toml.unusable-<timestamp>` and the shipped
default is put in its place. Nothing is deleted. Port any log paths or
thresholds you had set out of the backup:

```bash
ls /etc/pg-plansight-exporter/config.toml.unusable-*
sudo -u pg-plansight pg-plansight-exporter \
  --config /etc/pg-plansight-exporter/config.toml check-config
sudo systemctl restart pg-plansight-exporter
```

Note that 0.2.0 rejects unknown config keys outright, so a typo that was
silently ignored before now stops the daemon at startup. `check-config` names
the offending key.

> **On the way in from 0.1.0, check the service afterwards.** The fix ships
> *in* 0.2.0, and the scripts that run during this upgrade are partly v0.1.0's,
> so it cannot repair itself: on DEB, v0.1.0's `prerm` stops and disables the
> unit before the new `postinst` runs; on RPM, v0.1.0 never enabled it in the
> first place. The DEB upgrade prints this command. It applies to this one
> upgrade only:
>
> ```bash
> systemctl is-enabled pg-plansight-exporter; systemctl is-active pg-plansight-exporter
> sudo systemctl enable --now pg-plansight-exporter
> ```
>
> From 0.2.0 onward an upgrade restarts a running exporter and leaves a stopped
> one alone.

### 2. Dashboards and alerts need two edits

- `pg_plansight_logs_parsed_total` and `pg_plansight_parse_errors_total` now
  carry `log_path_pattern` (the configured glob) instead of `file_path` (the
  concrete filename). Anything selecting on `file_path` stops matching.
- Query hashes changed for statements containing `IN (...)` lists, because all
  literal lists now collapse to one placeholder regardless of length. Historical
  series for those queries will not line up across the upgrade.

`[pushgateway]` is accepted with a warning for this release only, then removed.
`filters.include_databases` now works, but needs `%d` in `log_parsing.log_line_prefix`
(set it to the server's `log_line_prefix`); without `%d` the config is rejected, because
every query would be attributed to `unknown`. `filters.exclude_databases` is new.

### 3. The extension needs ALTER EXTENSION, and a restart

```bash
# Install the new package for your major, then restart: the module is loaded
# at postmaster start via shared_preload_libraries.
sudo systemctl restart postgresql
```

```sql
-- In every database where the extension was created.
ALTER EXTENSION pg_plansight UPDATE;
```

Run it right after the restart: until then the new module's writes fail against
the 0.1.0 tables, so captures in that window are lost. The update:

- adds `database` and re-keys `plansight.statements` and `query_histogram` by
  `(fingerprint, database)`. Existing rows get `current_database()` (0.1.0 hook
  capture only persisted its own database), and rows that now share a fingerprint
  (the normalizer collapses `IN` lists of different lengths) are merged;
- adds `plansight_fingerprint()`, the `plansight.metrics*` views and the
  `metrics_window` table;
- rewrites both tables under an exclusive lock, so expect it to take time on a large
  store;
- runs the configuration check: warnings are printed, and an error-level finding
  (capture is on but cannot work, for example `plansight.database` points at another
  database) aborts the update until the setting is fixed.

The capture defaults changed in 0.2.0 — `plansight.min_duration_ms` is now `1`
(was `0`) and `plansight.sample_rate` is `0.8` (was `1.0`). Set both explicitly
if you were relying on exhaustive capture.

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