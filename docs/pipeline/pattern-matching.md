# Ruby `case … in`

`case … in` is ingested as an expression and translated once into the shared
expression IR. Ruby output is exercised against CRuby; JRuby uses that same
emission path. Other targets currently report a located unsupported diagnostic
before project emission. In particular, their expression wrappers do not all
preserve Ruby's local-variable scope, and their `===` implementations are not
uniform. This change does not claim support for those targets or Spinel.

## Design

The small analysis prefactor is `branch_ctx`: short-circuit conditions now carry
both local assignments and composed type narrowing into the next condition and
branch. Conditional assignments are joined when control flow rejoins, including
assignments made by an unsuccessful pattern or guard.

`ingest/pattern_match.rs` builds a transient pattern tree defined in
`expr/pattern_match.rs`. `lower/pattern_match.rs` translates it to existing
assignments, sequences, short-circuit operators, conditionals and collection
operations. No emitter contains a pattern matcher. Provenance hints retain the
source location for unsupported deconstruction and target diagnostics; they are
not part of expression round-trip equality.

Checked collection reads carry the proof that their length/key test passed.
Their types retain actual nil values without adding a synthetic nil for an
out-of-bounds or missing-key access. Removing matched record keys also updates
the inferred rest shape. Bound values otherwise retain the analyzer's existing
element/field precision: heterogeneous collections can still produce unions,
and an unresolved subject does not acquire invented field types.

The case subject is assigned once to a fresh local. Temporary names are checked
against the registered source. An ordinary Ruby `begin` groups the resulting
expression without introducing a local scope. Branches retain source order and
their value becomes the value of the case expression. Guards run only after a
successful pattern, with its bindings already assigned. Missing `else` raises
`NoMatchingPatternError`, or `NoMatchingPatternKeyError` for a single-arm missing
key failure, including `key` and `matchee`. Exception messages are simplified;
their exact CRuby wording is not reproduced.

Value patterns invoke `===`. Array patterns check length before accessing
elements. Hash patterns check every required key before reading any value, so a
missing key differs from a present `nil`. Rest capture duplicates the hash before
removing matched keys. Bindings of `false` and `nil` do not turn a successful
binding into a failed condition.

## Syntax

| Form | Examples | Status |
|---|---|---|
| Hash and nested patterns | `{code: :ok, data: {user:}}` | Supported |
| Array and nested patterns | `[first, {data: [a, b]}]` | Supported |
| Binding, wildcard and capture | `value`, `_`, `Integer => n` | Supported; `_` is also a Ruby local |
| Guards | `in [n] if n > 0`, `in [n] unless skip` | Supported |
| Literal, regexp, range and class | `nil`, `false`, `:ok`, `/^ok/`, `(1..)`, `Integer` | Supported through `===` |
| Alternatives | `:ok \| :cached`, `[1, _] \| [2, _]` | Supported, subject to Ruby's binding restrictions |
| Pin | `^value`, `^(expression)` | Supported through `===` |
| Array rest | `[first, *middle, last]`, `[first, *]` | Supported |
| Hash rest and exclusion | `{a:, **rest}`, `{a:, **}`, `{a:, **nil}` | Supported |
| Class-qualified structure | `Array[Integer => n]`, `Hash[a:]` | Supported |
| Find pattern | `[*, needle, *]` | Located strict/survey ingest diagnostic |
| Rightward assignment / boolean match | `value => pattern`, `value in pattern` | Remain separate unsupported constructs |

Objects are queried with `respond_to?` before `deconstruct` or
`deconstruct_keys`. Hash keys are passed as Ruby requires: a key array for an
ordinary pattern, `nil` when the whole hash is needed. Return values are checked
and invalid array/hash protocol results raise `TypeError`. Ordinary objects with
no protocol fall through. Statically resolved custom methods returning concrete
collection shapes are supported; unresolved dynamic methods or return shapes
produce an analysis error. Builtin collection protocols lower to their receiver
after analysis. Collection subclasses with custom access behavior and singleton
overrides are outside the modeled collection subset.

Ruby specifies that bindings from failed matches and the number of calls to
deconstruction methods are undefined. The tests pin binding behavior observed on
the supported CRuby runtime: assignments made before failure remain visible;
there is no transactional rollback. Deconstruction is performed per attempted
structural pattern; CRuby may cache an array deconstruction between arms. Code
depending on the unspecified call count is not portable. See the official
[Ruby pattern matching documentation](https://docs.ruby-lang.org/en/4.0/syntax/pattern_matching_rdoc.html).

## Practice-Ignition inventory (2026-09-29)

A Prism scan of the supplied app's Ruby files found **40 case expressions,
157 arms, and 36 files**, including one script. Four cases have an explicit
`else`. The trees contain 177 hash patterns, 23 array patterns, 12 alternatives,
9 ranges, 13 regexps, and three guarded arms. Nested command-result hashes,
tuple arrays, shorthand bindings, symbol alternatives, and the payment-fee
range/regexp rules drive the fixtures. Class, capture, pin, and rest forms were
not required by this snapshot, but have generic coverage.

The app defines both protocols on its command result class. Its hash protocol
selects requested keys from a result hash; support cannot be implemented by
assuming every pattern subject is already a hash.

The supplied audit reports **42 `CaseMatchNode` gaps in 34 distinct files**.
Survey ingestion with this change reports **zero `CaseMatchNode` gaps and zero
unsupported case-pattern syntax gaps**. Audit occurrences are not a count of
unique source cases: ingestion can visit engine sources more than once and can
omit methods for other reasons.

The total ingestion counts are not a controlled before/after comparison. This
checkout starts at `145183ac`, before the audit's `27d28cbc` and its PostgreSQL /
engine ingestion changes. It loads 162 tables and 9,772 source files, versus the
audit's 638 tables and 9,827 files. Its 293 remaining ingest gaps must not be
compared to the audit's 751 as if this feature fixed the difference.

All 40 extracted case expressions ingest individually; 39 also pass the CLI's
IR round-trip check. The remaining expression returns interpolated heredocs:
adjacent text parts are combined when Ruby is re-ingested. The same mismatch
reproduces with a standalone interpolated heredoc and no pattern matching. This
is a separate round-trip normalization gap, not an unsupported pattern form.

Other ingestion blockers include non-symbol block forwarding, anonymous keyword
forwarding, external engine mounts, unmodeled controller macros, and schema
migration `execute` calls. None was hidden or changed to accommodate this app.

The final full-app `roundhouse check --continue` completed in 527 seconds and
exited 1:

| Result | Count |
|---|---:|
| Parse errors | 0 |
| Analysis errors | 1,975 |
| Warnings | 13,680 |
| Gap-attributed notes | 1,390 |
| Survey ingestion gaps | 293 |
| `CaseMatchNode` / unsupported case-pattern syntax gaps | 0 |
| Unresolved pattern deconstruction checks, included in analysis errors | 167 |

All 167 explicit pattern diagnostics concern `deconstruct_keys`: the subject or
its protocol return shape cannot yet be established statically. They count
individual structural patterns, not distinct case expressions or syntax forms.
For example, the payment-fee method's `rule` remains unresolved after
`with_defaults`. Command-result subjects also depend on upstream inference.
These checks retain located errors rather than assuming opaque values are
hashes or claiming that dynamic custom deconstruction is supported.

The app is therefore **not fully compatible**. Its prior opaque case-expression
gaps have become shared IR with analyzable branches and specific protocol/type
blockers. The separate framework/schema/gem gaps remain; 124 of the app's 195
gems are still unmodeled. The full error totals are not comparable to the supplied
audit until the compiler bases and loaded source sets are aligned.

## Verification

`tests/pattern_matching.rs` compares original and emitted Ruby with the installed
Ruby runtime and checks ingest → emit → ingest equality for every differential
case. It covers branch precedence, single subject evaluation, failed matches,
guards, nested structures, missing keys, nil/false, lengths, rests, alternatives,
pins, exceptions, deconstruction arguments and invalid protocol returns.

Body-typer tests check bindings in guards and bodies, result types, nested
nil/false values, and visibility after failure. `tests/emit_and_run.rs` overlays
the generic fixtures on real-blog and requires both zero analysis errors and
successful execution of the emitted project, including custom deconstruction.

Local validation used CRuby 4.0.5 and a UTF-8 locale:

| Check | Result |
|---|---|
| `cargo build --tests -j 2` | Passed |
| `cargo test --all-targets --no-fail-fast -j 2` | 2,434 passed; 97 intentionally ignored |
| Library tests (included above) | 846 passed |
| Pattern tests (included above) | 16 groups passed, including Ruby behavioral comparisons and IR round trips |
| `emit_and_run` (included above) | 2 passed; 2 pre-existing ignored tests |
| Explicit Ruby toolchain lane | 17 of 18 Ruby suites passed; previewer suite failed as described below |
| `git diff --check` | Passed |

The Ruby toolchain's pinned gems were installed in a temporary bundle without
changing its lockfile. Its remaining failure is unrelated to pattern matching:
ffmpeg is absent, and `active_storage_previewer_test.rb` calls `File.exist?(nil)`
from `ensure` after skipping the test. The eight suites after that failure were
run separately and passed. JRuby and the other language toolchains were not run.
