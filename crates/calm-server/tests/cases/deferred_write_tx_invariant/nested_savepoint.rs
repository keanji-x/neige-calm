//! Explicit SQLx nested-savepoint source protocol.

#[test]
fn scanner_recognizes_explicit_sqlx_nested_savepoint() {
    let hits = super::scan_snippet(
        "async fn f() { <&mut ::sqlx::Transaction<'_, ::sqlx::Sqlite> as ::sqlx::Acquire>::begin(existing).await; }",
    );
    assert!(
        hits.is_empty(),
        "explicit SQLx Transaction receiver: {hits:?}"
    );
}
/// This is a compile-time receiver contract, not Rust type inference: only a global
/// SQLx Transaction Self and Acquire trait with one bare receiver are admitted.
/// In particular, dereferencing a transaction into a connection is not this protocol.
/// The caller continues scanning every token, including all argument expressions.
pub(super) fn is_explicit_savepoint(
    flat: &[(char, usize)],
    method_start: usize,
    open_paren: usize,
) -> bool {
    let Some(end) = super::owned_constructor::call_end(flat, open_paren) else {
        return false;
    };
    let prefix: String = flat[..method_start].iter().map(|(c, _)| *c).collect();
    let prefix = prefix.trim_end();
    let Some(prefix) = prefix.strip_suffix("::") else {
        return false;
    };
    let prefix = prefix.trim_end();
    if !prefix.ends_with('>') {
        return false;
    }
    let mut depth = 0usize;
    let mut start = None;
    for (index, c) in prefix.char_indices().rev() {
        match c {
            '\0' => return false,
            '>' => depth += 1,
            '<' => {
                depth -= 1;
                if depth == 0 {
                    start = Some(index);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(start) = start else { return false };
    if prefix[..start]
        .trim_end()
        .ends_with([':', '>', '.', ')', ']', '#', '\0'])
    {
        return false;
    }
    // `start` is a byte offset in prefix, whereas flat is indexed by chars.
    let start = prefix[..start].chars().count();
    let source: String = flat[start..end].iter().map(|(c, _)| *c).collect();
    let Ok(syn::Expr::Call(call)) = syn::parse_str::<syn::Expr>(&source) else {
        return false;
    };
    let syn::Expr::Path(callee) = &*call.func else {
        return false;
    };
    let Some(qself) = &callee.qself else {
        return false;
    };
    if qself.as_token.is_none()
        || qself.position != 2
        || !global_path(&callee.path, &["sqlx", "Acquire", "begin"])
        || call.args.len() != 1
    {
        return false;
    }
    let syn::Type::Reference(receiver) = &*qself.ty else {
        return false;
    };
    let syn::Type::Path(transaction) = &*receiver.elem else {
        return false;
    };
    if receiver.mutability.is_none() || transaction.qself.is_some() {
        return false;
    }
    let path = &transaction.path;
    if path.leading_colon.is_none()
        || path.segments.len() != 2
        || path.segments[0].ident != "sqlx"
        || !matches!(path.segments[0].arguments, syn::PathArguments::None)
        || path.segments[1].ident != "Transaction"
    {
        return false;
    }
    let syn::PathArguments::AngleBracketed(args) = &path.segments[1].arguments else {
        return false;
    };
    if args.colon2_token.is_some()
        || args.args.len() != 2
        || !matches!(args.args[0], syn::GenericArgument::Lifetime(_))
    {
        return false;
    }
    let syn::GenericArgument::Type(syn::Type::Path(database)) = &args.args[1] else {
        return false;
    };
    let syn::Expr::Path(argument) = &call.args[0] else {
        return false;
    };
    database.qself.is_none()
        && global_path(&database.path, &["sqlx", "Sqlite"])
        && argument.qself.is_none()
        && argument.path.get_ident().is_some()
}

fn global_path(path: &syn::Path, names: &[&str]) -> bool {
    path.leading_colon.is_some()
        && path.segments.len() == names.len()
        && path.segments.iter().zip(names).all(|(segment, name)| {
            segment.ident == *name && matches!(segment.arguments, syn::PathArguments::None)
        })
}

#[test]
fn scanner_savepoint_protocol_is_independent_of_receiver_name_and_layout() {
    for receiver in ["existing", "outer", "事务"] {
        let src = format!(
            "async fn f<'a>() {{\n < &'a mut ::sqlx::Transaction< 'a, ::sqlx::Sqlite >\n as ::sqlx::Acquire > :: begin(\n {receiver},\n ).await; }}"
        );
        assert!(super::scan_snippet(&src).is_empty(), "{src}");
    }
    // Local names cannot shadow either global SQLx path.
    let src = "struct Transaction; trait Acquire {}\nasync fn f() {\n <&mut ::sqlx::Transaction<'_, ::sqlx::Sqlite> as ::sqlx::Acquire>::begin(outer).await; }";
    assert!(super::scan_snippet(src).is_empty());
}

#[test]
fn scanner_rejects_unproven_savepoint_self_trait_and_arguments() {
    const SELF: &str = "&mut ::sqlx::Transaction<'_, ::sqlx::Sqlite>";
    for receiver_type in [
        "&mut Transaction<'_, Sqlite>",
        "&mut sqlx::Transaction<'_, ::sqlx::Sqlite>",
        "&mut ::evil::sqlx::Transaction<'_, ::sqlx::Sqlite>",
        "&mut ::sqlx::TransactionSuffix<'_, ::sqlx::Sqlite>",
        "&mut ::sqlx::Transaction<'_, Sqlite>",
        "&mut ::sqlx::Transaction<'_, sqlx::Sqlite>",
        "&mut ::sqlx::Transaction<'_, ::evil::sqlx::Sqlite>",
        "&mut ::sqlx::Transaction<'_, ::sqlx::SqliteSuffix>",
        "&mut ::sqlx::Transaction<'_, ::sqlx::Postgres>",
        "&mut ::sqlx::Transaction<'_, ::sqlx::Sqlite, Extra>",
        "&mut ::sqlx::Transaction<::sqlx::Sqlite>",
        "&mut ::sqlx::Transaction<'_, <T as Db>::Sqlite>",
        "&mut ::sqlx::Transaction<'_, ::sqlx::Sqlite<T>>",
        "&mut ::sqlx::Transaction<'_, Alias>",
        "&mut Alias<'_, ::sqlx::Sqlite>",
        "&mut ::sqlx::SqliteConnection",
        "&mut ::sqlx::SqlitePool",
        "&::sqlx::SqlitePool",
        "&mut Self",
        "Self",
        "::sqlx::Transaction<'_, ::sqlx::Sqlite>",
        "&::sqlx::Transaction<'_, ::sqlx::Sqlite>",
        "&mut <T as Wrapper>::Transaction<'_, ::sqlx::Sqlite>",
    ] {
        let src = format!("fn f() {{ <{receiver_type} as ::sqlx::Acquire>::begin(tx); }}");
        assert_eq!(super::scan_snippet(&src).len(), 1, "{src}");
    }
    for acquire in [
        "Acquire",
        "sqlx::Acquire",
        "::evil::sqlx::Acquire",
        "::sqlx::AcquireSuffix",
        "::sqlx::Connection",
        "::sqlx::Acquire<T>",
        "Alias",
    ] {
        let src = format!("fn f() {{ <{SELF} as {acquire}>::begin(tx); }}");
        assert_eq!(super::scan_snippet(&src).len(), 1, "{src}");
    }
    for argument in [
        "",
        "tx, extra",
        "&mut *tx",
        "&mut **tx",
        "*tx",
        "self.connection",
    ] {
        let src = format!("fn f() {{ <{SELF} as ::sqlx::Acquire>::begin({argument}); }}");
        assert_eq!(super::scan_snippet(&src).len(), 1, "{src}");
    }
    for src in [
        format!("fn f() {{ <{SELF}>::begin(tx); }}"),
        format!("fn f() {{ evil::<{SELF} as ::sqlx::Acquire>::begin(tx); }}"),
        format!("fn f() {{ <Q>::<{SELF} as ::sqlx::Acquire>::begin(tx); }}"),
        "struct Transaction; trait Acquire {}\nfn f() { <&mut Transaction as Acquire>::begin(tx); }".into(),
        "type Alias<'a> = ::sqlx::Transaction<'a, ::sqlx::Sqlite>;\nfn f() { <&mut Alias<'_> as ::sqlx::Acquire>::begin(tx); }".into(),
        "fn f() { tx.begin(); }".into(),
        "fn f() { pool.begin(); }".into(),
        "fn f() { connection.begin(); }".into(),
        "fn f() { ::sqlx::Acquire::begin(tx); }".into(),
    ] {
        assert_eq!(super::scan_snippet(&src).len(), 1, "{src}");
    }
}

#[test]
fn scanner_savepoint_classification_does_not_swallow_nested_root_begins() {
    const CALLEE: &str = "<&mut ::sqlx::Transaction<'_, ::sqlx::Sqlite> as ::sqlx::Acquire>::begin";
    // A valid savepoint inside another call must not hide a sibling deferred root.
    let src = format!("async fn f() {{ consume({CALLEE}(outer).await,\n pool.begin().await); }}");
    let hits = super::scan_snippet(&src);
    assert_eq!(hits, vec![(2, "deferred `.begin(`")], "{src}");
    // Even an invalid savepoint receiver must not swallow any inner calls.
    for argument in [
        "pool.begin().await",
        "{ pool.begin().await; tx }",
        "::sqlx::Acquire::begin(pool).await",
        "<&mut ::sqlx::SqliteConnection as ::sqlx::Acquire>::begin(connection).await",
    ] {
        let src = format!("async fn f() {{ {CALLEE}(\n {argument}); }}");
        let hits = super::scan_snippet(&src);
        assert_eq!(hits.len(), 2, "{src}: {hits:?}");
        assert_eq!(
            hits.iter().map(|(line, _)| *line).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }
    // Comments and quoted protocol text never authorize a method begin.
    let src =
        format!("fn f() {{ /* {CALLEE}(tx) */ let text = r#\"{CALLEE}(tx)\"#;\n tx.begin(); }}");
    assert_eq!(super::scan_snippet(&src), vec![(2, "deferred `.begin(`")]);
}
