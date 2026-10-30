use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Author {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fore: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aff: Vec<u16>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub orcid: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub coll: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct JournalInfo {
    pub title: String,
    pub iso: String,
    pub issn: String,
    pub eissn: String,
    pub vol: String,
    pub issue: String,
    pub pages: String,
    pub country: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Mesh {
    pub name: String,
    pub major: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quals: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Grant {
    pub id: String,
    pub agency: String,
    pub country: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Corr {
    pub kind: String,
    pub pmid: Option<u32>,
    pub note: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct JournalRank {
    pub q: u8,
    pub sjr: f32,
    pub h: u32,
    pub area: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Metrics {
    pub cites: u32,
    pub rcr: Option<f32>,
    pub pct: Option<f32>,
    pub apt: Option<f32>,
    pub cpy: Option<f32>,
    pub clin: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Article {
    pub pmid: u32,
    pub title: String,
    #[serde(default)]
    pub abs: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub other_abs: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub abs_cat: Vec<String>,
    #[serde(default)]
    pub authors: Vec<Author>,
    #[serde(default)]
    pub affs: Vec<String>,
    #[serde(default)]
    pub journal: JournalInfo,
    pub year: u16,
    pub date: String,
    #[serde(default)]
    pub epub: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub revised: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub book_date: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub coll_title: String,
    #[serde(default)]
    pub doi: String,
    #[serde(default)]
    pub pmc: String,
    #[serde(default)]
    pub types: Vec<String>,
    #[serde(default)]
    pub mesh: Vec<Mesh>,
    #[serde(default)]
    pub kw: Vec<String>,
    #[serde(default)]
    pub langs: Vec<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub grants: Vec<Grant>,
    #[serde(default)]
    pub banks: Vec<(String, Vec<String>)>,
    #[serde(default)]
    pub corrs: Vec<Corr>,
    #[serde(default)]
    pub nrefs: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jr: Option<JournalRank>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub m: Option<Metrics>,
}

impl Article {
    pub fn abstract_text(&self) -> String {
        let mut s = String::new();
        for (l, t) in &self.abs {
            if !s.is_empty() {
                s.push(' ');
            }
            if !l.is_empty() {
                s.push_str(l);
                s.push_str(": ");
            }
            s.push_str(t);
        }
        s
    }
}

#[derive(Debug, Clone, Default)]
pub struct Table {
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct Set {
    pub ids: Vec<u32>,
    pub arts: Vec<Article>,
    pub total: Option<u64>,
    pub notes: Vec<String>,
    pub table: Option<Table>,
    pub text: Option<String>,
    pub fields: Option<Vec<String>>,
    pub scores: HashMap<u32, f32>,
}

impl Set {
    pub fn from_ids(ids: Vec<u32>) -> Set {
        Set { ids, ..Default::default() }
    }

    pub fn from_arts(arts: Vec<Article>) -> Set {
        Set { ids: arts.iter().map(|a| a.pmid).collect(), arts, ..Default::default() }
    }

    pub fn text(s: impl Into<String>) -> Set {
        Set { text: Some(s.into()), ..Default::default() }
    }

    pub fn table(header: &[&str], rows: Vec<Vec<String>>) -> Set {
        Set {
            table: Some(Table { header: header.iter().map(|s| s.to_string()).collect(), rows }),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Need {
    pub rank: bool,
    pub metrics: bool,
    pub expand: bool,
}

impl Need {
    pub fn or(self, o: Need) -> Need {
        Need { rank: self.rank || o.rank, metrics: self.metrics || o.metrics, expand: self.expand || o.expand }
    }
}

impl Corr {
    pub fn is_comment(&self) -> bool {
        matches!(self.kind.as_str(), "CommentOn" | "CommentIn")
    }

    pub fn is_problem(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "ErratumIn" | "ErratumFor" | "RetractionIn" | "RetractionOf" | "ExpressionOfConcernIn"
                | "ExpressionOfConcernFor" | "PartialRetractionIn" | "PartialRetractionOf"
        )
    }
}

impl Article {
    pub fn has_problems(&self) -> bool {
        self.corrs.iter().any(|c| c.is_problem())
            || self.types.iter().any(|t| matches!(
                    t.as_str(),
                    "Retracted Publication" | "Retraction of Publication" | "Retraction Notice" | "Expression of Concern" | "Published Erratum"
                ))
    }

    pub fn is_retracted(&self) -> bool {
        self.corrs.iter().any(|c| c.kind == "RetractionIn")
            || self.types.iter().any(|t| t == "Retracted Publication")
    }
}
