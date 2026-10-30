use crate::model::*;
use anyhow::{Result, anyhow, bail};
use quick_xml::XmlVersion;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

#[derive(Clone, Copy, PartialEq, Eq)]
enum El {
    PubmedArticle,
    PubmedBookArticle,
    MedlineCitation,
    BookDocument,
    PubmedData,
    PubmedBookData,
    Article,
    Book,
    Pmid,
    ArticleTitle,
    BookTitle,
    CollectionTitle,
    Abstract,
    AbstractText,
    OtherAbstract,
    AuthorList,
    Author,
    LastName,
    ForeName,
    Initials,
    CollectiveName,
    Identifier,
    AffiliationInfo,
    Affiliation,
    Journal,
    JournalIssue,
    Title,
    IsoAbbr,
    Issn,
    Volume,
    Issue,
    PubDate,
    ArticleDate,
    ContributionDate,
    DateRevised,
    Year,
    Month,
    Day,
    MedlineDate,
    MedlinePgn,
    StartPage,
    EndPage,
    ELocationId,
    ArticleIdList,
    ArticleId,
    PublicationType,
    MeshHeading,
    DescriptorName,
    QualifierName,
    KeywordList,
    Keyword,
    Language,
    PublicationStatus,
    Grant,
    GrantId,
    Agency,
    Country,
    MedlineJournalInfo,
    DataBank,
    DataBankName,
    AccessionNumber,
    CommentsCorrections,
    RefSource,
    Reference,
    Skip,
    Other,
}

fn el(name: &str) -> El {
    match name {
        "PubmedArticle" => El::PubmedArticle,
        "PubmedBookArticle" => El::PubmedBookArticle,
        "MedlineCitation" => El::MedlineCitation,
        "BookDocument" => El::BookDocument,
        "PubmedData" => El::PubmedData,
        "PubmedBookData" => El::PubmedBookData,
        "Article" => El::Article,
        "Book" => El::Book,
        "PMID" => El::Pmid,
        "ArticleTitle" => El::ArticleTitle,
        "BookTitle" => El::BookTitle,
        "CollectionTitle" => El::CollectionTitle,
        "Abstract" => El::Abstract,
        "AbstractText" => El::AbstractText,
        "OtherAbstract" => El::OtherAbstract,
        "AuthorList" => El::AuthorList,
        "Author" => El::Author,
        "LastName" => El::LastName,
        "ForeName" => El::ForeName,
        "Initials" => El::Initials,
        "CollectiveName" => El::CollectiveName,
        "Identifier" => El::Identifier,
        "AffiliationInfo" => El::AffiliationInfo,
        "Affiliation" => El::Affiliation,
        "Journal" => El::Journal,
        "JournalIssue" => El::JournalIssue,
        "Title" => El::Title,
        "ISOAbbreviation" => El::IsoAbbr,
        "ISSN" => El::Issn,
        "Volume" => El::Volume,
        "Issue" => El::Issue,
        "PubDate" => El::PubDate,
        "ArticleDate" => El::ArticleDate,
        "ContributionDate" => El::ContributionDate,
        "DateRevised" => El::DateRevised,
        "Year" => El::Year,
        "Month" => El::Month,
        "Day" => El::Day,
        "MedlineDate" => El::MedlineDate,
        "MedlinePgn" => El::MedlinePgn,
        "StartPage" => El::StartPage,
        "EndPage" => El::EndPage,
        "ELocationID" => El::ELocationId,
        "ArticleIdList" => El::ArticleIdList,
        "ArticleId" => El::ArticleId,
        "PublicationType" => El::PublicationType,
        "MeshHeading" => El::MeshHeading,
        "DescriptorName" => El::DescriptorName,
        "QualifierName" => El::QualifierName,
        "KeywordList" => El::KeywordList,
        "Keyword" => El::Keyword,
        "Language" => El::Language,
        "PublicationStatus" => El::PublicationStatus,
        "Grant" => El::Grant,
        "GrantID" => El::GrantId,
        "Agency" => El::Agency,
        "Country" => El::Country,
        "MedlineJournalInfo" => El::MedlineJournalInfo,
        "DataBank" => El::DataBank,
        "DataBankName" => El::DataBankName,
        "AccessionNumber" => El::AccessionNumber,
        "CommentsCorrections" => El::CommentsCorrections,
        "RefSource" => El::RefSource,
        "Reference" => El::Reference,
        "History" | "ChemicalList" | "InvestigatorList" | "SupplMeshList" | "CoiStatement"
        | "Sections" | "GeneSymbolList" | "PersonalNameSubjectList" | "SpaceFlightMission" | "DateCompleted"
        | "VernacularTitle" | "OtherID" => El::Skip,
        _ => El::Other,
    }
}

fn holds_text(e: El) -> bool {
    matches!(
        e,
        El::Pmid
            | El::ArticleTitle
            | El::BookTitle
            | El::CollectionTitle
            | El::AbstractText
            | El::LastName
            | El::ForeName
            | El::Initials
            | El::CollectiveName
            | El::Identifier
            | El::Affiliation
            | El::Title
            | El::IsoAbbr
            | El::Issn
            | El::Volume
            | El::Issue
            | El::Year
            | El::Month
            | El::Day
            | El::MedlineDate
            | El::MedlinePgn
            | El::StartPage
            | El::EndPage
            | El::ELocationId
            | El::ArticleId
            | El::PublicationType
            | El::DescriptorName
            | El::QualifierName
            | El::Keyword
            | El::Language
            | El::PublicationStatus
            | El::GrantId
            | El::Agency
            | El::Country
            | El::DataBankName
            | El::AccessionNumber
            | El::RefSource
    )
}

fn has_markup(e: El) -> bool {
    matches!(
        e,
        El::ArticleTitle
            | El::BookTitle
            | El::CollectionTitle
            | El::AbstractText
            | El::Affiliation
            | El::Keyword
            | El::DescriptorName
            | El::QualifierName
            | El::Agency
            | El::CollectiveName
            | El::Title
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IdKind {
    Doi,
    Pmc,
    Other,
}

#[derive(Default)]
struct Date {
    year: String,
    month: String,
    day: String,
    medline: String,
}

impl Date {
    fn render(&self) -> String {
        let structured = !self.year.trim().is_empty();
        let (mut s, month) = if structured {
            (self.year.trim().to_string(), month_num(&self.month))
        } else {
            medline_date(&self.medline)
        };
        if !month.is_empty() {
            s.push('-');
            s.push_str(&month);
            if structured
                && let Some(d) = self.day.trim().parse::<u8>().ok().filter(|d| (1..=31).contains(d)) {
                    s.push_str(&format!("-{d:02}"));
                }
        }
        s
    }
}

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

fn month_name(s: &str) -> Option<String> {
    let p = s.get(..3)?;
    let i = MONTHS.iter().position(|m| m.eq_ignore_ascii_case(p))?;
    Some(format!("{:02}", i + 1))
}

fn month_num(s: &str) -> String {
    let s = s.trim();
    match s.parse::<u8>() {
        Ok(n) if (1..=12).contains(&n) => format!("{n:02}"),
        Ok(_) => String::new(),
        Err(_) => month_name(s).unwrap_or_default(),
    }
}

fn medline_date(s: &str) -> (String, String) {
    let mut year = String::new();
    for tok in s.split(|c: char| !c.is_ascii_alphanumeric()).filter(|t| !t.is_empty()) {
        let is_year = tok.len() == 4 && tok.bytes().all(|b| b.is_ascii_digit());
        if year.is_empty() {
            if is_year {
                year = tok.to_string();
            }
        } else if is_year {
            break;
        } else if let Some(m) = month_name(tok) {
            return (year, m);
        }
    }
    (year, String::new())
}

fn norm(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for w in s.split_whitespace() {
        if !o.is_empty() {
            o.push(' ');
        }
        o.push_str(w);
    }
    o
}

fn attr(e: &BytesStart, key: &str) -> Option<String> {
    let a = e.try_get_attribute(key).ok()??;
    a.normalized_value(XmlVersion::Implicit1_0).ok().map(|v| v.into_owned())
}

fn invalid(e: &BytesStart) -> bool {
    attr(e, "ValidYN").is_some_and(|v| v.eq_ignore_ascii_case("n"))
}

#[derive(Default)]
struct AuthorBuf {
    last: String,
    fore: String,
    init: String,
    coll: String,
    orcid: String,
    aff: Vec<u16>,
    skip: bool,
    book: bool,
}

struct Parser {
    out: Vec<Article>,
    a: Article,
    stack: Vec<El>,
    buf: String,
    want: bool,
    mixed: bool,
    au: AuthorBuf,
    date: Date,
    contrib: String,
    revised: String,
    abs_label: String,
    abs_cat: String,
    major: bool,
    id_kind: IdKind,
    eloc_doi: bool,
    pg: (String, String, bool),
    orcid_id: bool,
    issn_electronic: bool,
    corr: Corr,
    grant: Grant,
    bank: (String, Vec<String>),
    editors: bool,
    book_authors: Vec<Author>,
}

impl Parser {
    fn new() -> Parser {
        Parser {
            out: Vec::new(),
            a: Article::default(),
            stack: Vec::with_capacity(16),
            buf: String::new(),
            want: false,
            mixed: false,
            au: AuthorBuf::default(),
            date: Date::default(),
            contrib: String::new(),
            revised: String::new(),
            abs_label: String::new(),
            abs_cat: String::new(),
            major: false,
            id_kind: IdKind::Other,
            eloc_doi: false,
            pg: Default::default(),
            orcid_id: false,
            issn_electronic: false,
            corr: Corr::default(),
            grant: Grant::default(),
            bank: Default::default(),
            editors: false,
            book_authors: Vec::new(),
        }
    }

    fn up(&self, n: usize) -> Option<El> {
        self.stack.len().checked_sub(n + 1).map(|i| self.stack[i])
    }

    fn start(&mut self, e: &BytesStart, k: El) {
        if !self.mixed {
            self.buf.clear();
            self.want = holds_text(k);
            self.mixed = has_markup(k);
        }
        self.stack.push(k);
        match k {
            El::PubmedArticle | El::PubmedBookArticle => {
                self.a = Article::default();
                self.pg = Default::default();
                self.contrib.clear();
                self.revised.clear();
                self.book_authors.clear();
            }
            El::AuthorList => self.editors = attr(e, "Type").is_some_and(|t| t.eq_ignore_ascii_case("editors")),
            El::Author => {
                let book = self.up(2) == Some(El::Book);
                self.au = AuthorBuf { skip: (book && self.editors) || invalid(e), book, ..Default::default() };
            }
            El::PubDate | El::ArticleDate | El::ContributionDate | El::DateRevised => self.date = Date::default(),
            El::AbstractText => {
                self.abs_cat = attr(e, "NlmCategory").filter(|c| c != "UNASSIGNED").unwrap_or_default();
                self.abs_label = attr(e, "Label").unwrap_or_else(|| self.abs_cat.clone());
            }
            El::DescriptorName | El::QualifierName => {
                self.major = attr(e, "MajorTopicYN").is_some_and(|v| v == "Y");
            }
            El::ArticleId => {
                self.id_kind = match attr(e, "IdType").as_deref() {
                    _ if invalid(e) => IdKind::Other,
                    Some("doi") => IdKind::Doi,
                    Some("pmc") => IdKind::Pmc,
                    _ => IdKind::Other,
                };
            }
            El::ELocationId => self.eloc_doi = !invalid(e) && attr(e, "EIdType").is_some_and(|v| v == "doi"),
            El::Identifier => self.orcid_id = attr(e, "Source").is_some_and(|v| v.eq_ignore_ascii_case("orcid")),
            El::Issn => self.issn_electronic = attr(e, "IssnType").is_some_and(|v| v == "Electronic"),
            El::CommentsCorrections => {
                self.corr = Corr { kind: attr(e, "RefType").unwrap_or_default(), ..Default::default() };
            }
            El::Grant => self.grant = Grant::default(),
            El::DataBank => self.bank = Default::default(),
            _ => {}
        }
    }

    fn end(&mut self, k: El) {
        if has_markup(k) {
            self.mixed = false;
        }
        self.stack.pop();
        let par = self.stack.last().copied();
        match k {
            El::PubmedArticle | El::PubmedBookArticle => self.finish(),
            El::Pmid => match par {
                Some(El::MedlineCitation | El::BookDocument) if self.a.pmid == 0 => {
                    self.a.pmid = self.buf.trim().parse().unwrap_or(0);
                }
                Some(El::CommentsCorrections) => self.corr.pmid = self.buf.trim().parse().ok(),
                _ => {}
            },
            El::ArticleTitle => self.a.title = norm(&self.buf),
            El::BookTitle => self.a.journal.title = norm(&self.buf),
            El::CollectionTitle if par == Some(El::Book) => self.a.coll_title = norm(&self.buf),
            El::AbstractText if par == Some(El::Abstract) => {
                let t = norm(&self.buf);
                if !t.is_empty() {
                    self.a.abs.push((std::mem::take(&mut self.abs_label), t));
                    self.a.abs_cat.push(std::mem::take(&mut self.abs_cat));
                }
            }
            El::AbstractText if par == Some(El::OtherAbstract) => {
                let t = norm(&self.buf);
                if !t.is_empty() {
                    if !self.a.other_abs.is_empty() {
                        self.a.other_abs.push('\n');
                    }
                    self.a.other_abs.push_str(&t);
                }
                self.abs_label.clear();
                self.abs_cat.clear();
            }
            El::LastName if par == Some(El::Author) => self.au.last = norm(&self.buf),
            El::ForeName if par == Some(El::Author) => self.au.fore = norm(&self.buf),
            El::Initials if par == Some(El::Author) => self.au.init = norm(&self.buf),
            El::CollectiveName if par == Some(El::Author) => self.au.coll = norm(&self.buf),
            El::Identifier if par == Some(El::Author) && self.orcid_id => {
                let t = self.buf.trim();
                self.au.orcid = t.rsplit('/').next().unwrap_or(t).to_string();
            }
            El::Affiliation if par == Some(El::AffiliationInfo) => match self.up(1) {
                Some(El::Author) => {
                    if !self.au.skip
                        && let Some(i) = self.aff_index()
                            && !self.au.aff.contains(&i) {
                                self.au.aff.push(i);
                            }
                }
                _ => {
                    self.aff_index();
                }
            },
            El::Affiliation if par == Some(El::Article) => {
                self.aff_index();
            }
            El::Author => self.finish_author(),
            El::Title if par == Some(El::Journal) => self.a.journal.title = norm(&self.buf),
            El::IsoAbbr if par == Some(El::Journal) => self.a.journal.iso = norm(&self.buf),
            El::Issn if par == Some(El::Journal) => {
                let t = self.buf.trim();
                let j = &mut self.a.journal;
                if self.issn_electronic {
                    j.eissn = t.to_string();
                } else if j.issn.is_empty() {
                    j.issn = t.to_string();
                }
            }
            El::Volume if par == Some(El::JournalIssue) => self.a.journal.vol = self.buf.trim().to_string(),
            El::Issue if par == Some(El::JournalIssue) => self.a.journal.issue = self.buf.trim().to_string(),
            El::MedlinePgn => {
                self.a.journal.pages = self.buf.trim().to_string();
                self.pg.2 = true;
            }
            El::StartPage | El::EndPage => {
                let v = self.buf.trim().to_string();
                if k == El::StartPage { self.pg.0 = v } else { self.pg.1 = v }
                if !self.pg.2 && !self.pg.0.is_empty() {
                    self.a.journal.pages = if self.pg.1.is_empty() || self.pg.1 == self.pg.0 {
                        self.pg.0.clone()
                    } else {
                        format!("{}-{}", self.pg.0, self.pg.1)
                    };
                }
            }
            El::Year if matches!(par, Some(El::PubDate | El::ArticleDate | El::ContributionDate | El::DateRevised)) => self.date.year = self.buf.trim().to_string(),
            El::Month if matches!(par, Some(El::PubDate | El::ArticleDate | El::ContributionDate | El::DateRevised)) => self.date.month = self.buf.trim().to_string(),
            El::Day if matches!(par, Some(El::PubDate | El::ArticleDate | El::ContributionDate | El::DateRevised)) => self.date.day = self.buf.trim().to_string(),
            El::MedlineDate if par == Some(El::PubDate) => self.date.medline = self.buf.trim().to_string(),
            El::PubDate if self.a.date.is_empty() => self.a.date = self.date.render(),
            El::ArticleDate if self.a.epub.is_empty() => self.a.epub = self.date.render(),
            El::ContributionDate if self.contrib.is_empty() => self.contrib = self.date.render(),
            El::DateRevised if par == Some(El::BookDocument) && self.revised.is_empty() => self.revised = self.date.render(),
            El::ELocationId if self.eloc_doi && self.a.doi.is_empty() => self.a.doi = self.buf.trim().to_string(),
            El::ArticleId
                if matches!(self.up(1), Some(El::PubmedData | El::PubmedBookData | El::BookDocument)) =>
            {
                match self.id_kind {
                    IdKind::Doi => self.a.doi = self.buf.trim().to_string(),
                    IdKind::Pmc => self.a.pmc = self.buf.trim().to_string(),
                    IdKind::Other => {}
                }
            }
            El::PublicationType => push_nonempty(&mut self.a.types, &self.buf),
            El::DescriptorName if par == Some(El::MeshHeading) => {
                self.a.mesh.push(Mesh { name: norm(&self.buf), major: self.major, quals: Vec::new() });
            }
            El::QualifierName if par == Some(El::MeshHeading) => {
                if let Some(m) = self.a.mesh.last_mut() {
                    m.major |= self.major;
                    m.quals.push(norm(&self.buf));
                }
            }
            El::Keyword if par == Some(El::KeywordList) => push_nonempty(&mut self.a.kw, &self.buf),
            El::Language => push_nonempty(&mut self.a.langs, &self.buf),
            El::PublicationStatus if self.a.status.is_empty() => self.a.status = self.buf.trim().to_string(),
            El::GrantId if par == Some(El::Grant) => self.grant.id = norm(&self.buf),
            El::Agency if par == Some(El::Grant) => self.grant.agency = norm(&self.buf),
            El::Country => match par {
                Some(El::MedlineJournalInfo) => self.a.journal.country = norm(&self.buf),
                Some(El::Grant) => self.grant.country = norm(&self.buf),
                _ => {}
            },
            El::Grant => {
                let g = std::mem::take(&mut self.grant);
                if !g.id.is_empty() || !g.agency.is_empty() {
                    self.a.grants.push(g);
                }
            }
            El::DataBankName if par == Some(El::DataBank) => self.bank.0 = norm(&self.buf),
            El::AccessionNumber => push_nonempty(&mut self.bank.1, &self.buf),
            El::DataBank => {
                let (name, mut acc) = std::mem::take(&mut self.bank);
                if name.is_empty() || acc.is_empty() {
                } else if let Some((_, v)) = self.a.banks.iter_mut().find(|(n, _)| *n == name) {
                    v.append(&mut acc);
                } else {
                    self.a.banks.push((name, acc));
                }
            }
            El::RefSource if par == Some(El::CommentsCorrections) => self.corr.note = norm(&self.buf),
            El::CommentsCorrections
                if !self.corr.kind.is_empty() => {
                    self.a.corrs.push(std::mem::take(&mut self.corr));
                }
            _ => {}
        }
        if !self.mixed {
            self.want = false;
        }
    }

    fn aff_index(&mut self) -> Option<u16> {
        let t = norm(&self.buf);
        if t.is_empty() {
            return None;
        }
        if let Some(i) = self.a.affs.iter().position(|a| *a == t) {
            return Some(i as u16);
        }
        let i = u16::try_from(self.a.affs.len()).ok()?;
        self.a.affs.push(t);
        Some(i)
    }

    fn finish_author(&mut self) {
        let au = std::mem::take(&mut self.au);
        if au.skip {
            return;
        }
        let coll = !au.coll.is_empty();
        let name = if coll {
            au.coll
        } else if au.last.is_empty() {
            au.fore.clone()
        } else if au.init.is_empty() {
            au.last
        } else {
            format!("{} {}", au.last, au.init)
        };
        if name.is_empty() {
            return;
        }
        let author = Author { name, fore: au.fore, aff: au.aff, orcid: au.orcid, coll };
        if au.book { self.book_authors.push(author) } else { self.a.authors.push(author) }
    }

    fn finish(&mut self) {
        let mut a = std::mem::take(&mut self.a);
        if a.pmid == 0 {
            return;
        }
        if a.title.is_empty() {
            a.title = a.journal.title.clone();
        }
        if !self.contrib.is_empty() {
            let book = std::mem::replace(&mut a.date, std::mem::take(&mut self.contrib));
            if book != a.date {
                a.book_date = book;
            }
        }
        a.revised = std::mem::take(&mut self.revised);
        if a.authors.is_empty() {
            a.authors = std::mem::take(&mut self.book_authors);
        }
        a.year = a.date.get(..4).and_then(|y| y.parse().ok()).unwrap_or(0);
        self.out.push(a);
    }
}

fn push_nonempty(v: &mut Vec<String>, s: &str) {
    let t = norm(s);
    if !t.is_empty() {
        v.push(t);
    }
}

pub fn parse_articles(xml: &str) -> Result<Vec<Article>> {
    let mut rd = Reader::from_str(xml);
    let mut p = Parser::new();
    let mut depth = 0usize;
    loop {
        match rd.read_event().map_err(|e| anyhow!("bad xml: {e}"))? {
            Event::Start(e) if e.name().into_inner() == "ERROR" => {
                let msg = rd.read_text(e.name()).map_err(|e| anyhow!("bad xml: {e}"))?;
                bail!("efetch: {}", crate::http::redact(msg.trim()).chars().take(120).collect::<String>());
            }
            Event::Start(e) => match el(e.name().as_ref()) {
                El::Reference => {
                    p.a.nrefs += 1;
                    rd.read_to_end(e.name()).map_err(|e| anyhow!("bad xml: {e}"))?;
                }
                El::Skip => {
                    rd.read_to_end(e.name()).map_err(|e| anyhow!("bad xml: {e}"))?;
                }
                k => {
                    depth += 1;
                    p.start(&e, k);
                }
            },
            Event::Empty(e) => match el(e.name().as_ref()) {
                El::Reference => p.a.nrefs += 1,
                El::Skip => {}
                k => {
                    p.start(&e, k);
                    p.end(k);
                }
            },
            Event::End(e) => {
                depth = depth.saturating_sub(1);
                p.end(el(e.name().as_ref()));
            }
            Event::Text(t) => {
                if p.want {
                    p.buf.push_str(&t);
                }
            }
            Event::CData(t) => {
                if p.want {
                    p.buf.push_str(&t.xml10_content());
                }
            }
            Event::GeneralRef(r) => {
                if p.want {
                    if let Ok(Some(c)) = r.resolve_char_ref() {
                        p.buf.push(c);
                    } else if let Some(s) = resolve_predefined_entity(&r) {
                        p.buf.push_str(s);
                    }
                }
            }
            Event::Eof => {
                if depth != 0 {
                    bail!("truncated XML");
                }
                break;
            }
            _ => {}
        }
    }
    Ok(p.out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncated_xml_is_an_error() {
        let cut = "<PubmedArticleSet><PubmedArticle><MedlineCitation><PMID>1</PMID><Article><ArticleTitle>T</ArticleTitle>";
        assert_eq!(parse_articles(cut).unwrap_err().to_string(), "truncated XML");
        assert!(parse_articles("<PubmedArticleSet></PubmedArticleSet>").unwrap().is_empty());
    }

    #[test]
    fn error_element_is_an_error() {
        let x = "<?xml version=\"1.0\"?><eFetchResult><ERROR>backend unavailable</ERROR></eFetchResult>";
        assert_eq!(parse_articles(x).unwrap_err().to_string(), "efetch: backend unavailable");
    }

    #[test]
    fn inline_markup_does_not_drop_text() {
        let x = "<PubmedArticleSet><PubmedArticle><MedlineCitation><PMID>8</PMID><Article><ArticleTitle>T</ArticleTitle><GrantList><Grant><GrantID>G1</GrantID><Agency>NIH <i>X</i> Inst</Agency><Country>US</Country></Grant></GrantList></Article><KeywordList><Keyword>CD8<sup>+</sup> T cells</Keyword><Keyword>H<sub>2</sub>O</Keyword></KeywordList><MeshHeadingList><MeshHeading><DescriptorName MajorTopicYN=\"N\">CD8<sup>+</sup> T-Lymphocytes</DescriptorName></MeshHeading></MeshHeadingList></MedlineCitation></PubmedArticle></PubmedArticleSet>";
        let a = &parse_articles(x).unwrap()[0];
        assert_eq!(a.kw, ["CD8+ T cells", "H2O"]);
        assert_eq!(a.mesh[0].name, "CD8+ T-Lymphocytes");
        assert_eq!(a.grants[0].agency, "NIH X Inst");
        assert_eq!(a.grants[0].id, "G1");
    }

    #[test]
    fn other_abstract_is_captured_apart() {
        let x = "<PubmedArticleSet><PubmedArticle><MedlineCitation><PMID>9</PMID><Article><ArticleTitle>T</ArticleTitle><Abstract><AbstractText>Main text.</AbstractText></Abstract></Article><OtherAbstract Type=\"Publisher\" Language=\"spa\"><AbstractText Label=\"A\">Resumen <i>uno</i>.</AbstractText><AbstractText>Dos.</AbstractText></OtherAbstract></MedlineCitation></PubmedArticle></PubmedArticleSet>";
        let a = &parse_articles(x).unwrap()[0];
        assert_eq!(a.abs, vec![(String::new(), "Main text.".to_string())]);
        assert_eq!(a.other_abs, "Resumen uno.\nDos.");
        let none = "<PubmedArticleSet><PubmedArticle><MedlineCitation><PMID>9</PMID></MedlineCitation></PubmedArticle></PubmedArticleSet>";
        assert!(parse_articles(none).unwrap()[0].other_abs.is_empty());
    }

    #[test]
    fn cdata_title_kept() {
        let x = "<PubmedArticleSet><PubmedArticle><MedlineCitation><PMID>7</PMID><Article><ArticleTitle><![CDATA[p<0.05 trial]]></ArticleTitle></Article></MedlineCitation></PubmedArticle></PubmedArticleSet>";
        assert_eq!(parse_articles(x).unwrap()[0].title, "p<0.05 trial");
    }

    const ARTICLE: &str = r#"<?xml version="1.0"?>
<PubmedArticleSet>
<PubmedArticle><MedlineCitation Status="MEDLINE"><PMID Version="1">111</PMID>
<Article PubModel="Print-Electronic">
<Journal><ISSN IssnType="Print">0028-4793</ISSN><JournalIssue CitedMedium="Print"><Volume>382</Volume><Issue>8</Issue><PubDate><Year>2020</Year><Month>Feb</Month><Day>5</Day></PubDate></JournalIssue><Title>The New England journal of medicine</Title><ISOAbbreviation>N Engl J Med</ISOAbbreviation></Journal>
<ArticleTitle>Effect of <i>Foo</i> on H<sub>2</sub>O
   levels &amp; more.</ArticleTitle>
<Pagination><MedlinePgn>727-733</MedlinePgn></Pagination>
<ELocationID EIdType="pii" ValidYN="Y">e1</ELocationID>
<ELocationID EIdType="doi" ValidYN="Y">10.1/abc</ELocationID>
<Abstract>
<AbstractText Label="BACKGROUND" NlmCategory="BACKGROUND">Bg <i>text</i> here.</AbstractText>
<AbstractText Label="METHODS" NlmCategory="METHODS">Did it.</AbstractText>
<AbstractText NlmCategory="CONCLUSIONS">Works &#x2265; 3.</AbstractText>
<CopyrightInformation>(c)</CopyrightInformation>
</Abstract>
<AuthorList CompleteYN="Y">
<Author ValidYN="Y"><LastName>Smith</LastName><ForeName>John A</ForeName><Initials>JA</Initials><Identifier Source="ORCID">https://orcid.org/0000-0001-2345-6789</Identifier><AffiliationInfo><Affiliation> Dept of X,  Univ Y. </Affiliation></AffiliationInfo><AffiliationInfo><Affiliation>Dept Z</Affiliation></AffiliationInfo></Author>
<Author ValidYN="Y"><LastName>Doe</LastName><ForeName>Jane</ForeName><Initials>J</Initials><AffiliationInfo><Affiliation>Dept of X, Univ Y.</Affiliation></AffiliationInfo><AffiliationInfo><Affiliation></Affiliation></AffiliationInfo></Author>
<Author ValidYN="Y"><CollectiveName>Big Study Group</CollectiveName></Author>
</AuthorList>
<Language>eng</Language>
<GrantList><Grant><GrantID>R01 X</GrantID><Acronym>CA</Acronym><Agency>NCI NIH HHS</Agency><Country>United States</Country></Grant></GrantList>
<PublicationTypeList><PublicationType UI="D016428">Journal Article</PublicationType><PublicationType UI="D016449">Randomized Controlled Trial</PublicationType></PublicationTypeList>
<ArticleDate DateType="Electronic"><Year>2020</Year><Month>01</Month><Day>24</Day></ArticleDate>
<DataBankList><DataBank><DataBankName>ClinicalTrials.gov</DataBankName><AccessionNumberList><AccessionNumber>NCT001</AccessionNumber><AccessionNumber>NCT002</AccessionNumber></AccessionNumberList></DataBank></DataBankList>
</Article>
<MedlineJournalInfo><Country>United States</Country></MedlineJournalInfo>
<CommentsCorrectionsList><CommentsCorrections RefType="CommentIn"><RefSource>Lancet. 2020.</RefSource><PMID Version="1">222</PMID></CommentsCorrections><CommentsCorrections RefType="ErratumIn"><RefSource>N Engl J Med. 2020</RefSource></CommentsCorrections></CommentsCorrectionsList>
<MeshHeadingList>
<MeshHeading><DescriptorName UI="D1" MajorTopicYN="N">Humans</DescriptorName></MeshHeading>
<MeshHeading><DescriptorName UI="D2" MajorTopicYN="N">Pneumonia</DescriptorName><QualifierName UI="Q1" MajorTopicYN="N">drug therapy</QualifierName><QualifierName UI="Q2" MajorTopicYN="Y">virology</QualifierName></MeshHeading>
<MeshHeading><DescriptorName UI="D3" MajorTopicYN="Y">Virus</DescriptorName></MeshHeading>
</MeshHeadingList>
<KeywordList Owner="NOTNLM"><Keyword MajorTopicYN="N">covid</Keyword><Keyword>sars</Keyword></KeywordList>
<KeywordList Owner="NOTNLM"><Keyword>other</Keyword></KeywordList>
</MedlineCitation>
<PubmedData><History><PubMedPubDate PubStatus="pubmed"><Year>2019</Year><Month>1</Month><Day>1</Day></PubMedPubDate></History><PublicationStatus>ppublish</PublicationStatus>
<ArticleIdList><ArticleId IdType="pubmed">111</ArticleId><ArticleId IdType="doi">10.1/abc</ArticleId><ArticleId IdType="pmc">PMC999</ArticleId></ArticleIdList>
<ReferenceList><Reference><Citation>c1</Citation><ArticleIdList><ArticleId IdType="doi">10.9/ref</ArticleId></ArticleIdList></Reference><Reference><Citation>c2</Citation></Reference></ReferenceList></PubmedData>
</PubmedArticle>
</PubmedArticleSet>"#;

    #[test]
    fn abstract_categories_stay_aligned() {
        let x = r#"<PubmedArticleSet><PubmedArticle><MedlineCitation><PMID>9</PMID><Article><Abstract>
<AbstractText Label="Findings" NlmCategory="RESULTS">Rates fell.</AbstractText>
<AbstractText Label="Interpretation" NlmCategory="CONCLUSIONS">Drug works.</AbstractText>
<AbstractText Label="Funding" NlmCategory="UNASSIGNED">US National Institutes of Health.</AbstractText>
</Abstract></Article></MedlineCitation></PubmedArticle></PubmedArticleSet>"#;
        let a = &parse_articles(x).unwrap()[0];
        assert_eq!(a.abs.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(), ["Findings", "Interpretation", "Funding"]);
        assert_eq!(a.abs_cat, ["RESULTS", "CONCLUSIONS", ""]);
    }

    #[test]
    fn full_article() {
        let v = parse_articles(ARTICLE).unwrap();
        assert_eq!(v.len(), 1);
        let a = &v[0];
        assert_eq!(a.pmid, 111);
        assert_eq!(a.title, "Effect of Foo on H2O levels & more.");
        assert_eq!(
            a.abs,
            vec![
                ("BACKGROUND".to_string(), "Bg text here.".to_string()),
                ("METHODS".to_string(), "Did it.".to_string()),
                ("CONCLUSIONS".to_string(), "Works \u{2265} 3.".to_string()),
            ]
        );
        assert_eq!(a.authors.len(), 3);
        assert_eq!(a.authors[0].name, "Smith JA");
        assert_eq!(a.authors[0].fore, "John A");
        assert_eq!(a.authors[0].orcid, "0000-0001-2345-6789");
        assert_eq!(a.authors[0].aff, vec![0, 1]);
        assert_eq!(a.authors[1].aff, vec![0]);
        assert_eq!(a.authors[2].name, "Big Study Group");
        assert!(a.authors[2].coll && !a.authors[0].coll);
        assert_eq!(a.affs, vec!["Dept of X, Univ Y.".to_string(), "Dept Z".to_string()]);
        let j = &a.journal;
        assert_eq!(
            (j.title.as_str(), j.iso.as_str(), j.issn.as_str(), j.vol.as_str(), j.issue.as_str(), j.pages.as_str(), j.country.as_str()),
            ("The New England journal of medicine", "N Engl J Med", "0028-4793", "382", "8", "727-733", "United States")
        );
        assert_eq!((a.year, a.date.as_str(), a.epub.as_str()), (2020, "2020-02-05", "2020-01-24"));
        assert_eq!((a.doi.as_str(), a.pmc.as_str(), a.status.as_str()), ("10.1/abc", "PMC999", "ppublish"));
        assert_eq!(a.types, vec!["Journal Article", "Randomized Controlled Trial"]);
        assert_eq!(a.mesh.len(), 3);
        assert!(!a.mesh[0].major && a.mesh[1].major && a.mesh[2].major);
        assert_eq!(a.mesh[1].quals, vec!["drug therapy", "virology"]);
        assert_eq!(a.kw, vec!["covid", "sars", "other"]);
        assert_eq!(a.langs, vec!["eng"]);
        assert_eq!(a.grants.len(), 1);
        assert_eq!((a.grants[0].id.as_str(), a.grants[0].agency.as_str(), a.grants[0].country.as_str()), ("R01 X", "NCI NIH HHS", "United States"));
        assert_eq!(a.banks, vec![("ClinicalTrials.gov".to_string(), vec!["NCT001".to_string(), "NCT002".to_string()])]);
        assert_eq!(a.corrs.len(), 2);
        assert_eq!((a.corrs[0].kind.as_str(), a.corrs[0].pmid, a.corrs[0].note.as_str()), ("CommentIn", Some(222), "Lancet. 2020."));
        assert_eq!((a.corrs[1].kind.as_str(), a.corrs[1].pmid), ("ErratumIn", None));
        assert_eq!(a.nrefs, 2);
        assert!(a.jr.is_none() && a.m.is_none());
    }

    #[test]
    fn book_revision_date_is_kept_and_journal_revisions_are_ignored() {
        let x = r#"<PubmedArticleSet><PubmedBookArticle><BookDocument><PMID>20301579</PMID>
<Book><BookTitle>GeneReviews</BookTitle><PubDate><Year>1993</Year></PubDate></Book>
<ArticleTitle>Chapter</ArticleTitle>
<ContributionDate><Year>2002</Year><Month>03</Month><Day>04</Day></ContributionDate>
<DateRevised><Year>2026</Year><Month>10</Month><Day>08</Day></DateRevised></BookDocument></PubmedBookArticle>
<PubmedArticle><MedlineCitation><PMID>9</PMID><DateRevised><Year>2026</Year><Month>01</Month><Day>02</Day></DateRevised><Article><Journal><JournalIssue><PubDate><Year>2021</Year></PubDate></JournalIssue></Journal><ArticleTitle>J</ArticleTitle></Article></MedlineCitation></PubmedArticle></PubmedArticleSet>"#;
        let v = parse_articles(x).unwrap();
        assert_eq!((v[0].year, v[0].date.as_str(), v[0].revised.as_str()), (2002, "2002-03-04", "2026-10-08"));
        assert_eq!((v[0].book_date.as_str(), v[1].book_date.as_str()), ("1993", ""));
        assert_eq!((v[1].year, v[1].date.as_str(), v[1].revised.as_str()), (2021, "2021", ""));
    }

    #[test]
    fn book_chapter_prefers_contribution_date() {
        let x = r#"<PubmedArticleSet><PubmedBookArticle><BookDocument><PMID>26844336</PMID>
<Book><BookTitle>Endotext</BookTitle><PubDate><Year>2000</Year></PubDate></Book>
<ArticleTitle>Familial Hypercholesterolemia</ArticleTitle>
<ContributionDate><Year>2024</Year><Month>09</Month><Day>19</Day></ContributionDate></BookDocument></PubmedBookArticle>
<PubmedArticle><MedlineCitation><PMID>8</PMID><Article><Journal><JournalIssue><PubDate><Year>2021</Year></PubDate></JournalIssue></Journal><ArticleTitle>J</ArticleTitle></Article></MedlineCitation></PubmedArticle></PubmedArticleSet>"#;
        let v = parse_articles(x).unwrap();
        assert_eq!((v[0].year, v[0].date.as_str()), (2024, "2024-09-19"));
        assert_eq!((v[1].year, v[1].date.as_str()), (2021, "2021"));
    }

    #[test]
    fn book_article() {
        let x = r#"<PubmedArticleSet><PubmedBookArticle><BookDocument><PMID Version="1">5</PMID>
<ArticleIdList><ArticleId IdType="bookaccession">NBK1</ArticleId></ArticleIdList>
<Book><BookTitle book="g">Gene<sup>&#xae;</sup></BookTitle><PubDate><Year>1993</Year></PubDate>
<AuthorList Type="editors"><Author><LastName>Ed</LastName><Initials>E</Initials></Author></AuthorList></Book>
<ArticleTitle>Chapter One</ArticleTitle><Language>eng</Language>
<AuthorList Type="authors"><Author><LastName>Au</LastName><Initials>A</Initials></Author></AuthorList>
<Abstract><AbstractText Label="X">abs</AbstractText></Abstract></BookDocument>
<PubmedBookData><PublicationStatus>ppublish</PublicationStatus><ArticleIdList><ArticleId IdType="pubmed">5</ArticleId></ArticleIdList></PubmedBookData></PubmedBookArticle></PubmedArticleSet>"#;
        let v = parse_articles(x).unwrap();
        let a = &v[0];
        assert_eq!((a.pmid, a.title.as_str(), a.year, a.date.as_str()), (5, "Chapter One", 1993, "1993"));
        assert_eq!(a.journal.title, "Gene\u{ae}");
        assert_eq!(a.authors.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), vec!["Au A"]);
        assert_eq!(a.abs, vec![("X".to_string(), "abs".to_string())]);
        assert_eq!(a.status, "ppublish");
    }

    #[test]
    fn book_collection_title_is_captured() {
        let x = r#"<PubmedArticleSet><PubmedBookArticle><BookDocument><PMID>9</PMID>
<Book><BookTitle>Guide</BookTitle><CollectionTitle>NICE <i>Process</i> and Methods Guides</CollectionTitle><PubDate><Year>2016</Year></PubDate></Book>
<ArticleTitle>Chapter</ArticleTitle></BookDocument></PubmedBookArticle>
<PubmedArticle><MedlineCitation><PMID>10</PMID><Article><ArticleTitle>J</ArticleTitle></Article></MedlineCitation></PubmedArticle></PubmedArticleSet>"#;
        let v = parse_articles(x).unwrap();
        assert_eq!(v[0].coll_title, "NICE Process and Methods Guides");
        assert_eq!(v[0].journal.title, "Guide");
        assert_eq!(v[1].coll_title, "");
    }

    #[test]
    fn whole_book_authors_are_kept_when_no_chapter_authors() {
        let x = r#"<PubmedArticleSet><PubmedBookArticle><BookDocument><PMID>7</PMID>
<Book><BookTitle>Handbook</BookTitle><PubDate><Year>2017</Year></PubDate>
<AuthorList Type="authors"><Author><LastName>Whole</LastName><Initials>B</Initials></Author></AuthorList>
<AuthorList Type="editors"><Author><LastName>Ed</LastName><Initials>E</Initials></Author></AuthorList></Book>
</BookDocument></PubmedBookArticle></PubmedArticleSet>"#;
        let a = &parse_articles(x).unwrap()[0];
        assert_eq!(a.authors.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), vec!["Whole B"]);
    }

    #[test]
    fn book_without_chapter_title_uses_book_title() {
        let x = r#"<PubmedArticleSet><PubmedBookArticle><BookDocument><PMID>6</PMID><Book><BookTitle>Handbook</BookTitle><PubDate><Year>2001</Year></PubDate></Book></BookDocument></PubmedBookArticle></PubmedArticleSet>"#;
        let a = &parse_articles(x).unwrap()[0];
        assert_eq!((a.title.as_str(), a.year), ("Handbook", 2001));
    }

    #[test]
    fn dates() {
        let d = |y: &str, m: &str, dd: &str, ml: &str| {
            Date { year: y.into(), month: m.into(), day: dd.into(), medline: ml.into() }.render()
        };
        assert_eq!(d("2020", "", "", ""), "2020");
        assert_eq!(d("2020", "Mar", "", ""), "2020-03");
        assert_eq!(d("2020", "11", "7", ""), "2020-11-07");
        assert_eq!(d("2020", "", "7", ""), "2020");
        assert_eq!(d("", "", "", "2019 Nov-Dec"), "2019-11");
        assert_eq!(d("", "", "", "1966-1967"), "1966");
        assert_eq!(d("", "", "", "Winter 2008"), "2008");
        assert_eq!(d("", "", "", "1998 Dec-1999 Jan"), "1998-12");
        assert_eq!(d("", "", "", "2001 Spring"), "2001");
        assert_eq!(d("", "", "", ""), "");
    }

    #[test]
    fn medline_date_article() {
        let x = r#"<PubmedArticleSet><PubmedArticle><MedlineCitation><PMID>7</PMID><Article><Journal><JournalIssue><PubDate><MedlineDate>2019 Nov-Dec</MedlineDate></PubDate></JournalIssue></Journal><ArticleTitle>T</ArticleTitle></Article></MedlineCitation></PubmedArticle></PubmedArticleSet>"#;
        let a = &parse_articles(x).unwrap()[0];
        assert_eq!((a.year, a.date.as_str()), (2019, "2019-11"));
    }

    #[test]
    fn legacy_affiliation_and_issn_variants() {
        let x = r#"<PubmedArticleSet><PubmedArticle><MedlineCitation><PMID>8</PMID><Article><Journal><ISSN IssnType="Electronic">1111-2222</ISSN><Title>J</Title></Journal><ArticleTitle>T</ArticleTitle><Affiliation>Old Univ</Affiliation><AuthorList><Author><LastName>A</LastName><ForeName>B</ForeName></Author></AuthorList></Article></MedlineCitation></PubmedArticle></PubmedArticleSet>"#;
        let a = &parse_articles(x).unwrap()[0];
        assert_eq!((a.journal.eissn.as_str(), a.journal.issn.as_str()), ("1111-2222", ""));
        assert_eq!(a.affs, vec!["Old Univ"]);
        assert_eq!(a.authors[0].name, "A");
    }

    #[test]
    fn invalid_authors_and_dois_are_skipped() {
        let x = r#"<PubmedArticleSet><PubmedArticle><MedlineCitation><PMID>9</PMID><Article><Journal><Title>J</Title></Journal><ArticleTitle>T</ArticleTitle>
<ELocationID EIdType="doi" ValidYN="N">10.0/old</ELocationID><ELocationID EIdType="doi" ValidYN="Y">10.1/new</ELocationID>
<AuthorList><Author ValidYN="N"><LastName>Smyth</LastName><Initials>J</Initials><AffiliationInfo><Affiliation>Bad U</Affiliation></AffiliationInfo></Author><Author ValidYN="Y"><LastName>Smith</LastName><Initials>J</Initials></Author><Author><LastName>Lee</LastName><Initials>K</Initials></Author></AuthorList></Article></MedlineCitation>
<PubmedData><ArticleIdList><ArticleId IdType="doi" ValidYN="N">10.2/bad</ArticleId></ArticleIdList></PubmedData></PubmedArticle></PubmedArticleSet>"#;
        let a = &parse_articles(x).unwrap()[0];
        assert_eq!(a.authors.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["Smith J", "Lee K"]);
        assert_eq!(a.doi, "10.1/new");
        assert!(a.affs.is_empty());
    }

    #[test]
    fn book_start_end_pages() {
        let mk = |p: &str| {
            format!(r#"<PubmedArticleSet><PubmedArticle><MedlineCitation><PMID>11</PMID><Article><Journal><Title>J</Title></Journal><ArticleTitle>T</ArticleTitle><Pagination>{p}</Pagination></Article></MedlineCitation></PubmedArticle></PubmedArticleSet>"#)
        };
        let pages = |p: &str| parse_articles(&mk(p)).unwrap()[0].journal.pages.clone();
        assert_eq!(pages("<StartPage>9</StartPage><EndPage>21</EndPage>"), "9-21");
        assert_eq!(pages("<StartPage>9</StartPage>"), "9");
        assert_eq!(pages("<StartPage>9</StartPage><EndPage>9</EndPage>"), "9");
        assert_eq!(pages("<MedlinePgn>727-733</MedlinePgn><StartPage>1</StartPage><EndPage>2</EndPage>"), "727-733");
        assert_eq!(pages("<StartPage>1</StartPage><EndPage>2</EndPage><MedlinePgn>e5</MedlinePgn>"), "e5");
    }

    #[test]
    fn invalid_eloc_doi_only() {
        let x = r#"<PubmedArticleSet><PubmedArticle><MedlineCitation><PMID>10</PMID><Article><Journal><Title>J</Title></Journal><ArticleTitle>T</ArticleTitle><ELocationID EIdType="doi" ValidYN="N">10.0/old</ELocationID></Article></MedlineCitation></PubmedArticle></PubmedArticleSet>"#;
        assert_eq!(parse_articles(x).unwrap()[0].doi, "");
    }

    #[test]
    fn empty_and_malformed() {
        assert!(parse_articles("<PubmedArticleSet></PubmedArticleSet>").unwrap().is_empty());
        assert!(parse_articles("<PubmedArticleSet><PubmedArticle></PubmedArticle></Foo>").is_err());
    }

    #[test]
    fn many_records() {
        let one = ARTICLE.split("<PubmedArticleSet>").nth(1).unwrap().trim_end_matches("</PubmedArticleSet>");
        let big = format!("<PubmedArticleSet>{}</PubmedArticleSet>", one.repeat(200));
        let v = parse_articles(&big).unwrap();
        assert_eq!(v.len(), 200);
        assert_eq!(v[199].mesh.len(), 3);
    }
}
