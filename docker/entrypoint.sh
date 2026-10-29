#!/bin/sh
# Runs Flixnet as PUID:PGID (Unraid defaults 99:100) and grants access to the
# GPU render node so hardware transcoding works without running as root.
set -eu

PUID="${PUID:-99}"
PGID="${PGID:-100}"
umask "${UMASK:-022}"

if [ "$(id -u)" != "0" ]; then
    # Already started as a non-root user (e.g. `--user`): nothing to set up.
    exec "$@"
fi

if ! getent group "$PGID" >/dev/null; then
    groupadd -o -g "$PGID" flixnet
fi
if ! getent passwd "$PUID" >/dev/null; then
    # Unraid's UID 99 is below UID_MIN; that is intended, so silence the warning.
    useradd -o -u "$PUID" -g "$PGID" -d /config -s /usr/sbin/nologin -M -K UID_MIN=1 flixnet
fi

# Collect the groups owning the GPU device nodes (render/video GIDs differ per host).
extra_groups=""
for dev in /dev/dri/renderD* /dev/dri/card* /dev/nvidia*; do
    [ -e "$dev" ] || continue
    gid="$(stat -c %g "$dev")"
    case ",$extra_groups," in
        *",$gid,"*) ;;
        *) extra_groups="${extra_groups:+$extra_groups,}$gid" ;;
    esac
done

mkdir -p /config
chown "$PUID:$PGID" /config
if [ -d /transcode ]; then
    chown "$PUID:$PGID" /transcode
fi

groups="$PGID${extra_groups:+,$extra_groups}"
exec setpriv --reuid="$PUID" --regid="$PGID" --groups="$groups" --inh-caps=-all -- "$@"
