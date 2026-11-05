//! The OSM objects the graph needs, and their reading from a PBF. The graph builder takes
//! `Element`s, not a file, so its tests need no PBF.

use std::path::Path;

use geko_module_sdk::{GekoError, Result};

pub type Tags = Vec<(String, String)>;

pub fn tag<'a>(tags: &'a Tags, key: &str) -> Option<&'a str> {
    tags.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberKind {
    Node,
    Way,
    Relation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub kind: MemberKind,
    pub id: i64,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Element {
    Node { id: i64, lat: f64, lon: f64, tags: Tags },
    Way { id: i64, refs: Vec<i64>, tags: Tags },
    Relation { id: i64, members: Vec<Member>, tags: Tags },
}

/// Every object of the PBF, in file order (nodes, ways, then relations).
pub fn read_pbf(path: &Path, mut f: impl FnMut(Element)) -> Result<()> {
    let error = |e: osmpbf::Error| GekoError::Data(format!("{}: {e}", path.display()));
    fn tags<'a>(iter: impl Iterator<Item = (&'a str, &'a str)>) -> Tags {
        iter.map(|(k, v)| (k.to_owned(), v.to_owned())).collect()
    }
    let reader = osmpbf::ElementReader::from_path(path).map_err(error)?;
    reader
        .for_each(|element| match element {
            osmpbf::Element::Node(n) => f(Element::Node { id: n.id(), lat: n.lat(), lon: n.lon(), tags: tags(n.tags()) }),
            osmpbf::Element::DenseNode(n) => f(Element::Node { id: n.id(), lat: n.lat(), lon: n.lon(), tags: tags(n.tags()) }),
            osmpbf::Element::Way(w) => f(Element::Way { id: w.id(), refs: w.refs().collect(), tags: tags(w.tags()) }),
            osmpbf::Element::Relation(r) => {
                let members = r
                    .members()
                    .map(|m| Member {
                        kind: match m.member_type {
                            osmpbf::RelMemberType::Node => MemberKind::Node,
                            osmpbf::RelMemberType::Way => MemberKind::Way,
                            osmpbf::RelMemberType::Relation => MemberKind::Relation,
                        },
                        id: m.member_id,
                        role: m.role().unwrap_or_default().to_owned(),
                    })
                    .collect();
                f(Element::Relation { id: r.id(), members, tags: tags(r.tags()) });
            }
        })
        .map_err(error)
}
