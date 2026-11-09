---
version: 1
slug: "settings"
primary_target: "settings"
related_targets: ["onboarding", "problems", "overlay"]
---

# Surface: Settings

Scope: every surface that renders in-process — Settings (`src/ui_settings.rs`), Onboarding (`src/ui_onboarding.rs`), the startup-problems notice (`src/ui_problems.rs`) and the overlay pill (`src/overlay_render.rs`), all composed from the shared vocabulary in `src/ui.rs`. Visitor mode: Operate.

Audience and job: a Linux/Wayland user who opens Settings mid-work to change one thing and close it again, and a first-run user who has to get from install to a first dictation. Action: read the machine's real state and change one setting; every change applies at once.

Proof/content: real product states — backend, device, hotkey, verified / on-disk / downloading / not-downloaded / blocked, the /dev/uinput remediation with its exact remedy, the model directory.

Constraints: iced only, no new runtime deps, assets embedded; standard iced controls. Structure is frozen by the user's decision (2026-11-09, "restyle only"): every section, control, filter, sort, expansion and state stays. The world contributes type, palette, density and one signature move. Copy moves to plain language where a technical word carried no information.

Chosen direction: **The First Reader** (the roll, 2026-11-09). Memorable moment: the crayon rule under the row that is live, and one plain sentence at the top that says whether the machine is ready.

Unresolved: none.

## Direction contract

THESIS: the window explains itself the way a good reading book does — one plain sentence at a time, generous margins, one crayon colour marking where you are. It refuses the equipment-panel reading it replaces: condensed caps, stamped metal, double frames, monospace values, everything shouting at once.

OWN-WORLD: warm paper ground #FBF7EE, sheets #FFFDF6, ink #2C2721, muted #665E52, hairline rules #E7DECB, recessed wells #F1EADA; one crayon coral #C0442F used for action and the live rule; done-green #2E6B45; failure #8E2A20. One face, Atkinson Hyperlegible, in weight steps only. Sentence case everywhere, 14/17/19/20/26 ladder, 1.5 line height, 10–14px radii, 4/8/12/16/24/36 spacing.

STORY: the visitor reads one plain sentence that says whether dictation works, scans section titles, changes one thing, closes the window. Nothing needs decoding.

FIRST VIEWPORT: title, one readiness sentence, the four machine facts as a quiet labelled grid with state words (no lamps-only encoding), then "Model" with its filters and the list. The live row carries the crayon rule.

FORM: grounded direction "school primers and literacy readers", built as candidate 7 of the grounded list; seed:e4a7f36ca4f0c10f. Raised before build: hierarchy by weight, case and space instead of a bigger type size (from the timetable rack); every state carries its word beside its colour (from the transit diagram); exactly one saturated accent (from the data field).

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md and the DESIGN.json sidecar.
