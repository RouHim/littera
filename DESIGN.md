---
name: Littera
description: Local voice dictation for Linux/Wayland — a window that explains itself in plain sentences.
colors:
  paper: "#FBF7EE"
  sheet: "#FFFDF6"
  well: "#F1EADA"
  rule: "#E7DECB"
  rule-strong: "#C9BCA3"
  ink: "#2C2721"
  muted: "#665E52"
  crayon: "#C0442F"
  crayon-hover: "#AE3A25"
  crayon-pressed: "#9C331F"
  paper-on-crayon: "#FFF9F0"
  done: "#2E6B45"
  caution: "#8A5A12"
  failure: "#8E2A20"
  failure-fill: "#A62B21"
typography:
  title:
    fontFamily: "Atkinson Hyperlegible, sans-serif"
    fontSize: "26px"
    fontWeight: 700
    lineHeight: 1.25
  heading:
    fontFamily: "Atkinson Hyperlegible, sans-serif"
    fontSize: "20px"
    fontWeight: 700
    lineHeight: 1.25
  lead:
    fontFamily: "Atkinson Hyperlegible, sans-serif"
    fontSize: "19px"
    fontWeight: 700
    lineHeight: 1.5
  body:
    fontFamily: "Atkinson Hyperlegible, sans-serif"
    fontSize: "17px"
    fontWeight: 400
    lineHeight: 1.5
  caption:
    fontFamily: "Atkinson Hyperlegible, sans-serif"
    fontSize: "14px"
    fontWeight: 400
    lineHeight: 1.5
  control:
    fontFamily: "Atkinson Hyperlegible, sans-serif"
    fontSize: "15px"
    fontWeight: 700
  data:
    fontFamily: "ui-monospace, monospace"
    fontSize: "14px"
    fontWeight: 400
rounded:
  control: "10px"
  sheet: "14px"
  overlay: "16px"
  pill: "999px"
spacing:
  xs: "4px"
  sm: "8px"
  md: "12px"
  lg: "16px"
  xl: "24px"
  xxl: "36px"
components:
  button-primary:
    backgroundColor: "{colors.crayon}"
    textColor: "{colors.paper-on-crayon}"
    typography: "{typography.control}"
    rounded: "{rounded.control}"
    padding: "10px 18px"
  button-primary-hover:
    backgroundColor: "{colors.crayon-hover}"
    textColor: "{colors.paper-on-crayon}"
    typography: "{typography.control}"
    rounded: "{rounded.control}"
    padding: "10px 18px"
  button-secondary:
    backgroundColor: "{colors.sheet}"
    textColor: "{colors.ink}"
    typography: "{typography.control}"
    rounded: "{rounded.control}"
    padding: "10px 18px"
  chip-active:
    backgroundColor: "{colors.sheet}"
    textColor: "{colors.crayon}"
    typography: "{typography.caption}"
    rounded: "{rounded.control}"
    padding: "2px 8px"
  sheet:
    backgroundColor: "{colors.sheet}"
    textColor: "{colors.ink}"
    rounded: "{rounded.sheet}"
    padding: "16px"
  field:
    backgroundColor: "{colors.well}"
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    padding: "10px 18px"
  overlay-pill:
    backgroundColor: "rgba(255, 253, 246, 0.96)"
    textColor: "{colors.ink}"
    rounded: "{rounded.overlay}"
    width: "560px"
    height: "112px"
---

# Design System: Littera

## Overview

**Creative North Star: "The First Reader"** (chosen 2026-11-09, replacing "The
Nameplate").

Littera's windows explain themselves the way a good reading book does: one plain
sentence at a time, generous margins, and one crayon colour marking where you
are. The Settings window opens on a sentence that answers the only question a
person has when they open it — can I dictate right now? — then states four
machine facts as label/value rows and gets out of the way.

The use scene fixes the light: a desk, a bright room or a dim one, the window
open for the fifteen seconds it takes to change one setting. Paper, not steel;
ink, not silk-screen. Nothing is condensed, nothing is stamped, nothing is
upper-case — the previous world read as lab equipment, and this one reads as a
page.

**Key Characteristics:**
- Warm paper ground, white sheets, one crayon accent.
- One typeface (Atkinson Hyperlegible) in two weights; hierarchy by weight and
  space, never by a fourth size.
- Sentence case everywhere; monospace only for real data.
- Every state says its word; colour is the second signal.
- Flat and softly rounded: 10px controls, 14px sheets, no shadow except menus.

## Colors

Colour is information, not decoration. The crayon marks the live row and the one
primary action; the semantic inks mark success, caution and failure; everything
else is paper, ink and hairlines.

### Primary
- **Crayon** (#C0442F): the live row's frame, the primary button's fill, the
  overlay's level bar, the selected onboarding step. 4.8:1 on paper.
- **Crayon Hover** (#AE3A25) and **Crayon Pressed** (#9C331F): the darker fills
  of a pressed or hovered primary. Never lighten on hover — a fill must stay
  darker than the paper it sits on.

### Neutral
- **Paper** (#FBF7EE): the window ground.
- **Sheet** (#FFFDF6): cards, model rows, the capability plate — one step
  lighter than the paper, so a sheet reads as a page laid on the desk.
- **Well** (#F1EADA): recessed interiors — the search field, pickers, an inset
  region inside a sheet.
- **Rule** (#E7DECB) and **Rule Strong** (#C9BCA3): the 1px hairline around a
  sheet and the heavier line of a hovered field.
- **Ink** (#2C2721): all text. 13.8:1 on paper.
- **Muted** (#665E52): keys, explanations, secondary lines. 6.0:1 on paper.

### Semantic
- **Done** (#2E6B45): verified, resident, ready, a passing check.
- **Caution** (#8A5A12): the model/language mismatch.
- **Failure** (#8E2A20): failure text, 7.9:1 — always this ink, never the fill.
- **Failure Fill** (#A62B21): failure frames and the overlay's error bar.

### Named Rules
**The One Crayon Rule.** Exactly one saturated accent exists. If an element is
neither live nor the primary action, it is ink, muted or a hairline.

**The Word-Before-Colour Rule.** A state is named in words and *then* toned. A
list where every row carries an attention colour has no attention left to spend
on the row that needs it — so "Not downloaded" is grey, and only "Ready",
"Downloading" and "Failed" carry ink.

## Typography

**One face:** Atkinson Hyperlegible (embedded, SIL OFL 1.1), drawn by the Braille
Institute for maximum letter distinction. Regular carries prose, values and
labels; Bold carries titles, headings, names, key chords and every control label.

**Data face:** the system monospace, for file sizes, paths, SHA-256 digests and
commands — never for prose or for tone.

### Hierarchy
- **Title** (700, 26px, 1.25): the surface title ("Set up Littera").
- **Heading** (700, 20px, 1.25): a section name ("Model", "Hotkey").
- **Lead** (700, 19px, 1.5): the readiness sentence and every key chord.
- **Body** (400, 17px, 1.5): values, explanations, placard text.
- **Control** (700, 15px): button and chip labels.
- **Caption** (400, 14px): field keys, metadata, hints.

### Named Rules
**The Sentence-Case Rule.** Nothing is set in upper case. A label reads like a
word a person says: "Microphone", "Not downloaded", "Change keys".

**The Weight-Not-Size Rule.** A new level of emphasis is a weight step or more
space; the scale has five steps and does not grow.

## Layout

One scrolling column per window, 24px outer padding, sections separated by 36px
and their content by 12px — more air above a heading than below it. Settings is
900×720 (minimum 620×480), onboarding 820×660, the problems notice 560×620, and
the overlay pill a fixed 560×112 floating 48px above the bottom edge.

The capability plate is four label/value rows in a 150px label column, preceded
by one bold sentence stating whether dictation works. A model row is two lines:
the radio, name, size and the one action on line 1; the language summary, the
quant chips, the state word, the quality/speed bars and "Details" on line 2. The
name keeps the whole slot line 1 leaves it, so it wraps rather than clipping at
620px — the state word moved to line 2 for exactly that reason. Nothing is ever
hidden behind the window edge.

## Elevation & Depth

Sheets are defined by their hairline and their tone step, not by shadow. One
thing floats and carries a soft shadow: an open pick-list menu over the content
it covers. The overlay card is flat paper with one hairline — it sits over other
applications' windows, not over a page it can borrow depth from. Nothing else
casts.

## Shapes

Softly rounded and consistent: 10px for controls, fields and chips; 14px for
sheets, rows and placards; 16px for the overlay; fully round only for radio
dots, the level bar and the onboarding step chips. Hairlines are 1px; a live row
thickens its frame to 1.5px rather than changing its fill.

### Named Rules
**The No-Card-Inside-A-Card Rule.** A grouped block inside a sheet is an inset: a
hairline with no fill of its own. A card inside a card is a defect.

## Components

### Buttons
- **Primary:** crayon fill, paper-on-crayon ink, 10px radius, 10px/18px padding,
  control type. Hover and pressed darken the fill.
- **Secondary:** sheet fill, ink text, `Rule Strong` hairline; hover switches the
  fill to the well tone.
- **Chip:** a quiet control at 2px/8px padding. Selected chips carry crayon ink
  and a crayon frame on a sheet — a chosen value must not look like the primary
  action.
- **Disabled:** the hairline drops to `Rule` and the ink to `Muted`; the control
  never keeps an active-looking fill.

### Sheets and insets
- **Sheet:** `Sheet` fill, 1px `Rule`, 14px radius, 16px padding. The live sheet
  adds a 1.5px crayon frame and keeps the same fill.
- **Inset:** a hairline with no fill and 12px padding, for a block inside a sheet.

### Fields
- **Style:** `Well` fill, 1px `Rule`, 10px radius, body type, 10px/18px padding.
- **Focus:** the frame turns crayon; hover turns it `Rule Strong`.
- **Radios:** a crayon dot in a `Rule Strong` ring; the ring turns crayon on
  hover.

### Placards
- **Style:** the tone's wash at 8%, a 1px frame in the tone at 32%, the tone's
  ink, 10px radius, 12px/16px padding. A placard is always fully framed.
- **Use:** a placard warns about one thing — its sentence names the cause and,
  where the fix is a literal command, a monospace line carries it (the hotkey
  binding in Settings). A list of failed checks is rows, not placards; there the
  remedy is prose (see System-check rows).

### List rows (model list)
- **Line 1:** radio, name at the lead step in regular weight (it wraps instead of
  clipping), size in mono, then the one action the state allows.
- **Line 2:** the language summary, the quant chips, the state in words, the
  Quality/Speed bars in their own slot, and "Details"/"Hide".
- **Selected:** the same sheet behind a 1.5px crayon frame; the row never
  changes its fill.
- **Expansion:** grows in place with the published measurements and the catalog
  record; the page scroll remains the only scroll.

### Capability plate
One sheet, four rows: Model, Backend, Microphone, Hotkey. Each value is a bold
worded statement — "Loads with the next dictation", "Ctrl + Super". No lamps: a
dot beside a word the reader has just read is one signal too many.

### System-check rows (problems notice)
- **Style:** an outline row per failed check — hairline, no fill — carrying a
  toned "Needs a fix" tag, the check's name in bold, its detail as prose, and a
  labelled "Fix" line in the reading face. A failure earns a placard only when it
  is a warning about one thing; three stacked failures read better as rows.
- **Re-check** is the view's primary action, **Close** the secondary one.

### Overlay pill
The one floating surface: a paper card at 96 % over whatever it covers, one
hairline so it exists on a white page, 16px radius, 20px side inset. A bold muted
status line ("Listening… 0:07"), the streaming transcript in body type, and a
6px fully-rounded level bar in crayon — failure ink while the state is an error.

## Do's and Don'ts

### Do:
- **Do** open a surface with one plain sentence that answers why it is open.
- **Do** state a state in words, then tone it.
- **Do** set sizes, paths, digests and file names in mono, and a command only
  where it is meant to be run verbatim.
- **Do** keep one primary action per view.
- **Do** leave more space above a heading than below it.

### Don't:
- **Don't** set anything in upper case, or reach for a condensed face.
- **Don't** add a second accent colour, a gradient, a glow or a shadow on a
  sheet.
- **Don't** nest a card inside a card — use an inset.
- **Don't** hide an action's word behind a tooltip.
- **Don't** use a bigger type size for emphasis; add weight or space.
