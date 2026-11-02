#!/usr/bin/env bash
set -euo pipefail
glibc_floor=2.35

check_floor() {
  local newest
  newest="$(objdump -T "$@" 2>/dev/null | grep -oE 'GLIBC_[0-9.]+' | sed 's/GLIBC_//' | sort -Vu | tail -n 1)"
  [ "$(printf '%s\n%s\n' "$newest" "$glibc_floor" | sort -V | tail -n 1)" = "$glibc_floor" ] \
    || { echo "needs glibc $newest, above the $glibc_floor baseline: $*" >&2; exit 1; }
  echo "glibc floor ok ($newest <= $glibc_floor)"
}

case "${1:-}" in
  app)
    [ -n "${TXTCASTER_UPDATE_PUBKEY:-}" ] || { echo "TXTCASTER_UPDATE_PUBKEY is required" >&2; exit 2; }
    ./scripts/fetch_ffmpeg_linux.sh apps/desktop/src-tauri
    ./scripts/release/stage_libpulse.sh /opt/libpulse apps/desktop/src-tauri/pulse-lib
    cd apps/desktop
    # node_modules is the host's, installed from the same lockfile: never reinstall it from in here.
    export pnpm_config_verify_deps_before_run=false npm_config_verify_deps_before_run=false
    # shellcheck disable=SC2086
    pnpm tauri build ${TAURI_BUILD_ARGS:-} --bundles deb,rpm --config src-tauri/tauri.bundle.conf.json \
      --config src-tauri/tauri.ffmpeg.linux.conf.json
    # linuxdeploy resolves every ELF it finds and moves FFmpeg's libraries into usr/lib itself,
    # rewriting the search path, so the AppImage takes only the binaries as resources.
    # shellcheck disable=SC2086
    LD_LIBRARY_PATH="$PWD/src-tauri/ffmpeg-lib" pnpm tauri build ${TAURI_BUILD_ARGS:-} --bundles appimage \
      --config src-tauri/tauri.bundle.conf.json --config src-tauri/tauri.ffmpeg.conf.json
    (cd /src && ./scripts/release/finish_appimage.sh /build/target/release/bundle/appimage)
    check_floor /build/target/release/txtcaster
    ls -la /build/target/release/bundle/*/*.{deb,rpm,AppImage} 2>/dev/null || true
    ;;
  engine)
    variant="${2:?engine variant required}"
    ENGINE_VARIANT="$variant" UV_PROJECT_ENVIRONMENT="/build/venv-$variant" UV_OFFLINE=0 ./scripts/build_engine.sh
    case "$variant" in
      cpu) dist=engine/dist ;;
      *) dist="engine/dist-$variant" ;;
    esac
    check_floor "$dist/txtcaster-engine/txtcaster-engine" "$dist/txtcaster-engine/_internal/libpython3.11.so.1.0"
    ;;
  signer)
    cd apps/desktop/src-tauri
    cargo build --release --features release-tools --bin update_sign
    ;;
  *)
    echo "usage: $0 app | engine VARIANT | signer" >&2
    exit 2
    ;;
esac
