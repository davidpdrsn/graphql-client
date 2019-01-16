use deprecation;
use failure;
use fragments::GqlFragment;
use graphql_parser::query;
use operations::Operation;
use proc_macro2::{Ident, Span, TokenStream};
use query::QueryContext;
use schema;
use selection::Selection;

/// Selects the first operation matching `struct_name` or the first one. Returns `None` when the query document defines no operation.
pub(crate) fn select_operation<'query>(
    query: &'query query::Document,
    struct_name: &str,
) -> Option<Operation<'query>> {
    let operations = all_operations(query);

    operations
        .iter()
        .find(|op| op.name == struct_name)
        .map(|i| i.to_owned())
        .or_else(|| operations.iter().next().map(|i| i.to_owned()))
}

pub(crate) fn all_operations(query: &query::Document) -> Vec<Operation> {
    let mut operations: Vec<Operation> = Vec::new();

    for definition in &query.definitions {
        if let query::Definition::Operation(op) = definition {
            operations.push(op.into());
        }
    }
    operations
}

/// The main code generation function.
pub(crate) fn response_for_query(
    schema: &schema::Schema,
    query: &query::Document,
    operation: &Operation,
    additional_derives: Option<String>,
    deprecation_strategy: deprecation::DeprecationStrategy,
    multiple_operation: bool,
) -> Result<TokenStream, failure::Error> {
    let mut context = QueryContext::new(schema, deprecation_strategy);

    if let Some(derives) = additional_derives {
        context.ingest_additional_derives(&derives).unwrap();
    }

    let mut definitions = Vec::new();

    for definition in &query.definitions {
        match definition {
            query::Definition::Operation(_op) => (),
            query::Definition::Fragment(fragment) => {
                let &query::TypeCondition::On(ref on) = &fragment.type_condition;
                context.fragments.insert(
                    &fragment.name,
                    GqlFragment {
                        name: &fragment.name,
                        selection: Selection::from(&fragment.selection_set),
                        on,
                        is_required: false.into(),
                    },
                );
            }
        }
    }

    let response_data_fields = {
        let root_name = operation.root_name(&context.schema);
        let opt_definition = context.schema.objects.get(&root_name);
        let definition = if let Some(definition) = opt_definition {
            definition
        } else {
            panic!(
                "operation type '{:?}' not in schema",
                operation.operation_type
            );
        };
        let prefix = &operation.name;
        let selection = &operation.selection;

        if operation.is_subscription() && selection.0.len() > 1 {
            Err(format_err!(
                "{}",
                ::constants::MULTIPLE_SUBSCRIPTION_FIELDS_ERROR
            ))?
        }

        definitions.extend(definition.field_impls_for_selection(&context, &selection, &prefix)?);
        definition.response_fields_for_selection(&context, &selection, &prefix)?
    };

    let enum_definitions = context.schema.enums.values().filter_map(|enm| {
        if enm.is_required.get() {
            Some(enm.to_rust(&context))
        } else {
            None
        }
    });
    let fragment_definitions: Result<Vec<TokenStream>, _> = context
        .fragments
        .values()
        .filter_map(|fragment| {
            if fragment.is_required.get() {
                Some(fragment.to_rust(&context))
            } else {
                None
            }
        })
        .collect();
    let fragment_definitions = fragment_definitions?;
    let variables_struct =
        operation.expand_variables(&context, &operation.name, multiple_operation);

    let input_object_definitions: Result<Vec<TokenStream>, _> = context
        .schema
        .inputs
        .values()
        .filter_map(|i| {
            if i.is_required.get() {
                Some(i.to_rust(&context))
            } else {
                None
            }
        })
        .collect();
    let input_object_definitions = input_object_definitions?;

    let scalar_definitions: Vec<TokenStream> = context
        .schema
        .scalars
        .values()
        .filter_map(|s| {
            if s.is_required.get() {
                Some(s.to_rust())
            } else {
                None
            }
        })
        .collect();

    let response_derives = context.response_derives();

    let respons_data_struct_name = if multiple_operation {
        Ident::new(
            format!("{}ResponseData", operation.name).as_str(),
            Span::call_site(),
        )
    } else {
        Ident::new("ResponseData", Span::call_site())
    };

    Ok(quote! {
        use serde_derive::*;

        #[allow(dead_code)]
        type Boolean = bool;
        #[allow(dead_code)]
        type Float = f64;
        #[allow(dead_code)]
        type Int = i64;
        #[allow(dead_code)]
        type ID = String;

        #(#scalar_definitions)*

        #(#input_object_definitions)*

        #(#enum_definitions)*

        #(#fragment_definitions)*

        #(#definitions)*

        #variables_struct

        #response_derives
        pub struct #respons_data_struct_name {
            #(#response_data_fields,)*
        }

    })
}
