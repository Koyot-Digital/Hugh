# Icon artwork

Hugh prints no emoji of its own. Every glyph on a case card, lookup card or
standing card comes from an icon slot, and a slot is empty until you point it at
one of your server's custom emoji. This directory holds the artwork to upload,
one file per slot, drawn from [Lucide](https://lucide.dev) 1.60.0.

- `png/<slot>.png` — 128×128, transparent, stroked in Discord's light text
  colour (`#E3E5E8`). Upload these.
- `svg/<slot>.svg` — the same icons as vectors, for recolouring or a different
  size.
- `icons.json` — slot, Lucide icon name and file path for each slot.

## Uploading

1. Server Settings → Emoji → Upload Emoji, and pick every file in `png/`.
2. Keep the file names: an emoji uploaded from `flagged.png` is named `flagged`,
   which matches its slot.
3. With Developer Mode on, right-click each emoji and Copy Emoji ID, or type
   `\:flagged:` in any channel to reveal the full `<:flagged:123…>` form.
4. Put those values in `config.toml`:

   ```toml
   [icons]
   flagged = "<:flagged:123456789012345678>"
   safe = "<:safe:123456789012345679>"
   ```

Hugh validates the slot names at startup and refuses a bare `:flagged:`
shortcode, because Discord expands shortcodes only in its own message box.

Fifty emoji slots is the free-tier limit, and there are 33 icons here, so the
whole set fits with room to spare. Slots you leave unset simply render as
nothing; no card depends on an icon to make sense.

## Refreshing

```sh
python scripts/icons.py                      # both formats, default colour
python scripts/icons.py --stroke "#2E3338"   # for a light-themed server
python scripts/icons.py --size 160 --stroke-width 2.5
```

The script reads the slot list out of `src/config.rs`, so it always matches the
slots Hugh renders. Rendering uses cairosvg, rsvg-convert, Inkscape,
ImageMagick or Node with `sharp`, whichever is installed; with none of them the
SVGs are still written.

Lucide is ISC licensed; see
<https://github.com/lucide-icons/lucide/blob/main/LICENSE>.
