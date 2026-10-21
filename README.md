# veloci

A package manager that resolves fast and downloads in parallel.

veloci is written from scratch in Rust. It is not a wrapper around `pacman`
or `apt`: the resolver, the index format and the downloader are its own, and
they are not tied to a distribution. veloci reads an index — a list of
packages with their dependencies, versions, files and checksums — and works
out what to install and how to get it.

Two things it does well:

* **Resolution.** The whole dependency closure of a set of packages is worked
  out in parallel, and the result is a verified install order.
* **Downloading.** Every package of a closure is fetched at the same time,
  checksummed as it arrives, resumed if it breaks off, and served from a
  local cache afterwards.

On the Arch package graph (16,318 packages) it resolves the full closure of
`neovim firefox rust git base-devel` — 373 packages — in **3 ms**, and
downloads all 373 of them, checksums verified, in about **10 seconds** with 16
parallel connections. Against a registry of release archives it resolves in
**0.1 ms** and downloads a package in **0.7 s**.

## Why it is fast

* **Wave-based parallel resolution.** The resolver expands the whole frontier
  of newly discovered packages at once instead of recursing one dependency at
  a time, so a wide graph collapses into a handful of rounds.
* **A packed index.** Parsing a 5.7 MB JSON index costs more time than
  resolving it. The packed binary index loads in 34 ms and resolves in 3 ms;
  the same data as JSON takes 120 ms just to parse.
* **Installing.** A package is unpacked and its binaries are linked into
  `~/.local/bin`, with a receipt that says what was done so the package can
  be removed again.
* **A checksum cache with a sidecar marker.** A cached artifact is trusted
  from a small `.sha256` file next to it, so the common case costs one tiny
  file read instead of re-hashing megabytes. The marker also records the size,
  so a truncated or replaced file is detected and re-fetched.
* **Resumable downloads.** An interrupted transfer continues with an HTTP
  `Range` request. The running digest is seeded with the bytes already on
  disk, so a resumed file is verified as a whole.
* **Mirror fallback.** Every mirror of a repository is stored in the index, so
  a package that is missing from the first mirror is fetched from the next.

## Install

```sh
cargo install --path .
```

## Use

```sh
# Fetch the index of the configured repositories.
velo sync

# Fetch an index from a url and remember it for the next sync.
velo sync https://raw.githubusercontent.com/homu-sys/oxide-receipts/main/index

# Resolve a set of packages and print the install order.
velo resolve neovim firefox

# Check that every dependency comes before its dependent.
velo verify neovim firefox rust git base-devel

# Download a closure in parallel, verifying every checksum.
velo download neovim

# Install a package, and everything it depends on.
velo install bat

# Install without the confirmation.
velo install --yes bat

# List what is installed, and remove it again.
velo list
velo remove bat

# Pack a JSON index into the fast binary format.
velo index pack index.json index.veloci

# Download an index and pack it in one go.
velo index fetch https://example.org/index.json index.veloci
```

`--index` takes a path to a JSON or packed index, or a url to fetch the index
from. Without it, veloci uses the index `velo sync` wrote, and falls back to
an `index.json` in the working directory. `VELOCI_INDEX`, `VELOCI_CACHE` and
`VELOCI_CONCURRENCY` are read from the environment.

## Installing

`velo install` resolves the package, downloads it with everything it depends
on, unpacks the archive into `~/.local/share/veloci/packages/<name>/` and
links the binaries into `~/.local/bin`. Nothing needs root, and the binaries
are on the `PATH` right away.

The binaries to link come from the index when it names them
(`"install": {"bins": ["rg"]}`). When it does not, veloci looks for an
executable named after the package, then for the executables at the top level
of the archive, and finally for the ones further down, skipping the shared
libraries. Every install writes a receipt to
`~/.local/share/veloci/receipts/<name>.json`, which is what `velo remove`
uses to undo it.

An install asks before it does anything, and shows what it is about to
install:

```
$ velo install wasmer
loaded index in 1.940ms
resolved 1 packages in 159.220µs

Install packages:
  - wasmer 7.5.0 (288.7 MB)

Do you want to install these packages? (Y/n)
downloaded 1 packages in 1.863s (154.9 MB/s)
Installing wasmer 7.5.0...
  installed wasmer 7.5.0
  /home/homu/.local/bin/wasmer
installed 1 packages in 4.926s
```

`--root` and `--bin` move the installation somewhere else, for a system wide
install:

```sh
velo install --root /opt/veloci/packages --bin /usr/local/bin bat
```

## Syncing

`velo sync` downloads the index of every configured repository, merges them
into one, writes it to `~/.cache/veloci/index.veloci` in the packed format,
and reports what changed:

```
synced 1004 packages from 1 repositories in 125ms
2 added, 1 updated, 0 removed, 1001 unchanged
added   bat
added   ripgrep
updated zoxide 0.9.8 -> 0.9.9
```

The repositories are listed in `~/.config/veloci/config.toml`:

```toml
[[repo]]
name = "oxide"
url = "https://raw.githubusercontent.com/homu-sys/oxide-receipts/main/index"
```

`velo sync <url>` adds a repository to that file, so the first sync needs no
configuration file at all.

## The index

An index is a list of packages. The canonical JSON shape is:

```json
{
  "packages": [
    {
      "name": "popt",
      "version": "1.19-2",
      "deps": ["glibc"],
      "provides": ["libpopt.so=0-64"],
      "conflicts": [],
      "url": "https://example.org/core/os/x86_64/popt-1.19-2-x86_64.pkg.tar.zst",
      "size": 78173,
      "sha256": "877635962b604efa79409c3d423f4fdffc4868776af3e0e804cc94a0b57ca151"
    }
  ]
}
```

Indexes are usually written by another tool, and every tool names its fields
differently, so the loader is deliberately forgiving. It accepts a bare array
as well as an object with a `packages` key, and it understands
`pkgname`/`name`, `pkgver`/`version`, `dependencies`/`deps`/`requires`,
`download_url`/`url`, `csize`/`size`, `sha256sum`/`sha256`/`checksum`, and a
package wrapped in a `{"package": ..., "archive": ...}` envelope. A nested
`{"dependencies": {"depends": [...]}}` is understood as well, and so are
`"install": {"bins": [...]}` and `"strip_prefix"`.

### Mirrors

A package may point at a mirror set, and the downloader falls back to the
other mirrors when one of them does not have the file:

```json
{
  "mirrors": [
    ["https://mirror.one.example.org/repo/{file}", "https://mirror.two.example.org/repo/{file}"]
  ],
  "packages": [
    { "name": "a", "version": "1", "url": "https://mirror.one.example.org/repo/a.pkg", "mirror": 0 }
  ]
}
```

`{file}` is replaced with the file name of the package's own url.

### Versions

Versions use the `epoch:version-release` form every distribution settled on,
and they are compared the way `pacman` does it, because that is the
behaviour users expect:

* A dependency without a release matches any release: `libelf=0.196` is
  satisfied by `libelf 0.196-1`, while `libelf=0.196-2` is not.
* The version part has to match exactly: `alsa-topology-conf=1.2.5` is *not*
  satisfied by version `1.2.5.1-4`.
* The version of a provide is compared the same way, so `libverto.so=1-64`
  picks the 64-bit build and `libverto.so=1-32` the 32-bit one.

When several packages provide the same name, the one with the highest version
becomes the canonical provider, so the result does not depend on the order
the packages happened to be resolved in.

## Cycles

Real package graphs contain cycles — Arch ships `mesa` <-> `libglvnd` and
`librsvg` -> `gdk-pixbuf2` -> `glycin` -> `librsvg`. veloci breaks such a
cycle at the member with the fewest unmet dependencies and reports it:

```
CYCLE: libglvnd -> mesa (broken)
OK: 373 packages, dependency order verified
```

pacman and apt break cycles the same way. Refusing to install would be worse
than picking an order.

## Backends

The resolver and the downloader only see the `Repository` trait, so any
backend can feed them. `MemoryRepo` is the in-memory implementation used by
the tests; a backend that has an index file can use `Index`, which reads
JSON or the packed binary format.

## Development

```sh
cargo test          # unit, index, resolver and downloader tests
cargo clippy --all-targets
cargo bench         # resolver throughput
```

## License

Apache-2.0. See [LICENSE](LICENSE).

## Optional read:

- This README was made based on an LLM, feel free to fork this repository or populate.
