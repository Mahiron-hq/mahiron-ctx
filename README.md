# mahiron-ctx

Consolidate a codebase into a single, faithfully reproduced document — for pasting into a
model, attaching to a review, or archiving as a snapshot.

Installed as two binaries: `mahiron-ctx` and the shorter alias `mhrn`. They are the same
program.

```sh
mhrn                                   # package the current directory into ./<name>-mhrn.md
mhrn ./api ./web -o context.xml        # several sources, format inferred from the name
mhrn --include '**/*.rs' --compress    # only Rust, reduced to structural signatures
mhrn https://github.com/owner/repo.git # one remote snapshot, retrieved and discarded
mhrn --mcp-server                      # serve the same engine to an agent
```

## What it is for

Any tool can concatenate files. The value here is that the document is a faithful copy:
no injected whitespace, no byte order marks leaking into content, no normalised line
endings, no re-indentation, no invented trailing newline inside a file's content, no
approximate decoding of a file it could not read. Where a file cannot be reproduced
exactly, it is excluded and reported rather than reproduced approximately.

Everything else follows from that. Transformations exist but are strictly opt-in.
Delimiters are derived from each file's own content, so a file containing the delimiter
cannot break the document that carries it. Statistics describe the document that was
actually produced, not the one the pipeline intended to produce.

## Architecture

```
config     settings, pattern compilation and specificity, precedence constants
engine     the whole of the packaging decision
  source     designations become local roots (local paths, one remote snapshot)
  discovery  parallel traversal collecting paths and metadata only
  ignore_rules  layered ignore resolution with per-rule attribution
  classify   content-based text/binary/undetermined classification
  decode     encoding detection, byte order mark removal, strict decoding
  transform  the opt-in mechanical transformations
  compose    ordered streaming of content into the chosen writer
output     one writer per format, plus the structural overview
compress   the language-compressor trait and its registry
tokens     streaming and second-pass token estimation
report     per-file records, warnings, statistics, console rendering
delivery   file, standard output, clipboard, dry run
cli        argument parsing and console reporting
mcp        Model Context Protocol server over stdio or SSE
watch      repeat a run when a source changes
```

Two properties hold the design together.

**One engine, many interfaces.** `engine::Engine::run` is the only thing that decides what
a run does. The CLI and the MCP server are both consumers of it. The MCP server goes
further and translates each request into the exact command line it stands for, so the two
surfaces cannot drift apart in capability.

**Files appear in the order the overview draws them.** Segment by segment, directories
before files at each level. A reader who has just looked at the tree finds the sections in
the sequence they expect, rather than in raw byte order, which interleaves a directory's
contents with its siblings because `/` sorts after `.` and `-`.

**Nothing is tokenized twice and nothing is copied without reason.** The encoding tables
are built once per process rather than per call; the token count is produced by a pool of
workers fed at the same safe boundaries the single-threaded path used, so parallelism
changes the speed and not the number; and file content is borrowed from the bytes read
until something actually rewrites it.

**Discovery is metadata-only.** Traversal runs in parallel and collects paths, sizes and
classifications — never content. The candidate list is sorted once, and composition then
streams each file's content through in that order. Memory stays proportional to the
largest single file rather than to the size of the tree, and the output is byte-identical
across runs regardless of how the traversal threads interleaved.

Composition writes the file sections into a spool first, then emits the preface, the
structural overview and the spooled sections into the destination. That extra step is what
lets the preface state the number of files the document actually contains — a file that
turns out to be unreadable at composition time is dropped from both the document and the
count, and the summary and the document therefore always agree.

The document is named after what it holds: packaging `my-project` writes
`./my-project-mhrn.md`, and a remote repository is named after the repository. `-o` names
it whatever you like. A run never packages its own output document, so running the same
command twice does not double the result or distort the per-file statistics.

## Rule precedence

Highest first:

1. `--include` / `--exclude` given at invocation
2. the tool's own ignore file (`.mahironignore`, `.mhrnignore`)
3. version-control ignore rules (`.gitignore`, `.ignore`, `core.excludesFile`, `.git/info/exclude`)
4. built-in defaults (hidden entries, well-known generated and dependency directories,
   lock files — `Cargo.lock`, `package-lock.json`, `go.sum` and the rest, which are
   machine-generated, enormous and of no use to a reader — and files whose names identify
   them as credentials: `.env`, `id_rsa`, `.npmrc`, anything ending `.pem` or `.key`)

Within (1), the more specific pattern wins: a pattern is scored by how much of it is
literal and how deeply it is anchored, less what it leaves open with wildcards. Where two
patterns are equally specific, the exclusion wins, so the resolution is always defined and
never depends on argument order. Within (2) and (3), a deeper ignore file overrides a
shallower one.

Precedence applies to directories exactly as it does to files. `--include '.github/**'`
descends into a directory the built-in defaults would otherwise prune, and `--exclude
'node_modules/**'` prunes the subtree at its boundary rather than walking all of it to
reject each file in turn.

The credential category is name-based, which means it is predictable rather than clever:
it will not recognise a secret stored under an unusual name. It exists because the output
of this tool is almost always about to be handed to a model, and that is not a mistake you
can take back. Excluded credentials are listed by name in the run summary, not merely
counted, and `--allow-secrets` turns the category off.

Every excluded file keeps the reason it was excluded and the rule that excluded it. Run
with `--verbose` to see them individually rather than aggregated.

## Fidelity, precisely

Content inside a file section is reproduced byte for byte after decoding, with one
documented exception:

- **XML and JSON are exact in every case.** Content is carried in character-data sections
  or as a JSON string, neither of which needs the content to end in any particular way.
- **Markdown and text place their closing delimiter on its own line.** A file whose last
  line has no newline therefore gains one *newline that belongs to the delimiter* — the
  file's own bytes are otherwise untouched. Use `-f json` or `-f xml` when byte-exactness
  at that boundary matters. The test suite asserts this rule as stated, rather than
  tolerating a general difference.

Byte order marks are stripped during decoding and never re-emitted. Line endings,
indentation and trailing whitespace are preserved unless a transformation was requested;
where a project mixes line endings, that is reported rather than repaired. A file that
cannot be decoded is excluded with a reason, never reproduced with replacement characters.

Markdown fences are `max(3, longest run of backticks in the file + 1)`, derived per file.
XML character-data sections are split around every occurrence of the terminator, including
the overlapping and adjacent cases that defeat a naive replacement. Both rules are covered
by property tests as well as by fixed cases.

## Formats

| Format | Selected by | Notes |
| --- | --- | --- |
| markdown | default, or `.md` | Content-derived fences, ASCII or compact overview |
| text | `-f text`, or `.txt` | Content-derived separators |
| xml | `-f xml`, or `.xml` | Nested structural elements, CDATA content, `schemaVersion` |
| json | `-f json`, or `.json` | Nested `tree`, flat `files` array of `{path, content}`, `schemaVersion` |

The format is inferred from the output file's extension when `--format` is not given, and
never affects which files are included — only how they are rendered. Asking for one format
while naming a file that says another (`-f markdown -o out.xml`) is refused rather than
resolved silently, because a file whose extension lies about its contents is a trap for
whatever reads it next. An extension the tool has no opinion about contradicts nothing.

Markdown fences carry the language identifier a highlighter expects rather than the raw
extension — `rust` not `rs`, `yaml` not `yml`, `bash` not `sh` — with well-known extensionless
names (`Makefile`, `Dockerfile`) recognised too. Around 180 extensions are mapped; anything
unmapped is passed through verbatim rather than dropped or guessed at.

`schemaVersion` is `1.0` and versions the *shape of the document*. It is independent of
the crate's own release version, which versions the command-line surface.

## Statistics

Every run reports what was discovered, what was included, and what was excluded broken
down by reason; the document's size in bytes and lines; and an exact token count.

Exclusions are broken down by reason and each reason names what it excluded, marked as a
directory or a file and annotated with the rule responsible; `--verbose` lists them all
rather than a sample. `--top-files` (five by default, or `--top-files 10`) ranks the
included files by what they cost in tokens, which is usually the fastest way to find what
is filling a context window.

On a terminal, scanning shows an animated indicator with a running file count, retires
itself with a completion line, and hands over to a proportional bar during packaging. A
dedicated thread drives the redraw, so work that produces no events for several seconds — a
large tree, a remote retrieval — still looks alive rather than hung. Writing the assembled
document out gets its own byte-wise bar, because on a large codebase that stage takes
longer than everything before it. The indicator and the summary are plain ASCII, because a
console on a legacy code page renders anything else as replacement characters. Redirected
runs print none of it.

The token count is exact for the reference encoding it names — `cl100k_base` by default,
`o200k_base` with `--token-encoding o200k_base` — not a heuristic and not rounded. It is
computed while the document streams past, at chunk boundaries chosen so the streamed result
is identical to tokenising the whole document at once. `--verify-tokens` recounts from the
delivered file and reports any disagreement; the test suite asserts the two paths agree
exactly for every corpus case in every format. A different model with a different
tokeniser will of course give a different number — the encoding is named alongside the
count for that reason.

Exit status is `0` for a clean run, `1` for a run that completed with warnings, and `2` for
a failure — so a pipeline can distinguish "worked" from "worked, but look at this". A
routine exclusion is not a warning: a binary file skipped by design, or a directory pruned
by a rule you asked for, appears in the exclusion breakdown and nowhere else. Warnings are
reserved for what you might want to act on — a file that could not be read, an encoding
that could not be determined, a link not followed.

Reported separately, and deliberately not affecting the status, are **notices**: mixed
line endings, a language this build cannot reduce, credentials left out. None of these is
a failure and none of them has an action attached. Counting them as warnings meant that
any repository touched from both Windows and Unix exited `1` on a completely successful
run, and any repository with more than one language did too — which cost the exit code the
very distinction it exists to make. A notice about an unsupported language is raised once
for the whole run, naming the languages and the file count, rather than once per file.

## Remote sources

One remote repository may be designated per run, and only ever as an explicit URL. An
argument is never guessed into a remote source: if it names something that exists locally,
it is local.

Retrieval uses the installed `git` with a shallow, single-branch, no-tags fetch, so no
history is placed on the machine; if `git` is missing, or older than 1.9 and so unable to
do that, the run stops with an actionable message rather than falling back to a full
clone. The copy is deleted when the run ends, including on interruption, unless
`--keep-remote-copy` is given.

A remote source's own `.mahironignore` is **not** honoured. It is packaged as an ordinary
file instead, so that fetching a repository cannot silently change what a run includes.
`--trust-remote-config <designation>` opts a specific source in.

## MCP server mode

`--mcp-server` serves the same engine over JSON-RPC 2.0, on stdio by default or over
server-sent events with `--transport sse` (loopback only unless `--bind` says otherwise,
with a warning when it does). Two tools are exposed: `package_codebase` and
`preview_package`. Documents up to 256 KiB are returned inline; larger ones are written to
a temporary file, and that file is **not** cleaned up — the path is returned precisely so
the agent can read it afterwards. The server has no capability the command line lacks.

The SSE transport is not a default feature. Enable it with `--features mcp-sse` if you
want it; a tool that packages source code should not open a listening socket unless it was
asked to. When it is enabled, requests must come from the same machine: `Origin` and
`Host` are checked against loopback, because any page in your browser can reach
`127.0.0.1` and a request that writes a file is a request worth authenticating. A message
must name a live session, request bodies are capped, and connections have timeouts and a
concurrency limit. Writing to a path that already exists requires `force` on the request —
it is not implied.

## Building

```sh
cargo build --release          # both binaries, all default features
cargo test                     # unit, fidelity, property and integration tests
cargo bench                    # throughput over a synthetic tree
cargo run --bin mhrn -- --help
```

Rust 1.77 or later, edition 2021. Builds and is tested on Linux, macOS and Windows, on
x86-64 and ARM64.

### Features

| Feature | Default | Effect |
| --- | --- | --- |
| `clipboard` | on | `--clipboard` delivery |
| `watch` | on | `--watch` mode |
| `compress-rust` | on | Rust structural-signature compression |
| `mcp-sse` | on | the SSE transport for MCP server mode |

`cargo build --no-default-features` produces a minimal build; every removed feature
degrades to a clear message rather than to silence.

## Adding a compressed language

Implement `compress::LanguageCompressor` and register it. Nothing in the shared dispatch
changes:

```rust
struct Elm;

impl LanguageCompressor for Elm {
    fn language(&self) -> &'static str { "elm" }
    fn extensions(&self) -> &'static [&'static str] { &["elm"] }
    fn compress(&self, source: &str) -> Result<String, CompressionError> { /* ... */ }
}

inventory::submit! { Registration { factory: || Box::new(Elm) } }
```

Compression is always opt-in, and a file in a language this build cannot reduce is
packaged verbatim and reported under its own outcome rather than being silently skipped.

## Continuous integration

`.github/workflows/ci.yml` builds and tests on Linux, macOS and Windows, checks formatting
and lints, and runs the fidelity corpus on every platform. The same workflow shows the
shape of a documentation-freshness job:

```yaml
- name: Regenerate the project digest
  run: cargo run --release --bin mhrn -- --include 'src/**' -o docs/context.md --force
- name: Fail if it is out of date
  run: |
    changed=$(git status --porcelain -- docs/context.md)
    if [ -n "$changed" ]; then
      echo "docs/context.md is out of date or untracked; regenerate and commit it"
      git --no-pager diff -- docs/context.md
      exit 1
    fi
```

`git status --porcelain` rather than `git diff --exit-code`, and the difference matters:
`git diff` does not consider untracked files, so if the generated file is not committed
the job passes no matter what the tool produced. The file has to be tracked, and it must
not be listed in `.mahironignore`, or the check has nothing to compare against.

## Privacy

No telemetry, no crash reporting, no update checks — none exist in any build. The only
network activity the tool can perform is retrieving a remote source you named, and serving
the SSE transport you asked for.

## Licence

MIT or Apache-2.0, at your option.
