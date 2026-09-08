# Packaging the extension (deb / rpm)

Build a versioned `.deb`/`.rpm` of the `pg_plansight` **PostgreSQL extension**,
one per PostgreSQL major, copy it to a server, and install. (This is separate
from the TUI/exporter binary packages — those use the `build-deb`/`build-rpm`
recipes.)

## How it works

`cargo pgrx package` stages the real install tree for a given major:

```
crates/pg_extension/target/release/pg_plansight-pg16/
  usr/lib/postgresql/16/lib/pg_plansight.so
  usr/share/postgresql/16/extension/pg_plansight.control
  usr/share/postgresql/16/extension/pg_plansight--<version>.sql
```

The `just ext-*` recipes then wrap that tree with **cargo-deb** / **cargo-generate-rpm**
using the per-major `[package.metadata.deb.variants.pgNN]` /
`[…generate-rpm.variants.pgNN]` config in `crates/pg_extension/Cargo.toml`. The
globbed assets pick up the `.control`, the install SQL, and any future
`pg_plansight--<old>--<new>.sql` upgrade scripts automatically.

## Prerequisites (build host)

```bash
cargo install cargo-pgrx --locked --version "=0.19.1"
cargo install cargo-deb cargo-generate-rpm
# the target major's server headers + a pgrx init against them:
sudo apt-get install -y postgresql-server-dev-16    # PGDG/Debian
cargo pgrx init --pg16 /usr/lib/postgresql/16/bin/pg_config
```

## Build (host architecture)

```bash
just ext-package 16          # -> crates/pg_extension/target/{debian,generate-rpm}/
just ext-package-all         # all of PG 13..18
# individual formats:
just ext-deb 16
just ext-rpm 16
# non-Debian pg_config layout:
just ext-build 16 /opt/pg16/bin/pg_config
```

Output (verified end-to-end on PG16/amd64):

```
crates/pg_extension/target/debian/postgresql-16-plansight_0.2.0-1_amd64.deb
crates/pg_extension/target/generate-rpm/plansight_16-0.2.0-1.x86_64.rpm
```

The `.deb` declares `Depends: postgresql-16`; the `.rpm` declares no hard
PostgreSQL requirement (server package names vary by distro — `postgresqlNN-server`
on PGDG vs `postgresql-server` on base) so it installs into any existing PG 16
cluster.

## Install on the server

```bash
# Debian/Ubuntu (PGDG):
sudo dpkg -i postgresql-16-plansight_0.2.0-1_amd64.deb
# RHEL/Fedora/Rocky/Alma:
sudo rpm -Uvh plansight_16-0.2.0-1.x86_64.rpm

# then enable + create (see below for why the restart):
echo "shared_preload_libraries = 'pg_plansight'" | sudo tee -a /etc/postgresql/16/main/postgresql.conf
echo "plansight.capture_mode = 'hook'"           | sudo tee -a /etc/postgresql/16/main/postgresql.conf
sudo systemctl restart postgresql@16-main
sudo -u postgres psql -c "CREATE EXTENSION pg_plansight;"
```

## Cross-architecture (ARM64, etc.)

`cargo pgrx` builds for the host arch, and the extension's `.so` needs the
**target** arch's PostgreSQL headers + clang — so `cross` (which the binary
packages use) is not sufficient. Instead, build inside a target-platform
container with Docker Buildx + QEMU:

```bash
just ext-package-cross 16 linux/arm64
# artifacts -> crates/pg_extension/target/cross/linux/arm64/
```

This runs `crates/pg_extension/docker/Dockerfile.package` (the same
`pgrx package` + cargo-deb/cargo-generate-rpm pipeline, in a `--platform`
container; `--build-arg EXTRA_CA=…` is available for proxied networks). The
host-arch pipeline is verified end-to-end; the cross-arch container path reuses
the proven `Dockerfile.bench` build steps and is intended to run in CI/Buildx.

## Updating an installed extension

There are two distinct update paths — which one applies depends on whether the
**SQL surface** changed (not just the `.so`):

1. **Binary-only update (same extension version).** A `.so` change with no
   new/changed SQL objects — e.g. a bug fix, or *new GUCs* (they're registered in
   `_PG_init`, not SQL objects). **Install the new package and restart
   PostgreSQL.** The worker + executor hooks load at postmaster start via
   `shared_preload_libraries`, so a restart (not just reconnect) is required;
   new GUCs appear after it. **No `ALTER EXTENSION` needed.**

2. **SQL-surface update → new extension version.** New/changed tables, views, or
   functions. Bump the crate version (→ `.control` `default_version`), ship an
   upgrade script `pg_plansight--<old>--<new>.sql` (it gets packaged
   automatically by the globbed assets), install, restart for the new `.so`, then
   in each database:

   ```sql
   ALTER EXTENSION pg_plansight UPDATE;   -- applies the migration chain
   ```

   > **Status:** upgrade scripts start at 0.2.0
   > (`sql/pg_plansight--0.1.0--0.2.0.sql`, empty because no SQL object changed
   > between those versions). A script is required for *every* version bump,
   > not only ones that change SQL: `default_version` tracks the crate version,
   > so without a path PostgreSQL answers `ALTER EXTENSION … UPDATE` with "no
   > update path from version X to version Y" and the only way forward is
   > `DROP EXTENSION`, which discards every captured statistic.
   > `version-check.yml` fails the build when a version has no script targeting
   > it, so this cannot be forgotten again.

`pg_config`-reported paths are baked into the package per major, so a package
built for PG 16 installs only into a PG 16 cluster.
