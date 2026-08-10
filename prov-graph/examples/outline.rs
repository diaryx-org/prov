//! Print a workspace's outline — the whole read-only consumer, in one file.
//!
//! ```text
//! cargo run -p prov-graph --example outline -- <root> [<start-doc>]
//! ```
//!
//! This exists to keep the crate boundary honest. It depends on `prov-graph`
//! and nothing else, so if traversal ever grows a dependency on the mutation
//! engine, the config layer, or the history store, *this example stops
//! compiling* — which is a much louder failure than a module doc that says the
//! read core is self-contained.
//!
//! It is also the shape a real consumer takes. A language server answering
//! "what links here?", a static renderer walking the tree, a browser viewer
//! resolving `[[wikilinks]]` — each is this plus a protocol. Note what is *not*
//! here: no `Storage`, so there is no way to write a byte; no `IndexStore`, so
//! there is no way to change a registration. Those are not omissions of
//! discipline, they are absences the compiler enforces.

use std::path::{Path, PathBuf};

use prov_graph::{
    Graph, NoIndex, Node, NodeKind, ReadSettings, Result, StdFs, block_on, graph::invert,
};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().unwrap_or_else(|| {
        eprintln!("usage: outline <root> [<start-doc>]");
        std::process::exit(2);
    }));
    let start = args.next().unwrap_or_else(|| "index.md".into());

    // `NoIndex` because this consumer has no registry to resolve `id:` links
    // through — they will report as unresolved, which is the honest answer.
    // Point `Ix` at a parsed `registry.yaml` instead and they resolve.
    let graph = Graph::new(StdFs, &root, NoIndex, ReadSettings::default());

    block_on(async {
        // One read scope for the whole run: the tree walk and the census both
        // read the same documents, and within the scope each is parsed once.
        let _scope = graph.read_scope();

        println!("{}", root.display());
        let tree = graph.tree(&start).await?;
        print_node(&tree, "", true);

        let census = graph.census(&start).await?;
        let backlinks = invert(census.clone());
        println!(
            "\n{} links, {} linked-to documents",
            census.len(),
            backlinks.len()
        );

        let mut most: Vec<_> = backlinks.iter().collect();
        most.sort_by_key(|(path, links)| (std::cmp::Reverse(links.len()), (*path).clone()));
        for (path, links) in most.iter().take(5) {
            println!("  {:3} inbound  {}", links.len(), path.display());
        }
        Ok(())
    })
}

/// Render one node and its children as an indented outline.
fn print_node(node: &Node, prefix: &str, last: bool) {
    let name = node
        .title
        .clone()
        .or_else(|| stem(&node.path))
        .unwrap_or_else(|| node.path.display().to_string());
    let note = match &node.kind {
        NodeKind::Doc => String::new(),
        NodeKind::Cycle => "  (cycle)".into(),
        NodeKind::Unreadable(why) => format!("  (unreadable: {why})"),
        other => format!("  ({other:?})"),
    };
    println!("{prefix}{}{name}{note}", if last { "└── " } else { "├── " });

    let child_prefix = format!("{prefix}{}", if last { "    " } else { "│   " });
    let n = node.children.len();
    for (i, child) in node.children.iter().enumerate() {
        print_node(child, &child_prefix, i + 1 == n);
    }
}

fn stem(path: &Path) -> Option<String> {
    path.file_stem()?.to_str().map(str::to_owned)
}
