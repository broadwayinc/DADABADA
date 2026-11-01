# DADABADA

A schemaless record database server with writer-based access control (WBAC), written in Rust with RocksDB
compiled in. This file is for building and working on DADABADA itself, starting from a fresh machine. For
using it, see the documentation site in [`../doc-dadabada`](../doc-dadabada) (Getting started, API
reference, command line).

How to read the commands: a line starting with `$` is a command to type, without the `$`. Replace a
`<placeholder>` including its angle brackets. Lines without `$` show what the command prints.

## What you need

Everything below is installed in the steps that follow; nothing has to be there already except Linux and
a user that can run `sudo`.

| tool | needed for | installed in |
|---|---|---|
| Linux, x86_64 or arm64 | everything: DADABADA is built for and runs on Linux (tested on Ubuntu 22.04) | |
| C and C++ compilers, make | building: RocksDB, zstd, lz4 and the TLS library are compiled from source | step 1 (`build-essential`) |
| libclang | building: the RocksDB bindings are generated from its C headers | step 1 (`libclang-dev`) |
| curl, git | downloading Rust and the source code | step 1 |
| Rust (stable) with cargo | building: the compiler and the build tool | step 2 (rustup) |

On another Linux distribution, install the same things under that distribution's package names: a C and
C++ compiler with make, the libclang development package, curl and git. Steps 2 to 6 are the same.

## Start clean: from a fresh machine to a running server

### 1. Install the system packages

```bash
$ sudo apt-get update
$ sudo apt-get install -y build-essential libclang-dev curl git ca-certificates
```

### 2. Install Rust

Rust is installed with rustup, its official installer. It puts the compiler (`rustc`) and the build tool
(`cargo`) in `~/.cargo/bin` and adds that folder to your PATH for new terminals. No `sudo` is needed.

```bash
$ curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
```

It ends with:

```
Rust is installed now. Great!
```

Load it into the terminal you are in (new terminals do this by themselves), then check it:

```bash
$ . "$HOME/.cargo/env"
$ rustc --version
$ cargo --version
```

Both print a version number, such as `rustc 1.99.1 (58cf8f9c3 2026-10-13)`. Any current stable version
works.

### 3. Get the source code

```bash
$ git clone <repository URL>
$ cd dadabada
```

`<repository URL>` is the address of this repository, the one shown by the Code button on its web page.

### 4. Build

```bash
$ cargo build --release
```

cargo downloads the libraries DADABADA uses and compiles them with it. The first build compiles RocksDB
from source and takes a while: about 12 minutes on a 4-core machine. Later builds take under a minute.
It is finished when it prints a line like this (the time varies):

```
    Finished `release` profile [optimized] target(s) in 11m 48s
```

The program is `target/release/dadabada`.

### 5. Check the program

```bash
$ ./target/release/dadabada version
```

```
dadabada 0.1.2
```

### 6. Run it

Make a data folder for development and start the server. Keep the folder outside this repository; it
holds the database, its settings and a secret key.

```bash
$ ./target/release/dadabada init --data /tmp/dadabada-dev
$ ./target/release/dadabada start --data /tmp/dadabada-dev
```

The server answers on `http://localhost:7070` and keeps this terminal. In a second terminal, add a user
(the password you type is shown as you type it) and send requests from the user shell:

```bash
$ cd dadabada
$ ./target/release/dadabada admin --data /tmp/dadabada-dev user add alice --level 10
$ ./target/release/dadabada connect localhost:7070 --user alice
```

To use the web admin, set its password, then open `http://localhost:7070` in a browser:

```bash
$ ./target/release/dadabada admin --data /tmp/dadabada-dev web-password
```

To stop the server, press Ctrl-C in its terminal, or run
`./target/release/dadabada stop --data /tmp/dadabada-dev` from any terminal.

## Working on the code

After changing code, build again and restart the server: press Ctrl-C in the server's terminal and start it
again, or run `restart` from any terminal, which stops the server and starts the new build in that
terminal:

```bash
$ cargo build --release
$ ./target/release/dadabada restart --data /tmp/dadabada-dev
```

- The web admin is plain HTML, CSS and JavaScript in `src/ui/`. The files are compiled into the program,
  so there is no separate build step for them: edit, build, restart.
- `cargo build` (without `--release`) builds faster but makes a much slower program. Use the release build
  for anything that measures speed.

## Benchmarks

The scripts behind the performance figures on the docs site are in `tools/bench/`. They also need
PostgreSQL and its Python driver, installed next to the scripts; see
[`tools/bench/README.md`](tools/bench/README.md).

## When something goes wrong

| what you see | what it means and what to do |
|---|---|
| rustup says `cannot install while Rust is installed` | Rust was installed before in another way, usually with `apt`. Remove that one (`sudo apt-get remove -y rustc cargo`) and run step 2 again, so rustup's Rust is the one used. |
| `cargo: command not found` (or `rustc`) | Rust is installed but this terminal has not loaded it. Run `. "$HOME/.cargo/env"`, or open a new terminal. |
| ``error: linker `cc` not found`` | The C compiler is missing. Install `build-essential` (step 1). |
| The build stops saying it is unable to find libclang | Install `libclang-dev` (step 1). If it is installed, point the build at the folder that holds `libclang.so`, for example `export LIBCLANG_PATH=/usr/lib/llvm-14/lib`, and build again. |
| `Could not listen on 0.0.0.0:7070: Address already in use (os error 98)` | Another program (or another DADABADA) uses port 7070. Stop it, or make the data folder with another port: `init --data <folder> --port <port>`. |

## Where things are

| path | what it is |
|---|---|
| `src/main.rs` | the command line: `init`, `start`, `stop`, `restart`, `admin`, `connect` |
| `src/store.rs` | RocksDB: column families, batches, the outbox bookkeeping |
| `src/codec/` | key and value encoding (sort order, loose match, ids) |
| `src/engine/` | records, queries, access, users, agent keys, subscriptions, the background worker |
| `src/server/` | the HTTP API, TLS, `/v1/listen`, the admin socket, the web admin API |
| `src/shell/` | the admin shell and the user shell |
| `src/ui/` | the web admin (compiled into the program) |
| `install.sh` | the installer users run with `curl ... \| sh` |
| `tools/` | third-party notices, benchmarks |
| `target/` | build output (not kept in version control) |
