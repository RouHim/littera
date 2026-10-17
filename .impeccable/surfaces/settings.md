---
version: 1
slug: "settings"
primary_target: "settings"
related_targets: []
---

# Surface: Settings

Scope: the Settings window (`src/ui_settings.rs` composed from the shared vocabulary in `src/ui.rs`), plus a restyle of the overlay pill (`src/overlay_render.rs`) in the same world. Visitor mode: Operate. Onboarding and the startup-problems window inherit the world through the shared vocabulary; their own compositions are not rebuilt in this pass.

Audience and job: a Linux/Wayland user who opens Settings deliberately — mid-work — to change one thing (model, language, hotkey, device, idle unload) and close it again. Action: read the machine's real state (backend, verification, blocked paths) and change one setting; every change applies at once, there is no Save.

Proof/content: real product states — capability fields (backend/device/model/hotkey), verified / on-disk / downloading / not-downloaded / blocked states, the /dev/uinput remediation with its exact command, the model directory.

Constraints: iced only, no new runtime deps, assets embedded; standard iced controls; the world contributes type, palette, density, and one signature move (mode rule, Operate).

Chosen direction: **The Nameplate** (decision round 2026-10-16; the pick card, chosen over the roll's assigned direction). Memorable moment: the single capability plate — stamped label/value fields with state lamps — sitting once above the working rows.

Unresolved: which OFL condensed legend face ships; final accent budget of the overlay pill; whether later surfaces (onboarding) repeat a plate or inherit only the vocabulary.

## Direction contract

THESIS: Settings is a precision instrument's data plate: the machine states its own truth in stamped fields. It refuses the category-default arrangement — the grey dashboard of cards-in-cards with a scattered accent — in favour of one plate and disciplined rows of field facts.

OWN-WORLD: Enamelled steel ground (#22282E), silk legends (#E9EBE4), signal yellow (#E3B23C) the only action/attention ink, stop red (#C14B3A) for failures, run green (#5E9E63) for ok. Condensed tracked caps for every legend, a workhorse face for values, mono for paths and commands; 1px hairlines; a compact field grid; state lamps and stamped field rows as the component language; placards with a thick left bar for real warnings.

STORY: The visitor reads the plate and knows the machine's state in one sweep — backend, device, model, hotkey — believes the app because it states plainly what it owns and what is blocked, and changes exactly one setting with a stamped control. Nothing is hidden, nothing is styled as decoration.

FIRST VIEWPORT: The 900×720 window opens directly on the capability plate ONCE — a 2×2 field grid with lamps — then the Model section: the model list as field rows (name, quant and size in mono, state stamp, one action); backend/status fields live in the plate; the idle-unload radio row; the /dev/uinput placard in stop red with its mono command; storage footer. No header strip (user decision, 2026-10-17). Signature interaction: the plate answers live — a transfer fills its row, verification flips a lamp, a failure lights its placard.

FORM: Grounded direction "The Nameplate" (machine nameplates and safety placards), rank 1 of 7 on my grounded list; locked as the pick card, not the roll's assignment. seed:f7629749430fa5de

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance.
