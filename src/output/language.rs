//! Language identifiers used as the info string on a fenced block.
//!
//! Renderers and models key off the identifier a highlighter would recognise, which is
//! often not the extension: `rs` highlights nothing, `rust` highlights correctly. An
//! extension the table does not know is passed through verbatim rather than dropped or
//! guessed at, so an unusual language still gets whatever support the reader can offer.

/// Whole file names that identify a language on their own, extension or not.
const BY_FILE_NAME: &[(&str, &str)] = &[
    ("Makefile", "makefile"),
    ("makefile", "makefile"),
    ("GNUmakefile", "makefile"),
    ("Dockerfile", "dockerfile"),
    ("Containerfile", "dockerfile"),
    ("Jenkinsfile", "groovy"),
    ("Vagrantfile", "ruby"),
    ("Rakefile", "ruby"),
    ("Gemfile", "ruby"),
    ("Brewfile", "ruby"),
    ("Podfile", "ruby"),
    ("CMakeLists.txt", "cmake"),
    ("go.mod", "go-module"),
    (".gitignore", "gitignore"),
    (".gitattributes", "gitattributes"),
    (".editorconfig", "editorconfig"),
    (".env", "dotenv"),
    (".bashrc", "bash"),
    (".zshrc", "bash"),
];

/// Extension to the identifier a highlighter expects.
const BY_EXTENSION: &[(&str, &str)] = &[
    // systems
    ("rs", "rust"),
    ("go", "go"),
    ("c", "c"),
    ("h", "c"),
    ("cc", "cpp"),
    ("cpp", "cpp"),
    ("cxx", "cpp"),
    ("hpp", "cpp"),
    ("hh", "cpp"),
    ("zig", "zig"),
    ("swift", "swift"),
    ("m", "objectivec"),
    ("mm", "objectivec"),
    ("d", "d"),
    ("nim", "nim"),
    ("v", "v"),
    ("odin", "odin"),
    ("asm", "asm"),
    ("s", "asm"),
    // managed
    ("java", "java"),
    ("kt", "kotlin"),
    ("kts", "kotlin"),
    ("scala", "scala"),
    ("sc", "scala"),
    ("groovy", "groovy"),
    ("cs", "csharp"),
    ("fs", "fsharp"),
    ("fsx", "fsharp"),
    ("vb", "vbnet"),
    ("dart", "dart"),
    // scripting
    ("py", "python"),
    ("pyi", "python"),
    ("pyw", "python"),
    ("rb", "ruby"),
    ("rake", "ruby"),
    ("gemspec", "ruby"),
    ("php", "php"),
    ("pl", "perl"),
    ("pm", "perl"),
    ("lua", "lua"),
    ("r", "r"),
    ("jl", "julia"),
    ("tcl", "tcl"),
    ("ex", "elixir"),
    ("exs", "elixir"),
    ("erl", "erlang"),
    ("hrl", "erlang"),
    ("hs", "haskell"),
    ("lhs", "haskell"),
    ("ml", "ocaml"),
    ("mli", "ocaml"),
    ("clj", "clojure"),
    ("cljs", "clojure"),
    ("cljc", "clojure"),
    ("edn", "clojure"),
    ("scm", "scheme"),
    ("ss", "scheme"),
    ("rkt", "racket"),
    ("lisp", "lisp"),
    ("el", "lisp"),
    ("cr", "crystal"),
    // web
    ("js", "javascript"),
    ("mjs", "javascript"),
    ("cjs", "javascript"),
    ("jsx", "jsx"),
    ("ts", "typescript"),
    ("mts", "typescript"),
    ("cts", "typescript"),
    ("tsx", "tsx"),
    ("vue", "vue"),
    ("svelte", "svelte"),
    ("astro", "astro"),
    ("html", "html"),
    ("htm", "html"),
    ("xhtml", "html"),
    ("css", "css"),
    ("scss", "scss"),
    ("sass", "sass"),
    ("less", "less"),
    ("styl", "stylus"),
    // shells
    ("sh", "bash"),
    ("bash", "bash"),
    ("zsh", "bash"),
    ("ksh", "bash"),
    ("fish", "fish"),
    ("ps1", "powershell"),
    ("psm1", "powershell"),
    ("psd1", "powershell"),
    ("bat", "batch"),
    ("cmd", "batch"),
    ("nu", "nushell"),
    // data and configuration
    ("json", "json"),
    ("jsonc", "jsonc"),
    ("json5", "json5"),
    ("yaml", "yaml"),
    ("yml", "yaml"),
    ("toml", "toml"),
    ("ini", "ini"),
    ("cfg", "ini"),
    ("conf", "ini"),
    ("properties", "properties"),
    ("xml", "xml"),
    ("xsd", "xml"),
    ("xsl", "xml"),
    ("svg", "xml"),
    ("plist", "xml"),
    ("csv", "csv"),
    ("tsv", "tsv"),
    ("proto", "protobuf"),
    ("graphql", "graphql"),
    ("gql", "graphql"),
    ("avsc", "json"),
    // build and infrastructure
    ("gradle", "groovy"),
    ("cmake", "cmake"),
    ("mk", "makefile"),
    ("bzl", "starlark"),
    ("bazel", "starlark"),
    ("star", "starlark"),
    ("tf", "hcl"),
    ("tfvars", "hcl"),
    ("hcl", "hcl"),
    ("nix", "nix"),
    ("dockerfile", "dockerfile"),
    ("sol", "solidity"),
    // markup and documents
    ("md", "markdown"),
    ("markdown", "markdown"),
    ("mdx", "mdx"),
    ("rst", "rst"),
    ("adoc", "asciidoc"),
    ("asciidoc", "asciidoc"),
    ("tex", "latex"),
    ("bib", "bibtex"),
    ("org", "org"),
    ("txt", "text"),
    // query and other
    ("sql", "sql"),
    ("psql", "sql"),
    ("prisma", "prisma"),
    ("wat", "wasm"),
    ("wgsl", "wgsl"),
    ("glsl", "glsl"),
    ("vert", "glsl"),
    ("frag", "glsl"),
    ("hlsl", "hlsl"),
    ("cu", "cuda"),
    ("ipynb", "json"),
    ("patch", "diff"),
    ("diff", "diff"),
    ("log", "log"),
];

/// Identifier for a path, or an empty string when there is nothing useful to say.
pub fn identifier(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);

    if let Some((_, identifier)) = BY_FILE_NAME.iter().find(|(file, _)| *file == name) {
        return identifier;
    }

    let Some((stem, extension)) = name.rsplit_once('.') else {
        return "";
    };
    if stem.is_empty() || extension.is_empty() {
        return "";
    }

    // A borrowed return cannot own a lowercased copy, so the common case is matched
    // directly and only a mixed-case extension pays for the comparison.
    BY_EXTENSION
        .iter()
        .find(|(candidate, _)| *candidate == extension || candidate.eq_ignore_ascii_case(extension))
        .map(|(_, identifier)| *identifier)
        .unwrap_or(extension)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_map_to_the_identifier_a_highlighter_expects() {
        assert_eq!(identifier("src/main.rs"), "rust");
        assert_eq!(identifier("config.yml"), "yaml");
        assert_eq!(identifier("app/index.tsx"), "tsx");
        assert_eq!(identifier("deploy.sh"), "bash");
        assert_eq!(identifier("schema.gql"), "graphql");
        assert_eq!(identifier("main.KT"), "kotlin");
    }

    #[test]
    fn well_known_file_names_are_recognised_without_an_extension() {
        assert_eq!(identifier("Makefile"), "makefile");
        assert_eq!(identifier("docker/Dockerfile"), "dockerfile");
        assert_eq!(identifier("CMakeLists.txt"), "cmake");
        assert_eq!(identifier(".gitignore"), "gitignore");
    }

    #[test]
    fn an_unknown_extension_is_passed_through_rather_than_discarded() {
        assert_eq!(identifier("a/b.weird-ext"), "weird-ext");
        assert_eq!(identifier("model.safetensors"), "safetensors");
    }

    #[test]
    fn a_file_with_nothing_to_say_says_nothing() {
        assert_eq!(identifier("LICENSE"), "");
        assert_eq!(identifier("path/to/README"), "");
    }
}
