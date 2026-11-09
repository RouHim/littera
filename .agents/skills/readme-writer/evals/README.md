# readme-writer evals

Four cases live in `evals.json`. Stage each one in a temporary directory. Never run an eval in this repository.

- Littera: `git worktree add /tmp/littera-eval f64ff85` for the pre-screenshot README, then `cp assets/screenshot.png /tmp/littera-eval/screenshot.png`.
- `fixtures/gui-with-screenshot`: copy to a temp directory. `assets/app.png` is the supplied screenshot.
- `fixtures/gui-without-screenshot`: copy to a temp directory. It has no image.
- `fixtures/cli`: copy to a temp directory. It has no image.

Run each prompt once with the skill and once without it. Grade the README with `scripts/check_readme.py`, which covers the section order, word budget, fences, links, images, placeholders, em dashes and the GUI screenshot. The GUI-without-screenshot case is correct when no README is written and the response asks for a screenshot.
