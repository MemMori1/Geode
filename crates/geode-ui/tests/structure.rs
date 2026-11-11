//! 结构树模型的测试：投影、展开、反查。

use geode_bspec::node::Node;
use geode_bspec::value::Value;
use geode_ui::structure::{StructureTree, value_text};

fn sample() -> Node {
    let mut header = Node::branch("header", "ZipHeader", 0, 30);
    header.push_child(Node::leaf(
        "signature",
        "u32",
        0,
        4,
        Some(Value::UInt(0x0403_4b50)),
    ));
    header.push_child(Node::leaf("version", "u16", 4, 2, Some(Value::UInt(20))));
    header.push_child(Node::leaf("method", "u16", 8, 2, Some(Value::UInt(8))));
    let mut tail = Node::branch("tail", "ZipTail", 30, 16);
    tail.push_child(Node::leaf(
        "crc",
        "u32",
        30,
        4,
        Some(Value::UInt(0xDEAD_BEEF)),
    ));
    let mut root = Node::branch("zip", "ZipFile", 0, 46);
    root.push_child(header);
    root.push_child(tail);
    root
}

#[test]
fn rows_project_expanded_state() {
    let mut t = StructureTree::new(sample());
    // 新建时只展开根
    let rows = t.rows();
    assert_eq!(rows.len(), 3, "根 + header + tail");
    assert_eq!(rows[0].depth, 0);
    assert_eq!(rows[1].depth, 1);
    assert!(rows[1].has_children);
    assert!(!rows[1].expanded);

    assert_eq!(t.toggle("zip.header"), Some(true));
    let rows = t.rows();
    assert_eq!(rows.len(), 6, "header 的 3 个字段出现");
    assert_eq!(rows[2].name, "signature");
    assert_eq!(rows[2].depth, 2);
    assert_eq!(rows[2].value.as_deref(), Some("67324752"));
}

#[test]
fn collapse_hides_subtree_only() {
    let mut t = StructureTree::new(sample());
    t.expand_all();
    assert_eq!(t.rows().len(), 7);
    t.collapse_all();
    let rows = t.rows();
    assert_eq!(rows.len(), 3, "collapse_all 保留根展开");
    assert!(rows[0].expanded);
}

#[test]
fn reveal_expands_ancestor_chain() {
    let mut t = StructureTree::new(sample());
    t.collapse_all();
    // 30 落在 tail.crc
    let path = t.reveal(30).expect("应当命中");
    assert_eq!(path, "zip.tail.crc");
    assert!(t.is_expanded("zip.tail"));
    let rows = t.rows();
    assert!(rows.iter().any(|r| r.path == "zip.tail.crc"));
}

#[test]
fn reveal_returns_none_outside() {
    let mut t = StructureTree::new(sample());
    assert!(t.reveal(46).is_none());
    assert!(t.reveal(1_000_000).is_none());
}

#[test]
fn reveal_prefers_deepest_node() {
    let mut t = StructureTree::new(sample());
    // offset 4 同时落在根(0..46)、header(0..30) 与 version(4..6)
    assert_eq!(t.reveal(4).as_deref(), Some("zip.header.version"));
}

#[test]
fn row_index_matches_visible_rows() {
    let mut t = StructureTree::new(sample());
    assert_eq!(t.row_index("zip.header"), Some(1));
    // 收起后不可见 ⇒ None
    t.set_expanded("zip.header", false);
    assert_eq!(t.row_index("zip.header.version"), None);
}

#[test]
fn hidden_fields_are_filtered_unless_asked() {
    let mut root = Node::branch("x", "X", 0, 4);
    let mut pad = Node::leaf("_pad", "u32", 0, 4, Some(Value::UInt(0)));
    pad.attrs.push(("hidden".to_string(), "true".to_string()));
    root.push_child(pad);
    let mut t = StructureTree::new(root.clone());
    assert!(!t.rows().iter().any(|r| r.name == "_pad"));
    t.set_show_hidden(true);
    assert!(t.rows().iter().any(|r| r.name == "_pad"));
}

#[test]
fn value_text_formats_without_lying() {
    assert_eq!(value_text(&Value::UInt(42)), "42");
    assert_eq!(value_text(&Value::Int(-7)), "-7");
    assert_eq!(value_text(&Value::Bool(true)), "true");
    assert_eq!(value_text(&Value::Str("a\nb".to_string())), "\"a\\nb\"");
    assert_eq!(value_text(&Value::Bytes(vec![0xDE, 0xAD])), "[DE AD]");
    assert_eq!(value_text(&Value::None), "—");
    let nan = value_text(&Value::Float(f64::NAN));
    assert_eq!(nan, "NaN");
}

#[test]
fn offset_text_handles_64_bit() {
    let mut root = Node::branch("r", "R", 0x1_0000_0002, 1);
    root.push_child(Node::leaf(
        "f",
        "u8",
        0x1_0000_0002,
        1,
        Some(Value::UInt(0)),
    ));
    let t = StructureTree::new(root);
    let rows = t.rows();
    assert_eq!(rows[0].offset_text(), "0x00000001_00000002");
}
