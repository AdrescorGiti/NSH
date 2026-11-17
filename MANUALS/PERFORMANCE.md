<a id="профилирование-nsh"></a>

# Profiling NSH

[Русская версия](PERFORMANCE.ru.md)

[Comparison of five shells and measurement methodology](BENCHMARKS.md)

<a id="разбор-и-выполнение"></a>

## Parsing and execution

The parser keeps three lookahead tokens in its structure. A separate queue is used when alias expansion adds tokens. Unchanged token text is borrowed from the source line. An owned buffer is used for line continuations and words retained in the AST.

Short arithmetic expressions use eight stack nodes; longer expressions grow in a single array. Numeric variable values are read from existing bytes.

Simple patterns are compared byte by byte; POSIX patterns with metacharacters use `fnmatch`. Prefix or suffix removal compiles the pattern once and reuses a buffer when checking boundaries.

<a id="счётчики-выделений"></a>

## Allocation counters

~~~sh
cargo build --locked --release
python3 benchmarks/profile_allocations.py target/release/nsh --rounds 11
~~~

Profiling uses Linux/glibc and the system C compiler. Timings are measured without intercepting the allocator. The `malloc`, `calloc` and `realloc` counts are collected separately through `LD_PRELOAD`. Requested bytes include reallocations and represent the total volume of allocator requests.
