# Service marks

The logos in this folder identify the cloud storage services Vireo can
upload to. They are shown next to a service's name in Settings (the Service
picker, the Cloud Storage list, the account editor) to say "this works with
that service", and for nothing else.

**They are not covered by Vireo's licence.** Each is a trademark of its
owner and is used under that owner's brand guidelines, unmodified apart
from being rendered to PNG at the size shown. Vireo is not affiliated with
or endorsed by any of them. Section 7(e) of the AGPLv3 lets the project
decline to grant trademark rights, and it does: nothing here may be reused
as if it were part of the AGPL-licensed work.

| File | Mark of | Source (fetched 2026-09-10) |
| --- | --- | --- |
| `nextcloud` | Nextcloud GmbH | https://nextcloud.com/c/uploads/2022/08/nextcloud-logo-icon.svg (linked from https://nextcloud.com/trademarks/) |
| `owncloud` | ownCloud GmbH, a Kiteworks company | `packages/web-runtime/themes/owncloud/assets/owncloud-app-icon.png` in https://github.com/owncloud/web |
| `opencloud` | OpenCloud GmbH | `packages/design-system/docs/public/logo.svg` in https://github.com/opencloud-eu/web |
| `onedrive` | Microsoft Corporation | the current OneDrive product icon, as published on Wikimedia Commons (`Microsoft OneDrive Icon (2025 - present).svg`) |
| `dropbox` | Dropbox, Inc. | the glyph of the 2017 Dropbox logo, in its brand blue #0061FF, as published on Wikimedia Commons (`Dropbox logo 2017.svg`); Dropbox's brand rules are at https://www.dropbox.com/branding |
| `seafile` | Seafile Ltd. | `data/icons/scalable/apps/seafile.svg` in https://github.com/haiwen/seafile-client |

`src/` keeps the files as fetched; the 128 px PNGs beside this file are
what the binary embeds (`src/brand.rs`). To refresh one, replace the source
and re-run:

```sh
magick -background none -density 400 data/brands/src/NAME.svg -resize 128x128 \
  -gravity center -extent 128x128 data/brands/NAME.png
```

The ownCloud and OpenCloud marks are square tiles; they get the corner
radius the other tile-shaped icons in the app have (20 px on 128), and
nothing else changes:

```sh
magick data/brands/src/NAME.* -resize 128x128 \
  \( -size 128x128 xc:none -draw "roundrectangle 0,0,127,127,20,20" \) \
  -alpha set -compose DstIn -composite data/brands/NAME.png
```
