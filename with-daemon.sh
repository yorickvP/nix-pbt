#!/usr/bin/env bash
# Run a command against a private Nix daemon of a given version, without
# root, e.g. to tell client bugs from daemon bugs:
#
#   ./with-daemon.sh ../nix/result -- ./target/debug/nix-pbt-eval '...'
#
# In a user and mount namespace, /nix/store is replaced by a fresh directory
# with only the closures of: DAEMON, what /, /bin, /usr/bin, /etc and $PATH
# point into the store, the `result*` links in the current directory, and
# $WITH_DAEMON_ROOTS. It's
# served by DAEMON/bin/nix-daemon with a fresh /nix/var/nix, so every path
# the command adds is new to the daemon (the closures themselves aren't
# valid paths in its database).
#
# The scratch directory (the new store and state) is deleted afterwards
# unless WITH_DAEMON_KEEP is set.
#
# DAEMON_CONFIG adds nix.conf lines for the daemon. The client is root in the
# namespace, so it is trusted unless DAEMON_CONFIG sets `trusted-users =`.
# Needs unprivileged user namespaces and util-linux.
set -euo pipefail

daemon=$(realpath "$1")
shift
[ "${1:-}" = -- ] && shift

util_linux=${UTIL_LINUX:-$(nix-build '<nixpkgs>' -A util-linux.bin --no-out-link 2>/dev/null)}
scratch=$(mktemp -d "${NIX_PBT_TMPDIR:-${TMPDIR:-/tmp}}/nix-pbt-daemon-XXXXXX")
mkdir -p "$scratch"/{store,var,cache}

store_paths() { grep -o '^/nix/store/[^/]*' || true; }
roots=$(
    {
        echo "$daemon" "$util_linux" | tr ' ' '\n'
        # Both the link's own target and where it ends up: /bin/sh points
        # into one store path, which points into another.
        for l in /* /bin/* /usr/bin/* $(find /etc -maxdepth 3 -type l 2>/dev/null); do
            [ -L "$l" ] && readlink "$l" && readlink -f "$l"
        done
        tr : '\n' <<<"$PATH"
        for r in result*; do [ -e "$r" ] && readlink -f "$r"; done
        tr ' ' '\n' <<<"${WITH_DAEMON_ROOTS:-}" | xargs -r readlink -f
    } | store_paths | sort -u
)
# shellcheck disable=SC2086
nix-store -qR $roots > "$scratch/closure"

"$util_linux/bin/unshare" --user --map-root-user --mount --fork \
    env PATH="$util_linux/bin:$PATH" SCRATCH="$scratch" DAEMON="$daemon" \
    DAEMON_CONFIG="${DAEMON_CONFIG:-}" \
    bash -euo pipefail -c '
# Mount the closures into the new store, then the new store over the old.
while read -r path; do
    target="$SCRATCH/store/${path#/nix/store/}"
    if [ -d "$path" ]; then mkdir "$target"; else touch "$target"; fi
    mount --bind "$path" "$target"
done < "$SCRATCH/closure"
mount --rbind "$SCRATCH/store" /nix/store
mount --bind "$SCRATCH/var" /nix/var/nix

# As root in the namespace, but with no build users and nothing to build.
NIX_REMOTE=local NIX_CONFIG="build-users-group =
sandbox = false
experimental-features = nix-command flakes
substituters =
$DAEMON_CONFIG" \
    "$DAEMON/bin/nix-daemon" > "$SCRATCH/daemon.log" 2>&1 &
daemon_pid=$!
trap "kill $daemon_pid" EXIT
for _ in $(seq 100); do
    [ -S /nix/var/nix/daemon-socket/socket ] && break
    sleep 0.05
done
[ -S /nix/var/nix/daemon-socket/socket ] || { cat "$SCRATCH/daemon.log" >&2; exit 1; }

export NIX_REMOTE=daemon XDG_CACHE_HOME="$SCRATCH/cache"
"$@" && status=0 || status=$?
echo "with-daemon.sh: $("$DAEMON/bin/nix-daemon" --version | head -1)" >&2
exit $status
' with-daemon "$@" && status=0 || status=$?
if [ -z "${WITH_DAEMON_KEEP:-}" ]; then
    chmod -R u+w "$scratch"
    rm -rf "$scratch"
fi
exit $status
