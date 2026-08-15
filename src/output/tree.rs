use std::collections::BTreeMap;
use std::fmt::Write as _;

/// One path contributing to the structural overview.
#[derive(Debug, Clone)]
pub struct TreeEntry {
    pub path: String,
    /// Whether the file's content is present in the document, as opposed to being listed only.
    pub included: bool,
}

#[derive(Debug, Default)]
struct Node {
    directories: BTreeMap<String, Node>,
    files: BTreeMap<String, bool>,
}

impl Node {
    fn insert(&mut self, segments: &[&str], included: bool) {
        match segments {
            [] => {}
            [name] => {
                self.files.insert((*name).to_string(), included);
            }
            [head, tail @ ..] => self
                .directories
                .entry((*head).to_string())
                .or_default()
                .insert(tail, included),
        }
    }
}

/// Hierarchy of the packaged paths, rendered differently by each output format.
#[derive(Debug)]
pub struct FileTree {
    root_label: String,
    root: Node,
}

impl FileTree {
    pub fn build(root_label: impl Into<String>, entries: &[TreeEntry]) -> Self {
        let mut root = Node::default();
        for entry in entries {
            let segments: Vec<&str> = entry.path.split('/').filter(|s| !s.is_empty()).collect();
            root.insert(&segments, entry.included);
        }
        Self {
            root_label: root_label.into(),
            root,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.root.directories.is_empty() && self.root.files.is_empty()
    }

    /// Nested rendering for consumers that would rather walk a structure than parse text.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.root_label,
            "type": "directory",
            "children": children_json(&self.root),
        })
    }

    /// Indented box-drawing rendering, with directories before files at each level.
    pub fn render_ascii(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{}/", self.root_label);
        render_ascii_node(&self.root, "", &mut out);
        out
    }

    /// Tag-based rendering that carries the same hierarchy with far fewer characters.
    pub fn render_compact(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "<{}>", self.root_label);
        render_compact_node(&self.root, &mut out);
        let _ = writeln!(out, "</{}>", self.root_label);
        out
    }

    /// Nested elements native to the XML format.
    pub fn render_xml(&self, indent: usize) -> String {
        let mut out = String::new();
        let pad = " ".repeat(indent);
        let _ = writeln!(
            out,
            "{pad}<directory name=\"{}\">",
            super::xml::escape_attribute(&self.root_label)
        );
        render_xml_node(&self.root, indent + 2, &mut out);
        let _ = writeln!(out, "{pad}</directory>");
        out
    }
}

fn render_ascii_node(node: &Node, prefix: &str, out: &mut String) {
    let directories: Vec<_> = node.directories.iter().collect();
    let files: Vec<_> = node.files.iter().collect();
    let total = directories.len() + files.len();

    for (index, (name, child)) in directories.iter().enumerate() {
        let last = index + 1 == total;
        let connector = if last { "└── " } else { "├── " };
        let _ = writeln!(out, "{prefix}{connector}{name}/");
        let child_prefix = format!("{prefix}{}", if last { "    " } else { "│   " });
        render_ascii_node(child, &child_prefix, out);
    }

    for (index, (name, included)) in files.iter().enumerate() {
        let last = directories.len() + index + 1 == total;
        let connector = if last { "└── " } else { "├── " };
        let marker = if **included {
            ""
        } else {
            "  (content omitted)"
        };
        let _ = writeln!(out, "{prefix}{connector}{name}{marker}");
    }
}

/// A file name in the compact rendering, quoted where it would otherwise be ambiguous.
///
/// Names are separated by spaces and an omitted file is marked with a trailing asterisk,
/// so a name containing a space, or ending in an asterisk, could not be told apart from
/// two names or from a marker. Quoting only the names that need it keeps the common case
/// exactly as it was.
fn compact_name(name: &str, included: bool) -> String {
    let marker = if included { "" } else { "*" };
    let ambiguous =
        name.contains(|c: char| c.is_whitespace() || c == '"' || c == '\\') || name.ends_with('*');
    if ambiguous {
        let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"{marker}")
    } else {
        format!("{name}{marker}")
    }
}

/// A directory name used as a tag in the compact rendering.
///
/// Emitted unescaped, a name containing `<` or `&` produced a rendering no reader could
/// parse — and, in the XML format, a document no parser would accept.
fn compact_tag(name: &str) -> String {
    super::xml::escape_attribute(name)
}

fn render_compact_node(node: &Node, out: &mut String) {
    if !node.files.is_empty() {
        let names: Vec<String> = node
            .files
            .iter()
            .map(|(name, included)| compact_name(name, *included))
            .collect();
        let _ = writeln!(out, "{}", names.join(" "));
    }
    for (name, child) in &node.directories {
        let tag = compact_tag(name);
        let _ = writeln!(out, "<{tag}>");
        render_compact_node(child, out);
        let _ = writeln!(out, "</{tag}>");
    }
}

fn render_xml_node(node: &Node, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    for (name, child) in &node.directories {
        let _ = writeln!(
            out,
            "{pad}<directory name=\"{}\">",
            super::xml::escape_attribute(name)
        );
        render_xml_node(child, indent + 2, out);
        let _ = writeln!(out, "{pad}</directory>");
    }
    for (name, included) in &node.files {
        let attribute = if *included {
            String::new()
        } else {
            " contentOmitted=\"true\"".to_string()
        };
        let _ = writeln!(
            out,
            "{pad}<file name=\"{}\"{attribute}/>",
            super::xml::escape_attribute(name)
        );
    }
}

/// Directories before files, matching the order of the file sections in the document.
fn children_json(node: &Node) -> serde_json::Value {
    let mut children = Vec::with_capacity(node.directories.len() + node.files.len());
    for (name, directory) in &node.directories {
        children.push(serde_json::json!({
            "name": name,
            "type": "directory",
            "children": children_json(directory),
        }));
    }
    for (name, included) in &node.files {
        children.push(serde_json::json!({
            "name": name,
            "type": "file",
            "included": included,
        }));
    }
    serde_json::Value::Array(children)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<TreeEntry> {
        vec![
            TreeEntry {
                path: "src/main.zig".into(),
                included: true,
            },
            TreeEntry {
                path: "src/utils.zig".into(),
                included: true,
            },
            TreeEntry {
                path: "README.md".into(),
                included: true,
            },
        ]
    }

    #[test]
    fn ascii_places_directories_before_files() {
        let tree = FileTree::build("my-project", &entries());
        let rendered = tree.render_ascii();
        let expected =
            "my-project/\n├── src/\n│   ├── main.zig\n│   └── utils.zig\n└── README.md\n";
        assert_eq!(rendered, expected);
    }

    #[test]
    fn rendering_is_stable_regardless_of_insertion_order() {
        let mut reversed = entries();
        reversed.reverse();
        assert_eq!(
            FileTree::build("p", &entries()).render_ascii(),
            FileTree::build("p", &reversed).render_ascii()
        );
    }

    #[test]
    fn json_keeps_directories_before_files() {
        let tree = FileTree::build("my-project", &entries());
        let value = tree.to_json();
        let children = value["children"].as_array().unwrap();
        assert_eq!(children[0]["name"], "src");
        assert_eq!(children[0]["type"], "directory");
        assert_eq!(children.last().unwrap()["name"], "README.md");
        assert_eq!(children.last().unwrap()["type"], "file");
    }

    #[test]
    fn compact_uses_tags() {
        let tree = FileTree::build("my-project", &entries());
        let rendered = tree.render_compact();
        assert!(rendered.contains("<src>"));
        assert!(rendered.contains("main.zig utils.zig"));
    }
}

#[cfg(test)]
mod compact_tests {
    use super::*;

    #[test]
    fn a_name_that_would_be_ambiguous_is_quoted() {
        assert_eq!(compact_name("main.rs", true), "main.rs");
        assert_eq!(compact_name("main.rs", false), "main.rs*");
        // Two names, or one name with a space in it? Quoting settles it.
        assert_eq!(compact_name("my file.txt", true), "\"my file.txt\"");
        // A name genuinely ending in an asterisk, against the omitted-content marker.
        assert_eq!(compact_name("star*", true), "\"star*\"");
        assert_eq!(compact_name("star*", false), "\"star*\"*");
    }

    #[test]
    fn a_directory_name_cannot_break_out_of_its_tag() {
        assert_eq!(compact_tag("a<b>c"), "a&lt;b&gt;c");
    }
}
