//! The file of the graph in the bundle: little-endian, with the counts up front, so that the same
//! graph always gives the same bytes. Read whole for now; laid out so that it can be mapped later.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use geko_module_sdk::{GekoError, Result};

use crate::graph::{Edge, Graph, Node, Restriction, RestrictionKind};

const MAGIC: &[u8; 8] = b"GEKORG02";

pub fn write(graph: &Graph, path: &Path) -> Result<()> {
    let mut out = BufWriter::new(File::create(path)?);
    out.write_all(MAGIC)?;
    for count in [graph.nodes.len(), graph.edges.len(), graph.shapes.len(), graph.restrictions.len()] {
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
        out.write_all(&[e.reversed as u8])?;
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
    out.flush()?;
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let end = self.at + N;
        let slice = self.bytes.get(self.at..end).ok_or_else(|| GekoError::Data("the graph file is truncated".into()))?;
        self.at = end;
        Ok(slice.try_into().expect("N bytes"))
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take::<1>()?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.take()?))
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take()?))
    }
    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_le_bytes(self.take()?))
    }
}

pub fn read(path: &Path) -> Result<Graph> {
    let bytes = std::fs::read(path)?;
    let mut r = Reader { bytes: &bytes, at: 0 };
    if &r.take::<8>()? != MAGIC {
        return Err(GekoError::Data(format!("{}: not a graph file", path.display())));
    }
    let (nodes, edges, shapes, restrictions) = (r.u64()? as usize, r.u64()? as usize, r.u64()? as usize, r.u64()? as usize);
    let mut graph = Graph::default();
    for _ in 0..nodes {
        graph.nodes.push(Node { osm_id: r.i64()?, lat: r.f64()?, lon: r.f64()?, signal: r.u8()? != 0 });
    }
    for _ in 0..edges {
        graph.edges.push(Edge {
            from: r.u32()?,
            to: r.u32()?,
            way: r.i64()?,
            length_m: r.f32()?,
            seconds: r.f32()?,
            shape_start: r.u32()?,
            shape_len: r.u32()?,
            reversed: r.u8()? != 0,
        });
    }
    for _ in 0..shapes {
        graph.shapes.push([r.f64()?, r.f64()?]);
    }
    for _ in 0..restrictions {
        let (from_way, via, to_way) = (r.i64()?, r.u32()?, r.i64()?);
        let kind = if r.u8()? != 0 { RestrictionKind::Mandatory } else { RestrictionKind::Prohibited };
        graph.restrictions.push(Restriction { from_way, via, to_way, kind });
    }
    Ok(graph)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> Graph {
        Graph {
            nodes: vec![Node { osm_id: 7, lat: 39.5, lon: 2.6, signal: true }, Node { osm_id: -3, lat: 39.6, lon: 2.7, signal: false }],
            edges: vec![Edge { from: 0, to: 1, way: 99, length_m: 12.5, seconds: 1.5, shape_start: 0, shape_len: 2, reversed: true }],
            shapes: vec![[39.5, 2.6], [39.6, 2.7]],
            restrictions: vec![Restriction { from_way: 99, via: 1, to_way: 100, kind: RestrictionKind::Mandatory }],
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
}
