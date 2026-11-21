# Contributing to tessera

Thanks for your interest! Bug reports, ideas, and pull requests are all welcome.

## Getting set up

1. Install [Rust](https://rustup.rs) with the default MSVC toolchain. The Visual
   Studio Build Tools ("Desktop development with C++") provide the linker.
2. Clone and build:

   ```powershell
   git clone https://github.com/radiumcoders/tessera
   cd tessera
   cargo build
   ```

3. Run the checks CI runs:

   ```powershell
   cargo fmt --check
   cargo clippy --all-targets -- -D warnings
   cargo test
   ```

## Running a development build

Run `cargo run` from a terminal. The log streams to that terminal, and
`Ctrl+C` exits cleanly, restoring every window. If a debugging session ever
leaves windows hidden, run `cargo run -- --restore`.

Only one instance can run at a time. Exit any installed tessera first
(`mod+Shift+E`).

## Where things live

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). In short:

- Layout math goes in `src/layout.rs`, config parsing in `src/config.rs`.
  Both are pure Rust, so add unit tests there.
- Behaviour goes in `src/wm.rs`. Keep Win32 calls behind helpers in `src/win.rs`.
- Callbacks and window procedures must never touch `Wm` directly. Post a
  message to the control window instead.

## Adding a dispatcher

1. Add a variant to `config::Command` and parse it in `config::parse_command`,
   with a test.
2. Handle it in `Wm::dispatch`.
3. Document it in `config/default.toml` and the README's dispatcher table.

## Commits and pull requests

- Keep each commit to one logical change, with a message that explains *why*.
- Format with `cargo fmt` and keep clippy warning-free.
- When you change behaviour, describe how you tested it. Mention your Windows
  version and monitor setup if relevant.

## Reporting bugs

Please include:

- your Windows version,
- tessera's version (`tessera --version`),
- your config (or say you use the defaults),
- the relevant part of `%LOCALAPPDATA%\tessera\tessera.log`.

`tessera --list-windows` output helps when a specific app misbehaves.
