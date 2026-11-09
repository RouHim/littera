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
2. Given the finished README, When a reader scans it, Then the title, pitch, screenshot, install command and smallest usage example appear before troubleshooting and license.
3. Given the finished README, When its prose is counted, Then it stays under 500 words excluding code blocks, image markup and headings.

### Scenario 2 - A GUI app has no screenshot (P1)

The maintainer asks for a README, but the repository has no screenshot. The skill must not invent one or ship a placeholder.

**Acceptance**

1. Given a GUI repository with no screenshot and an environment where the app can run, When the skill runs, Then it captures the current main UI and saves it as a local repository asset.
2. Given the app cannot run, the capture fails, or the environment has no display, When the skill runs, Then it stops before writing the README and asks the user for a screenshot.
3. Given the user supplies no screenshot, When the skill finishes, Then the README contains no placeholder, mockup, stock image or invented image reference.

### Scenario 3 - An existing README has become an operations manual (P1)

A maintainer asks to shorten a README that contains build internals, release steps, library tables and architecture notes. The skill preserves the facts, moves the developer content, and gets approval for the new outline before rewriting.

**Acceptance**

1. Given an existing README with more than 500 words of prose, When the skill runs, Then it presents the proposed outline and waits for explicit approval before writing.
2. Given approval, When the skill rewrites the README, Then developer and operations detail moves to a linked developer document, which is created or extended if it does not already contain that detail.
3. Given the old and new README, When the skill reports, Then every command, path, number, version and link from the old README is either preserved verbatim or listed as intentionally removed.

### Scenario 4 - A CLI or TUI app (P2)

A command-line project has no window to screenshot. The README still needs to show the tool working.

**Acceptance**

1. Given a CLI or TUI repository, When the skill writes the README, Then it uses a short terminal recording or a text output block instead of a GUI screenshot.
2. Given the install and usage sections, When a reader copies them, Then the primary install command and the smallest working example run without editing.

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

## Functional Requirements

- **FR-001**: The skill is reusable across repositories and can be invoked by name or triggered when a user asks to create, rewrite, shorten, audit or restructure a README.
- **FR-002**: Before writing, the skill reads the repository's package manifest, existing README, license, documentation and image assets, and any run or install instructions it can find.
- **FR-003**: The skill classifies the project as GUI, CLI/TUI, library or service, or documentation-only, and states that classification in the proposed outline.
- **FR-004**: When a README already exists, the skill presents a proposed outline and waits for explicit approval before writing.
- **FR-005**: The README follows a fixed section order: title, one-line pitch, screenshot or demo, requirements, install, usage, troubleshooting and help, license.
- **FR-006**: The one-line pitch says what the product does and for whom in one sentence; implementation language and stack names appear only when they are the point.
- **FR-007**: Every GUI README includes at least one real screenshot of the current main UI, placed after the pitch and before the requirements.
- **FR-008**: The skill attempts to capture a screenshot when the app can run; if capture fails or is impossible, it requests one from the user and does not write the README until a real screenshot exists.
- **FR-009**: Screenshots are local repository assets with relative paths and alt text, contain no personal data, and are compressed for a fast page load.
- **FR-010**: A screenshot that shows an outdated UI is treated as missing; the skill asks for a current one instead of reusing it.
- **FR-011**: CLI and TUI projects use a short terminal recording or a text output block in place of a GUI screenshot.
- **FR-012**: README prose stays under 500 words, excluding code blocks, image markup and headings.
- **FR-013**: Install gives one primary copy-paste command first and lists alternative routes only when the project supports them.
- **FR-014**: Usage gives the smallest working example, not a feature tour or a set of variations.
- **FR-015**: Badge walls, long feature lists, roadmaps, FAQs, contributor walls and architecture descriptions are omitted from the README.
- **FR-016**: Developer and operations detail removed from the README is preserved in a linked developer document; if none exists, the skill creates or extends one.
- **FR-017**: Every command, path, number, version and link carried over from existing documentation is preserved verbatim.
- **FR-018**: The skill never invents a command, feature, number, compatibility claim or support channel; facts it cannot verify are left out or listed as unverified.
- **FR-019**: Troubleshooting names the project's own diagnostic command or the most common failure and its fix; help points to one support channel.
- **FR-020**: The license section is present and last; if no license is declared, the skill asks instead of guessing.
- **FR-021**: Before finishing, the skill verifies balanced code fences, resolving relative links and image paths, and the presence of every required section.
- **FR-022**: The skill reports what moved, what could not be verified, and which screenshot or asset is still missing.
- **FR-023**: The README language follows the existing README or primary documentation; English is the default when no other language is present.
- **FR-024**: The skill does not modify application code, configuration or unrelated documentation.

## Key Entities

- **Project profile**: The app type, primary install route, run command, diagnostic command, existing documentation and assets, and documentation language.
- **README outline**: The ordered sections, proposed omissions, and the destination for moved developer content, presented for approval before writing.
- **Screenshot asset**: A current image of the main UI, its local path, alt text, and whether private data has been removed.
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
- The screenshot would exceed a reasonable page weight unless compressed.

## Research Notes

- https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/customizing-your-repository/about-readmes — GitHub says a README covers what the project does, why it is useful, how to start and where to get help, and that longer documentation belongs elsewhere.
- https://uttir.com/blog/how-to-write-a-readme-that-actually-gets-read — The seven-section order puts a screenshot or demo third, budgets under 500 words of prose, asks for one install command and one usage example, and cuts badge walls, feature lists, roadmaps and FAQs.
- https://github.com/RichardLitt/standard-readme/blob/main/spec.md — Standard Readme confirms a short description, install and usage code blocks, a table of contents only for long files, and license as the last section; its library focus means only these transferable rules apply.
- https://thejunkland.com/blog/how-to-write-good-readme.html — A README states what the project is, how to install it, its options and its license, with copy-pasteable build and install instructions.
- https://skerritt.blog/make-popular-open-source-projects/ — A strong header pairs a logo with a one-line slogan and a quick install, and images or GIFs show the tool in use.

## Assumptions

- The skill is installed once for the user and reused across repositories, with automatic and explicit invocation.
- The fixed section order and the 500-word prose budget apply to every project type; optional sections are omitted rather than invented.
- "Developer detail moves to a linked document" means `CONTRIBUTING.md` or an existing `docs/` page, and the skill creates one when neither exists.
- The first acceptance case is Littera, whose README currently has no screenshot.
- The 500-word budget counts prose only, not code blocks, image markup or headings.
- English is the default output language, and an existing non-English README overrides it.
- Capturing a screenshot is allowed when the app can run in the current environment; otherwise the user supplies one.

## Success Criteria

- **SC-001**: The Littera README gains a current screenshot placed after the pitch and before requirements, and its prose stays under 500 words.
- **SC-002**: For a GUI repository with no screenshot and no way to capture one, the skill writes no README and requests a screenshot; the output contains zero placeholders or invented image paths.
- **SC-003**: For an existing README over 500 words, the rewrite preserves or explicitly accounts for 100 percent of its commands, paths, numbers, versions and links, verified by diff, and moves developer detail into a linked document.
- **SC-004**: On sample GUI, CLI and library repositories, every output has the required sections in the fixed order and contains no broken relative link, image path or code fence.
- **SC-005**: Every output contains a primary install command and a smallest working usage example before troubleshooting and license.
- **SC-006**: Every output contains no em dashes, no chatbot residue, no placeholder text and no invented feature or compatibility claim.
- **SC-007**: The skill's final report names the moved content, the unverified facts and any missing screenshot or asset.
