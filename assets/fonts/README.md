# Embedded fonts

All fonts are licensed under the SIL Open Font License 1.1 (OFL). The license
texts are shipped next to the files and must stay with any redistribution.

| File | Family / weight | Source |
|---|---|---|
| `DMSans-Regular.ttf` | DM Sans 400 | https://github.com/googlefonts/dm-fonts/blob/main/Sans/fonts/ttf/DMSans-Regular.ttf |
| `DMSans-Medium.ttf` | DM Sans 500 | https://github.com/googlefonts/dm-fonts/blob/main/Sans/fonts/ttf/DMSans-Medium.ttf |
| `DMSans-SemiBold.ttf` | DM Sans 600 | https://github.com/googlefonts/dm-fonts/blob/main/Sans/fonts/ttf/DMSans-SemiBold.ttf |
| `GeistMono-Regular.ttf` | Geist Mono 400 | https://github.com/vercel/geist-font/releases/tag/v1.9.2 (`geist-font-v1.9.2.zip`, `geist-font/GeistMono/ttf/`) |
| `GeistMono-Medium.ttf` | Geist Mono 500 | same archive |
| `OFL-DMSans.txt` | License | https://github.com/googlefonts/dm-fonts/blob/main/Sans/OFL.txt |
| `OFL-GeistMono.txt` | License | `geist-font/OFL.txt` in the archive above |

All files are static instances (no variable fonts). Copyright:
"The DM Sans Project Authors" and "The Geist Project Authors".

The `theme` crate embeds them with `include_bytes!` (see `theme::fonts::embedded()`).
