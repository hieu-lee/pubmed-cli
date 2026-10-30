# pm: a PubMed CLI for LLM agents

`pm` is a fast Rust CLI over NCBI E-utilities, iCite and PMC, built for LLMs rather than humans. Output is compact (TSV by default, no decoration), help is a few hundred tokens, and many actions can be chained or run in parallel in one call.

```
pm search "sepsis fluids" -t rct --quartile 1,2 --last 3y -n 20
pm search -i "Johns Hopkins" -t rct --last 3y -n 100 :: sort cites :: top 5 -f t,cites,q
pm search "sepsis" --grep '\bbalanced\b|\bsaline\b' -f pmid,y,snip      # why each record matched
pm 'search "statin" -n 500 | facet journal,year -n 5'                    # overview of a result set
pm exam 31452104 ; pm comments 26903338 --expand ; pm problems 35898916  # parallel tasks
```

## Features

- **Query**: free text or raw PubMed syntax, `--all/--any/--not`, `--mesh/--major/--tag/--kw`, `--phrase`, `--near "a b:3"`, regex (`--grep`, `--grep-ti`, `--grep-ab`, `--no-grep`), typo-tolerant terms (`--fz`).
- **Filters**: fuzzy author/affiliation/journal, first/last author, country, publication type, status (published, ahead of print, preprint, retracted), language, species, sex, age group, free full text, grants, NCT ids, SJR journal quartile (Q1-Q4), SJR value, h-index, subject area, citations, relative citation ratio, and `--where` expressions (`(q<=2 || cites>100) & abs~~'\bplacebo\b'`).
- **Recency**: `--last 30d|6m|2y`, `--since/--until`, `--new 7d` (newly added to PubMed), `--sort recent`.
- **Articles**: `show`, `exam` (deep dive), `comments`, `problems` (retractions, errata, expressions of concern), `related`, `citedby`, `refs`, `fulltext` (PMC), `cite`, `id`, `metrics`.
- **Discovery**: `count`, `trend`, `explain`, `facet`, `mesh`, `author`, `journal`.
- **Sequencing**: `a | b` chains sets, `a ; b` runs tasks in parallel, `pm batch` runs one pipeline per line. In a shell, quote the pipeline or use `::` for `|` and `++` for `;`.
- **Token control**: `-f` field selection, `--abs N`, `--authors N`, `--budget N` tokens, `--fmt tsv|txt|jsonl|json|ids`, `-f snip` keyword-in-context snippets.
- **Speed**: HTTP/2 with connection reuse, a token-bucket rate limiter (3 req/s, 10 with `NCBI_API_KEY`), parallel batched efetch, an on-disk cache of parsed records, and a journal quartile table embedded in the binary.

Run `pm --help`, then `pm help <topic>` (`filters`, `fields`, `regex`, `recency`, `pipe`, `examples`, or a command name).

## Install

```
cargo install --path .
```

Environment: `NCBI_API_KEY`, `NCBI_EMAIL`, `PM_CACHE` (cache directory).

## Data sources and notes

- PubMed records and links: NCBI E-utilities. Citation metrics: NIH iCite. Full text: PMC open access.
- Journal quartiles use SCImago Journal Rank (SJR 2025), embedded from `data/sjr.bin`. See `data/README.md` for provenance and the build script in `scripts/`. Journals without a SCImago entry have no quartile.
- Quartile, citation and `--where` filters are evaluated locally on fetched records, so they scan up to `--scan N` (default 600) candidates from the server query.
