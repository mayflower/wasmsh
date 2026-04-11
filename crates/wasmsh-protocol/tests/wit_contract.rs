use std::collections::BTreeSet;
use std::path::Path;

use wit_parser::{FunctionKind, Resolve, Type, TypeDefKind, WorldItem};

fn load_wit_resolve() -> Resolve {
    let mut resolve = Resolve::default();
    resolve
        .push_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("wit"))
        .expect("experimental WIT contract should parse");
    resolve
}

fn worker_interface<'a>(resolve: &'a Resolve) -> &'a wit_parser::Interface {
    let world = resolve
        .worlds
        .iter()
        .find_map(|(_, world)| (world.name == "worker-protocol").then_some(world))
        .expect("worker-protocol world should exist");

    let interface_id = world
        .exports
        .values()
        .find_map(|item| match item {
            WorldItem::Interface { id, .. }
                if resolve.interfaces[*id].name.as_deref() == Some("worker") =>
            {
                Some(*id)
            }
            _ => None,
        })
        .expect("worker-protocol world should export the worker interface");

    &resolve.interfaces[interface_id]
}

fn named_type<'a>(
    resolve: &'a Resolve,
    interface: &'a wit_parser::Interface,
    name: &str,
) -> &'a wit_parser::TypeDef {
    let type_id = interface
        .types
        .get(name)
        .copied()
        .unwrap_or_else(|| panic!("worker interface should define {name}"));
    &resolve.types[type_id]
}

fn expect_list_of_named_type(resolve: &Resolve, ty: Type, expected: &str) {
    let list_id = match ty {
        Type::Id(id) => id,
        other => panic!("expected list<{expected}>, found {other:?}"),
    };

    let list_inner = match &resolve.types[list_id].kind {
        TypeDefKind::List(inner) => *inner,
        other => panic!("expected list<{expected}>, found {other:?}"),
    };

    let inner_id = match list_inner {
        Type::Id(id) => id,
        other => panic!("expected list<{expected}>, found list<{other:?}>"),
    };

    assert_eq!(resolve.types[inner_id].name.as_deref(), Some(expected));
}

fn expect_list_of_u8(resolve: &Resolve, ty: Type) {
    let list_id = match ty {
        Type::Id(id) => id,
        other => panic!("expected list<u8>, found {other:?}"),
    };

    match &resolve.types[list_id].kind {
        TypeDefKind::List(Type::U8) => {}
        other => panic!("expected list<u8>, found {other:?}"),
    }
}

#[test]
fn experimental_wit_world_parses_and_exports_worker_interface() {
    let resolve = load_wit_resolve();
    let interface = worker_interface(&resolve);

    assert_eq!(interface.name.as_deref(), Some("worker"));

    let expected_functions = BTreeSet::from([
        "cancel",
        "init",
        "list-dir",
        "mount",
        "poll-run",
        "read-file",
        "run",
        "start-run",
        "write-file",
    ]);
    let actual_functions = interface
        .functions
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(actual_functions, expected_functions);
}

#[test]
fn experimental_wit_event_and_diagnostic_types_cover_protocol_surface() {
    let resolve = load_wit_resolve();
    let interface = worker_interface(&resolve);

    let diagnostic_level = named_type(&resolve, interface, "diagnostic-level");
    let levels = match &diagnostic_level.kind {
        TypeDefKind::Enum(enum_) => enum_
            .cases
            .iter()
            .map(|case| case.name.as_str())
            .collect::<Vec<_>>(),
        other => panic!("diagnostic-level should be an enum, found {other:?}"),
    };
    assert_eq!(levels, vec!["info", "warning", "error", "trace"]);

    let worker_event = named_type(&resolve, interface, "worker-event");
    let cases = match &worker_event.kind {
        TypeDefKind::Variant(variant) => variant
            .cases
            .iter()
            .map(|case| (case.name.as_str(), case.ty))
            .collect::<Vec<_>>(),
        other => panic!("worker-event should be a variant, found {other:?}"),
    };
    let case_names = cases.iter().map(|(name, _)| *name).collect::<Vec<_>>();
    assert_eq!(
        case_names,
        vec![
            "stdout",
            "stderr",
            "exit",
            "yielded",
            "diagnostic",
            "fs-changed",
            "version",
        ]
    );

    let diagnostic = named_type(&resolve, interface, "diagnostic-event");
    let fields = match &diagnostic.kind {
        TypeDefKind::Record(record) => record
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        other => panic!("diagnostic-event should be a record, found {other:?}"),
    };
    assert_eq!(fields, vec!["level", "message"]);
}

#[test]
fn experimental_wit_functions_match_progressive_protocol_shape() {
    let resolve = load_wit_resolve();
    let interface = worker_interface(&resolve);

    let init = interface.functions.get("init").expect("init should exist");
    assert_eq!(init.kind, FunctionKind::Freestanding);
    assert_eq!(init.params.len(), 1);
    assert_eq!(init.params[0].0, "config");
    let config_id = match init.params[0].1 {
        Type::Id(id) => id,
        other => panic!("expected init-config param, found {other:?}"),
    };
    assert_eq!(
        resolve.types[config_id].name.as_deref(),
        Some("init-config")
    );
    expect_list_of_named_type(
        &resolve,
        init.result.expect("init should return events"),
        "worker-event",
    );

    let run = interface.functions.get("run").expect("run should exist");
    assert_eq!(run.params, vec![("input".into(), Type::String)]);
    expect_list_of_named_type(
        &resolve,
        run.result.expect("run should return events"),
        "worker-event",
    );

    let start_run = interface
        .functions
        .get("start-run")
        .expect("start-run should exist");
    assert_eq!(start_run.params, vec![("input".into(), Type::String)]);
    expect_list_of_named_type(
        &resolve,
        start_run.result.expect("start-run should return events"),
        "worker-event",
    );

    let poll_run = interface
        .functions
        .get("poll-run")
        .expect("poll-run should exist");
    assert!(poll_run.params.is_empty());
    expect_list_of_named_type(
        &resolve,
        poll_run.result.expect("poll-run should return events"),
        "worker-event",
    );

    let cancel = interface
        .functions
        .get("cancel")
        .expect("cancel should exist");
    assert!(cancel.params.is_empty());
    expect_list_of_named_type(
        &resolve,
        cancel.result.expect("cancel should return events"),
        "worker-event",
    );

    for name in ["mount", "read-file", "list-dir"] {
        let function = interface
            .functions
            .get(name)
            .unwrap_or_else(|| panic!("{name} should exist"));
        assert_eq!(function.params, vec![("path".into(), Type::String)]);
        expect_list_of_named_type(
            &resolve,
            function
                .result
                .expect("filesystem operations should return events"),
            "worker-event",
        );
    }

    let write_file = interface
        .functions
        .get("write-file")
        .expect("write-file should exist");
    assert_eq!(write_file.params[0], ("path".into(), Type::String));
    expect_list_of_u8(&resolve, write_file.params[1].1);
    expect_list_of_named_type(
        &resolve,
        write_file.result.expect("write-file should return events"),
        "worker-event",
    );
}
