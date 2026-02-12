#!/usr/bin/env bash
set -euo pipefail

echo "🦀 Building voice-irc..."

# Check if Rust is installed
if ! command -v cargo &> /dev/null; then
    echo "❌ Cargo not found. Install Rust from https://rustup.rs"
    exit 1
fi

# Build release binary
echo "📦 Compiling optimized binary..."
cargo build --release

# Check if build succeeded
if [ -f "target/release/voice-irc" ]; then
    echo "✅ Build successful!"
    echo ""
    echo "Binary location: target/release/voice-irc"
    echo "Size: $(du -h target/release/voice-irc | cut -f1)"
    echo ""
    echo "Usage:"
    echo "  ./target/release/voice-irc --server irc.libera.chat:6697 --channel \"#test\" --nickname yourname"
    echo ""
    echo "Or install globally:"
    echo "  sudo cp target/release/voice-irc /usr/local/bin/"
else
    echo "❌ Build failed"
    exit 1
fi
