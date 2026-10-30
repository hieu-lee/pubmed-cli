# data/sjr.bin

SCImago Journal Rank (SJR), year 2025 (released 2026): 31,087 journals/series with a best quartile, SJR, H index and per-category quartiles.

- Source: SCImago Journal & Country Rank, https://www.scimagojr.com (data (c) SCImago Lab / Scopus; free use with attribution). scimagojr.com is behind Cloudflare, so the yearly tables are taken from the public mirror https://github.com/ikashnitsky/sjrdata (`data-raw/sjr-journal/sjr_journals-2026.parquet`, rows with `year==2025`).
- ISO abbreviations and extra ISSNs come from the NLM catalogue list https://ftp.ncbi.nih.gov/pubmed/J_Entrez.txt (public domain) and are stored as normalized aliases.
- Journals without a quartile ("-", discontinued, or not indexed by Scopus such as Cureus) are omitted, so they resolve to no rank.
- Format (embedded uncompressed with include_bytes!, zero parse/decompress cost; ~2 MB if gzipped): see the docstring of `scripts/build_sjr.py`.
- Rebuild: `pip install pyarrow && python3 scripts/build_sjr.py --year 2025 --sjr <parquet> --nlm <J_Entrez.txt>` (downloads missing inputs to the given paths).
