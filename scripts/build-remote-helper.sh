#!/usr/bin/env bash
#
# Build the glibc-independent (statically linked, musl) limux CLI that Limux
# uploads to remote SSH hosts for agent notifications.
#
# Why: the ordinary build links dynamically against this machine's glibc, so the
# uploaded binary fails to even start on a remote whose glibc is older —
#   /lib64/libc.so.6: version `GLIBC_x.y' not found (required by .../limux)
# A musl build embeds its libc and depends on no system glibc at all, so it runs
# on any x86_64 Linux regardless of the remote's glibc version.
#
# Once built, `limux`'s remote provisioning (opt-in per SSH host) prefers this
# binary automatically — see layout_state::remote_helper_cli.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="x86_64-unknown-linux-musl"

cd "$ROOT_DIR"

# The musl std is a separate rustup component; add it if missing (no-op if present).
rustup target add "$TARGET"

cargo build -p limux-cli --release --target "$TARGET"

BIN="$ROOT_DIR/target/$TARGET/release/limux-cli"
echo
echo "Built glibc-independent remote helper:"
echo "  $BIN"
file "$BIN" 2>/dev/null || true
if command -v ldd >/dev/null 2>&1; then
    # Expected: "not a dynamic executable" / "statically linked".
    ldd "$BIN" 2>&1 | sed 's/^/  ldd: /' || true
fi
