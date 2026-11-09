---
name: readme-writer
description: "Write or rewrite a README for human readers. Use when the user asks to create, rewrite, shorten, condense, audit, restructure or review a README; when a README is too long, too technical, aimed at contributors instead of users, missing install steps, or missing a GUI screenshot; or when a repository needs a quickstart for end users. Applies to README files, not API reference or contribution docs."
---

# README writer

A README is the front door, not the manual. A visitor gives it about 90 seconds to answer four questions: what is this, why should I care, how do I install it, how do I use it. Everything else belongs in a linked document.

The job is to remove everything that does not help a new user install and run the thing.

A GUI README without a real screenshot is not a deliverable. If the app is GUI and no current screenshot exists, stop before writing and ask for one. Do not write a draft, a placeholder or a warning inside a finished README.

## Workflow

1. Read the repository before writing. Check the package manifest, existing README, license, docs, images, run scripts and install instructions. Facts come from the repository, never from habit.
2. Classify the project as GUI, CLI/TUI, library or service, or documentation-only. State the classification in the outline.
3. Decide the mode. If the user asked for an audit or review, report only and change no files. If the user asked to create or rewrite, continue.
4. Gate the screenshot. For a GUI app, find a current screenshot and get the user's approval for it. If none exists, stop here, ask for one and write nothing. This is a stop condition, not a warning to add later.
5. For an existing README, present the proposed outline and wait for explicit approval. A new README can be written directly.
6. Write the README in the fixed order below.
7. Move developer content to its linked document and add the link.
8. Run the static checker. Fix every failed check.
9. Report what changed, what moved, what is unverified and whether the screenshot is still missing.

## Fixed order

Use this order and keep the headings plain:

1. `# Project name`
2. Existing banner, if the repository already has one
3. One-line pitch
4. Screenshot or demo
5. Requirements
6. Install
7. Usage
8. Troubleshooting and help
9. License

Drop a section only when the project genuinely has nothing for it. A library has no GUI screenshot. Every README keeps install, usage and license.

The title is the product name, not a sentence. The pitch is one sentence under 120 characters: what the product does and for whom. Stack names belong in the pitch only when the stack is the point.

Bad:
> A robust, fully in-process image transformation pipeline leveraging modern GPU acceleration.

Good:
> A local photo converter that never uploads your files.

## Screenshot or demo

A GUI README has one real screenshot of the current main UI. A browser interface counts as a GUI. The image goes after the pitch and before the requirements. A banner is not a screenshot.

For an app whose main window is a settings screen, show that screen. It is the persistent UI a visitor recognizes.

Rules:

- No screenshot, no README. For a GUI app, stop before writing when no current screenshot exists. A README written without one is a failed run, even if the missing image is mentioned in the text.
- Look for an existing screenshot first. Open it and compare its visible labels or version with the current UI. If you cannot prove it is current, ask the user.
- If no current screenshot exists, ask the user for one. Offer to capture it only when the user explicitly asks for a capture and a working display and capture tool exist.
- Never launch, focus or drive a GUI without consent. Never click, type or inject input. If capture would take over the user's session, ask for a file instead.
- Never write a placeholder, mockup, stock image or invented image path. If the user supplies nothing, stop and say the README is blocked on the screenshot.
- The user owns redaction. Do not blur or crop personal data yourself unless asked. Flag visible personal data and ask for a redacted file.
- Store one local image in the repository's existing image directory, or `assets/` when none exists. Use a relative path and descriptive alt text. PNG or WebP, at most 500 KB and 1600 px wide.
- A CLI or TUI app uses a short terminal recording or a text output block instead of a screenshot.

## Install and usage

Install gives one primary copy-paste block of at most three commands. Put alternatives after it, not before it.

Usage gives the smallest working example: one command, one snippet, one first step. A feature tour belongs elsewhere.

## Move developer content

Contribution rules go to `CONTRIBUTING.md`. Build, release, architecture and operations detail goes to `DEVELOPMENT.md` or an existing documentation page. Create the destination when it is missing and link it from the README.

Preserve every command, path, number, version and link from the old README in the README or the moved document. A fact proven false is removed with the reason in the report. An unverifiable fact is preserved and flagged. Never silently drop a fact and never resurrect content that was already deleted from git history.

## Style

Aim for under 500 words of prose. The count includes list items and table cells. It excludes headings, code blocks, URLs and image markup.

- Use short sentences and plain verbs: is, has, does.
- No em dashes in prose.
- No AI staging: "not just X but Y", "it's not about X, it's about Y", one-line closers, "delve", "robust", "seamless", "testament", "landscape", "showcase".
- No badge walls, long feature lists, roadmaps, FAQs, contributor walls or architecture descriptions.
- Keep the repository's language. English is the default only when no other language is present.
- Markdown only. Leave a non-Markdown README untouched unless the user explicitly asks for a conversion.

## Verify

Run the bundled checker from this skill's directory:

```sh
python3 scripts/check_readme.py <repo>/README.md --kind gui
```

Use `--kind cli` or `--kind library` when the project is not GUI. The checker reports word count, section order, fences, relative links and images, placeholders, em dashes and the GUI screenshot. It does not run any install, build or run command.

Verification is static. Do not execute the project's commands, use the network or change the system. Do not commit or push.

## Audit mode

For an audit, write no files. Report the missing sections, the developer content that should move, the facts that are stale or unverifiable, the word count and the screenshot problem. Propose the outline. Stop there until the user asks you to apply it.

## Report

Finish with a short report:

- Changed files
- Content moved, with destinations
- Facts that could not be verified
- Screenshot status
- Checker result

## Before finishing

- The screenshot is real, current, local and approved.
- The pitch says what the product does in one sentence.
- The install block is copy-pasteable and first.
- The usage example is the smallest working one.
- Prose is under 500 words.
- No placeholder, em dash, badge wall or invented claim remains.
- Every relative link and image path resolves.
- The checker passes.
