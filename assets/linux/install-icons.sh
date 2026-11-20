#!/usr/bin/env bash
# Script de instalación de íconos para Linux (FreeDesktop hicolor theme)
# Uso: ./install-icons.sh [nombre-de-tu-app] [app-icon-dark|app-icon-light|transparent-for-dark-mode|transparent-for-light-mode]

set -e
APP_NAME="${1:-mi-terminal}"
VARIANT="${2:-app-icon-dark}"
ICON_BASE="$HOME/.local/share/icons/hicolor"

if [ ! -d "../${VARIANT}" ]; then
  echo "Error: variante '${VARIANT}' no encontrada."
  exit 1
fi

for SIZE in 16 24 32 48 64 128 256 512; do
  mkdir -p "$ICON_BASE/${SIZE}x${SIZE}/apps"
  cp "../${VARIANT}/png/${SIZE}x${SIZE}.png" "$ICON_BASE/${SIZE}x${SIZE}/apps/${APP_NAME}.png"
done

mkdir -p "$ICON_BASE/scalable/apps"
cp "../${VARIANT}/icon.svg" "$ICON_BASE/scalable/apps/${APP_NAME}.svg"

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -f -t "$ICON_BASE" || true
fi

echo "Íconos (${VARIANT}) instalados correctamente para '${APP_NAME}' en $ICON_BASE"
