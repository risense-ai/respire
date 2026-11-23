use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
};
use syn::{
    Expr, Item, Pat, Type, parse_quote,
    visit_mut::{self, VisitMut},
};

pub type Fields = BTreeMap<String, BTreeSet<String>>;

pub fn optional_nullable(doc: &Value) -> Result<Fields, Box<dyn Error>> {
    let mut targets = Fields::new();
    for (name, schema) in doc["components"]["schemas"]
        .as_object()
        .ok_or("Missing component schemas")?
    {
        let Some(properties) = schema["properties"].as_object() else {
            continue;
        };
        for (field, property) in properties {
            let nullable = property["type"]
                .as_array()
                .is_some_and(|types| types.iter().any(|item| item == "null"))
                || property["oneOf"]
                    .as_array()
                    .is_some_and(|variants| variants.iter().any(|item| item["type"] == "null"));
            let required = schema["required"]
                .as_array()
                .is_some_and(|list| list.iter().any(|item| item == field.as_str()));
            if nullable && !required {
                targets
                    .entry(name.clone())
                    .or_default()
                    .insert(field.clone());
            }
        }
    }
    Ok(targets)
}

pub fn preserve_presence(file: &mut syn::File, mut targets: Fields) -> Result<(), Box<dyn Error>> {
    let Some(items) = file.items.iter_mut().find_map(|item| match item {
        Item::Mod(module) if module.ident == "types" => {
            module.content.as_mut().map(|(_, items)| items)
        }
        _ => None,
    }) else {
        return Err("Missing generated type module".into());
    };
    for item in items {
        let Item::Struct(item) = item else { continue };
        let Some(mut fields) = targets.remove(&item.ident.to_string()) else {
            continue;
        };
        for field in &mut item.fields {
            let Some(name) = field.ident.as_ref().map(ToString::to_string) else {
                continue;
            };
            if !fields.remove(&name) {
                continue;
            }
            let Type::Path(ty) = &field.ty else {
                return Err(format!("Unexpected generated shape for {name}").into());
            };
            if !ty
                .path
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "Option")
            {
                return Err(format!("Expected generated Option for {name}").into());
            }
            // Serde treats JSON null as None for a plain Option. Preserve the
            // distinction without re-declaring generated DTOs by hand.
            let inner = &field.ty;
            field.ty = parse_quote!(::std::option::Option<#inner>);
            field.attrs.push(
                parse_quote!(#[serde(default, deserialize_with = "crate::present_nullable")]),
            );
        }
        if !fields.is_empty() {
            return Err(format!("Ungenerated fields: {fields:?}").into());
        }
    }
    if !targets.is_empty() {
        return Err(format!("Ungenerated types: {:?}", targets.keys()).into());
    }
    Ok(())
}

pub struct SpecificStatusesFirst;
impl VisitMut for SpecificStatusesFirst {
    fn visit_expr_match_mut(&mut self, expr: &mut syn::ExprMatch) {
        if matches!(&*expr.expr, Expr::MethodCall(call) if call.method == "as_u16")
            && expr
                .arms
                .iter()
                .all(|arm| matches!(arm.pat, Pat::Lit(_) | Pat::Range(_) | Pat::Wild(_)))
        {
            expr.arms.sort_by_key(|arm| match arm.pat {
                Pat::Lit(_) => 0,
                Pat::Range(_) => 1,
                _ => 2,
            });
        }
        visit_mut::visit_expr_match_mut(self, expr);
    }
}
