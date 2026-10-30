enum Piece {
    Skip,
    Sep,
    S(&'static str),
    C(char),
}

fn fold_piece(c: char) -> Piece {
    let r = match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'č' | 'ĉ' | 'ċ' => "c",
        'ď' | 'đ' | 'ð' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
        'ĵ' => "j",
        'ķ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
        'œ' => "oe",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        'α' => "alpha",
        'β' => "beta",
        'γ' => "gamma",
        'δ' => "delta",
        'ε' => "epsilon",
        'ζ' => "zeta",
        'η' => "eta",
        'θ' => "theta",
        'ι' => "iota",
        'κ' => "kappa",
        'λ' => "lambda",
        'μ' | 'µ' => "mu",
        'ν' => "nu",
        'ξ' => "xi",
        'ο' => "omicron",
        'π' => "pi",
        'ρ' => "rho",
        'σ' | 'ς' => "sigma",
        'τ' => "tau",
        'υ' => "upsilon",
        'φ' => "phi",
        'χ' => "chi",
        'ψ' => "psi",
        'ω' => "omega",
        '\'' | '’' | '`' | '\u{0300}'..='\u{036f}' => return Piece::Skip,
        c if c.is_alphanumeric() => return Piece::C(c),
        _ => return Piece::Sep,
    };
    Piece::S(r)
}

fn fold_into(s: &str, out: &mut String) {
    let start = out.len();
    let mut gap = false;
    let lead = |out: &mut String, gap: &mut bool| {
        if std::mem::take(gap) && out.len() > start {
            out.push(' ');
        }
    };
    for c in s.chars() {
        if c.is_ascii() {
            if c.is_ascii_alphanumeric() {
                lead(out, &mut gap);
                out.push(c.to_ascii_lowercase());
            } else if c != '\'' {
                gap = true;
            }
            continue;
        }
        for l in c.to_lowercase() {
            match fold_piece(l) {
                Piece::Skip => {}
                Piece::Sep => gap = true,
                Piece::S(r) => {
                    lead(out, &mut gap);
                    out.push_str(r);
                }
                Piece::C(x) => {
                    lead(out, &mut gap);
                    out.push(x);
                }
            }
        }
    }
}

pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    fold_into(s, &mut out);
    out
}

pub fn fold_padded(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push(' ');
    fold_into(s, &mut out);
    out.push(' ');
    out
}

pub fn tokens(s: &str) -> Vec<String> {
    fold(s).split(' ').filter(|t| !t.is_empty()).map(str::to_string).collect()
}

fn jaro(a: &[char], b: &[char]) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let window = (a.len().max(b.len()) / 2).saturating_sub(1);
    let mut am = vec![false; a.len()];
    let mut bm = vec![false; b.len()];
    let mut m = 0usize;
    for (i, ca) in a.iter().enumerate() {
        let lo = i.saturating_sub(window);
        let hi = (i + window + 1).min(b.len());
        for j in lo..hi {
            if !bm[j] && b[j] == *ca {
                am[i] = true;
                bm[j] = true;
                m += 1;
                break;
            }
        }
    }
    if m == 0 {
        return 0.0;
    }
    let mut t = 0usize;
    let mut j = 0usize;
    for i in 0..a.len() {
        if am[i] {
            while !bm[j] {
                j += 1;
            }
            if a[i] != b[j] {
                t += 1;
            }
            j += 1;
        }
    }
    let m = m as f32;
    (m / a.len() as f32 + m / b.len() as f32 + (m - (t / 2) as f32) / m) / 3.0
}

pub fn jaro_winkler(a: &str, b: &str) -> f32 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let j = jaro(&a, &b);
    if j < 0.7 {
        return j;
    }
    let p = a.iter().zip(&b).take(4).take_while(|(x, y)| x == y).count();
    j + p as f32 * 0.1 * (1.0 - j)
}

fn is_subseq(abbr: &str, full: &str) -> bool {
    let mut it = full.chars();
    abbr.chars().all(|c| it.any(|f| f == c))
}

fn abbrev_of(abbr: &str, full: &str) -> bool {
    if abbr.is_empty() || full.len() <= abbr.len() || !full.starts_with(abbr.chars().next().unwrap()) {
        return false;
    }
    full.starts_with(abbr) || (abbr.chars().count() >= 3 && abbr.chars().last() == full.chars().last() && is_subseq(abbr, full))
}

pub fn token_sim(a: &str, b: &str) -> f32 {
    if a == b {
        return 1.0;
    }
    let (s, l) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let jw = jaro_winkler(a, b);
    if s.chars().count() >= 3 && abbrev_of(s, l) {
        return jw.max(0.92);
    }
    jw
}

fn best(t: &str, others: &[String]) -> f32 {
    let b = others.iter().map(|o| token_sim(t, o)).fold(0.0, f32::max);
    if b < 0.75 { 0.0 } else { b }
}

fn overlap(ta: &[String], tb: &[String]) -> f32 {
    let s: f32 = ta.iter().map(|t| best(t, tb)).sum::<f32>() + tb.iter().map(|t| best(t, ta)).sum::<f32>();
    s / (ta.len() + tb.len()) as f32
}

pub fn similarity(a: &str, b: &str) -> f32 {
    let (fa, fb) = (fold(a), fold(b));
    if fa.is_empty() || fb.is_empty() {
        return 0.0;
    }
    if fa == fb {
        return 1.0;
    }
    let ta: Vec<String> = fa.split(' ').map(str::to_string).collect();
    let tb: Vec<String> = fb.split(' ').map(str::to_string).collect();
    let ov = overlap(&ta, &tb);
    if ta.len() != tb.len() {
        return ov;
    }
    let (mut sa, mut sb) = (ta, tb);
    sa.sort();
    sb.sort();
    ov.max(jaro_winkler(&fa, &fb)).max(jaro_winkler(&sa.join(" "), &sb.join(" "))).min(1.0)
}

const STOP: &[&str] = &["of", "the", "and", "for", "in", "de", "la", "le", "du", "des", "und", "et", "at", "on"];

fn content(toks: Vec<String>) -> Vec<String> {
    if toks.iter().all(|t| STOP.contains(&t.as_str())) {
        return toks;
    }
    toks.into_iter().filter(|t| !STOP.contains(&t.as_str())).collect()
}

#[derive(Debug, Clone)]
pub struct Needle {
    folded: String,
    toks: Vec<String>,
}

impl Needle {
    pub fn new(s: &str) -> Needle {
        let folded = fold(s);
        let toks = content(folded.split(' ').filter(|t| !t.is_empty()).map(str::to_string).collect());
        Needle { folded, toks }
    }

    pub fn matches(&self, folded_hay: &str, thr: f32) -> bool {
        if self.folded.is_empty() || folded_hay.contains(&self.folded) {
            return true;
        }
        self.toks.iter().all(|n| folded_hay.split(' ').any(|h| !h.is_empty() && token_sim(n, h) >= thr))
    }
}

#[cfg(test)]
pub fn contains_fuzzy(hay: &str, needle: &str, thr: f32) -> bool {
    Needle::new(needle).matches(&fold(hay), thr)
}

fn within<T: Eq + Copy>(a: &[T], b: &[T], k: usize) -> bool {
    if a.len().abs_diff(b.len()) > k {
        return false;
    }
    let p = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let (a, b) = (&a[p..], &b[p..]);
    if a.is_empty() || b.is_empty() {
        return a.len().max(b.len()) <= k;
    }
    k > 0
        && (within(&a[1..], &b[1..], k - 1)
            || within(&a[1..], b, k - 1)
            || within(a, &b[1..], k - 1)
            || (a.len() > 1 && b.len() > 1 && a[0] == b[1] && a[1] == b[0] && within(&a[2..], &b[2..], k - 1)))
}

pub fn edit_within(a: &str, b: &str, k: usize) -> bool {
    if a.is_ascii() && b.is_ascii() {
        within(a.as_bytes(), b.as_bytes(), k)
    } else {
        let (x, y): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
        within(&x, &y, k)
    }
}

#[derive(Debug, Clone)]
pub struct Typo(Vec<(String, bool, usize)>);

impl Typo {
    pub fn new(term: &str) -> Typo {
        let mut words = Vec::new();
        for raw in term.split_whitespace() {
            let prefix = raw.ends_with('*');
            let f = fold(raw);
            let n = f.split(' ').count();
            for (i, t) in f.split(' ').filter(|t| !t.is_empty()).enumerate() {
                let pre = prefix && i + 1 == n;
                let k = match t.chars().count() {
                    0..=4 => 0,
                    5..=8 => 1,
                    _ => 2,
                };
                words.push((t.to_string(), pre, if pre { 0 } else { k }));
            }
        }
        Typo(words)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn in_text(&self, folded_hay: &str) -> bool {
        self.0.iter().all(|(w, pre, k)| {
            folded_hay.split(' ').any(|t| {
                !t.is_empty() && if *pre { t.starts_with(w.as_str()) } else { t == w || (*k > 0 && edit_within(w, t, *k)) }
            })
        })
    }
}

pub fn journal_sim(a: &str, b: &str) -> f32 {
    let ta = content(tokens(a));
    let tb = content(tokens(b));
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let mut sim = similarity(&ta.join(" "), &tb.join(" "));
    if ta.len() == tb.len() {
        let ok = |x: &[String], y: &[String]| x.iter().zip(y).all(|(p, q)| p == q || abbrev_of(p, q));
        if ok(&ta, &tb) || ok(&tb, &ta) {
            sim = sim.max(0.97);
        }
    }
    sim
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Person {
    pub last: String,
    pub ini: String,
}

const PARTICLES: &[&str] =
    &["van", "von", "de", "der", "den", "di", "da", "del", "della", "la", "le", "du", "dos", "das", "el", "al", "bin", "ibn", "ten", "ter", "st"];

fn tail_initials(t: &str) -> Option<String> {
    let n = t.chars().count();
    (t.chars().all(char::is_alphabetic) && (n == 1 || (n <= 3 && t.chars().all(char::is_uppercase)))).then(|| fold(t).replace(' ', ""))
}

fn initials_last_surname(toks: &[&str]) -> String {
    fold(&toks.join(" "))
}

fn given_initials(toks: &[&str]) -> String {
    toks.iter().flat_map(|t| t.split('-')).filter_map(|p| fold(p).chars().next()).collect()
}

fn split_surname(toks: &[&str]) -> (String, String) {
    let n = toks.len();
    let mut s = n - 1;
    while s > 0 && PARTICLES.contains(&fold(toks[s - 1]).as_str()) {
        s -= 1;
    }
    if s < n - 1 {
        (fold(&toks[s..].join(" ")), given_initials(&toks[..s]))
    } else {
        (fold(toks[n - 1]), given_initials(&toks[..n - 1]))
    }
}

fn is_suffix(t: &str) -> bool {
    matches!(
        t.trim_matches(|c| c == '.' || c == ','),
        "Jr" | "Sr" | "jr" | "sr" | "II" | "III" | "IV" | "ii" | "iii" | "iv" | "2nd" | "3rd"
    )
}

fn strip_suffix(s: &str) -> Vec<&str> {
    let mut v: Vec<&str> = s.split_whitespace().collect();
    while v.len() > 1 && v.last().is_some_and(|t| is_suffix(t)) {
        v.pop();
    }
    v
}

pub fn parse_person(s: &str) -> Person {
    if let Some((l, g)) = s.split_once(',') {
        let g = g.split(',').next().unwrap_or("");
        let g = strip_suffix(g).join(" ");
        let l = strip_suffix(l).join(" ");
        let ini = g
            .split_whitespace()
            .flat_map(|t| t.split('-'))
            .map(|t| t.replace('.', ""))
            .map(|t| tail_initials(&t).unwrap_or_else(|| fold(&t).chars().take(1).collect::<String>()))
            .collect();
        return Person { last: fold(&l), ini };
    }
    let mut toks: Vec<&str> = Vec::new();
    for t in strip_suffix(s) {
        let t = t.trim_matches('.');
        let hy: Vec<&str> = t.split('-').map(|p| p.trim_matches('.')).collect();
        if hy.len() > 1 && hy.iter().all(|p| p.chars().count() == 1 && p.chars().all(char::is_alphabetic)) {
            toks.extend(hy);
            continue;
        }
        let parts: Vec<&str> = t.split('.').collect();
        if parts.len() > 1 && parts.iter().all(|p| p.chars().count() == 1 && p.chars().all(char::is_alphabetic)) {
            toks.extend(parts);
        } else if !t.is_empty() {
            toks.push(t);
        }
    }
    let n = toks.len();
    if n == 0 {
        return Person::default();
    }
    if n == 1 {
        return Person { last: fold(toks[0]), ini: String::new() };
    }
    let run = toks[1..].iter().rev().take(3).take_while(|t| t.chars().count() == 1 && t.chars().all(char::is_alphabetic)).count();
    if run > 0 {
        let ini: String = toks[n - run..].iter().map(|t| fold(t)).collect();
        return Person { last: initials_last_surname(&toks[..n - run]), ini };
    }
    if let Some(ini) = tail_initials(toks[n - 1]) {
        return Person { last: initials_last_surname(&toks[..n - 1]), ini };
    }
    let (last, ini) = split_surname(&toks);
    Person { last, ini }
}

pub fn person_sim(q: &Person, a: &Person) -> f32 {
    person_sim_by(q, a, false)
}

pub fn person_sim_loose(q: &Person, a: &Person) -> f32 {
    person_sim_by(q, a, true)
}

fn person_sim_by(q: &Person, a: &Person, loose: bool) -> f32 {
    if q.last.is_empty() || a.last.is_empty() {
        return 0.0;
    }
    let ok = a.ini.starts_with(&q.ini) || (loose && q.ini.starts_with(&a.ini));
    if !q.ini.is_empty() && !ok && !(loose && a.ini.is_empty()) {
        return 0.0;
    }
    if loose { similarity(&q.last, &a.last) } else if fold(&q.last) == fold(&a.last) { 1.0 } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generational_suffixes_are_ignored() {
        for q in ["Kaelin WG Jr", "Kaelin WG Jr.", "Kaelin, WG, Jr", "Kaelin, W G Jr.", "Kaelin WG"] {
            let p = parse_person(q);
            assert_eq!((p.last.as_str(), p.ini.as_str()), ("kaelin", "wg"), "{q}");
        }
        let p = parse_person("Smith J III");
        assert_eq!((p.last.as_str(), p.ini.as_str()), ("smith", "j"));
        assert_eq!(parse_person("Kaelin Jr").last, "kaelin");
        let p = parse_person("Smith JR");
        assert_eq!((p.last.as_str(), p.ini.as_str()), ("smith", "jr"));
        let p = parse_person("Smith, SR");
        assert_eq!((p.last.as_str(), p.ini.as_str()), ("smith", "sr"));
        assert_eq!(person_sim(&parse_person("Kaelin WG Jr"), &parse_person("Kaelin WG")), 1.0);
    }

    #[test]
    fn strict_surname_must_be_equal() {
        let rec = parse_person("Rodriguez V");
        assert_eq!(person_sim(&parse_person("Rodriguez Rodriguez V"), &rec), 0.0);
        assert_eq!(person_sim(&parse_person("Rodriguez V"), &rec), 1.0);
        assert_eq!(person_sim(&parse_person("Müller J"), &parse_person("Muller J")), 1.0);
        assert!(person_sim_loose(&parse_person("Rodriguez Rodriguez V"), &rec) > 0.0);
    }

    #[test]
    fn hyphenated_given_names_give_one_initial_per_part() {
        for (s, last, ini) in [
            ("Jean-Didier Vincent", "vincent", "jd"),
            ("Marie-Claire Dupont", "dupont", "mc"),
            ("John Paul Smith", "smith", "jp"),
            ("Diederik van de Beek", "van de beek", "d"),
        ] {
            let p = parse_person(s);
            assert_eq!((p.last.as_str(), p.ini.as_str()), (last, ini), "{s}");
        }
        assert_eq!(person_sim(&parse_person("Jean-Didier Vincent"), &parse_person("Vincent JL")), 0.0);
        assert_eq!(person_sim(&parse_person("Jean-Didier Vincent"), &parse_person("Vincent JD")), 1.0);
    }

    #[test]
    fn hyphenated_and_dotted_initials() {
        for s in ["Smith J-P", "Smith J.-P.", "Smith JP", "Smith, J-P", "Smith, J.-P."] {
            let p = parse_person(s);
            assert_eq!((p.last.as_str(), p.ini.as_str()), ("smith", "jp"), "{s}");
        }
        let p = parse_person("Smith, Jean-Paul");
        assert_eq!((p.last.as_str(), p.ini.as_str()), ("smith", "jp"));
        assert_eq!(parse_person("Smith-Jones J").last, "smith jones");
        assert!(person_sim(&parse_person("Smith JP"), &parse_person("Smith J-P")) > 0.99);
    }

    #[test]
    fn compound_surnames_before_trailing_initials() {
        for s in ["Schoepflin Sanders S", "Schoepflin Sanders, S", "Schoepflin Sanders S."] {
            let p = parse_person(s);
            assert_eq!((p.last.as_str(), p.ini.as_str()), ("schoepflin sanders", "s"), "{s}");
        }
        let p = parse_person("Diederik van de Beek D");
        assert_eq!((p.last.as_str(), p.ini.as_str()), ("diederik van de beek", "d"));
        let p = parse_person("Sanz de la Garza M");
        assert_eq!((p.last.as_str(), p.ini.as_str()), ("sanz de la garza", "m"));
        assert_eq!(parse_person("Sanz de la Garza, M"), p);
        let p = parse_person("John Paul Smith");
        assert_eq!((p.last.as_str(), p.ini.as_str()), ("smith", "jp"));
        let p = parse_person("Smith JA");
        assert_eq!((p.last.as_str(), p.ini.as_str()), ("smith", "ja"));
        assert!(person_sim(&parse_person("Schoepflin Sanders, S"), &parse_person("Schoepflin Sanders S")) > 0.99);
    }

    #[test]
    fn given_names_before_particle_surnames() {
        let p = parse_person("Diederik van de Beek");
        assert_eq!((p.last.as_str(), p.ini.as_str()), ("van de beek", "d"));
        let p = parse_person("Jan Willem van der Poll");
        assert_eq!((p.last.as_str(), p.ini.as_str()), ("van der poll", "jw"));
        let p = parse_person("van de Beek D");
        assert_eq!((p.last.as_str(), p.ini.as_str()), ("van de beek", "d"));
        assert!(person_sim(&parse_person("Diederik van de Beek"), &parse_person("van de Beek D")) > 0.9);
    }

    #[test]
    fn comma_tails_distinguish_given_names_from_initials() {
        for (s, l, i) in [
            ("Li, Bo", "li", "b"),
            ("Smith, John Paul", "smith", "jp"),
            ("Smith, john", "smith", "j"),
            ("de Azevedo, W F", "de azevedo", "wf"),
            ("Cheng, KT", "cheng", "kt"),
            ("Cheng, K.T.", "cheng", "kt"),
            ("Smith, J", "smith", "j"),
        ] {
            let p = parse_person(s);
            assert_eq!((p.last.as_str(), p.ini.as_str()), (l, i), "{s}");
        }
        assert!(person_sim(&parse_person("Li, Bo"), &parse_person("Li B")) > 0.9);
    }

    #[test]
    fn particle_surnames_without_initials() {
        for (s, l) in [("van der Poll", "van der poll"), ("de la Cruz", "de la cruz"), ("von Neumann", "von neumann")] {
            let p = parse_person(s);
            assert_eq!((p.last.as_str(), p.ini.as_str()), (l, ""), "{s}");
        }
        let p = parse_person("van der Poll T");
        assert_eq!((p.last.as_str(), p.ini.as_str()), ("van der poll", "t"));
        let p = parse_person("john smith");
        assert_eq!((p.last.as_str(), p.ini.as_str()), ("smith", "j"));
        assert!(person_sim(&parse_person("van der Poll"), &parse_person("van der Poll T")) > 0.9);
    }

    #[test]
    fn spaced_single_letter_initials() {
        for (s, l, i) in [
            ("Salomonsson S E", "salomonsson", "se"),
            ("Smith J A", "smith", "ja"),
            ("Smith J. A.", "smith", "ja"),
            ("de Azevedo W F", "de azevedo", "wf"),
            ("Wu J", "wu", "j"),
            ("Gowdar S.D.", "gowdar", "sd"),
            ("Smith J.A.", "smith", "ja"),
            ("de Azevedo W.F.", "de azevedo", "wf"),
            ("John A Smith", "smith", "ja"),
            ("Smith, J A", "smith", "ja"),
        ] {
            let p = parse_person(s);
            assert_eq!((p.last.as_str(), p.ini.as_str()), (l, i), "{s}");
        }
    }

    #[test]
    fn uppercase_tail_means_initials() {
        let p = parse_person("John Wu");
        assert_eq!((p.last.as_str(), p.ini.as_str()), ("wu", "j"));
        let p = parse_person("Wu J");
        assert_eq!((p.last.as_str(), p.ini.as_str()), ("wu", "j"));
        let p = parse_person("Smith JA");
        assert_eq!((p.last.as_str(), p.ini.as_str()), ("smith", "ja"));
        let p = parse_person("smith j");
        assert_eq!((p.last.as_str(), p.ini.as_str()), ("smith", "j"));
    }

    #[test]
    fn comma_initials_kept() {
        assert_eq!(parse_person("Cheng, KT").ini, "kt");
        assert_eq!(parse_person("Cheng, K.T.").ini, "kt");
        assert_eq!(parse_person("Cheng, Kar Ting").ini, "kt");
        assert_eq!(parse_person("Cheng, K").ini, "k");
    }

    #[test]
    fn comma_spaced_initials() {
        let p = parse_person("de Azevedo, W F");
        assert_eq!((p.last.as_str(), p.ini.as_str()), ("de azevedo", "wf"));
        let p = parse_person("Cheng, K T");
        assert_eq!((p.last.as_str(), p.ini.as_str()), ("cheng", "kt"));
        assert_eq!(parse_person("Cheng, K. T.").ini, "kt");
    }

    #[test]
    fn folds() {
        assert_eq!(fold("Müller-Jones, J."), "muller jones j");
        assert_eq!(fold("O'Brien"), "obrien");
        assert_eq!(fold("Łukasz  Żółć"), "lukasz zolc");
        assert_eq!(fold("Straße"), "strasse");
    }

    #[test]
    fn jw_known_values() {
        assert!((jaro_winkler("martha", "marhta") - 0.961).abs() < 0.002);
        assert!((jaro_winkler("dixon", "dicksonx") - 0.813).abs() < 0.002);
        assert_eq!(jaro_winkler("abc", "xyz"), 0.0);
    }

    #[test]
    fn names() {
        assert!(similarity("Müller", "Muller") == 1.0);
        assert!(similarity("Smith J", "J Smith") > 0.99);
        assert!(similarity("smith", "smyth") >= 0.85);
        assert!(similarity("smith", "jones") < 0.6);
    }

    #[test]
    fn people() {
        let q = parse_person("smith j");
        assert_eq!(q, Person { last: "smith".into(), ini: "j".into() });
        assert_eq!(parse_person("Smith JA"), Person { last: "smith".into(), ini: "ja".into() });
        assert_eq!(parse_person("John A Smith"), Person { last: "smith".into(), ini: "ja".into() });
        assert_eq!(parse_person("Smith, John A"), Person { last: "smith".into(), ini: "ja".into() });
        assert_eq!(parse_person("van der Berg J"), Person { last: "van der berg".into(), ini: "j".into() });
        assert_eq!(person_sim(&q, &parse_person("Smith JA")), 1.0);
        assert_eq!(person_sim(&q, &parse_person("Smith AJ")), 0.0);
        assert!(person_sim(&parse_person("Muller K"), &parse_person("Müller KL")) == 1.0);
        assert_eq!(person_sim(&parse_person("smyth j"), &parse_person("Smith J")), 0.0);
        assert!(person_sim_loose(&parse_person("smyth j"), &parse_person("Smith J")) >= 0.85);
        assert!(person_sim(&parse_person("smith"), &parse_person("Smith J")) == 1.0);
    }

    #[test]
    fn affiliations() {
        let h = "Department of Medicine, Harvard Medical School, Boston, MA, USA";
        assert!(contains_fuzzy(h, "harvard med", 0.85));
        assert!(contains_fuzzy("Stanford University", "Stanford Univ", 0.85));
        assert!(contains_fuzzy("Dept of Surgery, Univ of Toronto", "department university toronto", 0.85));
        assert!(!contains_fuzzy(h, "stanford", 0.85));
        assert!(contains_fuzzy(h, "Havard Medical", 0.85));
    }

    #[test]
    fn journals() {
        assert!(journal_sim("J Clin Oncol", "Journal of Clinical Oncology") >= 0.95);
        assert!(journal_sim("Lancet Oncol", "The Lancet. Oncology") >= 0.95);
        assert!(journal_sim("Lancet", "Lancet Oncology") < 0.85);
        assert!(journal_sim("Nature", "Nature Medicine") < 0.85);
        assert!(journal_sim("Nat Med", "Nature Medicine") >= 0.95);
    }

    #[test]
    fn fold_padded_edges() {
        assert_eq!(fold_padded("  Hello,  World! "), " hello world ");
        assert_eq!(fold_padded(""), "  ");
        assert_eq!(fold("a\u{00a0}b\tc"), "a b c");
        assert_eq!(fold("..x.."), "x");
    }

    #[test]
    fn typo_tolerance() {
        let hit = |term: &str, text: &str| Typo::new(term).in_text(&fold_padded(text));
        assert!(hit("hepatitis", "chronic hepatits b"));
        assert!(hit("hepatitis", "hepatitus"));
        assert!(hit("hepatitis", "hepatits")); // 1 deletion
        assert!(hit("hepatitis", "hepatitsi")); // transposition
        assert!(hit("hepatitis", "hepatiitus")); // 2 edits (len>=9)
        assert!(!hit("hepatitis", "hepxxtitus"));
        assert!(hit("cancer", "cancre")); // len 6: 1 edit (transposition)
        assert!(hit("cancer", "cancor"));
        assert!(!hit("cancer", "cannot"));
        assert!(hit("heart", "hear"));
        assert!(hit("heart", "heard"));
        assert!(!hit("rats", "rat"));
        assert!(hit("rats", "rats"));
        assert!(!hit("rats", "cats"));
        assert!(hit("muller", "Müller") && hit("müller", "muller"));
        assert!(hit("immuno*", "immunotherapy"));
        assert!(!hit("immuno*", "immune"));
        assert!(hit("heart failure", "failure of the heart"));
        assert!(!hit("heart failure", "heart attack"));
    }


    #[test]
    fn edit_bounds() {
        assert!(edit_within("abc", "abc", 0));
        assert!(!edit_within("abc", "abd", 0));
        assert!(edit_within("abc", "abd", 1));
        assert!(edit_within("abc", "acb", 1));
        assert!(!edit_within("abcdef", "abxxef", 1));
        assert!(edit_within("abcdef", "abxxef", 2));
        assert!(edit_within("łódź", "lodz", 4));
    }

    #[test]
    fn exact_query_initials_need_record_initials() {
        let q = parse_person("Batejin Q");
        let bare = parse_person("Batejin");
        assert_eq!(person_sim(&q, &bare), 0.0);
        assert!(person_sim_loose(&q, &bare) > 0.9);
        assert!(person_sim(&parse_person("Batejin"), &bare) > 0.9);
        assert!(person_sim(&parse_person("Batejin"), &parse_person("Batejin Q")) > 0.9);
    }

    #[test]
    fn greek_letters_fold_to_their_names() {
        assert_eq!(fold("β-blocker"), "beta blocker");
        assert_eq!(fold("beta-blocker"), "beta blocker");
        assert_eq!(fold("TNF-α and IL-1β"), fold("TNF-alpha and IL-1beta"));
        assert_eq!(fold("µ-opioid"), fold("mu-opioid"));
        assert_eq!(fold("Σ"), "sigma");
        assert_eq!(fold("ς"), "sigma");
        assert_eq!(fold("α2"), "alpha2");
        assert!(contains_fuzzy("a beta-blocker was given", "β-blocker", 0.85));
        assert!(contains_fuzzy("a β-blocker was given", "beta-blocker", 0.85));
    }
}
