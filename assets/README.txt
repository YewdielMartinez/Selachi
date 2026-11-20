========================================================================
PAQUETE DE ÍCONOS (MODO CLARO + MODO OSCURO) - APP DE TERMINAL
========================================================================

ESTRUCTURA DE CARPETAS:

1. app-icon-dark/
   - Tiburón claro sobre fondo oscuro redondeado (#14161c).
   - Funciona bien en CUALQUIER tema (claro u oscuro) para Dock, barra de
     tareas y menú de apps. Incluye .ico, .icns, .svg y PNGs (16 a 1024 px).

2. app-icon-light/
   - Tiburón carbón oscuro sobre fondo claro redondeado (#f5f6f9).
   - Alternativa si deseas un ícono principal en tema claro.

3. transparent-for-dark-mode/
   - Tiburón claro SIN fondo.
   - Usar en: System Tray / barra de tareas en MODO OSCURO (Windows/Linux)
     o pantallas/UI oscuras.

4. transparent-for-light-mode/
   - Tiburón oscuro SIN fondo.
   - Usar en: System Tray / barra de tareas en MODO CLARO (Windows/Linux)
     o pantallas/UI claras.

5. macos-tray-template/
   - Íconos Template oficiales para la barra de menú de macOS:
       trayTemplate.png (16x16), trayTemplate@2x.png (32x32), trayTemplate@3x.png (48x48)
       trayTemplate-22.png (22x22), trayTemplate-22@2x.png (44x44)
       trayTemplate.svg
   - IMPORTANTE: El sufijo "Template" en el nombre del archivo hace que
     macOS invierta el color automáticamente al cambiar entre modo claro
     y modo oscuro (tanto en apps nativas Swift/AppKit como en Electron y Tauri).

6. adaptive-svg/
   - icon-transparent-adaptive.svg  (cambia entre tiburón oscuro/claro)
   - icon-app-adaptive.svg          (cambia fondo y tiburón automáticamente)
   - Usan @media (prefers-color-scheme: dark), ideales para favicon,
     webview de Electron/Tauri o documentación web.

------------------------------------------------------------------------
EJEMPLO RÁPIDO PARA SYSTEM TRAY EN ELECTRON:
------------------------------------------------------------------------
  const { Tray, nativeTheme } = require('electron');
  const path = require('path');

  function getTrayIcon() {
    if (process.platform === 'darwin') {
      // macOS detecta "Template" y adapta claro/oscuro automáticamente
      return path.join(__dirname, 'macos-tray-template/trayTemplate.png');
    }
    return nativeTheme.shouldUseDarkColors
      ? path.join(__dirname, 'transparent-for-dark-mode/png/32x32.png')
      : path.join(__dirname, 'transparent-for-light-mode/png/32x32.png');
  }

  const tray = new Tray(getTrayIcon());
  nativeTheme.on('updated', () => tray.setImage(getTrayIcon()));
