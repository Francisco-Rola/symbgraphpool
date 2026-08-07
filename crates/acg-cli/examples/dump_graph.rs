use std::{env, error::Error, fs};

use acg_profile_graph::{GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};

fn main() -> Result<(), Box<dyn Error>> {
    let path = env::args()
        .nth(1)
        .ok_or("usage: dump_graph <artifact.json>")?;

    let bytes = fs::read(path)?;
    let artifact = ProfileGraphArtifact::from_json(&bytes)?;
    let graph = ProfileGraph::load(artifact, GraphLoadConfig::default())?;

    for profile in graph.profiles() {
        println!(
            "ProfileId({:>2})  {}  {}",
            profile.id.0, profile.definition.stable_key, profile.definition.entrypoint_name,
        );

        for adjacency in graph.neighbors(profile.id) {
            let neighbor = graph
                .profile(adjacency.neighbor)
                .expect("adjacency references a loaded profile");

            let edge = &graph.edges()[adjacency.edge_index.0 as usize];

            println!(
                "    -> ProfileId({:>2}) {:<48} relation={:?} score={:.2}",
                neighbor.id.0,
                neighbor.definition.entrypoint_name,
                edge.relation,
                edge.symbolic_score,
            );
        }
    }

    Ok(())
}
