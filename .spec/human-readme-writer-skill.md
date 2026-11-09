# Feature Specification: Human README writer skill

**Created**: 2026-11-09
**Status**: Approved
**Input**: Create a reusable skill that writes short, human-first READMEs, with a real screenshot for GUI apps.

## Goal

A README visitor wants to know what a project is, whether it fits, and how to install it, without reading an operations manual. This feature adds a reusable agent skill that creates or rewrites a README for human readers: short, scannable, install-first, and illustrated with a real screenshot for GUI apps. Developer and operations detail moves to a linked document instead of staying in the README.

## User Scenarios

### Scenario 1 - A first-time visitor decides in about 90 seconds (P1)

A visitor opens a GUI project's repository. The README shows the product name, a one-line pitch, a current screenshot, the primary install command and the first usage step without a wall of prose first.

**Acceptance**

1. Given a GUI repository with a runnable app and a current screenshot, When the skill writes the README, Then the screenshot sits after the one-line pitch and before the requirements section.
2. Given a repository that already has a banner, When the skill writes the README, Then the banner stays directly under the title, above the pitch, and the screenshot is a separate image after the pitch.
3. Given the finished README, When a reader scans it, Then the title, pitch, screenshot, install block and smallest usage example appear before troubleshooting and license.
4. Given the finished README, When its prose is counted, Then it stays under 500 words, excluding headings, code blocks, URLs and image markup; list items and table cells count.

### Scenario 2 - A GUI app has no screenshot (P1)

The maintainer asks for a README, but the repository has no screenshot. The skill must not invent one or ship a placeholder.

**Acceptance**

1. Given a GUI repository with no screenshot, When the skill runs, Then it first looks for an existing current image, then asks the user for one, and never writes the README without a real screenshot.
2. Given the user explicitly asks the skill to capture the app, When a working display and capture tool exist, Then the skill captures the current main UI without taking focus or sending input.
3. Given the app cannot run, the capture fails, the environment has no display, or the user does not consent, When the skill runs, Then it stops and asks for a screenshot file.
4. Given the user supplies no screenshot, When the skill finishes, Then the README contains no placeholder, mockup, stock image or invented image reference.
5. Given a browser-based application, When the skill needs a screenshot, Then the same consent and fallback rules apply.

### Scenario 3 - An existing README has become an operations manual (P1)

A maintainer asks to shorten a README that contains build internals, release steps, library tables and architecture notes. The skill preserves the facts, moves the developer content, and gets approval for the new outline before rewriting.

**Acceptance**

1. Given an existing README with more than 500 words of prose, When the skill runs, Then it presents the proposed outline and waits for explicit approval before writing.
2. Given approval, When the skill rewrites the README, Then contribution rules move to `CONTRIBUTING.md` and build, release and architecture detail moves to `DEVELOPMENT.md` or an existing documentation page, and the README links to it.
3. Given the old and new documents, When the skill reports, Then every command, path, number, version and link from the old README is either preserved in the repository or listed as removed with a reason.
4. Given content that was already deleted from the README before the skill ran, When the skill rewrites, Then it does not resurrect that content from git history.

### Scenario 4 - A CLI or TUI app (P2)

A command-line project has no window to screenshot. The README still needs to show the tool working.

**Acceptance**

1. Given a CLI or TUI repository, When the skill writes the README, Then it uses a short terminal recording or a text output block instead of a GUI screenshot.
2. Given the install and usage sections, When a reader copies them, Then the primary install block and the smallest working example run without editing.

### Scenario 5 - A repository with several packages (P2)

A monorepo holds more than one application or package. The skill must not mix their install instructions.

**Acceptance**

1. Given a repository with multiple packages, When the skill starts, Then it identifies the README in scope or asks which package the user means.
2. Given the chosen package, When the skill writes, Then install, usage and requirements refer only to that package.
3. Given other packages with their own READMEs, When the skill finishes, Then those files are not modified.

### Scenario 6 - A non-English repository (P3)

An existing README is written in another language. The rewrite keeps that language.

**Acceptance**

1. Given a non-English existing README, When the skill writes the new one, Then the prose and section titles stay in that language.
2. Given a repository whose documentation is in English, When no language is specified, Then the skill writes in English.

### Scenario 7 - The user only wants an audit (P2)

A maintainer asks the skill to audit or review a README without asking for a rewrite. The skill reports findings and a proposed outline, and changes no files.

**Acceptance**

1. Given an existing README and an audit request, When the skill runs, Then it writes no files, including the README, developer documents and screenshots.
2. Given the audit report, When the user reads it, Then it names the missing sections, the developer content that should move, and any screenshot or fact problem.
3. Given a later instruction to apply the audit, When the skill writes, Then it follows the normal outline-approval flow.

## Functional Requirements

- **FR-001**: The skill is reusable across repositories, installed once for the user, and invoked automatically or by name when the user asks to create, rewrite, shorten, audit or restructure a README.
- **FR-002**: Before writing, the skill reads the repository's package manifest, existing README, license, documentation and image assets, and any run or install instructions it can find.
- **FR-003**: The skill classifies the project as GUI, CLI/TUI, library or service, or documentation-only, and states that classification in the proposed outline.
- **FR-004**: When a README already exists, the skill presents a proposed outline and waits for explicit approval before writing. An audit request produces a report and no file changes.
- **FR-005**: The README follows a fixed section order: title, optional existing banner, one-line pitch, screenshot or demo, requirements, install, usage, troubleshooting and help, license.
- **FR-006**: The one-line pitch says what the product does and for whom in one sentence; implementation language and stack names appear only when they are the point.
- **FR-007**: Every GUI README includes one real screenshot of the current main UI, placed after the pitch and before the requirements. Browser-based interfaces count as GUI apps.
- **FR-008**: The skill looks for an existing screenshot first, asks the user for one next, and captures only when the user explicitly asks for a capture and a working display and capture tool exist; capture must not take focus or send input.
- **FR-009**: A screenshot that shows an outdated UI is treated as missing; the skill inspects it against the current UI and version, asks when freshness cannot be proven, and never silently reuses a stale image.
- **FR-010**: The user owns redaction. The skill accepts a screenshot the user supplies or approves, flags visible personal data, and never guesses what is private.
- **FR-011**: A screenshot is a local repository asset in the repository's existing image directory or `assets/` when none exists, with a relative path and descriptive alt text. It is PNG or WebP, at most 500 KB and 1600 px wide.
- **FR-012**: CLI and TUI projects use a short terminal recording or a text output block in place of a GUI screenshot.
- **FR-013**: README prose stays under 500 words. The count includes list items and table cells, and excludes headings, code blocks, URLs and image markup.
- **FR-014**: Install gives one primary copy-paste block of at most three commands, with alternative routes listed after it.
- **FR-015**: Usage gives the smallest working example, not a feature tour or a set of variations.
- **FR-016**: Badge walls, long feature lists, roadmaps, FAQs, contributor walls and architecture descriptions are omitted from the README.
- **FR-017**: Developer and operations detail removed from the README is preserved in the repository: contribution rules in `CONTRIBUTING.md`, build, release and architecture detail in `DEVELOPMENT.md` or an existing documentation page. The skill creates the destination when it is missing and links it from the README.
- **FR-018**: Every command, path, number, version and link carried over from existing documentation is preserved in the README or the moved document. A fact proven false is removed with the reason in the report; an unverifiable fact is preserved and flagged.
- **FR-019**: The skill never invents a command, feature, number, compatibility claim or support channel. Facts it cannot verify are preserved and flagged, not silently dropped.
- **FR-020**: Troubleshooting names the project's own diagnostic command or the most common failure and its fix; help points to one support channel.
- **FR-021**: The license section is present and last; if no license is declared, the skill asks instead of guessing.
- **FR-022**: Before finishing, the skill verifies balanced code fences, resolving relative links and image paths, the presence of every required section, the word budget and the absence of placeholders.
- **FR-023**: Verification is static. The skill does not execute install, build or run commands, does not use the network, and does not change the system.
- **FR-024**: The skill reports what changed, what moved, what could not be verified, and which screenshot or asset is still missing.
- **FR-025**: The README language follows the existing README or primary documentation; English is the default when no other language is present.
- **FR-026**: The skill handles Markdown READMEs only. A non-Markdown README is left untouched unless the user asks for a conversion.
- **FR-027**: The skill does not modify application code, configuration, unrelated documentation, or git state. It never commits or pushes.

## Key Entities

- **Project profile**: The app type, primary install route, run command, diagnostic command, existing documentation and assets, and documentation language.
- **README outline**: The ordered sections, proposed omissions, and the destination for moved developer content, presented for approval before writing.
- **Screenshot asset**: A current image of the main UI, its local path, alt text, and whether the user has approved its redaction.
- **Developer document**: The linked destination that receives build, release, architecture and operations detail removed from the README.

## Edge Cases

- The app is GUI but cannot be built or run in the current environment.
- Screenshot capture needs a display server, a running compositor, or a login session that the skill does not have.
- A screenshot exists but shows an older interface or contains personal data.
- The repository declares no license.
- The repository has no README, no documentation, and no install instructions.
- The README is longer than 1,500 words and mixes user and developer content.
- The repository is a monorepo, or the root README describes several products.
- The project is a library, a service or documentation-only, where a GUI screenshot does not apply.
- The only install route needs privileges, hardware, a package manager or a running service.
- Documentation exists in more than one language.
- Commands in the existing README are stale or contradictory; the skill must flag them rather than silently pick one.
- The screenshot would exceed the page weight limit unless compressed.
- The existing README is not Markdown.
- The user asks for an audit but later wants the changes applied.

## Research Notes

- https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/customizing-your-repository/about-readmes — GitHub says a README covers what the project does, why it is useful, how to start and where to get help, and that longer documentation belongs elsewhere.
- https://uttir.com/blog/how-to-write-a-readme-that-actually-gets-read — The seven-section order puts a screenshot or demo third, budgets under 500 words of prose, asks for one install command and one usage example, and cuts badge walls, feature lists, roadmaps and FAQs.
- https://github.com/RichardLitt/standard-readme/blob/main/spec.md — Standard Readme confirms a short description, install and usage code blocks, a table of contents only for long files, and license as the last section; its library focus means only these transferable rules apply.
- https://thejunkland.com/blog/how-to-write-good-readme.html — A README states what the project is, how to install it, its options and its license, with copy-pasteable build and install instructions.
- https://skerritt.blog/make-popular-open-source-projects/ — A strong header pairs a logo with a one-line slogan and a quick install, and images or GIFs show the tool in use.

## Assumptions

- The skill is named `readme-writer`, installed once for the user, and reused across repositories with automatic and explicit invocation.
- The style checklist is embedded in the skill. It does not depend on the `humanizer` skill.
- The fixed section order and the 500-word prose budget apply to every project type; optional sections are omitted rather than invented.
- Developer detail moves by purpose: contribution rules to `CONTRIBUTING.md`, build, release and architecture detail to `DEVELOPMENT.md` or an existing documentation page. The skill creates the destination when it is missing.
- The first acceptance case is Littera, whose README has no screenshot; its first screenshot is captured on a headless Wayland session or supplied by the user.
- Content already deleted from a README is not resurrected from git history.
- The 500-word budget counts list items and table cells, and excludes headings, code blocks, URLs and image markup.
- English is the default output language, and an existing non-English README overrides it.
- Screenshot capture is opt-in; the user supplies the image when capture is unavailable or declined.
- The skill is built and evaluated with the `skill-creator` skill.

## Success Criteria

- **SC-001**: The Littera README gains a current screenshot placed after the pitch and before requirements, and its prose stays under 500 words.
- **SC-002**: For a GUI repository with no screenshot and no capture consent or display, the skill writes no README and requests a screenshot; the output contains zero placeholders or invented image paths.
- **SC-003**: For an existing README over 500 words, the rewrite preserves or explicitly accounts for 100 percent of its commands, paths, numbers, versions and links, verified by diff, and moves developer detail into a linked document.
- **SC-004**: On four fixtures (Littera, a GUI app with a screenshot, a GUI app without one, and a CLI app), every output either has the required sections in the fixed order and no broken relative link, image path or code fence, or is a correct no-write audit/ask-for-screenshot result.
- **SC-005**: Every output contains a primary install block and a smallest working usage example before troubleshooting and license.
- **SC-006**: Every output contains no em dashes, no chatbot residue, no placeholder text and no invented feature or compatibility claim.
- **SC-007**: The skill's final report names the changed files, moved content, unverified facts and any missing screenshot or asset.
- **SC-008**: An audit request produces a report and changes zero files.
- **SC-009**: A non-Markdown README is left unchanged unless the user explicitly requests a conversion.
