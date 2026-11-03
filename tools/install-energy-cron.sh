#!/bin/sh
# Installs (or updates) the daily GRDF (Gazpar) and Enedis (Linky) energy
# import cron entries without overwriting unrelated crontab lines.
#
# Modes:
#   sh tools/install-energy-cron.sh --systemd
#     For a native/LXC Moli OS installation (e.g. /opt/moli + /usr/local/bin/moli-os,
#     installed in the `moli` user's crontab).
#
#   sh tools/install-energy-cron.sh --docker
#     For a Docker Compose installation (uses ~/.config/moli/*.env and
#     `docker exec -i moli-os /moli-os energy import` in the current user's crontab).

set -eu

MODE="${1:---systemd}"

case "$MODE" in
  --systemd)
    CRON_USER="${MOLI_CRON_USER:-moli}"
    MOLI_DIR="${MOLI_TOOLS_DIR:-/opt/moli}"
    PYTHON_BIN="${MOLI_PYTHON:-$MOLI_DIR/venv/bin/python}"
    MOLI_BIN="${MOLI_BIN:-/usr/local/bin/moli-os}"
    MOLI_CONFIG="${MOLI_CONFIG:-/data/moli.toml}"

    GRDF_LINE="15 7 * * * set -a; . $MOLI_DIR/grdf.env; set +a; $PYTHON_BIN $MOLI_DIR/gazpar-export.py | $MOLI_BIN -c $MOLI_CONFIG energy import"
    ENEDIS_LINE="30 7 * * * set -a; . $MOLI_DIR/enedis.env; set +a; $PYTHON_BIN $MOLI_DIR/enedis-export.py | $MOLI_BIN -c $MOLI_CONFIG energy import"

    EXISTING="$(crontab -u "$CRON_USER" -l 2>/dev/null || true)"
    FILTERED="$(printf '%s\n' "$EXISTING" | grep -v 'gazpar-export\.py' | grep -v 'enedis-export\.py' || true)"

    {
      if [ -n "$FILTERED" ]; then
        printf '%s\n' "$FILTERED"
      fi
      if [ -f "$MOLI_DIR/grdf.env" ]; then
        printf '%s\n' "$GRDF_LINE"
      fi
      if [ -f "$MOLI_DIR/enedis.env" ]; then
        printf '%s\n' "$ENEDIS_LINE"
      fi
    } | crontab -u "$CRON_USER" -

    echo "Installed crontab for user '$CRON_USER':"
    crontab -u "$CRON_USER" -l
    ;;

  --docker)
    REPO_DIR="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
    ENV_DIR="${MOLI_ENV_DIR:-$HOME/.config/moli}"
    PYTHON_BIN="${MOLI_PYTHON:-$HOME/.venvs/gazpar/bin/python}"
    CONTAINER="${MOLI_CONTAINER:-moli-os}"

    GRDF_LINE="15 8 * * * PATH=/usr/local/bin:\$PATH; set -a; . \"$ENV_DIR/grdf.env\"; set +a; \"$PYTHON_BIN\" \"$REPO_DIR/tools/grdf/gazpar-export.py\" | docker exec -i $CONTAINER /moli-os energy import"
    ENEDIS_LINE="30 8 * * * PATH=/usr/local/bin:\$PATH; set -a; . \"$ENV_DIR/enedis.env\"; set +a; \"$PYTHON_BIN\" \"$REPO_DIR/tools/enedis/enedis-export.py\" | docker exec -i $CONTAINER /moli-os energy import"

    EXISTING="$(crontab -l 2>/dev/null || true)"
    FILTERED="$(printf '%s\n' "$EXISTING" | grep -v 'gazpar-export\.py' | grep -v 'enedis-export\.py' || true)"

    {
      if [ -n "$FILTERED" ]; then
        printf '%s\n' "$FILTERED"
      fi
      if [ -f "$ENV_DIR/grdf.env" ]; then
        printf '%s\n' "$GRDF_LINE"
      fi
      if [ -f "$ENV_DIR/enedis.env" ]; then
        printf '%s\n' "$ENEDIS_LINE"
      fi
    } | crontab -

    echo "Installed crontab for current user:"
    crontab -l
    ;;

  *)
    echo "Usage: $0 [--systemd|--docker]" >&2
    exit 1
    ;;
esac
