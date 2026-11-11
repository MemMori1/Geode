//! BinSpec AST → 字节码编译器集成测试。

use geode_bspec::bytecode;
use geode_bspec::compile::Compiler;
use geode_bspec::node::Node;
use geode_bspec::parser::parse_source;
use geode_bspec::value::Value;
use geode_bspec::vm::{RunOutput, Vm};
use geode_core::error_codes;

fn parse(source: &str) -> geode_bspec::ast::Program {
    parse_source(source).unwrap_or_else(|diags| panic!("parse failed: {diags:?}"))
}

fn compile_ok(source: &str, entry: &str) -> bytecode::Program {
    let program = parse(source);
    Compiler::compile(&program, entry).unwrap_or_else(|diags| panic!("compile failed: {diags:?}"))
}

fn run(source: &str, entry: &str, data: &[u8]) -> RunOutput {
    let program = compile_ok(source, entry);
    let mut vm = Vm::new(&program, data);
    vm.run()
        .unwrap_or_else(|error| panic!("vm failed: {error:?}"))
}

fn child<'a>(node: &'a Node, name: &str) -> &'a Node {
    node.children
        .iter()
        .find(|child| child.name == name)
        .unwrap_or_else(|| panic!("missing child {name}; got {:?}", names(node)))
}

fn names(node: &Node) -> Vec<&str> {
    node.children
        .iter()
        .map(|child| child.name.as_str())
        .collect()
}

fn compile_error(source: &str, entry: &str) -> Vec<geode_core::Diag> {
    let program = parse(source);
    Compiler::compile(&program, entry).expect_err("expected compile error")
}

#[test]
fn minimal_fixed_struct_runs_in_vm() {
    let source = r#"
        struct Header : 8 {
            u32 magic;
            u16 version;
            u16 flags;
        }
    "#;
    let data = [0x50, 0x4b, 0x03, 0x04, 20, 0, 1, 0];
    let output = run(source, "Header", &data);
    assert!(
        output.diags.is_empty(),
        "unexpected diags: {:?}",
        output.diags
    );
    assert_eq!(output.root.children.len(), 1);

    let header = &output.root.children[0];
    assert_eq!(header.name, "Header");
    assert_eq!(header.type_name, "Header");
    assert_eq!(header.offset, 0);
    assert_eq!(header.size, 8);
    assert_eq!(names(header), ["magic", "version", "flags"]);

    let magic = child(header, "magic");
    assert_eq!(
        (magic.offset, magic.size, magic.type_name.as_str()),
        (0, 4, "u32")
    );
    assert_eq!(magic.value, Some(Value::UInt(0x0403_4b50)));

    let version = child(header, "version");
    assert_eq!((version.offset, version.size), (4, 2));
    assert_eq!(version.value, Some(Value::UInt(20)));

    let flags = child(header, "flags");
    assert_eq!((flags.offset, flags.size), (6, 2));
    assert_eq!(flags.value, Some(Value::UInt(1)));
}

#[test]
fn nested_struct_is_expanded_with_offsets() {
    let source = r#"
        struct Inner : 4 {
            u16 a;
            u16 b;
        }
        struct Outer : 8 {
            u32 prefix;
            Inner inner;
        }
    "#;
    let data = [0, 0, 0, 1, 2, 0, 3, 0];
    let output = run(source, "Outer", &data);
    let outer = &output.root.children[0];
    assert_eq!(outer.size, 8);

    let inner = child(outer, "inner");
    assert_eq!((inner.offset, inner.size), (4, 4));
    assert_eq!(names(inner), ["a", "b"]);
    assert_eq!(child(inner, "a").value, Some(Value::UInt(2)));
    assert_eq!(child(inner, "b").value, Some(Value::UInt(3)));
    assert_eq!(child(inner, "b").offset, 6);
}

#[test]
fn fixed_array_is_loaded_as_bytes() {
    let source = r#"
        struct Bytes : 4 {
            u8 data[4];
        }
    "#;
    let output = run(source, "Bytes", &[1, 2, 3, 4]);
    let data = child(&output.root.children[0], "data");
    assert_eq!(data.size, 4);
    assert_eq!(data.offset, 0);
    assert_eq!(data.type_name, "u8[4]");
    assert_eq!(data.value, Some(Value::Bytes(vec![1, 2, 3, 4])));
}

#[test]
fn at_offset_seeks_before_field() {
    let source = r#"
        struct Located : 0x14 {
            u32 first;
            u32 second @ 0x10;
        }
    "#;
    let mut data = vec![0u8; 0x14];
    data[0..4].copy_from_slice(&7u32.to_le_bytes());
    data[0x10..0x14].copy_from_slice(&9u32.to_le_bytes());
    let output = run(source, "Located", &data);
    let located = &output.root.children[0];
    assert_eq!(located.size, 0x14);
    let second = child(located, "second");
    assert_eq!(second.offset, 0x10);
    assert_eq!(second.size, 4);
    assert_eq!(second.value, Some(Value::UInt(9)));
}

#[test]
fn conditional_field_true_and_false() {
    let source = r#"
        struct Conditional {
            u8 marker;
            u32 value if 1;
        }
    "#;
    let with_value = run(source, "Conditional", &[1, 1, 0, 0, 0]);
    let node = &with_value.root.children[0];
    assert_eq!(node.size, 5);
    let value = child(node, "value");
    assert_eq!(value.offset, 1);
    assert_eq!(value.size, 4);
    assert_eq!(value.value, Some(Value::UInt(1)));

    let false_source = r#"
        struct Conditional {
            u8 marker;
            u32 value if 0;
        }
    "#;
    let without_value = run(false_source, "Conditional", &[0]);
    let node = &without_value.root.children[0];
    assert_eq!(node.size, 1);
    assert!(node.children.iter().all(|child| child.name != "value"));
}

#[test]
fn endian_pragma_and_field_prefix_precedence() {
    let source = r#"
        #pragma endian = big;
        struct EndianMix {
            u16 pragma_default;
            le u16 prefix_le;
            #[endian(big)] u16 attr_big;
        }
    "#;
    let data = [0x01, 0x02, 0x04, 0x03, 0x05, 0x06];
    let output = run(source, "EndianMix", &data);
    let node = &output.root.children[0];
    assert_eq!(
        child(node, "pragma_default").value,
        Some(Value::UInt(0x0102))
    );
    assert_eq!(child(node, "prefix_le").value, Some(Value::UInt(0x0304)));
    assert_eq!(child(node, "attr_big").value, Some(Value::UInt(0x0506)));
}

#[test]
fn string_with_encoding_and_terminator() {
    let source = r#"
        struct Name {
            str(enc="utf8", term=0) value;
        }
    "#;
    let output = run(source, "Name", &[b'a', b'b', b'c', 0]);
    let node = &output.root.children[0];
    assert_eq!(node.size, 4);
    let value = child(node, "value");
    assert_eq!(value.size, 4);
    assert_eq!(value.type_name, "str");
    assert_eq!(value.value, Some(Value::Str("abc".to_owned())));
}

#[test]
fn missing_entry_reports_e4004_with_range() {
    let errors = compile_error("struct S { u8 a; }", "Missing");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].code, error_codes::E4004);
    assert!(errors[0].range.is_some(), "Diag 必须带 ByteRange");
}

#[test]
fn non_constant_at_offset_reports_e4001() {
    let errors = compile_error("struct S { u32 x @ $size; }", "S");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].code, error_codes::E4001);
    assert!(
        errors[0].detail.contains("编译期常量"),
        "{}",
        errors[0].detail
    );
}

#[test]
fn unknown_type_reports_e4003() {
    let errors = compile_error("struct S { Missing x; }", "S");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].code, error_codes::E4003);
    assert!(errors[0].detail.contains("Missing"), "{}", errors[0].detail);
}

#[test]
fn recursive_struct_reports_e4007() {
    let errors = compile_error("struct A { A inner; }", "A");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].code, error_codes::E4007);
    assert!(errors[0].detail.contains("64"), "{}", errors[0].detail);
}

#[test]
fn zip_local_file_header_end_to_end() {
    let source = include_str!("fixtures/zip.bspec");
    let mut data = vec![0u8; 30];
    data[0..4].copy_from_slice(b"PK\x03\x04");
    data[4..6].copy_from_slice(&20u16.to_le_bytes());
    data[6..8].copy_from_slice(&0x0800u16.to_le_bytes());
    data[26..28].copy_from_slice(&5u16.to_le_bytes());
    data[28..30].copy_from_slice(&0u16.to_le_bytes());

    let output = run(source, "ZipLocalFileHeader", &data);
    assert!(
        output.diags.is_empty(),
        "unexpected diags: {:?}",
        output.diags
    );
    let header = &output.root.children[0];
    assert_eq!(header.size, 30);
    assert_eq!(
        child(header, "signature").value,
        Some(Value::UInt(0x0403_4b50))
    );
    assert_eq!(child(header, "version_needed").value, Some(Value::UInt(20)));
    assert_eq!(child(header, "file_name_len").value, Some(Value::UInt(5)));
}

#[test]
fn union_members_all_read_from_same_offset() {
    let source = r#"
        union Number : 4 {
            u32 as_u32;
            u16 low_word;
        }
    "#;
    let output = run(source, "Number", &[0x78, 0x56, 0x34, 0x12]);
    let union = &output.root.children[0];
    assert_eq!(union.size, 4);
    assert_eq!(child(union, "as_u32").offset, 0);
    assert_eq!(child(union, "as_u32").value, Some(Value::UInt(0x1234_5678)));
    assert_eq!(child(union, "low_word").offset, 0);
    assert_eq!(child(union, "low_word").size, 2);
    assert_eq!(child(union, "low_word").value, Some(Value::UInt(0x5678)));
}

#[test]
fn enum_and_bitfield_emit_whole_repr_value() {
    let source = r#"
        enum Kind : u8 { A = 1, B = 2 }
        bitfield Flags : u8 { low: 1; high: 2; _: 5; }
        struct Tagged : 2 {
            Kind kind;
            Flags flags;
        }
    "#;
    let output = run(source, "Tagged", &[2, 5]);
    let node = &output.root.children[0];
    assert_eq!(child(node, "kind").value, Some(Value::UInt(2)));
    assert_eq!(child(node, "kind").type_name, "Kind");
    assert_eq!(child(node, "flags").value, Some(Value::UInt(5)));
    assert_eq!(child(node, "flags").type_name, "Flags");
}

#[test]
fn for_loop_is_statically_unrolled() {
    let source = r#"
        fn count_three() {
            for (let i = 0; i < 3; i = i + 1) {
                u8 item;
            }
        }
    "#;
    let output = run(source, "count_three", &[10, 20, 30]);
    assert_eq!(output.root.children.len(), 3);
    assert_eq!(output.root.children[0].offset, 0);
    assert_eq!(output.root.children[2].offset, 2);
    assert_eq!(output.root.children[2].value, Some(Value::UInt(30)));
}

#[test]
fn while_loop_with_variable_condition_is_unrolled() {
    let source = r#"
        fn two_items() {
            let i = 0;
            while (i < 2) {
                u8 item;
                i = i + 1;
            }
        }
    "#;
    let output = run(source, "two_items", &[7, 8]);
    assert_eq!(output.root.children.len(), 2);
    assert_eq!(output.root.children[1].value, Some(Value::UInt(8)));
}

#[test]
fn until_array_stops_at_terminator() {
    let source = r#"
        struct UntilBytes {
            u8 data[until(0)];
        }
    "#;
    let output = run(source, "UntilBytes", &[1, 2, 0, 9]);
    let node = &output.root.children[0];
    assert_eq!(node.size, 3);
    assert_eq!(node.children.len(), 2);
    assert_eq!(node.children[1].value, Some(Value::UInt(2)));
}

#[test]
fn unsized_array_reads_to_declared_struct_end() {
    let source = r#"
        struct Tail : 4 {
            u8 data[];
        }
    "#;
    let output = run(source, "Tail", &[1, 2, 3, 4]);
    let data = child(&output.root.children[0], "data");
    assert_eq!(data.size, 4);
    assert_eq!(data.value, Some(Value::Bytes(vec![1, 2, 3, 4])));
}

#[test]
fn sizeof_and_len_fold_at_compile_time() {
    let source = r#"
        struct Word : 4 { u32 value; }
        fn use_sizeof() {
            let n = sizeof(Word);
            u8 bytes[n];
            u8 label[len("abc")];
        }
    "#;
    let output = run(source, "use_sizeof", &[1, 2, 3, 4, b'x', b'y', b'z']);
    assert_eq!(child(&output.root, "bytes").size, 4);
    assert_eq!(child(&output.root, "label").size, 3);
}
