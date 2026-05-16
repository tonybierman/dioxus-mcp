# Contributing to dioxus-mcp

Thanks for your interest in contributing. This project welcomes bug reports,
feature requests, and pull requests.

## Contributor License Agreement

`dioxus-mcp` is dual-licensed: it is published under
[GPL-3.0-or-later](LICENSE) for open-source use, and a separate commercial
license is available for users whose use case is incompatible with GPL
(see the README). To keep this model viable, every contribution must come
with broad rights to the project maintainer.

**By submitting a pull request, opening a patch, or otherwise contributing
code, documentation, or other content to this repository, you agree that:**

1. Your contribution is your original work, or you have the right to submit
   it under the terms below.
2. Your contribution is licensed to the project and its users under the
   same terms as the project: **GPL-3.0-or-later**.
3. You additionally grant **Tony Bierman** (the project maintainer) a
   perpetual, worldwide, non-exclusive, royalty-free, irrevocable license
   to use, reproduce, modify, sublicense, and distribute your contribution
   under **any other license terms**, including proprietary or commercial
   licenses, without further notice or compensation.

This is the same model used by Sidekiq, Qt, and other dual-licensed
projects. It is what allows the commercial license offering to remain
available as the project grows.

If you cannot agree to clause 3, please do not submit a contribution.

## Pull request process

- Open an issue first for non-trivial changes so we can discuss scope.
- Run `cargo fmt`, `cargo clippy`, and `cargo test --workspace` before
  pushing.
- Keep PRs focused — one logical change per PR.
- Write commit messages in the existing style (see `git log`).

## Reporting bugs

Open a GitHub issue with:

- What you expected to happen
- What actually happened
- A minimal reproduction (a small Dioxus project, or the exact tool call
  and project layout that triggered the issue)
- `dioxus-mcp --version` output

## Questions

For commercial licensing inquiries, contact **tonybierman@gmail.com**.
For everything else, GitHub issues are the right venue.
