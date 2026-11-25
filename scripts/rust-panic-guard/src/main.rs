//! Syntax-only panic-call policy; test scope must be proved, never guessed by path.
use proc_macro2::{Delimiter, Span, TokenStream, TokenTree};
use quote::ToTokens;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, Read};
use syn::ext::IdentExt;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{Attribute, Expr, ForeignItem, ImplItem, Item, Meta, Token, TraitItem};

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Site(usize, usize, String, u64);

#[derive(Default)]
struct Scanner(Vec<Site>);

fn nested(meta: &Meta) -> Option<Punctuated<Meta, Token![,]>> {
    let Meta::List(list) = meta else { return None };
    Punctuated::<Meta, Token![,]>::parse_terminated
        .parse2(list.tokens.clone())
        .ok()
}

// Constant propagation under test=false. Unknown features and target atoms
// do not warrant an exemption: cfg(any(test, windows)) remains production.
fn prod(meta: &Meta) -> Option<bool> {
    if matches!(meta, Meta::Path(path) if path.is_ident("test")) {
        return Some(false);
    }
    let args = nested(meta)?;
    if meta.path().is_ident("not") && args.len() == 1 {
        return prod(args.first()?).map(|bit| !bit);
    }
    if !meta.path().is_ident("any") && !meta.path().is_ident("all") {
        return None;
    }
    let deciding = meta.path().is_ident("any");
    let mut unknown = false;
    for arg in &args {
        match prod(arg) {
            Some(bit) if bit == deciding => return Some(deciding),
            None => unknown = true,
            _ => {}
        }
    }
    if unknown {
        None
    } else {
        Some(!deciding)
    }
}

fn excludes_prod(meta: &Meta) -> bool {
    if meta.path().is_ident("cfg") {
        return nested(meta)
            .is_some_and(|args| args.len() == 1 && args.first().and_then(prod) == Some(false));
    }
    if meta.path().is_ident("cfg_attr") {
        return nested(meta).is_some_and(|args| {
            let mut it = args.iter();
            it.next().and_then(prod) == Some(true) && it.any(excludes_prod)
        });
    }
    false
}

fn test_only(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| excludes_prod(&attr.meta))
}

fn panic_name(id: &syn::Ident) -> Option<String> {
    let name = id.unraw().to_string();
    matches!(name.as_str(), "unwrap" | "expect").then_some(name)
}

macro_rules! attributes {
    ($node:expr, $($variant:path),* $(,)?) => { match $node {
        $( $variant(item) => item.attrs.as_slice(), )*
        _ => &[],
    }};
}

impl Scanner {
    fn mark(&mut self, span: Span, name: String, kind: &str, context: impl Hash) {
        let point = span.start();
        let mut hash = DefaultHasher::new();
        kind.hash(&mut hash);
        context.hash(&mut hash);
        self.0
            .push(Site(point.line, point.column, name, hash.finish()));
    }

    // Macro source is tokenized, not regex-scanned. Rust strings and comments
    // cannot look like a call here. Metavariable/expansion analysis is not done.
    fn tokens(&mut self, stream: TokenStream) {
        let tokens: Vec<_> = stream.into_iter().collect();
        let punct = |n, p| matches!(tokens.get(n), Some(TokenTree::Punct(x)) if x.as_char() == p);
        for (i, token) in tokens.iter().enumerate() {
            if let TokenTree::Group(group) = token {
                self.tokens(group.stream());
            }
            let TokenTree::Ident(id) = token else {
                continue;
            };
            let Some(name) = panic_name(id) else { continue };
            let invoke = matches!(tokens.get(i + 1), Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis)
                || (punct(i + 1, ':') && punct(i + 2, ':') && punct(i + 3, '<'));
            if i >= 1 && punct(i - 1, '.') && invoke {
                self.mark(id.span(), name, "macro-method", "");
            } else if i >= 2 && punct(i - 1, ':') && punct(i - 2, ':') {
                self.mark(id.span(), name, "macro-qualified", "");
            }
        }
    }
}

impl<'ast> Visit<'ast> for Scanner {
    fn visit_file(&mut self, file: &'ast syn::File) {
        if !test_only(&file.attrs) {
            visit::visit_file(self, file);
        }
    }
    fn visit_item(&mut self, node: &'ast Item) {
        let attrs = attributes!(
            node,
            Item::Const,
            Item::Enum,
            Item::ExternCrate,
            Item::Fn,
            Item::ForeignMod,
            Item::Impl,
            Item::Macro,
            Item::Mod,
            Item::Static,
            Item::Struct,
            Item::Trait,
            Item::TraitAlias,
            Item::Type,
            Item::Union,
            Item::Use
        );
        if test_only(attrs) {
            return;
        }
        if let Item::Verbatim(tokens) = node {
            self.tokens(tokens.clone());
        }
        visit::visit_item(self, node);
    }
    fn visit_impl_item(&mut self, node: &'ast ImplItem) {
        if !test_only(attributes!(
            node,
            ImplItem::Const,
            ImplItem::Fn,
            ImplItem::Macro,
            ImplItem::Type
        )) {
            visit::visit_impl_item(self, node);
        }
    }
    fn visit_trait_item(&mut self, node: &'ast TraitItem) {
        if !test_only(attributes!(
            node,
            TraitItem::Const,
            TraitItem::Fn,
            TraitItem::Macro,
            TraitItem::Type
        )) {
            visit::visit_trait_item(self, node);
        }
    }
    fn visit_foreign_item(&mut self, node: &'ast ForeignItem) {
        if !test_only(attributes!(
            node,
            ForeignItem::Fn,
            ForeignItem::Macro,
            ForeignItem::Static,
            ForeignItem::Type
        )) {
            visit::visit_foreign_item(self, node);
        }
    }
    fn visit_expr(&mut self, node: &'ast Expr) {
        if test_only(attributes!(
            node,
            Expr::Array,
            Expr::Assign,
            Expr::Async,
            Expr::Await,
            Expr::Binary,
            Expr::Block,
            Expr::Break,
            Expr::Call,
            Expr::Cast,
            Expr::Closure,
            Expr::Const,
            Expr::Continue,
            Expr::Field,
            Expr::ForLoop,
            Expr::Group,
            Expr::If,
            Expr::Index,
            Expr::Infer,
            Expr::Let,
            Expr::Lit,
            Expr::Loop,
            Expr::Macro,
            Expr::Match,
            Expr::MethodCall,
            Expr::Paren,
            Expr::Path,
            Expr::Range,
            Expr::RawAddr,
            Expr::Reference,
            Expr::Repeat,
            Expr::Return,
            Expr::Struct,
            Expr::Try,
            Expr::TryBlock,
            Expr::Tuple,
            Expr::Unary,
            Expr::Unsafe,
            Expr::While,
            Expr::Yield
        )) {
            return;
        }
        if let Expr::Verbatim(tokens) = node {
            self.tokens(tokens.clone());
        }
        visit::visit_expr(self, node);
    }
    fn visit_arm(&mut self, node: &'ast syn::Arm) {
        if !test_only(&node.attrs) {
            visit::visit_arm(self, node);
        }
    }
    fn visit_local(&mut self, node: &'ast syn::Local) {
        if !test_only(&node.attrs) {
            visit::visit_local(self, node);
        }
    }
    fn visit_stmt_macro(&mut self, node: &'ast syn::StmtMacro) {
        if !test_only(&node.attrs) {
            visit::visit_stmt_macro(self, node);
        }
    }
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.tokens(node.tokens.clone());
    }
    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        if let Some(name) = panic_name(&node.method) {
            self.mark(
                node.method.span(),
                name,
                "method",
                node.receiver.to_token_stream().to_string(),
            );
        }
        visit::visit_expr_method_call(self, node);
    }
    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        // Qualified call or function value, including type aliases and UFCS.
        if node.qself.is_some() || node.path.segments.len() > 1 {
            if let Some(segment) = node.path.segments.last() {
                if let Some(name) = panic_name(&segment.ident) {
                    self.mark(
                        segment.ident.span(),
                        name,
                        "qualified",
                        node.to_token_stream().to_string(),
                    );
                }
            }
        }
        visit::visit_expr_path(self, node);
    }
}

fn inspect(source: &str) -> syn::Result<Vec<Site>> {
    let mut scanner = Scanner::default();
    scanner.visit_file(&syn::parse_file(source)?);
    scanner.0.sort();
    scanner.0.dedup();
    Ok(scanner.0)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut source = String::new();
    io::stdin().read_to_string(&mut source)?;
    for Site(line, column, method, shape) in inspect(&source)? {
        println!("{line}\t{column}\t{method}\t{shape:016x}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn names(source: &str) -> Vec<String> {
        inspect(source)
            .unwrap()
            .into_iter()
            .map(|site| site.2)
            .collect()
    }

    #[test]
    fn split_lines_comments_raw_strings_and_other_names() {
        let s = "fn f() {\n  let _ = value\n    . /* keep */ unwrap\n    ();\n  let _ = r#\".expect()\"#; // x.unwrap()\n  let _ = value.unwrap_or_default();\n}";
        let found = inspect(s).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, 3);
    }

    #[test]
    fn qualified_calls_and_function_values_include_type_aliases() {
        assert_eq!(names("use std::option::Option as Alias; fn f(){ Alias::unwrap(x); let cb = <Alias>::expect; }"),
            ["unwrap", "expect"]);
        assert_eq!(
            names("fn f(){ let cb = Result::unwrap; x.r#expect(\"x\"); }"),
            ["unwrap", "expect"]
        );
    }

    #[test]
    fn only_proof_of_nonproduction_disables_a_site() {
        assert!(names("#![cfg(test)] fn helper(){x.unwrap();}").is_empty());
        assert!(
            names("#[cfg(all(test, windows))] mod anything { fn helper(){x.unwrap();} }")
                .is_empty()
        );
        assert!(names(
            "#[cfg(not(not(test)))] fn f(){x.unwrap();} #[cfg(test)] #[test] fn t(){x.expect();}"
        )
        .is_empty());
        assert!(names("#[cfg_attr(not(test), cfg(test))] fn f(){x.unwrap();}").is_empty());
        assert_eq!(names("#[cfg(any(test, windows))] fn f(){x.unwrap();} #[cfg(not(test))] fn g(){x.expect();}"),
            ["unwrap", "expect"]);
        assert_eq!(names("mod tests { fn helper(){x.unwrap();} }"), ["unwrap"]);
    }

    #[test]
    fn scans_unexpanded_macro_tokens_without_looking_inside_literals() {
        assert_eq!(
            names("fn f(){ make!(x.unwrap(), \"x.expect()\", Option::expect); }"),
            ["unwrap", "expect"]
        );
        assert!(names("#[cfg(test)] fn f(){ make!(x.unwrap()); }").is_empty());
    }
}
