# Brand assets

Two marks, one for the platform and one for the app. Use one per surface; don't
put both side by side.

- **Langstroth** (the platform: the GitHub org, langstroth.ai, the relay, the
  docs) uses the **frame mark**: a hive box seen from above with its movable
  frames.
- **Ligus** (the app: desktop, phone, CLI, `lt`, Homebrew, the App Store) uses
  the **honeycomb mark**. Ligus is short for *ligustica*.

## Langstroth

| File | What it is | Used for |
|---|---|---|
| `langstroth-mark.svg` | The frame mark in the brand colours, transparent | langstroth.ai header and footer (inlined in the site as `FrameMark`), docs |
| `langstroth-mark-mono.svg` | The same mark in `currentColor` | One-colour places: dark/light UI chrome, print |
| `langstroth-org-avatar.svg` | The mark centred on the dark brand tile, square | Source for the org avatar |
| `langstroth-org-avatar-1024.png` | 1024×1024 render of the avatar | The `langstroth-ai` GitHub org profile picture (uploaded by hand: org Settings → Profile) |

Re-render the avatar after changing its SVG:

```sh
rsvg-convert -w 1024 -h 1024 langstroth-org-avatar.svg -o langstroth-org-avatar-1024.png
oxipng -o 4 langstroth-org-avatar-1024.png   # optional, smaller file
```

## Ligus

| File | What it is | Used for |
|---|---|---|
| `ligus-mark.svg` | The honeycomb mark in the brand colours | The app, docs, the Ligus pages of langstroth.ai |
| `ligus-mark-mono.svg` | The honeycomb mark in `currentColor` | One-colour places |
| `ligus-lockup.svg` | Mark plus wordmark | Headers, READMEs |
| `ligus-app-icon.svg`, `ligus-app-icon-1024.png` | The app icon | Desktop and phone app icons (`render-tauri-icons.sh` makes the desktop sizes) |
| `ligus-tray-template.svg`, `ligus-tray-template-128.png` | Monochrome template icon | The macOS menu bar |
| `ligus-brand-tokens.json` | Colours, type and spacing tokens | The desktop app, the website, `lt` themes |
| `LigusLogo.html`, `generate-brand-assets.mjs` | Logo reference and the generator | Regenerating the Ligus assets |

The `lt` terminal app's lockups and screenshots live in its own repository
(`langstroth-ai/ligus-term`, `assets/`), built from these marks.
