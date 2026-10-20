# Contribution guidelines

Thank you for considering contributing to Dismas, a fork of Amberol!

You can help by reporting bugs, suggesting features, improving the
documentation, translating the strings added in Dismas, or writing code.

Dismas is separate from [upstream Amberol](https://gitlab.gnome.org/World/amberol).
Report problems with Dismas here, not to the upstream project, and don't send
its changes upstream: upstream has its own contribution policy, which doesn't
allow AI-generated work.

## Reporting bugs

Open an issue on this repository's issue tracker, and include:

 0. the commit you built from
 0. your operating system, and whether you're running a Flatpak or a native build
 0. the steps to reproduce the issue
 0. what you expected to happen
 0. what happened instead; screenshots are welcome

If the issue includes a crash, also include:

 0. any warnings printed in the terminal
 0. a backtrace, from GDB or LLDB, or from running with `RUST_BACKTRACE=1`

Screen recordings are welcome as well, but describe the problem in enough
detail that someone can reproduce it without watching the video.

For small fixes, such as typos, comment clean-ups, CI or build changes, open a
pull request directly without filing an issue first.

### Security issues

Report security issues privately, through GitHub's private vulnerability
reporting, rather than in a public issue.

### Features and enhancements

When you propose a feature, explain:

 0. what you're trying to achieve
 0. how other applications handle it, if they do
 0. any design changes it needs

For anything big, open an issue to discuss it before you start writing code.

## Use of AI tools

Contributions made with AI tools such as Claude are welcome, in code,
documentation, issues and artwork. Much of Dismas was written that way.

You remain responsible for what you submit:

 - **Understand it.** Read and review the changes before opening a pull
   request, and be ready to explain them and answer questions about them.
 - **Test it.** Build the app, run it, and run the checks below. Don't submit
   changes you haven't tried.
 - **Keep it focused.** One change per pull request, matching the style of the
   code around it. Don't include unrelated reformatting or rewrites.
 - **Credit it.** Note AI assistance in the commit message with a
   `Co-Authored-By:` trailer naming the tool, for example:

   ```plain
   Co-Authored-By: Claude <noreply@anthropic.com>
   ```

The repository has a [`CLAUDE.md`](./CLAUDE.md) file describing how the code
is organized. It is written for Claude Code, but people will find it useful
too.

## Your first contribution

### Prerequisites

You will need:

 - Meson and Ninja
 - the Rust compiler and Cargo
 - `blueprint-compiler` and gettext
 - the development files for gtk4 ≥ 4.19.5, libadwaita ≥ 1.8, GStreamer ≥ 1.20
   (including plugins-bad), SQLite, libseccomp and fontconfig
 - the glycin loaders, plus `bwrap` outside Flatpak, to decode cover art

GNOME Builder can also build the project from the Flatpak manifest,
`io.github.dachshund7.Dismas.json`, and download the dependencies for you.

### Getting started

Fork this repository on GitHub and clone your fork:

```sh
$ git clone git@github.com:yourusername/dismas.git
$ cd dismas
```

Configure and build with Meson, which runs Cargo itself:

```sh
$ meson setup _build
$ meson compile -C _build
```

Run the app without installing it:

```sh
$ meson devenv -C _build ./src/dismas
```

Run `meson setup` before using Cargo directly. It generates `src/config.rs`,
which Cargo, rustfmt and clippy need.

Create a branch to work on:

```sh
$ git switch -c your-branch
```

When you're done, push the branch to your fork and open a pull request.

### Checks

CI runs these on every pull request, but it's quicker to run them before you
push:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
build-aux/checks.sh                # formatting and spelling
reuse lint                         # licensing headers
```

Every source file needs `SPDX-FileCopyrightText` and `SPDX-License-Identifier`
headers.

### Translations

The `po/*.po` files come from upstream Amberol's translators, so leave the
existing translations alone. If you add translatable files, list them in
`po/POTFILES.in`.

### Commit messages

Format commit messages like this:

```plain
Short explanation of the commit

Longer explanation of exactly what changed, whether any external or
private interfaces changed, what bugs were fixed (with an issue reference
if there is one) and so forth. Be concise but not too brief.

Closes #1234
```

 - The first line is a single sentence of at most 72 characters. Start it
 with a capital letter, unless it starts with a lowercase identifier, and
 don't end it with a period. Follow it with a blank line.

 - The body is ordinary prose. Explain **why** you changed something; the
 diff already shows how.

 - If the commit addresses an issue, use `Closes #1234` or `Fixes #1234`, and
 GitHub will close the issue when the pull request is merged.

 - When committing someone else's work, use `--author`, e.g.
 `git commit --author "Joe Coder <joe@coder.org>"`.
