#!/usr/bin/env bash
# Shared by the in-container package tests: distro detection, PostgreSQL
# provisioning from PGDG, and the pass/fail helpers. Sourced, not run.
set -uo pipefail
PG="${PG:?}"
. /etc/os-release
case "$ID" in
    ubuntu | debian) FAMILY=deb ;;
    *) FAMILY=rpm ;;
esac

if [ "$FAMILY" = deb ]; then
    PGBIN="/usr/lib/postgresql/$PG/bin"
    LIBDIR="/usr/lib/postgresql/$PG/lib"
    EXTDIR="/usr/share/postgresql/$PG/extension"
else
    PGBIN="/usr/pgsql-$PG/bin"
    LIBDIR="/usr/pgsql-$PG/lib"
    EXTDIR="/usr/pgsql-$PG/share/extension"
fi
PGDATA=/var/lib/pgtest/data
PGLOGDIR=/var/log/pgtest
PGLOG=$PGLOGDIR/pg.log
WORKLOAD=7

failures=0
pass() { printf '  ok   %s\n' "$1"; }
fail() { printf '  FAIL %s\n' "$1"; failures=$((failures + 1)); }
check() { # check <description> <command...>
    local what="$1"
    shift
    if "$@" >/dev/null 2>&1; then pass "$what"; else fail "$what"; fi
}
step() { printf '\n== %s\n' "$1"; }
sql() { su postgres -c "$PGBIN/psql -v ON_ERROR_STOP=1 -X -At -d postgres -c \"$1\""; }

# ---------------------------------------------------------------- provision
step "provision ($ID $VERSION_ID, PG$PG, $FAMILY)"
if [ "$FAMILY" = deb ]; then
    export DEBIAN_FRONTEND=noninteractive
    if [ "${VERSION_CODENAME:-}" = buster ]; then
        # Debian 10 is EOL; its mirrors moved to archive.debian.org.
        sed -i 's|deb.debian.org|archive.debian.org|; s|security.debian.org|archive.debian.org|; /buster-updates/d' /etc/apt/sources.list
        echo 'Acquire::Check-Valid-Until "false";' > /etc/apt/apt.conf.d/99archive
    fi
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends curl ca-certificates gnupg locales python3 >/dev/null
    curl -fsSL https://www.postgresql.org/media/keys/ACCC4CF8.asc | apt-key add - >/dev/null 2>&1 \
        || { curl -fsSL https://www.postgresql.org/media/keys/ACCC4CF8.asc | gpg --dearmor -o /etc/apt/trusted.gpg.d/pgdg.gpg; }
    codename="${VERSION_CODENAME:-$(. /etc/os-release; echo "$UBUNTU_CODENAME")}"
    case "$codename" in
        bionic | buster) repo="https://apt-archive.postgresql.org/pub/repos/apt" ;; # past PGDG main support
        *) repo="https://apt.postgresql.org/pub/repos/apt" ;;
    esac
    echo "deb $repo $codename-pgdg main" > /etc/apt/sources.list.d/pgdg.list
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends "postgresql-$PG" >/dev/null
else
    dnf -qy install python3 curl >/dev/null
    major="${VERSION_ID%%.*}"
    arch="$(uname -m)"
    dnf -qy install "https://download.postgresql.org/pub/repos/yum/reporpms/EL-$major-$arch/pgdg-redhat-repo-latest.noarch.rpm" >/dev/null
    dnf -qy module disable postgresql >/dev/null 2>&1 || true
    if ! dnf -qy install "postgresql$PG-server" "postgresql$PG-contrib" >/dev/null 2>&1; then
        # PGDG's live yum repo drops EOL majors (PG12); they remain in the archive
        # as bare rpms without repodata.
        base="https://yum-archive.postgresql.org/$PG/redhat/rhel-$major-$arch"
        ver="$(curl -fsSL "$base/" | grep -o "postgresql$PG-server-[0-9][^\"]*\.rpm" | sort -V | tail -1 | sed "s/postgresql$PG-server-//; s/\.rpm//")"
        dnf -qy install $(for n in "" -libs -server -contrib; do printf '%s ' "$base/postgresql$PG$n-$ver.rpm"; done) >/dev/null
    fi
fi
ls "$PGBIN/postgres" >/dev/null || { echo "no server binary at $PGBIN"; exit 2; }


start_cluster() { # start_cluster <preload-libraries>
    mkdir -p "$PGLOGDIR" /var/lib/pgtest
    chown postgres "$PGLOGDIR" /var/lib/pgtest
    [ -d "$PGDATA" ] || su postgres -c "$PGBIN/initdb -D $PGDATA -A trust >/dev/null"
    sed -i '/^# pkgtest begin/,/^# pkgtest end/d' "$PGDATA/postgresql.conf"
    cat >> "$PGDATA/postgresql.conf" <<CONF
# pkgtest begin
shared_preload_libraries = '$1'
logging_collector = on
log_directory = '$PGLOGDIR'
log_filename = 'pg.log'
log_line_prefix = '%m [%p] '
auto_explain.log_min_duration = 0
auto_explain.log_analyze = on
auto_explain.log_format = 'text'
plansight.capture_mode = 'hook'
plansight.sample_rate = 1.0
plansight.min_duration_ms = 0
plansight.flush_interval = 1
port = 5432
unix_socket_directories = '/tmp'
# pkgtest end
CONF
    su postgres -c "$PGBIN/pg_ctl -D $PGDATA -l $PGLOGDIR/ctl.log -w start" >/dev/null
}
stop_cluster() { su postgres -c "$PGBIN/pg_ctl -D $PGDATA -m fast -w stop" >/dev/null; }
export PGHOST=/tmp
