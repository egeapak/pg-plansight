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
    
    (cd crates/tui && cargo generate-rpm)
    (cd crates/exporter && cargo generate-rpm)
    
    # Clean up build directories
    rm -rf crates/tui/build crates/exporter/build
    
    echo "✅ RPM packages built successfully!"
    echo "📂 Location: target/generate-rpm/"
    ls -la target/generate-rpm/ || echo "No RPM packages found"

# Build both DEB and RPM packages
build-all target=default_target: (build-deb target) (build-rpm target)
    @echo "🎉 Both DEB and RPM packages built for {{target}}!"

# Validate RPM packages
validate-rpm:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🔍 Validating RPM packages..."
    
    if [ ! -d "target/generate-rpm" ]; then
        echo "❌ No RPM packages found. Run 'just build-rpm' first."
        exit 1
    fi
    
    for rpm in target/generate-rpm/*.rpm; do
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
all-packages target=default_target: check (build-all target) (validate-deb target) validate-rpm
    @echo "🎯 Complete packaging workflow finished for {{target}}!"

