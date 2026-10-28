//! Recognize one explicit SQLx owned-transaction protocol, not a source-file exemption.

/// The lexical scanner still visits every token after this classification, including nested
/// deferred calls in the connection argument. A parse/boundary mismatch always fails closed.
pub(super) fn is_explicit_immediate(
    flat: &[(char, usize)],
    method_start: usize,
    open_paren: usize,
) -> bool {
    let Some(end) = call_end(flat, open_paren) else {
        return false;
    };
    let mut start = method_start;
    while start > 0 {
        let c = flat[start - 1].0;
        if c.is_alphanumeric() || matches!(c, '_' | ':' | ' ') || !c.is_ascii() {
            start -= 1;
        } else {
            break;
        }
    }
    // Do not mistake a suffix of a qualified/generic/member callee for a global SQLx path.
    if start > 0 && matches!(flat[start - 1].0, '>' | '.' | ')' | ']' | '#' | '\0') {
        return false;
    }
    let source: String = flat[start..end].iter().map(|(c, _)| *c).collect();
    let Ok(syn::Expr::Call(call)) = syn::parse_str::<syn::Expr>(&source) else {
        return false;
    };
    if !global_path(&call.func, &["sqlx", "Transaction", "begin"]) || call.args.len() != 2 {
        return false;
    }
    let Some(some) = call_arg(&call.args[1], &["std", "option", "Option", "Some"]) else {
        return false;
    };
    let Some(literal) = call_arg(some, &["std", "borrow", "Cow", "Borrowed"]) else {
        return false;
    };
    matches!(peel(literal), syn::Expr::Lit(syn::ExprLit {
        lit: syn::Lit::Str(value), ..
    }) if value.value() == "BEGIN IMMEDIATE" && value.suffix().is_empty())
}

pub(super) fn call_end(flat: &[(char, usize)], open: usize) -> Option<usize> {
    if flat.get(open)?.0 != '(' {
        return None;
    }
    let mut depth = 0usize;
    for (index, (c, _)) in flat.iter().enumerate().skip(open) {
        match c {
            '\0' => return None,
            '(' => depth += 1,
            ')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn peel(expr: &syn::Expr) -> &syn::Expr {
    match expr {
        syn::Expr::Paren(inner) => peel(&inner.expr),
        syn::Expr::Group(inner) => peel(&inner.expr),
        _ => expr,
    }
}

fn global_path(expr: &syn::Expr, names: &[&str]) -> bool {
    let syn::Expr::Path(path) = peel(expr) else {
        return false;
    };
    path.qself.is_none()
        && path.path.leading_colon.is_some()
        && path.path.segments.len() == names.len()
        && path.path.segments.iter().zip(names).all(|(segment, name)| {
            segment.ident == *name && matches!(segment.arguments, syn::PathArguments::None)
        })
}

fn call_arg<'a>(expr: &'a syn::Expr, path: &[&str]) -> Option<&'a syn::Expr> {
    let syn::Expr::Call(call) = peel(expr) else {
        return None;
    };
    (global_path(&call.func, path) && call.args.len() == 1).then(|| &call.args[0])
}
