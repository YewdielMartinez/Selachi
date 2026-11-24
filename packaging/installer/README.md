# Instalador de Selachi (Inno Setup, por usuario, sin administrador)

Decisión y comparativa: `docs/adr/0002-instalador.md`.

## Construir en local

```powershell
./packaging/installer/build.ps1               # cargo build --release + stage + ISCC
./packaging/installer/build.ps1 -SkipBuild    # empaqueta los exes que ya hay en target\release
./packaging/installer/build.ps1 -Version v0.2.0 -NoContextMenu
```

Salida: `packaging/installer/dist/Selachi-Setup-<versión>.exe` y `SHA256SUMS.txt` (`dist/` y `stage/` están en `.gitignore`).

Requisito: `ISCC.exe` (Inno Setup 6.3+ o 7.x). Instalación sin admin:
`innosetup-<ver>-x64.exe /CURRENTUSER /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP-` (descarga de https://jrsoftware.org/isdl.php; comprueba la firma Authenticode). El script lo busca en `$env:ISCC`, `PATH`, `%LOCALAPPDATA%\Programs\Inno Setup 6|7`, `%ProgramFiles(x86)%`, `%ProgramFiles%`, o con `-IsccPath`.

## Qué instala (`%LOCALAPPDATA%\Programs\Selachi`)

| Archivo | Para qué |
|---|---|
| `selachi.exe` | La app (subsistema Windows) |
| `selachi-agent.exe` | CLI de consola del harness (`selachi-agent run|list|status…`) |
| `assets\onboarding\shark.gif` | El GIF de bienvenida; la app lo busca en `<carpeta del exe>\assets\onboarding\shark.gif` |
| `selachi.ico` | Icono de los accesos directos |
| `LICENSE.txt`, `THIRD-PARTY-NOTICES.txt`, `THIRD-PARTY-LICENSES.txt`, `licenses\` | Licencias (OFL de las fuentes, Apache-2.0 de gpui y alacritty_terminal, índice de crates) |

Las fuentes están incrustadas en `selachi.exe`; `integration.ps1` también (la app lo escribe en `%LOCALAPPDATA%\Selachi\shell`); las gramáticas se descargan a `%APPDATA%\Selachi\plugins\grammars`. No se instala ninguna plantilla de configuración: la app crea `%APPDATA%\Selachi\config.toml` al arrancar.

Tareas del asistente (todas desmarcadas por defecto): icono de escritorio, añadir la carpeta al PATH de usuario (para `selachi-agent`), "Abrir en Selachi" en el menú contextual del Explorador (HKCU; en Windows 11 aparece en "Mostrar más opciones"). Siempre crea el acceso del menú Inicio.

## Instalación y desinstalación silenciosas

```
Selachi-Setup-x.y.z.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /TASKS="addtopath,contextmenu,desktopicon"
Selachi-Setup-x.y.z.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /CLOSEAPPLICATIONS /RESTARTAPPLICATIONS   # actualización
"%LOCALAPPDATA%\Programs\Selachi\unins000.exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART                  # conserva %APPDATA%\Selachi
"%LOCALAPPDATA%\Programs\Selachi\unins000.exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /DELETEDATA     # y borra los datos
```

Desinstalación interactiva: pregunta "¿Borrar también tus datos?" (por defecto No). Con "Sí" borra `%APPDATA%\Selachi` y `%LOCALAPPDATA%\Selachi`; con "No" conserva ambos salvo `%LOCALAPPDATA%\Selachi\shell` (script que la app regenera). Quita también la entrada del PATH y las claves del menú contextual.

`AppId` en `selachi.iss` no debe cambiar jamás: es lo que convierte un instalador nuevo en actualización.

## Firma de código (pendiente del usuario)

No se firma nada sin credenciales. Con certificado: `build.ps1 -SignCommand 'signtool sign /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 /f cert.pfx /p ****'` firma `selachi.exe`, `selachi-agent.exe`, el instalador y el desinstalador. En CI ver `TODO(signing)` en `.github/workflows/release.yml` (secretos `WINDOWS_CERT_PFX_BASE64`, `WINDOWS_CERT_PASSWORD`, o Azure Trusted Signing).

## Pendiente

- No hay `LICENSE` en la raíz del repo. Mientras tanto se empaqueta `packaging/installer/LICENSE.txt` (MIT, titular "Selachi contributors" como marcador): confirmar el titular o añadir un `LICENSE` raíz (tiene prioridad).
- La app debe aceptar `--cwd <dir>` (menú contextual) y `--version`.
