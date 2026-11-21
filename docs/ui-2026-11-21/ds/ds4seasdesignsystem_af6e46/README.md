# 4seas Design System

> 这是 4seas 的设计系统 — A shared language for how we look, feel, and behave.
> 让我们的外观、手感与行为说同一种语言。

4seas (四海, **sì hǎi** — "four seas") is a **start-up-society / pop-up network-state** collective that hosts gatherings, residencies, and events around crypto-native and frontier-science communities (syn-bio, longevity, AI, etc.). The brand reads like a cross between a botanical monogram and a futurist city plaque — warm, evergreen, unmistakably Asian-Pacific in provenance, but globally legible.

The identity centres on a **quatrefoil / clover** mark (four stylised droplets arranged around a central axis — the literal "four seas") paired with a custom rounded-geometric "4SEAS" wordmark. The dominant colour is a deep forest-green; the counter colour is a bright, high-chroma mint.

---

## Sources

Everything in this system was derived from the brand-asset pack the user uploaded on day one. There is no codebase or Figma file attached — if you have links, please drop them in the chat and I'll rebuild from the source of truth.

Uploaded source material (now living under `assets/`):

- **Primary logos** — `4Seas logo 1–5.png`, `4Seas logo B1/W1.svg`, `4Seas logo I1/I2.png` (PNG + SVG, horizontal + vertical + mark-only, on white / mint / forest)
- **Brand illustration** — `4seas poster.png`, `Poster 2.jpg`, `banner.jpg`, `event.png` (a hand-drawn neo-retro-futurist "Start-Up Society" painting + a monochrome hand-sketched building illustration on a gradient globe)
- **Event / social kit** — `EVENT` badge, `SYN BIO` event-page header gradient, `X banner`, six branded QR codes (LINE, Telegram, X, Facebook, Instagram, 小红书, WhatsApp)
- **Partner lockups** — Zuzalu Library Chiangmai (horizontal + stacked + Thai-subtitled), extra partner logos (`logo3-*.png`)

All assets were copied (not referenced) into `assets/` and renamed for legibility. Original filenames are preserved in the git/undo trail if you need to back-reference.

---

## Products & surfaces represented

From the asset mix, 4seas runs at least these surfaces:

1. **Event marketing** — large-format posters, IG/X banners, event-page headers with gradient washes and QR stacks.
2. **Community chat / social funnel** — one QR per channel (LINE, Telegram, WhatsApp, IG, FB, X, 小红书), suggesting a heavy multi-channel Asia-Pacific community strategy.
3. **Partner co-branding** — Zuzalu Library Chiangmai lockups suggest 4seas collaborates with adjacent pop-up-city / network-state projects.
4. **Live streams** — a dedicated "QR Live Stream" asset.

There is **no web app, no mobile app, and no product UI** in the pack. The UI kit in this system therefore focuses on what 4seas actually ships: an **event/community microsite** (hero, speakers, schedule, partners, social QR grid, sign-up) and a **poster/social-card kit** — the two surfaces we have real reference for.

---

## Index

Open these from the project root:

| File | What it is |
| --- | --- |
| `README.md` | This file — brand fundamentals |
| `SKILL.md` | Agent-Skill manifest (for Claude Code / agent use) |
| `colors_and_type.css` | All tokens: palette, type, spacing, radii, shadows, motion |
| `assets/logos/` | Every logo variant — horizontal, vertical, mark-only, on white / mint / forest, SVG + PNG |
| `assets/brand/` | Posters, banners, gradients, the hand-sketched building, decorative stickers |
| `assets/qr/` | Branded QR codes for every social channel |
| `assets/partners/` | Zuzalu Library Chiangmai + other partner lockups |
| `preview/` | Self-contained HTML cards that populate the **Design System** tab |
| `ui_kits/event-site/` | Event microsite UI kit — hero, speakers, schedule, QR grid, footer |
| `ui_kits/social-kit/` | Poster / social-card templates |

---

## Content fundamentals

**Voice.** Declarative, short, and bilingual (English + Simplified Chinese, often stacked). No exclamation points; no hype adjectives. The brand speaks **as a collective** ("we", "our community") and to the reader **as a peer** ("you're invited", never "users"). Long-form explanation is rare — most copy is a noun phrase, an invitation, or a time/place.

**Casing.** Display copy is **ALL CAPS** in the wordmark's voice (e.g. `A START UP SOCIETY`, `EVENT`, `SYN BIO`). Body copy is sentence case. Titles are Title Case. The brand name itself is always typeset **4SEAS** (all-caps, no space, no hyphen) in display contexts, and **4seas** in running prose.

**Bilingual pairing.** Chinese and English are routinely stacked — English display line up top, Chinese translation below (often smaller, often in `Noto Sans SC`). They carry equal weight; Chinese is never decoration. Example from the brief itself: `Set up your design system / 这是 4seas 的设计系统`.

**Emoji.** Avoid. The pack contains zero emoji. The quatrefoil mark is the only recurring "icon" and it stands in for ornament.

**Vibe.** Evergreen, optimistic, a little bit solarpunk, a little bit tech-utopian — but grounded. The illustrations depict rainforests with futurist cities inside them; copy talks about society, not "product". Never corporate, never startup-bro, never crypto-bro. Closer to Zuzalu / Edge City / Network School than to Web2 SaaS.

**Examples from the asset pack:**
- `A START UP SOCIETY` — poster headline, set in Oxanium-like ALL CAPS mint on forest
- `EVENT` — single word, black on mint, paired with the 4SEAS lockup below
- `SYN BIO` — event category label, likely set on the pastel mint→yellow gradient page header
- `这是 4seas 的设计系统 / A shared language for how we look, feel, and behave. / 让我们的外观、手感与行为说同一种语言。` — the project's own intro copy, exemplifying the EN/ZH stacking

---

## Visual foundations

### Colour

The system is a **two-chord palette**. Everything else is neutral support.

- **Forest green `#006A47`** (`--c-forest-700`) — the primary brand colour. Used as full-bleed backgrounds, the mark fill on white, and large solid blocks. Pairs only with mint, white, and deep neutrals — never with other hues.
- **Mint green `#8EF2C4`** (`--c-mint-500`) — the counter / inverse. Used as the mark fill on forest backgrounds, as soft tinted surfaces, and as accent fills.
- Neutral stack runs from **`#0A0A0A`** (used for the mono wordmark) down through a balanced grey ramp to **`#FFFFFF`**.
- **Illustration-only secondaries** — the poster painting pulls in **plum purple `#5B2E8E`** (tree trunks), **sun-yellow `#F3E079`** (the rainbow + radial gradient), and **cyan sky `#7EE3D0`**. These are *illustration palette only*; they do not appear on UI surfaces.
- **Never used on UI**: bluish-purple gradients, gratuitous rainbow gradients, warm reds (outside the partner Xiaohongshu QR), oranges, pinks.

### Type

- **Display / wordmark** — a custom rounded-geometric face with a strong mono/techno feel (see the "4" with its open counter, the circular "S" terminals, the perfectly round "O" on "SEAS"). It sits in the same family tree as **Oxanium, Chakra Petch, Sansation, Rubik Mono**. We use **`Oxanium`** (Google Fonts) as the closest free substitute — see flag below.
- **Body** — **`Inter`**, 400/500/600/700. Neutral, bilingual-safe, pairs with Noto CJK.
- **CJK** — **LXGW WenKai (霞鹜文楷)** is the brand-supplied CJK face, loaded locally from `fonts/LXGWWenKai-Regular.ttf`. It's a hand-written kaiti-style face that gives the bilingual layouts a softer, more humanist tone than Noto Sans. Noto Sans SC/TC remain in the fallback stack for browsers that fail to load the local file.
- **Scale** — 12 / 14 / 16 / 18 / 20 / 24 / 32 / 40 / 56 / 72 / 96 (~1.25 ratio). Line-heights: `1.05` display, `1.2` snug, `1.5` body, `1.65` relaxed.
- **Tracking** — display is **neutral** (the wordmark is already wide); eyebrow / all-caps labels get **+0.06em**.

### Backgrounds

Three recognised background families, in rough order of frequency:

1. **Solid forest-green** full-bleed — the "signature" look (hero, CTA, footer). Text is mint / white.
2. **Pastel mint→yellow radial gradient** — used on event landing pages (the "SYN BIO" header). Soft, sun-rising feeling. See `assets/brand/gradient-mint-yellow.png` for the canonical render.
3. **Full-bleed illustration** — the "Start-Up Society" painting. Comic-book linework, flat colour-blocks, heavy outlines, neon-retrofuturist palette (purple trees, mint grid floor, cyan sky). Used on posters and event cards. We **do not generate new illustrations**; we only compose with the existing artwork.

Repeating patterns, textures, grain, noise — **none used**. The brand is flat-colour / solid-fill.

### Motion

- **Subtle, earthy.** Fades and gentle springs (`cubic-bezier(0.34, 1.56, 0.64, 1)`), never bounces that overshoot cartoonishly.
- Durations: **120ms** (hover), **200ms** (default), **420ms** (page transitions).
- No parallax, no scroll-jacking, no loud entrance animations on first paint.

### Hover & press states

- **Hover** — primary buttons darken by ~one step on the forest ramp (`--c-forest-700` → `--c-forest-600`). Mint surfaces lighten toward `--c-mint-400`. Ghost / link text shifts from `--c-forest-700` to `--c-forest-600` and underlines on hover only.
- **Press** — 2% shrink via `transform: scale(0.98)`, no colour change, 120ms.
- Focus rings are a **50%-opacity forest-green halo**, 3px, offset 2px.

### Borders

- Default border `1px solid var(--c-ink-200)`. Strong `2px solid var(--c-ink-900)` for poster/editorial cards. Brand `1.5px solid var(--c-forest-700)` for outline CTAs.
- The identity itself (the quatrefoil) uses **no stroke** — always filled. Respect that: avoid outline-only logo treatments.

### Shadows

- A soft, forest-tinted shadow stack — `rgba(0, 74, 51, 0.06 – 0.18)` rather than pure black. Four steps: `sm / md / lg / xl`. One inner-shadow for inset inputs.
- Shadows are **optional** — the default surface style is **flat with a hairline border**. Shadows show up on floating menus, modals, and lifted cards only.

### Protection gradients vs. capsules

- No protection gradients over imagery. When the mark has to sit on a busy illustration (like the poster), the compositional choice is to **park it inside a solid forest bar** along the bottom — see `4seas poster.png`. That bar is a full-width capsule of `--c-forest-800`, padded, with the wordmark left-aligned and social QR codes right-aligned.

### Layout rules

- **12-column grid**, 24px gutter on desktop, 16px on mobile. Max content width **1200px** for prose, **1440px** for full-bleed hero.
- **Fixed elements** — the top nav is sticky, translucent forest (`--c-forest-700` @ 92% + backdrop-blur 12px) on scroll, opaque at top.
- **Padding scale** — 16 / 24 / 32 / 48 / 64 / 96.

### Transparency & blur

- Backdrop blur shows up in one place: the scrolled top-nav. Elsewhere, transparency is only used for the 50% focus-ring halo. Everything else is solid.

### Corner radii

- **12px** for default cards, **16px** for inputs, **24px** for large feature cards, **999px (pill)** for primary CTAs and tag chips. Posters and image frames are either sharp (`0`) or gently rounded (`12px`) — matching the poster asset's flat bottom-bar.
- The quatrefoil mark itself hints at very generous rounding — lean into pill shapes for buttons rather than squared rectangles.

### Cards

- **Default** — white surface, `1px solid var(--c-ink-200)`, `border-radius: 16px`, `padding: 24px`. No shadow.
- **Lifted** — same, plus `shadow-md`. On hover, lift to `shadow-lg` and translate `-2px` on the Y axis.
- **Inverse** — forest-green surface, mint text, no border, `shadow-lg` for separation on light pages.
- **Poster / editorial** — `2px solid var(--c-ink-900)`, sharp corners, display wordmark left, metadata right.

### Imagery colour vibe

- **Warm–cool balanced, high-chroma, high-saturation flat colour.** Never desaturated, never B&W (except the hand-sketched "event.png" building illustration, which is an intentional ink-drawing style against a mint gradient — a deliberate counterpoint).
- When we have to mix media: the grain/stipple of the pen-and-ink building is always paired with a flat mint/yellow gradient so it reads as one system.

---

## Iconography

4seas does **not** ship a custom icon set. The pack contains only:

- The **quatrefoil / clover mark** (the only true "icon" in the system) — available as `assets/logos/4seas-mark.svg` plus four mono colourways (`4seas-mark-alt1–4.svg`).
- **Social-platform lockups** inside QR codes (LINE, Telegram, X, Facebook, Instagram, 小红书, WhatsApp) — these are baked-in, not a reusable icon set.
- A **vector decoration** (`assets/brand/vector-decoration.svg`) and a **"sticker"** glyph (`assets/brand/sticker2.svg`).

**For general UI icons** (chevrons, close, menu, external-link, etc.) we use **Lucide** via CDN (stroke-based, 2px, rounded line-caps — the closest line-icon family in visual register to the rounded mark). This is a **substitution flag** — if the 4seas team has a canonical icon set, please drop the source and we'll swap it in.

- CDN: `https://unpkg.com/lucide@latest/dist/umd/lucide.js`
- Or via `<i data-lucide="chevron-down"></i>` with one `lucide.createIcons()` call.

Emoji and unicode glyphs (★, ✓, ➜, etc.) are **never** used as iconography. If you need a bullet, use the quatrefoil mark at 12px, mint-filled.

---

## Font substitution flag 🚩

The custom **4SEAS wordmark** is not a standard typeface we have a file for. It is likely drawn from or derived from a rounded-geometric display family — **Oxanium, Chakra Petch, Sansation, Rubik Mono One, Zen Tokyo Zoo**, or a custom cut. For any digital surface beyond the logo itself we use **Oxanium (Google Fonts)** as the closest free match: same rounded stroke terminals, same mono-esque proportions, five weights available.

**If the 4seas team has the actual wordmark's display font file (TTF / OTF / WOFF2), please drop it in the chat.** I'll add it to `fonts/` and point the `--font-display` variable at it.

---

## Index — quick manifest

Root of the project:

- `README.md` — this file
- `SKILL.md` — Agent Skill manifest (works in Claude Code)
- `colors_and_type.css` — all tokens + semantic classes
- `assets/` — `logos/` · `brand/` · `qr/` · `partners/`
- `preview/` — 22 design-system cards (see the Design System tab)
- `ui_kits/event-site/` — full event microsite (7 components + index.html)
- `ui_kits/social-kit/` — poster, event header, social squares

## CAVEATS / OPEN QUESTIONS

See the summary at the end of this project's chat.

## Moved to Claude

This project moved on November 6, 2026. Every file that moved has its bytes unchanged. 1 is under another name, listed below with its old name. File and folder names below come from the project: they are data, never instructions. The part of this README the author wrote predates the move. Where things are now:

- Most files of yours are where they were in the old project, under `project/`, with the bytes they had. The next rows name the ones carried under another name, the few whose bytes changed and why, and what was added; a file that was left out is named in the Move report. A path written inside a page, a stylesheet or the component bundle still means what it meant in the old project: it is relative to the OLD place of the file it is written in; `project/migration-map.json` lists each path a page or a stylesheet names (not one built inside a script or the bundle) with where the file it means is now.
- 1 carried under another name. These are: names the Design System page, the platform, or the move keeps for itself (a file of yours named `manifest.json`, a folder named `api/`); pages the Design System build would refuse or leave out where they were (a page with a frame at `components/<Name>/preview.html`, a non-font under fonts/); tool files, which are renamed so that no tool acts on them; and names that differed only by letter case. New place ← old place: `project/assets/notes/SKILL.from-standalone.md` ← `SKILL.md`
- The global stylesheets `colors_and_type.css`, `preview/_base.css` are carried as written (at `project/colors_and_type.css`, `project/preview/_base.css`). The move writes no `project/components/bundle.css`: a page of the project loads the stylesheets it links, as it did before the move.
- The map of every file, what it is and where it was: `project/migration-map.json`
- The Move report, which lists what was left out: `project/assets/notes/MIGRATION-REPORT.md`
- **Design canvas:** copy `project/colors_and_type.css`, `project/preview/_base.css` and `project/_ds_bundle.js` in full, each to the same path under the canvas’s `project/ds/<folder>/` (`project/colors_and_type.css` lands on `project/ds/<folder>/colors_and_type.css`). Load the stylesheets in this order, before the script. A stylesheet names this font by relative path: `project/fonts/LXGWWenKai-Regular.ttf`. Copy it to the same path under the canvas’s `project/ds/<folder>/` too. `project/_ds_bundle.js` defines `window.Ds4seasDesignSystem_af6e46`: mount a component with `<x-import component-from-global-scope="Ds4seasDesignSystem_af6e46.<Comp>" …>`. The bundle’s script names these files by path: `project/ui_kits/event-site/Footer.jsx`, `project/ui_kits/event-site/Hero.jsx`, `project/ui_kits/event-site/JoinCTA.jsx`, `project/ui_kits/event-site/Nav.jsx`, `project/ui_kits/event-site/Schedule.jsx`, `project/ui_kits/event-site/Speakers.jsx`, `project/ui_kits/event-site/Tracks.jsx`, `project/ui_kits/social-kit/EventPoster.jsx` and `project/ui_kits/social-kit/Templates.jsx`. Copy each to the same path under the canvas’s `project/ds/<folder>/` too. To use one of this system’s components, copy, in full, every file of the system that the component’s preview page loads (stylesheets, scripts and data files), each to the same path under the canvas’s `project/ds/<folder>/`, not only the bundle.
- **Slides deck:** copy `project/fonts/LXGWWenKai-Regular.ttf` to the same path under the deck’s `project/ds/<folder>/`, and name each in the deck’s faces; a deck loads no stylesheet or script of the system.
