# Default target (auto-detected from current platform)
default_target := `rustc -vV | grep host | cut -d' ' -f2`

# Build Debian packages for specified target (defaults to current platform)
build-deb target=default_target:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🔨 Building packages for target: {{target}}"
    
    # Determine if we need cross-compilation.
    #
    # PLANSIGHT_USE_CROSS=1 forces the containerised toolchain even when the
    # target matches the host. The release workflow sets it. A native build on
    # a modern runner links against that runner's glibc — the v0.1.0 amd64
    # packages came out needing 2.39 (Ubuntu 24.04) and so refused to install
    # on Debian 12, Ubuntu 22.04 or RHEL 9. Building every architecture in the
    # same old cross sysroot keeps one low, uniform glibc floor, which is what
    # the explicit `depends` in the crate manifests declare.
    current_target="{{default_target}}"
    if [ "{{target}}" != "$current_target" ] || [ "${PLANSIGHT_USE_CROSS:-0}" = "1" ]; then
        echo "📦 Building {{target}} with cross (host: $current_target)"
        cross build --release --locked --target {{target}} -p pg-plansight
        # The exporter advertises both backends; without the (non-default)
        # opentelemetry feature the shipped binary aborts on
        # backends=["opentelemetry"] configs.
        cross build --release --locked --target {{target}} -p pg-plansight-exporter --features opentelemetry
    else
        echo "📦 Building natively for {{target}}"
        cargo build --release --locked --target {{target}} -p pg-plansight
        cargo build --release --locked --target {{target}} -p pg-plansight-exporter --features opentelemetry
    fi
    
    echo "📋 Generating Debian packages..."

    # Create build directories for cargo-deb to find binaries
    mkdir -p crates/tui/build/release crates/exporter/build/release

    # Copy binaries from target-specific directory to build directory
    cp target/{{target}}/release/pg-plansight crates/tui/build/release/pg-plansight
    cp target/{{target}}/release/pg-plansight-exporter crates/exporter/build/release/pg-plansight-exporter

    # Use workspace target directory
    export CARGO_TARGET_DIR="$PWD/target"

    cargo deb --target {{target}} -p pg-plansight --no-build
    cargo deb --target {{target}} -p pg-plansight-exporter --no-build

    # Clean up build directories
    rm -rf crates/tui/build crates/exporter/build
    
    echo "✅ Packages built successfully!"
    echo "📂 Location: target/{{target}}/debian/"
    ls -la target/{{target}}/debian/

# List all supported target architectures
targets:
    @echo "Supported target architectures:"
    @echo "  x86_64-unknown-linux-gnu   (amd64) - Intel/AMD 64-bit"
    @echo "  aarch64-unknown-linux-gnu  (arm64) - ARM 64-bit (servers, modern ARM)"
    @echo "  armv7-unknown-linux-gnueabihf (armhf) - ARM 32-bit (Raspberry Pi, embedded)"
    @echo "  i686-unknown-linux-gnu     (i386)  - Intel/AMD 32-bit (legacy)"
    @echo ""
    @echo "Debian architecture mapping:"
    @echo "  x86_64-unknown-linux-gnu   → amd64"
    @echo "  aarch64-unknown-linux-gnu  → arm64"
    @echo "  armv7-unknown-linux-gnueabihf → armhf"
    @echo "  i686-unknown-linux-gnu     → i386"
    @echo ""
    @echo "Usage examples:"
    @echo "  just build-deb                          # Build DEB for current platform"
    @echo "  just build-rpm                          # Build RPM for current platform"
    @echo "  just build-all                          # Build both DEB and RPM"
    @echo "  just build-deb aarch64-unknown-linux-gnu   # Build DEB for ARM64"
    @echo "  just build-rpm x86_64-unknown-linux-gnu    # Build RPM for AMD64"
    @echo ""
    @echo "Validation and testing:"
    @echo "  just validate-deb <target>              # Validate DEB packages"
    @echo "  just validate-rpm                       # Validate RPM packages"
    @echo "  just all-deb <target>                   # Complete DEB workflow"
    @echo "  just all-packages <target>              # Complete DEB + RPM workflow"

# Clean all build artifacts and generated packages
clean:
    @echo "🧹 Cleaning build artifacts..."
    cargo clean
    @echo "✅ Clean complete!"

# Test the built DEB packages (requires packages to be built first)
test-deb target=default_target:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🧪 Testing packages for target: {{target}}"
    
    # Check if packages exist
    deb_dir="target/{{target}}/debian"
    if [ ! -d "$deb_dir" ]; then
        echo "❌ No packages found for {{target}}. Run 'just build {{target}}' first."
        exit 1
    fi
    
    echo "📋 Package information:"
    for deb in "$deb_dir"/*.deb; do
        if [ -f "$deb" ]; then
            echo "  📦 $(basename "$deb")"
            # Show package info if dpkg is available
            if command -v dpkg >/dev/null 2>&1; then
                dpkg --info "$deb" | grep -E "(Package|Version|Architecture|Description)"
            fi
            echo ""
        fi
    done
    
    echo "✅ Package validation complete!"


# Quick development build (native only, no packaging)
dev:
    @echo "🚀 Quick development build..."
    cargo build --release --locked
    @echo "✅ Development build complete!"

# Run tests before building packages
check:
    @echo "🔍 Running tests and checks..."
    cargo fmt --all -- --check
    cargo clippy --workspace --all-features --all-targets -- -D warnings
    cargo test --workspace --all-features
    @echo "✅ All checks passed!"

# Validate DEB package contents without dpkg dependencies
validate-deb target=default_target:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🔍 Validating packages for target: {{target}}"
    
    deb_dir="target/{{target}}/debian"
    if [ ! -d "$deb_dir" ]; then
        echo "❌ No packages found for {{target}}. Run 'just build {{target}}' first."
        exit 1
    fi
    
    for deb in "$deb_dir"/*.deb; do
        if [ -f "$deb" ]; then
            echo "📦 Validating $(basename "$deb")..."
            
            # Extract package info using ar and tar (available on most systems)
            temp_dir=$(mktemp -d)
            abs_deb=$(realpath "$deb")
            cd "$temp_dir"
            ar x "$abs_deb"
            
            if [ -f control.tar.* ]; then
                echo "  ✅ Control archive found"
                tar -tf control.tar.* 2>/dev/null | head -5 || true
            fi

            if [ -f data.tar.* ]; then
                echo "  ✅ Data archive found"
                echo "  📁 Package contents:"
                tar -tf data.tar.* 2>/dev/null | head -10 || true
            fi
            
            cd - > /dev/null
            rm -rf "$temp_dir"
            echo ""
        fi
    done
    
    echo "✅ Package validation complete!"

# Assert every shipped binary runs on the glibc its package claims to need.
#
# This is the check that would have caught the whole v0.1.0 dependency mess.
# The manifests declare a fixed `libc6 (>= X)` rather than letting
# dpkg-shlibdeps guess (see the comment above `depends` in crates/*/Cargo.toml),
# and a fixed number is only safe if something verifies it. Reads the ELF
# symbol versions out of the binary inside the .deb, so it checks exactly what
# ships rather than whatever is left in target/.
#
# WEAK undefined symbols are excluded on purpose. Rust's std references
# `pidfd_spawnp`/`pidfd_getpid`/`statx`/`getrandom` weakly: on a glibc that
# lacks them they resolve to NULL and std takes an older code path, so they do
# not raise the real floor. Anything mandatory is a strong symbol.
validate-glibc target=default_target:
    #!/usr/bin/env bash
    set -euo pipefail

    for tool in readelf dpkg-deb; do
        command -v "$tool" >/dev/null 2>&1 || {
            echo "❌ $tool is required (install binutils / dpkg)" >&2
            exit 1
        }
    done

    deb_dir="target/{{target}}/debian"
    if [ ! -d "$deb_dir" ]; then
        echo "❌ No packages found for {{target}}. Run 'just build-deb {{target}}' first." >&2
        exit 1
    fi

    checked=0
    for deb in "$deb_dir"/*.deb; do
        [ -f "$deb" ] || continue
        name=$(basename "$deb")

        declared=$(dpkg-deb -f "$deb" Depends \
            | tr ',' '\n' \
            | sed -n 's/.*libc6 *(>= *\([0-9][0-9.]*\)).*/\1/p' \
            | head -1)
        if [ -z "$declared" ]; then
            echo "❌ $name declares no libc6 minimum — dpkg cannot refuse a too-old system" >&2
            exit 1
        fi

        tmp=$(mktemp -d)
        trap 'rm -rf "$tmp"' EXIT
        dpkg-deb -x "$deb" "$tmp"

        while IFS= read -r bin; do
            actual=$(readelf --dyn-syms -W "$bin" 2>/dev/null \
                | grep -v ' WEAK ' \
                | grep -oE 'GLIBC_[0-9]+(\.[0-9]+)+' \
                | sed 's/GLIBC_//' \
                | sort -uV | tail -1)
            [ -n "$actual" ] || continue
            # sort -V puts the lower version first; if that is not `actual`,
            # the binary needs more than the package promises.
            lowest=$(printf '%s\n%s\n' "$actual" "$declared" | sort -V | head -1)
            if [ "$lowest" != "$actual" ]; then
                echo "❌ $name: $(basename "$bin") needs glibc $actual but the package declares >= $declared" >&2
                echo "   Either rebuild against an older sysroot (PLANSIGHT_USE_CROSS=1) or raise the floor" >&2
                echo "   in [package.metadata.deb] depends — and update docs/INSTALLATION.md to match." >&2
                exit 1
            fi
            echo "  ✅ $(basename "$bin"): needs glibc $actual, package declares >= $declared"
            checked=$((checked + 1))
        done < <(find "$tmp/usr/bin" -type f 2>/dev/null)

        rm -rf "$tmp"
        trap - EXIT
    done

    if [ "$checked" -eq 0 ]; then
        echo "❌ no binaries were checked — the packages in $deb_dir look empty" >&2
        exit 1
    fi
    echo "✅ glibc floor verified for $checked binaries ({{target}})"

# Install DEB packages locally for testing (requires sudo)
install-deb target=default_target:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "📥 Installing packages locally for {{target}}..."
    
    deb_dir="target/{{target}}/debian"
    if [ ! -d "$deb_dir" ]; then
        echo "❌ No packages found for {{target}}. Run 'just build {{target}}' first."
        exit 1
    fi
    
    # Check if we're on a compatible system
    if ! command -v dpkg >/dev/null 2>&1; then
        echo "❌ dpkg not available. This recipe only works on Debian/Ubuntu systems."
        exit 1
    fi
    
    echo "⚠️  This will install packages system-wide. Continue? (y/N)"
    read -r response
    if [[ ! "$response" =~ ^[Yy]$ ]]; then
        echo "Installation cancelled."
        exit 0
    fi
    
    for deb in "$deb_dir"/*.deb; do
        if [ -f "$deb" ]; then
            echo "📦 Installing $(basename "$deb")..."
            sudo dpkg -i "$deb" || {
                echo "📋 Fixing dependencies..."
                sudo apt-get install -f
            }
        fi
    done
    
    echo "✅ Local installation complete!"

# Uninstall DEB packages from local system
uninstall-deb:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🗑️  Uninstalling pg-plansight packages..."
    
    if ! command -v dpkg >/dev/null 2>&1; then
        echo "❌ dpkg not available. This recipe only works on Debian/Ubuntu systems."
        exit 1
    fi
    
    sudo apt-get remove --purge pg-plansight pg-plansight-exporter || echo "Some packages may not have been installed."
    echo "✅ Uninstallation complete!"

# Build RPM packages for specified target (defaults to current platform)
build-rpm target=default_target:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🔨 Building RPM packages for target: {{target}}"
    
    # Determine if we need cross-compilation.
    #
    # PLANSIGHT_USE_CROSS=1 forces the containerised toolchain even when the
    # target matches the host. The release workflow sets it. A native build on
    # a modern runner links against that runner's glibc — the v0.1.0 amd64
    # packages came out needing 2.39 (Ubuntu 24.04) and so refused to install
    # on Debian 12, Ubuntu 22.04 or RHEL 9. Building every architecture in the
    # same old cross sysroot keeps one low, uniform glibc floor, which is what
    # the explicit `depends` in the crate manifests declare.
    current_target="{{default_target}}"
    if [ "{{target}}" != "$current_target" ] || [ "${PLANSIGHT_USE_CROSS:-0}" = "1" ]; then
        echo "📦 Building {{target}} with cross (host: $current_target)"
        cross build --release --locked --target {{target}} -p pg-plansight
        # The exporter advertises both backends; without the (non-default)
        # opentelemetry feature the shipped binary aborts on
        # backends=["opentelemetry"] configs.
        cross build --release --locked --target {{target}} -p pg-plansight-exporter --features opentelemetry
    else
        echo "📦 Building natively for {{target}}"
        cargo build --release --locked --target {{target}} -p pg-plansight
        cargo build --release --locked --target {{target}} -p pg-plansight-exporter --features opentelemetry
    fi
    
    echo "📋 Generating RPM packages..."

    # Create build directories for cargo-generate-rpm to find binaries
    mkdir -p crates/tui/build/release crates/exporter/build/release

    # Copy binaries from target-specific directory to build directory
    cp target/{{target}}/release/pg-plansight crates/tui/build/release/pg-plansight
    cp target/{{target}}/release/pg-plansight-exporter crates/exporter/build/release/pg-plansight-exporter

    # auto-req is always disabled, not just when ldd is missing.
    #
    # cargo-generate-rpm's automatic requirement discovery shells out to ldd,
    # which cannot read a foreign-architecture binary — so on the release
    # runner it produced dependable output for the host target only, and
    # silently nothing for the three cross-built ones. Rather than ship
    # requirements that mean different things per architecture, none are
    # derived: the binaries need nothing beyond glibc and libgcc, and the
    # packages declare what they genuinely require via
    # [package.metadata.generate-rpm.requires]. The extension RPMs already
    # build this way. `just validate-glibc` covers the glibc floor.
    auto_req_flag="--auto-req disabled"

    # Use workspace target directory
    export CARGO_TARGET_DIR="$PWD/target"

    (cd crates/tui && cargo generate-rpm --target {{target}} $auto_req_flag)
    (cd crates/exporter && cargo generate-rpm --target {{target}} $auto_req_flag)

    # Clean up build directories
    rm -rf crates/tui/build crates/exporter/build

    echo "✅ RPM packages built successfully!"
    echo "📂 Location: target/{{target}}/generate-rpm/"
    ls -la target/{{target}}/generate-rpm/

# Build both DEB and RPM packages
build-all target=default_target: (build-deb target) (build-rpm target)
    @echo "🎉 Both DEB and RPM packages built for {{target}}!"

# Validate RPM packages
validate-rpm target=default_target:
    #!/usr/bin/env bash
    set -euo pipefail

    echo "🔍 Validating RPM packages for target: {{target}}..."

    rpm_dir="target/{{target}}/generate-rpm"
    if [ ! -d "$rpm_dir" ]; then
        echo "❌ No RPM packages found for {{target}}. Run 'just build-rpm {{target}}' first."
        exit 1
    fi

    for rpm in "$rpm_dir"/*.rpm; do
        if [ -f "$rpm" ]; then
            echo "📦 Validating $(basename "$rpm")..."

            # Show RPM package info if rpm command is available
            if command -v rpm >/dev/null 2>&1; then
                echo "  ℹ️  Package info:"
                rpm -qip "$rpm" | head -10
                echo ""
                echo "  📁 Package contents:"
                rpm -qlp "$rpm" | head -10
                echo ""
            else
                echo "  ⚠️  rpm command not available - basic validation only"
                file "$rpm"
                echo ""
            fi
        fi
    done

    echo "✅ RPM package validation complete!"

# Full workflow: check, build, and validate DEB packages
all-deb target=default_target: check (build-deb target) (validate-deb target)
    @echo "🎯 DEB workflow completed for {{target}}!"

# Full workflow for both DEB and RPM packages
all-packages target=default_target: check (build-all target) (validate-deb target) (validate-rpm target)
    @echo "🎯 Complete packaging workflow finished for {{target}}!"


# ===========================================================================
# PostgreSQL extension (pg_plansight) — build + package one deb/rpm per major.
# The extension is its own pgrx workspace, so these are separate from the
# binary package recipes above. Output (PGDG naming): crates/pg_extension/target/
# debian/postgresql-NN-plansight_<ver>_<arch>.deb and
# generate-rpm/plansight_NN-<ver>.<arch>.rpm.
# ===========================================================================

# Supported PostgreSQL majors.
ext_majors := "13 14 15 16 17 18"

# Stage the extension for one PG major via pgrx (host arch). Override pg_config
# for non-Debian layouts:  just ext-build 16 /opt/pg16/bin/pg_config
ext-build pg pg_config="":
    #!/usr/bin/env bash
    set -euo pipefail
    pgc="{{pg_config}}"; [ -n "$pgc" ] || pgc="/usr/lib/postgresql/{{pg}}/bin/pg_config"
    echo "📦 staging pg_plansight for PG{{pg}} ($pgc)"
    cd crates/pg_extension && cargo pgrx package --no-default-features --features "pg{{pg}}" --pg-config "$pgc"

# Build the .deb for one PG major (stages first).
ext-deb pg pg_config="": (ext-build pg pg_config)
    #!/usr/bin/env bash
    set -euo pipefail
    cd crates/pg_extension && cargo deb --no-build --variant "pg{{pg}}"
    ls -1 target/debian/*.deb 2>/dev/null | tail -1 || true

# Build the .rpm for one PG major (stages first). auto-req off so the package
# installs on any distro regardless of how its PostgreSQL server is named.
ext-rpm pg pg_config="": (ext-build pg pg_config)
    #!/usr/bin/env bash
    set -euo pipefail
    cd crates/pg_extension && cargo generate-rpm --variant "pg{{pg}}" --auto-req disabled
    ls -1 target/generate-rpm/*.rpm 2>/dev/null | tail -1 || true

# Both formats for one major.
ext-package pg pg_config="": (ext-deb pg pg_config) (ext-rpm pg pg_config)
    @echo "✅ PG{{pg}} packages in crates/pg_extension/target/{debian,generate-rpm}/"

# Both formats for every supported major (host arch).
ext-package-all:
    #!/usr/bin/env bash
    set -euo pipefail
    for pg in {{ext_majors}}; do just ext-package "$pg"; done
    echo "🎯 all majors packaged in crates/pg_extension/target/{debian,generate-rpm}/"

# Cross-arch: build + package inside a target-platform container (needs Docker
# Buildx + QEMU for non-host arches). Artifacts are written to
# crates/pg_extension/target/cross/<platform>/.
#   just ext-package-cross 16 linux/arm64
ext-package-cross pg platform:
    #!/usr/bin/env bash
    set -euo pipefail
    out="crates/pg_extension/target/cross/{{platform}}"
    mkdir -p "$out"
    docker buildx build --platform "{{platform}}" \
        -f crates/pg_extension/docker/Dockerfile.package \
        --build-arg PG_MAJOR="{{pg}}" \
        --target export --output "type=local,dest=$out" .
    echo "✅ {{platform}} PG{{pg}} packages in $out/"

# Run the extension's checks (fmt, clippy, cargo pgrx test) against a real
# PostgreSQL of the given major in Docker — same steps as the pgrx CI job, no
# local pgrx/PostgreSQL toolchain required.  just ext-test-docker 16
ext-test-docker pg="16":
    #!/usr/bin/env bash
    set -euo pipefail
    echo "🐘 building pgrx test image for PG{{pg}} (this compiles cargo-pgrx; first run is slow)"
    docker build -f crates/pg_extension/docker/Dockerfile.test \
        --build-arg PG_MAJOR="{{pg}}" -t pg_plansight_test:pg{{pg}} .
    echo "🧪 running fmt + clippy + pgrx test on PG{{pg}}"
    docker run --rm pg_plansight_test:pg{{pg}}

# ===========================================================================
# Changelog (git-cliff). Generates the release section for `version` from
# Conventional-Commit history since the last tag and splices it into
# CHANGELOG.md below [Unreleased], above the newest released entry. The curated
# 0.1.0 entry and the file header are left byte-for-byte untouched. Review the
# result and commit it.  Example:  just changelog 0.2.0
# NB: use the splice, NOT `git cliff --prepend`, which inserts above the header.
# ===========================================================================
changelog version:
    #!/usr/bin/env bash
    set -euo pipefail
    tag="v{{version}}"
    section="$(mktemp)"; out="$(mktemp)"
    trap 'rm -f "$section" "$out"' EXIT
    git cliff --config cliff.toml --unreleased --tag "$tag" > "$section"
    if ! grep -qE '^- ' "$section"; then
        echo "No Conventional-Commit changes since the last release — nothing to add for $tag." >&2
        exit 0
    fi
    # First released-version heading (## [x.y.z]); the new section goes just above it.
    n="$(grep -nE '^## \[[0-9]' CHANGELOG.md | head -1 | cut -d: -f1)"
    { head -n "$((n - 1))" CHANGELOG.md; cat "$section"; echo; tail -n "+$n" CHANGELOG.md; } > "$out"
    mv "$out" CHANGELOG.md
    trap 'rm -f "$section"' EXIT
    echo "✅ inserted the $tag section into CHANGELOG.md — review, then bump crate versions to {{version}} and commit."
