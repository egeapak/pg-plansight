# Package Development Guide

This guide covers building, testing, and developing packages for pg-plansight.

## Prerequisites

### Required Tools
```bash
# Install Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Install packaging tools
cargo install cargo-deb cargo-generate-rpm cross just

# Verify installations
cargo --version
just --version
cross --version
cargo deb --version
cargo generate-rpm --version
```

### System Dependencies

#### For DEB packaging (Debian/Ubuntu):
```bash
sudo apt-get update
sudo apt-get install build-essential pkg-config libssl-dev dpkg-dev
```

#### For RPM packaging (RHEL/Fedora):
```bash
# Fedora
sudo dnf install gcc pkg-config openssl-devel rpm-build

# RHEL/CentOS
sudo yum install gcc pkg-config openssl-devel rpm-build
```

## Quick Start

### Building Packages
```bash
# Clone the repository
git clone https://github.com/egeapak/pg-plansight.git
cd pg-plansight

# Build packages for current platform
just build-all

# Build for specific architecture
just build-deb x86_64-unknown-linux-gnu
just build-rpm aarch64-unknown-linux-gnu

# Validate packages
just validate-deb
just validate-rpm
```

## Available Commands

### Core Build Commands
```bash
just build-deb [target]    # Build DEB packages
just build-rpm [target]    # Build RPM packages  
just build-all [target]    # Build both DEB and RPM packages
```

### Development Commands
```bash
just check                 # Run tests and linting
just dev                   # Quick development build
just clean                 # Clean build artifacts
just targets               # List supported architectures
```

### Validation and Testing
```bash
just validate-deb [target] # Validate DEB packages
just validate-rpm          # Validate RPM packages
just test-deb [target]     # Test DEB package contents
```

### Installation Commands
```bash
just install-deb [target]  # Install DEB packages locally (requires sudo)
just uninstall-deb         # Remove DEB packages from system
```

### Complete Workflows
```bash
just all-deb [target]      # Complete DEB workflow (check + build + validate)
just all-packages [target] # Complete workflow for both DEB and RPM
```

## Supported Architectures

| Target Triple | Debian Arch | RPM Arch | Description |
|---------------|-------------|----------|-------------|
| `x86_64-unknown-linux-gnu` | amd64 | x86_64 | Intel/AMD 64-bit |
| `aarch64-unknown-linux-gnu` | arm64 | aarch64 | ARM 64-bit |
| `armv7-unknown-linux-gnueabihf` | armhf | armv7hl | ARM 32-bit |
| `i686-unknown-linux-gnu` | i386 | i686 | Intel/AMD 32-bit |

## Package Structure

### Workspace Layout
```
pg-plansight/
├── Cargo.toml              # Workspace configuration
├── Cross.toml              # Cross-compilation settings
├── LICENSE                 # MIT license
├── justfile                # Build automation
├── crates/
│   ├── core/              # Core parsing library
│   ├── tui/               # TUI application
│   │   ├── Cargo.toml     # DEB/RPM metadata
│   │   └── src/
│   └── exporter/          # Prometheus exporter
│       ├── Cargo.toml     # DEB/RPM metadata
│       ├── debian/        # DEB-specific scripts
│       ├── rpm/           # RPM-specific scripts
│       └── systemd/       # Systemd service files
└── docs/                  # Documentation
```

### Core crate features (`crates/core`)
The `pg-plansight-core` library is feature-gated so it can be embedded in a
PostgreSQL backend without heavy/unsafe dependencies. All three are **on by
default** (full CLI/TUI/exporter behaviour); the pgrx extension depends on core
with `default-features = false` to drop all of them:

| Feature | Enables | Dependency dropped when off |
|---------|---------|------------------------------|
| `parallel` | rayon-based parallel grouping / multi-file parsing | `rayon` |
| `file-io` | filesystem reads, gzip/bzip2, path globbing, JSON export, hostname | `flate2`, `bzip2`, `hostname` |
| `regression-analysis` | statistical (Student's-t) regression detector | `statrs` |

The regex `unicode` feature is intentionally left on everywhere: the parser
patterns use `\d`/`\s`/`\w`, which the regex crate only compiles with
`unicode-perl` (otherwise `Regex::new` errors and the `LazyLock` constructors
panic on first parse). Dropping it would save ~0.5 MB in the extension `.so` but
risk a runtime panic inside a backend, so it is not a supported configuration.

Without `regression-analysis` the basic heuristic regression engine
(`BasicRegressionEngine`) is still available — only the statistical detector is
compiled out. Build the embeddable lib with
`cargo build -p pg-plansight-core --no-default-features` (optionally re-adding
individual features).

### Package Metadata

Both DEB and RPM metadata are configured in each crate's `Cargo.toml`:

```toml
[package.metadata.deb]
maintainer = "Ege Apak <ege@apak.dev>"
copyright = "2025, Ege Apak <ege@apak.dev>"
license-file = ["../../LICENSE", "0"]
extended-description = "..."
depends = "$auto"
section = "database"
priority = "optional"
assets = [
    ["build/release/binary-name", "usr/bin/", "755"],
    ["../../LICENSE", "usr/share/doc/package-name/", "644"],
]

[package.metadata.generate-rpm]
assets = [
    { source = "build/release/binary-name", dest = "/usr/bin/binary-name", mode = "755" },
]
```

## Cross-Compilation

### Configuration

Cross-compilation is configured in `Cross.toml`:

```toml
[target.aarch64-unknown-linux-gnu]
image = "ghcr.io/cross-rs/cross:aarch64-unknown-linux-gnu"

[target.armv7-unknown-linux-gnueabihf]
image = "ghcr.io/cross-rs/cross:armv7-unknown-linux-gnueabihf"

[target.i686-unknown-linux-gnu]
image = "ghcr.io/cross-rs/cross:i686-unknown-linux-gnu"
```

### Building for Different Architectures

```bash
# Native build (current platform)
just build-all

# Cross-compile for ARM64
just build-all aarch64-unknown-linux-gnu

# Cross-compile for ARM32 (Raspberry Pi)
just build-all armv7-unknown-linux-gnueabihf

# Cross-compile for x86 32-bit
just build-all i686-unknown-linux-gnu
```

## Package Content

### DEB Package Structure
```
pkg-name_version_arch.deb
├── DEBIAN/
│   ├── control           # Package metadata
│   ├── postinst         # Post-installation script
│   ├── prerm            # Pre-removal script
│   └── postrm           # Post-removal script
└── usr/
    ├── bin/
    │   └── binary-name   # Main executable
    └── share/
        └── doc/
            └── LICENSE   # License file
```

### RPM Package Structure
```
pkg-name-version.arch.rpm
├── /usr/bin/binary-name               # Main executable
├── /usr/lib/systemd/system/service    # Systemd service (exporter only)
└── /etc/package-name/                 # Configuration directory (exporter only)
```

## Systemd Integration

### Service Configuration (Exporter only)

The exporter package includes systemd integration:

```ini
[Unit]
Description=Plansight Prometheus Exporter
Documentation=https://github.com/egeapak/pg-plansight
After=network.target
Wants=network.target

[Service]
Type=simple
User=pg-plansight
Group=pg-plansight
ExecStart=/usr/bin/pg-plansight-exporter --config /etc/pg-plansight-exporter/config.toml
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
```

### User Management

Both DEB and RPM packages create a dedicated system user:

```bash
# Created during installation
useradd --system --shell /bin/false --home-dir /var/lib/pg-plansight pg-plansight
```

## Testing

### Package Validation

```bash
# Validate package contents
just validate-deb x86_64-unknown-linux-gnu
just validate-rpm

# Test installation (requires sudo)
just install-deb x86_64-unknown-linux-gnu

# Verify installation
pg-plansight --version
pg-plansight-exporter --help
systemctl status pg-plansight-exporter.service

# Clean up
just uninstall-deb
```

### Manual Testing

```bash
# Extract and inspect DEB package
dpkg --contents target/x86_64-unknown-linux-gnu/debian/pg-plansight_*.deb
dpkg --info target/x86_64-unknown-linux-gnu/debian/pg-plansight_*.deb

# Extract and inspect RPM package  
rpm -qlp target/generate-rpm/pg-plansight-*.rpm
rpm -qip target/generate-rpm/pg-plansight-*.rpm
```

## CI/CD Integration

### GitHub Actions

The project includes comprehensive CI/CD with GitHub Actions:

```yaml
# .github/workflows/packages.yml
- Builds packages for all supported architectures
- Tests package installation on Ubuntu (DEB) and Fedora (RPM)
- Creates releases with all package artifacts
- Generates installation documentation
```

### Local CI Simulation

```bash
# Run the same checks as CI
just check                    # Tests and linting
just build-all               # Build packages
just validate-deb            # Validate DEB packages
just validate-rpm            # Validate RPM packages
```

## Troubleshooting

### Common Build Issues

#### Cross-compilation failures
```bash
# Update cross images
docker pull ghcr.io/cross-rs/cross:aarch64-unknown-linux-gnu

# Clean and retry
just clean
just build-all aarch64-unknown-linux-gnu
```

#### cargo-deb binary not found
```bash
# The build system uses a temporary build folder approach
# This is automatically handled by justfile recipes
ls -la crates/tui/build/release/     # Should be empty when not building
ls -la crates/exporter/build/release/ # Should be empty when not building
```

#### RPM generation UTF-8 errors
```bash
# This may occur on macOS - build on Linux instead
# Or use GitHub Actions for RPM builds
```

### Package Issues

#### Dependencies not resolved
```bash
# DEB packages
sudo apt-get update
sudo apt-get install -f

# RPM packages  
sudo dnf check
sudo dnf install --skip-broken
```

#### Service won't start
```bash
# Check systemd service
sudo systemctl status pg-plansight-exporter.service
sudo journalctl -u pg-plansight-exporter.service

# Verify user exists
id pg-plansight

# Check file permissions
ls -la /etc/pg-plansight-exporter/
```

## Contributing

### Development Workflow

1. **Fork and clone** the repository
2. **Create a feature branch**: `git checkout -b feature/new-packaging`
3. **Make changes** to package configuration
4. **Test locally**: `just all-packages`
5. **Validate packages**: `just validate-deb && just validate-rpm`
6. **Submit a pull request**

### Package Configuration Changes

When modifying package metadata:

1. Update `Cargo.toml` in the appropriate crate
2. Update maintainer scripts in `debian/` or `rpm/` directories
3. Test on both DEB and RPM systems
4. Update documentation if needed

### Adding New Architectures

1. Add target to `Cross.toml` if cross-compilation is needed
2. Update GitHub Actions matrix in `.github/workflows/packages.yml`
3. Test the new architecture builds
4. Update documentation

## Release Process

### Creating a Release

1. **Update version** in all `Cargo.toml` files
2. **Test thoroughly**: `just all-packages`
3. **Commit changes**: `git commit -m "Bump version to X.Y.Z"`
4. **Create tag**: `git tag -a vX.Y.Z -m "Release vX.Y.Z"`
5. **Push tag**: `git push origin vX.Y.Z`
6. **GitHub Actions** will automatically build and create the release

### Release Artifacts

Each release includes:
- DEB packages for all supported architectures
- RPM packages for all supported architectures
- Source code archives
- Checksums and signatures
- Release notes

## Resources

- **Cargo Book**: https://doc.rust-lang.org/cargo/
- **cargo-deb**: https://github.com/kornelski/cargo-deb
- **cargo-generate-rpm**: https://github.com/cat-in-136/cargo-generate-rpm
- **Cross**: https://github.com/cross-rs/cross
- **Just**: https://github.com/casey/just