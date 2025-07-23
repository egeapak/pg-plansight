# Default target (auto-detected from current platform)
default_target := `rustc -vV | grep host | cut -d' ' -f2`

# Build Debian packages for specified target (defaults to current platform)
build target=default_target:
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
    @echo "  just build                              # Build for current platform"
    @echo "  just build aarch64-unknown-linux-gnu   # Build for ARM64"
    @echo "  just build x86_64-unknown-linux-gnu    # Build for AMD64"

# Clean all build artifacts and generated packages
clean:
    @echo "🧹 Cleaning build artifacts..."
    cargo clean
    @echo "✅ Clean complete!"

# Test the built packages (requires packages to be built first)
test target=default_target:
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

# Build packages for all supported architectures
build-all:
    @echo "🏗️  Building packages for all supported architectures..."
    just build x86_64-unknown-linux-gnu
    just build aarch64-unknown-linux-gnu
    just build armv7-unknown-linux-gnueabihf
    just build i686-unknown-linux-gnu
    @echo "🎉 All packages built successfully!"

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

# Validate package contents without dpkg dependencies
validate target=default_target:
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
                tar -tf control.tar.* | head -5
            fi
            
            if [ -f data.tar.* ]; then
                echo "  ✅ Data archive found"
                echo "  📁 Package contents:"
                tar -tf data.tar.* | head -10
            fi
            
            cd - > /dev/null
            rm -rf "$temp_dir"
            echo ""
        fi
    done
    
    echo "✅ Package validation complete!"

# Install packages locally for testing (requires sudo)
install-local target=default_target:
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

# Uninstall packages from local system
uninstall-local:
    #!/usr/bin/env bash
    set -euo pipefail
    
    echo "🗑️  Uninstalling pg-loganalyze packages..."
    
    if ! command -v dpkg >/dev/null 2>&1; then
        echo "❌ dpkg not available. This recipe only works on Debian/Ubuntu systems."
        exit 1
    fi
    
    sudo apt-get remove --purge pg-loganalyze pg-loganalyze-exporter || echo "Some packages may not have been installed."
    echo "✅ Uninstallation complete!"

# Full workflow: check, build, and test packages
all target=default_target: check (build target) (validate target)
    @echo "🎯 Full workflow completed for {{target}}!"