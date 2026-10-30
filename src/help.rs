use crate::fields::REG;
use std::fmt::Write;

const MAIN: &str = "\
pm: PubMed CLI for LLMs. Compact output; header '#total= shown= skip=' (--quiet hides).
CMDS: search|s count|c explain trend show|get id related|rel citedby|cb refs comments problems exam fulltext|ft cite mesh author journal|j filter sort top|head facet metrics cache batch
Help: pm help cmds|query|filters|fields|regex|recency|pipe|examples|<cmd>
FLAGS (value follows flag; a,b = list):
 -n N limit(10)  --skip N  -f t,a,au | +abs add to default | -f=-q,-au drop  --fmt auto|tsv|txt|jsonl|json|ids
 --abs N chars(0=full)  --budget N tokens  --authors N  --quiet  --no-header  --no-cache  --refresh
 --all a,b  --any a,b  --not a,b  --ti/--ab/--tiab  --mesh X  --major X  --kw X
 -a \"Last I\"  -i affiliation  -j journal  -y 2020|2018-2022  --last 6m|2y  -t rct,review,meta,sr
 -q 1,2 (SJR quartile)  --status published|preprint|retracted  --lang en  --oa  --cites-min N
 --grep/--grep-ti/--grep-ab RE (regex, repeat)  --no-grep RE  --fz TERM (typo-tolerant, x* prefix)  --phrase \"a b\"  --near \"a b:3\"
 --since D --until D --within 6m --new 7d (newly added)  --sort date|recent|cites|sjr|rcr
 --where \"(q<=2 || cites>100) & abs~~'\\bplacebo\\b' & !(type~review)\" (pm help regex)
 -f snip: only text around matches (--snip N chars, --snips K)  facet journal,year -n 10: group counts
PIPE (quote whole pipeline, or use shell-safe :: for | and ++ for ;): 'a | b' chains sets; 'a ; b' parallel; pm batch < file
 pm search \"sepsis fluids\" -t rct -q 1,2 --last 3y --grep 'mortality|survival' -f pmid,y,snip
 pm search -i \"Johns Hopkins\" -t rct --last 3y :: sort cites :: top 5 -f t,abs";

const QUERY: &str = "\
QUERY: positional words -> PubMed query (automatic term mapping: MeSH + all fields).
Raw syntax (AND OR NOT, \"phrases\", (parens), trunc*, [tag]) is passed as-is. Flags add AND clauses. 'pm explain ...' prints the final query.
--all a,b: each term in title/abstract, ANDed. --any a,b: ORed. --not a,b: excluded.
--ti/--ab/--tiab: restrict free text to title / abstract / both.
TAGS: [ti] title [tiab] title+abstract (abstract-only: --ab) [tw] text words [all] everything. Wildcards need 4+ chars before *
 [au] author \"Smith J\"[au]  [1au] first  [lastau] last  [fau] full name  [auid] ORCID  [ad] affiliation/address
 [mh] MeSH (exploded) [mh:noexp] [majr] major topic [sh] subheading [ot] keyword/other term [nm] substance [pa] pharm action
 [pt] pub type \"randomized controlled trial\"[pt] [dp] pub date 2020:2022[dp] [edat] entrez [mdat] modified [crdt] created
 [ta] journal abbrev [jour] journal [la] language [sb] subset (medline, pubmed pmc, free full text[sb], systematic[sb])
 [gr] grant [aid] article id (doi) [pmid] [pmc] [si] secondary id NCT01234567[si] [vi] volume [ip] issue [pg] page
Examples: (\"heart failure\"[mh] OR hfpef[tiab]) AND exercise[tiab] NOT review[pt]
 smith j[au] AND stanford[ad] AND 2021:2024[dp]";

const FILTERS: &str = "\
FILTERS (all combine with AND; repeat flags or comma lists for OR within a flag):
TEXT --all a,b | --any a,b | --not a,b | --ti --ab --tiab (scope) | --mesh X | --major X | --tag X (mesh|keyword|otherterm) | --kw X
GREP (local, title+abstract, case-insensitive; 'pm help regex') --grep RE | --grep-ti RE | --grep-ab RE | --no-grep RE (exclude) | --fz TERM (typo-tolerant, x* = prefix) | --phrase \"exact words\" | --near \"a b:3\" (within 3 words)
PEOPLE -a/--author \"Last I\" (repeat) | --first-author | --last-author | --fuzzy (tolerant author/aff/journal match) | -i/--aff X (university, institute, country) | --country X
JOURNAL -j/--journal X | --issn X | -q/--quartile 1,2 (SJR best quartile) | --sjr-min N | --hindex-min N | --area \"Oncology\" (SJR category)
DATE -y/--year 2020 or 2018-2022 | --from D --to D (YYYY[/MM[/DD]]) | --last 30d|6m|2y | --datetype pdat|edat|mdat|crdt
RECENCY --since D | --until D | --within 6m | --new 7d (newly added to PubMed, entry date) | --sort recent ('pm help recency')
TYPE -t/--type rct,review,meta,sr,trial,case,guideline,... | --not-type X | --cat type|subset
STATUS --status published|aop|preprint|retracted|any | --published | --unpublished | --preprint | --no-retracted | --retracted
CONTENT --lang en | --species human|animal | --sex f|m | --age child|adult|aged | --free | --fulltext | --oa | --abstract | --data | --nct ID | --grant X
IMPACT --cites-min N | --rcr-min N | --clinical (cited by clinical articles) | --has-comments | --has-problems
ORDER --sort relevance|date|recent|pubdate|author|journal|cites|sjr|rcr (--desc|--asc) | --scan N (max scanned when post-filtering, 600) | -n N | --skip N
LOCAL --where \"year>=2020 & q<=2 & cites>10 & aff~stanford\": & && AND, || OR, ! NOT, ( ) group; ops = != > >= < <= ~ (contains) !~ ~~ (regex) !~~ % (typo-tolerant) in; fields: 'pm help fields', grammar: 'pm help regex'
Local filters (quartile, cites, grep, where, ...) need records/enrichment, so they scan more; set --scan higher for rare matches.";

const PIPE: &str = "\
PIPELINES
 a | b      (unquoted bash eats |; write a :: b, or quote the pipeline) stage b receives a's set (ids; records loaded on demand). Only the last set is printed.
 a ; b      (shell-safe: a ++ b) independent tasks, run in parallel; each printed under '## <task>' when >1 task.
 pm batch   reads one pipeline per line from stdin / -f FILE / args; all run in parallel.
Stages taking ids (show related citedby refs comments problems exam cite filter sort top facet fulltext metrics) use piped ids when no ids are given.
Positional ids: PMID, doi:10.x/y, pmc:PMC123.
Stage flags apply to their own stage; --fmt/--abs/--budget/-f/--quiet apply to the final render (so -f snip reads --grep/--phrase/--fz/query words of the LAST stage).
 pm search \"x\" -n 50 | filter -q 1,2 | sort cites | top 5 -f t,cites
 pm refs 31452104 | filter -y 2020-2024 -t rct | sort rcr | top 10
 pm search \"a\" -n 5 ; search \"b\" -n 5 ; count \"c\"
 printf 'search a -n 3\\ncount b\\n' | pm batch";

const REGEX: &str = "\
REGEX / FUZZY / WHERE (local: records are fetched, then matched; raise --scan for rare hits)
--grep RE | --grep-ti RE | --grep-ab RE   Rust regex (case-insensitive) on title+abstract | title | abstract; repeatable
--no-grep RE   drop records matching RE
--fz TERM   typo-tolerant word (hyperkalaemia~hyperkalemia); TERM* = prefix; repeatable
--phrase \"a b\"   exact phrase; repeatable
--near \"a b:3\"   a and b within 3 words
Regex tips: single-quote it in the shell; \\bword\\b whole word, a|b either, ^ $ anchors, (?-i) case-sensitive; bad regex = error.
--where EXPR: field op value joined by & (&&) AND, || OR, ! NOT, ( ) groups; value = word or 'quoted'
 ops: = != > >= < <= (numbers, exact) | ~ contains, !~ | ~~ regex, !~~ | % typo-tolerant | in list (x in a,b)
 numbers: year q sjr h cites rcr pct apt nrefs age nauthors; text: title abs concl text(title+abs) journal first last aff type mesh kw
EXAMPLES
1 pm search \"statin\" --grep 'dementia|alzheimer' -f pmid,y,snip
2 pm search \"asthma\" --grep-ti 'randomi[sz]ed' --no-grep 'pediatric|children' -f pmid,y,t
3 pm search \"arrhythmia\" --fz tachycardya --fz fibrill* -f pmid,y,snip
4 pm search \"heart failure\" --phrase \"ejection fraction\" -f pmid,y,snip --snip 80
5 pm search \"hypertension\" --near \"salt restriction:3\" -f pmid,y,snip
6 pm search \"asthma\" -n 300 --where \"(q<=2 || cites>100) & abs~~'\\bplacebo\\b' & !(type~review)\"
7 pm search \"sepsis\" -n 300 --where \"title~~'^(effect|efficacy) of' && nauthors>=10 && first!~smith\"
8 pm search \"diabetes\" -n 300 --where \"abs%hypertention & year>=2020\" -f pmid,y,t
Read why records matched: add -f pmid,y,snip (--snip N chars each side, --snips K).";

const RECENCY: &str = "\
RECENCY
--since D / --until D   lower / upper date bound; D = YYYY[-MM[-DD]]
--within 6m|2y|30d   window ending today
--new 7d   newly ADDED to PubMed in the last 7 days (entry date), whatever the publication date
--sort recent   newest first (--sort cites|sjr|rcr for impact); --last 30d|6m|2y = classic publication window
where: age (days since pubdate) e.g. --where \"age<=2 & cites>20\"
 pm search \"long covid\" --new 7d --sort recent -n 30 -f pmid,date,t
 pm search \"glp-1\" --since 2024-06 -t rct -f pmid,y,t
 pm search \"x\" --within 6m -q 1 --sort cites";

const CMDS: &str = "\
COMMANDS (ids = PMIDs, doi:, pmc:, or piped)
search|s [query] [filters]  list matching records
count|c [query] [filters]  total only; --by-year per-year counts
explain [query] [filters]  show the PubMed query that would run
trend [query] [filters]  yearly hit counts
show|get <ids>  full records (-f +abs,mesh; abstract full for 1 id)
id <ids>  convert pmid/doi/pmc
related|rel <ids>  similar articles (score)
citedby|cb <ids>  articles citing ids
refs <ids>  articles cited by ids
comments <ids>  comments/replies linked to ids (titles expanded)
problems <ids>  retractions, errata, expressions of concern
exam <ids>  deep dive: abstract, MeSH, grants, NCT, corrections, metrics
fulltext|ft <id>  PMC open-access text; --sec methods,results  --grep term
cite <ids>  citation: vancouver|bibtex|ris|apa|ama
mesh <term>  MeSH descriptor: tree numbers, entry terms, scope note
author \"Last I\" [--aff X]  profile: counts, years, top journals/affiliations/coauthors/MeSH
journal|j <name|issn>  SJR quartile, h-index, areas (--area X, -q 1,2, --areas)
filter [filters]  keep piped records matching filters
sort <key> [--desc|--asc]  keys: date cites sjr rcr year journal author score
top|head N  first N
facet <field[,field]> [-n 10]  group-by counts of the piped/loaded set (journal year type q author mesh kw country aff lang status)
metrics <ids>  iCite: cites rcr pct apt clin
cache [clear|stats]  local cache
batch  one pipeline per line
help [topic]";

const EXAMPLES: &str = "\
EXAMPLES
1 screening: pm search \"statin dementia\" --tiab -t rct,meta --last 5y -n 100 -f pmid,year,t,concl --abs 200
2 Q1 RCTs from a university, 3y: pm search -i \"Stanford University\" -t rct --last 3y -q 1 -n 30
3 retraction audit: pm search \"topic\" -n 200 --has-problems | filter --retracted -f pmid,year,journal,problems,t
4 author profile: pm author \"Topol E\" --aff Scripps
5 citing papers with comments: pm citedby 31452104 -n 100 --has-comments -f pmid,t,comments
6 top cited in field: pm search \"CRISPR\" --area Genetics --last 2y -n 300 | sort cites | top 10 -f pmid,t,cites,rcr,q
7 evidence ladder: pm count \"sepsis\" -t meta ; count \"sepsis\" -t rct ; count \"sepsis\" -t review
8 trial registry link: pm search --nct NCT03000000 -f pmid,t,nct,status
9 grant search: pm search --grant R01CA123456 -f pmid,year,t,grants
10 reading list: pm related 31452104 -n 15 -f pmid,score,year,journal,t | filter -q 1,2
11 methods text: pm fulltext PMC1234567 --sec methods --grep randomi
12 cite: pm search \"x\" -n 3 | cite bibtex
13 unpublished/preprint only: pm search \"covid\" --preprint --last 6m -n 20
14 token-capped output: pm search \"x\" -n 200 -f t,abs --budget 3000
15 regex slice + why it matched: pm search \"statin\" -n 200 --grep 'dementia|alzheimer' -f pmid,y,snip --snip 80
16 what is new this week: pm search \"long covid\" --new 7d --sort recent -n 30 -f pmid,date,t
17 facet overview, then drill down: pm search \"sepsis\" --last 3y -n 500 :: facet journal,type -n 8 ; then pm search \"sepsis\" --last 3y -j \"Crit Care Med\" -t rct
18 proximity: pm search \"hypertension\" --near \"salt restriction:3\" -f pmid,y,snip
19 typo-tolerant + exact phrase: pm search --fz \"tachycardya\" --phrase \"heart rate\" -f pmid,y,snip
20 boolean local filter: pm search \"asthma\" -n 300 --where \"(q<=2 || cites>100) & abs~~'\\bplacebo\\b' & !(type~review)\" -f pmid,y,cites,t
21 lean fields: pm search \"x\" -f=-q,-authors,+snip";

fn command(name: &str) -> Option<&'static str> {
    Some(match name {
        "search" | "s" => "search [query] [filters] [-n 10 --skip N --sort K -f F]\nList records for a PubMed query. Filters in 'pm help filters', query syntax in 'pm help query'. Local post-filters over-fetch up to --scan (600) records; header notes the scan. --fmt ids for bare PMIDs.",
        "count" | "c" => "count [query] [filters] [--by-year]\nPrints #total=N (no records). --by-year: table year/count.",
        "explain" => "explain [query] [filters]\nPrints the exact PubMed query string built from words and flags, plus any local-only filters; no search is run.",
        "trend" => "trend [query] [filters] [-y 2000-2024]\nYearly publication counts for the query (table).",
        "show" | "get" => "show <ids> [-f fields] [--abs N]\nRecords by id. One id shows the full abstract; several use --abs (default 500). Default fields: pmid year journal q authors title; add with -f +abs,mesh.",
        "id" => "id <pmid|doi:..|pmc:..> ...\nConvert between PMID, DOI, PMCID (table).",
        "related" | "rel" => "related <ids> [-n N] [-f F]\nPubMed 'similar articles'; score field = relevance. Multiple ids: union.",
        "citedby" | "cb" => "citedby <ids> [-n N] [filters]\nArticles citing the ids (elink). Multiple ids: union with score = number of links. Combine with --has-comments; rank with `:: sort cites :: top N` (no --sort here).",
        "refs" => "refs <ids> [-n N]\nReference list of ids that exist in PubMed. Multiple ids: union.",
        "comments" => "comments <ids> [-f F]\nComment/reply articles linked to ids: 'in:PMID title' (comments about it) and 'on:PMID title' (it comments on).",
        "problems" => "problems <ids>\nRetractions, errata, expressions of concern: 'retracted:PMID', 'erratum:PMID', 'eoc:PMID'.",
        "exam" => "exam <ids>\nFull-record deep dive per id: abstract (full), MeSH, grants, NCT ids, corrections with titles, journal rank, iCite metrics.",
        "fulltext" | "ft" => "fulltext <pmid|pmc:..> [--sec methods,results] [--grep term]\nPMC open-access full text as section text. --sec selects sections; --grep keeps matching paragraphs. Not open access: error.",
        "cite" => "cite <ids> [vancouver|bibtex|ris|apa|ama]\nFormatted citations, one per id. Default vancouver.",
        "mesh" => "mesh <term>\nMeSH descriptor: id, tree numbers, entry terms, scope note, pharmacological actions.",
        "author" => "author \"Last I\" [--aff X] [-y R]\nProfile from the author's works: n, year span, top journals, affiliations, coauthors, MeSH. Use --aff to disambiguate common names.",
        "journal" | "j" => "journal <name|issn> [--area X] [-q 1,2] [--areas]\nLookup SJR best quartile, SJR, h-index, area, ISSN. Name may be partial or an ISO abbreviation; --areas lists subject areas.",
        "filter" => "filter [filters] [--where EXPR]\nKeeps piped records that match the filters (same flags as search; local evaluation). Enriches journal rank/metrics as needed.",
        "sort" => "sort <key> [--desc|--asc]\nKeys: date year cites sjr rcr journal author score. Default direction: best first.",
        "top" | "head" => "top <N>\nFirst N of the piped set.",
        "facet" => crate::cmd::facet::HELP,
        "metrics" => "metrics <ids>\niCite table: cites rcr pct apt clin.",
        "cache" => "cache [clear|stats]\nManage the local response cache (PM_CACHE dir). --no-cache bypasses, --refresh refetches.",
        "batch" | "run" => "batch [-f FILE] [pipeline ...]\nOne pipeline per line from stdin, file or args; all run in parallel; each output under '## <line>'.",
        "help" => "help [topic]\nTopics: cmds query filters fields regex recency pipe examples, or a command name.",
        _ => return None,
    })
}

fn fields() -> String {
    let mut s = String::from("FIELDS (-f a,b | -f +abs adds to default | -f=-q,-authors drops from default | -f all | aliases after '|')\n");
    for (f, name, aliases, doc) in REG {
        s.push_str(name);
        for a in *aliases {
            s.push('|');
            s.push_str(a);
        }
        let n = f.need();
        let _ = writeln!(s, "{} {doc}", if n.rank || n.metrics || n.expand { " *" } else { "" });
    }
    s.push_str(
        "Default: pmid [score] year journal q authors title. * = needs enrichment (q sjr h: local journal rank; cites rcr pct apt clin: iCite; comments problems: extra fetch).\n\
         Long fields (abs concl bg methods results mesh major kw grants comments problems links all) switch auto format to txt.\n\
         FORMATS: tsv = header row + one record/row. txt = per record 'PMID | year journal Q# | authors' / title / 'field: value' lines, blank line between records. jsonl = one object/line, short keys, empty omitted (first line '#...' header). json = array. ids = one PMID per line.\n\
         -f spec: bare names replace the default; any +name/-name/~name makes it relative to the default (-f +abs,-q ok; a value starting with '-' needs '=': -f=-q,-au; '~q' also drops). Unknown names get did-you-mean.\n\
         snip: KWIC fragments around matches of --grep/--grep-ti/--grep-ab regexes, --phrase, --fz, --near, --all/--any/--ti/--ab/--tiab and the query words (PubMed tags/AND/OR/NOT/quotes stripped); '…' marks cuts, 'T:' = title hit, merged windows, no markup; --snip N chars each side (60), --snips K max fragments (3); no match/terms: first 160 chars of abstract.\n\
         Filter exprs (--where) use the same names plus text title abs concl age nauthors first last; numeric: year q sjr h cites rcr pct apt nrefs age nauthors. See 'pm help regex'.",
    );
    s
}

const TOPICS: &str = "topics: cmds query filters fields regex recency pipe examples; commands: search count explain trend show id related citedby refs comments problems exam fulltext cite mesh author journal filter sort top facet metrics cache batch";

pub fn text(topic: Option<&str>) -> String {
    let Some(t) = topic.map(str::trim).filter(|t| !t.is_empty()) else {
        return MAIN.to_string();
    };
    let t = t.to_ascii_lowercase();
    match t.as_str() {
        "cmds" | "commands" => CMDS.to_string(),
        "query" | "tags" => QUERY.to_string(),
        "filters" | "filter-flags" => FILTERS.to_string(),
        "fields" | "field" => fields(),
        "regex" | "grep" | "fuzzy" | "where" => REGEX.to_string(),
        "recency" | "recent" | "new" => RECENCY.to_string(),
        "pipe" | "pipeline" | "pipes" => PIPE.to_string(),
        "examples" | "example" => EXAMPLES.to_string(),
        t => match command(t) {
            Some(s) => s.to_string(),
            None => format!("unknown topic\n{TOPICS}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_help_fits_450_tokens() {
        let h = text(None);
        assert!(h.len() / 4 <= 450, "{} chars ~{} tokens", h.len(), h.len() / 4);
    }

    #[test]
    fn main_help_documents_new_surface() {
        let h = text(None);
        for k in ["--grep", "--grep-ti", "--grep-ab", "--no-grep", "--fz", "--phrase", "--near", "--since", "--until", "--within", "--new", "--sort date|recent", "snip", "facet", "::", "++", "fields|regex|recency", "-f=-q"] {
            assert!(h.contains(k), "{k}");
        }
    }

    #[test]
    fn regex_and_recency_topics() {
        let r = text(Some("regex"));
        assert_eq!(r.matches(" pm search").count(), 8, "{r}");
        assert!(r.contains("(q<=2 || cites>100) & abs~~'\\bplacebo\\b' & !(type~review)"));
        for op in ["~~", "!~~", "%", "in list", "||", "&&"] {
            assert!(r.contains(op), "{op}");
        }
        let c = text(Some("recency"));
        assert!(c.contains("--new 7d") && c.contains("--since") && c.contains("--within"));
        assert_eq!(text(Some("fuzzy")), r);
        assert!(r.len() / 4 <= 700 && c.len() / 4 <= 250);
    }

    #[test]
    fn facet_and_snip_documented() {
        assert!(text(Some("facet")).contains("FIELDS: comma list of journal year month type q"));
        assert!(text(Some("cmds")).contains("facet <field[,field]>"));
        let f = text(Some("fields"));
        assert!(f.contains("snip|sn") && f.contains("--snips K") && f.contains("-f=-q"));
        assert!(text(Some("examples")).contains("facet journal,type"));
        assert!(text(Some("filters")).contains("--grep-ab RE"));
    }

    #[test]
    fn help_does_not_advertise_ab_tag() {
        for t in ["query", "filters", "regex", "examples"] {
            assert!(!text(Some(t)).contains("[ab]"), "{t}");
        }
    }

    #[test]
    fn help_examples_respect_runtime_rejections() {
        let mut topics: Vec<Option<&str>> = vec![None];
        topics.extend(["cmds", "query", "filters", "fields", "regex", "recency", "pipe", "examples"].map(Some));
        topics.extend(crate::cmd::NAMES.iter().map(|n| Some(*n)));
        let mut seen = 0;
        for topic in topics {
            let h = text(topic);
            for seg in h.split("pm ").skip(1) {
                let seg = seg.lines().next().unwrap_or("");
                let seg = seg.split(" pm ").next().unwrap_or(seg);
                for stage in seg.split(['|', ';']).chain(seg.split("::")) {
                    let toks = match crate::args::split_words(stage.trim()) {
                        Ok(t) => t,
                        Err(_) => continue,
                    };
                    let Some(cmd) = toks.first().map(String::as_str) else { continue };
                    let Ok(args) = crate::args::Args::parse(&toks[1..]) else { continue };
                    match cmd {
                        "related" | "rel" | "citedby" | "cb" | "refs" => {
                            assert!(!args.has("sort"), "links reject --sort: pm {stage}");
                            seen += 1;
                        }
                        "count" | "c" | "trend" => {
                            if let Ok(f) = crate::filter::Filters::from_args(&args) {
                                assert!(!f.needs_post(), "count rejects local-only filters: pm {stage}");
                                seen += 1;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        assert!(seen >= 2, "only {seen} examples checked");
        assert!(!text(Some("citedby")).contains("--sort cites."));
    }

    #[test]
    fn every_topic_resolves() {
        for t in ["cmds", "query", "filters", "fields", "regex", "recency", "pipe", "examples"] {
            assert!(!text(Some(t)).starts_with("unknown"), "{t}");
        }
        for t in TOPICS.split("commands: ").nth(1).unwrap().split(' ') {
            assert!(command(t).is_some(), "{t}");
        }
        assert!(text(Some("s")).starts_with("search"));
    }

    #[test]
    fn unknown_topic_lists_topics() {
        let h = text(Some("zzz"));
        assert!(h.starts_with("unknown topic\n") && !h.contains("zzz") && h.contains("topics: cmds query"));
    }

    #[test]
    fn fields_topic_lists_registry() {
        let h = text(Some("fields"));
        assert!(h.contains("title|t|ti title") && h.contains("cites|cited|citations *"));
        assert!(REG.iter().all(|r| h.contains(r.1)));
    }

    #[test]
    fn main_help_mentions_every_command() {
        let h = text(None);
        for c in ["search", "count", "explain", "trend", "show", "related", "citedby", "refs", "comments", "problems", "exam", "fulltext", "cite", "mesh", "author", "journal", "filter", "sort", "top", "facet", "metrics", "cache", "batch"] {
            assert!(h.contains(c), "{c}");
        }
    }
}
