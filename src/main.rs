use jseqio::seq_db::SeqDB;
use jseqio::reader::DynamicFastXReader;
use jseqio::record::*;
use jseqio::writer;
use std::collections::HashMap;
use std::hash::Hash;

#[derive(Copy, Clone, Debug, PartialEq)]
enum Orientation{
    Forward,
    Reverse,
}

impl Orientation {
    fn flip(&self) -> Orientation {
        match self {
            Orientation::Forward => Orientation::Reverse,
            Orientation::Reverse => Orientation::Forward,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Position{
    Start,
    End,
}

#[derive(Copy, Clone, Debug)]
struct Edge{
    from: usize,
    to: usize,
    from_orientation: Orientation,
    to_orientation: Orientation,
}

struct DBG{
    unitigs: SeqDB, // A sequence database with random access to the i-th unitig
    edges: Vec<Vec<Edge>> // edges[i] = outgoing edges from unitig i
}

struct MapValue{
    unitig_id: usize,
    position: Position,
}

fn insert_if_not_present(map: &mut HashMap<Vec<u8>, Vec<MapValue>>, key: &[u8]){
    if !map.contains_key(key){
        map.insert(key.to_owned(), Vec::<MapValue>::new());
    }
}

fn rc(c: u8) -> u8{
    match c{
        b'A' => b'T',
        b'T' => b'A',
        b'G' => b'C',
        b'C' => b'G',
        _ => panic!("Invalid character: {}", c),
    }
}

fn push_edges(from: usize, from_orientation: Orientation, to_orientation: Orientation, to_position: Position, linking_kmer: &[u8], edges: &mut Vec<Vec<Edge>>, borders: &HashMap<Vec<u8>, Vec<MapValue>>){
    if let Some(vec) = borders.get(linking_kmer){
        for x in vec.iter(){
            if x.position == to_position {
                let edge = Edge{from, to: x.unitig_id, from_orientation, to_orientation};
                edges[from].push(edge);
            }
        }
    }
}


fn build_dbg(unitigs: SeqDB, k: usize) -> DBG{
    let mut borders: HashMap<Vec<u8>, Vec<MapValue>> = HashMap::new(); // (k-1)-mer to locations of that k-mer

    let n = unitigs.sequence_count();

    // Build borders map
    for i in 0..n{
        let unitig = unitigs.get(i).unwrap();

        let first = &unitig.seq[..k-1];
        let last = &unitig.seq[unitig.seq.len()-(k-1)..];

        insert_if_not_present(&mut borders, first);
        insert_if_not_present(&mut borders, last);

        borders.get_mut(first).unwrap().push(
            MapValue{
                unitig_id: i, 
                position: Position::Start, 
            }
        );

        borders.get_mut(last).unwrap().push(
            MapValue{
                unitig_id: i, 
                position: Position::End, 
            }
        );
    }

    let mut edges = Vec::<Vec::<Edge>>::new();
    edges.resize_with(n, || Vec::<Edge>::new()); // Allocate n edge lists

    // Build edges
    for i in 0..n{
        let unitig = unitigs.get(i).unwrap().to_owned();
        let unitig_rc = OwnedRecord{
            head: unitig.head.clone(),
            seq: unitig.seq.iter().rev().map(|&c| rc(c)).collect(),
            qual: unitig.qual.clone(),
        };

        let first = &unitig.seq[..k-1];
        let last = &unitig.seq[unitig.seq.len()-(k-1)..];

        let first_rc = &unitig_rc.seq[unitig_rc.seq.len()-(k-1)..];
        let last_rc = &unitig_rc.seq[..k-1];


        push_edges(i, Orientation::Forward, Orientation::Forward, Position::Start, last, &mut edges, &borders);
        push_edges(i, Orientation::Forward, Orientation::Reverse, Position::End, last_rc, &mut edges, &borders);
        push_edges(i, Orientation::Reverse, Orientation::Reverse, Position::End, first, &mut edges, &borders);
        push_edges(i, Orientation::Reverse, Orientation::Forward, Position::Start, first_rc, &mut edges, &borders);

    }

    DBG {unitigs, edges}
}

fn pick_orientations(dbg: &DBG) -> Vec<Orientation>{
    let mut orientations = Vec::<Orientation>::new();
    orientations.resize(dbg.unitigs.sequence_count(), Orientation::Forward);

    let mut visited = vec![false; dbg.unitigs.sequence_count()];

    let mut stack = Vec::<(usize, Orientation)>::new(); // Reused DFS stack between iterations
    let mut n_components: usize = 0;    
    for component_root in 0..dbg.unitigs.sequence_count(){
        if visited[component_root]{
            continue;
        }

        n_components += 1;
        // Arbitrarily orient the root as forward        
        stack.push((component_root, Orientation::Forward));

        let mut component_size: usize = 0;
        // DFS from root and orient all reachable unitigs the same way
        while let Some((unitig_id, orientation)) = stack.pop(){
            if visited[unitig_id]{
                continue;
            }

            component_size += 1;
            visited[unitig_id] = true;
            orientations[unitig_id] = orientation;
    
            for edge in dbg.edges[unitig_id].iter(){
                let next_orientation = match (edge.from_orientation, edge.to_orientation){
                    (Orientation::Forward, Orientation::Forward) => orientation,
                    (Orientation::Forward, Orientation::Reverse) => orientation.flip(),
                    (Orientation::Reverse, Orientation::Forward) => orientation.flip(),
                    (Orientation::Reverse, Orientation::Reverse) => orientation,
                };
                stack.push((edge.to, next_orientation));
            }
        }
        eprintln!("Component size = {}", component_size);
    }

    eprintln!("Found {} component{}", n_components, match n_components > 1 {true => "s", false => ""});

    orientations
}

fn main() {
    let filename = std::env::args().nth(1).unwrap();
    let k = std::env::args().nth(2).unwrap().parse::<usize>().unwrap();
    let reader = DynamicFastXReader::from_file(&filename).unwrap();
    let filetype = reader.filetype();
    let db = reader.into_db().unwrap();
    let dbg = build_dbg(db, k);
    let orientations = pick_orientations(&dbg);

    // Todo: gzip
    let mut writer = jseqio::writer::DynamicFastXWriter::new_to_stdout(filetype, false);
    for i in 0..dbg.unitigs.sequence_count(){
        let orientation = orientations[i];
        let rec: OwnedRecord = match orientation{
            Orientation::Forward => dbg.unitigs.get(i).unwrap().to_owned(),
            Orientation::Reverse => {
                let mut unitig = dbg.unitigs.get(i).unwrap().to_owned();
                unitig.seq = unitig.seq.iter().rev().map(|&c| rc(c)).collect(); // Reverse complement
                unitig
            }
        };
        writer.write(&rec);
    }

}
