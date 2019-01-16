#![recursion_limit = "512"]
#![deny(missing_docs)]

//! Crate for internal use by other graphql-client crates, for code generation.
//!
//! It is not meant to be used directly by users of the library.

#[macro_use]
extern crate failure;
extern crate graphql_parser;
extern crate heck;
extern crate itertools;
#[macro_use]
extern crate lazy_static;
extern crate proc_macro;
extern crate proc_macro2;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;
extern crate syn;
#[macro_use]
extern crate quote;

use proc_macro2::TokenStream;
use syn::Visibility;

mod codegen;
/// Deprecation-related code
pub mod deprecation;
mod introspection_response;
mod query;
/// Contains the [Schema] type and its implementation.
pub mod schema;

mod constants;
mod enums;
mod field_type;
mod fragments;
mod inputs;
mod interfaces;
mod objects;
mod operations;
mod scalars;
mod selection;
mod shared;
mod unions;
mod variables;

use heck::SnakeCase;

#[cfg(test)]
mod tests;
use proc_macro2::{Ident, Span};

type CacheMap<T> =
    ::std::sync::Mutex<::std::collections::hash_map::HashMap<::std::path::PathBuf, T>>;

lazy_static! {
    static ref SCHEMA_CACHE: CacheMap<String> = CacheMap::default();
    static ref QUERY_CACHE: CacheMap<(String, graphql_parser::query::Document)> =
        CacheMap::default();
}

/// Used to configure code generation.
#[derive(Clone)]
pub struct GraphQLClientDeriveOptions {
    /// Name of the operation we want to generate code for. If it does not match, we use all queries.
    pub operation_name: Option<String>,
    /// The name of implemention target struct.
    pub struct_name: Option<String>,
    /// The module that contains queries.
    pub module_name: Option<String>,
    /// Comma-separated list of additional traits we want to derive.
    pub additional_derives: Option<String>,
    /// The deprecation strategy to adopt.
    pub deprecation_strategy: Option<deprecation::DeprecationStrategy>,
    /// target module visibility.
    pub module_visibility: Visibility,
}

/// Generates the code for a Rust module given a query, a schema and options.
pub fn generate_module_token_stream(
    query_path: std::path::PathBuf,
    schema_path: &std::path::Path,
    options: Option<GraphQLClientDeriveOptions>,
) -> Result<TokenStream, failure::Error> {
    let options = options.unwrap();

    let module_visibility = options.module_visibility.clone();
    let response_derives = options.additional_derives.clone();

    // The user can determine what to do about deprecations.
    let deprecation_strategy = options.deprecation_strategy.clone().unwrap_or_default();

    // We need to qualify the query with the path to the crate it is part of
    let (query_string, query) = {
        let mut lock = QUERY_CACHE.lock().expect("query cache is poisoned");
        match lock.entry(query_path) {
            ::std::collections::hash_map::Entry::Occupied(o) => o.get().clone(),
            ::std::collections::hash_map::Entry::Vacant(v) => {
                let query_string = read_file(v.key())?;
                let query = graphql_parser::parse_query(&query_string)?;
                v.insert((query_string, query)).clone()
            }
        }
    };

    // Determine which operation we are generating code for. This will be used in operationName.
    let operations = if options.operation_name.is_some() {
        let op = codegen::select_operation(&query, &(options.operation_name.clone().unwrap()));
        if op.is_some() {
            vec![op.unwrap()]
        } else {
            codegen::all_operations(&query)
        }
    } else {
        codegen::all_operations(&query)
    };

    let schema_extension = schema_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("INVALID");

    // Check the schema cache.
    let schema_string: String = {
        let mut lock = SCHEMA_CACHE.lock().expect("schema cache is poisoned");
        match lock.entry(schema_path.to_path_buf()) {
            ::std::collections::hash_map::Entry::Occupied(o) => o.get().clone(),
            ::std::collections::hash_map::Entry::Vacant(v) => {
                let schema_string = read_file(v.key())?;
                v.insert(schema_string).to_string()
            }
        }
    };

    let parsed_schema = match schema_extension {
                        "graphql" | "gql" => {
                            let s = graphql_parser::schema::parse_schema(&schema_string)?;
                            schema::ParsedSchema::GraphQLParser(s)
                        }
                        "json" => {
                            let parsed: introspection_response::IntrospectionResponse = ::serde_json::from_str(&schema_string)?;
                            schema::ParsedSchema::Json(parsed)
                        }
                        extension => panic!("Unsupported extension for the GraphQL schema: {} (only .json and .graphql are supported)", extension)
                    };
    let schema = schema::Schema::from(&parsed_schema);

    let struct_name = if options.struct_name.is_some() {
        Some(Ident::new(
            options.struct_name.clone().unwrap().as_str(),
            Span::call_site(),
        ))
    } else {
        None
    };

    let module_name = Ident::new(
        options
            .module_name
            .as_ref()
            .unwrap_or_else(|| options.operation_name.as_ref().unwrap())
            .to_snake_case()
            .as_str(),
        Span::call_site(),
    );

    let operation_count = operations.len();

    let multiple_operations = operation_count > 1;

    let mut schema_and_operations = Vec::with_capacity(operation_count);

    for operation in &operations {
        let schema_output = codegen::response_for_query(
            &schema.clone(),
            &query.clone(),
            &operation,
            response_derives.clone(),
            deprecation_strategy.clone(),
            multiple_operations,
        )?;
        let operation_name = Ident::new(operation.name.as_str(), Span::call_site());
        schema_and_operations.push((schema_output, operation_name, operation.name.as_str()));
    }

    let result = build_module_token_stream(
        &module_visibility,
        &module_name,
        &struct_name,
        &query_string,
        schema_and_operations,
    );

    Ok(result)
}

fn build_module_token_stream(
    module_visibility: &syn::Visibility,
    module_name: &Ident,
    struct_name: &Option<Ident>,
    query_string: &str,
    schema_and_operations: Vec<(TokenStream, Ident, &str)>,
) -> TokenStream {
    let mut schema_token_streams = vec![];
    let mut trait_token_streams = vec![];
    let multiple_operations = schema_and_operations.len() > 1;
    for (schema_output, operation_name, operation_name_literal) in schema_and_operations {
        let (schema_token_stream, trait_token_stream) = build_query_struct_token_stream(
            &module_name,
            struct_name.clone(),
            &schema_output,
            &operation_name,
            operation_name_literal,
            multiple_operations,
        );
        schema_token_streams.push(schema_token_stream);
        trait_token_streams.push(trait_token_stream);
    }

    merge_with_common_token_stream(
        &module_visibility,
        &module_name,
        query_string,
        schema_token_streams,
        trait_token_streams,
    )
}

fn merge_with_common_token_stream(
    module_visibility: &syn::Visibility,
    module_name: &Ident,
    query_string: &str,
    schema_token_streams: Vec<TokenStream>,
    trait_token_streams: Vec<TokenStream>,
) -> TokenStream {
    quote!(
        #module_visibility mod #module_name {
            #![allow(non_camel_case_types)]
            #![allow(non_snake_case)]
            #![allow(dead_code)]

            use serde;

            pub const QUERY: &'static str = #query_string;
            #(#schema_token_streams)*
        }
        #(#trait_token_streams)*
    )
}

fn build_query_struct_token_stream(
    module_name: &Ident,
    struct_name: Option<Ident>,
    schema_output: &TokenStream,
    operation_name: &Ident,
    operation_name_literal: &str,
    multiple_operations: bool,
) -> (TokenStream, TokenStream) {
    let struct_name = if struct_name.is_some() {
        struct_name.unwrap()
    } else {
        operation_name.clone()
    };

    let (respons_data_struct_name, variables_struct_name) = if multiple_operations {
        (
            Ident::new(
                format!("{}ResponseData", operation_name_literal).as_str(),
                Span::call_site(),
            ),
            Ident::new(
                format!("{}Variables", operation_name).as_str(),
                Span::call_site(),
            ),
        )
    } else {
        (
            Ident::new("ResponseData", Span::call_site()),
            Ident::new("Variables", Span::call_site()),
        )
    };

    let schema_token = quote!(
        pub const OPERATION_NAME: &'static str = #operation_name_literal;
        #schema_output
    );
    let trait_token = quote!(
        impl ::graphql_client::GraphQLQuery for #struct_name {
            type Variables = #module_name::#variables_struct_name;
            type ResponseData = #module_name::#respons_data_struct_name;

            fn build_query(variables: Self::Variables) -> ::graphql_client::QueryBody<Self::Variables> {
                ::graphql_client::QueryBody {
                    variables,
                    query: #module_name::QUERY,
                    operation_name: #module_name::OPERATION_NAME,
                }

            }
        }
    );
    (schema_token, trait_token)
}

fn read_file(path: &::std::path::Path) -> Result<String, failure::Error> {
    use std::io::prelude::*;

    let mut out = String::new();
    let mut file = ::std::fs::File::open(path).map_err(|io_err| {
        let err: failure::Error = io_err.into();
        err.context(format!(
            r#"
            Could not find file with path: {}
            Hint: file paths in the GraphQLQuery attribute are relative to the project root (location of the Cargo.toml). Example: query_path = "src/my_query.graphql".
            "#,
            path.display()
        ))
    })?;
    file.read_to_string(&mut out)?;
    Ok(out)
}
