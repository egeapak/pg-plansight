# Default target (auto-detected from current platform)
default_target := `rustc -vV | grep host | cut -d' ' -f2`

# Build Debian packages for specified target (defaults to current platform)
build-deb target=default_target:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🔨 Building packages for target: {{target}}"
    
    # Determine if we need cross-compilation
    current_target="{{default_target}}"
    if [ "{{target}}" != "$current_target" ]; then
        echo "📦 Cross-compiling from $current_target to {{target}}"
        cross build --release --target {{target}} -p pg-loganalyze
        cross build --release --target {{target}} -p pg-loganalyze-exporter
    else
        echo "📦 Building natively for {{target}}"
        cargo build --release --target {{target}} -p pg-loganalyze
        cargo build --release --target {{target}} -p pg-loganalyze-exporter
    fi
    
    echo "📋 Generating Debian packages..."

    # Create build directories for cargo-deb to find binaries
    mkdir -p crates/tui/build/release crates/exporter/build/release

    # Copy binaries from target-specific directory to build directory
    cp target/{{target}}/release/pg-loganalyze crates/tui/build/release/pg-loganalyze
    cp target/{{target}}/release/pg-loganalyze-exporter crates/exporter/build/release/pg-loganalyze-exporter

    # Use workspace target directory
    export CARGO_TARGET_DIR="$PWD/target"

    cargo deb --target {{target}} -p pg-loganalyze --no-build
    cargo deb --target {{target}} -p pg-loganalyze-exporter --no-build

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
    cargo build --release
    @echo "✅ Development build complete!"

# Run tests before building packages
check:
    @echo "🔍 Running tests and checks..."
    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings
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
    
    echo "🗑️  Uninstalling pg-loganalyze packages..."
    
    if ! command -v dpkg >/dev/null 2>&1; then
        echo "❌ dpkg not available. This recipe only works on Debian/Ubuntu systems."
        exit 1
    fi
    
    sudo apt-get remove --purge pg-loganalyze pg-loganalyze-exporter || echo "Some packages may not have been installed."
    echo "✅ Uninstallation complete!"

# Build RPM packages for specified target (defaults to current platform)
build-rpm target=default_target:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🔨 Building RPM packages for target: {{target}}"
    
    # Determine if we need cross-compilation
    current_target="{{default_target}}"
    if [ "{{target}}" != "$current_target" ]; then
        echo "📦 Cross-compiling from $current_target to {{target}}"
        cross build --release --target {{target}} -p pg-loganalyze
        cross build --release --target {{target}} -p pg-loganalyze-exporter
    else
        echo "📦 Building natively for {{target}}"
        cargo build --release --target {{target}} -p pg-loganalyze
        cargo build --release --target {{target}} -p pg-loganalyze-exporter
    fi
    
    echo "📋 Generating RPM packages..."

    # Create build directories for cargo-generate-rpm to find binaries
    mkdir -p crates/tui/build/release crates/exporter/build/release

    # Copy binaries from target-specific directory to build directory
    cp target/{{target}}/release/pg-loganalyze crates/tui/build/release/pg-loganalyze
    cp target/{{target}}/release/pg-loganalyze-exporter crates/exporter/build/release/pg-loganalyze-exporter

    # Disable auto-req when ldd is not available (e.g., cross-compiling from macOS)
    auto_req_flag=""
    if ! command -v ldd >/dev/null 2>&1; then
        auto_req_flag="--auto-req disabled"
    fi

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
# PostgreSQL extension (pg_loganalyze) — build + package one deb/rpm per major.
# The extension is its own pgrx workspace, so these are separate from the
# binary package recipes above. Output: crates/pg_extension/target/{debian,
# generate-rpm}/pg-loganalyze-pgNN_<version>_<arch>.{deb,rpm}.
# ===========================================================================

# Supported PostgreSQL majors.
ext_majors := "13 14 15 16 17 18"

# Stage the extension for one PG major via pgrx (host arch). Override pg_config
# for non-Debian layouts:  just ext-build 16 /opt/pg16/bin/pg_config
ext-build pg pg_config="":
    #!/usr/bin/env bash
    set -euo pipefail
    pgc="{{pg_config}}"; [ -n "$pgc" ] || pgc="/usr/lib/postgresql/{{pg}}/bin/pg_config"
    echo "📦 staging pg_loganalyze for PG{{pg}} ($pgc)"
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
