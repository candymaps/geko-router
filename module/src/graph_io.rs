//! The file of the graph in the bundle: little-endian, with the counts up front, so that the same
//! graph always gives the same bytes. Read whole for now; laid out so that it can be mapped later.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use geko_module_sdk::{GekoError, Result};

use crate::graph::{Edge, Fragment, Graph, Node, RawRestriction, Restriction, RestrictionKind, UsableWay};
use crate::profile::Direction;

const MAGIC: &[u8; 8] = b"GEKORG04";

/// The file of the fragment of an extract, a work file that never leaves the machine. It ends as it
/// starts, so that a file cut short is told from a whole one.
const FRAGMENT_MAGIC: &[u8; 8] = b"GEKORF01";
const NO_NAME_LEN: u32 = u32::MAX;

pub fn write(graph: &Graph, path: &Path) -> Result<()> {
    let mut out = BufWriter::new(File::create(path)?);
    out.write_all(MAGIC)?;
    for count in [graph.nodes.len(), graph.edges.len(), graph.shapes.len(), graph.restrictions.len(), graph.names.len()] {
        out.write_all(&(count as u64).to_le_bytes())?;
    }
    for n in &graph.nodes {
        out.write_all(&n.osm_id.to_le_bytes())?;
        out.write_all(&n.lat.to_le_bytes())?;
        out.write_all(&n.lon.to_le_bytes())?;
        out.write_all(&[n.signal as u8])?;
    }
    for e in &graph.edges {
        out.write_all(&e.from.to_le_bytes())?;
        out.write_all(&e.to.to_le_bytes())?;
        out.write_all(&e.way.to_le_bytes())?;
        out.write_all(&e.length_m.to_le_bytes())?;
        out.write_all(&e.seconds.to_le_bytes())?;
        out.write_all(&e.shape_start.to_le_bytes())?;
        out.write_all(&e.shape_len.to_le_bytes())?;
        out.write_all(&e.name.to_le_bytes())?;
        out.write_all(&[e.reversed as u8, e.ferry as u8])?;
    }
    for [lat, lon] in &graph.shapes {
        out.write_all(&lat.to_le_bytes())?;
        out.write_all(&lon.to_le_bytes())?;
    }
    for r in &graph.restrictions {
        out.write_all(&r.from_way.to_le_bytes())?;
        out.write_all(&r.via.to_le_bytes())?;
        out.write_all(&r.to_way.to_le_bytes())?;
        out.write_all(&[matches!(r.kind, RestrictionKind::Mandatory) as u8])?;
    }
    for name in &graph.names {
        out.write_all(&(name.len() as u32).to_le_bytes())?;
        out.write_all(name.as_bytes())?;
    }
    out.flush()?;
    Ok(())
}

const NODE_BYTES: usize = 8 + 8 + 8 + 1;
const EDGE_BYTES: usize = 4 + 4 + 8 + 4 + 4 + 4 + 4 + 4 + 1 + 1;
const NAME_HEADER_BYTES: usize = 4;
const SHAPE_BYTES: usize = 16;
const RESTRICTION_BYTES: usize = 8 + 4 + 8 + 1;

/// Reads from a slice whose size `read` has already checked, so that no field can run past its end.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let field = self.bytes[self.at..self.at + N].try_into().expect("N bytes");
        self.at += N;
        field
    }

    fn u8(&mut self) -> u8 {
        self.take::<1>()[0]
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take())
    }
    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.take())
    }
    fn i64(&mut self) -> i64 {
        i64::from_le_bytes(self.take())
    }
    fn f32(&mut self) -> f32 {
        f32::from_le_bytes(self.take())
    }
    fn f64(&mut self) -> f64 {
        f64::from_le_bytes(self.take())
    }
}

pub fn read(path: &Path) -> Result<Graph> {
    let truncated = || GekoError::Data("the graph file is truncated".into());
    let bytes = std::fs::read(path)?;
    if bytes.get(..8) != Some(MAGIC) {
        return Err(GekoError::Data(format!("{}: not a graph file", path.display())));
    }
    let mut r = Reader { bytes: &bytes, at: 8 };
    if bytes.len() < 8 + 5 * 8 {
        return Err(truncated());
    }
    let (nodes, edges, shapes, restrictions, names) = (r.u64() as usize, r.u64() as usize, r.u64() as usize, r.u64() as usize, r.u64() as usize);
    let body = [(nodes, NODE_BYTES), (edges, EDGE_BYTES), (shapes, SHAPE_BYTES), (restrictions, RESTRICTION_BYTES), (names, NAME_HEADER_BYTES)]
        .into_iter()
        .try_fold(0usize, |sum, (count, size)| count.checked_mul(size).and_then(|bytes| sum.checked_add(bytes)))
        .ok_or_else(truncated)?;
    if bytes.len() - r.at < body {
        return Err(truncated());
    }
    let mut graph = Graph::default();
    graph.nodes.reserve_exact(nodes);
    graph.edges.reserve_exact(edges);
    graph.shapes.reserve_exact(shapes);
    graph.restrictions.reserve_exact(restrictions);
    graph.names.reserve_exact(names);
    for _ in 0..nodes {
        graph.nodes.push(Node { osm_id: r.i64(), lat: r.f64(), lon: r.f64(), signal: r.u8() != 0 });
    }
    for _ in 0..edges {
        graph.edges.push(Edge {
            from: r.u32(),
            to: r.u32(),
            way: r.i64(),
            length_m: r.f32(),
            seconds: r.f32(),
            shape_start: r.u32(),
            shape_len: r.u32(),
            name: r.u32(),
            reversed: r.u8() != 0,
            ferry: r.u8() != 0,
        });
    }
    for _ in 0..shapes {
        graph.shapes.push([r.f64(), r.f64()]);
    }
    for _ in 0..restrictions {
        let (from_way, via, to_way) = (r.i64(), r.u32(), r.i64());
        let kind = if r.u8() != 0 { RestrictionKind::Mandatory } else { RestrictionKind::Prohibited };
        graph.restrictions.push(Restriction { from_way, via, to_way, kind });
    }
    for _ in 0..names {
        if bytes.len() - r.at < NAME_HEADER_BYTES {
            return Err(truncated());
        }
        let len = r.u32() as usize;
        if bytes.len() - r.at < len {
            return Err(truncated());
        }
        let name = std::str::from_utf8(&bytes[r.at..r.at + len]).map_err(|_| GekoError::Data("the graph file has a name that is not UTF-8".into()))?;
        graph.names.push(name.to_owned());
        r.at += len;
    }
    Ok(graph)
}

pub fn write_fragment(fragment: &Fragment, path: &Path) -> Result<()> {
    let mut out = BufWriter::new(File::create(path)?);
    out.write_all(FRAGMENT_MAGIC)?;
    for count in [fragment.ways.len(), fragment.restrictions.len(), fragment.nodes.len()] {
        out.write_all(&(count as u64).to_le_bytes())?;
    }
    for w in &fragment.ways {
        out.write_all(&w.id.to_le_bytes())?;
        out.write_all(&w.speed_kmh.to_le_bytes())?;
        out.write_all(&w.duration_s.unwrap_or(f32::NAN).to_le_bytes())?;
        out.write_all(&[match w.direction {
            Direction::Both => 0,
            Direction::Forward => 1,
            Direction::Backward => 2,
        }, w.ferry as u8])?;
        out.write_all(&(w.refs.len() as u32).to_le_bytes())?;
        for node in &w.refs {
            out.write_all(&node.to_le_bytes())?;
        }
        match &w.name {
            Some(name) => {
                out.write_all(&(name.len() as u32).to_le_bytes())?;
                out.write_all(name.as_bytes())?;
            }
            None => out.write_all(&NO_NAME_LEN.to_le_bytes())?,
        }
    }
    for r in &fragment.restrictions {
        for id in [r.id, r.from_way, r.via_node, r.to_way] {
            out.write_all(&id.to_le_bytes())?;
        }
        out.write_all(&[matches!(r.kind, RestrictionKind::Mandatory) as u8])?;
    }
    for (id, lat, lon, signal) in &fragment.nodes {
        out.write_all(&id.to_le_bytes())?;
        out.write_all(&lat.to_le_bytes())?;
        out.write_all(&lon.to_le_bytes())?;
        out.write_all(&[*signal as u8])?;
    }
    out.write_all(FRAGMENT_MAGIC)?;
    out.flush()?;
    Ok(())
}

pub fn read_fragment(path: &Path) -> Result<Fragment> {
    let bytes = std::fs::read(path)?;
    let invalid = || GekoError::Data(format!("{}: not a whole fragment", path.display()));
    if bytes.len() < 8 + 3 * 8 + 8 || bytes.get(..8) != Some(FRAGMENT_MAGIC) || !bytes.ends_with(FRAGMENT_MAGIC) {
        return Err(invalid());
    }
    let end = bytes.len() - 8;
    let mut r = Reader { bytes: &bytes[..end], at: 8 };
    let (ways, restrictions, nodes) = (r.u64() as usize, r.u64() as usize, r.u64() as usize);
    let mut fragment = Fragment::default();
    for _ in 0..ways {
        if end - r.at < 8 + 4 + 4 + 2 + 4 {
            return Err(invalid());
        }
        let (id, speed_kmh, duration_s) = (r.i64(), r.f32(), r.f32());
        let direction = match r.u8() {
            0 => Direction::Both,
            1 => Direction::Forward,
            2 => Direction::Backward,
            _ => return Err(invalid()),
        };
        let ferry = r.u8() != 0;
        let refs = r.u32() as usize;
        if (end - r.at) / 8 < refs || end - r.at - refs * 8 < 4 {
            return Err(invalid());
        }
        let refs = (0..refs).map(|_| r.i64()).collect();
        let name = match r.u32() {
            NO_NAME_LEN => None,
            len => {
                let len = len as usize;
                if end - r.at < len {
                    return Err(invalid());
                }
                let name = std::str::from_utf8(&bytes[r.at..r.at + len]).map_err(|_| invalid())?.to_owned();
                r.at += len;
                Some(name)
            }
        };
        fragment.ways.push(UsableWay { id, refs, speed_kmh, direction, ferry, duration_s: (!duration_s.is_nan()).then_some(duration_s), name });
    }
    let fixed = restrictions.checked_mul(33).and_then(|a| nodes.checked_mul(25).and_then(|b| a.checked_add(b))).ok_or_else(invalid)?;
    if end - r.at != fixed {
        return Err(invalid());
    }
    fragment.restrictions.reserve_exact(restrictions);
    for _ in 0..restrictions {
        let (id, from_way, via_node, to_way) = (r.i64(), r.i64(), r.i64(), r.i64());
        let kind = if r.u8() != 0 { RestrictionKind::Mandatory } else { RestrictionKind::Prohibited };
        fragment.restrictions.push(RawRestriction { id, from_way, via_node, to_way, kind });
    }
    fragment.nodes.reserve_exact(nodes);
    for _ in 0..nodes {
        fragment.nodes.push((r.i64(), r.f64(), r.f64(), r.u8() != 0));
    }
    Ok(fragment)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fragment() -> Fragment {
        Fragment {
            ways: vec![
                UsableWay { id: 5, refs: vec![1, -2], speed_kmh: 50.0, direction: Direction::Backward, ferry: true, duration_s: Some(600.0), name: Some("Ma-13".into()) },
                UsableWay { id: 6, refs: vec![2, 3, 4], speed_kmh: 25.0, direction: Direction::Both, ferry: false, duration_s: None, name: None },
            ],
            restrictions: vec![RawRestriction { id: 9, from_way: 5, via_node: 2, to_way: 6, kind: RestrictionKind::Mandatory }],
            nodes: vec![(-2, 39.5, 2.6, true), (1, 39.6, 2.7, false)],
        }
    }

    #[test]
    fn a_fragment_round_trips_and_one_cut_short_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.bin"), dir.path().join("b.bin"));
        write_fragment(&fragment(), &a).unwrap();
        let read = read_fragment(&a).unwrap();
        write_fragment(&read, &b).unwrap();
        let bytes = std::fs::read(&a).unwrap();
        assert_eq!(bytes, std::fs::read(&b).unwrap());
        assert_eq!(read.ways[0].duration_s, Some(600.0));
        assert_eq!(read.ways[1].duration_s, None);
        assert_eq!(read.ways[0].name.as_deref(), Some("Ma-13"));
        assert_eq!(read.nodes, fragment().nodes);
        for cut in [1, 9, bytes.len() / 2] {
            std::fs::write(&b, &bytes[..bytes.len() - cut]).unwrap();
            assert!(read_fragment(&b).is_err(), "{cut}");
        }
        let mut wrong = bytes.clone();
        wrong[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
        std::fs::write(&b, &wrong).unwrap();
        assert!(read_fragment(&b).is_err());
    }

    fn graph() -> Graph {
        Graph {
            nodes: vec![Node { osm_id: 7, lat: 39.5, lon: 2.6, signal: true }, Node { osm_id: -3, lat: 39.6, lon: 2.7, signal: false }],
            edges: vec![Edge { from: 0, to: 1, way: 99, length_m: 12.5, seconds: 1.5, shape_start: 0, shape_len: 2, name: 0, reversed: true, ferry: true }],
            shapes: vec![[39.5, 2.6], [39.6, 2.7]],
            restrictions: vec![Restriction { from_way: 99, via: 1, to_way: 100, kind: RestrictionKind::Mandatory }],
            names: vec!["Carrer de Sant Miquel".into(), "Ma-13".into()],
        }
    }

    #[test]
    fn round_trip_and_same_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.bin"), dir.path().join("b.bin"));
        write(&graph(), &a).unwrap();
        write(&graph(), &b).unwrap();
        assert_eq!(read(&a).unwrap(), graph());
        assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
    }

    #[test]
    fn broken_files_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.bin");
        std::fs::write(&path, b"not a graph at all").unwrap();
        assert!(read(&path).is_err());
        write(&graph(), &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 5]).unwrap();
        assert!(read(&path).is_err());
    }

    #[test]
    fn counts_that_do_not_fit_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.bin");
        let mut bytes = MAGIC.to_vec();
        for count in [u64::MAX, 1, 1, 1, 1] {
            bytes.extend_from_slice(&count.to_le_bytes());
        }
        std::fs::write(&path, &bytes).unwrap();
        assert!(read(&path).is_err());
        std::fs::write(&path, &bytes[..20]).unwrap();
        assert!(read(&path).is_err());
    }
}
