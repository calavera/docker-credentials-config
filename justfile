# List available recipes
default:
    @just --list

# Run all tests
test:
    cargo test

# Format code
fmt:
    cargo fmt

# Check formatting without modifying files
fmt-check:
    cargo fmt --check

# Run Clippy lints (fail on warnings)
lint:
    cargo clippy -- -D warnings

# Check the crate compiles without building
check:
    cargo check

# Build the crate
build:
    cargo build

# Run all CI checks locally (fmt, lint, test)
ci: fmt-check lint test

# Remove build artifacts
clean:
    cargo clean
