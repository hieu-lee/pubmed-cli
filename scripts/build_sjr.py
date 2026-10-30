#!/usr/bin/env python3
"""Build data/sjr.bin from SCImago Journal Rank (SJR) data + NLM journal list.

Usage: build_sjr.py [--year 2025] [--sjr SJR.parquet] [--nlm J_Entrez.txt] [--out data/sjr.bin]
Requires pyarrow. Missing inputs are downloaded:
  SJR  : github.com/ikashnitsky/sjrdata (mirror of scimagojr.com yearly CSVs; scimagojr.com is behind Cloudflare)
  NLM  : https://ftp.ncbi.nih.gov/pubmed/J_Entrez.txt (ISO abbreviations + ISSNs, used for alias matching)

Output data/sjr.bin (little endian, embedded uncompressed so lookups need no parsing or decompression):
  header 8*u32: magic n_entries n_issn n_names n_abbr len_cats len_pool 0
  cats   : category names joined by '|' (index = category id)
  offsets: (n_entries+1)*u32 into pool
  pool   : one TSV line per journal: issns(',') \t title \t aliases('|', normalized) \t sjr \t h \t cats('id:q,...')
  issn   : n_issn * (u32 key, u32 idx) sorted by key; key = first7digits*11 + check (X=10)
  names  : n_names * (u64 fnv1a(normalized name), u32 idx) sorted by hash
  abbr   : n_abbr * u32 sorted; (first_byte<<8 | ntokens)<<16 | idx for titles with >=2 non-stopword tokens
Entries are sorted by SJR descending (so idx order = rank order); best quartile is min(q) over cats.
"""
import argparse, os, re, struct, sys, urllib.request

SJR_URL = "https://raw.githubusercontent.com/ikashnitsky/sjrdata/master/data-raw/sjr-journal/sjr_journals-2026.parquet"
NLM_URL = "https://ftp.ncbi.nih.gov/pubmed/J_Entrez.txt"


def fetch(path, url):
    if not os.path.exists(path):
        print("downloading", url, file=sys.stderr)
        urllib.request.urlretrieve(url, path)
    return path


def norm(s):
    s = s.strip()
    if len(s) > 4 and s[:4].lower() == "the ":
        s = s[4:]
    s = s.lower()
    return "".join(c for c in s.replace("&", "and") if c.isalnum())


def strip_paren(s):
    t = s.rstrip()
    if t.endswith(")"):
        i = t.rfind("(")
        if i > 0:
            return t[:i].rstrip()
    return t


STOP = {"of", "the", "and", "for", "in", "on", "a", "an", "de", "la", "le", "et", "und", "der", "die", "das", "del", "y"}


def fnv(s):
    h = 0xCBF29CE484222325
    for b in s.encode():
        h = ((h ^ b) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def issn_num(k):
    return int(k[:7]) * 11 + (10 if k[7] == "X" else int(k[7]))


def tokens(s):
    return [t for t in re.split(r"[^\w]|_", s) if t and t.lower() not in STOP]


def issn_key(s):
    return re.sub(r"[^0-9Xx]", "", s).upper()


def nlm_records(path):
    rec = {}
    with open(path, encoding="utf-8") as f:
        for line in f:
            if line.startswith("-----"):
                if rec:
                    yield rec
                rec = {}
            elif ": " in line or line.rstrip().endswith(":"):
                k, _, v = line.rstrip("\n").partition(":")
                rec[k] = v.strip()
    if rec:
        yield rec


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--year", type=int, default=2025)
    ap.add_argument("--sjr", default="/tmp/sjr_journals.parquet")
    ap.add_argument("--nlm", default="/tmp/J_Entrez.txt")
    ap.add_argument("--out", default=os.path.join(os.path.dirname(__file__), "..", "data", "sjr.bin"))
    a = ap.parse_args()
    import pyarrow.parquet as pq

    cols = ["year", "title", "issn", "sjr", "sjr_best_quartile", "h_index", "categories"]
    rows = [r for r in pq.read_table(fetch(a.sjr, SJR_URL), columns=cols).to_pylist() if r["year"] == a.year]

    cat_ids, cat_names = {}, []
    journals = []
    for r in rows:
        if r["sjr_best_quartile"] not in ("Q1", "Q2", "Q3", "Q4") or not r["categories"]:
            continue
        cats = []
        for c in r["categories"].split("; "):
            m = re.match(r"(.*) \(Q([1-4])\)$", c)
            if not m:
                continue
            name = m.group(1)
            if name not in cat_ids:
                cat_ids[name] = len(cat_names)
                cat_names.append(name)
            cats.append((cat_ids[name], int(m.group(2))))
        if not cats:
            continue
        issns = [k for k in (issn_key(x) for x in (r["issn"] or "").split(",")) if len(k) == 8]
        title = " ".join((r["title"] or "").split()).replace("\t", " ")
        journals.append(
            dict(issns=issns, title=title, sjr=r["sjr"] or 0.0, h=int(r["h_index"] or 0), cats=cats, alias=set())
        )
    journals.sort(key=lambda j: -j["sjr"])

    by_issn, by_title = {}, {}
    for i, j in enumerate(journals):
        for k in j["issns"]:
            by_issn.setdefault(k, i)
        for t in {norm(j["title"]), norm(strip_paren(j["title"]))}:
            by_title.setdefault(t, i)

    for n in nlm_records(fetch(a.nlm, NLM_URL)):
        keys = [issn_key(n.get("ISSN (Print)", "")), issn_key(n.get("ISSN (Online)", ""))]
        keys = [k for k in keys if len(k) == 8]
        title = n.get("JournalTitle", "")
        hits = {by_issn[k] for k in keys if k in by_issn}
        if not hits:
            t = norm(strip_paren(title))
            if t in by_title:
                hits = {by_title[t]}
        if len(hits) != 1:
            continue
        j = journals[hits.pop()]
        for k in keys:
            if k not in j["issns"]:
                j["issns"].append(k)
        for s in (n.get("IsoAbbr", ""), n.get("MedAbbr", ""), title, strip_paren(title)):
            if s:
                j["alias"].add(norm(s))

    owners = {}
    for i, j in enumerate(journals):
        j["alias"] -= {norm(j["title"]), norm(strip_paren(j["title"])), ""}
        for s in j["alias"]:
            owners.setdefault(s, set()).add(i)
    for i, j in enumerate(journals):
        j["alias"] = sorted(s for s in j["alias"] if len(owners[s]) == 1 and s not in by_title)

    pool, offs = bytearray(), []
    for j in journals:
        offs.append(len(pool))
        cats = ",".join(f"{c}:{q}" for c, q in j["cats"])
        line = "\t".join([",".join(j["issns"]), j["title"], "|".join(j["alias"]), f"{j['sjr']:g}", str(j["h"]), cats])
        pool += line.encode() + b"\n"
    offs.append(len(pool))

    issn, names, abbr = {}, {}, []
    for i, j in enumerate(journals):
        for k in j["issns"]:
            if k[:7].isdigit() and (k[7].isdigit() or k[7] == "X"):
                issn.setdefault(issn_num(k), i)
        for t in [norm(j["title"]), norm(strip_paren(j["title"])), *j["alias"]]:
            if t:
                names.setdefault(fnv(t), i)
        tk = tokens(j["title"])
        if len(tk) >= 2:
            b0 = tk[0].encode()[0]
            b0 += 32 if 65 <= b0 <= 90 else 0
            abbr.append(((b0 << 8 | min(len(tk), 255)) << 16) | i)
    abbr.sort()
    cats = "|".join(cat_names).encode()
    os.makedirs(os.path.dirname(os.path.abspath(a.out)), exist_ok=True)
    with open(a.out, "wb") as f:
        f.write(struct.pack("<8I", 0x31524A53, len(journals), len(issn), len(names), len(abbr), len(cats), len(pool), 0))
        f.write(cats)
        f.write(struct.pack(f"<{len(offs)}I", *offs))
        f.write(pool)
        for k, i in sorted(issn.items()):
            f.write(struct.pack("<II", k, i))
        for h, i in sorted(names.items()):
            f.write(struct.pack("<QI", h, i))
        f.write(struct.pack(f"<{len(abbr)}I", *abbr))
    print(f"{len(journals)} journals, {len(cat_names)} categories -> {a.out} ({os.path.getsize(a.out)} bytes)")


if __name__ == "__main__":
    main()
