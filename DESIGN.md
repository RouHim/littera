---
name: Littera
description: Local voice dictation for Linux/Wayland — an instrument that states its own truth.
colors:
  enamel: "#22282E"
  plate: "#262D34"
  well: "#1D2329"
  silk: "#E9EBE4"
  muted: "#9AA39B"
  signal: "#E3B23C"
  signal-hover: "#EDC158"
  signal-pressed: "#C99A2E"
  stop: "#C14B3A"
  fault: "#E07361"
  run: "#6FAF74"
typography:
  brand:
    fontFamily: "Barlow Condensed, sans-serif"
    fontSize: "18px"
    fontWeight: 600
  title:
    fontFamily: "Barlow Condensed, sans-serif"
    fontSize: "24px"
    fontWeight: 600
    lineHeight: 1.2
  legend:
    fontFamily: "Barlow Condensed, sans-serif"
    fontSize: "18px"
    fontWeight: 600
    lineHeight: 1.2
  key:
    fontFamily: "Barlow Condensed, sans-serif"
    fontSize: "15px"
    fontWeight: 600
  body:
    fontFamily: "Inter, sans-serif"
    fontSize: "17px"
    fontWeight: 400
  identifier:
    fontFamily: "ui-monospace, monospace"
    fontSize: "15px"
    fontWeight: 400
rounded:
  control: "0px"
  surface: "0px"
  pill: "16px"
spacing:
  xs: "4px"
  sm: "8px"
  md: "12px"
  lg: "16px"
  xl: "24px"
  xxl: "32px"
components:
  button-primary:
    backgroundColor: "transparent"
    textColor: "{colors.signal}"
    typography: "{typography.key}"
    rounded: "{rounded.control}"
    padding: "8px 16px"
  button-primary-hover:
    backgroundColor: "{colors.signal-hover}"
    textColor: "{colors.enamel}"
    typography: "{typography.key}"
    rounded: "{rounded.control}"
    padding: "8px 16px"
  button-secondary:
    backgroundColor: "transparent"
    textColor: "{colors.silk}"
    typography: "{typography.key}"
    rounded: "{rounded.control}"
    padding: "8px 16px"
  plate:
    backgroundColor: "{colors.plate}"
    textColor: "{colors.silk}"
    rounded: "{rounded.surface}"
    padding: "12px"
  field:
    backgroundColor: "{colors.well}"
    textColor: "{colors.silk}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    padding: "8px 10px"
  overlay-pill:
    backgroundColor: "rgba(38, 45, 52, 0.90)"
    textColor: "{colors.silk}"
    rounded: "{rounded.pill}"
    width: "560px"
    height: "112px"
---

# Design System: Littera

## Overview

**Creative North Star: "The Nameplate"**

Littera's windows are the machine talking about itself. The Settings screen opens on a
single stamped plate — model, backend, device, hotkey, one state lamp each — and then
disciplined rows of field facts below it. Legends are condensed and capitalised as if
silk-screened onto enamelled steel; values are set plainly; identifiers, paths and
commands are monospace because they are data. Alarms are placards: a full frame, the
tone's wash, the tone's ink. Nothing is rounded, nothing floats, nothing glows.

The use scene fixes the light: someone at a desk, often at night, screen-lit, opening
Settings deliberately mid-work to change one setting and close it again; the overlay
pill floats over any content at any hour. That forces a dark steel ground with one
signal colour, and it forces restraint — the app is a background instrument, not a
destination.

The system is deliberately sparse where it can be and explicit where it must be. Every
capability the platform withholds has a surface: a lamp dims, a placard turns on, a
blocked path names its fix. The world carries colour as information — the signal
yellow means "actable or attention", the stop red means failure, the run green means a
verified machine state — and nothing else is coloured.

**Key Characteristics:**
- One capability plate per surface, framed twice, never repeated.
- Stamped square controls; hover fills, nothing tweens.
- Legends in condensed caps; values in Inter; identifiers in monospace.
- Lamps and placards as the only state vocabulary.
- Flat steel: no shadows, no gradients, no rounded cards.

## Colors

The palette is a single dark steel family plus three signal inks; colour is never decoration.

### Primary
- **Signal Amber** (#E3B23C): the one action colour. Primary buttons (as a frame at rest, a fill on hover), focused field frames, the running download bar, active chips, and the caution wash of warning placards. Used sparingly; it is the only saturated ink on the screen.
- **Signal Amber, Shallow** (#EDC158) and **Signal Amber, Deep** (#C99A2E): the hover and pressed fills of primary controls.

### Neutral
- **Enamel** (#22282E): the window ground — flat, matte, unbudging.
- **Plate** (#262D34): the lifted surface of the capability plate and every field-row card.
- **Well** (#1D2329): recessed interiors — search fields, pickers, the model filter.
- **Silk** (#E9EBE4): primary ink and the legends; the 1px field grid is silk at 26 %, strong frames at 55 %.
- **Muted Steel** (#9AA39B): keys, hints, secondary lines. 5.7:1 on the ground, above AA.

### Semantic
- **Run Green** (#6FAF74): verified, resident, bound, ok. 5.7:1 on the ground.
- **Fault Ink** (#E07361): failure text, 4.8:1 — the light shade failures are *read* in.
- **Stop Red** (#C14B3A): failure frames and bars, never body text.

### Named Rules
**The Signal-Only Rule.** Exactly one saturated colour exists. It marks what can be acted on and what demands attention; if an element is neither, it is silk, muted steel or nothing.

**The Legible-Failure Rule.** Failure text always uses Fault Ink, never Stop Red; Stop Red only frames. A failure must stay readable before it may look severe.

## Typography

**Display/Legend Font:** Barlow Condensed SemiBold (embedded, SIL OFL 1.1)
**Body Font:** Inter Regular (embedded, SIL OFL 1.1)
**Data Font:** the system monospace, for paths, sizes, chords, commands

**Character:** The condensed legend face does the talking — short, tracked, always upper-case, like silk-screen stencils on equipment; Inter carries sentences and values at a working size. The pairing reads as a machine label next to a printed field, never as a document.

### Hierarchy
- **Title/legend** (600, 24px, 1.2): surface titles ("SET UP LITTERA"), upper-case.
- **Legend** (600, 18px, 1.2): section headings ("MODEL", "HOTKEY"), upper-case.
- **Key** (600, 15px): field keys and state captions ("BACKEND", "NOT DOWNLOADED"), upper-case, muted unless toned.
- **Body** (400, 17px): values, ledes, placard text; sentence case.
- **Identifier** (400, 15px): paths, model files, chords, percentages — monospace, silk.

Condensed caps are set a step above the value size: at the same pixel size they read smaller than sentence-case body text.

### Named Rules
**The Stencil-Caps Rule.** Anything that labels, names a section, or reports a state is upper-case in the legend face. Anything that is a sentence stays in Inter at sentence case.

**The Data-Is-Mono Rule.** Monospace is reserved for identifiers, measurements and commands; it is never a costume for technical tone.

## Layout

One scrolling column per window, 24px outer padding, sections separated by 32px and their content by 12px — more air above a heading than below it. Every setting is its own section with its own legend heading; nothing that is a separate setting hangs off the end of a list or a neighbouring card. Field rows lock their key in a 96px label column and align the value beside it, so a plate reads as a grid without drawing one. The Settings window is 900×720 (minimums 620×480); onboarding 820×660; the problems notice 560×620. The overlay pill is a fixed 560×112 floating 48px above the bottom edge. Density is compact-instrument: 4/8/12/16/24/32 is the whole spacing scale.

## Elevation & Depth

There are no shadows in this system. Depth is declared by tone alone: ground (Enamel) → lifted plate (Plate) → recessed well (Well), each stepped a few percent in luminance, each framed by a 1px silk hairline. Nothing floats, so nothing needs a shadow; the one legacy menu shadow was removed with the world it belonged to.

### Named Rules
**The Flat-Steel Rule.** No shadow, no blur, no gradient. A surface is defined by its fill tone and its frame; if that is not enough to separate it, the tone step is wrong.

## Shapes

Square by contract: controls, plates, placards, fields and scrollbars all use 0 radius, so the corners of the interface read as stamped metal. Two shapes round, and only two: the floating overlay pill (16px) and the onboarding step chips (999px pills) — small moving things that must read as floating or passable. Lamps are 7px circles; the only other circle on a surface is a radio.

### Named Rules
**The Square-Stamp Rule.** Radius 0 is the default for anything that sits in a panel. Anything rounded must be small, floating or transient; if it is none of those, it is a defect.

## Components

### Buttons
- **Shape:** square (0 radius), 1px frame, 8px/16px padding, legend caps at 13px.
- **Primary:** transparent fill, signal frame and ink; hover fills Signal Amber with enamel ink; pressed uses the deep signal.
- **Secondary:** transparent fill, silk frame at 55 %, silk ink; hover adds a 7 % silk wash.
- **Link:** no frame, muted steel ink, signal on hover — for identifiers that open something.
- **Disabled:** frame and ink drop to a 20 % silk whisper; never a filled ghost card.

### Plates
- **Corner Style:** square.
- **Background:** Plate tone, 1px frame at 55 % silk.
- **Signature:** the capability plate frames itself twice — an outer strong frame, 4px in, an inner hairline — the nameplate's double edge.
- **Internal Padding:** 12px inside the inner frame.
- **Elevation:** none; see Flat-Steel.

### Fields
- **Style:** Well fill, 1px hairline frame, square, 8px/10px padding, body type.
- **Focus:** the frame turns Signal Amber; hover widens the hairline to 55 % silk.
- **Radios:** standard control, signal dot, silent ring — the control itself stays platform-shaped inside the square world.

### Placards
- **Style:** the tone's wash at 10 %, a 1px frame in the same tone at 70 %, tone ink, square, 12px padding.
- **Tone:** info and caution use Signal Amber; success uses Run Green; failures frame in Stop Red while their text stays in Fault Ink.
- **Rule:** a placard is always fully framed. No edge bars, no stripes, no icon.

### Lamps
- **Style:** a 7px filled circle in the field it reports; the designed-off state is a hollow 1px ring, never a dimmed dot.
- **Use:** one per capability-plate field (model load state, backend, device, hotkey) and one per model row, where it states the artifact's state — solid in the state's tone, the hollow ring for "on disk, not checked yet".

### Icons
- **Style:** drawn marks on a 16-unit grid, 1.5-unit strokes, square-proportioned and unrounded (`src/ui_icons.rs`), rendered with iced's canvas in the palette's role ink — silk for secondary, signal for the primary control, dimmed when a control is unavailable. Six marks carry the whole vocabulary: chevron down/up (disclosure), download arrow (fetch), tick (verify), cross (cancel), cycle (retry).
- **Use:** icon-only controls, always with their word in a tooltip; a state is never stated by an icon alone — the state caption beside it does the naming. Hand-drawn geometry only: no font glyphs, no framework icon set, no bitmap assets.
- **Sizing:** 18px box on the lifted type ladder, one icon per control at the row's right edge.

### List Rows (model list)
- **Style:** one two-line field row per entry (`.spec/model-list-field-rows.md`): line 1 the identity and state — radio, name in the value face, download size in mono, the state's lamp and its one icon control (download / tick / cross / cycle, tooltip-worded); the state is never spelled out — the lamp's tone and the mark carry it, the word waits in the tooltip; line 2 the facts and the disclosure — the language summary, the quant chips as stamped mini-buttons, the language marker, then the Quality/Speed bars in their own slot and the chevron that opens the detail. The bar slot is anchored to the row's right edge, so the two bars stand on one axis across every row whatever the summary and the quants measure before them.
- **Selected:** the same row behind a 1.5px signal frame — selection is a frame, not a fill. The active model's row is pinned as the list's first row and not repeated in the sorted remainder.
- **Expansion:** the chevron grows the row in place (one row at a time) with the published values and the catalog record the row leaves out — parameter count, license, datasets, reference machine, file, SHA-256, URL. Never an inner scroll area: the page scroll is the only scroll.
- **Narrow widths:** the fact line wraps rather than clipping; the label and the language summary give up width first, so state, controls and the disclosure stay reachable at the window minimum (620×480).

### Overlay Pill
- **Style:** the one floating surface, and the only surface where a rounded corner and a
  translucent fill are the point — Plate tone at 90 % over whatever it covers, 16px radius,
  no frame, no shadow, 20px side inset with the status line 16px from the top edge.
- **Content:** a legend-caps status line (15px, Muted Steel — Fault Ink when the state is an
  error), the streaming transcript below it in Inter at 17px, wrapped and elided after two
  lines, and an 8px fully-rounded level bar 20px above the bottom edge: Signal Amber, Fault
  Ink while the state is an error, no track, growing from the left.
- **Status line:** single-line and elided while a transcript sits below it — those rows belong
  to the speech; with no transcript (errors, notices) it wraps onto the free second row,
  because a cut-off recovery is worse than a longer notice.

## Do's and Don'ts

### Do:
- **Do** state every capability the machine owns in the plate, with a lamp whose colour is the truth of that moment.
- **Do** stamp controls: legend caps, square frames, hover fills.
- **Do** keep failure text in Fault Ink and reserve Stop Red for frames.
- **Do** set keys, identifiers and percentages in their own faces — legend caps and mono — instead of styling prose.
- **Do** leave whitespace between a state and its action: the caption left, the button right.

### Don't:
- **Don't** round a plate, a field, a button or a placard — radius 0 is the contract.
- **Don't** introduce a second saturated colour, a gradient, a glow or a shadow.
- **Don't** use glyphs or emoji as icons; draw the mark on the icon grid or speak the word. An icon-only control keeps its word in a tooltip, and a row's state is read from its lamp beside the mark, never spelled out in the row.
- **Don't** stripe an alert; a placard carries its meaning in its frame and wash.
- **Don't** let a disabled control look interactive, and never dim a lamp to hide a state — draw it hollow.
- **Don't** run monospace outside data, and never upper-case prose.
