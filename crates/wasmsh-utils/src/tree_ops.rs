//! Tree utility: tree.

use wasmsh_fs::Vfs;

use crate::helpers::{child_path, resolve_path, simple_glob_match};
use crate::UtilContext;

/// Display directory contents in a tree-like format.
pub(crate) fn util_tree(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut max_depth: Option<usize> = None;
    let mut dirs_only = false;
    let mut show_hidden = false;
    let mut exclude_pattern: Option<String> = None;
    let mut full_path = false;
    let mut noreport = false;
    let mut json_output = false;

    // Parse flags
    while let Some(arg) = args.first() {
        match *arg {
            "-L" if args.len() > 1 => {
                max_depth = args[1].parse().ok();
                args = &args[2..];
            }
            "-d" => {
                dirs_only = true;
                args = &args[1..];
            }
            "-a" => {
                show_hidden = true;
                args = &args[1..];
            }
            "-I" if args.len() > 1 => {
                exclude_pattern = Some(args[1].to_string());
                args = &args[2..];
            }
            "-f" => {
                full_path = true;
                args = &args[1..];
            }
            "--noreport" => {
                noreport = true;
                args = &args[1..];
            }
            "-J" => {
                json_output = true;
                args = &args[1..];
            }
            _ if arg.starts_with('-') => {
                let msg = format!("tree: unknown option '{arg}'\n");
                ctx.output.stderr(msg.as_bytes());
                return 1;
            }
            _ => break,
        }
    }

    // Remaining args are paths to display
    let root = if args.is_empty() { "." } else { args[0] };
    let root_path = resolve_path(ctx.cwd, root);

    // Verify the root exists and is a directory
    match ctx.fs.stat(&root_path) {
        Ok(meta) if meta.is_dir => {}
        Ok(_) => {
            let msg = format!("tree: {root}: not a directory\n");
            ctx.output.stderr(msg.as_bytes());
            return 1;
        }
        Err(e) => {
            let msg = format!("tree: {root}: {e}\n");
            ctx.output.stderr(msg.as_bytes());
            return 1;
        }
    }

    let opts = TreeOpts {
        max_depth,
        dirs_only,
        show_hidden,
        exclude_pattern,
        full_path,
    };

    if json_output {
        let node = build_json_tree(ctx, &root_path, &opts, 0);
        let mut buf = String::new();
        json_emit(&node, &mut buf, 0);
        buf.push('\n');
        ctx.output.stdout(buf.as_bytes());
    } else {
        // Print the root line
        ctx.output.stdout(b".\n");

        let mut dir_count: usize = 0;
        let mut file_count: usize = 0;
        let root_for_walk = root_path.clone();
        walk_tree(
            ctx,
            &root_path,
            &root_for_walk,
            &opts,
            &mut Vec::new(),
            0,
            &mut dir_count,
            &mut file_count,
        );

        if !noreport {
            let summary = if dirs_only {
                format!(
                    "\n{dir_count} director{}\n",
                    if dir_count == 1 { "y" } else { "ies" }
                )
            } else {
                format!(
                    "\n{dir_count} director{}, {file_count} file{}\n",
                    if dir_count == 1 { "y" } else { "ies" },
                    if file_count == 1 { "" } else { "s" },
                )
            };
            ctx.output.stdout(summary.as_bytes());
        }
    }

    0
}

struct TreeOpts {
    max_depth: Option<usize>,
    dirs_only: bool,
    show_hidden: bool,
    exclude_pattern: Option<String>,
    full_path: bool,
}

fn should_show(name: &str, opts: &TreeOpts) -> bool {
    if !opts.show_hidden && name.starts_with('.') {
        return false;
    }
    if let Some(ref pat) = opts.exclude_pattern {
        if simple_glob_match(pat, name) {
            return false;
        }
    }
    true
}

fn walk_tree(
    ctx: &mut UtilContext<'_>,
    dir_path: &str,
    root_path: &str,
    opts: &TreeOpts,
    prefix_parts: &mut Vec<bool>,
    depth: usize,
    dir_count: &mut usize,
    file_count: &mut usize,
) {
    if opts.max_depth.is_some_and(|max| depth >= max) {
        return;
    }

    let visible = match tree_visible_entries(ctx, dir_path, opts) {
        Ok(entries) => entries,
        Err(()) => return,
    };

    for (idx, entry) in visible.iter().enumerate() {
        let is_last = idx == visible.len() - 1;
        let prefix = tree_prefix(prefix_parts, is_last);
        let display_name = tree_display_name(dir_path, root_path, &entry.name, opts.full_path);
        let line = format!("{prefix}{display_name}\n");
        ctx.output.stdout(line.as_bytes());

        if entry.is_dir {
            *dir_count += 1;
            let child = child_path(dir_path, &entry.name);
            prefix_parts.push(!is_last);
            walk_tree(
                ctx,
                &child,
                root_path,
                opts,
                prefix_parts,
                depth + 1,
                dir_count,
                file_count,
            );
            prefix_parts.pop();
        } else {
            *file_count += 1;
        }
    }
}

fn tree_visible_entries(
    ctx: &mut UtilContext<'_>,
    dir_path: &str,
    opts: &TreeOpts,
) -> Result<Vec<wasmsh_fs::DirEntry>, ()> {
    match ctx.fs.read_dir(dir_path) {
        Ok(mut entries) => {
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(entries
                .into_iter()
                .filter(|entry| should_show(&entry.name, opts) && (!opts.dirs_only || entry.is_dir))
                .collect())
        }
        Err(e) => {
            let msg = format!("tree: {dir_path}: {e}\n");
            ctx.output.stderr(msg.as_bytes());
            Err(())
        }
    }
}

fn tree_prefix(prefix_parts: &[bool], is_last: bool) -> String {
    let mut prefix = String::new();
    for &has_more in prefix_parts {
        prefix.push_str(if has_more { "\u{2502}   " } else { "    " });
    }
    prefix.push_str(if is_last {
        "\u{2514}\u{2500}\u{2500} "
    } else {
        "\u{251c}\u{2500}\u{2500} "
    });
    prefix
}

fn tree_display_name(dir_path: &str, root_path: &str, name: &str, full_path: bool) -> String {
    if !full_path {
        return name.to_string();
    }
    let abs_child = child_path(dir_path, name);
    let relative = abs_child
        .strip_prefix(root_path)
        .and_then(|rest| rest.strip_prefix('/').or(Some(rest)))
        .unwrap_or(&abs_child);
    format!("./{relative}")
}

// ---------------------------------------------------------------------------
// JSON output support
// ---------------------------------------------------------------------------

struct JsonNode {
    name: String,
    is_dir: bool,
    children: Option<Vec<JsonNode>>,
}

fn build_json_tree(
    ctx: &mut UtilContext<'_>,
    dir_path: &str,
    opts: &TreeOpts,
    depth: usize,
) -> JsonNode {
    let name = if depth == 0 {
        ".".to_string()
    } else {
        dir_path.rsplit('/').next().unwrap_or(dir_path).to_string()
    };

    JsonNode {
        name,
        is_dir: true,
        children: Some(json_child_nodes(ctx, dir_path, opts, depth)),
    }
}

fn json_child_nodes(
    ctx: &mut UtilContext<'_>,
    dir_path: &str,
    opts: &TreeOpts,
    depth: usize,
) -> Vec<JsonNode> {
    if opts.max_depth.is_some_and(|max| depth >= max) {
        return Vec::new();
    }
    let Ok(mut entries) = ctx.fs.read_dir(dir_path) else {
        return Vec::new();
    };
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
        .into_iter()
        .filter(|entry| should_show(&entry.name, opts) && (!opts.dirs_only || entry.is_dir))
        .map(|entry| {
            if entry.is_dir {
                build_json_tree(ctx, &child_path(dir_path, &entry.name), opts, depth + 1)
            } else {
                JsonNode {
                    name: entry.name,
                    is_dir: false,
                    children: None,
                }
            }
        })
        .collect()
}

fn json_emit(node: &JsonNode, buf: &mut String, indent: usize) {
    let pad = "  ".repeat(indent);
    let pad_inner = "  ".repeat(indent + 1);

    buf.push_str(&pad);
    buf.push('{');
    buf.push('\n');

    // "name"
    buf.push_str(&pad_inner);
    buf.push_str("\"name\": \"");
    json_escape_into(buf, &node.name);
    buf.push('"');

    // "type"
    buf.push_str(",\n");
    buf.push_str(&pad_inner);
    buf.push_str("\"type\": \"");
    buf.push_str(if node.is_dir { "directory" } else { "file" });
    buf.push('"');

    // "contents" (only for directories)
    if let Some(ref children) = node.children {
        buf.push_str(",\n");
        buf.push_str(&pad_inner);
        buf.push_str("\"contents\": [\n");
        for (i, child) in children.iter().enumerate() {
            json_emit(child, buf, indent + 2);
            if i + 1 < children.len() {
                buf.push(',');
            }
            buf.push('\n');
        }
        buf.push_str(&pad_inner);
        buf.push(']');
    }

    buf.push('\n');
    buf.push_str(&pad);
    buf.push('}');
}

fn json_escape_into(buf: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '"' => buf.push_str("\\\""),
            '\\' => buf.push_str("\\\\"),
            '\n' => buf.push_str("\\n"),
            '\r' => buf.push_str("\\r"),
            '\t' => buf.push_str("\\t"),
            c if c.is_control() => {
                use std::fmt::Write;
                let _ = write!(buf, "\\u{:04x}", c as u32);
            }
            c => buf.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{UtilContext, VecOutput};
    use wasmsh_fs::{MemoryFs, OpenOptions, Vfs};

    fn make_tree_fs() -> MemoryFs {
        let mut fs = MemoryFs::new();
        fs.create_dir("/project").unwrap();
        fs.create_dir("/project/src").unwrap();
        let h = fs
            .open("/project/src/main.rs", OpenOptions::write())
            .unwrap();
        fs.write_file(h, b"fn main() {}").unwrap();
        fs.close(h);
        let h = fs
            .open("/project/src/lib.rs", OpenOptions::write())
            .unwrap();
        fs.write_file(h, b"").unwrap();
        fs.close(h);
        fs.create_dir("/project/tests").unwrap();
        let h = fs
            .open("/project/tests/test_main.rs", OpenOptions::write())
            .unwrap();
        fs.write_file(h, b"").unwrap();
        fs.close(h);
        let h = fs
            .open("/project/Cargo.toml", OpenOptions::write())
            .unwrap();
        fs.write_file(h, b"").unwrap();
        fs.close(h);
        fs
    }

    fn run_tree(argv: &[&str], fs: &mut MemoryFs, cwd: &str) -> (i32, String) {
        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs,
                output: &mut output,
                cwd,
                stdin: None,
                state: None,
            };
            util_tree(&mut ctx, argv)
        };
        (status, output.stdout_str().to_string())
    }

    #[test]
    fn tree_basic() {
        let mut fs = make_tree_fs();
        let (status, out) = run_tree(&["tree"], &mut fs, "/project");
        assert_eq!(status, 0);
        assert!(out.starts_with(".\n"));
        assert!(out.contains("src"));
        assert!(out.contains("main.rs"));
        assert!(out.contains("Cargo.toml"));
        assert!(out.contains("2 directories"));
        assert!(out.contains("4 files"));
    }

    #[test]
    fn tree_max_depth() {
        let mut fs = make_tree_fs();
        let (status, out) = run_tree(&["tree", "-L", "1"], &mut fs, "/project");
        assert_eq!(status, 0);
        assert!(out.contains("src"));
        // Should not descend into src/
        assert!(!out.contains("main.rs"));
    }

    #[test]
    fn tree_dirs_only() {
        let mut fs = make_tree_fs();
        let (status, out) = run_tree(&["tree", "-d"], &mut fs, "/project");
        assert_eq!(status, 0);
        assert!(out.contains("src"));
        assert!(out.contains("tests"));
        assert!(!out.contains("main.rs"));
        assert!(!out.contains("Cargo.toml"));
    }

    #[test]
    fn tree_noreport() {
        let mut fs = make_tree_fs();
        let (status, out) = run_tree(&["tree", "--noreport"], &mut fs, "/project");
        assert_eq!(status, 0);
        assert!(!out.contains("directories"));
        assert!(!out.contains("files"));
    }

    #[test]
    fn tree_hidden_files() {
        let mut fs = MemoryFs::new();
        fs.create_dir("/home").unwrap();
        let h = fs.open("/home/.hidden", OpenOptions::write()).unwrap();
        fs.write_file(h, b"").unwrap();
        fs.close(h);
        let h = fs.open("/home/visible", OpenOptions::write()).unwrap();
        fs.write_file(h, b"").unwrap();
        fs.close(h);

        // Without -a, hidden files should not appear
        let (_, out) = run_tree(&["tree"], &mut fs, "/home");
        assert!(!out.contains(".hidden"));
        assert!(out.contains("visible"));

        // With -a, hidden files should appear
        let (_, out) = run_tree(&["tree", "-a"], &mut fs, "/home");
        assert!(out.contains(".hidden"));
        assert!(out.contains("visible"));
    }

    #[test]
    fn tree_exclude_pattern() {
        let mut fs = make_tree_fs();
        let (status, out) = run_tree(&["tree", "-I", "*.rs"], &mut fs, "/project");
        assert_eq!(status, 0);
        assert!(!out.contains("main.rs"));
        assert!(!out.contains("lib.rs"));
        assert!(out.contains("Cargo.toml"));
    }

    #[test]
    fn tree_full_path() {
        let mut fs = make_tree_fs();
        let (_, out) = run_tree(&["tree", "-f"], &mut fs, "/project");
        assert!(out.contains("./src/main.rs") || out.contains("./src/lib.rs"));
    }

    #[test]
    fn tree_json() {
        let mut fs = MemoryFs::new();
        fs.create_dir("/d").unwrap();
        let h = fs.open("/d/a.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"hi").unwrap();
        fs.close(h);

        let (status, out) = run_tree(&["tree", "-J"], &mut fs, "/d");
        assert_eq!(status, 0);
        assert!(out.contains("\"name\": \".\""));
        assert!(out.contains("\"type\": \"directory\""));
        assert!(out.contains("\"name\": \"a.txt\""));
        assert!(out.contains("\"type\": \"file\""));
    }

    #[test]
    fn tree_box_drawing() {
        let mut fs = MemoryFs::new();
        fs.create_dir("/root").unwrap();
        let h = fs.open("/root/a.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"").unwrap();
        fs.close(h);
        let h = fs.open("/root/b.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"").unwrap();
        fs.close(h);

        let (_, out) = run_tree(&["tree"], &mut fs, "/root");
        // First entry uses ├──, last uses └──
        assert!(out.contains("\u{251c}\u{2500}\u{2500} a.txt"));
        assert!(out.contains("\u{2514}\u{2500}\u{2500} b.txt"));
    }

    #[test]
    fn tree_not_a_dir() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/file.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"data").unwrap();
        fs.close(h);

        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut output,
                cwd: "/",
                stdin: None,
                state: None,
            };
            util_tree(&mut ctx, &["tree", "/file.txt"])
        };
        assert_eq!(status, 1);
    }

    #[test]
    fn tree_single_dir_singular() {
        let mut fs = MemoryFs::new();
        fs.create_dir("/root").unwrap();
        fs.create_dir("/root/sub").unwrap();
        let h = fs.open("/root/sub/f.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"").unwrap();
        fs.close(h);

        let (_, out) = run_tree(&["tree"], &mut fs, "/root");
        assert!(out.contains("1 directory, 1 file"));
    }
}
